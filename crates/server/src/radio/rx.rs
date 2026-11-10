//! Events of the main receiver: signal, ensemble, time, TII, spectrum, audio, labels, PAD
//! objects (slides, data services), ETI frames; and RadioDNS for the tuned multiplex.

use super::*;

impl Radio {
    pub(super) fn on_rx(&mut self, ev: RxEvent) {
        match ev {
            RxEvent::Retuned => {
                self.awaiting_retune = false;
            }
            RxEvent::Signal(s) => {
                if self.awaiting_retune {
                    return;
                }
                if self.scan.is_none() && self.gain_seek_active() && self.device_running {
                    let gain = self.device.status().gain_index;
                    if let Some(c) = self.gain_seek.on_signal(s.synced, s.snr, s.null_snr, gain, Instant::now()) {
                        self.device.set_gain_ceiling(Some(c.index), c.jump);
                    }
                }
                if self.scan.is_none() && self.eti_input.is_none() && s.synced != self.signal.synced {
                    let ch = self.channel.clone().unwrap_or_default();
                    if !s.synced {
                        self.sync_lost_at = Some(Instant::now());
                    } else if let Some(t) = self.sync_lost_at.take().filter(|t| t.elapsed() > Duration::from_secs(2)) {
                        log::info!("signal on {ch} was lost for {:.1} s (SNR now {:.1} dB)", t.elapsed().as_secs_f32(), s.snr);
                    }
                }
                self.follower.on_signal(s.synced, s.snr);
                self.signal = s;
                let v = self.signal_json();
                self.dxrec_signal(&v);
                self.hub.broadcast_main(text(&v));
            }
            RxEvent::Ensemble(e, labels) => {
                if self.awaiting_retune {
                    return;
                }
                let identified = self.ensemble.eid.is_none() && e.eid.is_some();
                if let (true, Some(ch), Some(eid)) = (identified, self.channel.as_deref(), e.eid) {
                    self.dyn_labels.set_mux(format!("{ch}_{eid:04X}"));
                }
                for (sid, main) in self.dyn_labels.observe(&self.ensemble, &e, Instant::now()) {
                    // the usual name: as the service list knows the station on this multiplex
                    let known = self
                        .service_list
                        .ensembles
                        .iter()
                        .find(|x| Some(x.channel.as_str()) == self.channel.as_deref() && Some(x.eid) == e.eid)
                        .and_then(|x| x.services.iter().find(|s| s.sid == sid))
                        .map(|s| s.label.clone());
                    let ecc = e.services.get(&sid).and_then(|s| s.ecc).or(e.ecc).unwrap_or(0);
                    self.dyn_labels.set_hint(sid, self.spi.service_name(ecc, sid));
                    self.dyn_labels.pin(sid, known.unwrap_or(main));
                    let kept = self.dyn_labels.usual(sid).unwrap_or("").trim().to_string();
                    log::info!("{sid:04X} changes its label all the time (like a scrolling RDS PS): the service list and the change log keep \"{kept}\"");
                }
                self.ensemble = e;
                self.labels = labels;
                self.update_vis();
                if self.last_fig_observe.elapsed() > Duration::from_secs(10) {
                    self.observe_fig_cache();
                }
                self.ensemble_msg = None;
                self.ensemble_changed = Instant::now();
                let m = self.ensemble_message();
                self.hub.broadcast_main(m);
                if identified {
                    self.send_cached_slides();
                }
                self.update_decoding();
                if self.ensemble.eid.is_some() {
                    let now = Instant::now();
                    self.list_dirty = Some((self.list_dirty.map_or(now, |d| d.0), now));
                    if self.ensemble.ecc.is_some() {
                        self.request_radiodns();
                    }
                }
            }
            RxEvent::Time(t) => {
                self.time = Some(t);
                self.hub.broadcast_main(text(&self.time_json()));
            }
            RxEvent::Tii { codes, pairs } => {
                if self.awaiting_retune {
                    return;
                }
                self.tii_codes = codes;
                self.tii = self.tii_json();
                self.hub.broadcast_main(text(&self.tii));
                if self.dxrec_recording() {
                    let rec = self.tii_for_dxrec(&pairs);
                    self.dxrec_side(crate::dxrec::SideKind::Tii, rec);
                }
                let pm = json!({"type": "tiiPairs", "pairs": pairs.iter().map(|p| r1(*p)).collect::<Vec<_>>()});
                self.hub.broadcast(text(&pm), |c| c.subs.tii_pairs && c.on_main());
                if let Some(scan) = self.scan.as_mut() {
                    for c in &self.tii_codes {
                        let code = format!("{}-{}", c.main, c.sub);
                        if !scan.tii.contains(&code) {
                            scan.tii.push(code);
                        }
                        if let Some(dx) = scan.dx.as_mut() {
                            let e = dx.tii.entry((c.main, c.sub)).or_insert(f32::MIN);
                            *e = e.max(c.level_db);
                        }
                    }
                }
                self.log_tii();
            }
            RxEvent::Spectrum { spectrum, null, freq_offset, constellation, impulse, advance, noise } => {
                let n = spectrum.len();
                let k = impulse.len();
                let mut payload = Vec::with_capacity(n * 8 + constellation.len() * 8 + k * 4);
                for src in [&spectrum, &null] {
                    for i in 0..n {
                        // FFT order -> ascending frequency
                        let v = src[(i + n / 2) % n];
                        payload.extend_from_slice(&(10.0 * v.max(1e-20).log10()).to_le_bytes());
                    }
                }
                // DQPSK points (re, im), then the power delay profile in dB from
                // -k/2 to k/2 samples around the start of the FFT window
                for c in &constellation {
                    payload.extend_from_slice(&c.re.to_le_bytes());
                    payload.extend_from_slice(&c.im.to_le_bytes());
                }
                for j in 0..k {
                    payload.extend_from_slice(&(10.0 * impulse[(j + k / 2) % k].max(1e-20).log10()).to_le_bytes());
                }
                // without synchronisation there is no null symbol
                let has_null = null.iter().any(|&v| v > 0.0);
                let h = json!({
                    "frequency": self.frequency, "offset": freq_offset, "bins": n, "rate": 2_048_000, "null": has_null,
                    "points": constellation.len(), "taps": k, "advance": advance, "guard": T_G,
                    "noise": r1(10.0 * noise.max(1e-20).log10()),
                });
                self.hub.broadcast(binary(BIN_SPECTRUM, &h, &payload), |c| c.subs.spectrum && c.on_main());
                self.dxrec_spectrum(h, payload);
            }
            RxEvent::Audio { key, pcm } => {
                if self.clips.contains_key(&key) {
                    self.clips_feed(key, &pcm);
                }
                if key.scids == 0 && self.streams_wanted.contains(&key.sid) {
                    self.streams_feed(key, &pcm);
                }
                let now = Instant::now();
                if let Some(gap) = self.last_audio.insert(key, now).map(|t| now - t) {
                    // (from half a second on, which a listener's buffer may not
                    // bridge, when someone listens: the dropouts the browsers
                    // report are then the server's, not the network's)
                    let heard = self.wants.get(&key).is_some_and(|w| w.0 || w.1);
                    if gap > Duration::from_millis(if heard { 500 } else { 1500 }) && gap < Duration::from_secs(600) {
                        log::info!(
                            "audio of {} interrupted for {:.1} s ({})",
                            self.service_name(key.sid),
                            gap.as_secs_f32(),
                            self.input_state()
                        );
                    }
                }
                let (o, p) = self.wants.get(&key).copied().unwrap_or((false, false));
                match self.linker.as_mut().filter(|_| self.follower.service() == Some(key)) {
                    Some(l) => {
                        let out = l.push_dab(&pcm, now);
                        if !out.is_empty() {
                            self.audio.push(&self.hub, key, &out, o, p);
                        }
                    }
                    None => self.audio.push(&self.hub, key, &pcm, o, p),
                }
            }
            RxEvent::AudioFormat { key, format, bitrate } => {
                let m = text(&format_json(key, &format, bitrate));
                // (the service list keeps it: the server map shows what a station sends)
                if let (0, Some(ch), Some(eid)) = (key.scids, self.channel.clone(), self.ensemble.eid) {
                    if self.service_list.set_format(&ch, eid, key.sid, &FormatEntry::of(&format)) {
                        self.service_list.save(&self.data_dir);
                    }
                }
                self.services.entry(key).or_default().format = Some((format, bitrate));
                self.hub.broadcast(m, |c| c.service == Some(key) && c.on_main());
            }
            RxEvent::Label { key, label } => {
                if label.text.trim().is_empty() {
                    // Some stations send "clear display" right before every new
                    // text: clear only if no new text follows shortly.
                    self.pending_clears.insert(key, Instant::now());
                    return;
                }
                self.pending_clears.remove(&key);
                self.publish_label(key, label);
            }
            RxEvent::Stats { key, stats, .. } => {
                if self.follower.service() == Some(key) {
                    self.follower.on_stats(&stats, Instant::now());
                }
                if let Some(r) = self.clips.get_mut(&key) {
                    r.note_errors(stats.au_errors);
                }
                let m = text(&stats_json(key, &stats));
                self.services.entry(key).or_default().stats = Some(stats);
                self.hub.broadcast(m, |c| c.service == Some(key) && c.on_main());
            }
            RxEvent::Encoded { key, data, aac } => {
                let h = json!({"sid": key.sid, "aac": aac});
                self.hub.broadcast(binary(BIN_ENCODED, &h, &data), |c| c.record && c.service == Some(key) && c.on_main());
            }
            RxEvent::DataObject { ua_type, object, .. } => {
                if ua_type == 0x007 && self.spi.on_object(&self.ensemble, &object) {
                    self.logos_dirty = true;
                    self.logos_changed = Some(Instant::now());
                }
                self.observe_si();
            }
            RxEvent::DataProgress { sid, scid, ua_type, progress } => {
                let ch = self.channel.clone().unwrap_or_default();
                note_data_progress(&mut self.data_progress, &ch, sid, scid, ua_type, progress);
                self.hub.broadcast_main(text(&self.data_progress_json()));
            }
            RxEvent::XpadProgress { key, objects } => {
                let app = self.slideshow_app_type(key);
                let st = self.services.entry(key).or_default();
                // a repetition of the picture shown (e.g. received by the background
                // decoder before) is not a new slide
                let progress = objects.into_iter().find(|(a, _)| *a == app).map(|(_, p)| p).filter(|p| {
                    !st.slide.as_ref().is_some_and(|(h, _)| {
                        h["transportId"] == p.transport_id && h["size"] == p.size && h["contentName"] == p.content_name.as_str()
                    })
                });
                if progress != st.slide_progress {
                    let m = slide_progress_json(key, progress.as_ref());
                    st.slide_progress = progress;
                    self.hub.broadcast(text(&m), |c| c.service == Some(key) && c.on_main());
                }
            }
            RxEvent::XpadObject { key, app_type, object } => {
                if self.xpad_ua_type(key, app_type) == Some(0x007) {
                    if self.spi.on_object(&self.ensemble, &object) {
                        self.logos_dirty = true;
                        self.logos_changed = Some(Instant::now());
                    }
                    self.observe_si();
                    return;
                }
                let slideshow_app = self.slideshow_app_type(key);
                // only real pictures: some broadcasters send other files as slides
                // (e.g. a Windows Thumbs.db named 0000.jpg); keep them for inspection
                if object.is_image() && app_type == slideshow_app && crate::spi::sniff_mime(&object.body) == "application/octet-stream" {
                    let name: String =
                        object.content_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
                    let name = format!("{:04X}-{name}.bin", key.sid);
                    if self.rejected.len() < 1000 && self.rejected.insert(name.clone()) {
                        let kind = if object.body.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
                            "OLE2 file (Windows Thumbs.db?)"
                        } else {
                            "unknown data"
                        };
                        let label =
                            self.ensemble.services.get(&key.sid).and_then(|s| s.label.as_ref()).map(|l| l.text.clone()).unwrap_or_default();
                        log::warn!(
                            "slideshow of {label} ({:04X}): '{}' is not a picture but {kind}, {} bytes – ignored",
                            key.sid,
                            object.content_name,
                            object.body.len()
                        );
                        // a few kept for inspection, not a disk full of them
                        let dir = self.data_dir.join("rejected");
                        let _ = std::fs::create_dir_all(&dir);
                        if std::fs::read_dir(&dir).map(|d| d.count() < 20).unwrap_or(false) {
                            let _ = std::fs::write(dir.join(&name), &object.body);
                        }
                    }
                    return;
                }
                if object.is_image() && app_type == slideshow_app {
                    let shown = self.services.get(&key).and_then(|st| st.slide.as_ref());
                    if shown.is_some_and(|(h, data)| {
                        h["transportId"] == object.transport_id && h["contentName"] == object.content_name.as_str() && **data == object.body
                    }) {
                        // repetition of the picture shown
                        return;
                    }
                    // categorised slideshow: category id and slide id
                    let catslide = slide_category(&object);
                    // category 0 is no category: it takes the slide out of its
                    // category (TS 101 499 5.3.5; encoders also send 0/0 for plain slides)
                    if let Some((0, slide)) = catslide.filter(|&(_, s)| s != 0) {
                        self.gallery.decategorise(key.sid, slide);
                    }
                    let catslide = catslide.filter(|&(c, s)| c != 0 && s != 0);
                    let h = slide_header(key, &object, catslide);
                    let data = Arc::new(object.body);
                    self.gallery.add(key.sid, catslide, &h, &data);
                    self.hub.broadcast(binary(BIN_IMAGE, &h, &data), |c| c.service == Some(key) && c.on_main());
                    self.store_slide(key.sid, &h, &data);
                    self.services.entry(key).or_default().slide = Some((h, data));
                }
            }
            RxEvent::Eti(frame) => {
                // (frames of the multiplex left behind)
                if !self.awaiting_retune {
                    self.dxrec_on_frame(frame);
                }
            }
            RxEvent::ServiceStopped { .. } => {}
        }
    }

    fn xpad_ua_type(&self, key: ServiceKey, app_type: u8) -> Option<u16> {
        xpad_ua(&self.ensemble, key, app_type)
    }

    /// Ask RadioDNS for logos of the audio services of the current ensemble.
    fn request_radiodns(&mut self) {
        if !self.settings.internet || self.scan.is_some() {
            return;
        }
        let Some(tx) = self.radiodns.clone() else { return };
        let Some(eid) = self.ensemble.eid else { return };
        let known = Arc::new(self.logo_sids());
        for s in self.ensemble.services.values().filter(|s| s.is_audio() && !s.is_data) {
            let Some(ecc) = s.ecc.or(self.ensemble.ecc) else { continue };
            let done = self.radiodns_done.get(&(eid, s.sid));
            if done.is_some_and(|t| t.elapsed() < Duration::from_secs(12 * 3600)) {
                continue;
            }
            if self.spi.has_radiodns_logos(s.sid, ecc) && done.is_some() {
                continue;
            }
            self.radiodns_done.insert((eid, s.sid), Instant::now());
            let other_eids = self
                .service_list
                .ensembles
                .iter()
                .filter(|e| e.eid != eid && e.services.iter().any(|x| x.sid == s.sid && x.ecc.unwrap_or(e.ecc) == ecc))
                .map(|e| e.eid)
                .collect();
            let _ = tx.send(radiodns::Request { sid: s.sid, ecc, eid, scids: 0, other_eids, known: known.clone() });
        }
    }

    fn slideshow_app_type(&self, key: ServiceKey) -> u8 {
        slideshow_app(&self.ensemble, key)
    }

    pub(super) fn tii_json(&self) -> Value {
        tii_view(
            &self.txdb,
            self.settings.location.as_ref(),
            &self.tii_codes,
            self.ensemble.eid.unwrap_or(0),
            self.ensemble.ecc,
            self.frequency,
        )
    }

    /// TII heard while a DX recording runs goes into it (dxrec.rs).
    fn tii_for_dxrec(&self, pairs: &[f32]) -> Vec<u8> {
        let codes: Vec<Value> = self.tii_codes.iter().map(|c| json!([c.main, c.sub, r1(c.level_db), r1(c.margin_db)])).collect();
        serde_json::to_vec(&json!({"codes": codes, "pairs": pairs.iter().map(|p| r1(*p)).collect::<Vec<_>>()})).unwrap_or_default()
    }

    fn log_tii(&self) {
        if !self.settings.tii_log || self.tii_codes.is_empty() {
            return;
        }
        let dir = self.data_dir.join("tii");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("tii-{}.csv", crate::localtime::now().format("%Y-%m-%d")));
        let new = !file.exists();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
            if new {
                let _ = writeln!(f, "time;channel;frequency;eid;label;main;sub;level;snr;location");
            }
            let label = self.ensemble.label.as_ref().map(|l| l.text.clone()).unwrap_or_default();
            for c in &self.tii_codes {
                let tx = self.txdb.find(self.ensemble.eid.unwrap_or(0), self.ensemble.ecc, c.main, c.sub, self.frequency);
                let _ = writeln!(
                    f,
                    "{};{};{};{:04X};{};{};{};{:.1};{:.1};{}",
                    crate::localtime::now().format("%Y-%m-%d %H:%M:%S"),
                    self.channel.clone().unwrap_or_default(),
                    self.frequency,
                    self.ensemble.eid.unwrap_or(0),
                    label,
                    c.main,
                    c.sub,
                    c.level_db,
                    self.signal.snr,
                    tx.map(|t| t.location.clone()).unwrap_or_default()
                );
            }
        }
    }
}

