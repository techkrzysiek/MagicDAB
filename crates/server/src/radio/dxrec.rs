//! DX recordings (dxrec.rs) on the radio thread: a multiplex heard on a
//! channel for the first time (changes.rs: ensembleNew, ensembleId) is
//! recorded from the main receiver for up to a few minutes, also while
//! listeners listen; the background scan stays on it meanwhile (radio.rs:
//! ScanPhase::Hold) and a listener who turns the sound on still takes the
//! tuner back (the recording so far is kept). Admins see the recordings,
//! play one back on a side receiver of their own (slots.rs: Kind::Playback),
//! delete them, and load .eti files of their own.

use super::slots::Kind;
use super::*;
use crate::dxrec::{self as store, Meta, MetaService, Writer, FRAMES_PER_MINUTE};
use magicdab_dab::eti::ETI_FRAME;

/// A recording ends after this long without synchronisation.
const NO_SYNC: Duration = Duration::from_secs(30);
/// Playbacks at once (a receiver each).
const MAX_PLAYBACKS: usize = 2;

pub struct State {
    active: Option<Active>,
    /// When the admins get the list next (the writer finishes a file a
    /// moment after a recording ends).
    list_due: Option<Instant>,
    last_progress: Instant,
    last_prune: Instant,
}

struct Active {
    writer: Writer,
    /// Frames to record.
    limit: u64,
    channel: String,
    last_sync: Instant,
    /// For the means while recording: SNR and FIB error ratio sums, samples.
    snr_sum: f32,
    fib_sum: f32,
    samples: u32,
    tii: Vec<String>,
}

impl State {
    pub fn new() -> Self {
        State { active: None, list_due: None, last_progress: Instant::now(), last_prune: Instant::now() }
    }
}

impl Radio {
    fn dxrec_dir(&self) -> PathBuf {
        self.data_dir.join(store::DIR)
    }

    /// From observe_changes: a multiplex heard on this channel for the first
    /// time (or another one than before) is recorded.
    pub(super) fn dxrec_on_changes(&mut self, ev: &changes::ChangeEvent) {
        let Some(kind) = ev.changes.iter().map(|c| c.kind.as_str()).find(|k| *k == "ensembleNew" || *k == "ensembleId") else { return };
        if !self.settings.dx_record.enabled {
            return;
        }
        if let Err(e) = self.dxrec_start(kind, None) {
            log::info!("no DX recording of {} on {}: {e}", ev.ensemble.trim(), ev.channel);
        }
    }

