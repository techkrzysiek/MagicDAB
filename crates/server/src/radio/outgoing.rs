//! What the radio thread sends to clients: the hello and snapshot of a new client, the
//! state, signal, time and settings, the public status, and the JSON of these messages.

use super::*;

impl Radio {
    pub(super) fn ensemble_message(&mut self) -> hub::Out {
        if let Some(m) = &self.ensemble_msg {
            return m.clone();
        }
        let ch = self.channel.as_deref();
        let live = view::ensemble(&self.ensemble, &self.labels, ch, self.frequency);
        let mut v = match ch.and_then(|c| self.fig_cache.merged(c, &self.ensemble, &self.labels)) {
            Some((ens, labels, time)) => {
                let mut m = view::ensemble(&ens, &labels, ch, self.frequency);
                figcache::flag_cached(&mut m, &live, time);
                m
            }
            None => live,
        };
        v["type"] = json!("ensemble");
        // a service with a scrolling label keeps its place in the list
        if self.dyn_labels.any() {
            if let Some(list) = v["services"].as_array_mut() {
                // (two services may have the same name: then by SId, not by what they show)
                let key = |s: &Value| {
                    let sid = s["sid"].as_u64().unwrap_or(0) as u32;
                    (self.dyn_labels.usual(sid).unwrap_or_else(|| s["label"].as_str().unwrap_or("")).trim().to_lowercase(), sid)
                };
                list.sort_by_cached_key(key);
            }
        }
        let m = text(&v);
        self.ensemble_msg = Some(m.clone());
        m
    }

    /// Refresh the snapshot used by /api, /static_data and the link previews.
    pub(super) fn update_public(&self) {
        let ens = &self.ensemble;
        let key = self.main_service;
        let svc = key.and_then(|k| ens.services.get(&k.sid));
        let st = key.and_then(|k| self.services.get(&k));
        let country = ens.ecc.zip(ens.eid).and_then(|(e, id)| magicdab_dab::tables::country(e, (id >> 12) as u8));
        let primary = svc.and_then(|s| s.primary());
        let sc = primary.and_then(|c| c.subch_id).and_then(|id| ens.subchannels.get(&id));
        // FM frequencies of the service (FIG 0/21 for its PI)
        let af: Vec<f64> = svc
            .map(|s| {
                ens.frequency_info
                    .iter()
                    .filter(|f| (f.rm == 8 || f.rm == 9) && f.id == (s.sid & 0xFFFF))
                    .flat_map(|f| f.frequencies.iter().map(|e| e.khz as f64 / 1000.0))
                    .collect()
            })
            .unwrap_or_default();
        let tx = self.tii_codes.first().and_then(|c| {
            let t = self.txdb.find(ens.eid.unwrap_or(0), ens.ecc, c.main, c.sub, self.frequency)?;
            let (dist, azi) = match &self.settings.location {
                Some(l) => {
                    let (d, a) = distance_azimuth(l.lat, l.lon, t.lat, t.lon);
                    (Some(d), Some(a))
                }
                None => (None, None),
            };
            Some(crate::public::TxInfo {
                name: t.label.clone(),
                city: t.location.clone(),
                country: t.country.clone(),
                dist_km: dist,
                azimuth: azi,
                erp_kw: t.power_kw,
                pol: t.polarization.clone(),
                id: format!("{}-{}", c.main, c.sub),
            })
        });
        let logo = svc.and_then(|s| self.spi.best_logo(s.sid, s.ecc.or(ens.ecc).unwrap_or(0)));
        let info = crate::public::PublicInfo {
            no_index: self.settings.no_index,
            chat_enabled: self.settings.chat_enabled,
            station_name: self.settings.station_name.clone(),
            description: self.settings.description.clone(),
            lat: self.settings.location.as_ref().map(|l| l.lat),
            lon: self.settings.location.as_ref().map(|l| l.lon),
            channel: self.channel.clone(),
            frequency_khz: self.frequency,
            ensemble: ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
            eid: ens.eid,
            ecc: ens.ecc,
            country_name: country.map(|c| c.1.to_string()).unwrap_or_default(),
            country_iso: country.map(|c| c.0.to_string()).unwrap_or_else(|| "UN".into()),
            sid: svc.map(|s| s.sid),
            label: svc.and_then(|s| s.label.as_ref()).map(|l| l.text.clone()).unwrap_or_default(),
            short: svc.and_then(|s| s.label.as_ref()).map(|l| l.short.clone()).unwrap_or_default(),
            pty: svc.and_then(|s| s.pty),
            dls: st.and_then(|s| s.dls.as_ref()).map(|d| d.text.clone()).unwrap_or_default(),
            stereo: st.and_then(|s| s.format.as_ref()).map(|(f, _)| f.stereo).unwrap_or(false),
            bitrate: sc.map(|x| x.bitrate),
            codec: primary.map(|c| if c.ctype == 63 { "DAB+" } else { "DAB" }).unwrap_or("").to_string(),
            synced: self.signal.synced,
            snr: self.signal.snr,
            listeners: self.hub.count(),
            af,
            tx,
            logo,
            map_id: crate::servermap::ServerMap::listed_id(&self.settings),
            private: !self.settings.public_server,
        };
        *self.public.write() = info;
    }

