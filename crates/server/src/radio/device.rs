//! The input of the main receiver: the device (RTL-SDR, rtl_tcp, the DTR-100, or a stream or
//! recording mapped to the channel), tuning, the gain, overload, and which services are decoded.

use super::*;

impl Radio {
    pub(super) fn start_device(&mut self, freq_khz: u32) {
        let queue = self.rx.queue.clone();
        let rec = self.raw_recorder.clone();
        let done = self.raw_done.clone();
        let samples_at = self.samples_at.clone();
        samples_at.store(now_ms(), Ordering::Relaxed);
        let cb: magicdab_sdr::SampleCallback = Arc::new(move |s| {
            samples_at.store(now_ms(), Ordering::Relaxed);
            queue.push(s);
            // (one lock at a time: the radio thread takes them the other way round)
            let finished = {
                let mut r = rec.lock();
                let complete = r.as_mut().is_some_and(|recorder| !recorder.push(s));
                if complete {
                    r.take()
                } else {
                    None
                }
            };
            if let Some(rr) = finished {
                let p = rr.finish();
                *done.lock() = Some(p);
            }
        });
        self.device.set_gain_mode(self.settings.gain.clone());
        match self.device.start(freq_khz * 1000, cb) {
            Ok(()) => {
                self.device_running = true;
                self.device_error = None;
                self.device.set_gain_mode(self.settings.gain.clone());
                log::info!("input device started: {}", self.device.status().name);
            }
            Err(e) => {
                self.device_running = false;
                let e = e.to_string();
                if self.device_error.as_ref() != Some(&e) {
                    log::error!("cannot start input device: {e} (trying again every 10 s)");
                }
                self.device_error = Some(e);
            }
        }
    }

    pub(super) fn reopen_device(&mut self) {
        self.device.stop();
        self.device = magicdab_sdr::open(&self.settings.device);
        self.input = self.settings.device.clone();
        self.device_running = false;
        self.device_error = None;
        if self.scan.is_some() {
            self.stop_scan();
        }
        self.fixed_channel = self.compute_fixed_channel();
        if !matches!(self.settings.device, DeviceConfig::Dtr100) {
            self.dtr100 = None;
        }
        if let Some(ch) = self.fixed_channel.clone() {
            self.channel = None;
            self.tune(&ch);
            self.broadcast_state();
            return;
        }
        if matches!(self.settings.device, DeviceConfig::Dtr100) {
            if let Some(ch) = self.channel.take() {
                self.tune(&ch);
            }
            self.broadcast_state();
            return;
        }
        let f = self.frequency;
        if f > 0 {
            self.start_device(f);
            self.apply_gain_ceiling();
            self.rx.queue.reset();
            self.awaiting_retune = true;
        }
        self.broadcast_state();
    }

    /// Remember the rarely transmitted FIC information of the current ensemble.
    pub(super) fn observe_fig_cache(&mut self) {
        self.last_fig_observe = Instant::now();
        let Some(ch) = self.channel.clone() else { return };
        if self.ensemble.eid.is_none() {
            return;
        }
        let complete = self.tuned_at.elapsed() > figcache::COMPLETE_AFTER;
        self.fig_cache.observe(&ch, &self.ensemble, &self.labels, complete);
    }

    /// ETI / EDI stream mapped to a channel.
    pub(super) fn mapped_source(&self, channel: &str) -> Option<etiinput::Source> {
        self.settings.channel_maps.iter().find(|m| m.channel.eq_ignore_ascii_case(channel)).and_then(|m| etiinput::Source::parse(&m.url))
    }

    pub(super) fn mapped_channels(&self) -> Vec<String> {
        self.settings
            .channel_maps
            .iter()
            .filter(|m| etiinput::Source::parse(&m.url).is_some() || m.file().is_some())
            .map(|m| m.channel.clone())
            .collect()
    }

    /// Recording played as the tuner of `channel`.
    pub(super) fn mapped_file(&self, channel: &str) -> Option<String> {
        self.settings.channel_maps.iter().find(|m| m.channel.eq_ignore_ascii_case(channel)).and_then(|m| m.file()).map(|p| p.to_string())
    }

    pub(super) fn recorded_channels(&self) -> Vec<String> {
        self.settings.channel_maps.iter().filter(|m| m.file().is_some()).map(|m| m.channel.clone()).collect()
    }