    /// Starts recording the multiplex of the main receiver, for `seconds` or
    /// the length of the settings.
    pub(super) fn dxrec_start(&mut self, reason: &str, seconds: Option<u32>) -> Result<(), String> {
        if self.dxrec.active.is_some() {
            return Err("one is being recorded already".into());
        }
        let Some(channel) = self.channel.clone() else { return Err("no channel".into()) };
        let Some(eid) = self.ensemble.eid else { return Err("no ensemble identified".into()) };
        let dir = self.dxrec_dir();
        let cfg = &self.settings.dx_record;
        // room for this one under the cap (a recording of the full length)
        let limit = match seconds {
            Some(s) => (s as u64 * FRAMES_PER_MINUTE).div_ceil(60),
            None => cfg.minutes.max(1) as u64 * FRAMES_PER_MINUTE,
        };
        let expected = limit * ETI_FRAME as u64;
        let max = (cfg.max_mb as u64 * 1024 * 1024).saturating_sub(expected);
        for name in store::enforce_cap(&dir, max, None) {
            log::info!("DX recording {name} removed: the folder is kept under {} MB", cfg.max_mb);
        }
        let ens = &self.ensemble;
        let services = ens
            .services
            .values()
            .map(|s| {
                let sc = s.primary().and_then(|c| c.subch_id).and_then(|id| ens.subchannels.get(&id));
                let codec = match s.primary() {
                    Some(c) if c.tmid == 0 && c.ctype == 63 => "DAB+",
                    Some(c) if c.tmid == 0 => "DAB",
                    _ => "data",
                };
                MetaService {
                    sid: format!("{:04X}", s.sid),
                    label: s.label.as_ref().map(|l| l.text.trim().to_string()).unwrap_or_default(),
                    bitrate: sc.map_or(0, |x| x.bitrate),
                    codec: codec.into(),
                }
            })
            .collect();
        let tii: Vec<String> = self.tii_codes.iter().map(|c| format!("{}-{}", c.main, c.sub)).collect();
        let meta = Meta {
            time: now(),
            channel: channel.clone(),
            frequency: self.frequency,
            ecc: format!("{:02X}", ens.ecc.unwrap_or(0)),
            eid: format!("{eid:04X}"),
            label: ens.label.as_ref().map(|l| l.text.trim().to_string()).unwrap_or_default(),
            services,
            snr: self.signal.snr,
            tii: tii.clone(),
            reason: reason.to_string(),
            device: self.device.status().name,
            ..Default::default()
        };
        let writer = Writer::start(&dir, meta).map_err(|e| e.to_string())?;
        log::info!(
            "DX recording of {} ({}, {}) started: {} (up to {} s, {reason})",
            writer.meta.label,
            writer.meta.eid,
            channel,
            writer.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            limit * 60 / FRAMES_PER_MINUTE
        );
        self.rx.send(RxCommand::RecordEti(true));
        self.dxrec.active = Some(Active { writer, limit, channel, last_sync: Instant::now(), snr_sum: 0.0, fib_sum: 0.0, samples: 0, tii });
        self.dxrec_broadcast(false);
        Ok(())
    }

    /// Ends the recording; `ended` says how (complete, retuned, noSync,
    /// stopped, error).
    pub(super) fn dxrec_stop(&mut self, ended: &str) {
        let Some(a) = self.dxrec.active.take() else { return };
        self.rx.send(RxCommand::RecordEti(false));
        let mut writer = a.writer;
        if a.samples > 0 {
            writer.meta.snr_mean = a.snr_sum / a.samples as f32;
            writer.meta.fib_error_rate = a.fib_sum / a.samples as f32;
        }
        writer.meta.tii = a.tii;
        let dropped = writer.dropped;
        let name = writer.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let meta = writer.finish(ended);
        log::info!(
            "DX recording {name} ended ({ended}): {:.0} s, {:.1} MB{}",
            meta.duration,
            meta.size as f64 / 1e6,
            if dropped > 0 { format!(", {dropped} frames lost (disk too slow)") } else { String::new() }
        );
        self.dxrec.list_due = Some(Instant::now() + Duration::from_millis(1500));
        self.dxrec_broadcast(false);
    }

    /// A frame of the main receiver (RxEvent::Eti).
    pub(super) fn dxrec_on_frame(&mut self, frame: Vec<u8>) {
        let Some(a) = self.dxrec.active.as_mut() else { return };
        a.writer.push(frame);
        if a.writer.frames >= a.limit {
            self.dxrec_stop("complete");
        } else if a.writer.failed() {
            self.dxrec_stop("error");
        }
    }

    /// The band scan stays on the channel of a recording (radio.rs).
    pub(super) fn dxrec_holds_scan(&self) -> bool {
        self.dxrec.active.as_ref().is_some_and(|a| self.channel.as_deref() == Some(a.channel.as_str()))
    }

