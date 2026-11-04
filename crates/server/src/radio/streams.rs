//! The radio's side of the Icecast style streams (icy.rs): which stations may
//! be streamed, their decoding while somebody listens, their Opus packets.

use super::*;
use magicdab_codecs::opus::{OpusEncoder, FRAME};

impl Radio {
    /// On every tick: the stations listened to (their decoding), and once a
    /// second what may be streamed.
    pub(super) fn streams_tick(&mut self) {
        if self.streams.take_changed() {
            let wanted = self.streams.wanted();
            if wanted != self.streams_wanted {
                for sid in wanted.difference(&self.streams_wanted) {
                    log::info!("stream: {} listened to, decoding it", self.service_name(*sid));
                }
                self.streams_wanted = wanted;
                let wanted = &self.streams_wanted;
                self.stream_encoders.retain(|k, _| wanted.contains(&k.sid));
                // (a stream's listener gets the tuner back from the background scan)
                if !self.streams_wanted.is_empty() && self.scan.as_ref().is_some_and(|s| s.silent) {
                    log::info!("a stream is listened to, stopping background scan");
                    self.stop_scan();
                    self.idle_since = None;
                }
                self.update_decoding();
            }
        }
        // (not from the multiplexes a scan passes)
        if self.streams_policy_at.elapsed() >= Duration::from_secs(1) && self.scan.is_none() {
            self.streams_policy_at = Instant::now();
            self.streams.set_policy(self.streams_policy());
        }
    }

    fn streams_policy(&self) -> crate::icy::Policy {
        let s = &self.settings.streams;
        let stations = self
            .ensemble
            .services
            .values()
            .filter(|x| x.is_audio() && x.ca_id == 0)
            .map(|x| {
                (
                    x.sid,
                    x.label
                        .as_ref()
                        .map(|l| l.text.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| format!("{:04X}", x.sid)),
                )
            })
            .collect();
        // the dongle receives one sub-channel: the station it plays
        let single = self.dongle_active();
        let only = self.service_lock.or_else(|| self.main_service.map(|k| k.sid).filter(|_| single));
        crate::icy::Policy {
            enabled: s.enabled,
            max: if single { 1 } else { s.max as usize },
            stations,
            only,
            channel: self.channel.clone().unwrap_or_default(),
            bitrate: s.bitrate,
        }
    }

    /// The stations streamed, to be decoded with the ones listened to on the page.
    pub(super) fn stream_keys(&self) -> impl Iterator<Item = ServiceKey> + '_ {
        self.streams_wanted.iter().map(|&sid| ServiceKey { sid, scids: 0 })
    }

    /// Decoded audio of a streamed station: Opus packets to its listeners.
    pub(super) fn streams_feed(&mut self, key: ServiceKey, pcm: &[f32]) {
        let bitrate = self.settings.streams.bitrate.clamp(32, 256) as i32 * 1000;
        let (enc, pending) = match self.stream_encoders.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => match OpusEncoder::new(bitrate) {
                Ok(enc) => v.insert((enc, Vec::new())),
                Err(e) => {
                    log::warn!("stream: {e}");
                    return;
                }
            },
        };
        pending.extend_from_slice(pcm);
        while pending.len() >= FRAME * 2 {
            let frame: Vec<f32> = pending.drain(..FRAME * 2).collect();
            if let Some(pkt) = enc.encode(&frame) {
                self.streams.send(key.sid, pkt.to_vec());
            }
        }
    }
}
