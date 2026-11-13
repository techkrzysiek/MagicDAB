//! Band scans: the full scan, the silent one in the background and the DX scan, with its
//! results (CSV).

use super::*;

impl Radio {
    pub(super) fn start_scan(&mut self, silent: bool) {
        self.start_scan_with(silent, None, None, Vec::new());
    }

    /// `only`: the channels a listener may tune to (empty: no limit). False
    /// when no scan was started.
    pub(super) fn start_scan_with(
        &mut self,
        silent: bool,
        dx: Option<DxScan>,
        channels_override: Option<Vec<String>>,
        only: Vec<String>,
    ) -> bool {
        if self.scan.is_some() || self.fixed_channel.is_some() {
            return false;
        }
        // (a tuner that does not open: the scan would find nothing, and say so)
        if self.device_error.is_some() && !self.device_running && self.eti_input.is_none() && self.dtr100.is_none() {
            log::info!("band scan not started: the input device does not work");
            return false;
        }
        let mut channels: Vec<String> = if let Some(c) = channels_override.filter(|c| !c.is_empty()) {
            c
        } else if self.settings.scan_channels.is_empty() {
            BAND_III.iter().map(|(n, _)| n.to_string()).collect()
        } else {
            self.settings.scan_channels.clone()
        };
        if matches!(self.settings.device, DeviceConfig::None) {
            channels = self.mapped_channels();
            if channels.is_empty() {
                return false;
            }
        }
        if let DeviceConfig::RawFile { .. } = self.settings.device {
            // a recording only contains one channel
            let ch = self
                .device
                .status()
                .frequency
                .checked_div(1000)
                .and_then(magicdab_dab::consts::channel_name)
                .map(|c| c.to_string())
                .or_else(|| self.channel.clone());
            channels = ch.into_iter().collect();
            if channels.is_empty() {
                return false;
            }
        }
        // a listener scans only the channels they may tune to
        if !only.is_empty() {
            channels.retain(|c| crate::access::channel_allowed(&only, c));
            if channels.is_empty() {
                return false;
            }
        }
        log::info!(
            "{} started ({} channels{})",
            if dx.is_some() { "DX scan" } else { "band scan" },
            channels.len(),
            if silent { ", background" } else { "" }
        );
        let return_to = self.channel.clone().map(|c| (c, self.main_service));
        self.scan = Some(BandScan {
            channels,
            index: 0,
            phase: ScanPhase::Settle,
            since: Instant::now(),
            found: 0,
            silent,
            return_to,
            tii: Vec::new(),
            dx,
            other_gain: false,
            started: now(),
            report: Vec::new(),
            found_here: Vec::new(),
        });
        self.update_decoding();
        let first = self.scan.as_ref().unwrap().channels[0].clone();
        self.channel = None;
        self.tune(&first);
        true
    }

    pub(super) fn stop_scan(&mut self) {
        let Some(scan) = self.scan.take() else { return };
        // (clips of a scan cut short are not finished)
        self.clips.clear();
        if scan.dx.is_none() && scan.index >= scan.channels.len() {
            // the whole band: what each channel gave, received on air
            let channels: Vec<Value> = scan
                .report
                .into_iter()
                .filter(|c| c["channel"].as_str().is_some_and(|ch| crate::servermap::source(&self.settings, ch) == "air"))
                .collect();
            if !channels.is_empty() {
                self.server_map.send_scan(json!({"started": scan.started, "finished": now(), "channels": channels}));
            }
        }
        if scan.dx.is_some() {
            log::info!("DX scan stopped: {} results", self.dx_results.len());
            self.save_dx_csv();
        } else {
            log::info!("band scan finished: {} ensembles", scan.found);
            self.service_list.save(&self.data_dir);
            self.hub.broadcast_all(text(&self.service_list_json()));
            // (logos of stations new to the list)
            self.logos_dirty = true;
            self.logos_changed = Some(Instant::now());
        }
        // back where it was; but from an empty channel and with no station
        // chosen (the first scan of a new installation) to the strongest
        // multiplex found, not to silence
        let return_to = scan.return_to.map(|(ch, svc)| {
            let empty = svc.is_none() && !self.service_list.ensembles.iter().any(|e| e.channel == ch);
            let best = self.service_list.ensembles.iter().max_by(|a, b| a.snr.total_cmp(&b.snr)).map(|e| e.channel.clone());
            match best.filter(|_| empty && scan.dx.is_none()) {
                Some(b) => (b, None),
                None => (ch, svc),
            }
        });
        if let Some((ch, svc)) = return_to {
            self.channel = None;
            self.tune(&ch);
            self.main_service = svc;
            if let Some(k) = svc {
                for id in self.hub.ids_in(0) {
                    self.hub.update(id, |c| c.service = Some(k));
                }
                self.hub.broadcast_main(text(&json!({"type": "select", "sid": k.sid, "scids": k.scids, "reason": "scan"})));
            }
        }
        self.update_decoding();
        self.broadcast_state();
    }

