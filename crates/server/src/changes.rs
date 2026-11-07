//! Change tracking of received ensembles ("mux diff"): every stable reception
//! of an ensemble is compared with the previous one and the differences are
//! logged (data/changes.jsonl), offered to the web clients and optionally
//! posted to a Discord webhook (those of a multiplex together, 5 minutes
//! after the first of them).

use magicdab_dab::ensemble::Ensemble;
use magicdab_dab::tables;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ServiceSnap {
    label: String,
    short: String,
    pty: Option<u8>,
    /// FIG 0/17 says the PTY follows the programme
    #[serde(default)]
    pty_dynamic: bool,
    codec: String,
    bitrate: u16,
    protection: String,
    subch: Option<u8>,
    user_apps: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    label: String,
    short: String,
    services: BTreeMap<String, ServiceSnap>,
    used_cu: u32,
    observations: u32,
    /// sub-channel id -> description (start, size, protection, bitrate)
    #[serde(default)]
    subchannels: BTreeMap<u8, String>,
    #[serde(default)]
    ecc: Option<u8>,
    /// Unix time of the last observation.
    #[serde(default)]
    seen: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// serviceAdded, serviceRemoved, label, shortLabel, bitrate, protection, pty, codec,
    /// userApps, ensembleLabel, capacity, linkAdded, linkRemoved, fiAdded, fiRemoved, oeAdded, oeRemoved
    pub kind: String,
    pub service: Option<String>,
    pub sid: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEvent {
    pub time: i64,
    pub channel: String,
    pub eid: String,
    pub ensemble: String,
    pub changes: Vec<Change>,
    /// A new multiplex: what its first minute brought (radio/newmux.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
}

/// The first minute of a multiplex never heard here: how long and how strong
/// it came, and its transmitters by TII.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Seconds with synchronisation.
    pub seconds: u32,
    /// Why it ended before the minute: lost (no signal) or retuned.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ended: String,
    pub snr_mean: f32,
    pub snr_max: f32,
    /// Share of the FIBs with a bad CRC, mean.
    pub fib_errors: f32,
    /// ISO code of the country of its ECC and EId.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// The strongest first.
    pub transmitters: Vec<ReportTx>,
    /// The DX recording made of it (radio/dxrec.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportTx {
    /// TII main-sub.
    pub code: String,
    /// Mean level against the strongest transmitter (0 dB).
    pub level: f32,
    /// From the transmitter database, when it has the code.
    pub location: Option<String>,
    pub country: Option<String>,
    pub distance: Option<f64>,
    pub azimuth: Option<f64>,
    pub power_kw: Option<f32>,
}

/// A new service name is reported once it has lasted this long: a label
/// scrolling like an RDS PS never does.
const LABEL_CONFIRM: std::time::Duration = std::time::Duration::from_secs(60);
/// A new PTY is reported once it has lasted this long: many stations change it
/// with the programme (news, then music), flagged as dynamic or not.
const PTY_CONFIRM: std::time::Duration = std::time::Duration::from_secs(3 * 3600);
/// A new structure (services, their codec, bitrate and protection, the
/// sub-channels, the capacity) is reported once the same one has lasted this
/// long: a reception begun anew (a resync, the end of a scan) brings part of
/// the information first, and operators sometimes reconfigure in steps.
const STRUCTURE_CONFIRM: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// Discord gets the changes of a multiplex this long after the first of them,
/// in one message: a reconfiguration comes in several confirmed steps.
const DISCORD_BATCH: std::time::Duration = std::time::Duration::from_secs(5 * 60);

pub struct ChangeTracker {
    path: PathBuf,
    log: PathBuf,
    snapshots: HashMap<String, Snapshot>,
    pub recent: Vec<ChangeEvent>,
    /// New service names waiting for confirmation: (label, short label, since).
    pending_labels: HashMap<String, (String, String, std::time::Instant)>,
    /// PTY changes waiting for confirmation: key -> (new PTY, since).
    pending_pty: HashMap<String, (Option<u8>, std::time::Instant)>,
    /// New structures waiting for confirmation: ensemble -> (structure, since).
    pending_structure: HashMap<String, (String, std::time::Instant)>,
    /// Started without any snapshot: the first half hour (the first scan)
    /// learns the multiplexes without reporting each of them as new.
    fresh_until: Option<std::time::Instant>,
    /// Changes waiting to be posted to Discord.
    discord: Batch,
}

/// A label of random bytes: two or more characters no station name has
/// (controls, the replacement character, symbols of the EBU table such as
/// ¨ ¼ ¤ ¦, and Latin-1 letters from random bytes such as ÿ þ ð).
fn garbled(text: &str) -> bool {
    text.chars().filter(|c| c.is_control() || "\u{FFFD}$¨¼½¾¤¦¯´¸¹²³¬¶ÿþð{}|~^`\\".contains(*c)).count() >= 2
}

fn snapshot_of(ens: &Ensemble) -> Snapshot {
    let mut snap = Snapshot {
        label: ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
        short: ens.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
        used_cu: ens.used_cus(),
        ecc: ens.ecc,
        subchannels: ens
            .subchannels
            .values()
            .map(|sc| (sc.id, format!("start {}, {} CU, {}, {} kbps", sc.start, sc.size, sc.protection_label, sc.bitrate)))
            .collect(),
        ..Default::default()
    };
    for s in ens.services.values() {
        let p = s.primary();
        let sc = p.and_then(|c| c.subch_id).and_then(|id| ens.subchannels.get(&id));
        let codec = match p {
            Some(c) if c.tmid == 0 && c.ctype == 63 => "DAB+",
            Some(c) if c.tmid == 0 => "DAB",
            Some(_) => "data",
            None => "?",
        };
        let mut uas: Vec<String> =
            s.components.iter().flat_map(|c| c.user_apps.iter().map(|u| tables::user_app_name(u.ua_type).to_string())).collect();
        uas.sort();
        uas.dedup();
        snap.services.insert(
            format!("{:04X}", s.sid),
            ServiceSnap {
                label: s.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
                short: s.label.as_ref().map(|l| l.short.clone()).unwrap_or_default(),
                pty: s.pty,
                pty_dynamic: s.pty_dynamic,
                codec: codec.to_string(),
                bitrate: sc.map(|x| x.bitrate).unwrap_or(0),
                protection: sc.map(|x| x.protection_label.clone()).unwrap_or_default(),
                subch: sc.map(|x| x.id),
                user_apps: uas,
            },
        );
    }
    snap
}