    pub(super) fn publish_label(&mut self, key: ServiceKey, label: DynamicLabel) {
        let m = text(&dls_json(key, &label));
        self.services.entry(key).or_default().dls = Some(label.clone());
        self.streams.set_title(key.sid, &label.text);
        self.hub.broadcast(m, |c| c.service == Some(key) && c.on_main());
        // short "now playing" line of every service for the service list
        let all = json!({"type": "dlsAll", "sid": key.sid, "text": label.text});
        self.hub.broadcast(text(&all), |c| c.service != Some(key) && c.on_main());
    }

    pub(super) fn data_progress_json(&self) -> Value {
        data_progress_json(self.channel.as_deref(), &self.ensemble, &self.data_progress)
    }

    pub(super) fn state_json(&self) -> Value {
        let dev = self.device.status();
        json!({
            "type": "state",
            "channel": self.channel,
            "frequency": self.frequency,
            "device": dev,
            // only the kind: hosts and file paths are for the admin (settings)
            "deviceConfig": {"type": serde_json::to_value(&self.settings.device).ok().and_then(|v| v["type"].as_str().map(String::from))},
            "gain": self.settings.gain,
            "scan": self.scan.as_ref().map(|s| json!({
                "running": true,
                "silent": s.silent,
                "index": s.index,
                "total": s.channels.len(),
                "channel": s.channels.get(s.index),
                "found": s.found,
                "dx": s.dx.as_ref().map(|d| json!({"mode": d.mode, "cycle": d.cycle + 1, "cycles": d.cycles})),
            })),
            "listeners": self.hub.count(),
            "tunerLocked": self.settings.tuner_locked,
            "fixedChannel": self.fixed_channel,
            "input": if self.eti_input.is_some() { "eti" } else if self.dongle_active() { "dongle" } else { "rf" },
            "dongle": self.dtr100.as_ref().map(|d| d.status()),
            "etiConnected": self.eti_input.as_ref().map(|e| e.connected.load(std::sync::atomic::Ordering::Relaxed)),
            "mappedChannels": self.mapped_channels(),
            "recordedChannels": self.recorded_channels(),
            "noTuner": matches!(self.settings.device, DeviceConfig::None),
            "mainService": self.main_service.map(|k| k.sid),
            // the listener's receiver (0: the main one) and the main one's channel (radio/slots.rs)
            "slot": 0,
            "mainChannel": self.channel,
            // the computer cannot keep up with the receiver: % of the input lost, the receiver thread's CPU
            "overload": self.overload.active().then(|| json!({"lost": r1(self.overload.lost), "cpu": self.overload.cpu.map(|c| c.round())})),
        })
    }

    pub(super) fn broadcast_state(&self) {
        self.hub.broadcast_main(text(&self.state_json()));
        // (the listeners of the side slots see where the main receiver is)
        self.broadcast_side_state();
    }

