//! DAB/FM service following for the service heard: which FM programme belongs
//! to it (RDS PI = SId, FIG 0/6 links), where it is (FIG 0/21, frequencies
//! verified before, RDS AF, stations heard before, a band scan), finding the
//! best frequency and antenna with the FM tuner while DAB plays, and deciding
//! which one is heard (see `linker`).

use crate::linker::{Linker, Measurement, Source};
use magicdab_dab::audio::AudioStats;
use magicdab_dab::ensemble::Ensemble;
use magicdab_dab::receiver::ServiceKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Auto,
    Dab,
    Fm,
}

/// What the FM tuner reports, as far as linking is concerned.
pub struct FmView {
    pub connected: bool,
    pub khz: u32,
    pub antenna: u8,
    pub antennas: u8,
    pub level: f32,
    pub cci: i32,
    pub aci: i32,
    pub pi: Option<(u16, u8)>,
    pub ps: String,
    pub af: Vec<u32>,
    pub scan: Vec<(u32, f32)>,
    /// Incremented with every band scan result.
    pub scan_id: u64,
}

pub enum Action {
    Tune(u32),
    Antenna(u8),
    Scan,
}

/// Quality of a frequency on one antenna.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measure {
    antenna: u8,
    level: f32,
    cci: f32,
    aci: f32,
    pi: Option<String>,
    /// Level less penalties for interference; none without the right PI.
    score: Option<f32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    #[serde(skip)]
    at: Instant,
    result: &'static str,
    measures: Vec<Measure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    khz: u32,
    sources: Vec<&'static str>,
    check: Option<Check>,
    best: bool,
}

#[derive(Default)]
struct Samples {
    level: Vec<f32>,
    cci: Vec<f32>,
    aci: Vec<f32>,
    pi: Option<u16>,
}

enum Phase {
    /// Nothing to follow (see `reason`).
    Idle,
    Scanning {
        since: Instant,
        scan_id: u64,
    },
    /// PI of stations of the band scan (no frequency of the service known).
    Probing {
        queue: VecDeque<u32>,
        cur: Option<(u32, Instant, Option<Instant>)>,
    },
    /// Quality of the candidates on every antenna.
    Evaluating {
        plan: VecDeque<(u32, u8)>,
        total: usize,
        cur: Option<(u32, u8, Instant, Option<Instant>)>,
        samples: Samples,
    },
    Locked {
        khz: u32,
        antenna: u8,
        since: Instant,
        level: f32,
        wrong_pi: Option<Instant>,
        weak: Option<Instant>,
        moved: Option<Instant>,
    },
    /// Nothing found; tried again later.
    Failed {
        since: Instant,
    },
}

/// Where a PI was verified: PI -> (kHz, antenna, level, unix time).
type Known = HashMap<String, Vec<(u32, u8, f32, i64)>>;
/// PI heard on each frequency: kHz -> (PI or none without RDS, level, unix time).
type Stations = HashMap<u32, (Option<u16>, f32, i64)>;

/// How long the programme heard on a frequency is trusted without tuning there.
const STATION_FRESH_S: i64 = 7 * 86400;
/// Periodic new evaluation of frequencies and antennas while DAB is heard.
const REEVALUATE: Duration = Duration::from_secs(15 * 60);