    /// The fixed channel and station of the settings, now: the receiver goes
    /// there, its listeners with it.
    pub(super) fn apply_locks(&mut self) {
        self.channel_arg = self.settings.lock_channel.clone().filter(|c| channel_frequency(c).is_some());
        self.service_lock = self.settings.lock_service.as_deref().and_then(|s| u32::from_str_radix(s, 16).ok()).filter(|s| *s != 0);
        log::info!(
            "fixed channel {}, fixed station {}",
            self.channel_arg.as_deref().unwrap_or("none"),
            self.service_lock.map(|s| format!("{s:X}")).unwrap_or_else(|| "none".into())
        );
        let fixed = self.compute_fixed_channel();
        if fixed != self.fixed_channel {
            self.fixed_channel = fixed;
            if let Some(ch) = self.fixed_channel.clone().filter(|ch| self.channel.as_deref() != Some(ch.as_str())) {
                if self.scan.is_some() {
                    self.stop_scan();
                }
                self.channel = None;
                self.tune(&ch);
            }
        }
        if let Some(k) = self.service_lock.map(|sid| ServiceKey { sid, scids: 0 }) {
            if self.main_service != Some(k) {
                self.main_service = Some(k);
                for id in self.hub.ids_in(0) {
                    self.hub.update(id, |c| c.service = Some(k));
                }
                self.hub.broadcast_main(text(&json!({"type": "select", "sid": k.sid, "scids": k.scids, "reason": "fixed"})));
                self.update_decoding();
            }
        }
        // (listeners' own multiplexes need a receiver that may tune)
        self.check_slots();
        self.broadcast_state();
    }

    /// rtl_tcp servers usually stream one fixed multiplex (e.g. a modulator)
    /// and recordings contain a single channel: such inputs stay on one channel.
    /// (rtl_tcp on this machine is a stick of its own: it tunes.)
    pub(super) fn compute_fixed_channel(&self) -> Option<String> {
        if self.settings.device.local_rtltcp() {
            return self.channel_arg.clone();
        }
        match self.settings.device {
            DeviceConfig::RawFile { .. } => self
                .device
                .status()
                .frequency
                .checked_div(1000)
                .and_then(magicdab_dab::consts::channel_name)
                .map(|c| c.to_string())
                .or_else(|| self.channel_arg.clone())
                .or_else(|| Some("5A".into())),
            DeviceConfig::RtlTcp { .. } => self.channel_arg.clone().or_else(|| Some("5A".into())),
            _ => self.channel_arg.clone(),
        }
    }

    /// Whether the SNR based gain optimiser drives the software AGC ceiling.
    pub(super) fn gain_seek_active(&self) -> bool {
        matches!(self.settings.gain, GainMode::Software)
            && matches!(self.settings.device, DeviceConfig::RtlSdr { .. } | DeviceConfig::RtlTcp { .. })
    }

    /// Apply the learned (or default) gain ceiling of the current channel.
    pub(super) fn apply_gain_ceiling(&mut self) {
        if !self.gain_seek_active() || !self.device_running {
            return;
        }
        let Some(ch) = self.channel.clone() else { return };
        let gains = self.device.status().gains;
        if let Some(c) = self.gain_seek.retune(&ch, &gains) {
            self.device.set_gain_ceiling(Some(c), true);
        }
    }