    pub(super) fn signal_json(&self) -> Value {
        let mut v = signal_json(&self.signal, &self.device.status());
        if self.eti_input.is_some() {
            v["input"] = json!("eti");
        } else if let Some(d) = self.dtr100.as_ref().filter(|_| self.dongle_active()) {
            let st = d.status();
            v["input"] = json!("dongle");
            v["bars"] = json!(st.locked.map(|_| st.bars).unwrap_or(0));
            v["cer"] = json!(st.cer);
        }
        v
    }

    pub(super) fn time_json(&self) -> Value {
        time_json(self.time, &self.ensemble)
    }

    pub(super) fn hello_json(&self, client: u64) -> Value {
        json!({
            "type": "hello",
            "version": VERSION,
            "sourceUrl": if self.settings.source_url.is_empty() { SOURCE_URL } else { self.settings.source_url.as_str() },
            "clientId": client,
            "stationName": self.settings.station_name,
            "description": self.settings.description,
            "admin": self.hub.is_admin(client),
            "chat": self.settings.chat_enabled,
            "tunerLocked": self.settings.tuner_locked,
            "fixedChannel": self.fixed_channel,
            "mappedChannels": self.mapped_channels(),
            "recordedChannels": self.recorded_channels(),
            "noTuner": matches!(self.settings.device, DeviceConfig::None),
            "location": self.settings.location,
            "channels": BAND_III.iter().map(|(n, f)| json!({"name": n, "frequency": f})).collect::<Vec<_>>(),
            "txdb": { "count": self.txdb.count, "updated": self.txdb.updated },
            "opusBitrate": self.settings.opus_bitrate,
            // a tuning password exists, this client gave it; the channels of the listeners (empty: all)
            "tunePassword": self.settings.access.tune_password_hash.is_some(),
            "tuner": self.hub.update(client, |c| c.tuner).unwrap_or(false),
            "allowedChannels": self.settings.access.allowed_channels,
            // listeners tune the mapped channels on receivers of their own
            "listenerMultiplex": self.own_multiplex_on(),
            "backgroundImage": self.settings.background_image,
            // (everyone: visitors are asked to log in, the admin is walked through it)
            "setupPending": self.settings.setup_pending,
            // a private server: this client was let in (radio/gate.rs)
            "private": !self.settings.public_server,
        })
    }

    pub(super) fn service_list_json(&self) -> Value {
        json!({"type": "serviceList", "ensembles": self.service_list.ensembles})
    }

    pub(super) fn settings_json(&self) -> Value {
        let mut s = serde_json::to_value(&self.settings).unwrap_or(json!({}));
        if let Some(o) = s.as_object_mut() {
            o.remove("adminPasswordHash");
            if let Some(a) = o.get_mut("access").and_then(|a| a.as_object_mut()) {
                let set = a.remove("tunePasswordHash").is_some_and(|h| !h.is_null());
                a.insert("tunePasswordSet".into(), json!(set));
            }
            // (the token of the map entry stays in the server)
            if let Some(m) = o.get_mut("serverMap").and_then(|m| m.as_object_mut()) {
                m.remove("entry");
            }
        }
        json!({
            "type": "settings",
            "settings": s,
            "rtlsdrDevices": magicdab_sdr::list_rtlsdr(),
            // (false: the interface shows no FM settings, Android)
            "fmAvailable": crate::fmradio::AVAILABLE,
            "fmPorts": magicdab_fm::tuner::list_ports(),
            "fmAudioDevices": magicdab_fm::capture::list_devices(),
            "clients": self.hub.list(),
            "tunnel": self.tunnel.status(),
            "torExits": crate::access::get().tor_status(),
            "refusedNetworks": crate::access::get().networks_status(),
            "serverMap": self.server_map.status(),
            // set again at every start: changing them here lasts until then
            "passwordFromEnv": std::env::var("MAGICDAB_ADMIN_PASSWORD").is_ok_and(|p| !p.is_empty()),
            "deviceFromArgs": self.device_from_args,
        })
    }

