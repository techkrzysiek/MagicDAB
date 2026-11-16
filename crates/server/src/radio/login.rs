//! Passwords, and the logins a browser keeps. A password is checked off the
//! radio thread (config.rs: PBKDF2, a fraction of a second on a small board,
//! and this thread carries the listeners' sound), one at a time, by a thread
//! of its own; its answer comes back as Command::Password. A page that gave
//! the right one gets a token for its next connections (sessions.rs), so it
//! neither keeps the password nor sends it again.
//!
//! Tries are counted per network and for everyone (access.rs: Attempts),
//! before the check: many at once do not get past the count.

use super::*;
use crate::sessions::Kind;

/// What a password was given for: the admin login, the gate of a private
/// server (the admin's or the tuning password), tuning while the tuner is
/// locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Login,
    Enter,
    Tune,
}

/// A password to check against the hashes it may be of.
pub struct Check {
    client: u64,
    ask: Ask,
    auto: bool,
    password: String,
    admin: Option<String>,
    tune: Option<String>,
}

/// What it was.
pub struct Checked {
    client: u64,
    ask: Ask,
    auto: bool,
    admin: bool,
    tuner: bool,
    /// The hash it matched was of an older kind: that one, and the one made
    /// now that the password is here.
    renew: Option<(String, String)>,
}

/// Passwords waiting for their check; more are refused like tries too many.
const WAITING: usize = 16;

fn start(results: Sender<Command>) -> Option<Sender<Check>> {
    let (tx, rx) = crossbeam_channel::bounded::<Check>(WAITING);
    let run = move || {
        for c in rx {
            let matches = |hash: &Option<String>| hash.as_deref().is_some_and(|h| config::verify_password(h, &c.password));
            let admin = matches(&c.admin);
            let tuner = !admin && matches(&c.tune);
            let matched = if admin { c.admin.as_ref() } else { c.tune.as_ref().filter(|_| tuner) };
            let renew = matched.filter(|h| config::hash_outdated(h)).map(|h| (h.clone(), config::hash_password(&c.password)));
            if results.send(Command::Password(Checked { client: c.client, ask: c.ask, auto: c.auto, admin, tuner, renew })).is_err() {
                return;
            }
        }
    };
    std::thread::Builder::new().name("login".into()).spawn(run).ok().map(|_| tx)
}

impl Radio {
    /// The network of a client, as password tries are counted.
    fn client_net(&self, client: u64) -> String {
        crate::access::network(&self.client_ip(client))
    }

    /// A password from a client (`login`, `enter`, `tuneLogin`): counted, then
    /// checked off this thread; refused at once after too many tries.
    pub(super) fn ask_password(&mut self, client: u64, ask: Ask, msg: &Value) {
        let net = self.client_net(client);
        let known = self.sessions.knows(&net);
        let now = Instant::now();
        let tries = if ask == Ask::Tune { &mut self.tune_tries } else { &mut self.login_tries };
        if tries.locked(&net, known, now) {
            return self.password_refused(client, ask, true);
        }
        let (n, all) = tries.tried(&net, now);
        let what = if ask == Ask::Tune { "tuning passwords" } else { "admin passwords" };
        if n == LOGIN_TRIES {
            log::warn!("{n} {what} tried from {net}: no more from there for {} min", LOGIN_LOCK.as_secs() / 60);
        }
        if all == LOGIN_TRIES_ALL {
            log::warn!("{all} {what} tried within {} min: for a while only from where someone logged in before", LOGIN_LOCK.as_secs() / 60);
        }
        let s = &self.settings;
        let check = Check {
            client,
            ask,
            auto: msg["auto"].as_bool().unwrap_or(false),
            password: msg["password"].as_str().unwrap_or("").to_string(),
            admin: s.admin_password_hash.clone().filter(|_| ask != Ask::Tune),
            tune: s.access.tune_password_hash.clone().filter(|_| ask != Ask::Login),
        };
        if self.password_checks.is_none() {
            self.password_checks = self.commands_tx.clone().and_then(start);
        }
        // (the queue full: as tries too many)
        if !self.password_checks.as_ref().is_some_and(|tx| tx.try_send(check).is_ok()) {
            self.password_refused(client, ask, true);
        }
    }

    /// The answer to a password that opened nothing.
    fn password_refused(&mut self, client: u64, ask: Ask, locked: bool) {
        // (the gate says it in its own words)
        if locked && ask != Ask::Enter {
            self.notice(client, "warning", "login_locked");
        }
        let reply = match ask {
            Ask::Login => json!({"type": "login", "ok": false, "admin": false, "chatToken": null}),
            Ask::Enter => json!({"type": "enter", "ok": false, "admin": false, "tuner": false, "chatToken": null, "locked": locked}),
            Ask::Tune => json!({"type": "tuneLogin", "ok": false, "tuner": false, "locked": locked, "chatToken": null}),
        };
        self.hub.send_always(client, text(&reply));
    }

