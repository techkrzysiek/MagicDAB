//! Gain optimiser for 8 bit tuners (RTL-SDR, rtl_tcp).
//!
//! The device's software AGC keeps the ADC level safe below a gain ceiling;
//! this module moves the ceiling by the signal quality, not by the level:
//! too much gain overloads the tuner in areas with strong transmitters (the
//! products of the overload fill the channel and look like signal to a level
//! based AGC), too little lets the tuner noise dominate.
//!
//! Without (good) reception the gain range is swept in big steps, measured
//! by the SNR seen in the depth of the null symbol, which needs no
//! synchronisation: a weak channel next to strong ones may only appear at a
//! much lower gain. With reception the ceiling is refined by hill climbing on
//! the demodulator SNR. The best ceiling is remembered per channel.
//!
//! The AGC climbs to a higher ceiling only as far as the ADC level allows.
//! A ceiling above the gain it can use (a strong signal) comes down to that
//! gain, and a probe the gain could not get to counts as a worse one: else
//! the optimiser keeps probing a ceiling that is not the gain, and every
//! probe up throws the gain into the overload for the AGC to walk it back.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_millis(1500);
const MEASURE: Duration = Duration::from_secs(3);
const MIN_INTERVAL: Duration = Duration::from_secs(5);
const MAX_INTERVAL: Duration = Duration::from_secs(240);
/// SNR improvement needed to accept a probe (dB): more than the SNR of a
/// steady signal drifts between two measurements.
const ACCEPT_DB: f32 = 0.5;
/// Size of a probe step (dB).
const STEP_DB: f32 = 3.0;
/// Spacing of the levels of a sweep (dB).
const SWEEP_STEP_DB: f32 = 7.0;
/// Time a sweep level is given to take effect, and to be measured.
const SWEEP_SETTLE: Duration = Duration::from_millis(400);
const SWEEP_MEASURE: Duration = Duration::from_millis(700);
/// Without good reception for this long the gain range is swept.
const SWEEP_AFTER: Duration = Duration::from_millis(2500);
/// A sweep that found no reception is repeated after this long.
const SWEEP_AGAIN: Duration = Duration::from_secs(30);
/// SNR below which a synchronised channel is swept too, once per tuning (dB).
const POOR_SNR: f32 = 8.0;
/// SNR from which the gain is left alone (dB): DAB+ is decoded without
/// errors long before, and every change of the gain of an R828D upsets the
/// demodulator for a moment (at 28.7 dB a probe cost 0.7 s of audio for an
/// SNR of 33.3 dB nobody hears).
const GOOD_SNR: f32 = 15.0;
/// SNR of a sweep level that counts as reception (dB).
const FOUND_SNR: f32 = 1.0;
/// A sweep stops going further once a level is this much worse than the best (dB).
const WORSE_DB: f32 = 3.0;

#[derive(Clone, Copy)]
enum Phase {
    /// Waiting before measuring the current ceiling.
    Settle(Instant),
    /// Measuring the SNR at the current ceiling, and the lowest and highest
    /// gain the device used meanwhile.
    Base { until: Instant, sum: f32, n: u32, lo: usize, hi: usize },
    /// Probe ceiling applied, waiting for the receiver to settle.
    ProbeSettle { until: Instant, base: f32 },
    /// Measuring the SNR at the probe ceiling; `off` once the gain was not the probe's.
    Probe { until: Instant, base: f32, sum: f32, n: u32, off: bool },
    /// Idle until the next probe.
    Wait(Instant),
}

/// A sweep over the gain range: the starting level, then the ones below it,
/// then (if going down did not help) the ones above it.
struct Sweep {
    start: usize,
    down: Vec<usize>,
    up: Vec<usize>,
    /// Level being measured and whether it belongs to `up`.
    level: usize,
    upward: bool,
    measure_from: Instant,
    until: Instant,
    best: Option<(f32, usize)>,
    max: f32,
    reports: u32,
}

