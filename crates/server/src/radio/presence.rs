//! "Are you still listening?" (Settings > Access, off by default): a listener
//! who sent no command by hand for a while is asked; without an answer in
//! time the connection is closed, and the interface says why and connects
//! again only on a click. Admins are not asked. Commands the interface sends
//! by itself (pings, subscriptions, logos, diagnostics, and those marked
//! `auto`: after a reconnect, or a station the server chose) are no actions.

use super::*;
use crate::hub::Out;

/// Commands the interface sends without the listener doing anything.
const AUTOMATIC: [&str; 6] = ["ping", "subscribe", "diag", "getLogo", "getSettings", "listeners"];

/// The network gets this long on top of the time to answer.
const GRACE: Duration = Duration::from_secs(3);

impl Radio {
    /// A command came from a client. By hand, it is an action: the question
    /// pending, if any, is answered by it.
    pub(super) fn note_action(&mut self, client: u64, kind: &str, msg: &Value) {
        if AUTOMATIC.contains(&kind) || msg["auto"].as_bool() == Some(true) {
            return;
        }
        let asked = self
            .hub
            .update(client, |c| {
                c.last_action = Instant::now();
                c.asked.take().is_some()
            })
            .unwrap_or(false);
        // (the listener did something else: the dialog of the interface goes away)
        if asked && kind != "stillHere" {
            self.hub.send(client, text(&json!({"type": "stillHere", "ask": false})));
        }
    }

    /// From the tick: ask the listeners idle for long, show out those who did
    /// not answer.
    pub(super) fn presence_tick(&mut self) {
        let cfg = self.settings.idle_check.clone();
        let now = Instant::now();
        if !cfg.enabled {
            // (switched off while a question was pending)
            for (id, _, asked) in self.hub.presence() {
                if asked.is_some() {
                    self.hub.update(id, |c| c.asked = None);
                    self.hub.send(id, text(&json!({"type": "stillHere", "ask": false})));
                }
            }
            return;
        }
        let idle = Duration::from_secs(cfg.idle_minutes.max(1) as u64 * 60);
        let seconds = cfg.answer_seconds.max(5);
        for (id, last, asked) in self.hub.presence() {
            match asked {
                None if now.duration_since(last) >= idle => {
                    self.hub.update(id, |c| c.asked = Some(now));
                    self.hub.send(id, text(&json!({"type": "stillHere", "ask": true, "seconds": seconds})));
                    log::info!(
                        "client {id} ({}): nothing for {} min, asked whether it still listens",
                        self.client_ip(id),
                        cfg.idle_minutes
                    );
                }
                Some(t) if now.duration_since(t) >= Duration::from_secs(seconds as u64) + GRACE => {
                    log::info!("client {id} ({}): no answer within {seconds} s, disconnected", self.client_ip(id));
                    // (a client with a full queue gets no close frame: dropping its
                    // queue ends the connection all the same)
                    if !self.hub.try_send(id, Out::Close(crate::access::CLOSE_IDLE, "no answer: still listening?".into())) {
                        self.hub.unregister(id);
                    }
                    // (the connection ends in a moment: not asked again meanwhile)
                    self.hub.update(id, |c| {
                        c.asked = None;
                        c.last_action = now;
                    });
                }
                _ => {}
            }
        }
    }
}