/// The TII message: the codes heard with their transmitters (distance and
/// azimuth from the receiver), all known transmitters of the ensemble (the map).
pub(super) fn tii_view(txdb: &TxDb, loc: Option<&config::Location>, tii: &[TiiCode], eid: u16, ecc: Option<u8>, frequency: u32) -> Value {
    let codes: Vec<Value> = tii
        .iter()
        .map(|c| {
            let tx = txdb.find(eid, ecc, c.main, c.sub, frequency);
            let (dist, az) = match (loc, tx) {
                (Some(l), Some(t)) => {
                    let (d, a) = distance_azimuth(l.lat, l.lon, t.lat, t.lon);
                    (Some((d * 10.0).round() / 10.0), Some((a * 10.0).round() / 10.0))
                }
                _ => (None, None),
            };
            json!({
                "main": c.main,
                "sub": c.sub,
                "code": format!("{}-{}", c.main, c.sub),
                "level": r1(c.level_db),
                "margin": r1(c.margin_db),
                "tx": tx,
                "distance": dist,
                "azimuth": az,
            })
        })
        .collect();
    let known: Vec<Value> = txdb
        .ensemble(eid)
        .into_iter()
        .filter(|t| ecc.map(|e| t.ecc == 0 || t.ecc == e).unwrap_or(true))
        .map(|t| json!({"code": format!("{}-{}", t.main, t.sub), "location": t.location, "lat": t.lat, "lon": t.lon, "power": t.power_kw, "frequency": t.frequency}))
        .collect();
    json!({"type": "tii", "codes": codes, "known": known, "eid": format!("{eid:04X}"), "frequency": frequency})
}