    pub(super) fn tune(&mut self, channel: &str) -> bool {
        let Some(freq) = channel_frequency(channel) else { return false };
        self.last_audio.clear();
        self.sync_lost_at = None;
        let channel = BAND_III.iter().find(|(n, _)| n.eq_ignore_ascii_case(channel)).map(|(n, _)| n.to_string()).unwrap();
        if self.fixed_channel.as_deref().is_some_and(|f| f != channel) {
            return false;
        }
        let mapped = self.mapped_source(&channel);
        let file = self.mapped_file(&channel);
        if mapped.is_none() && file.is_none() && matches!(self.settings.device, DeviceConfig::None) {
            // without a tuner only mapped channels exist
            return false;
        }
        let dongle = mapped.is_none() && file.is_none() && matches!(self.settings.device, DeviceConfig::Dtr100);
        if self.channel.as_deref() == Some(channel.as_str())
            && (self.device_running || self.eti_input.is_some() || (dongle && self.dtr100.is_some()))
        {
            return true;
        }
        log::info!("tuning to {channel} ({:.3} MHz)", freq as f64 / 1000.0);
        self.observe_fig_cache();
        for k in self.decoding.drain() {
            self.rx.send(RxCommand::StopService(k));
        }
        self.audio.clear();
        self.services.clear();
        self.dyn_labels.clear();
        self.gallery.clear();
        self.spi.clear_session();
        self.channel = Some(channel.clone());
        self.frequency = freq;
        self.tuned_at = Instant::now();
        self.eti_input = None;
        if !dongle {
            // (the dongle lets go: nothing of it may reach the receiver)
            if let Some(d) = &self.dtr100 {
                d.tune(None);
            }
        }
        if dongle {
            // the dongle demodulates; its FIC and sub-channel come like a stream
            if self.device_running {
                self.device.stop();
                self.device_running = false;
            }
            if self.input != self.settings.device {
                self.device = magicdab_sdr::open(&self.settings.device);
                self.input = self.settings.device.clone();
            }
            self.rx.set_eti_mode(true);
            let eti = self.rx.eti.clone();
            let d = self.dtr100.get_or_insert_with(|| Dtr100Input::start(eti));
            d.set_stream(None);
            d.tune(Some(freq));
        } else if let Some(src) = mapped {
            // the stream replaces the tuner on this channel
            if self.device_running {
                self.device.stop();
                self.device_running = false;
            }
            log::info!("{channel} from {} stream {}:{}", if src.edi { "EDI" } else { "ETI" }, src.host, src.port);
            self.rx.set_eti_mode(true);
            self.eti_input = Some(EtiInput::start(src, self.rx.eti.clone(), Duration::from_millis(self.settings.eti_delay_ms as u64)));
        } else {
            self.rx.set_eti_mode(false);
            // a recording mapped to the channel replaces the tuner (in a loop)
            let want = match &file {
                Some(path) => DeviceConfig::RawFile { path: path.clone(), looping: true },
                None => self.settings.device.clone(),
            };
            if want != self.input {
                if self.device_running {
                    self.device.stop();
                    self.device_running = false;
                }
                if let Some(path) = &file {
                    log::info!("{channel} from recording {path}");
                }
                self.device = magicdab_sdr::open(&want);
                self.input = want;
                self.device_error = None;
            }
            if self.device_running {
                if let Err(e) = self.device.tune(freq * 1000) {
                    log::warn!("retune failed ({e}), restarting device");
                    self.device.stop();
                    self.start_device(freq);
                }
            } else {
                self.start_device(freq);
            }
            self.apply_gain_ceiling();
        }
        self.rx.queue.reset();
        self.awaiting_retune = true;
        self.ensemble = Arc::new(Ensemble::default());
        self.labels = Arc::new(LongLabels::new());
        self.ensemble_msg = None;
        self.tii_codes.clear();
        self.tii = json!({"type": "tii", "codes": []});
        self.signal = SignalInfo::default();
        self.data_progress.clear();
        self.hub.broadcast_main(text(&self.data_progress_json()));
        if self.scan.as_ref().map(|s| !s.silent).unwrap_or(true) {
            self.settings.last_channel = Some(channel);
            self.settings_dirty = true;
        }
        self.broadcast_state();
        let m = self.ensemble_message();
        self.hub.broadcast_main(m);
        self.hub.broadcast_main(text(&self.tii));
        true
    }

    /// RadioVIS follows the services listeners have selected.
    pub(super) fn update_vis(&mut self) {
        let mut want: Vec<(ServiceKey, radiovis::Bearer)> = Vec::new();
        if self.settings.internet && self.settings.radio_vis && self.scan.is_none() {
            if let Some(eid) = self.ensemble.eid {
                let mut keys: Vec<ServiceKey> = self.hub.wanted_services(0).into_iter().map(|(k, _, _)| k).collect();
                keys.sort_by_key(|k| (k.sid, k.scids));
                for k in keys {
                    let Some(s) = self.ensemble.services.get(&k.sid).filter(|s| s.is_audio() && !s.is_data) else { continue };
                    let Some(ecc) = s.ecc.or(self.ensemble.ecc) else { continue };
                    want.push((k, radiovis::Bearer { sid: k.sid, ecc, eid, scids: k.scids }));
                }
            }
        }
        if want != self.vis_wanted {
            if let Some(v) = &self.radiovis {
                v.want(want.clone());
            }
            self.vis_wanted = want;
        }
    }