pub struct Follower {
    pub enabled: bool,
    pub mode: Mode,
    service: Option<ServiceKey>,
    label: String,
    ecc: Option<u8>,
    targets: Vec<(u16, &'static str)>,
    candidates: Vec<Candidate>,
    phase: Phase,
    reason: String,
    scanned: Option<Instant>,
    suspended_until: Option<Instant>,
    known: Known,
    known_path: PathBuf,
    stations: Stations,
    stations_path: PathBuf,
    dirty: bool,
    /// Stations of the last band scan (kHz, level) and when it was made.
    peaks: Vec<(u32, f32)>,
    peaks_at: Option<Instant>,
    seen_scan_id: u64,
    // DAB quality of the service
    au: VecDeque<(Instant, u64, u64)>,
    au_rate: f32,
    /// The sub-channel comes, but not one audio unit in it for seconds (the
    /// broadcaster's encoder sends nothing valid): FM plays for good.
    no_audio: bool,
    last_stats: Option<Instant>,
    synced: bool,
    snr: f32,
    good_since: Option<Instant>,
    bad_since: Option<Instant>,
    last_measure: Instant,
    last: Option<Measurement>,
    history: VecDeque<(i64, f32, f32, bool)>,
    events: VecDeque<(i64, String)>,
}

/// Stations of a band scan: local maxima of at least 20 dBf, strongest first.
fn peaks(scan: &[(u32, f32)]) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, f32)> = Vec::new();
    for (i, &(f, l)) in scan.iter().enumerate() {
        let prev = if i > 0 { scan[i - 1].1 } else { f32::MIN };
        let next = scan.get(i + 1).map_or(f32::MIN, |p| p.1);
        if l >= 20.0 && l >= prev && l > next {
            out.push((f, l));
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn hex(pi: u16) -> String {
    format!("{pi:04X}")
}

fn mhz(khz: u32) -> String {
    format!("{:.1} MHz", khz as f64 / 1000.0)
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

fn load<T: serde::de::DeserializeOwned + Default>(p: &Path) -> T {
    std::fs::read(p).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

impl Follower {
    pub fn new(data_dir: &Path, enabled: bool, mode: Mode) -> Self {
        let known_path = data_dir.join("fmlinks.json");
        let stations_path = data_dir.join("fmstations.json");
        Follower {
            enabled,
            mode,
            service: None,
            label: String::new(),
            ecc: None,
            targets: Vec::new(),
            candidates: Vec::new(),
            phase: Phase::Idle,
            reason: String::new(),
            scanned: None,
            suspended_until: None,
            known: load(&known_path),
            known_path,
            stations: load(&stations_path),
            stations_path,
            dirty: false,
            peaks: Vec::new(),
            peaks_at: None,
            seen_scan_id: 0,
            au: VecDeque::new(),
            au_rate: 0.0,
            no_audio: false,
            last_stats: None,
            synced: false,
            snr: 0.0,
            good_since: None,
            bad_since: None,
            last_measure: Instant::now(),
            last: None,
            history: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    pub fn event(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::info!("linking: {text}");
        self.events.push_back((now_ms(), text));
        while self.events.len() > 200 {
            self.events.pop_front();
        }
    }

    pub fn service(&self) -> Option<ServiceKey> {
        self.service
    }

    fn targets_text(&self) -> String {
        self.targets.iter().map(|t| hex(t.0)).collect::<Vec<_>>().join("/")
    }

    /// The service heard changed (or its ensemble data): PIs and candidates.
    pub fn set_service(&mut self, key: Option<ServiceKey>, ens: &Ensemble) {
        let changed = key != self.service;
        if changed {
            self.service = key;
            self.candidates.clear();
            self.phase = Phase::Idle;
            self.last = None;
            self.history.clear();
            self.au.clear();
            self.good_since = None;
            self.bad_since = None;
        }
        let Some(key) = key else {
            self.targets.clear();
            self.reason = "no service".into();
            return;
        };
        let svc = ens.services.get(&key.sid);
        let label = svc.and_then(|s| s.label.as_ref()).map(|l| l.text.trim().to_string()).unwrap_or_default();
        let ecc = svc.and_then(|s| s.ecc).or(ens.ecc);
        let mut targets: Vec<(u16, &'static str)> = Vec::new();
        if key.sid <= 0xFFFF {
            targets.push((key.sid as u16, "SId"));
        }
        for set in ens.linkage.iter().filter(|l| !l.oe) {
            let ours = set.key_sid == Some(key.sid) || set.ids.iter().any(|i| i.kind == 0 && i.id == key.sid);
            if !ours {
                continue;
            }
            for id in set.ids.iter().filter(|i| i.kind == 1 && i.id <= 0xFFFF) {
                if !targets.iter().any(|t| t.0 == id.id as u16) {
                    targets.push((id.id as u16, if set.hard { "FIG 0/6 hard link" } else { "FIG 0/6 soft link" }));
                }
            }
        }
        let before = (self.targets.clone(), self.candidates.len());
        if changed || targets != self.targets {
            self.targets = targets;
            if !label.is_empty() && changed {
                self.event(format!("service {label} ({:04X}): FM with PI {}", key.sid, self.targets_text()));
            }
        }
        self.label = label;
        self.ecc = ecc;
        for fi in ens.frequency_info.iter().filter(|f| f.rm == 8 || f.rm == 9) {
            if self.targets.iter().any(|t| t.0 as u32 == fi.id) {
                for f in &fi.frequencies {
                    self.add_candidate(f.khz, "FIG 0/21");
                }
            }
        }
        let known: Vec<u32> =
            self.targets.iter().flat_map(|t| self.known.get(&hex(t.0)).cloned().unwrap_or_default()).map(|k| k.0).collect();
        for k in known {
            self.add_candidate(k, "verified before");
        }
        let heard: Vec<u32> = self.stations.iter().filter(|(_, v)| v.0.is_some_and(|p| self.pi_matches(p))).map(|(k, _)| *k).collect();
        for k in heard {
            self.add_candidate(k, "station list");
        }
        // new PIs or frequencies (e.g. slow FIGs arriving): search again
        if !changed && (before.0 != self.targets || before.1 != self.candidates.len()) && matches!(self.phase, Phase::Failed { .. }) {
            self.event("new linking information, searching again");
            self.phase = Phase::Idle;
        }
    }

    fn add_candidate(&mut self, khz: u32, source: &'static str) {
        if !(87_500..=108_000).contains(&khz) {
            return;
        }
        match self.candidates.iter_mut().find(|c| c.khz == khz) {
            Some(c) => {
                if !c.sources.contains(&source) {
                    c.sources.push(source);
                }
            }
            None => self.candidates.push(Candidate { khz, sources: vec![source], check: None, best: false }),
        }
    }

    /// Statistics of the decoded DAB audio of the service.
    pub fn on_stats(&mut self, s: &AudioStats, now: Instant) {
        self.last_stats = Some(now);
        self.au.push_back((now, s.au_ok, s.au_errors));
        while self.au.front().is_some_and(|(t, _, _)| now.duration_since(*t) > Duration::from_millis(2500)) {
            self.au.pop_front();
        }
        if let (Some(a), Some(b)) = (self.au.front(), self.au.back()) {
            let ok = b.1.saturating_sub(a.1);
            let err = b.2.saturating_sub(a.2);
            self.au_rate = if ok + err > 0 { err as f32 / (ok + err) as f32 } else { 0.0 };
            // (statistics over 1.5 s at least, they come every second: the
            // decoder's first ones come before any audio unit)
            let no_audio = ok + err == 0 && b.0.duration_since(a.0) >= Duration::from_millis(1500) && s.frames > 0;
            if no_audio != self.no_audio {
                self.no_audio = no_audio;
                if no_audio {
                    self.event("DAB without audio: not one audio unit in the sub-channel (the broadcaster's encoder?)");
                } else {
                    self.event("DAB audio again");
                }
            }
        }
    }

    pub fn on_signal(&mut self, synced: bool, snr: f32) {
        self.synced = synced;
        self.snr = snr;
    }

    /// The FM tuner is used by hand: no automatic tuning for a while.
    pub fn suspend(&mut self, d: Duration) {
        self.suspended_until = Some(Instant::now() + d);
    }

    fn dab_good(&self, now: Instant) -> bool {
        self.synced
            && !self.no_audio
            && self.au_rate < 0.01
            && self.last_stats.is_some_and(|t| now.duration_since(t) < Duration::from_millis(1500))
    }

    fn dab_bad(&self, now: Instant) -> bool {
        !self.synced
            || self.no_audio
            || self.au_rate > 0.1
            || self.last_stats.is_none_or(|t| now.duration_since(t) > Duration::from_millis(1500))
    }

    fn pi_matches(&self, pi: u16) -> bool {
        self.targets.iter().any(|t| t.0 == pi)
    }

    /// One step (about every 200 ms): search, evaluate, decide, measure.
    pub fn step(&mut self, now: Instant, fm: &FmView, fm_in_use: bool, linker: Option<&mut Linker>) -> Vec<Action> {
        if self.dab_good(now) {
            self.good_since.get_or_insert(now);
            self.bad_since = None;
        } else {
            self.good_since = None;
            if self.dab_bad(now) {
                self.bad_since.get_or_insert(now);
            }
        }
        if fm.scan_id != self.seen_scan_id {
            self.seen_scan_id = fm.scan_id;
            self.peaks = peaks(&fm.scan);
            self.peaks_at = Some(now);
        }
        let Some(linker) = linker else {
            self.phase = Phase::Idle;
            self.reason = if !self.enabled {
                "linking switched off".into()
            } else if !fm.connected {
                "no FM tuner".into()
            } else {
                "no service".into()
            };
            return Vec::new();
        };
        for e in std::mem::take(&mut linker.events) {
            self.event(e);
        }
        if self.targets.is_empty() {
            self.phase = Phase::Idle;
            self.reason = "no RDS PI linked to this service".into();
            linker.set_fm_valid(false);
            return Vec::new();
        }
        // the programme on the frequency tuned (outside of a measurement)
        if let Some((pi, 0)) = fm.pi.filter(|_| fm.connected && fm.khz > 0) {
            if matches!(self.phase, Phase::Idle | Phase::Locked { .. } | Phase::Failed { .. }) {
                self.heard(fm.khz, Some(pi), fm.level);
            }
        }
        let suspended = if fm_in_use {
            Some("FM is listened to directly (FM tab): no automatic tuning")
        } else if self.suspended_until.is_some_and(|t| now < t) {
            Some("FM tuned by hand: automatic search paused for 2 minutes")
        } else {
            None
        };
        let heard_fm = linker.source() == Source::Fm;
        let actions = self.search(now, fm, suspended, heard_fm, linker);
        if suspended.is_none() && self.reason.starts_with("FM ") {
            self.reason.clear();
        }
        // correlation every 2 s while the station is tuned
        if matches!(self.phase, Phase::Locked { .. }) && now.duration_since(self.last_measure) > Duration::from_secs(2) {
            self.last_measure = now;
            if let Some(m) = linker.measure(now) {
                self.history.push_back((now_ms(), m.delay_ms, m.confidence, m.accepted));
                while self.history.len() > 300 {
                    self.history.pop_front();
                }
                self.last = Some(m);
            }
        }
        self.decide(now, linker);
        actions
    }

    fn search(&mut self, now: Instant, fm: &FmView, suspended: Option<&'static str>, heard_fm: bool, linker: &mut Linker) -> Vec<Action> {
        // the tuner is on a station of the service already (e.g. tuned by hand)
        if matches!(self.phase, Phase::Idle | Phase::Failed { .. }) {
            if let Some((pi, 0)) = fm.pi.filter(|p| fm.connected && fm.khz > 0 && self.pi_matches(p.0)) {
                if suspended.is_some() || self.candidates.len() <= 1 {
                    self.add_candidate(fm.khz, "tuned");
                    self.lock(fm.khz, fm.antenna, fm.level, pi, now, linker);
                    self.reason.clear();
                    return Vec::new();
                }
            }
        }
        if let Some(why) = suspended {
            if matches!(self.phase, Phase::Idle | Phase::Failed { .. } | Phase::Probing { .. } | Phase::Evaluating { .. }) {
                if !matches!(self.phase, Phase::Idle | Phase::Failed { .. }) {
                    self.phase = Phase::Idle;
                }
                self.reason = why.into();
                return Vec::new();
            }
        }
        match self.phase {
            Phase::Idle => {
                self.reason.clear();
                self.plan(now, fm)
            }
            Phase::Failed { since } => {
                if now.duration_since(since) > Duration::from_secs(300) {
                    self.scanned = None;
                    self.phase = Phase::Idle;
                }
                Vec::new()
            }
            Phase::Scanning { since, scan_id } => {
                if scan_id == u64::MAX {
                    self.phase = Phase::Scanning { since, scan_id: fm.scan_id };
                    return Vec::new();
                }
                if fm.scan_id > scan_id {
                    self.event(format!("band scan: {} stations", self.peaks.len()));
                    return self.plan(now, fm);
                }
                if now.duration_since(since) > Duration::from_secs(40) {
                    self.event("band scan did not finish");
                    self.phase = Phase::Failed { since: now };
                }
                Vec::new()
            }
            Phase::Probing { .. } => self.probe(now, fm),
            Phase::Evaluating { .. } => self.evaluate(now, fm, linker),
            Phase::Locked { khz, antenna, since, level, wrong_pi, weak, moved } => {
                // tuned elsewhere (by hand or an antenna switched)
                let away = fm.khz != khz || fm.antenna != antenna;
                let moved = if away { Some(moved.unwrap_or(now)) } else { None };
                if moved.is_some_and(|t| now.duration_since(t) > Duration::from_secs(3)) {
                    linker.set_fm_valid(false);
                    self.phase = Phase::Idle;
                    return Vec::new();
                }
                for f in fm.af.clone() {
                    self.add_candidate(f, "RDS AF");
                }
                let wrong = !away && matches!(fm.pi, Some((pi, 0)) if !self.pi_matches(pi));
                let wrong_pi = if wrong { Some(wrong_pi.unwrap_or(now)) } else { None };
                let weak = if !away && fm.level < level - 10.0 { Some(weak.unwrap_or(now)) } else { None };
                self.phase = Phase::Locked { khz, antenna, since, level, wrong_pi, weak, moved };
                if wrong_pi.is_some_and(|t| now.duration_since(t) > Duration::from_secs(3)) {
                    self.event(format!("{}: another PI now, searching again", mhz(khz)));
                    linker.set_fm_valid(false);
                    self.phase = Phase::Idle;
                } else if !heard_fm && suspended.is_none() {
                    // while DAB is heard: look for something better
                    if weak.is_some_and(|t| now.duration_since(t) > Duration::from_secs(10)) {
                        self.event(format!("{} {:.0} dB weaker, evaluating again", mhz(khz), level - fm.level));
                        linker.set_fm_valid(false);
                        self.phase = Phase::Idle;
                    } else if now.duration_since(since) > REEVALUATE {
                        self.event("periodic evaluation of frequencies and antennas");
                        linker.set_fm_valid(false);
                        self.phase = Phase::Idle;
                    }
                }
                Vec::new()
            }
        }
    }

    /// Evaluate the candidates of the service; without any, find out the
    /// programmes of the scanned stations first (or scan).
    fn plan(&mut self, now: Instant, fm: &FmView) -> Vec<Action> {
        let mut freqs: Vec<(u32, i32)> = self
            .candidates
            .iter()
            .map(|c| {
                let rank = ["verified before", "FIG 0/21", "station list", "RDS AF", "tuned"]
                    .iter()
                    .position(|s| c.sources.contains(s))
                    .unwrap_or(9) as i32;
                (c.khz, rank)
            })
            .collect();
        freqs.sort_by_key(|f| f.1);
        let unix = now_ms() / 1000;
        // another programme heard there lately: no need to measure
        freqs.retain(|(f, _)| {
            !self.stations.get(f).is_some_and(|s| unix - s.2 <= STATION_FRESH_S && s.0.is_some_and(|p| !self.pi_matches(p)))
        });
        if !freqs.is_empty() {
            let antennas = fm.antennas.clamp(1, 4);
            let plan: VecDeque<(u32, u8)> = freqs.iter().flat_map(|&(f, _)| (0..antennas).map(move |a| (f, a))).collect();
            let total = plan.len();
            for c in self.candidates.iter_mut() {
                c.best = false;
            }
            self.event(format!("evaluating {} frequencies on {antennas} antenna(s)", freqs.len()));
            self.phase = Phase::Evaluating { plan, total, cur: None, samples: Samples::default() };
            return Vec::new();
        }
        let unknown: VecDeque<u32> =
            self.peaks.iter().map(|p| p.0).filter(|f| self.stations.get(f).is_none_or(|s| unix - s.2 > STATION_FRESH_S)).take(40).collect();
        if !unknown.is_empty() {
            self.event(format!("finding out the programme of {} scanned stations", unknown.len()));
            self.phase = Phase::Probing { queue: unknown, cur: None };
            return Vec::new();
        }
        let scan_old = self.peaks_at.is_none_or(|t| now.duration_since(t) > Duration::from_secs(600));
        if scan_old && self.scanned.is_none_or(|t| now.duration_since(t) > Duration::from_secs(600)) {
            self.scanned = Some(now);
            self.phase = Phase::Scanning { since: now, scan_id: u64::MAX };
            self.event("no frequency of the service known, scanning the FM band");
            return vec![Action::Scan];
        }
        self.event(format!("no FM station with PI {} found", self.targets_text()));
        self.phase = Phase::Failed { since: now };
        self.reason = "not found".into();
        Vec::new()
    }

    /// The PI of the next scanned station.
    fn probe(&mut self, now: Instant, fm: &FmView) -> Vec<Action> {
        let Phase::Probing { queue, cur } = &mut self.phase else { return Vec::new() };
        let Some((khz, since, tuned)) = *cur else {
            return match queue.pop_front() {
                Some(khz) => {
                    *cur = Some((khz, now, None));
                    vec![Action::Tune(khz)]
                }
                None => self.plan_after_probe(now, fm),
            };
        };
        let tuned = match tuned {
            Some(t) => t,
            None if fm.khz == khz => {
                *cur = Some((khz, since, Some(now)));
                return Vec::new();
            }
            None if now.duration_since(since) > Duration::from_secs(3) => {
                *cur = None;
                return Vec::new();
            }
            None => return Vec::new(),
        };
        let waited = now.duration_since(tuned);
        let pi = match fm.pi {
            Some((pi, 0)) if waited >= Duration::from_millis(300) => Some(pi),
            _ if waited > Duration::from_millis(2500) => None,
            _ => return Vec::new(),
        };
        *cur = None;
        let level = fm.level;
        self.heard(khz, pi, level);
        if pi.is_some_and(|p| self.pi_matches(p)) {
            self.add_candidate(khz, "station list");
            self.event(format!("{}: PI {} found ({level:.0} dBf)", mhz(khz), self.targets_text()));
        }
        Vec::new()
    }

    fn plan_after_probe(&mut self, now: Instant, fm: &FmView) -> Vec<Action> {
        let found = self.candidates.iter().any(|c| c.sources.contains(&"station list"));
        if found {
            self.phase = Phase::Idle;
            return self.plan(now, fm);
        }
        self.event(format!("no FM station with PI {} among the scanned stations", self.targets_text()));
        self.phase = Phase::Failed { since: now };
        self.reason = "not found".into();
        Vec::new()
    }

    /// Measure the next frequency/antenna; when all are done take the best.
    fn evaluate(&mut self, now: Instant, fm: &FmView, linker: &mut Linker) -> Vec<Action> {
        let Phase::Evaluating { plan, cur, samples, .. } = &mut self.phase else { return Vec::new() };
        let Some((khz, antenna, since, settled)) = *cur else {
            return match plan.pop_front() {
                Some((khz, antenna)) => {
                    *cur = Some((khz, antenna, now, None));
                    *samples = Samples::default();
                    let mut a = Vec::new();
                    if fm.antenna != antenna {
                        a.push(Action::Antenna(antenna));
                    }
                    if fm.khz != khz {
                        a.push(Action::Tune(khz));
                    }
                    a
                }
                None => self.choose(now, linker),
            };
        };
        let settled = match settled {
            Some(t) => t,
            None if fm.khz == khz && fm.antenna == antenna => {
                *cur = Some((khz, antenna, since, Some(now)));
                return Vec::new();
            }
            None if now.duration_since(since) > Duration::from_secs(3) => {
                *cur = None;
                return Vec::new();
            }
            None => return Vec::new(),
        };
        let waited = now.duration_since(settled);
        if waited < Duration::from_millis(300) {
            return Vec::new();
        }
        samples.level.push(fm.level);
        samples.cci.push(fm.cci.max(0) as f32);
        // (-1: the tuner does not report it)
        samples.aci.push(fm.aci.max(0) as f32);
        if let Some((pi, 0)) = fm.pi {
            samples.pi = Some(pi);
        }
        // enough samples, and a PI or long enough waited for one
        if samples.level.len() < 4 || (samples.pi.is_none() && waited < Duration::from_millis(2500)) {
            return Vec::new();
        }
        let (level, cci, aci) = (mean(&samples.level), mean(&samples.cci), mean(&samples.aci));
        let pi = samples.pi;
        *cur = None;
        let ok = pi.is_some_and(|p| self.pi_matches(p));
        // Quality first: above about 50 dBf more level gives the same audio,
        // while multipath and co-channel (CCI) and adjacent-channel (ACI)
        // interference are heard; a clean antenna beats a stronger noisy one.
        let score = ok.then(|| level.min(50.0) - 0.5 * cci - 0.5 * aci);
        if pi.is_some() {
            self.heard(khz, pi, level);
        }
        let m = Measure { antenna, level, cci, aci, pi: pi.map(hex), score };
        if let Some(c) = self.candidates.iter_mut().find(|c| c.khz == khz) {
            let check = c.check.get_or_insert(Check { at: now, result: "", measures: Vec::new() });
            if now.duration_since(check.at) > Duration::from_secs(30) {
                check.measures.clear();
            }
            check.at = now;
            check.measures.retain(|x| x.antenna != antenna);
            check.measures.push(m);
            check.measures.sort_by_key(|x| x.antenna);
            check.result = if check.measures.iter().any(|x| x.score.is_some()) {
                "verified"
            } else if check.measures.iter().any(|x| x.pi.is_some()) {
                "other programme"
            } else if level < 15.0 {
                "too weak"
            } else {
                "no RDS"
            };
        }
        Vec::new()
    }

    /// The best frequency and antenna of the evaluation.
    fn choose(&mut self, now: Instant, linker: &mut Linker) -> Vec<Action> {
        let best = self
            .candidates
            .iter()
            .filter_map(|c| c.check.as_ref().map(|k| (c.khz, k)))
            .flat_map(|(khz, k)| k.measures.iter().filter_map(move |m| m.score.map(|s| (khz, m.antenna, m.level, s, m.pi.clone()))))
            .max_by(|a, b| a.3.total_cmp(&b.3));
        let Some((khz, antenna, level, score, pi)) = best else {
            self.event(format!("no candidate carries PI {}", self.targets_text()));
            self.phase = Phase::Failed { since: now };
            self.reason = "not found".into();
            return Vec::new();
        };
        for c in self.candidates.iter_mut() {
            c.best = c.khz == khz;
        }
        let pi = pi.and_then(|p| u16::from_str_radix(&p, 16).ok()).unwrap_or(0);
        self.event(format!("best: {} on antenna {} ({level:.0} dBf, score {score:.0})", mhz(khz), antenna + 1));
        self.lock(khz, antenna, level, pi, now, linker);
        vec![Action::Antenna(antenna), Action::Tune(khz)]
    }

    /// The programme heard on a frequency (none: no RDS).
    fn heard(&mut self, khz: u32, pi: Option<u16>, level: f32) {
        let unix = now_ms() / 1000;
        let e = self.stations.entry(khz).or_insert((pi, level, 0));
        if e.0 != pi || unix - e.2 > 600 {
            *e = (pi, level, unix);
            self.dirty = true;
        }
    }

    fn lock(&mut self, khz: u32, antenna: u8, level: f32, pi: u16, now: Instant, linker: &mut Linker) {
        self.phase = Phase::Locked { khz, antenna, since: now, level, wrong_pi: None, weak: None, moved: None };
        linker.set_fm_valid(true);
        let unix = now_ms() / 1000;
        let list = self.known.entry(hex(pi)).or_default();
        list.retain(|k| k.0 != khz);
        list.push((khz, antenna, level, unix));
        self.dirty = true;
    }

    fn decide(&mut self, now: Instant, linker: &mut Linker) {
        let aligned = linker.aligned();
        // the linker may have gone to FM by itself (DAB stalled)
        let src = linker.target();
        // DAB without any audio: FM plays without being aligned (there is
        // nothing to align it with) until DAB has audio and is aligned again
        let fm_only = self.no_audio && linker.fm_valid();
        let want = match self.mode {
            Mode::Dab => Source::Dab,
            Mode::Fm if aligned || fm_only => Source::Fm,
            Mode::Fm => Source::Dab,
            Mode::Auto if fm_only => {
                if src == Source::Dab {
                    self.event("DAB has no audio, FM plays (not aligned)");
                }
                Source::Fm
            }
            Mode::Auto => {
                if src == Source::Dab && aligned && self.bad_since.is_some_and(|t| now.duration_since(t) > Duration::from_millis(500)) {
                    self.event(format!(
                        "DAB bad ({}{:.0}% AU errors), FM takes over",
                        if self.synced { "" } else { "no sync, " },
                        self.au_rate * 100.0
                    ));
                    Source::Fm
                } else if src == Source::Fm && aligned && self.good_since.is_some_and(|t| now.duration_since(t) > Duration::from_secs(8)) {
                    self.event("DAB good again for 8 s, back to DAB");
                    Source::Dab
                } else {
                    src
                }
            }
        };
        linker.allow_auto = self.mode == Mode::Auto;
        linker.want(want);
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        for (path, data) in [(&self.known_path, serde_json::to_vec(&self.known)), (&self.stations_path, serde_json::to_vec(&self.stations))]
        {
            if let Ok(d) = data {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, d).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
    }

    /// Short state for the listen view of every listener (sent on changes).
    pub fn summary(&self, fm: &FmView, linker: Option<&Linker>) -> Value {
        let Some(key) = self.service else { return json!({"type": "linkInfo", "sid": null, "enabled": self.enabled}) };
        // delay: median of the last accepted measurements (stable to 0.1 s)
        let mut d: Vec<f32> = self.history.iter().rev().filter(|h| h.3).take(7).map(|h| h.1).collect();
        d.sort_by(|a, b| a.total_cmp(b));
        let delay = d.get(d.len() / 2).map(|v| (v / 100.0).round() / 10.0);
        let (name, khz, antenna, progress) = match &self.phase {
            Phase::Idle => ("idle", None, None, None),
            Phase::Scanning { .. } => ("scanning", None, None, None),
            Phase::Probing { queue, cur } => ("probing", cur.map(|c| c.0), None, Some(json!({"left": queue.len()}))),
            Phase::Evaluating { plan, total, cur, .. } => {
                ("evaluating", cur.map(|c| c.0), cur.map(|c| c.1), Some(json!({"done": total - plan.len(), "total": total})))
            }
            Phase::Locked { khz, antenna, .. } => ("locked", Some(*khz), Some(*antenna), None),
            Phase::Failed { .. } => ("failed", None, None, None),
        };
        json!({
            "type": "linkInfo",
            "sid": key.sid,
            "enabled": self.enabled,
            "mode": self.mode,
            "phase": name,
            "khz": khz,
            "antenna": antenna,
            "antennas": fm.antennas,
            "progress": progress,
            "targets": self.targets.iter().map(|t| hex(t.0)).collect::<Vec<_>>(),
            "aligned": linker.is_some_and(|l| l.aligned()),
            "source": linker.map(|l| l.source()),
            "delay": delay,
            "reason": self.reason,
        })
    }

    pub fn json(&self, fm: &FmView, linker: Option<&Linker>) -> Value {
        let now = Instant::now();
        let secs = |t: &Instant| now.duration_since(*t).as_secs();
        let phase = match &self.phase {
            Phase::Idle => json!({"name": "idle"}),
            Phase::Scanning { since, .. } => json!({"name": "scanning", "for": secs(since)}),
            Phase::Probing { queue, cur } => json!({"name": "probing", "khz": cur.map(|c| c.0), "left": queue.len()}),
            Phase::Evaluating { plan, total, cur, .. } => {
                json!({"name": "evaluating", "khz": cur.map(|c| c.0), "antenna": cur.map(|c| c.1), "done": total - plan.len(), "total": total})
            }
            Phase::Locked { khz, antenna, since, .. } => json!({"name": "locked", "khz": khz, "antenna": antenna, "for": secs(since)}),
            Phase::Failed { since } => json!({"name": "failed", "for": secs(since)}),
        };
        let candidates: Vec<Value> = self
            .candidates
            .iter()
            .map(|c| {
                let mut v = serde_json::to_value(c).unwrap_or_default();
                if let Some(k) = &c.check {
                    v["check"]["ago"] = json!(secs(&k.at));
                }
                v
            })
            .collect();
        let unix = now_ms() / 1000;
        let mut stations: Vec<Value> = self
            .stations
            .iter()
            .map(|(k, v)| json!({"khz": k, "pi": v.0.map(hex), "level": v.1, "ago": unix - v.2, "match": v.0.is_some_and(|p| self.pi_matches(p))}))
            .collect();
        stations.sort_by_key(|s| s["khz"].as_u64());
        json!({
            "type": "linking",
            "enabled": self.enabled,
            "mode": self.mode,
            "reason": self.reason,
            "service": self.service.map(|k| json!({"sid": format!("{:04X}", k.sid), "label": self.label, "ecc": self.ecc.map(|e| format!("{e:02X}"))})),
            "targets": self.targets.iter().map(|t| json!({"pi": hex(t.0), "source": t.1})).collect::<Vec<_>>(),
            "phase": phase,
            "candidates": candidates,
            "stations": stations,
            "fm": {"connected": fm.connected, "khz": fm.khz, "antenna": fm.antenna, "antennas": fm.antennas, "level": fm.level, "cci": fm.cci, "aci": fm.aci, "pi": fm.pi.map(|p| hex(p.0)), "piErrors": fm.pi.map(|p| p.1), "ps": fm.ps},
            "dab": {"synced": self.synced, "snr": self.snr, "auErrorRate": self.au_rate, "good": self.good_since.is_some(), "bad": self.bad_since.is_some(), "noAudio": self.no_audio},
            "linker": linker.map(|l| l.status()),
            "measurement": self.last,
            "history": self.history,
            "events": self.events.iter().rev().take(80).collect::<Vec<_>>(),
        })
    }
}
