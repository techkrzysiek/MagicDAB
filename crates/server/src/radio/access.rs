//! Access: bans, the tuning password, the channels listeners may tune to and the list of
//! listeners for admins.

use super::*;

impl Radio {
    /// The tunnel server forwards for its visitors: it is not a client (access.rs).
    pub(super) fn note_tunnel_server(&self) {
        let t = &self.settings.tunnel;
        crate::access::get().set_tunnel_host(if t.enabled { &t.host } else { "" });
    }

    /// The admin tunes anywhere, listeners to the channels of Settings > Access.
    pub(super) fn channel_allowed(&self, client: u64, channel: &str) -> bool {
        self.hub.is_admin(client) || crate::access::channel_allowed(&self.settings.access.allowed_channels, channel)
    }

    /// The channels a client may scan (empty: no limit).
    pub(super) fn only_channels(&self, client: u64) -> Vec<String> {
        if self.hub.is_admin(client) {
            Vec::new()
        } else {
            self.settings.access.allowed_channels.clone()
        }
    }

    /// The bans as they are now into the settings (saved soon) and to the admins.
    pub(super) fn sync_bans(&mut self) {
        self.settings.access.bans = crate::access::get().bans();
        self.settings_dirty = true;
        let m = text(&self.settings_json());
        self.hub.broadcast(m, |c| c.admin);
    }

    /// Who is connected, for the admins: address and its network, browser,
    /// since when, station.
    fn listener_list_json(&self, client: u64) -> Value {
        let access = crate::access::get();
        let mut list = self.hub.list();
        list.sort_by_key(|c| c.id);
        let clients: Vec<Value> = list
            .iter()
            .map(|c| {
                // (looked up in the background for the next refresh)
                let asn = c.address.parse::<std::net::IpAddr>().ok().and_then(|ip| {
                    access.lookup(ip);
                    access.asn_of(ip)
                });
                json!({
                    "id": c.id,
                    "address": c.address,
                    "agent": c.agent,
                    "via": c.via,
                    "connectedSecs": c.connected_secs,
                    "admin": c.admin,
                    "tuner": c.tuner,
                    "service": c.service,
                    "station": c.service.map(|s| self.service_name_in(c.slot, s)),
                    "channel": self.slot_channel(c.slot),
                    "audio": c.audio,
                    "asn": asn.map(|a| json!({"asns": a.asns, "prefix": a.prefix, "country": a.country})),
                    // (at the gate of a private server)
                    "gated": c.gated,
                })
            })
            .collect();
        json!({"type": "listenerList", "clients": clients, "self": client})
    }

    pub(super) fn on_access_message(&mut self, client: u64, kind: &str, msg: &Value) {
        let ip = self.client_ip(client);
        match kind {
            // a trusted listener may tune while the tuner is locked, without becoming admin
            "tuneLogin" => {
                let pw = msg["password"].as_str().unwrap_or("");
                let now = Instant::now();
                let locked = self.tune_failures.locked(&ip, now);
                let ok = !locked && self.settings.access.tune_password_hash.as_deref().is_some_and(|h| config::verify_password(h, pw));
                if locked {
                    self.notice(client, "warning", "login_locked");
                } else if ok {
                    self.tune_failures.succeeded(&ip);
                    let was = self.hub.update(client, |c| std::mem::replace(&mut c.tuner, true)).unwrap_or(false);
                    if !was && !msg["auto"].as_bool().unwrap_or(false) {
                        log::info!("client {client} ({ip}) may tune (tuning password)");
                    }
                } else {
                    log::warn!("client {client} ({ip}): wrong tuning password");
                    let (n, all) = self.tune_failures.failed(&ip, now);
                    if n == LOGIN_TRIES {
                        log::warn!("{n} wrong tuning passwords from {ip}: none accepted from there for {} min", LOGIN_LOCK.as_secs() / 60);
                    }
                    if all == LOGIN_TRIES_ALL {
                        log::warn!(
                            "{all} wrong tuning passwords within {} min: tuning passwords refused for a while",
                            LOGIN_LOCK.as_secs() / 60
                        );
                    }
                }
                // (a client at the gate of a private server comes in, with a token for its chat)
                let chat_token = if ok { self.after_login(client) } else { None };
                self.hub.send_always(
                    client,
                    text(&json!({"type": "tuneLogin", "ok": ok, "tuner": ok, "locked": locked, "chatToken": chat_token})),
                );
            }
            "tuneLogout" => {
                self.hub.update(client, |c| c.tuner = false);
                self.hub.send(client, text(&json!({"type": "tuneLogin", "ok": true, "tuner": false})));
                self.after_logout(client);
            }
            _ if !self.hub.is_admin(client) => self.notice(client, "warning", "admin_required"),
            "listeners" => {
                let v = self.listener_list_json(client);
                self.hub.send(client, text(&v));
            }
            "kick" => {
                let Some(target) = msg["address"].as_str().and_then(|a| a.trim().parse::<std::net::IpAddr>().ok()) else { return };
                if ip.parse::<std::net::IpAddr>().ok() == Some(crate::access::normalize(target)) {
                    self.notice(client, "warning", "access_self");
                    return;
                }
                log::info!("client {client} ({ip}) kicked {target}");
                crate::access::get().kick(target);
            }
            "ban" => {
                let address = msg["address"].as_str().unwrap_or("").trim();
                let Some(net) = crate::access::Net::parse(address) else {
                    self.notice(client, "error", "ban_invalid");
                    return;
                };
                // not the admin's own address, nor this machine
                let own = ip.parse::<std::net::IpAddr>().ok();
                let local: [std::net::IpAddr; 2] = [std::net::Ipv4Addr::LOCALHOST.into(), std::net::Ipv6Addr::LOCALHOST.into()];
                if own.into_iter().chain(local).any(|a| net.contains(a)) {
                    self.notice(client, "warning", "access_self");
                    return;
                }
                let now = now();
                let hours = msg["hours"].as_u64().filter(|&h| h > 0).map(|h| h.min(24 * 3650) as i64);
                let reason: String = msg["reason"].as_str().unwrap_or("").trim().chars().take(200).collect();
                let ban =
                    config::Ban { address: address.to_string(), reason, since: now, until: hours.map(|h| now + h * 3600), auto: false };
                if let Some(b) = crate::access::get().ban(ban) {
                    let until = hours.map(|h| format!(" for {h} h")).unwrap_or_default();
                    let why = if b.reason.is_empty() { String::new() } else { format!(": {}", b.reason) };
                    log::info!("client {client} ({ip}) banned {}{until}{why}", b.address);
                    let _ = crate::access::get().take_changed();
                    self.sync_bans();
                    self.notice(client, "info", "ban_added");
                }
            }
            "unban" => {
                let address = msg["address"].as_str().unwrap_or("");
                if crate::access::get().unban(address) {
                    log::info!("client {client} ({ip}) lifted the ban of {}", address.trim());
                    let _ = crate::access::get().take_changed();
                    self.sync_bans();
                }
            }
            _ => {}
        }
    }