    pub(super) fn update_decoding(&mut self) {
        self.update_side_decoding();
        self.update_vis();
        let wanted = self.hub.wanted_services(0);
        self.wants = wanted.iter().map(|(k, o, p)| (*k, (*o, *p))).collect();
        let mut keys: Vec<ServiceKey> = Vec::new();
        if let Some(m) = self.main_service {
            if self.ensemble.services.contains_key(&m.sid) {
                keys.push(m);
            }
        }
        for (k, _, _) in &wanted {
            if !keys.contains(k) && self.ensemble.services.contains_key(&k.sid) {
                keys.push(*k);
            }
        }
        // the dongle streams one sub-channel: one station for everybody
        let single = self.dongle_active();
        keys.truncate(if single { 1 } else { self.settings.max_services.max(1) });
        // (the streams' own limit: settings.streams, icy.rs)
        for k in self.stream_keys().collect::<Vec<_>>() {
            if !keys.contains(&k) && !single && self.ensemble.services.contains_key(&k.sid) {
                keys.push(k);
            }
        }
        if let Some(d) = self.dtr100.as_ref().filter(|_| single) {
            let first = if self.scan.is_some() { self.clips.keys().next().copied() } else { keys.first().copied() };
            d.set_stream(first.and_then(|k| self.dongle_stream(k)));
        }
        let set: HashSet<ServiceKey> = keys.into_iter().collect();
        if self.scan.is_some() {
            // no audio decoding while scanning, but for the stations recorded
            // for the server map (radio/clips.rs)
            let clips: HashSet<ServiceKey> = self.clips.keys().copied().collect();
            for k in self.decoding.difference(&clips).copied().collect::<Vec<_>>() {
                self.rx.send(RxCommand::StopService(k));
                self.last_audio.remove(&k);
            }
            for k in clips.difference(&self.decoding).copied().collect::<Vec<_>>() {
                self.rx.send(RxCommand::StartService(k));
            }
            self.decoding = clips;
            return;
        }
        for k in set.difference(&self.decoding).copied().collect::<Vec<_>>() {
            self.rx.send(RxCommand::StartService(k));
        }
        for k in self.decoding.difference(&set).copied().collect::<Vec<_>>() {
            self.rx.send(RxCommand::StopService(k));
            self.audio.remove(&k);
            self.last_audio.remove(&k);
        }
        self.decoding = set;
        let rec = self.hub.recorded_services(0);
        for k in rec.iter().filter(|k| !self.recording.contains(k)) {
            self.rx.send(RxCommand::RecordEncoded(*k, true));
        }
        for k in self.recording.iter().filter(|k| !rec.contains(k)) {
            self.rx.send(RxCommand::RecordEncoded(*k, false));
        }
        self.recording = rec;
        // (also while a DX recording keeps it: dxrec.rs)
        let spectrum = self.hub.any(|c| c.subs.spectrum && c.on_main()) || self.dxrec_recording();
        if spectrum != self.spectrum_on {
            self.spectrum_on = spectrum;
            self.rx.send(RxCommand::Spectrum(spectrum));
        }
    }

    /// Every 5 s: input the receiver had to throw away because it fell behind
    /// (samples from the tuner, frames from the DTR-100). Listeners hear gaps
    /// then and the signal looks worse than it is: the log and the interface say so.
    pub(super) fn check_overload(&mut self) {
        let o = &mut self.overload;
        if o.at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        let first = o.at.is_none();
        o.at = Some(Instant::now());
        let q = &self.rx.queue;
        let (pushed, dropped) = (q.pushed.load(Ordering::Relaxed), q.dropped.load(Ordering::Relaxed));
        let frames = self.dtr100.as_ref().map(|d| d.status().dropped).unwrap_or(0);
        let (dp, dd, df) = (pushed.saturating_sub(o.pushed), dropped.saturating_sub(o.dropped), frames.saturating_sub(o.frames));
        (o.pushed, o.dropped, o.frames) = (pushed, dropped, frames);
        let sample = o.sampler.sample();
        if first {
            return;
        }
        // (5 s of the DTR-100 are about 208 frames of 24 ms)
        let lost = if dd > 0 { 100.0 * dd as f32 / (dp + dd) as f32 } else { (100.0 * df as f32 / 208.0).min(100.0) };
        let was = o.active();
        if lost >= 0.5 {
            o.since = Some(Instant::now());
            o.lost = lost;
            o.cpu = sample.threads.iter().find(|t| t.name == "dab-receiver").map(|t| t.cpu);
            if o.logged.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                o.logged = Some(Instant::now());
                let cpu = o.cpu.map(|c| format!(", the receiver thread takes {c:.0}% of a processor")).unwrap_or_default();
                log::warn!("this computer cannot keep up with the receiver: {lost:.1}% of the input lost in the last 5 s{cpu}");
            }
        }
        if was != o.active() {
            if !o.active() {
                log::info!("the receiver keeps up again");
            }
            self.broadcast_state();
        }
    }

    /// The DTR-100 tunes the current channel (not a stream or recording mapped to it).
    pub(super) fn dongle_active(&self) -> bool {
        self.dtr100.is_some() && self.eti_input.is_none() && matches!(self.input, DeviceConfig::Dtr100)
    }

    /// What the dongle has to stream for a service: its audio sub-channel.
    fn dongle_stream(&self, key: ServiceKey) -> Option<dtr100input::Stream> {
        let svc = self.ensemble.services.get(&key.sid)?;
        let c = svc.components.iter().find(|c| c.scids == Some(key.scids) || (key.scids == 0 && c.primary)).or_else(|| svc.primary())?;
        if !c.is_audio() {
            return None;
        }
        let id = c.subch_id?;
        let sub = self.ensemble.subchannels.get(&id)?;
        (sub.bitrate > 0).then_some(dtr100input::Stream { subch: id, bitrate: sub.bitrate, dabplus: c.is_dabplus() })
    }
}