/// What a structure change is judged by: the services with their codec,
/// bitrate, protection and sub-channel, the sub-channels and the capacity.
fn structure(s: &Snapshot) -> String {
    let services: Vec<String> =
        s.services.iter().map(|(sid, v)| format!("{sid}:{}:{}:{}:{:?}", v.codec, v.bitrate, v.protection, v.subch)).collect();
    let subchannels: Vec<String> = s.subchannels.iter().map(|(id, d)| format!("{id}={d}")).collect();
    format!("{}|{}|{}", services.join(","), subchannels.join(","), s.used_cu)
}

impl ChangeTracker {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("snapshots.json");
        let log = dir.join("changes.jsonl");
        let snapshots: HashMap<String, Snapshot> =
            std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut recent: Vec<ChangeEvent> = Vec::new();
        for ev in std::fs::read_to_string(&log).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<ChangeEvent>(l).ok()) {
            // (a report has a line of its own after its event: attach_report)
            if ev.changes.is_empty() && ev.report.is_some() {
                if let Some(e) = recent.iter_mut().rev().find(|e| e.time == ev.time && e.channel == ev.channel && e.eid == ev.eid) {
                    e.report = ev.report;
                }
                continue;
            }
            recent.push(ev);
        }
        if recent.len() > 500 {
            recent.drain(..recent.len() - 500);
        }
        let fresh_until = snapshots.is_empty().then(|| std::time::Instant::now() + std::time::Duration::from_secs(1800));
        let discord = Batch::load(&dir.join("discord-pending.json"));
        ChangeTracker {
            path,
            log,
            snapshots,
            recent,
            pending_labels: HashMap::new(),
            pending_pty: HashMap::new(),
            pending_structure: HashMap::new(),
            fresh_until,
            discord,
        }
    }

    /// Hold an event for Discord: the changes of its multiplex go together
    /// (post_discord) 5 minutes after the first of them.
    pub fn defer(&mut self, ev: &ChangeEvent) {
        self.discord.add(ev, std::time::Instant::now());
        self.discord.save(&self.pending_path());
    }

    /// The events whose 5 minutes are over, one per multiplex.
    pub fn due_for_discord(&mut self) -> Vec<ChangeEvent> {
        let before = self.discord.pending.len();
        let due = self.discord.due(std::time::Instant::now());
        if self.discord.pending.len() != before {
            self.discord.save(&self.pending_path());
        }
        due
    }

    /// The report of a new multiplex onto its event, which it returns; in
    /// the log as a line of its own (the log is only appended to).
    pub fn attach_report(&mut self, time: i64, channel: &str, eid: &str, report: Report) -> Option<ChangeEvent> {
        let ev = self.recent.iter_mut().rev().find(|e| e.time == time && e.channel == channel && e.eid == eid)?;
        ev.report = Some(report);
        let line = ChangeEvent { changes: Vec::new(), ..ev.clone() };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log) {
            if let Ok(l) = serde_json::to_string(&line) {
                let _ = writeln!(f, "{l}");
            }
        }
        Some(ev.clone())
    }

    /// The batches waiting for Discord survive a restart (an update installs
    /// itself when nobody listens, possibly right after a change).
    fn pending_path(&self) -> PathBuf {
        self.path.with_file_name("discord-pending.json")
    }

    fn save(&self) {
        if let Ok(s) = serde_json::to_string(&self.snapshots) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, &self.path);
            }
        }
    }

    /// Compare a stable reception with the stored state. `long` tells whether the
    /// observation lasted long enough to judge slowly repeated information.
    /// `dynamic`: services whose label changes all the time (no label changes reported).
    pub fn observe(
        &mut self,
        channel: &str,
        ens: &Ensemble,
        long: bool,
        dynamic: &[u32],
        mute: &crate::config::NotifyMute,
    ) -> Option<ChangeEvent> {
        let eid = ens.eid?;
        if ens.label.is_none() || ens.ecc.is_none() || ens.services.is_empty() || ens.services.values().any(|s| s.label.is_none()) {
            return None;
        }
        // more capacity than a CIF has, or services without their sub-channel:
        // the database is inconsistent (corrupted FIC data), do not judge it
        if ens.used_cus() > 864
            || ens.services.values().any(|s| s.primary().and_then(|c| c.subch_id).is_some_and(|id| !ens.subchannels.contains_key(&id)))
        {
            return None;
        }
        // a label of random bytes (a corrupted FIG that passed its CRC, such as
        // "Æ$ÿ¨Òð3'3¼m"): the database is not judged until it is gone
        if ens.label.as_ref().is_some_and(|l| garbled(&l.text))
            || ens.services.values().any(|s| s.label.as_ref().is_some_and(|l| garbled(&l.text)))
        {
            return None;
        }
        // a service without its component or an audio one without its
        // sub-channel: the FIC has not come complete yet (a reception begun anew)
        if ens.services.values().any(|s| s.primary().is_none_or(|c| c.tmid == 0 && c.subch_id.is_none())) {
            return None;
        }
        let key = format!("{channel}:{eid:04X}");
        let mut new = snapshot_of(ens);
        // a service whose sub-channel is not known yet keeps the one it had: a packet
        // one waits for FIG 0/3, which a weak signal brings late (unknown is no change)
        if let Some(o) = self.snapshots.get(&key) {
            for (sid, s) in new.services.iter_mut() {
                if let Some(os) = o.services.get(sid).filter(|os| s.subch.is_none() && os.subch.is_some()) {
                    s.bitrate = os.bitrate;
                    s.protection = os.protection.clone();
                    s.subch = os.subch;
                }
            }
        }
        let mut changes = Vec::new();
        let mut keep_label: Vec<String> = Vec::new();
        let mut keep_pty: Vec<String> = Vec::new();
        let mut first = !self.snapshots.contains_key(&key);
        // the ensemble of another EId heard here last, compared with until the
        // new one is taken (it stays apart while a new structure waits)
        let mut renumbered: Option<Snapshot> = None;
        if first {
            // another EId on this channel: compared with the ensemble heard there
            // last (a multiplex given a new identity, or another one now)
            let prefix = format!("{channel}:");
            let prev = self
                .snapshots
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .max_by_key(|(_, s)| s.seen)
                .map(|(k, s)| (k.clone(), s.clone()));
            // the same stations under a new EId are that multiplex renumbered;
            // others are another multiplex (an opening brings a foreign one
            // onto the channel of a local one)
            let same_stations = |ps: &Snapshot| {
                let shared = new.services.keys().filter(|sid| ps.services.contains_key(*sid)).count();
                shared * 2 >= new.services.len().max(1)
            };
            if let Some((pk, ps)) = prev.filter(|(_, s)| s.seen > 0 && same_stations(s)) {
                changes.push(Change {
                    kind: "ensembleId".into(),
                    service: None,
                    sid: None,
                    from: Some(pk.trim_start_matches(&prefix).to_string()),
                    to: Some(format!("{eid:04X}")),
                });
                renumbered = Some(ps);
                first = false;
            } else if self.fresh_until.is_none_or(|t| std::time::Instant::now() > t) {
                // a multiplex never heard here (e.g. an opening of the band)
                let mut names: Vec<&str> = new.services.values().map(|s| s.label.trim()).collect();
                names.sort_by_key(|n| n.to_lowercase());
                changes.push(Change {
                    kind: "ensembleNew".into(),
                    service: None,
                    sid: None,
                    from: None,
                    to: Some(format!("{} ({}): {}", new.label.trim(), new.services.len(), names.join(", "))),
                });
            }
        }
        if !first {
            if let Some(o) = renumbered.as_ref().or_else(|| self.snapshots.get(&key)) {
                let now = structure(&new);
                if structure(o) != now {
                    match self.pending_structure.get(&key) {
                        Some((s, since)) if *s == now && since.elapsed() >= STRUCTURE_CONFIRM => {
                            self.pending_structure.remove(&key);
                        }
                        // (still young: the rest waits for it too)
                        Some((s, _)) if *s == now => return None,
                        _ => {
                            self.pending_structure.insert(key.clone(), (now, std::time::Instant::now()));
                            return None;
                        }
                    }
                } else {
                    // (back as it was: nothing happened)
                    self.pending_structure.remove(&key);
                }
            }
        }
        if let Some(ps) = renumbered {
            self.snapshots.insert(key.clone(), ps);
        }
        let old = self.snapshots.entry(key).or_insert_with(|| Snapshot {
            label: new.label.clone(),
            short: new.short.clone(),
            ..Default::default()
        });
        if !first {
            if old.label != new.label {
                changes.push(Change {
                    kind: "ensembleLabel".into(),
                    service: None,
                    sid: None,
                    from: Some(old.label.clone()),
                    to: Some(new.label.clone()),
                });
            }
            if old.used_cu != 0 && old.used_cu != new.used_cu {
                changes.push(Change {
                    kind: "capacity".into(),
                    service: None,
                    sid: None,
                    from: Some(format!("{} CU", old.used_cu)),
                    to: Some(format!("{} CU", new.used_cu)),
                });
            }
            // which sub-channels changed (with the services using them)
            if !old.subchannels.is_empty() {
                let users = |id: u8| -> Option<String> {
                    let names: Vec<String> = ens
                        .services
                        .values()
                        .filter(|s| s.components.iter().any(|c| c.subch_id == Some(id)))
                        .map(|s| s.label.as_ref().map(|l| l.text.clone()).unwrap_or_else(|| format!("{:04X}", s.sid)))
                        .collect();
                    Some(if names.is_empty() { format!("SubCh {id}") } else { format!("SubCh {id} ({})", names.join(", ")) })
                };
                for (id, d) in &new.subchannels {
                    match old.subchannels.get(id) {
                        None => changes.push(Change {
                            kind: "subchAdded".into(),
                            service: users(*id),
                            sid: None,
                            from: None,
                            to: Some(d.clone()),
                        }),
                        Some(o) if o != d => changes.push(Change {
                            kind: "subchChanged".into(),
                            service: users(*id),
                            sid: None,
                            from: Some(o.clone()),
                            to: Some(d.clone()),
                        }),
                        _ => {}
                    }
                }
                for (id, o) in &old.subchannels {
                    if !new.subchannels.contains_key(id) {
                        changes.push(Change {
                            kind: "subchRemoved".into(),
                            service: Some(format!("SubCh {id}")),
                            sid: None,
                            from: Some(o.clone()),
                            to: None,
                        });
                    }
                }
            }
            for (sid, s) in &new.services {
                match old.services.get(sid) {
                    None => changes.push(Change {
                        kind: "serviceAdded".into(),
                        service: Some(s.label.clone()),
                        sid: Some(sid.clone()),
                        from: None,
                        to: Some(format!("{} {} kbps {}", s.codec, s.bitrate, s.protection)),
                    }),
                    Some(o) => {
                        let mut diff = |kind: &str, a: String, b: String| {
                            if a != b {
                                changes.push(Change {
                                    kind: kind.into(),
                                    service: Some(s.label.clone()),
                                    sid: Some(sid.clone()),
                                    from: Some(a),
                                    to: Some(b),
                                });
                            }
                        };
                        let pkey = format!("{channel}:{eid:04X}:{sid}");
                        let pty_key = format!("{pkey}:pty");
                        let renamed = o.label != s.label || o.short != s.short;
                        if renamed && !dynamic.iter().any(|d| format!("{d:04X}") == *sid) {
                            match self.pending_labels.get(&pkey) {
                                Some((l, sh, t)) if *l == s.label && *sh == s.short => {
                                    if t.elapsed() >= LABEL_CONFIRM {
                                        diff("label", o.label.clone(), s.label.clone());
                                        diff("shortLabel", o.short.clone(), s.short.clone());
                                        self.pending_labels.remove(&pkey);
                                    } else {
                                        keep_label.push(sid.clone());
                                    }
                                }
                                _ => {
                                    self.pending_labels.insert(pkey, (s.label.clone(), s.short.clone(), std::time::Instant::now()));
                                    keep_label.push(sid.clone());
                                }
                            }
                        } else if !renamed {
                            self.pending_labels.remove(&pkey);
                        }
                        diff("codec", o.codec.clone(), s.codec.clone());
                        diff("bitrate", format!("{} kbps", o.bitrate), format!("{} kbps", s.bitrate));
                        diff("protection", o.protection.clone(), s.protection.clone());
                        // PTY (FIG 0/17) and applications (FIG 0/13) may simply not have
                        // arrived yet: a missing value counts only after a long observation.
                        // A dynamic PTY is followed silently, other changes once they lasted.
                        if (s.pty.is_some() || long) && o.pty != s.pty && !s.pty_dynamic {
                            match self.pending_pty.get(&pty_key) {
                                Some((p, t)) if *p == s.pty && t.elapsed() >= PTY_CONFIRM => {
                                    diff(
                                        "pty",
                                        o.pty.map(tables::pty_name).unwrap_or("-").to_string(),
                                        s.pty.map(tables::pty_name).unwrap_or("-").to_string(),
                                    );
                                    self.pending_pty.remove(&pty_key);
                                }
                                Some((p, _)) if *p == s.pty => keep_pty.push(sid.clone()),
                                _ => {
                                    self.pending_pty.insert(pty_key, (s.pty, std::time::Instant::now()));
                                    keep_pty.push(sid.clone());
                                }
                            }
                        } else if o.pty == s.pty {
                            self.pending_pty.remove(&pty_key);
                        }
                        if !s.user_apps.is_empty() || long {
                            diff("userApps", o.user_apps.join(", "), s.user_apps.join(", "));
                        }
                    }
                }
            }
            for (sid, o) in &old.services {
                if !new.services.contains_key(sid) {
                    // (what it was, as "serviceAdded" tells what it is)
                    changes.push(Change {
                        kind: "serviceRemoved".into(),
                        service: Some(o.label.clone()),
                        sid: Some(sid.clone()),
                        from: Some(format!("{} {} kbps {}", o.codec, o.bitrate, o.protection)),
                        to: None,
                    });
                }
            }
        }
        // (linking, frequency and other ensemble information - FIG 0/6, 0/21,
        // 0/24 - is not reported: it changes too often to be news)
        old.label = new.label.clone();
        old.short = new.short.clone();
        // keep known slow values that this (short) observation did not see yet
        let mut services = new.services;
        // names waiting for confirmation stay as they were
        for sid in &keep_label {
            if let (Some(s), Some(o)) = (services.get_mut(sid), old.services.get(sid)) {
                s.label = o.label.clone();
                s.short = o.short.clone();
            }
        }
        for sid in &keep_pty {
            if let (Some(s), Some(o)) = (services.get_mut(sid), old.services.get(sid)) {
                s.pty = o.pty;
            }
        }
        if !long {
            for (sid, s) in services.iter_mut() {
                if let Some(o) = old.services.get(sid) {
                    if s.pty.is_none() {
                        s.pty = o.pty;
                    }
                    if s.user_apps.is_empty() {
                        s.user_apps = o.user_apps.clone();
                    }
                }
            }
        }
        old.services = services;
        old.used_cu = new.used_cu;
        old.subchannels = new.subchannels;
        old.ecc = new.ecc;
        old.observations += 1;
        old.seen = crate::servicelist::now();
        self.save();
        // what the admin chose not to hear about (the state above stays current)
        let ens_id = format!("{:02X}{eid:04X}", ens.ecc.unwrap_or(0));
        if mute.ensembles.iter().any(|m| m.id == ens_id) {
            changes.clear();
        }
        changes.retain(|c| c.sid.as_ref().is_none_or(|sid| !mute.services.iter().any(|m| m.id == *sid)));
        if changes.is_empty() {
            return None;
        }
        let ev = ChangeEvent {
            time: crate::servicelist::now(),
            channel: channel.to_string(),
            eid: ens_id,
            ensemble: ens.label.as_ref().map(|l| l.text.clone()).unwrap_or_default(),
            changes,
            report: None,
        };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log) {
            if let Ok(l) = serde_json::to_string(&ev) {
                let _ = writeln!(f, "{l}");
            }
        }
        self.recent.push(ev.clone());
        if self.recent.len() > 500 {
            self.recent.remove(0);
        }
        Some(ev)
    }
}