    /// Settings > Access (bans have messages of their own).
    pub(super) fn apply_access_settings(&mut self, s: &Value) {
        let a = &mut self.settings.access;
        let before = a.clone();
        if let Some(v) = s.get("blockTor").and_then(|v| v.as_bool()) {
            a.block_tor = v;
        }
        if let Some(v) = s.get("blockedAsns").and_then(|v| v.as_array()) {
            let mut asns: Vec<u32> = v.iter().filter_map(crate::access::parse_asn).collect();
            asns.sort_unstable();
            asns.dedup();
            a.blocked_asns = asns;
        }
        if let Some(v) = s.get("autoBanHours").and_then(|v| v.as_u64()) {
            a.auto_ban_hours = v.min(24 * 365) as u32;
        }
        if let Some(v) = s.get("allowedChannels").and_then(|v| v.as_array()) {
            a.allowed_channels = crate::access::clean_channels(v.iter().filter_map(|c| c.as_str()));
        }
        if let Some(pw) = s.get("tunePassword").and_then(|v| v.as_str()) {
            // empty: no tuning password any more
            if pw.is_empty() {
                a.tune_password_hash = None;
            } else if pw.len() >= 4 {
                a.tune_password_hash = Some(config::hash_password(pw));
            }
        }
        if *a == before {
            return;
        }
        let cfg = a.clone();
        if cfg.block_tor != before.block_tor {
            log::info!("Tor exit nodes {}", if cfg.block_tor { "refused" } else { "accepted" });
        }
        if cfg.blocked_asns != before.blocked_asns {
            let list: Vec<String> = cfg.blocked_asns.iter().map(|n| format!("AS{n}")).collect();
            log::info!("refused networks: {}", if list.is_empty() { "none".to_string() } else { list.join(", ") });
        }
        if cfg.auto_ban_hours != before.auto_ban_hours {
            log::info!("automatic bans: {}", if cfg.auto_ban_hours == 0 { "off".to_string() } else { format!("{} h", cfg.auto_ban_hours) });
        }
        if cfg.allowed_channels != before.allowed_channels {
            let list = if cfg.allowed_channels.is_empty() { "all".to_string() } else { cfg.allowed_channels.join(", ") };
            log::info!("channels of the listeners: {list}");
        }
        if cfg.tune_password_hash != before.tune_password_hash {
            log::info!("tuning password {}", if cfg.tune_password_hash.is_some() { "set" } else { "removed" });
            // trusted listeners give the new one
            for id in self.hub.ids() {
                self.hub.update(id, |c| c.tuner = false);
            }
        }
        // (listeners of a newly refused network are closed once its prefixes are in)
        crate::access::get().configure(&cfg);
    }
}