/// The codes as a DX recording keeps them: [main, sub, level dB, margin dB].
pub(super) fn tii_codes_from(v: &Value) -> Vec<TiiCode> {
    let n = |x: &Value| x.as_f64().unwrap_or(0.0) as f32;
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(TiiCode {
                main: u8::try_from(c[0].as_u64()?).ok()?,
                sub: u8::try_from(c[1].as_u64()?).ok()?,
                level_db: n(&c[2]),
                strength: 0.0,
                margin_db: n(&c[3]),
            })
        })
        .collect()
}

/// UA type of the X-PAD application `app_type` of a service.
pub(super) fn xpad_ua(ens: &Ensemble, key: ServiceKey, app_type: u8) -> Option<u16> {
    ens.services
        .get(&key.sid)
        .and_then(|s| s.primary())
        .and_then(|c| c.user_apps.iter().find(|u| u.xpad_app_type == Some(app_type)).map(|u| u.ua_type))
}

/// X-PAD application type of the slideshow of a service.
pub(super) fn slideshow_app(ens: &Ensemble, key: ServiceKey) -> u8 {
    ens.services
        .get(&key.sid)
        .and_then(|s| s.primary())
        .and_then(|c| c.user_apps.iter().find(|u| u.ua_type == 2).and_then(|u| u.xpad_app_type))
        .unwrap_or(12)
}