    pub(super) fn scan_step(&mut self) {
        let Some(scan) = self.scan.as_mut() else { return };
        let elapsed = scan.since.elapsed();
        let mut next = false;
        // nothing at this gain: try the channel at another one before moving on
        let mut other_gain = false;
        match scan.phase {
            ScanPhase::Settle => {
                if elapsed > Duration::from_millis(400) {
                    scan.phase = ScanPhase::Wait;
                    scan.since = Instant::now();
                }
            }
            ScanPhase::Hold => {
                if !self.holds_scan() {
                    next = true;
                }
            }
            ScanPhase::Clips => {
                if self.clips_done(elapsed) {
                    self.clips_finish();
                    next = true;
                }
            }
            ScanPhase::Wait if scan.dx.is_some() => {
                let dwell = scan.dx.as_ref().unwrap().mode.dwell();
                if let Some(dx) = scan.dx.as_mut() {
                    if self.signal.synced {
                        dx.snr = dx.snr.max(self.signal.snr);
                    }
                }
                let ens = &self.ensemble;
                if !self.signal.signal && elapsed > Duration::from_millis(1500) {
                    other_gain = true;
                } else if !self.signal.synced && elapsed > dwell.max(Duration::from_secs(5)) {
                    log::debug!("scan: {} signal without sync", scan.channels[scan.index]);
                    other_gain = true;
                } else if elapsed >= dwell && ens.eid.is_some() && ens.label.is_some() {
                    let rows = self.dx_rows();
                    let n = rows.len();
                    self.dx_results.extend(rows);
                    if self.dx_results.len() > 5000 {
                        let drop = self.dx_results.len() - 5000;
                        self.dx_results.drain(..drop);
                    }
                    self.dx_dirty = true;
                    let scan = self.scan.as_mut().unwrap();
                    scan.found += (n > 0) as usize;
                    next = true;
                } else if elapsed > dwell + Duration::from_secs(10) {
                    next = true;
                }
            }
            ScanPhase::Wait => {
                let ens = &self.ensemble;
                let complete = ens.eid.is_some()
                    && ens.label.is_some()
                    && !ens.services.is_empty()
                    && ens.services.values().all(|s| s.label.is_some())
                    && self.ensemble_changed.elapsed() > Duration::from_millis(2500);
                // a synced ensemble with FIC errors gets more time to complete its labels
                let weak = self.signal.synced && self.signal.fib_error_rate > 0.01;
                if (!self.signal.signal && elapsed > Duration::from_millis(1500))
                    || (!self.signal.synced && elapsed > Duration::from_millis(4000))
                {
                    other_gain = true;
                } else if complete && elapsed > Duration::from_millis(3000) {
                    let ch = scan.channels[scan.index].clone();
                    if scan.other_gain {
                        if let Some(db) = self.device.status().gain_db {
                            self.gain_seek.learn_db(&ch, db);
                        }
                    }
                    let stable = self.dyn_labels.stable(ens);
                    if self.service_list.update(&ch, self.frequency, &stable, self.signal.snr) {
                        log::info!(
                            "scan: {} {} ({} services)",
                            ch,
                            ens.label.as_ref().map(|l| l.text.as_str()).unwrap_or(""),
                            ens.services.len()
                        );
                    }
                    if let Some(e) = self.service_list.ensembles.iter_mut().find(|e| e.channel == ch && Some(e.eid) == ens.eid) {
                        e.tii = scan.tii.clone();
                    }
                    scan.found_here.push(json!({
                        "eid": format!("{:04X}", ens.eid.unwrap_or(0)), "ecc": format!("{:02X}", ens.ecc.unwrap_or(0)),
                        "label": ens.label.as_ref().map(|l| l.text.trim().to_string()).unwrap_or_default(),
                        "snr": (self.signal.snr as f64 * 10.0).round() / 10.0, "tii": scan.tii.clone(),
                    }));
                    scan.found += 1;
                    next = true;
                    self.observe_changes();
                    // a multiplex never heard here: recorded first (radio/dxrec.rs) and
                    // listened to for its report (radio/newmux.rs)
                    if self.holds_scan() {
                        self.scan.as_mut().unwrap().phase = ScanPhase::Hold;
                        next = false;
                    } else if self.scan.as_ref().is_some_and(|s| s.silent) && self.clips_start(&ch) {
                        // a few stations recorded for the server map (radio/clips.rs)
                        let scan = self.scan.as_mut().unwrap();
                        scan.phase = ScanPhase::Clips;
                        scan.since = Instant::now();
                        next = false;
                    }
                } else if elapsed > Duration::from_secs(if weak { 25 } else { 12 }) {
                    // keep what could be decoded of a weak ensemble
                    let ch = scan.channels[scan.index].clone();
                    let stable = self.dyn_labels.stable(ens);
                    if ens.eid.is_some() && ens.label.is_some() && self.service_list.update(&ch, self.frequency, &stable, self.signal.snr) {
                        log::info!(
                            "scan: {} {} (incomplete, {} services)",
                            ch,
                            ens.label.as_ref().map(|l| l.text.as_str()).unwrap_or(""),
                            ens.services.len()
                        );
                        scan.found += 1;
                    }
                    if let (Some(eid), Some(label)) = (ens.eid, ens.label.as_ref()) {
                        scan.found_here.push(json!({
                            "eid": format!("{eid:04X}"), "ecc": format!("{:02X}", ens.ecc.unwrap_or(0)), "label": label.text.trim(),
                            "snr": (self.signal.snr as f64 * 10.0).round() / 10.0, "tii": scan.tii.clone(),
                        }));
                    }
                    next = true;
                }
            }
        }
        // a multiplex from afar: recorded, the scan waits on it (radio/dxrec.rs)
        let done = self.scan.as_ref().is_some_and(|s| matches!(s.phase, ScanPhase::Wait | ScanPhase::Clips));
        if next && done && self.ensemble.eid.is_some() && self.dxrec_far(&self.scan_heard()) && self.holds_scan() {
            let scan = self.scan.as_mut().unwrap();
            scan.phase = ScanPhase::Hold;
            next = false;
        }
        if other_gain && !self.scan_other_gain() {
            next = true;
        }
        if next {
            // a band scan looked at the channel: what it did not find there
            // missed once more, and what is gone for a day leaves the list
            if let Some(s) = self.scan.as_ref().filter(|s| s.dx.is_none()) {
                let ch = s.channels[s.index].clone();
                let eids: Vec<u16> =
                    s.found_here.iter().filter_map(|f| f["eid"].as_str().and_then(|e| u16::from_str_radix(e, 16).ok())).collect();
                self.service_list.scanned(&ch, &eids);
                for e in self.service_list.prune_gone(now()) {
                    log::info!(
                        "{} {} ({:02X}{:04X}) left the station list: not received for {} h",
                        e.channel,
                        e.label.trim(),
                        e.ecc,
                        e.eid,
                        (now() - e.last_seen) / 3600
                    );
                }
            }
            let scan = self.scan.as_mut().unwrap();
            let found = std::mem::take(&mut scan.found_here);
            scan.report.push(json!({"channel": scan.channels[scan.index], "found": found}));
            scan.index += 1;
            scan.other_gain = false;
            scan.tii.clear();
            if let Some(dx) = scan.dx.as_mut() {
                dx.tii.clear();
                dx.snr = 0.0;
            }
            if scan.index >= scan.channels.len() {
                let again = match scan.dx.as_mut() {
                    Some(dx) => {
                        dx.cycle += 1;
                        dx.cycles == 0 || dx.cycle < dx.cycles
                    }
                    None => false,
                };
                if !again {
                    self.stop_scan();
                    return;
                }
                scan.index = 0;
                self.save_dx_csv();
            }
            let scan = self.scan.as_mut().unwrap();
            scan.phase = ScanPhase::Settle;
            scan.since = Instant::now();
            let ch = scan.channels[scan.index].clone();
            self.tune(&ch);
            self.broadcast_state();
        }
    }