    pub(super) fn send_snapshot(&mut self, client: u64) {
        let hello = self.hello_json(client);
        self.hub.send(client, text(&hello));
        self.hub.send(client, text(&self.state_json()));
        self.hub.send(client, text(&self.service_list_json()));
        let ens = self.ensemble_message();
        self.hub.send(client, ens);
        self.hub.send(client, text(&self.signal_json()));
        self.hub.send(client, text(&self.time_json()));
        self.hub.send(client, text(&self.tii));
        self.hub.send(client, text(&self.spi.list_json(&self.logo_sids())));
        self.hub.send(client, text(&self.spi.epg_index_json()));
        if !self.dx_results.is_empty() {
            self.hub.send(client, text(&self.dx_json()));
        }
        self.hub.send(client, text(&dls_lines_json(&self.services)));
        let recent: Vec<&changes::ChangeEvent> = self.changes.recent.iter().rev().take(200).collect();
        self.hub.send(client, text(&json!({"type": "changeLog", "events": recent})));
        if let Some(fm) = &self.fm {
            self.hub.send(client, text(&fm.state_json()));
            self.hub.send(client, text(&fm.scan_json()));
        }
        if !self.link_source.is_null() {
            self.hub.send(client, text(&self.link_source));
        }
        self.initial_pending.insert(client, Instant::now());
        if self.hub.is_admin(client) {
            self.hub.send(client, text(&self.settings_json()));
            self.send_dxrec_list(client);
        }
    }

    pub(super) fn send_service_data(&self, client: u64, key: ServiceKey) {
        let cached = self.slide_key(key.sid).and_then(|k| self.slide_cache.get(&k));
        send_service_state(&self.hub, client, key, self.services.get(&key), cached);
    }

    pub(super) fn notice(&self, client: u64, level: &str, message: &str) {
        self.hub.send(client, text(&json!({"type": "notice", "level": level, "message": message})));
    }
}

/// What a client gets of a service it selects: the last slide of the
/// multiplex (`cached`, unless one is being shown), format, DLS, slide.
pub(super) fn send_service_state(
    hub: &Hub,
    client: u64,
    key: ServiceKey,
    st: Option<&ServiceState>,
    cached: Option<&(Value, Arc<Vec<u8>>)>,
) {
    if !st.is_some_and(|st| st.slide.is_some()) {
        if let Some((h, data)) = cached {
            hub.send(client, binary(BIN_IMAGE, h, data));
        }
    }
    let Some(st) = st else { return };
    if let Some((f, br)) = &st.format {
        hub.send(client, text(&format_json(key, f, *br)));
    }
    if let Some(d) = &st.dls {
        hub.send(client, text(&dls_json(key, d)));
    }
    if let Some((h, data)) = &st.slide {
        hub.send(client, binary(BIN_IMAGE, h, data));
    }
    if let Some(p) = &st.slide_progress {
        hub.send(client, text(&slide_progress_json(key, Some(p))));
    }
    if let Some((h, data)) = &st.vis_slide {
        hub.send(client, binary(BIN_IMAGE, h, data));
    }
    if let Some(t) = &st.vis_text {
        hub.send(client, text_json_vis(key, t));
    }
    if let Some(s) = &st.stats {
        hub.send(client, text(&stats_json(key, s)));
    }
}

/// The "now playing" line of every service (the service list).
pub(super) fn dls_lines_json(services: &HashMap<ServiceKey, ServiceState>) -> Value {
    let lines: serde_json::Map<String, Value> =
        services.iter().filter_map(|(k, st)| st.dls.as_ref().map(|d| (k.sid.to_string(), json!(d.text)))).collect();
    json!({"type": "dlsAllBulk", "lines": lines})
}

/// "Label (SId)" of a service of an ensemble.
pub(super) fn service_label(ens: &Ensemble, sid: u32) -> String {
    let label = ens.services.get(&sid).and_then(|s| s.label.as_ref()).map(|l| l.text.trim().to_string());
    match label {
        Some(l) if !l.is_empty() => format!("{l} ({sid:04X})"),
        _ => format!("{sid:04X}"),
    }
}

pub(super) fn format_json(key: ServiceKey, f: &AudioFormat, bitrate: u16) -> Value {
    json!({
        "type": "audioFormat",
        "sid": key.sid,
        "scids": key.scids,
        "codec": f.codec,
        "sampleRate": f.sample_rate,
        "channels": f.channels,
        "sbr": f.sbr,
        "ps": f.ps,
        "stereo": f.stereo,
        "bitrate": bitrate,
    })
}