    /// From the tick: a retune or no synchronisation ends the recording,
    /// the admins see its progress; the list when due; old uploads go.
    pub(super) fn dxrec_tick(&mut self) {
        if let Some(a) = self.dxrec.active.as_mut() {
            if self.channel.as_deref() != Some(a.channel.as_str()) {
                self.dxrec_stop("retuned");
            } else {
                if self.signal.synced {
                    a.last_sync = Instant::now();
                    a.snr_sum += self.signal.snr;
                    a.fib_sum += self.signal.fib_error_rate;
                    a.samples += 1;
                }
                for c in &self.tii_codes {
                    let code = format!("{}-{}", c.main, c.sub);
                    if !a.tii.contains(&code) {
                        a.tii.push(code);
                    }
                }
                if a.last_sync.elapsed() > NO_SYNC {
                    self.dxrec_stop("noSync");
                } else if self.dxrec.last_progress.elapsed() > Duration::from_secs(2) {
                    self.dxrec.last_progress = Instant::now();
                    self.dxrec_broadcast(false);
                }
            }
        }
        if self.dxrec.list_due.is_some_and(|t| Instant::now() >= t) {
            self.dxrec.list_due = None;
            self.dxrec_broadcast(true);
        }
        if self.dxrec.last_prune.elapsed() > Duration::from_secs(60) {
            self.dxrec.last_prune = Instant::now();
            let in_use = self.dxrec_in_use();
            for name in store::prune_uploads(&self.dxrec_dir(), &in_use) {
                log::info!("uploaded recording {name} removed after a day");
                self.dxrec.list_due = Some(Instant::now());
            }
        }
    }

    /// The recordings being played back.
    fn dxrec_in_use(&self) -> Vec<String> {
        self.slots.iter().filter_map(|e| e.slot.playback_name()).map(|n| n.to_string()).collect()
    }

    /// The playback an admin listens to.
    fn playback_slot_of(&self, client: u64) -> Option<u32> {
        let id = self.hub.slot_of(client);
        self.slots.get(id).filter(|e| id != 0 && e.kind == Kind::Playback).map(|e| e.id)
    }

    /// What the admins see: the recording running and, with the list, the
    /// recordings of the folder.
    fn dxrec_json(&self, with_list: bool) -> Value {
        let cfg = &self.settings.dx_record;
        let active = self.dxrec.active.as_ref().map(|a| {
            json!({
                "channel": a.channel,
                "label": a.writer.meta.label,
                "eid": a.writer.meta.eid,
                "reason": a.writer.meta.reason,
                "seconds": a.writer.seconds(),
                "limit": a.limit * 60 / FRAMES_PER_MINUTE,
                "bytes": a.writer.bytes,
                "synced": self.signal.synced,
            })
        });
        let mut v = json!({
            "type": "dxrec",
            "active": active,
            "enabled": cfg.enabled,
            "minutes": cfg.minutes,
            "maxMb": cfg.max_mb,
            "uploadMaxMb": store::UPLOAD_MAX >> 20,
        });
        if with_list {
            let dir = self.dxrec_dir();
            v["recordings"] = json!(store::list(&dir));
            v["usedMb"] = json!(store::used(&dir) >> 20);
        }
        v
    }

    fn dxrec_broadcast(&self, with_list: bool) {
        self.hub.broadcast(text(&self.dxrec_json(with_list)), |c| c.admin);
    }

    /// The list to an admin who logged in or connected.
    pub(super) fn send_dxrec_list(&self, client: u64) {
        self.hub.send(client, text(&self.dxrec_json(true)));
    }