/// Events held back for Discord, the changes of a multiplex merged into one.
#[derive(Default)]
struct Batch {
    /// Per multiplex: the merged event and the time of its first change.
    pending: Vec<(String, ChangeEvent, std::time::Instant)>,
}

impl Batch {
    /// As saved: the first change's time in unix seconds.
    fn load(path: &Path) -> Batch {
        let Some(items) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Vec<(String, ChangeEvent, i64)>>(&s).ok())
        else {
            return Batch::default();
        };
        let (now, unix) = (std::time::Instant::now(), crate::servicelist::now());
        let pending = items
            .into_iter()
            .map(|(k, ev, since)| (k, ev, now.checked_sub(std::time::Duration::from_secs((unix - since).max(0) as u64)).unwrap_or(now)))
            .collect();
        Batch { pending }
    }

    fn save(&self, path: &Path) {
        if self.pending.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        let unix = crate::servicelist::now();
        let items: Vec<(&String, &ChangeEvent, i64)> =
            self.pending.iter().map(|(k, ev, since)| (k, ev, unix - since.elapsed().as_secs() as i64)).collect();
        if let Ok(s) = serde_json::to_string(&items) {
            let _ = std::fs::write(path, s);
        }
    }

    fn add(&mut self, ev: &ChangeEvent, now: std::time::Instant) {
        let key = format!("{}:{}", ev.channel, ev.eid);
        let i = match self.pending.iter().position(|(k, _, _)| *k == key) {
            Some(i) => i,
            None => {
                self.pending.push((key, ChangeEvent { changes: Vec::new(), ..ev.clone() }, now));
                self.pending.len() - 1
            }
        };
        let held = &mut self.pending[i].1;
        held.ensemble = ev.ensemble.clone();
        for c in &ev.changes {
            merge(&mut held.changes, c.clone());
        }
    }

    /// The events whose time is up, dropped when nothing is left of them.
    fn due(&mut self, now: std::time::Instant) -> Vec<ChangeEvent> {
        let mut due = Vec::new();
        self.pending.retain(|(_, ev, since)| {
            let keep = now < *since + DISCORD_BATCH;
            if !keep && !ev.changes.is_empty() {
                due.push(ev.clone());
            }
            keep
        });
        due
    }
}

