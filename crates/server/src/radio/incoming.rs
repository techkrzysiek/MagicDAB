//! Messages of the clients (WebSocket): login, selecting and tuning, scans, settings,
//! recordings, the gallery and the rest of what the interface asks for.

use super::*;

impl Radio {
    pub(super) fn on_message(&mut self, client: u64, msg: Value) {
        let kind = msg["type"].as_str().unwrap_or("");
        // (a visitor at the gate of a private server: its passwords only)
        if !self.gate_allows(client, kind) {
            return;
        }
        self.note_action(client, kind, &msg);
        match kind {
            "login" => {
                let pw = msg["password"].as_str().unwrap_or("");
                let ip = self.client_ip(client);
                // the program's own window (Windows application) needs no password
                let desktop = msg["desktop"].as_str().is_some_and(crate::desktop::is_token);
                // password guessing: a few attempts per address, not many of everyone
                // together (forwarded addresses can be made up)
                let locked = !desktop && self.login_locked(&ip);
                let ok = !locked
                    && (desktop || self.settings.admin_password_hash.as_deref().map(|h| config::verify_password(h, pw)).unwrap_or(false));
                if locked {
                    self.notice(client, "warning", "login_locked");
                } else if ok {
                    self.login_failures.remove(&ip);
                    // automatic re-login after reconnects is not worth a log line
                    let was = self.grant_admin(client);
                    if !was && !msg["auto"].as_bool().unwrap_or(false) {
                        log::info!("client {client} ({ip}) logged in as admin");
                    }
                } else {
                    log::warn!("client {client} ({ip}): wrong admin password");
                    self.login_failed(&ip);
                }
                // (a client at the gate of a private server comes in; one that failed there learns it)
                let chat_token = if ok { self.after_login(client) } else { None };
                self.hub.send_always(client, text(&json!({"type": "login", "ok": ok, "admin": ok, "chatToken": chat_token})));
                if ok {
                    self.hub.send(client, text(&self.settings_json()));
                    self.hub.send(client, text(&self.update_json()));
                    self.send_dxrec_list(client);
                }
            }
            "logout" => {
                self.hub.update(client, |c| {
                    c.admin = false;
                    c.chat_token = None;
                });
                self.hub.send(client, text(&json!({"type": "login", "ok": true, "admin": false})));
                self.after_logout(client);
            }
            // the gate of a private server: the admin or the tuning password
            "enter" => self.on_enter(client, &msg),
            // the answer to "are you still listening?" (an action like any other)
            "stillHere" => {}
            "select" => {
                let Some(sid) = msg["sid"].as_u64() else { return };
                let key = ServiceKey { sid: sid as u32, scids: msg["scids"].as_u64().unwrap_or(0) as u8 };
                if key == FM_KEY {
                    // (the FM tuner belongs to the main receiver)
                    self.back_to_main(client);
                    self.initial_pending.remove(&client);
                    self.hub.update(client, |c| c.service = Some(key));
                    self.update_decoding();
                    self.log_activity(client);
                    return;
                }
                // a station chosen during a background scan: the tuner goes back
                // first (its end moves everybody to the main station: this client
                // chose its own)
                if self.scan.as_ref().is_some_and(|s| s.silent) {
                    log::info!("client {client} chose a station, stopping background scan");
                    self.stop_scan();
                    self.idle_since = None;
                    self.hub.send(client, text(&json!({"type": "select", "sid": key.sid, "scids": key.scids, "reason": "user"})));
                }
                let mut channel = msg["channel"].as_str().map(|s| s.to_string());
                // back on its own multiplex after a reconnect, if that still works
                if msg["rejoin"].as_bool() == Some(true) && !channel.as_deref().is_some_and(|c| self.own_multiplex(client, c)) {
                    channel = None;
                }
                // a station of the listener's own multiplex (side slot), or of
                // another one it listens to on a receiver of its own
                if self.select_side(client, key, channel.as_deref()) {
                    return;
                }
                // (a receiver locked to one station: that one for everybody)
                if self.service_lock.is_some_and(|l| l != key.sid) {
                    if !msg["auto"].as_bool().unwrap_or(false) {
                        self.notice(client, "warning", "station_fixed");
                    }
                    self.send_initial_service(client);
                    return;
                }
                let in_current = self.ensemble.services.contains_key(&key.sid);
                if let Some(ch) = channel.filter(|ch| Some(ch.as_str()) != self.channel.as_deref() || !in_current) {
                    if Some(ch.as_str()) != self.channel.as_deref() {
                        if self.fixed_channel.is_some() {
                            self.notice(client, "warning", "channel_fixed");
                            return;
                        }
                        if !can_tune(&self.hub, &self.settings, client) {
                            self.notice(client, "warning", "tuner_locked");
                            return;
                        }
                        if !self.channel_allowed(client, &ch) {
                            self.notice(client, "warning", "channel_not_allowed");
                            return;
                        }
                        if self.scan.is_some() {
                            self.stop_scan();
                        }
                        log::info!("client {client} ({}) tunes to {ch} for {}", self.client_ip(client), self.service_name(key.sid));
                        self.tune(&ch);
                        // everybody on the main receiver follows the new multiplex
                        self.main_service = Some(key);
                        for id in self.hub.ids_in(0) {
                            self.hub.update(id, |c| c.service = Some(key));
                        }
                        self.hub.broadcast_main(text(&json!({"type": "select", "sid": key.sid, "scids": key.scids, "reason": "tuned"})));
                    }
                }
                // (a listener of a side slot listens to the main receiver again)
                self.back_to_main(client);
                self.initial_pending.remove(&client);
                // e.g. the station remembered by a reloaded page is not on this multiplex
                // (checked once the ensemble is complete: stable for a few seconds)
                let stable = self.ensemble.eid.is_some() && self.ensemble_changed.elapsed() > Duration::from_secs(3);
                if stable && !self.ensemble.services.contains_key(&key.sid) && self.main_service.is_some_and(|m| m != key) {
                    self.send_initial_service(client);
                    return;
                }
                // the dongle streams one station for everybody: the tuner decides
                let auto = msg["auto"].as_bool().unwrap_or(false);
                if self.dongle_active() && self.main_service.is_some_and(|m| m != key) && !can_tune(&self.hub, &self.settings, client) {
                    if !auto {
                        self.notice(client, "warning", "tuner_locked");
                    }
                    self.send_initial_service(client);
                    return;
                }
                self.hub.update(client, |c| c.service = Some(key));
                if can_tune(&self.hub, &self.settings, client) || self.main_service.is_none() {
                    let changed = self.main_service != Some(key);
                    self.main_service = Some(key);
                    self.settings.last_service = Some(key.sid);
                    self.settings_dirty = true;
                    if changed && self.dongle_active() {
                        for id in self.hub.ids_in(0) {
                            self.hub.update(id, |c| c.service = Some(key));
                        }
                        self.hub.broadcast_main(text(&json!({"type": "select", "sid": key.sid, "scids": key.scids, "reason": "tuned"})));
                    }
                }
                self.update_decoding();
                self.send_service_data(client, key);
                self.log_activity(client);
            }
            "speaker" => self.on_speaker_message(client, &msg),
            // Windows: the firewall exception (Settings > Setup), the admin's
            "firewall" => {
                let Some(tx) = self.commands_tx.clone().filter(|_| self.hub.is_admin(client) && cfg!(windows)) else { return };
                log::info!("client {client}: asks for the Windows firewall exception");
                crate::firewall::allow(move |result| {
                    let _ = tx.send(Command::Firewall { client, result });
                });
            }
            "audio" => {
                let enabled = msg["enabled"].as_bool().unwrap_or(false);
                // someone listens: the background scan gives the tuner back
                if enabled && self.scan.as_ref().is_some_and(|s| s.silent) {
                    log::info!("client {client} listens, stopping background scan");
                    self.stop_scan();
                    self.idle_since = None;
                }
                let codec = match msg["codec"].as_str() {
                    Some("pcm") => hub::Codec::Pcm,
                    // (the program plays it: only for a page on this computer)
                    Some("native") if self.speaker_allowed(client) => hub::Codec::Native,
                    _ => hub::Codec::Opus,
                };
                self.hub.update(client, |c| {
                    c.audio = enabled;
                    c.codec = codec;
                });
                self.update_decoding();
                self.log_activity(client);
            }
            "subscribe" => {
                let spectrum = msg["spectrum"].as_bool();
                let tii = msg["tiiPairs"].as_bool();
                let log = msg["log"].as_bool();
                let linking = msg["linking"].as_bool();
                let stats = msg["serverStats"].as_bool();
                let speaker = msg["speaker"].as_bool();
                self.hub.update(client, |c| {
                    if let Some(v) = speaker {
                        c.subs.speaker = v;
                    }
                    if let Some(v) = stats {
                        c.subs.stats = v;
                    }
                    if let Some(v) = linking {
                        c.subs.linking = v;
                    }
                    if let Some(v) = spectrum {
                        c.subs.spectrum = v;
                    }
                    if let Some(v) = tii {
                        c.subs.tii_pairs = v;
                    }
                    if let Some(v) = log {
                        c.subs.log = v;
                    }
                });
                // the log is for admins only (as its new lines, ws.rs): it holds
                // the addresses of listeners and the password generated at the
                // first start, which guards the setup of a new installation
                if log == Some(true) && self.hub.is_admin(client) {
                    let lines = crate::logbuf::recent();
                    self.hub.send(client, text(&json!({"type": "logHistory", "lines": lines})));
                }
                self.update_decoding();
            }
            "tune" => {
                let Some(ch) = msg["channel"].as_str() else { return };
                if self.fixed_channel.as_deref().is_some_and(|f| !f.eq_ignore_ascii_case(ch)) {
                    self.notice(client, "warning", "channel_fixed");
                    return;
                }
                // a multiplex of the listener's own choice: on a receiver of its
                // own, the main one stays where it is (the tuner lock is for it)
                if self.own_multiplex(client, ch) {
                    if !self.channel_allowed(client, ch) {
                        self.notice(client, "warning", "channel_not_allowed");
                    } else {
                        self.join_channel(client, ch, None);
                    }
                    return;
                }
                // (back on its own multiplex after a reconnect: not any more)
                if msg["rejoin"].as_bool() == Some(true) {
                    return;
                }
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                if !self.channel_allowed(client, ch) {
                    self.notice(client, "warning", "channel_not_allowed");
                    return;
                }
                if self.scan.is_some() {
                    self.stop_scan();
                }
                let ch = ch.to_string();
                log::info!("client {client} ({}) tunes to {ch}", self.client_ip(client));
                if self.tune(&ch) {
                    self.back_to_main(client);
                    self.main_service = None;
                    for id in self.hub.ids_in(0) {
                        self.hub.update(id, |c| c.service = None);
                    }
                    self.update_decoding();
                }
            }
            "scan" => {
                if self.fixed_channel.is_some() && msg["action"].as_str() != Some("stop") {
                    self.notice(client, "warning", "channel_fixed");
                    return;
                }
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                if msg["action"].as_str() == Some("stop") {
                    log::info!("client {client} ({}) stopped the scan", self.client_ip(client));
                    self.stop_scan();
                } else {
                    log::info!("client {client} ({}) started a band scan", self.client_ip(client));
                    let only = self.only_channels(client);
                    let limited = !only.is_empty();
                    if self.scan.is_none() && !self.start_scan_with(false, None, None, only) {
                        if self.device_error.is_some() && !self.device_running {
                            self.notice(client, "warning", "scan_no_device");
                        } else if limited {
                            self.notice(client, "warning", "channel_not_allowed");
                        }
                    }
                }
            }
            "dxScan" => {
                if self.fixed_channel.is_some() && msg["action"].as_str() == Some("start") {
                    self.notice(client, "warning", "channel_fixed");
                    return;
                }
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                match msg["action"].as_str() {
                    Some("start") => {
                        if self.scan.is_some() {
                            self.stop_scan();
                        }
                        let mode = serde_json::from_value(msg["mode"].clone()).unwrap_or(ScanMode::Normal);
                        let cycles = msg["cycles"].as_u64().unwrap_or(1) as u32;
                        let channels: Vec<String> = msg["channels"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|c| c.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default();
                        let dx = DxScan { mode, cycle: 0, cycles, tii: HashMap::new(), snr: 0.0 };
                        let only = self.only_channels(client);
                        let limited = !only.is_empty();
                        if !self.start_scan_with(false, Some(dx), Some(channels), only) && limited {
                            self.notice(client, "warning", "channel_not_allowed");
                        }
                    }
                    Some("stop") => self.stop_scan(),
                    Some("clear") => {
                        self.dx_results.clear();
                        self.dx_dirty = true;
                    }
                    _ => {}
                }
            }
            "serviceList" => {
                if !self.hub.is_admin(client) {
                    self.notice(client, "warning", "admin_required");
                    return;
                }
                match msg["action"].as_str() {
                    Some("remove") => {
                        let ch = msg["channel"].as_str().unwrap_or("");
                        let eid = msg["eid"].as_u64().unwrap_or(0) as u16;
                        if self.service_list.remove_ensemble(ch, eid) {
                            self.service_list.save(&self.data_dir);
                            self.hub.broadcast_all(text(&self.service_list_json()));
                            // (logos of stations new to the list)
                            self.logos_dirty = true;
                            self.logos_changed = Some(Instant::now());
                        }
                    }
                    Some("clear") => {
                        self.service_list.ensembles.clear();
                        self.service_list.save(&self.data_dir);
                        self.hub.broadcast_all(text(&self.service_list_json()));
                        // (logos of stations new to the list)
                        self.logos_dirty = true;
                        self.logos_changed = Some(Instant::now());
                    }
                    _ => {}
                }
            }
            "txdb" | "txdbPart" => {
                if !self.hub.is_admin(client) {
                    self.notice(client, "warning", "admin_required");
                    return;
                }
                // a big list comes in parts (messages are kept small)
                let whole = kind == "txdb";
                let part = msg["csv"].as_str().unwrap_or("");
                if whole || msg["part"].as_u64() == Some(0) {
                    self.txdb_upload = Some((client, String::new()));
                }
                match self.txdb_upload.as_mut() {
                    Some((c, csv)) if *c == client && csv.len() + part.len() <= 64 << 20 => csv.push_str(part),
                    _ => {
                        self.txdb_upload = None;
                        return;
                    }
                }
                if !whole && msg["last"].as_bool() != Some(true) {
                    return;
                }
                let Some((_, csv)) = self.txdb_upload.take() else { return };
                let csv = csv.as_str();
                let mut db = TxDb::default();
                let n = db.parse(csv);
                if n > 0 {
                    let _ = std::fs::write(TxDb::file(&self.data_dir), csv);
                    db.updated = Some(now());
                    self.txdb = db;
                    log::info!("transmitter database imported: {n} entries");
                    self.notice(client, "info", &format!("txdb_imported:{n}"));
                    self.tii = self.tii_json();
                    self.hub.broadcast_main(text(&self.tii));
                    for id in self.hub.ids() {
                        let h = self.hello_json(id);
                        self.hub.send(id, text(&h));
                    }
                } else {
                    self.notice(client, "error", "txdb_invalid");
                }
            }
            "settings" => {
                if !self.hub.is_admin(client) {
                    self.notice(client, "warning", "admin_required");
                    return;
                }
                // what really changed (forms send all their fields)
                let before = serde_json::to_value(&self.settings).unwrap_or_default();
                self.apply_settings(client, &msg["settings"]);
                let after = serde_json::to_value(&self.settings).unwrap_or_default();
                let changed: Vec<String> = after
                    .as_object()
                    .map(|a| {
                        a.iter()
                            .filter(|(k, v)| before.get(k.as_str()) != Some(*v))
                            .map(|(k, v)| match k.as_str() {
                                // (secrets; the access rules log their own lines)
                                "adminPasswordHash" | "discordWebhook" | "tunnel" | "access" | "serverMap" => k.clone(),
                                _ => format!("{k}={}", v.to_string().chars().take(80).collect::<String>()),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let what = if changed.is_empty() { "nothing".to_string() } else { changed.join(", ") };
                log::info!("client {client} ({}) changed settings: {what}", self.client_ip(client));
            }
            "fmAntenna" => {
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                let Some(n) = msg["antenna"].as_u64() else { return };
                let Some(fm) = self.fm.as_ref() else { return };
                if n < fm.antennas as u64 {
                    log::info!("client {client} ({}) switches the FM antenna to {}", self.client_ip(client), n + 1);
                    self.follower.suspend(Duration::from_secs(120));
                    fm.tuner.antenna(n as u8);
                }
            }
            "fmTune" | "fmScan" => {
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                let ip = self.client_ip(client);
                self.follower.suspend(Duration::from_secs(120));
                let Some(fm) = self.fm.as_ref() else { return };
                if kind == "fmScan" {
                    log::info!("client {client} ({ip}) scans the FM band");
                    fm.tuner.scan(87_500, 108_000, 100);
                } else if let Some(khz) = msg["khz"].as_u64().filter(|k| (64_000..=108_000).contains(k)) {
                    log::info!("client {client} ({ip}) tunes FM to {:.1} MHz", khz as f64 / 1000.0);
                    fm.tuner.tune(khz as u32);
                }
            }
            "updateCheck" | "updateInstall" => {
                if !self.hub.is_admin(client) {
                    self.notice(client, "warning", "admin_required");
                    return;
                }
                if crate::update::in_container() || self.update_busy {
                    self.hub.send(client, text(&self.update_json()));
                    return;
                }
                if kind == "updateCheck" {
                    if crate::update::url(&self.settings.update_url).is_empty() {
                        self.update_error = Some("no update address set".into());
                        self.hub.send(client, text(&self.update_json()));
                        return;
                    }
                    self.check_update();
                } else if let Some(a) = &self.update_available {
                    log::info!("client {client} ({}) installs update {}", self.client_ip(client), a.version);
                    self.install_update();
                }
                self.broadcast_update();
            }
            "linking" => {
                if !can_tune(&self.hub, &self.settings, client) {
                    self.notice(client, "warning", "tuner_locked");
                    return;
                }
                let ip = self.client_ip(client);
                if let Some(m) = msg["mode"].as_str().and_then(|m| serde_json::from_value::<crate::linking::Mode>(json!(m)).ok()) {
                    log::info!("client {client} ({ip}) sets linking to {m:?}");
                    self.follower.mode = m;
                    self.settings.linking.mode = m;
                    self.settings_dirty = true;
                }
                if let Some(v) = msg["enabled"].as_bool() {
                    log::info!("client {client} ({ip}) switches linking {}", if v { "on" } else { "off" });
                    self.follower.enabled = v;
                    self.settings.linking.enabled = v;
                    self.settings_dirty = true;
                }
                if let Some(v) = msg["ab"].as_bool() {
                    self.link_ab = v;
                    if let Some(l) = self.linker.as_mut() {
                        l.ab = v;
                    }
                }
                if msg["search"].as_bool() == Some(true) {
                    self.follower.event(format!("search requested by client {client}"));
                    // a fresh follower state for the service: search from the start
                    let key = self.follower.service();
                    let merged = self.channel.as_deref().and_then(|c| self.fig_cache.merged(c, &self.ensemble, &self.labels)).map(|m| m.0);
                    self.follower.set_service(None, &self.ensemble);
                    self.follower.set_service(key, merged.as_ref().unwrap_or(&self.ensemble));
                    if let Some(l) = self.linker.as_mut() {
                        l.set_fm_valid(false);
                    }
                }
                // (send at the next step; checked: Instant counts from boot on Windows)
                self.last_linking_sent = Instant::now().checked_sub(Duration::from_secs(5)).unwrap_or_else(Instant::now);
            }
            "diag" => {
                let ip = self.client_ip(client);
                let bg = if msg["visible"].as_bool() == Some(false) { ", page in the background" } else { "" };
                match msg["event"].as_str() {
                    Some("reconnected") => {
                        let down = msg["downMs"].as_f64().unwrap_or(0.0) / 1000.0;
                        let code = msg["code"].as_u64().unwrap_or(0);
                        let why = msg["reason"].as_str().filter(|r| !r.is_empty()).map(|r| format!(", {r}")).unwrap_or_default();
                        let how = if msg["clean"].as_bool() == Some(true) { "closed" } else { "broken" };
                        let offline = if msg["online"].as_bool() == Some(false) { ", browser offline" } else { "" };
                        log::info!("client {client} ({ip}): reconnected after {down:.1} s, the previous connection was {how} (code {code}{why}){offline}{bg}");
                    }
                    Some("uiError") => {
                        let message = msg["message"].as_str().unwrap_or("");
                        let stack = msg["stack"].as_str().unwrap_or("");
                        log::warn!("client {client} ({ip}): UI error: {message} (view {})\n{stack}", msg["view"].as_str().unwrap_or("?"));
                    }
                    Some("underruns") => {
                        let n = msg["count"].as_u64().unwrap_or(0);
                        let buffer = msg["buffer"].as_u64().unwrap_or(0);
                        let target = msg["target"].as_u64().map(|t| format!(" of {t}")).unwrap_or_default();
                        let slot = self.hub.slot_of(client);
                        let what = self
                            .hub
                            .session(client)
                            .and_then(|s| s.3)
                            .map(|s| format!(" of {}", self.service_name_in(slot, s)))
                            .unwrap_or_default();
                        // (messages the server could not queue for this client: its connection is too slow)
                        let dropped = self
                            .hub
                            .dropped(client)
                            .filter(|d| *d > 0)
                            .map(|d| format!(", {d} message(s) dropped so far"))
                            .unwrap_or_default();
                        log::info!(
                            "client {client} ({ip}): {n} audio dropout(s){what} in the last 15 s, buffer {buffer}{target} ms{dropped}{bg}"
                        );
                    }
                    _ => {}
                }
            }
            "getSettings" => {
                if self.hub.is_admin(client) {
                    self.hub.send(client, text(&self.settings_json()));
                }
            }
            "record" => {
                let on = msg["enabled"].as_bool().unwrap_or(false);
                let was = self.hub.update(client, |c| std::mem::replace(&mut c.record, on)).unwrap_or(false);
                if was != on {
                    let sid = self.hub.session(client).and_then(|s| s.3);
                    let what = sid.map(|s| self.service_name_in(self.hub.slot_of(client), s)).unwrap_or_default();
                    log::info!("client {client} ({}) {} recording {what}", self.client_ip(client), if on { "started" } else { "stopped" });
                }
                self.update_decoding();
            }
            "rawRecord" => {
                if !self.hub.is_admin(client) {
                    self.notice(client, "warning", "admin_required");
                    return;
                }
                if msg["action"].as_str() == Some("list") {
                    self.hub.send(client, text(&self.raw_state()));
                    return;
                }
                if msg["action"].as_str() == Some("stop") {
                    if let Some(r) = self.raw_recorder.lock().take() {
                        let p = r.finish();
                        log::info!("raw recording stopped: {}", p.display());
                    }
                } else if self.eti_input.is_some() || self.dongle_active() {
                    // (a stream, a recording of ETI or the DTR-100: no IQ samples to record)
                    self.notice(client, "warning", "raw_unavailable");
                } else if self.raw_recorder.lock().is_none() {
                    let seconds = msg["seconds"].as_u64().unwrap_or(60).clamp(1, 3600) as u32;
                    let ch = self.channel.clone().unwrap_or_default();
                    match RawRecorder::start(
                        self.data_dir.join(crate::rawrec::DIR),
                        &ch,
                        self.frequency,
                        seconds,
                        &self.device.status().name,
                    ) {
                        Ok(r) => {
                            log::info!("raw recording started: {} ({seconds} s)", r.path.display());
                            *self.raw_recorder.lock() = Some(r);
                        }
                        Err(e) => self.notice(client, "error", &format!("raw recording: {e}")),
                    }
                }
                self.broadcast_raw_state();
            }
            "getGallery" => {
                let Some(sid) = msg["sid"].as_u64().map(|s| s as u32) else { return };
                let have: Vec<u64> = msg["have"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).collect()).unwrap_or_default();
                self.hub.send(client, text(&self.gallery.index_json(sid)));
                // the pictures only to a client that takes what it gets
                if self.hub.queue_free(client) < 256 {
                    return;
                }
                for (h, data) in self.gallery.images(sid, &have) {
                    self.hub.send(client, binary(BIN_IMAGE, &h, &data));
                }
            }
            "getSpiDocs" => {
                self.hub.send(client, text(&self.spi.docs_json()));
            }
            "getSpiDoc" => {
                let id = msg["id"].as_str().unwrap_or("");
                self.hub.send(client, text(&self.spi.doc_json(id)));
            }
            "getEpg" => {
                if let Some(sid) = msg["sid"].as_u64() {
                    self.hub.send(client, text(&self.spi.epg_json(sid as u32)));
                }
            }
            "getLogo" => {
                if let Some(id) = msg["id"].as_str() {
                    if let Some((h, data)) = self.spi.get(id) {
                        self.hub.send(client, binary(BIN_LOGO, &h, &data));
                    }
                }
            }
            "testDiscord" => {
                if self.hub.is_admin(client) {
                    let ev = changes::ChangeEvent {
                        time: now(),
                        channel: self.channel.clone().unwrap_or_default(),
                        eid: format!("{:04X}", self.ensemble.eid.unwrap_or(0)),
                        ensemble: self.ensemble.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
                        changes: vec![changes::Change {
                            kind: "test".into(),
                            service: None,
                            sid: None,
                            from: Some("MagicDAB".into()),
                            to: Some("test webhooka".into()),
                        }],
                        report: None,
                        recording: None,
                    };
                    changes::post_discord(&self.settings.discord_webhook, &self.settings.station_name, None, &ev);
                }
            }
            "listeners" | "kick" | "ban" | "unban" | "tuneLogin" | "tuneLogout" => self.on_access_message(client, kind, &msg),
            "dxrec" => self.on_dxrec_message(client, &msg),
            // (also at the gate of a private server: a pong tells nothing)
            "ping" => self.hub.send_always(client, text(&json!({"type": "pong", "t": msg["t"]}))),
            _ => log::debug!("unknown message type {kind}"),
        }
    }
}