    /// A password was checked (Command::Password).
    pub(super) fn on_password(&mut self, c: Checked) {
        // (gone meanwhile)
        if self.hub.session(c.client).is_none() {
            return;
        }
        let ip = self.client_ip(c.client);
        let net = crate::access::network(&ip);
        // an older kind of hash: the new one in its place, unless the password changed meanwhile
        if let Some((old, new)) = c.renew {
            let s = &mut self.settings;
            let hash = if c.admin { &mut s.admin_password_hash } else { &mut s.access.tune_password_hash };
            if hash.as_deref() == Some(old.as_str()) {
                *hash = Some(new);
                self.store.save(&self.settings);
            }
        }
        if c.admin {
            self.login_tries.succeeded(&net);
            self.admin_in(c.client, c.ask, c.auto, &ip, Some(&net));
        } else if c.tuner {
            if c.ask == Ask::Tune {
                self.tune_tries.succeeded(&net);
            } else {
                self.login_tries.succeeded(&net);
            }
            self.tuner_in(c.client, c.ask, c.auto, &ip, Some(&net));
        } else {
            match c.ask {
                Ask::Login => log::warn!("client {} ({ip}): wrong admin password", c.client),
                Ask::Enter => log::warn!("client {} ({ip}): wrong password at the gate", c.client),
                Ask::Tune => log::warn!("client {} ({ip}): wrong tuning password", c.client),
            }
            self.password_refused(c.client, c.ask, false);
        }
    }

    /// A page comes back with the token of its login (`login` or `tuneLogin`
    /// with `session`): no password, no count of tries (a token is not guessed).
    pub(super) fn resume_session(&mut self, client: u64, ask: Ask, msg: &Value) {
        let token = msg["session"].as_str().unwrap_or("");
        let auto = msg["auto"].as_bool().unwrap_or(true);
        let ip = self.client_ip(client);
        let net = crate::access::network(&ip);
        let kind = if ask == Ask::Tune { Kind::Tuner } else { Kind::Admin };
        match self.sessions.resume(kind, token, &net) {
            Some(hash) => {
                self.hub.update(client, |c| {
                    *(if kind == Kind::Admin { &mut c.session } else { &mut c.tune_session }) = Some((token.to_string(), hash))
                });
                if kind == Kind::Admin {
                    self.admin_in(client, ask, auto, &ip, None);
                } else {
                    self.tuner_in(client, ask, auto, &ip, None);
                }
            }
            None => self.password_refused(client, ask, false),
        }
    }

    /// The token of a client's login for its page: the one it came with, or
    /// (after a password, `net`) a new one.
    fn session_token(&mut self, client: u64, kind: Kind, net: Option<&str>) -> Option<String> {
        if let Some(net) = net {
            let (token, hash) = self.sessions.open(kind, net);
            // (a login before on this connection ends)
            let before = self
                .hub
                .update(client, |c| (if kind == Kind::Admin { &mut c.session } else { &mut c.tune_session }).replace((token, hash)))
                .flatten();
            if let Some((_, hash)) = before {
                self.sessions.close(&hash);
            }
        }
        self.hub.update(client, |c| if kind == Kind::Admin { c.session.clone() } else { c.tune_session.clone() }).flatten().map(|s| s.0)
    }

    /// The client is admin from now on: by its password (`net`: a session for
    /// its page is opened) or by the token of a login before.
    fn admin_in(&mut self, client: u64, ask: Ask, auto: bool, ip: &str, net: Option<&str>) {
        let session = self.session_token(client, Kind::Admin, net);
        // (an automatic login again after a reconnect is not worth a log line)
        let was = self.grant_admin(client);
        if !was && !auto {
            match ask {
                Ask::Enter => log::info!("client {client} ({ip}) came in as admin"),
                _ => log::info!("client {client} ({ip}) logged in as admin"),
            }
        }
        // (a client at the gate of a private server comes in)
        let chat_token = self.after_login(client);
        let reply = match ask {
            Ask::Enter => {
                json!({"type": "enter", "ok": true, "admin": true, "tuner": false, "chatToken": chat_token, "locked": false, "session": session})
            }
            _ => json!({"type": "login", "ok": true, "admin": true, "chatToken": chat_token, "session": session}),
        };
        self.hub.send_always(client, text(&reply));
        self.hub.send(client, text(&self.settings_json()));
        self.hub.send(client, text(&self.update_json()));
        if ask != Ask::Enter {
            self.send_dxrec_list(client);
        }
    }

    /// The client may tune while the tuner is locked (and comes in at the
    /// gate of a private server): by the tuning password or by the token of
    /// a login with it before.
    fn tuner_in(&mut self, client: u64, ask: Ask, auto: bool, ip: &str, net: Option<&str>) {
        let session = self.session_token(client, Kind::Tuner, net);
        let was = self.hub.update(client, |c| std::mem::replace(&mut c.tuner, true)).unwrap_or(false);
        if !was && !auto {
            match ask {
                Ask::Enter => log::info!("client {client} ({ip}) came in with the tuning password"),
                _ => log::info!("client {client} ({ip}) may tune (tuning password)"),
            }
        }
        let chat_token = self.after_login(client);
        let reply = match ask {
            Ask::Enter => {
                json!({"type": "enter", "ok": true, "admin": false, "tuner": true, "chatToken": chat_token, "locked": false, "tuneSession": session})
            }
            _ => json!({"type": "tuneLogin", "ok": true, "tuner": true, "locked": false, "chatToken": chat_token, "tuneSession": session}),
        };
        self.hub.send_always(client, text(&reply));
    }

    /// A logout (the admin's or the tuning one): the login its page kept ends too.
    pub(super) fn end_session(&mut self, client: u64, kind: Kind) {
        let session = self.hub.update(client, |c| if kind == Kind::Admin { c.session.take() } else { c.tune_session.take() }).flatten();
        if let Some((_, hash)) = session {
            self.sessions.close(&hash);
        }
    }

    /// A new password of a kind: the logins kept with the old one end, but
    /// the one of the admin who set it (`by`).
    pub(super) fn password_changed(&mut self, kind: Kind, by: u64) {
        let keep = self.hub.update(by, |c| c.session.clone()).flatten().map(|s| s.1).filter(|_| kind == Kind::Admin);
        self.sessions.close_all(kind, keep.as_deref());
    }
}