/// What a change is about: a service (its SId), a sub-channel ("SubCh 3") or
/// the ensemble ("").
fn subject(c: &Change) -> String {
    match (&c.sid, &c.service) {
        (Some(sid), _) => sid.clone(),
        (None, Some(s)) if c.kind.starts_with("subch") => s.split('(').next().unwrap_or("").trim().to_string(),
        _ => String::new(),
    }
}

/// The net effect of a change on those collected before it: a value changed
/// twice counts once (A→B, B→C: A→C) or not at all (A→B, B→A); what came
/// and went again never was; what went and came back as it was never left.
fn merge(into: &mut Vec<Change>, c: Change) {
    let what = subject(&c);
    match c.kind.as_str() {
        "ensembleNew" | "test" => into.push(c),
        "serviceAdded" | "subchAdded" => {
            let removed = c.kind.replace("Added", "Removed");
            match into.iter().position(|p| p.kind == removed && subject(p) == what && p.from == c.to) {
                Some(i) => {
                    let gone = into.remove(i);
                    // (back under another name: renamed)
                    if gone.service != c.service && c.kind == "serviceAdded" {
                        into.push(Change {
                            kind: "label".into(),
                            service: c.service.clone(),
                            sid: c.sid,
                            from: gone.service,
                            to: c.service,
                        });
                    }
                }
                None => into.push(c),
            }
        }
        "serviceRemoved" | "subchRemoved" => {
            // whatever happened to it before does not matter any more
            let added = c.kind.replace("Removed", "Added");
            let came = into.iter().any(|p| p.kind == added && subject(p) == what);
            into.retain(|p| subject(p) != what);
            if !came {
                into.push(c);
            }
        }
        _ => match into.iter().position(|p| p.kind == c.kind && subject(p) == what) {
            Some(i) => {
                into[i].to = c.to;
                into[i].service = c.service;
                if into[i].from == into[i].to {
                    into.remove(i);
                }
            }
            None => into.push(c),
        },
    }
}