    /// The TII codes heard on the channel being scanned.
    fn scan_heard(&self) -> Vec<(u8, u8)> {
        let Some(scan) = self.scan.as_ref() else { return Vec::new() };
        scan.tii
            .iter()
            .filter_map(|c| {
                let (m, s) = c.split_once('-')?;
                Some((m.parse().ok()?, s.parse().ok()?))
            })
            .collect()
    }

    /// Second try of a scanned channel at another gain: a low one when the
    /// first was high (a strong neighbouring multiplex may overload the
    /// tuner), else the usual one. False when already tried or not possible.
    fn scan_other_gain(&mut self) -> bool {
        if !self.gain_seek_active() || !self.device_running {
            return false;
        }
        let Some(scan) = self.scan.as_mut() else { return false };
        if scan.other_gain {
            return false;
        }
        let status = self.device.status();
        let current = status.gain_db.unwrap_or(40.0);
        let target = if current > 20.0 { 12.0 } else { 40.0 };
        let Some((i, db)) = status.gains.iter().enumerate().min_by(|a, b| (a.1 - target).abs().total_cmp(&(b.1 - target).abs())) else {
            return false;
        };
        log::debug!("scan: {} again at {db:.1} dB", scan.channels[scan.index]);
        scan.other_gain = true;
        scan.phase = ScanPhase::Settle;
        scan.since = Instant::now();
        self.device.set_gain_ceiling(Some(i), true);
        true
    }

