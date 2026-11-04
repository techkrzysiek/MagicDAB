//! The gate of a private server (Settings > Access, "public server" off): a
//! visitor gives the admin password or the tuning password before the server
//! sends anything of the receiver. Every new connection waits at the gate
//! (hub.rs: `send` and `broadcast` skip it) until the radio thread lets it in
//! with its snapshot: at once on a public server, after a password on a
//! private one. With the tuning password the visitor is a listener who may
//! tune, with the admin password an admin; the window of the Windows
//! application logs in with its token as always.

use super::*;

/// A token for the chat (`/chat?token=`): the chat knows admins by it, and on
/// a private server those let in.
fn new_token() -> String {
    (0..16).map(|_| format!("{:02x}", rand::random::<u8>())).collect()
}

impl Radio {
    /// A new connection: in at once, or the gate's hello.
    pub(super) fn on_connected(&mut self, id: u64) {
        if self.settings.public_server {
            self.let_in(id);
        } else {
            self.hub.send_always(id, text(&self.gate_hello_json(id)));
        }
    }

    /// A client comes in: its snapshot. (An open page does not use the tuner:
    /// the background scan goes on until someone plays the sound, chooses a
    /// station or tunes.)
    fn let_in(&mut self, id: u64) {
        self.hub.update(id, |c| c.gated = false);
        self.send_snapshot(id);
        self.broadcast_listeners();
    }

    /// Back to the gate: a client of a private server that is neither admin
    /// nor trusted listener (any more) listens to nothing.
    fn gate(&mut self, id: u64) {
        self.hub.update(id, |c| {
            c.gated = true;
            c.service = None;
            c.audio = false;
            c.record = false;
            c.subs = Default::default();
            c.slot = 0;
            c.chat_token = None;
        });
        self.update_decoding();
        self.hub.send_always(id, text(&self.gate_hello_json(id)));
    }

    /// What a visitor at the gate learns: the name of the server, that it is
    /// private and which passwords open it (the other fields the interface
    /// expects of a hello are there, empty).
    fn gate_hello_json(&self, id: u64) -> Value {
        json!({
            "type": "hello",
            "version": VERSION,
            "sourceUrl": if self.settings.source_url.is_empty() { SOURCE_URL } else { self.settings.source_url.as_str() },
            "clientId": id,
            "stationName": self.settings.station_name,
            "private": true,
            "admin": false,
            "tuner": false,
            "tunePassword": self.settings.access.tune_password_hash.is_some(),
            "tunerLocked": true,
            "location": null,
            "channels": [],
            "txdb": {"count": 0, "updated": null},
            "opusBitrate": 0,
        })
    }

    /// Whether a command may come through the gate: the passwords, the ping
    /// and the diagnostics of the connection; nothing of the receiver.
    pub(super) fn gate_allows(&self, client: u64, kind: &str) -> bool {
        !self.hub.is_gated(client) || matches!(kind, "enter" | "login" | "tuneLogin" | "ping" | "diag")
    }

    /// Wrong admin passwords: a few from one address, and not many of everyone
    /// together (forwarded addresses can be made up), stop logins for a while.
    pub(super) fn login_locked(&mut self, ip: &str) -> bool {
        self.login_failures.retain(|_, (_, first)| first.elapsed() < LOGIN_LOCK);
        while self.login_failures_all.front().is_some_and(|t| t.elapsed() >= LOGIN_LOCK) {
            self.login_failures_all.pop_front();
        }
        self.login_failures.get(ip).is_some_and(|(n, _)| *n >= LOGIN_TRIES) || self.login_failures_all.len() >= LOGIN_TRIES_ALL
    }

    pub(super) fn login_failed(&mut self, ip: &str) {
        let e = self.login_failures.entry(ip.to_string()).or_insert((0, Instant::now()));
        e.0 += 1;
        let n = e.0;
        self.login_failures_all.push_back(Instant::now());
        if n == LOGIN_TRIES {
            log::warn!("{n} wrong admin passwords from {ip}: no logins from there for {} min", LOGIN_LOCK.as_secs() / 60);
        }
        if self.login_failures_all.len() == LOGIN_TRIES_ALL {
            log::warn!(
                "{LOGIN_TRIES_ALL} wrong admin passwords within {} min: password logins stopped for a while",
                LOGIN_LOCK.as_secs() / 60
            );
        }
    }