/// A gain ceiling for the device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ceiling {
    pub index: usize,
    /// The gain goes straight to it (a sweep level, back from a probe); else
    /// the AGC climbs to it as far as the ADC level allows.
    pub jump: bool,
}

pub struct GainSeeker {
    gains: Vec<f32>,
    channel: Option<String>,
    ceiling: usize,
    probe: usize,
    dir: i32,
    fails: u32,
    interval: Duration,
    phase: Phase,
    sweep: Option<Sweep>,
    /// Since when the reception is missing or poor.
    trouble_since: Option<Instant>,
    /// Last sweep of the channel and whether one ran since tuning.
    last_sweep: Option<Instant>,
    swept: bool,
    /// Best ceiling per channel in dB (the gain tables of devices differ).
    learned: HashMap<String, f32>,
    pub learned_dirty: bool,
}

impl GainSeeker {
    pub fn new() -> Self {
        GainSeeker {
            gains: Vec::new(),
            channel: None,
            ceiling: 0,
            probe: 0,
            dir: -1,
            fails: 0,
            interval: MIN_INTERVAL,
            phase: Phase::Settle(Instant::now()),
            sweep: None,
            trouble_since: None,
            last_sweep: None,
            swept: false,
            learned: HashMap::new(),
            learned_dirty: false,
        }
    }

    /// With the ceilings learned before (`gains.json` of the data folder).
    pub fn load(path: &Path) -> Self {
        let mut g = Self::new();
        if let Ok(data) = std::fs::read(path) {
            g.learned = serde_json::from_slice(&data).unwrap_or_default();
        }
        g
    }

    pub fn save(&mut self, path: &Path) {
        self.learned_dirty = false;
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.learned).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Index of the gain closest to `db`.
    fn nearest(&self, db: f32) -> usize {
        self.gains.iter().enumerate().min_by(|a, b| (a.1 - db).abs().total_cmp(&(b.1 - db).abs())).map(|(i, _)| i).unwrap_or(0)
    }

    /// Index of the gain closest to 40 dB.
    fn default_index(&self) -> usize {
        self.nearest(40.0)
    }

    fn learn(&mut self) {
        if let Some(ch) = &self.channel {
            let db = self.gains[self.ceiling];
            if self.learned.insert(ch.clone(), db) != Some(db) {
                self.learned_dirty = true;
            }
        }
    }

    /// Remember a gain found to receive `channel` (band scan).
    pub fn learn_db(&mut self, channel: &str, db: f32) {
        if self.learned.insert(channel.to_string(), db) != Some(db) {
            self.learned_dirty = true;
        }
    }

    /// Ceiling to use for `channel` (learned or default); restarts the search.
    pub fn retune(&mut self, channel: &str, gains: &[f32]) -> Option<usize> {
        if gains.is_empty() {
            return None;
        }
        self.gains = gains.to_vec();
        self.channel = Some(channel.to_string());
        self.ceiling = match self.learned.get(channel) {
            Some(&db) => self.nearest(db),
            None => self.default_index(),
        }
        .min(gains.len() - 1);
        self.fails = 0;
        self.interval = MIN_INTERVAL;
        self.phase = Phase::Settle(Instant::now() + SETTLE);
        self.sweep = None;
        self.trouble_since = None;
        self.last_sweep = None;
        self.swept = false;
        Some(self.ceiling)
    }

    /// Ceiling about `STEP_DB` away from the current one in direction `dir`.
    fn step(&self, dir: i32) -> Option<usize> {
        let cur = *self.gains.get(self.ceiling)?;
        let target = cur + dir as f32 * STEP_DB;
        let candidates: Vec<usize> = if dir < 0 { (0..self.ceiling).collect() } else { (self.ceiling + 1..self.gains.len()).collect() };
        // of two gains about as far from the target the nearer one (8.7 dB: 7.7, not 3.7)
        let off = |i: usize| ((self.gains[i] - target).abs() * 10.0).round();
        candidates
            .into_iter()
            .min_by(|&a, &b| off(a).total_cmp(&off(b)).then((self.gains[a] - cur).abs().total_cmp(&(self.gains[b] - cur).abs())))
    }