    fn dx_rows(&self) -> Vec<DxResult> {
        let Some(scan) = self.scan.as_ref() else { return Vec::new() };
        let Some(dx) = scan.dx.as_ref() else { return Vec::new() };
        let ens = &self.ensemble;
        let eid = ens.eid.unwrap_or(0);
        let ecc = ens.ecc.unwrap_or(0);
        let base = DxResult {
            time: now(),
            channel: scan.channels[scan.index].clone(),
            frequency: self.frequency,
            ueid: format!("{ecc:02X}{eid:04X}"),
            eid,
            label: ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
            services: ens.services.values().filter(|s| s.is_audio()).count(),
            snr: r1(dx.snr),
            main: None,
            sub: None,
            level: None,
            location: None,
            lat: None,
            lon: None,
            power: None,
            distance: None,
            azimuth: None,
            country: ens.ecc.and_then(|e| magicdab_dab::tables::country(e, (eid >> 12) as u8)).map(|c| c.0.to_string()),
        };
        if dx.tii.is_empty() {
            return vec![base];
        }
        let mut codes: Vec<(&(u8, u8), &f32)> = dx.tii.iter().collect();
        codes.sort_by(|a, b| b.1.total_cmp(a.1));
        codes
            .into_iter()
            .map(|(&(main, sub), &level)| {
                let mut r = base.clone();
                r.main = Some(main);
                r.sub = Some(sub);
                r.level = Some(r1(level));
                if let Some(t) = self.txdb.find(eid, ens.ecc, main, sub, self.frequency) {
                    r.location = Some(t.location.clone());
                    r.lat = Some(t.lat);
                    r.lon = Some(t.lon);
                    r.power = Some(t.power_kw);
                    if let Some(l) = &self.settings.location {
                        let (d, a) = distance_azimuth(l.lat, l.lon, t.lat, t.lon);
                        r.distance = Some((d * 10.0).round() / 10.0);
                        r.azimuth = Some((a * 10.0).round() / 10.0);
                    }
                }
                r
            })
            .collect()
    }

    pub(super) fn dx_json(&self) -> Value {
        json!({"type": "dxResults", "results": self.dx_results})
    }

    fn save_dx_csv(&self) {
        if self.dx_results.is_empty() {
            return;
        }
        let dir = self.data_dir.join("scanner");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("scan-{}.csv", crate::localtime::now().format("%Y-%m-%d")));
        let mut out = String::from("time;channel;frequency;ueid;label;services;snr;main;sub;level;location;power;distance;azimuth\n");
        for r in &self.dx_results {
            out.push_str(&format!(
                "{};{};{};{};{};{};{:.1};{};{};{};{};{};{};{}\n",
                chrono::DateTime::from_timestamp(r.time, 0)
                    .map(|t| crate::localtime::at(t).format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
                r.channel,
                r.frequency,
                r.ueid,
                r.label,
                r.services,
                r.snr,
                r.main.map(|v| v.to_string()).unwrap_or_default(),
                r.sub.map(|v| v.to_string()).unwrap_or_default(),
                r.level.map(|v| format!("{v:.1}")).unwrap_or_default(),
                r.location.clone().unwrap_or_default(),
                r.power.map(|v| v.to_string()).unwrap_or_default(),
                r.distance.map(|v| format!("{v:.1}")).unwrap_or_default(),
                r.azimuth.map(|v| format!("{v:.1}")).unwrap_or_default(),
            ));
        }
        let _ = std::fs::write(file, out);
    }
}