    /// The client is admin from now on, with a new token for the chat (a new
    /// one with every login). Whether it was admin already.
    pub(super) fn grant_admin(&self, client: u64) -> bool {
        let token = new_token();
        self.hub
            .update(client, |c| {
                c.chat_token = Some(token);
                std::mem::replace(&mut c.admin, true)
            })
            .unwrap_or(false)
    }

    /// After a password was accepted (the admin's or the tuning one): a client
    /// at the gate comes in. Its token for the chat: the admin's, or one made
    /// for a trusted listener of a private server (its chat is for those let in).
    pub(super) fn after_login(&mut self, client: u64) -> Option<String> {
        if self.hub.is_gated(client) {
            self.let_in(client);
        }
        let private = !self.settings.public_server;
        self.hub
            .update(client, |c| {
                if private && c.chat_token.is_none() {
                    c.chat_token = Some(new_token());
                }
                c.chat_token.clone()
            })
            .flatten()
    }

    /// After a logout (the admin's or the tuning one): on a private server a
    /// client that is neither any more goes back to the gate.
    pub(super) fn after_logout(&mut self, client: u64) {
        if !self.settings.public_server && !self.hub.may_tune(client) {
            self.gate(client);
            self.broadcast_listeners();
        }
    }

    /// A password at the gate: the admin's makes an admin, the tuning password
    /// a trusted listener. One message for both, so that a trusted listener's
    /// password is no wrong admin password; a wrong one counts as a guess at
    /// both and is limited like the admin login.
    pub(super) fn on_enter(&mut self, client: u64, msg: &Value) {
        let pw = msg["password"].as_str().unwrap_or("");
        let ip = self.client_ip(client);
        let locked = self.login_locked(&ip);
        let matches = |hash: Option<&String>| !locked && hash.is_some_and(|h| config::verify_password(h, pw));
        let admin = matches(self.settings.admin_password_hash.as_ref());
        let tuner = !admin && matches(self.settings.access.tune_password_hash.as_ref());
        if admin {
            self.login_failures.remove(&ip);
            self.grant_admin(client);
            log::info!("client {client} ({ip}) came in as admin");
        } else if tuner {
            self.login_failures.remove(&ip);
            self.hub.update(client, |c| c.tuner = true);
            log::info!("client {client} ({ip}) came in with the tuning password");
        } else if !locked {
            log::warn!("client {client} ({ip}): wrong password at the gate");
            self.login_failed(&ip);
        }
        let chat_token = if admin || tuner { self.after_login(client) } else { None };
        let reply =
            json!({"type": "enter", "ok": admin || tuner, "admin": admin, "tuner": tuner, "chatToken": chat_token, "locked": locked});
        self.hub.send_always(client, text(&reply));
        if admin {
            self.hub.send(client, text(&self.settings_json()));
            self.hub.send(client, text(&self.update_json()));
        }
    }

    /// Settings > Access: the server public or private (everyone who is
    /// neither admin nor trusted listener goes to the gate, or everyone at
    /// the gate comes in), and the question to idle listeners.
    pub(super) fn apply_gate_settings(&mut self, s: &Value) {
        if let Some(c) = s.get("idleCheck").and_then(|v| serde_json::from_value::<config::IdleCheck>(v.clone()).ok()) {
            self.settings.idle_check =
                config::IdleCheck { idle_minutes: c.idle_minutes.clamp(1, 24 * 60), answer_seconds: c.answer_seconds.clamp(10, 3600), ..c };
        }
        let Some(public) = s.get("publicServer").and_then(|v| v.as_bool()) else { return };
        if public == self.settings.public_server {
            return;
        }
        self.settings.public_server = public;
        if public {
            log::info!("the server is public now");
            for id in self.hub.ids() {
                if self.hub.is_gated(id) {
                    self.let_in(id);
                }
            }
        } else {
            log::info!("the server is private now: visitors give the admin or the tuning password first");
            for id in self.hub.ids() {
                if self.hub.is_admin(id) {
                    continue;
                }
                if self.hub.may_tune(id) {
                    // (a trusted listener stays; the chat of a private server knows it by a token)
                    let token = self.after_login(id);
                    self.hub.send(id, text(&json!({"type": "tuneLogin", "ok": true, "tuner": true, "chatToken": token})));
                } else {
                    self.gate(id);
                }
            }
            self.broadcast_listeners();
        }
        // (the plain HTTP endpoints follow at once)
        self.update_public();
    }
}