    /// Feed a signal report (about 4 per second): synchronisation, demodulator
    /// SNR, the SNR seen in the null symbol and the gain the device uses (its
    /// table index); returns a ceiling to apply.
    pub fn on_signal(&mut self, synced: bool, snr: f32, null_snr: f32, gain: Option<usize>, now: Instant) -> Option<Ceiling> {
        if self.gains.is_empty() || self.channel.is_none() {
            return None;
        }
        if self.sweep.is_some() {
            return self.sweep_step(if synced { snr } else { null_snr }, now);
        }
        if !synced || snr < POOR_SNR {
            let since = *self.trouble_since.get_or_insert(now);
            let again = self.last_sweep.is_none_or(|t| now.duration_since(t) >= SWEEP_AGAIN);
            if now.duration_since(since) >= SWEEP_AFTER && again && (!synced || !self.swept) {
                return self.start_sweep(now, synced, snr);
            }
        } else {
            self.trouble_since = None;
        }
        if !synced {
            let probing = matches!(self.phase, Phase::ProbeSettle { .. } | Phase::Probe { .. });
            self.phase = Phase::Settle(now + SETTLE);
            if probing {
                // the probe lost the signal: go back and try the other way next time
                self.dir = -self.dir;
                return Some(Ceiling { index: self.ceiling, jump: true });
            }
            return None;
        }
        match self.phase {
            Phase::Settle(t) | Phase::Wait(t) => {
                if now >= t {
                    let g = gain.unwrap_or(self.ceiling);
                    self.phase = Phase::Base { until: now + MEASURE, sum: 0.0, n: 0, lo: g, hi: g };
                }
            }
            Phase::Base { until, sum, n, lo, hi } => {
                let g = gain.unwrap_or(self.ceiling);
                let (sum, n, lo, hi) = (sum + snr, n + 1, lo.min(g), hi.max(g));
                if now < until || n < 6 {
                    self.phase = Phase::Base { until, sum, n, lo, hi };
                    return None;
                }
                let lowered = hi < self.ceiling;
                if lowered {
                    // the ADC level allows the AGC no more (a strong signal)
                    log::info!(
                        "gain: ceiling {:.1} dB -> {:.1} dB, the most the signal level allows",
                        self.gains[self.ceiling],
                        self.gains[hi]
                    );
                    self.ceiling = hi;
                    self.learn();
                }
                let base = sum / n as f32;
                if lo != hi {
                    // the AGC moved the gain meanwhile: measure again
                    self.phase = Phase::Base { until: now + MEASURE, sum: 0.0, n: 0, lo: g, hi: g };
                } else if base >= GOOD_SNR {
                    // good reception: nothing to win, a probe would be heard
                    self.learn();
                    self.phase = Phase::Wait(now + MAX_INTERVAL);
                } else if let Some(c) = self.start_probe(base, now) {
                    return Some(c);
                }
                if lowered {
                    return Some(Ceiling { index: self.ceiling, jump: false });
                }
            }
            Phase::ProbeSettle { until, base } => {
                if now >= until {
                    if gain.is_some_and(|g| g != self.probe) {
                        // the AGC could not take the gain there
                        return self.probe_failed(now);
                    }
                    self.phase = Phase::Probe { until: now + MEASURE, base, sum: 0.0, n: 0, off: false };
                }
            }
            Phase::Probe { until, base, sum, n, off } => {
                let (sum, n) = (sum + snr, n + 1);
                let off = off || gain.is_some_and(|g| g != self.probe);
                if now < until || n < 6 {
                    self.phase = Phase::Probe { until, base, sum, n, off };
                    return None;
                }
                let s = sum / n as f32;
                if !off && s > base + ACCEPT_DB {
                    log::info!(
                        "gain: {:.1} dB -> {:.1} dB (SNR {:.1} -> {:.1} dB)",
                        self.gains[self.ceiling],
                        self.gains[self.probe],
                        base,
                        s
                    );
                    self.ceiling = self.probe;
                    self.learn();
                    self.fails = 0;
                    self.interval = MIN_INTERVAL;
                    // keep going in the same direction
                    return self.start_probe(s, now);
                }
                return self.probe_failed(now);
            }
        }
        None
    }