    /// Messages of the admins: the list, a recording of the multiplex now,
    /// its end, a playback and its end, deleting, the setting.
    pub(super) fn on_dxrec_message(&mut self, client: u64, msg: &Value) {
        if !self.hub.is_admin(client) {
            self.notice(client, "warning", "admin_required");
            return;
        }
        let name = msg["name"].as_str().and_then(store::safe_name);
        match msg["action"].as_str().unwrap_or("") {
            "list" => self.send_dxrec_list(client),
            "record" => {
                // (the length asked for: the button of the Ensemble view)
                let seconds = msg["seconds"].as_u64().map(|s| s.clamp(5, 3600) as u32);
                if let Err(e) = self.dxrec_start("manual", seconds) {
                    log::info!("client {client}: no DX recording: {e}");
                    self.notice(client, "warning", "dxrec_no_start");
                }
            }
            "stopRecording" => {
                if self.dxrec.active.is_some() {
                    log::info!("client {client} ({}) stopped the DX recording", self.client_ip(client));
                    self.dxrec_stop("stopped");
                }
            }
            "delete" => {
                let Some(name) = name else { return };
                // (playing it: its receiver closes first)
                let playing: Vec<u32> = self.slots.iter().filter(|e| e.slot.playback_name() == Some(name.as_str())).map(|e| e.id).collect();
                for id in playing {
                    self.close_slot(id, "the recording was deleted", false);
                }
                match store::remove(&self.dxrec_dir().join(&name)) {
                    Ok(()) => log::info!("client {client} ({}) deleted the DX recording {name}", self.client_ip(client)),
                    Err(e) => log::warn!("cannot delete the DX recording {name}: {e}"),
                }
                self.dxrec_broadcast(true);
            }
            "play" => {
                if let Some(name) = name {
                    let sid = msg["sid"].as_u64().map(|s| s as u32);
                    self.dxrec_play(client, &name, sid);
                }
            }
            "stopPlayback" => {
                if let Some(id) = self.playback_slot_of(client) {
                    self.close_slot(id, "the admin stopped the playback", false);
                }
            }
            "settings" => {
                let cfg = &mut self.settings.dx_record;
                if let Some(on) = msg["enabled"].as_bool() {
                    cfg.enabled = on;
                    log::info!("DX recordings of new multiplexes switched {}", if on { "on" } else { "off" });
                }
                if let Some(m) = msg["minutes"].as_u64() {
                    cfg.minutes = (m as u32).clamp(1, 30);
                }
                if let Some(mb) = msg["maxMb"].as_u64() {
                    cfg.max_mb = (mb as u32).clamp(100, 100_000);
                }
                self.settings_dirty = true;
                let s = self.settings_json();
                self.hub.broadcast(text(&s), |c| c.admin);
                self.dxrec_broadcast(false);
            }
            _ => {}
        }
    }

    /// Plays a recording back to an admin on a side receiver of its own: the
    /// station `sid`, else the first one of its ensemble (slots.rs).
    fn dxrec_play(&mut self, client: u64, name: &str, sid: Option<u32>) {
        let path = self.dxrec_dir().join(name);
        if !path.is_file() {
            self.notice(client, "warning", "dxrec_missing");
            self.dxrec_broadcast(true);
            return;
        }
        // one playback per admin
        if let Some(id) = self.playback_slot_of(client) {
            self.close_slot(id, "another recording plays", false);
        }
        if self.slots.playbacks() >= MAX_PLAYBACKS {
            self.notice(client, "warning", "dxrec_busy");
            return;
        }
        // the channel it was recorded on (the name says it too: date_time_channel_eid)
        let meta = store::read_meta(&path);
        let channel = meta
            .as_ref()
            .map(|m| m.channel.clone())
            .filter(|c| channel_frequency(c).is_some())
            .or_else(|| name.split('_').nth(2).filter(|c| channel_frequency(c).is_some()).map(|c| c.to_string()))
            .unwrap_or_else(|| "5A".into());
        let key = sid.map(|sid| ServiceKey { sid, scids: 0 });
        let id = self.slots.next_id();
        match self.open_playback_slot(&channel, id, &path) {
            Ok(slot) => {
                self.slots.open(&channel, Kind::Playback, slot);
                log::info!("client {client} ({}) plays the DX recording {name} on a receiver of its own", self.client_ip(client));
                self.move_client(client, id, key);
                if let Some(k) = key {
                    self.hub.send(client, text(&json!({"type": "select", "sid": k.sid, "scids": k.scids, "reason": "playback"})));
                }
            }
            Err(e) => {
                log::warn!("cannot play the DX recording {name}: {e}");
                self.notice(client, "error", "dxrec_missing");
            }
        }
    }
}
