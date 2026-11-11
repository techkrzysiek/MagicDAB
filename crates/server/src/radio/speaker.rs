//! The radio's side of its own sound output (speaker.rs): which station it
//! plays, its settings from the page, its state for the page.

use super::*;
use crate::speaker::{self, Speaker};

/// How often the clients that show it get the output's state (the loudness
/// meter moves with it).
const REPORT_EVERY: Duration = Duration::from_millis(200);

impl Radio {
    /// After a change of the clients' stations or sound: what the output plays.
    pub(super) fn update_speaker(&mut self) {
        let want = self.hub.speaker_service();
        if want == self.speaker_on {
            return;
        }
        self.speaker_on = want;
        match want {
            Some(_) => self.speaker.get_or_insert_with(Speaker::new).start(),
            None => {
                if let Some(s) = &self.speaker {
                    s.stop();
                }
            }
        }
    }

    /// Decoded sound of a station: to the output when it plays it.
    pub(super) fn speaker_feed(&self, slot: u32, key: ServiceKey, pcm: &[f32]) {
        if self.speaker_on == Some((slot, key)) {
            if let Some(s) = &self.speaker {
                s.push(pcm);
            }
        }
    }

    /// The page asks for its sound from the program (or for Opus or PCM when
    /// the program cannot play here).
    pub(super) fn speaker_allowed(&self, client: u64) -> bool {
        self.hub.is_local(client) && speaker::available()
    }

    /// `{type: "speaker", gain, target}` from a page on this computer: the
    /// volume (a factor) and the buffer (seconds).
    pub(super) fn on_speaker_message(&mut self, client: u64, msg: &Value) {
        if !self.speaker_allowed(client) {
            return;
        }
        let s = self.speaker.get_or_insert_with(Speaker::new);
        if let Some(g) = msg["gain"].as_f64() {
            s.set_gain(g as f32);
        }
        if let Some(t) = msg["target"].as_f64() {
            s.set_target(t as f32);
        }
    }

    /// Every tick: the buffer and loudness to the pages that show them.
    pub(super) fn speaker_tick(&mut self) {
        if self.speaker_on.is_none() || self.speaker_reported.elapsed() < REPORT_EVERY {
            return;
        }
        self.speaker_reported = Instant::now();
        let Some(s) = &self.speaker else { return };
        let mut v = serde_json::to_value(s.report()).unwrap_or_default();
        v["type"] = json!("speaker");
        self.hub.broadcast(text(&v), |c| c.audio && c.codec == hub::Codec::Native && c.subs.speaker);
    }
}