pub(super) fn signal_json(s: &SignalInfo, dev: &magicdab_sdr::DeviceStatus) -> Value {
    json!({
        "type": "signal",
        "synced": s.synced,
        "signal": s.signal,
        "snr": r1(s.snr),
        "nullSnr": r1(s.null_snr),
        "freqOffset": r1(s.freq_offset),
        "fibErrorRate": s.fib_error_rate,
        "ficBer": s.fic_ber,
        "fibTotal": s.fib_total,
        "fibErrors": s.fib_errors,
        "gain": dev.gain_db.map(r1),
        "gainIndex": dev.gain_index,
        "rfLevel": dev.rf_level.map(r1),
        "position": dev.position,
        "duration": dev.duration,
    })
}

pub(super) fn time_json(time: Option<DabTime>, ens: &Ensemble) -> Value {
    match time {
        Some(t) => json!({"type": "time", "utc": t.unix(), "lto": ens.lto.unwrap_or(0)}),
        None => json!({"type": "time", "utc": null}),
    }
}

pub(super) fn data_progress_json(channel: Option<&str>, ens: &Ensemble, progress: &DataProgress) -> Value {
    let items: Vec<Value> = progress
        .iter()
        .map(|((sid, scid), (ua, p, _))| {
            let label = ens.services.get(sid).and_then(|s| s.label.as_ref()).map(|l| l.text.clone());
            json!({"sid": format!("{sid:X}"), "scid": scid, "ua": ua, "label": label, "objects": p.objects, "objectsDone": p.objects_done, "bytes": p.bytes, "bytesDone": p.bytes_done, "segments": p.segments, "segmentsTotal": p.segments_total})
        })
        .collect();
    json!({"type": "dataProgress", "channel": channel, "items": items})
}

/// Note the progress of a carousel (the log tells when one starts and completes).
pub(super) fn note_data_progress(
    map: &mut DataProgress,
    ch: &str,
    sid: u32,
    scid: u16,
    ua_type: u16,
    progress: magicdab_dab::mot::CarouselProgress,
) {
    let e = match map.entry((sid, scid)) {
        std::collections::hash_map::Entry::Vacant(v) => {
            if progress.objects > 0 {
                log::info!(
                    "SPI {ch}: service {sid:X} (UA {ua_type:#05x}) carousel with {} objects, {} kB",
                    progress.objects,
                    progress.bytes / 1024
                );
            } else {
                log::info!("SPI {ch}: receiving service {sid:X} (UA {ua_type:#05x}), objects with their own headers");
            }
            v.insert((ua_type, progress, false))
        }
        std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
    };
    e.1 = progress;
    if progress.objects > 0 && progress.objects_done == progress.objects && !e.2 {
        e.2 = true;
        log::info!("SPI {ch}: carousel of service {sid:X} complete ({} objects)", progress.objects);
    }
}

pub(super) fn dls_json(key: ServiceKey, d: &DynamicLabel) -> Value {
    json!({"type": "dls", "sid": key.sid, "scids": key.scids, "text": d.text, "tags": d.tags, "itemToggle": d.item_toggle, "itemRunning": d.item_running})
}

pub(super) fn text_json_vis(key: ServiceKey, t: &str) -> hub::Out {
    text(&json!({"type": "visText", "sid": key.sid, "scids": key.scids, "text": t}))
}

/// "1h 5m", "3m 12s" or "40s".
pub(super) fn duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Slide being received (`progress` null when none is).
pub(super) fn slide_progress_json(key: ServiceKey, p: Option<&magicdab_dab::mot::ObjectProgress>) -> Value {
    json!({"type": "slideProgress", "sid": key.sid, "scids": key.scids, "progress": p})
}

pub(super) fn stats_json(key: ServiceKey, s: &AudioStats) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(json!({}));
    v["type"] = json!("stats");
    v["sid"] = json!(key.sid);
    v["scids"] = json!(key.scids);
    v
}
