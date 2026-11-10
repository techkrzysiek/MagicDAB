//! Settings from the admin's Settings view: applied to the running receiver and saved.

use super::*;

impl Radio {
    pub(super) fn apply_settings(&mut self, client: u64, s: &Value) {
        let mut device_changed = false;
        if let Some(v) = s.get("stationName").and_then(|v| v.as_str()) {
            self.settings.station_name = v.to_string();
        }
        if let Some(v) = s.get("description").and_then(|v| v.as_str()) {
            self.settings.description = v.trim().to_string();
        }
        if let Some(v) = s.get("tunerLocked").and_then(|v| v.as_bool()) {
            self.settings.tuner_locked = v;
        }
        if let Some(v) = s.get("location") {
            self.settings.location = serde_json::from_value::<Location>(v.clone()).ok();
        }
        if let Some(v) = s.get("gain") {
            if let Ok(g) = serde_json::from_value::<GainMode>(v.clone()) {
                self.settings.gain = g.clone();
                self.device.set_gain_mode(g);
                self.apply_gain_ceiling();
            }
        }
        if let Some(v) = s.get("ppm").and_then(|v| v.as_i64()) {
            self.settings.ppm = v as i32;
            self.device.set_ppm(v as i32);
        }
        if let Some(v) = s.get("device") {
            match serde_json::from_value::<DeviceConfig>(v.clone()) {
                Ok(d) => {
                    if d != self.settings.device {
                        self.settings.device = d;
                        device_changed = true;
                    }
                }
                Err(e) => self.notice(client, "error", &format!("device: {e}")),
            }
        }
        if let Some(v) = s.get("idleScan") {
            if let Ok(i) = serde_json::from_value(v.clone()) {
                self.settings.idle_scan = i;
            }
        }
        if let Some(v) = s.get("opusBitrate").and_then(|v| v.as_u64()) {
            self.settings.opus_bitrate = (v as u32).clamp(32_000, 256_000);
            self.audio.set_bitrate(self.settings.opus_bitrate);
            self.slots.set_bitrate(self.settings.opus_bitrate);
        }
        if let Some(v) = s.get("tiiMode") {
            if let Ok(m) = serde_json::from_value::<TiiMode>(v.clone()) {
                self.settings.tii_mode = m;
                self.rx.send(RxCommand::TiiMode(m));
                self.slots.send_all(|| RxCommand::TiiMode(m));
            }
        }
        if let Some(v) = s.get("spi").and_then(|v| v.as_bool()) {
            self.settings.spi = v;
            self.rx.send(RxCommand::DataServices(v));
            self.slots.send_all(|| RxCommand::DataServices(v));
        }
        if let Some(v) = s.get("updateAuto").and_then(|v| v.as_bool()) {
            self.settings.update_auto = v;
        }
        if let Some(v) = s.get("openBrowser").and_then(|v| v.as_bool()) {
            self.settings.open_browser = v;
        }
        if let Some(v) = s.get("noIndex").and_then(|v| v.as_bool()) {
            self.settings.no_index = v;
        }
        if let Some(v) = s.get("chatEnabled").and_then(|v| v.as_bool()) {
            self.settings.chat_enabled = v;
        }
        if let Some(m) = s.get("notifyMute").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            self.settings.notify_mute = m;
        }
        if let Some(v) = s.get("sourceUrl").and_then(|v| v.as_str()) {
            self.settings.source_url = v.trim().to_string();
        }
        if let Some(v) = s.get("backgroundImage").and_then(|v| v.as_str()) {
            match config::background_url(v) {
                Some(u) => self.settings.background_image = u,
                None => self.notice(client, "error", "background_invalid"),
            }
        }
        // the first-run wizard finished or skipped
        if let Some(v) = s.get("setupPending").and_then(|v| v.as_bool()) {
            self.settings.setup_pending = v;
        }
        if let Some(v) = s.get("updateUrl").and_then(|v| v.as_str()) {
            // the built-in address is followed (later versions may move it)
            let v = v.trim();
            self.settings.update_url = if v == crate::update::DEFAULT_URL { String::new() } else { v.to_string() };
            self.update_available = None;
            self.update_error = None;
            self.update_checked = None;
        }
        if let Some(v) = s.get("fmEnabled").and_then(|v| v.as_bool()) {
            self.settings.fm.enabled = v;
            if v && self.fm.is_none() && crate::fmradio::AVAILABLE {
                if let Some(tx) = self.commands_tx.clone() {
                    self.fm = Some(FmRadio::start(self.settings.fm.port.clone(), self.settings.fm.audio.clone(), tx));
                }
            } else if !v && self.fm.is_some() {
                // stops the tuner and its audio; the follower lets go
                self.fm = None;
                self.linker = None;
                self.hub.broadcast_all(text(&json!({"type": "fm", "connected": false})));
            }
        }
        let port = s.get("fmPort").and_then(|v| v.as_str()).map(|v| v.trim().to_string());
        let audio = s.get("fmAudio").and_then(|v| v.as_str()).map(|v| v.trim().to_string());
        if port.is_some() || audio.is_some() {
            let changed =
                port.as_ref().is_some_and(|p| *p != self.settings.fm.port) || audio.as_ref().is_some_and(|a| *a != self.settings.fm.audio);
            if let Some(p) = port {
                self.settings.fm.port = p;
            }
            if let Some(a) = audio {
                self.settings.fm.audio = a;
            }
            // the tuner on its new port / sound card
            if changed && self.fm.is_some() {
                self.fm = None;
                self.linker = None;
                if let Some(tx) = self.commands_tx.clone() {
                    self.fm = Some(FmRadio::start(self.settings.fm.port.clone(), self.settings.fm.audio.clone(), tx));
                }
            }
        }
        if let Some(v) = s.get("linkingEnabled").and_then(|v| v.as_bool()) {
            self.settings.linking.enabled = v;
            self.follower.enabled = v;
        }
        if let Some(v) = s.get("linkingMatchLevel").and_then(|v| v.as_bool()) {
            self.settings.linking.match_level = v;
            if let Some(l) = self.linker.as_mut() {
                l.set_match_level(v);
            }
        }
        if let Some(v) = s.get("fmAntennas").and_then(|v| v.as_u64()) {
            self.settings.fm.antennas = (v as u8).clamp(1, 4);
            if let Some(fm) = self.fm.as_mut() {
                fm.antennas = self.settings.fm.antennas;
            }
        }
        if let Some(v) = s.get("radioVis").and_then(|v| v.as_bool()) {
            self.settings.radio_vis = v;
            self.update_vis();
        }
        if let Some(v) = s.get("internet").and_then(|v| v.as_bool()) {
            self.settings.internet = v;
        }
        // (the interface never has it: empty keeps it, null removes it)
        match s.get("discordWebhook") {
            Some(Value::Null) => self.settings.discord_webhook.clear(),
            Some(Value::String(v)) if !v.trim().is_empty() => self.settings.discord_webhook = v.trim().to_string(),
            _ => {}
        }
        if let Some(v) = s.get("tunnel") {
            if let Ok(mut t) = serde_json::from_value::<crate::tunnel::TunnelConfig>(v.clone()) {
                // (the interface never has the token: an empty one keeps it)
                if t.token.is_empty() {
                    t.token = self.settings.tunnel.token.clone();
                }
                self.settings.tunnel = t;
                let port = self.listen_port;
                self.tunnel.apply(&self.settings.tunnel, port);
                self.note_tunnel_server();
            }
        }
        if let Some(v) = s.get("channelMaps") {
            if let Ok(mut maps) = serde_json::from_value::<Vec<config::ChannelMap>>(v.clone()) {
                maps.retain(|m| channel_frequency(&m.channel).is_some() && !m.url.trim().is_empty());
                for m in maps.iter_mut() {
                    m.channel = m.channel.to_uppercase();
                    m.url = m.url.trim().to_string();
                }
                let cur = self.channel.clone();
                let before = cur.as_deref().and_then(|c| self.mapped_source(c));
                self.settings.channel_maps = maps;
                let after = cur.as_deref().and_then(|c| self.mapped_source(c));
                // the current channel changed its source: tune it again
                if before != after {
                    if let Some(c) = cur {
                        self.channel = None;
                        if !self.tune(&c) {
                            if let Some(first) = self.mapped_channels().first().cloned() {
                                self.tune(&first);
                            }
                        }
                    }
                }
            }
        }
        if let Some(v) = s.get("backgroundPad").and_then(|v| v.as_bool()) {
            self.settings.background_pad = v;
            // (the main receiver only: side receivers decode their station alone)
            self.rx.send(RxCommand::BackgroundPad(v));
        }
        if let Some(v) = s.get("listenerMultiplex").and_then(|v| v.as_bool()) {
            self.settings.listener_multiplex = v;
        }
        if let Some(v) = s.get("listenerMultiplexLimit").and_then(|v| v.as_u64()) {
            self.settings.listener_multiplex_limit = (v as usize).min(16);
        }
        if let Some(v) = s.get("tiiLog").and_then(|v| v.as_bool()) {
            self.settings.tii_log = v;
        }
        if let Some(v) = s.get("streams") {
            if let Ok(st) = serde_json::from_value::<config::StreamSettings>(v.clone()) {
                self.settings.streams = config::StreamSettings { max: st.max.clamp(1, 16), bitrate: st.bitrate.clamp(32, 256), ..st };
                // (a new bitrate takes new encoders; what may be streamed follows at once)
                self.stream_encoders.clear();
                self.streams_policy_at = Instant::now() - Duration::from_secs(2);
            }
        }
        if let Some(v) = s.get("maxServices").and_then(|v| v.as_u64()) {
            self.settings.max_services = (v as usize).clamp(1, 32);
        }
        // (from the next connection of a stream)
        if let Some(v) = s.get("etiDelayMs").and_then(|v| v.as_u64()) {
            self.settings.eti_delay_ms = v.min(10_000) as u32;
        }
        if let Some(v) = s.get("scanChannels").and_then(|v| v.as_array()) {
            self.settings.scan_channels = v.iter().filter_map(|c| c.as_str().map(|s| s.to_string())).collect();
        }
        if let Some(pw) = s.get("adminPassword").and_then(|v| v.as_str()) {
            if pw.len() >= 4 {
                self.settings.admin_password_hash = Some(config::hash_password(pw));
                log::info!("admin password changed");
            }
        }
        self.apply_access_settings(s);
        self.apply_gate_settings(s);
        if let Some(v) = s.get("serverMap") {
            crate::servermap::apply(&mut self.settings.server_map, v);
        }
        // (also after a new location, tunnel or name)
        self.server_map.configure(crate::servermap::Target::from_settings(&self.settings), s.get("serverMap").is_some());
        self.store.save(&self.settings);
        if device_changed {
            self.reopen_device();
        }
        // side slots the settings no longer allow (own multiplexes off, a map changed)
        self.check_slots();
        self.tii = self.tii_json();
        self.hub.broadcast_main(text(&self.tii));
        for id in self.hub.ids() {
            let h = self.hello_json(id);
            self.hub.send(id, text(&h));
            if self.hub.is_admin(id) {
                self.hub.send(id, text(&self.settings_json()));
            }
        }
        self.broadcast_state();
        self.broadcast_update();
        self.notice(client, "info", "settings_saved");
    }
}
