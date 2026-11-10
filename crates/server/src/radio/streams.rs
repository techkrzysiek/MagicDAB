//! The radio's side of the Icecast style streams (icy.rs): which stations may
//! be streamed, their decoding while somebody listens, their Opus packets.

use super::*;
use magicdab_codecs::opus::{OpusEncoder, FRAME};

/// A streamed station whose audio has not come for this long is quiet: the
/// stream goes on with silence (players would give up) and its title says why.
const QUIET_AFTER: Duration = Duration::from_millis(1500);
/// 20 ms of Opus.
const PACKET: Duration = Duration::from_millis(20);

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
        self.streams_quiet();
    }

    /// Streamed stations without audio for a while: silence at the pace of
    /// the air and a title saying why ("MagicDAB: NO DAB SIGNAL (11B)",
    /// "MagicDAB: NO AUDIO (11B)" when the multiplex comes but not the station).
    fn streams_quiet(&mut self) {
        let now = Instant::now();
        let wanted: Vec<u32> = self.streams_wanted.iter().copied().collect();
        self.stream_audio_at.retain(|sid, _| wanted.contains(sid));
        self.stream_silence_to.retain(|sid, _| wanted.contains(sid));
        let channel = self.channel.clone().unwrap_or_default();
        for sid in wanted {
            let since = *self.stream_audio_at.entry(sid).or_insert(now);
            if now.duration_since(since) < QUIET_AFTER {
                continue;
            }
            let why = if self.signal.synced { "NO AUDIO" } else { "NO DAB SIGNAL" };
            self.streams.set_status(sid, Some(format!("MagicDAB: {why} ({channel})")));
            // the silence from where the audio (or the last silence) ended
            let mut to = self.stream_silence_to.get(&sid).copied().filter(|t| *t > since).unwrap_or(since);
            let Some(pkt) = self.silence_packet() else { continue };
            while to + PACKET <= now {
                self.streams.send(sid, pkt.clone());
                to += PACKET;
            }
            self.stream_silence_to.insert(sid, to);
        }
    }

    /// 20 ms of silence at the streams' bitrate, of constant size: as many
    /// bytes a second as the audio, so the titles come as often (the ICY
    /// metadata goes every so many bytes).
    fn silence_packet(&mut self) -> Option<Vec<u8>> {
        let kbps = self.settings.streams.bitrate.clamp(32, 256);
        if self.stream_silence.as_ref().is_none_or(|(b, _)| *b != kbps) {
            let mut enc = OpusEncoder::new(kbps as i32 * 1000).ok()?.constant();
            let zeros = vec![0.0f32; FRAME * 2];
            let mut pkt = Vec::new();
            for _ in 0..3 {
                pkt = enc.encode(&zeros)?.to_vec();
            }
            self.stream_silence = Some((kbps, pkt));
        }
        self.stream_silence.as_ref().map(|(_, p)| p.clone())
    }

    /// The Opus encoder of a streamed station (and its PCM waiting for a whole packet).
    fn stream_encoder(&mut self, key: ServiceKey) -> Option<&mut (OpusEncoder, Vec<f32>)> {
        let bitrate = self.settings.streams.bitrate.clamp(32, 256) as i32 * 1000;
        match self.stream_encoders.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
            std::collections::hash_map::Entry::Vacant(v) => match OpusEncoder::new(bitrate) {
                Ok(enc) => Some(v.insert((enc, Vec::new()))),
                Err(e) => {
                    log::warn!("stream: {e}");
                    None
                }
            },
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
        // the audio again: the title is the label again
        if self.stream_audio_at.insert(key.sid, Instant::now()).is_none_or(|t| t.elapsed() >= QUIET_AFTER) {
            self.streams.set_status(key.sid, None);
        }
        let Some((enc, pending)) = self.stream_encoder(key) else { return };
        pending.extend_from_slice(pcm);
        let mut packets = Vec::new();
        while pending.len() >= FRAME * 2 {
            let frame: Vec<f32> = pending.drain(..FRAME * 2).collect();
            if let Some(pkt) = enc.encode(&frame) {
                packets.push(pkt.to_vec());
            }
        }
        for pkt in packets {
            self.streams.send(key.sid, pkt);
        }
    }
}