    /// The probe was no better, or the gain did not get there: back to the
    /// ceiling (the gain used before it) and the other way next time.
    fn probe_failed(&mut self, now: Instant) -> Option<Ceiling> {
        self.dir = -self.dir;
        self.fails += 1;
        let wait = if self.fails >= 2 {
            // both directions are worse: we are at the optimum
            self.fails = 0;
            self.learn();
            let w = self.interval;
            self.interval = (self.interval * 2).min(MAX_INTERVAL);
            w
        } else {
            // the gain back and the SNR settled before the next measurement
            SETTLE
        };
        self.phase = Phase::Wait(now + wait);
        Some(Ceiling { index: self.ceiling, jump: true })
    }

    fn start_probe(&mut self, base: f32, now: Instant) -> Option<Ceiling> {
        let p = self.step(self.dir).or_else(|| {
            self.dir = -self.dir;
            self.step(self.dir)
        });
        match p {
            Some(p) => {
                self.probe = p;
                self.phase = Phase::ProbeSettle { until: now + SETTLE, base };
                Some(Ceiling { index: p, jump: false })
            }
            None => {
                self.phase = Phase::Wait(now + self.interval);
                None
            }
        }
    }

    /// Levels `SWEEP_STEP_DB` apart from `start` towards `dir`, ending at the
    /// end of the gain table.
    fn sweep_levels(&self, start: usize, dir: f32) -> Vec<usize> {
        let (lo, hi) = (self.gains[0], self.gains[self.gains.len() - 1]);
        let mut v: Vec<usize> = Vec::new();
        let mut db = self.gains[start];
        loop {
            db += dir * SWEEP_STEP_DB;
            let i = self.nearest(db.clamp(lo, hi));
            if i == start || v.last() == Some(&i) {
                break;
            }
            v.push(i);
            if db <= lo || db >= hi {
                break;
            }
        }
        v
    }

    fn start_sweep(&mut self, now: Instant, synced: bool, snr: f32) -> Option<Ceiling> {
        let start = self.ceiling;
        if synced {
            log::info!("gain: weak reception at {:.1} dB (SNR {snr:.1} dB), trying other gains", self.gains[start]);
        } else {
            log::info!("gain: no reception at {:.1} dB, trying other gains", self.gains[start]);
        }
        let mut down = self.sweep_levels(start, -1.0);
        down.reverse();
        let mut up = self.sweep_levels(start, 1.0);
        up.reverse();
        self.sweep = Some(Sweep {
            start,
            down,
            up,
            level: start,
            upward: false,
            measure_from: now,
            until: now + SWEEP_MEASURE,
            best: None,
            max: f32::MIN,
            reports: 0,
        });
        self.swept = true;
        self.last_sweep = Some(now);
        None
    }

