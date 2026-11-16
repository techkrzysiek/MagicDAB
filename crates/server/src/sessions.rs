//! Logins a browser keeps: a page that gave a password gets a token for its
//! next connections (a reconnect, a reload, a restart of the program), so it
//! neither keeps the password nor sends it again each time. A token is 128
//! random bits; only its SHA-256 is kept here (data/sessions.json), so the
//! file opens no login. A session not used for 30 days is forgotten, a
//! logout ends it, and a new password ends all of its kind.
//!
//! The networks of the sessions are also the ones someone logged in from
//! before: the guesses of others do not keep them out (access.rs: Attempts).

use crate::servicelist::now;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A session unused for this long is forgotten.
const MAX_AGE: i64 = 30 * 86400;
/// Sessions kept (the ones used longest ago go first).
const MAX: usize = 200;
/// The time of its last use goes to the file this often.
const TOUCH_EVERY: i64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Admin,
    /// A listener who gave the tuning password.
    Tuner,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
    /// SHA-256 of its token (hex).
    hash: String,
    kind: Kind,
    /// The network it was last used from (access::network).
    net: String,
    /// Unix time of its last use.
    used: i64,
}

pub struct Sessions {
    path: PathBuf,
    list: Vec<Session>,
}

fn hash_of(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl Sessions {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("sessions.json");
        let list: Vec<Session> = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
        let mut s = Sessions { path, list };
        s.prune();
        s
    }

    fn prune(&mut self) {
        let oldest = now() - MAX_AGE;
        self.list.retain(|s| s.used >= oldest);
        if self.list.len() > MAX {
            self.list.sort_by_key(|s| std::cmp::Reverse(s.used));
            self.list.truncate(MAX);
        }
    }

    fn save(&mut self) {
        self.prune();
        match serde_json::to_vec_pretty(&self.list) {
            Ok(data) => {
                if let Err(e) = crate::config::write_private(&self.path, &data) {
                    log::warn!("cannot save the logins: {e}");
                }
            }
            Err(e) => log::warn!("cannot save the logins: {e}"),
        }
    }

    /// A new session for a page that gave the right password: its token (for
    /// the page) and its name here (the hash, for `close`).
    pub fn open(&mut self, kind: Kind, net: &str) -> (String, String) {
        let token: String = rand::random::<[u8; 16]>().iter().map(|b| format!("{b:02x}")).collect();
        let hash = hash_of(&token);
        self.list.push(Session { hash: hash.clone(), kind, net: net.to_string(), used: now() });
        self.save();
        (token, hash)
    }

    /// The session of a token a page kept, if it is still one of this kind:
    /// its name here.
    pub fn resume(&mut self, kind: Kind, token: &str, net: &str) -> Option<String> {
        // (a token is 32 hex digits: nothing else is hashed)
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let hash = hash_of(token);
        let oldest = now() - MAX_AGE;
        let s = self.list.iter_mut().find(|s| s.kind == kind && s.used >= oldest && s.hash == hash)?;
        let changed = s.net != net || now() - s.used >= TOUCH_EVERY;
        s.net = net.to_string();
        s.used = now();
        if changed {
            self.save();
        }
        Some(hash)
    }

    /// A logout.
    pub fn close(&mut self, hash: &str) {
        let n = self.list.len();
        self.list.retain(|s| s.hash != hash);
        if self.list.len() != n {
            self.save();
        }
    }

    /// Its password changed: every session of the kind ends, but `keep` (the
    /// one of the admin who changed it).
    pub fn close_all(&mut self, kind: Kind, keep: Option<&str>) {
        let n = self.list.len();
        self.list.retain(|s| s.kind != kind || Some(s.hash.as_str()) == keep);
        if self.list.len() != n {
            self.save();
        }
    }

    /// Someone logged in from this network before (a session from it lives).
    pub fn knows(&self, net: &str) -> bool {
        let oldest = now() - MAX_AGE;
        self.list.iter().any(|s| s.net == net && s.used >= oldest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_comes_back_with_its_token() {
        let dir = std::env::temp_dir().join(format!("magicdab-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Sessions::load(&dir);
        let (admin, admin_hash) = s.open(Kind::Admin, "203.0.113.7");
        let (tuner, _) = s.open(Kind::Tuner, "2001:db8:5:6::/64");
        assert_eq!(admin.len(), 32);
        assert_ne!(admin, tuner);
        // the file holds no token
        let file = std::fs::read_to_string(dir.join("sessions.json")).unwrap();
        assert!(!file.contains(&admin) && !file.contains(&tuner) && file.contains(&admin_hash));
        // after a restart of the program: the same sessions, each of its kind only
        let mut s = Sessions::load(&dir);
        assert_eq!(s.resume(Kind::Admin, &admin, "203.0.113.7"), Some(admin_hash.clone()));
        assert!(s.resume(Kind::Tuner, &admin, "203.0.113.7").is_none());
        assert!(s.resume(Kind::Admin, &tuner, "203.0.113.7").is_none());
        assert!(s.resume(Kind::Tuner, &tuner, "198.51.100.9").is_some());
        for bad in ["", "short", &"g".repeat(32), &"0".repeat(32), &format!("{admin}0")] {
            assert!(s.resume(Kind::Admin, bad, "203.0.113.7").is_none(), "{bad}");
        }
        // the networks someone logged in from (the tuner's moved)
        assert!(s.knows("203.0.113.7") && s.knows("198.51.100.9") && !s.knows("2001:db8:5:6::/64") && !s.knows("192.0.2.1"));
        // a new admin password: the other admin sessions end, the tuner's stays
        let (other, _) = s.open(Kind::Admin, "192.0.2.1");
        s.close_all(Kind::Admin, Some(&admin_hash));
        assert!(s.resume(Kind::Admin, &other, "192.0.2.1").is_none());
        assert!(s.resume(Kind::Admin, &admin, "203.0.113.7").is_some() && s.resume(Kind::Tuner, &tuner, "198.51.100.9").is_some());
        // a logout
        s.close(&admin_hash);
        assert!(s.resume(Kind::Admin, &admin, "203.0.113.7").is_none());
        assert!(Sessions::load(&dir).resume(Kind::Admin, &admin, "203.0.113.7").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_sessions_are_forgotten() {
        let dir = std::env::temp_dir().join(format!("magicdab-sessions-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let token = "00112233445566778899aabbccddeeff";
        let old = vec![Session { hash: hash_of(token), kind: Kind::Admin, net: "203.0.113.7".into(), used: now() - MAX_AGE - 10 }];
        std::fs::write(dir.join("sessions.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        let mut s = Sessions::load(&dir);
        assert!(s.resume(Kind::Admin, token, "203.0.113.7").is_none() && !s.knows("203.0.113.7"));
        // a damaged file: no sessions, and the next login writes a new one
        std::fs::write(dir.join("sessions.json"), "{").unwrap();
        let mut s = Sessions::load(&dir);
        let (t, _) = s.open(Kind::Admin, "203.0.113.7");
        assert!(Sessions::load(&dir).resume(Kind::Admin, &t, "203.0.113.7").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