/// The changes in a few lines: what came and what went in one line each, the
/// changes of a service together; the technical ones (sub-channels, capacity)
/// are left to the Changes view.
fn discord_lines(ev: &ChangeEvent) -> Vec<String> {
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    let name = |c: &Change| format!("**{}**", c.service.as_deref().map(str::trim).unwrap_or("?"));
    let mut lines = Vec::new();
    for c in &ev.changes {
        match c.kind.as_str() {
            "ensembleNew" => lines.push(format!("🆕 nowy multipleks: {}", text(&c.to))),
            "ensembleId" => lines.push(format!("🆔 EId multipleksu: {} → {}", text(&c.from), text(&c.to))),
            "ensembleLabel" => lines.push(format!("✏️ nazwa multipleksu: „{}” → „{}”", text(&c.from), text(&c.to))),
            _ => {}
        }
    }
    let added: Vec<String> = ev
        .changes
        .iter()
        .filter(|c| c.kind == "serviceAdded")
        .map(|c| format!("{} ({}{})", name(c), c.sid.as_deref().map(|s| format!("{s}, ")).unwrap_or_default(), text(&c.to)))
        .collect();
    if !added.is_empty() {
        lines.push(format!("➕ nowe: {}", added.join(", ")));
    }
    let removed: Vec<String> = ev.changes.iter().filter(|c| c.kind == "serviceRemoved").map(name).collect();
    if !removed.is_empty() {
        lines.push(format!("➖ zniknęły: {}", removed.join(", ")));
    }
    // the rest by service, in the order they came
    let mut services: Vec<(String, Vec<String>)> = Vec::new();
    for c in &ev.changes {
        let (from, to) = (text(&c.from), text(&c.to));
        let part = match c.kind.as_str() {
            "label" => format!("nazwa „{from}” → „{to}”"),
            "shortLabel" => format!("skrót „{from}” → „{to}”"),
            "codec" | "bitrate" | "protection" => format!("{from} → {to}"),
            "pty" => format!("PTY {from} → {to}"),
            "userApps" => format!("aplikacje: {} → {}", if from.is_empty() { "-" } else { &from }, if to.is_empty() { "-" } else { &to }),
            _ => continue,
        };
        let who = name(c);
        match services.iter_mut().find(|(n, _)| *n == who) {
            Some((_, parts)) => parts.push(part),
            None => services.push((who, vec![part])),
        }
    }
    for (who, parts) in services {
        lines.push(format!("🔄 {who}: {}", parts.join(", ")));
    }
    lines
}

/// The Discord message of a new multiplex, for its report (discord_report).
#[derive(Default)]
pub struct Posted {
    /// The posting is over (sent or failed).
    done: bool,
    /// The id and text of the last message posted.
    last: Option<(String, String)>,
}

pub type DiscordMsg = std::sync::Arc<parking_lot::Mutex<Posted>>;

fn discord_header(station: &str, ev: &ChangeEvent) -> String {
    format!("**{} · {}** · {} ({})", station, ev.channel, ev.ensemble.trim(), ev.eid)
}

/// The webhook's URL with `path` after it and `query` among its parameters
/// (that of a thread has ?thread_id=...).
fn webhook_url(webhook: &str, path: &str, query: &str) -> String {
    let (base, q) = webhook.split_once('?').unwrap_or((webhook, ""));
    let q: Vec<&str> = [q, query].into_iter().filter(|x| !x.is_empty()).collect();
    format!("{}{path}{}{}", base.trim_end_matches('/'), if q.is_empty() { "" } else { "?" }, q.join("&"))
}

/// Sends a message (or its new text); the id of the message when Discord
/// gives it (?wait=true).
fn discord_send(req: ureq::RequestBuilder<ureq::typestate::WithBody>, body: serde_json::Value) -> Result<Option<String>, ureq::Error> {
    let mut resp = req
        .header("Content-Type", "application/json")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .send(body.to_string())?;
    let data = resp.body_mut().with_config().limit(1 << 20).read_to_vec().unwrap_or_default();
    Ok(serde_json::from_slice::<serde_json::Value>(&data).ok().and_then(|v| v["id"].as_str().map(str::to_string)))
}