    fn sweep_step(&mut self, quality: f32, now: Instant) -> Option<Ceiling> {
        let sw = self.sweep.as_mut()?;
        if now < sw.measure_from {
            return None;
        }
        sw.max = sw.max.max(quality);
        sw.reports += 1;
        if now < sw.until || sw.reports < 2 {
            return None;
        }
        log::debug!("gain sweep: {:.1} dB -> SNR {:.1} dB", self.gains[sw.level], sw.max);
        let worse = sw.best.is_some_and(|(b, _)| b >= FOUND_SNR && sw.max < b - WORSE_DB);
        if sw.best.is_none_or(|(b, _)| sw.max > b) {
            sw.best = Some((sw.max, sw.level));
        }
        let (best, best_level) = sw.best.unwrap();
        let next = if !sw.upward && !worse && !sw.down.is_empty() {
            sw.down.pop()
        } else if best_level == sw.start && !(sw.upward && worse) && !sw.up.is_empty() {
            // going down did not help: the levels above
            sw.upward = true;
            sw.up.pop()
        } else {
            None
        };
        if let Some(level) = next {
            sw.level = level;
            sw.measure_from = now + SWEEP_SETTLE;
            sw.until = sw.measure_from + SWEEP_MEASURE;
            sw.max = f32::MIN;
            sw.reports = 0;
            return Some(Ceiling { index: level, jump: true });
        }
        let start = sw.start;
        self.sweep = None;
        self.phase = Phase::Settle(now + SETTLE);
        self.trouble_since = None;
        if best >= FOUND_SNR {
            log::info!("gain: {:.1} dB -> {:.1} dB by a sweep (SNR {best:.1} dB)", self.gains[start], self.gains[best_level]);
            self.ceiling = best_level;
            self.learn();
        } else {
            log::info!("gain: no reception at any gain, back to {:.1} dB", self.gains[start]);
            self.ceiling = start;
        }
        Some(Ceiling { index: self.ceiling, jump: true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAINS: [f32; 29] = [
        0.0, 0.9, 1.4, 2.7, 3.7, 7.7, 8.7, 12.5, 14.4, 15.7, 16.6, 19.7, 20.7, 22.9, 25.4, 28.0, 29.7, 32.8, 33.8, 36.4, 37.2, 38.6, 40.2,
        42.1, 43.4, 43.9, 44.5, 48.0, 49.6,
    ];

    /// The device: its AGC climbs a step every half second up to the ceiling
    /// and `limit` (the most gain the signal level allows), and steps back
    /// down above `limit`; a jump goes straight to the ceiling.
    struct Device {
        gain: usize,
        ceiling: usize,
        limit: usize,
        ticks: u32,
        changes: u32,
        /// Reports with the gain above `limit` after the first 30 s.
        over: u32,
    }

    impl Device {
        fn apply(&mut self, c: Ceiling) {
            self.ceiling = c.index;
            if c.jump || self.gain > c.index {
                self.set(c.index);
            }
        }

        fn set(&mut self, gain: usize) {
            if gain != self.gain {
                self.gain = gain;
                self.changes += 1;
            }
        }

        /// One report period (0.25 s).
        fn agc(&mut self) {
            self.ticks += 1;
            if self.ticks > 120 && self.gain > self.limit {
                self.over += 1;
            }
            if self.ticks % 2 == 0 {
                if self.gain > self.limit {
                    self.set(self.gain - 1);
                } else if self.gain < self.ceiling.min(self.limit) {
                    self.set(self.gain + 1);
                }
            }
        }
    }

    /// Run the optimiser against a reception curve (gain -> SNR, `None`
    /// without synchronisation) on a device allowing gains up to `limit`.
    fn run_device(curve: impl Fn(f32) -> Option<f32>, channel: &str, seconds: u64, limit: usize) -> (GainSeeker, Device) {
        let mut g = GainSeeker::new();
        let start = g.retune(channel, &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        let mut noise = 0x1234u32;
        for k in 0..seconds * 4 {
            noise = noise.wrapping_mul(1103515245).wrapping_add(12345);
            let jitter = ((noise >> 16) % 100) as f32 / 100.0 * 0.4 - 0.2;
            let (synced, snr, null_snr) = match curve(GAINS[dev.gain]) {
                Some(s) => (true, s + jitter, s + jitter),
                None => (false, 0.0, -10.0 + jitter),
            };
            if let Some(c) = g.on_signal(synced, snr, null_snr, Some(dev.gain), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        (g, dev)
    }

    /// The final ceiling in dB, on a device with no limit of its own.
    fn run_curve(curve: impl Fn(f32) -> Option<f32>, start_channel: &str, seconds: u64) -> f32 {
        let (g, _) = run_device(curve, start_channel, seconds, GAINS.len() - 1);
        GAINS[g.ceiling]
    }

    fn run(curve: impl Fn(f32) -> f32, start_channel: &str, seconds: u64) -> f32 {
        run_curve(|db| Some(curve(db)), start_channel, seconds)
    }

    #[test]
    fn finds_optimum_below_default() {
        // overload above 30 dB
        let best = run(|db| 12.0 - ((db - 30.0) / 4.0).powi(2), "11B", 300);
        assert!((best - 30.0).abs() <= 3.5, "best {best}");
    }

    #[test]
    fn finds_optimum_above_default() {
        let best = run(|db| 6.0 - ((db - 48.0) / 5.0).powi(2), "9C", 300);
        assert!(best >= 44.0, "best {best}");
    }

    #[test]
    fn stays_on_flat_curve() {
        let best = run(|_| 8.0, "12D", 300);
        assert!((best - 40.2).abs() < 4.0, "best {best}");
    }

    #[test]
    fn finds_channel_hidden_by_overload() {
        // a channel next to a strong one (measured): nothing above 30 dB,
        // poor between 20 and 30 dB, SNR 11.5 dB around 8 to 12 dB
        let curve = |db: f32| match db {
            d if d > 30.0 => None,
            d if d > 18.0 => Some(2.5),
            d if d > 5.0 => Some(11.5 - (d - 10.0).abs() * 0.1),
            _ => Some(10.5),
        };
        let best = run_curve(curve, "5A", 120);
        assert!((5.0..=16.0).contains(&best), "best {best}");
    }

    #[test]
    fn empty_channel_returns_to_start() {
        let mut g = GainSeeker::new();
        let mut ceiling = g.retune("13F", &GAINS).unwrap();
        let start = ceiling;
        let t0 = Instant::now();
        let mut changes = 0;
        for k in 0..4 * 25 {
            if let Some(c) = g.on_signal(false, 0.0, -10.0, Some(ceiling), t0 + Duration::from_millis(250 * k)) {
                ceiling = c.index;
                changes += 1;
            }
        }
        assert_eq!(ceiling, start);
        // one sweep down to 0 dB and back within 25 s, no second one yet
        assert!((5..=10).contains(&changes), "{changes} changes");
    }

    #[test]
    fn a_good_signal_is_left_alone() {
        // 25 dB at the ceiling learned, 30 dB a few dB lower: no probe for it
        let curve = |db: f32| Some(if db < 20.0 { 30.0 } else { 25.0 });
        let mut g = GainSeeker::new();
        g.learn_db("5B", 25.4);
        let start = g.retune("5B", &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit: GAINS.len() - 1, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        for k in 0..600 * 4 {
            let snr = curve(GAINS[dev.gain]).unwrap();
            if let Some(c) = g.on_signal(true, snr, snr, Some(dev.gain), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        assert_eq!(dev.changes, 0, "the gain was changed {} times", dev.changes);
    }

    #[test]
    fn a_probe_takes_the_smaller_of_two_steps_as_far_from_three_db() {
        let mut g = GainSeeker::new();
        g.retune("11A", &GAINS);
        g.ceiling = 6;
        assert_eq!(GAINS[g.step(-1).unwrap()], 7.7);
        assert_eq!(GAINS[g.step(1).unwrap()], 12.5);
    }

    #[test]
    fn a_strong_signal_brings_the_ceiling_down_to_the_gain_used() {
        // a strong multiplex: the signal level allows the AGC 8.7 dB (index 6), the
        // SNR is worse above it; the probes up must not throw the gain there
        let curve = |db: f32| Some(if db <= 8.7 { 19.5 } else { 19.5 - (db - 8.7) * 0.35 });
        let (g, dev) = run_device(curve, "11A", 600, 6);
        assert!(g.ceiling <= 6, "ceiling {:.1} dB", GAINS[g.ceiling]);
        assert_eq!(dev.over, 0, "the gain went above what the level allows");
        // the walk down from the 40 dB of the start, then a few probes down
        assert!(dev.changes <= 30, "{} gain changes in ten minutes", dev.changes);
    }
}
