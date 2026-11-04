//! Clips of stations for the server map (crate::clips): when the background
//! scan has read a multiplex received on air, a few of its stations not
//! recorded for a while are decoded together for some seconds, and the clips
//! go to the map. Only while the server is listed; the scan runs only while
//! nobody listens, so this costs no listener anything.

use super::*;
use crate::clips::{ClipRecorder, SECONDS};

/// A station is recorded again after this long.
const CLIP_EVERY: i64 = 6 * 3600;
/// After too little audio (no audio, a weak signal): tried again in an hour.
const RETRY_AFTER: i64 = 3600;
/// Stations recorded at once on one multiplex.
const AT_ONCE: usize = 4;
/// A slide goes with a clip up to this size (the map takes 256 KB).
const SLIDE_MAX_BYTES: usize = 256 << 10;
/// The scan waits at most this long for the clips of a multiplex.
const CLIPS_MAX: Duration = Duration::from_secs(SECONDS as u64 + 15);

impl Radio {
    /// The scan has read `channel`: records the stations due. True when it does
    /// (the scan stays until clips_done).
    pub(super) fn clips_start(&mut self, channel: &str) -> bool {
        if !self.server_map.listed() || crate::servermap::source(&self.settings, channel) != "air" {
            return false;
        }
        let Some(eid) = self.ensemble.eid else { return false };
        let Some(entry) = self.service_list.ensembles.iter().find(|e| e.channel == channel && e.eid == eid) else { return false };
        let now = crate::servicelist::now();
        let mut due: Vec<(i64, u32, String)> = entry
            .services
            .iter()
            .filter(|s| !s.ca && self.ensemble.services.contains_key(&s.sid))
            .map(|s| (self.clip_times.last(channel, eid, s.sid), s.sid, s.label.clone()))
            .filter(|(t, _, _)| now - t >= CLIP_EVERY)
            .collect();
        // (the ones recorded longest ago first)
        due.sort();
        // (the DTR-100 streams one sub-channel at a time)
        due.truncate(if self.dongle_active() { 1 } else { AT_ONCE });
        for (_, sid, label) in due {
            if let Some(r) = ClipRecorder::new(channel, eid, sid, &label) {
                self.clips.insert(ServiceKey { sid, scids: 0 }, r);
            }
        }
        if self.clips.is_empty() {
            return false;
        }
        log::debug!("clips: recording {} station(s) of {channel}", self.clips.len());
        self.update_decoding();
        true
    }

    /// The scan holds for clips: done when every one has its length (or after a while).
    pub(super) fn clips_done(&self, elapsed: Duration) -> bool {
        self.clips.values().all(|r| r.done()) || elapsed > CLIPS_MAX
    }

    /// The clips of the multiplex go to the map, their decoding stops.
    pub(super) fn clips_finish(&mut self) {
        let now = crate::servicelist::now();
        for (key, r) in std::mem::take(&mut self.clips) {
            if r.seconds() < SECONDS as f32 / 2.0 || r.silent() {
                log::debug!("clips: {} gave only {:.1} s{}", r.label, r.seconds(), if r.silent() { " of silence" } else { "" });
                self.clip_times.set(&r.channel, r.eid, r.sid, now - CLIP_EVERY + RETRY_AFTER);
                continue;
            }
            self.clip_times.set(&r.channel, r.eid, r.sid, now);
            let dls = self.services.get(&key).and_then(|s| s.dls.as_ref()).map(|d| d.text.trim().to_string()).unwrap_or_default();
            let (channel, eid, sid, time, seconds, glitches) = (r.channel.clone(), r.eid, r.sid, r.time, r.seconds(), r.glitches());
            let data = r.finish();
            // the station's slide (cached, from this scan or one at most 6 h before)
            let slide = self
                .slide_cache
                .get(&slide_key(&channel, eid, sid))
                .filter(|(h, d)| h["received"].as_i64().is_some_and(|t| t >= time - CLIP_EVERY) && d.len() <= SLIDE_MAX_BYTES)
                .and_then(|(h, d)| {
                    h["mime"].as_str().filter(|m| matches!(*m, "image/jpeg" | "image/png")).map(|m| (m.to_string(), d.clone()))
                });
            log::info!(
                "clips: {} ({channel}, {:.0} s, {} KB{}) to the server map",
                self.service_name(sid),
                seconds,
                data.len() / 1024,
                if glitches > 0 { format!(", {glitches} glitches: marked") } else { String::new() }
            );
            self.server_map.send_clip(crate::servermap::Clip { channel, eid, sid, time, seconds, dls, data, glitches, slide });
        }
        self.update_decoding();
    }

    /// Audio of a station being recorded.
    pub(super) fn clips_feed(&mut self, key: ServiceKey, pcm: &[f32]) {
        if let Some(r) = self.clips.get_mut(&key) {
            r.feed(pcm);
        }
    }
}