/// Post a change event to a Discord webhook (in a background thread).
pub fn post_discord(webhook: &str, station: &str, ev: &ChangeEvent) -> DiscordMsg {
    let posted = DiscordMsg::default();
    let body = discord_lines(ev);
    // (only sub-channels or capacity: the Changes view has them)
    if webhook.is_empty() || !webhook.starts_with("https://") || body.is_empty() {
        posted.lock().done = true;
        return posted;
    }
    let mut lines = vec![discord_header(station, ev)];
    lines.extend(body);
    // Discord limits messages to 2000 characters
    let mut messages = Vec::new();
    let mut cur = String::new();
    for l in lines {
        if cur.len() + l.len() + 1 > 1900 {
            messages.push(std::mem::take(&mut cur));
        }
        cur.push_str(&l);
        cur.push('\n');
    }
    if !cur.is_empty() {
        messages.push(cur);
    }
    let url = webhook_url(webhook, "", "wait=true");
    let msg = posted.clone();
    std::thread::spawn(move || {
        for m in messages {
            // labels come from the air: "@everyone" in one must not ping the server
            let body = serde_json::json!({"username": "MagicDAB", "content": m, "allowed_mentions": {"parse": []}});
            match discord_send(ureq::post(&url), body) {
                Ok(id) => msg.lock().last = id.map(|id| (id, m)),
                Err(e) => {
                    log::warn!("Discord webhook failed: {e}");
                    msg.lock().last = None;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        msg.lock().done = true;
    });
    posted
}

/// The report of a new multiplex onto its message (post_discord), edited
/// in; as a message of its own when that one did not go or the whole would
/// be too long. `base`: the public address of this server, for the link to
/// the recording.
pub fn discord_report(webhook: &str, station: &str, base: Option<&str>, ev: &ChangeEvent, posted: DiscordMsg) {
    let Some(report) = ev.report.as_ref() else { return };
    if webhook.is_empty() || !webhook.starts_with("https://") {
        return;
    }
    let lines = report_lines(report, base).join("\n");
    let head = discord_header(station, ev);
    let webhook = webhook.to_string();
    std::thread::spawn(move || {
        // (a report of a few seconds may come while its message is on its way)
        for _ in 0..30 {
            if posted.lock().done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let last = posted.lock().last.clone();
        let sent = match last {
            Some((id, text)) if text.len() + lines.len() < 1900 => discord_send(
                ureq::patch(&webhook_url(&webhook, &format!("/messages/{id}"), "")),
                serde_json::json!({"content": format!("{text}{lines}"), "allowed_mentions": {"parse": []}}),
            ),
            _ => discord_send(
                ureq::post(&webhook_url(&webhook, "", "")),
                serde_json::json!({"username": "MagicDAB", "content": format!("{head}\n{lines}"), "allowed_mentions": {"parse": []}}),
            ),
        };
        if let Err(e) = sent {
            log::warn!("Discord webhook failed: {e}");
        }
    });
}

/// The flag of an ISO country code (regional indicator letters).
fn flag(iso: &str) -> String {
    iso.chars()
        .filter(char::is_ascii_alphabetic)
        .filter_map(|c| char::from_u32(0x1F1E6 + (c.to_ascii_uppercase() as u32 - 'A' as u32)))
        .collect()
}

/// A report in a few lines: the signal, then the transmitters, then the
/// recording (a link that plays it to an admin, the web's ?dxrec=).
fn report_lines(r: &Report, base: Option<&str>) -> Vec<String> {
    let mut head = format!("📶 SNR śr. {:.1} dB, max {:.1} dB przez {} s", r.snr_mean, r.snr_max, r.seconds);
    match r.ended.as_str() {
        "lost" => head.push_str(", potem zanik"),
        "retuned" => head.push_str(", potem przestrojony"),
        _ => {}
    }
    if r.fib_errors >= 0.005 {
        head.push_str(&format!(" · błędy FIC {:.0}%", r.fib_errors * 100.0));
    }
    if let Some(c) = &r.country {
        head.push_str(&format!(" · {} {c}", flag(c)));
    }
    let mut lines = vec![head];
    if r.transmitters.is_empty() {
        lines.push("📡 TII: nic nie odebrano".into());
    }
    const SHOWN: usize = 10;
    for t in r.transmitters.iter().take(SHOWN) {
        let mut l = format!("📡 TII {}", t.code);
        match &t.location {
            Some(loc) => l.push_str(&format!(" · {}{loc}", t.country.as_deref().map(|c| format!("{} ", flag(c))).unwrap_or_default())),
            None => l.push_str(" · nie ma w bazie"),
        }
        if let (Some(d), Some(a)) = (t.distance, t.azimuth) {
            l.push_str(&format!(" · {d:.0} km, {a:.0}°"));
        }
        if let Some(p) = t.power_kw.filter(|p| *p > 0.0) {
            l.push_str(&format!(" · {p} kW"));
        }
        // (its own signal: the multiplex' SNR plus its TII level, as in the TII view)
        l.push_str(&format!(" · ≈{:.1} dB", r.snr_mean + t.level));
        lines.push(l);
    }
    if r.transmitters.len() > SHOWN {
        lines.push(format!("📡 … i {} więcej", r.transmitters.len() - SHOWN));
    }
    match (&r.recording, base) {
        // (in <>: Discord shows no preview of it)
        (Some(n), Some(b)) => lines.push(format!("🎙 nagranie: <{}/?dxrec={n}>", b.trim_end_matches('/'))),
        (Some(n), None) => lines.push(format!("🎙 nagranie: {n}")),
        _ => {}
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicdab_dab::ensemble::{Component, Label, Service, Subchannel};
    use magicdab_dab::puncture::Protection;

    fn ensemble(services: u32) -> Ensemble {
        let mut e = Ensemble {
            eid: Some(0x6A1E),
            ecc: Some(0xE2),
            label: Some(Label { text: "MUX A".into(), short: "MXA".into() }),
            ..Default::default()
        };
        for i in 0..services {
            let id = i as u8 + 1;
            e.subchannels.insert(
                id,
                Subchannel {
                    id,
                    start: i as u16 * 48,
                    size: 48,
                    protection: Protection::Eep { option: 0, level: 1 },
                    bitrate: 48,
                    protection_label: "EEP 2-A".into(),
                    code_rate: String::new(),
                    fec_scheme: 0,
                },
            );
            let component = Component {
                tmid: 0,
                ctype: 63,
                subch_id: Some(id),
                scid: None,
                scids: Some(0),
                primary: true,
                ca: false,
                label: None,
                language: None,
                user_apps: Vec::new(),
                packet_address: None,
                dg_flag: false,
            };
            let sid = 0x7C40 + i;
            e.services.insert(
                sid,
                Service {
                    sid,
                    label: Some(Label { text: format!("S-{i}"), short: String::new() }),
                    components: vec![component],
                    ..Default::default()
                },
            );
        }
        e
    }

    fn tracker() -> (ChangeTracker, PathBuf) {
        let dir = std::env::temp_dir().join(format!("magicdab-changes-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut t = ChangeTracker::load(&dir);
        // (not a new installation learning the band)
        t.fresh_until = None;
        (t, dir)
    }

    /// The structure waiting for confirmation as if it had lasted long enough.
    fn age(t: &mut ChangeTracker) {
        for (_, since) in t.pending_structure.values_mut() {
            *since -= STRUCTURE_CONFIRM;
        }
    }

    #[test]
    fn a_packet_service_waiting_for_its_sub_channel_is_no_change() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        // the last service carries packet data (an EPG) in its sub-channel
        let mut full = ensemble(4);
        let epg = &mut full.services.get_mut(&0x7C43).unwrap().components[0];
        epg.tmid = 3;
        epg.ctype = 60;
        t.observe("11B", &full, true, &[], &mute);
        // weak signal: FIG 0/3 not here yet, the service without its sub-channel
        let mut waiting = full.clone();
        waiting.services.get_mut(&0x7C43).unwrap().components[0].subch_id = None;
        assert!(t.observe("11B", &waiting, true, &[], &mute).is_none());
        age(&mut t);
        assert!(t.observe("11B", &waiting, true, &[], &mute).is_none());
        assert!(t.observe("11B", &full, true, &[], &mute).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reception_begun_anew_is_no_change() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        let full = ensemble(17);
        t.observe("12D", &full, true, &[], &mute);
        // after a resync only one service, without its sub-channel yet
        let mut part = ensemble(17);
        part.subchannels.clear();
        part.services.retain(|sid, _| *sid == 0x7C40);
        part.services.get_mut(&0x7C40).unwrap().components[0].subch_id = None;
        assert!(t.observe("12D", &part, true, &[], &mute).is_none());
        // part of the services complete, the others not here yet
        let fewer = ensemble(5);
        assert!(t.observe("12D", &fewer, true, &[], &mute).is_none());
        // all back: nothing happened, nothing is waiting
        assert!(t.observe("12D", &full, true, &[], &mute).is_none());
        assert!(t.pending_structure.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn garbled_labels_are_recognised() {
        assert!(garbled("Ð$ÿ¬Òð7'1¼q"));
        for ok in ["Radio ALFA", "AB24", "Radio Północ Zachód", "Česká Stanice", "NRK Ærøy", "Radio 100% Hits", "Rock & Pop", "Hit (Żółć)"]
        {
            assert!(!garbled(ok), "{ok}");
        }
    }

    #[test]
    fn discord_batches_survive_a_restart() {
        let (mut t, dir) = tracker();
        let ev = ChangeEvent {
            time: 0,
            channel: "12D".into(),
            eid: "E21234".into(),
            ensemble: "MUX A".into(),
            changes: vec![Change {
                kind: "serviceRemoved".into(),
                service: Some("S-16".into()),
                sid: Some("7C50".into()),
                from: None,
                to: None,
            }],
            report: None,
        };
        t.defer(&ev);
        let again = ChangeTracker::load(&dir);
        assert_eq!(again.discord.pending.len(), 1);
        assert_eq!(again.discord.pending[0].1.changes.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_multiplex_on_a_known_channel_is_new() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        t.observe("12D", &ensemble(17), true, &[], &mute);
        // a foreign multiplex on the channel: other stations, another EId
        let mut foreign = ensemble(6);
        foreign.eid = Some(0x1A01);
        foreign.ecc = Some(0xE1);
        for (sid, s) in std::mem::take(&mut foreign.services) {
            foreign.services.insert(sid + 0x1000, Service { sid: sid + 0x1000, ..s });
        }
        let ev = t.observe("12D", &foreign, true, &[], &mute).expect("reported at once");
        assert_eq!(ev.changes.len(), 1);
        assert_eq!(ev.changes[0].kind, "ensembleNew");
        // the local one is still known as it was
        assert!(t.observe("12D", &ensemble(17), true, &[], &mute).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_eid_with_a_new_structure_is_reported_with_it() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        t.observe("12D", &ensemble(17), true, &[], &mute);
        let mut other = ensemble(16);
        other.eid = Some(0x6A1F);
        // (waits: and the EId change with it)
        assert!(t.observe("12D", &other, true, &[], &mute).is_none());
        assert!(t.observe("12D", &other, true, &[], &mute).is_none());
        age(&mut t);
        let ev = t.observe("12D", &other, true, &[], &mute).expect("reported once it lasted");
        assert!(ev.changes.iter().any(|c| c.kind == "ensembleId" && c.from.as_deref() == Some("6A1E") && c.to.as_deref() == Some("6A1F")));
        assert!(ev.changes.iter().any(|c| c.kind == "serviceRemoved"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_change_that_lasts_is_reported_once_in_few_lines() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        t.observe("12D", &ensemble(17), true, &[], &mute);
        let mut changed = ensemble(16);
        changed.subchannels.get_mut(&1).unwrap().bitrate = 64;
        assert!(t.observe("12D", &changed, true, &[], &mute).is_none());
        age(&mut t);
        let ev = t.observe("12D", &changed, true, &[], &mute).expect("reported once it lasted");
        assert!(ev.changes.iter().any(|c| c.kind == "serviceRemoved" && c.service.as_deref() == Some("S-16")));
        let lines = discord_lines(&ev);
        assert_eq!(lines, vec!["➖ zniknęły: **S-16**".to_string(), "🔄 **S-0**: 48 kbps → 64 kbps".to_string()]);
        // and not again
        assert!(t.observe("12D", &changed, true, &[], &mute).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn change(kind: &str, sid: Option<&str>, service: &str, from: Option<&str>, to: Option<&str>) -> Change {
        Change {
            kind: kind.into(),
            service: Some(service.into()),
            sid: sid.map(String::from),
            from: from.map(String::from),
            to: to.map(String::from),
        }
    }

    fn event(channel: &str, changes: Vec<Change>) -> ChangeEvent {
        ChangeEvent { time: 0, channel: channel.into(), eid: "E21234".into(), ensemble: "MUX A".into(), changes, report: None }
    }

    /// The net effect of the events of a multiplex, as Discord gets it.
    fn merged(events: Vec<Vec<Change>>) -> Vec<Change> {
        let mut b = Batch::default();
        let t0 = std::time::Instant::now();
        for ev in events {
            b.add(&event("12D", ev), t0);
        }
        b.due(t0 + DISCORD_BATCH).into_iter().flat_map(|ev| ev.changes).collect()
    }

    #[test]
    fn discord_gets_the_changes_of_a_multiplex_together_after_five_minutes() {
        let mut b = Batch::default();
        let t0 = std::time::Instant::now();
        let s = std::time::Duration::from_secs;
        b.add(
            &event(
                "12D",
                vec![
                    change("label", Some("7C41"), "B", Some("A"), Some("B")),
                    change("serviceAdded", Some("7C45"), "New", None, Some("DAB+ 48 kbps EEP 2-A")),
                    change("bitrate", Some("7C42"), "S-2", Some("48 kbps"), Some("64 kbps")),
                ],
            ),
            t0,
        );
        assert!(b.due(t0 + s(299)).is_empty());
        // another multiplex waits on its own
        b.add(&event("5A", vec![change("ensembleLabel", None, "", Some("X"), Some("Y"))]), t0 + s(100));
        b.add(
            &event(
                "12D",
                vec![
                    change("label", Some("7C41"), "C", Some("B"), Some("C")),
                    change("serviceRemoved", Some("7C45"), "New", Some("DAB+ 48 kbps EEP 2-A"), None),
                    change("bitrate", Some("7C42"), "S-2", Some("64 kbps"), Some("48 kbps")),
                    change("pty", Some("7C43"), "S-3", Some("-"), Some("Pop Music")),
                ],
            ),
            t0 + s(60),
        );
        let due = b.due(t0 + s(300));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].channel, "12D");
        // the name A→B→C is A→C, the station added and removed never was, the
        // bitrate that went back did not change
        assert_eq!(discord_lines(&due[0]), vec!["🔄 **C**: nazwa „A” → „C”".to_string(), "🔄 **S-3**: PTY - → Pop Music".to_string()]);
        assert!(b.due(t0 + s(399)).is_empty());
        assert_eq!(b.due(t0 + s(400)).len(), 1);
        assert!(b.pending.is_empty());
    }

    #[test]
    fn what_went_and_came_back_as_it_was_never_left() {
        let gone = || change("serviceRemoved", Some("7C45"), "X", Some("DAB+ 48 kbps EEP 2-A"), None);
        let back = |name: &str, what: &str| change("serviceAdded", Some("7C45"), name, None, Some(what));
        assert!(merged(vec![vec![gone()], vec![back("X", "DAB+ 48 kbps EEP 2-A")]]).is_empty());
        // back with other parameters: both are told
        assert_eq!(merged(vec![vec![gone()], vec![back("X", "DAB+ 64 kbps EEP 2-A")]]).len(), 2);
        // back under another name: renamed
        let renamed = merged(vec![vec![gone()], vec![back("Y", "DAB+ 48 kbps EEP 2-A")]]);
        assert_eq!(renamed.len(), 1);
        assert_eq!((renamed[0].kind.as_str(), renamed[0].from.as_deref(), renamed[0].to.as_deref()), ("label", Some("X"), Some("Y")));
        // what happened to a station before it went does not matter
        let went = merged(vec![vec![change("bitrate", Some("7C45"), "X", Some("48 kbps"), Some("64 kbps"))], vec![gone()]]);
        assert_eq!(went.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(), vec!["serviceRemoved"]);
        // values of the ensemble: back is nothing, twice is once
        let ens = |kind: &str, from: &str, to: &str| change(kind, None, "", Some(from), Some(to));
        assert!(merged(vec![vec![ens("ensembleLabel", "A", "B")], vec![ens("ensembleLabel", "B", "A")]]).is_empty());
        let ids = merged(vec![vec![ens("ensembleId", "6A1E", "6A1F")], vec![ens("ensembleId", "6A1F", "6A28")]]);
        assert_eq!(ids.iter().map(|c| (c.from.as_deref().unwrap(), c.to.as_deref().unwrap())).collect::<Vec<_>>(), vec![("6A1E", "6A28")]);
        // sub-channels are told apart by their number
        let sub = |kind: &str, who: &str, from: Option<&str>, to: Option<&str>| change(kind, None, who, from, to);
        let subs = merged(vec![
            vec![
                sub("subchRemoved", "SubCh 3", Some("start 0, 48 CU, EEP 2-A, 48 kbps"), None),
                sub("subchChanged", "SubCh 4 (S-4)", Some("a"), Some("b")),
            ],
            vec![sub("subchAdded", "SubCh 3 (S-3)", None, Some("start 0, 48 CU, EEP 2-A, 48 kbps"))],
        ]);
        assert_eq!(subs.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(), vec!["subchChanged"]);
    }

    #[test]
    fn a_report_stays_with_its_event_after_a_restart() {
        let (mut t, dir) = tracker();
        let mute = crate::config::NotifyMute::default();
        let ev = t.observe("12D", &ensemble(5), true, &[], &mute).expect("a new multiplex");
        let report = Report {
            seconds: 60,
            snr_mean: 8.4,
            snr_max: 11.2,
            transmitters: vec![ReportTx { code: "3-1".into(), ..Default::default() }],
            ..Default::default()
        };
        let got = t.attach_report(ev.time, &ev.channel, &ev.eid, report.clone()).expect("its event");
        assert_eq!(got.report.as_ref(), Some(&report));
        // (one event again, with its changes and its report)
        let again = ChangeTracker::load(&dir);
        assert_eq!(again.recent.len(), 1);
        assert_eq!(again.recent[0].changes.len(), ev.changes.len());
        assert_eq!(again.recent[0].report.as_ref(), Some(&report));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_report_in_a_few_lines() {
        let r = Report {
            seconds: 23,
            ended: "lost".into(),
            snr_mean: 6.0,
            snr_max: 9.5,
            fib_errors: 0.02,
            country: Some("CZ".into()),
            transmitters: vec![
                ReportTx {
                    code: "5-2".into(),
                    level: 0.0,
                    location: Some("Hill A".into()),
                    country: Some("CZ".into()),
                    distance: Some(123.4),
                    azimuth: Some(250.0),
                    power_kw: Some(5.0),
                },
                ReportTx { code: "7-1".into(), level: -6.0, ..Default::default() },
            ],
            recording: Some("2026-01-02_030405_12C_1234.eti".into()),
        };
        let l = report_lines(&r, Some("https://rx.example.org/"));
        assert_eq!(l[0], "📶 SNR śr. 6.0 dB, max 9.5 dB przez 23 s, potem zanik · błędy FIC 2% · 🇨🇿 CZ");
        assert_eq!(l[1], "📡 TII 5-2 · 🇨🇿 Hill A · 123 km, 250° · 5 kW · ≈6.0 dB");
        assert_eq!(l[2], "📡 TII 7-1 · nie ma w bazie · ≈0.0 dB");
        assert_eq!(l[3], "🎙 nagranie: <https://rx.example.org/?dxrec=2026-01-02_030405_12C_1234.eti>");
        assert_eq!(l.len(), 4);
    }

    #[test]
    fn webhook_urls_keep_their_thread() {
        assert_eq!(webhook_url("https://x/api/webhooks/1/abc", "", "wait=true"), "https://x/api/webhooks/1/abc?wait=true");
        assert_eq!(
            webhook_url("https://x/api/webhooks/1/abc?thread_id=9", "/messages/5", ""),
            "https://x/api/webhooks/1/abc/messages/5?thread_id=9"
        );
        assert_eq!(
            webhook_url("https://x/api/webhooks/1/abc?thread_id=9", "", "wait=true"),
            "https://x/api/webhooks/1/abc?thread_id=9&wait=true"
        );
    }
}
