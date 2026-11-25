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
//!
//! Two gains are compared by what is decoded where that can be counted: the
//! bit errors of the FIC before its decoder. The SNR is measured against the
//! null symbol, so what distorts the signal only while it is there (a tuner
//! driven too hard by the multiplex itself, echoes longer than the guard
//! interval) is not in it: a gain may give more SNR and more errors. Without
//! errors to count the SNR decides.
//!
//! A ceiling far below the gain the level allows goes back up, in one step,
//! although the reception is good: whatever brought it down (a level since
//! gone, a burst) is not remembered for good, and at 8 bits a signal a few
//! steps of the ADC high has the ADC's resolution for its noise.

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
/// Bit error ratio of the FIC from which two gains are compared by it:
/// about a hundred errors in the second it is counted over.
const BER_COUNTED: f32 = 1e-3;
/// A gain is better by the bit errors with this share of them at most. The
/// ratio of one second moves by a quarter around its mean at a steady gain
/// (measured at an SNR of 10.6 dB: 6.0e-2 to 9.2e-2 around 7.4e-2).
const BER_BETTER: f32 = 0.7;
/// Bit error ratio from which the reception is not good whatever the SNR:
/// what an undistorted signal has some 4 dB below `GOOD_SNR`.
const GOOD_BER: f32 = 1e-2;
/// Room for more gain (dB, by the level: `magicdab_sdr::agc::room_db`) from
/// which a ceiling is raised although the reception is good. Measured, a
/// multiplex of a transmitter nearby at a ceiling of 0 dB learned before:
/// 3.6 steps of the ADC of signal over 0.5 of noise, an SNR of 17.5 dB with
/// 17.9 dB of room; 37.4 dB at 16.6 dB of gain.
const RISE_ROOM_DB: f32 = 10.0;
/// What a rise leaves of the room (dB): the gains of the table are nominal
/// (an R828D: 19.6 dB between its 0 and 16.6 dB), and the AGC steps down
/// 2 dB above the level it climbs to.
const RISE_MARGIN_DB: f32 = 4.0;
/// SNR from which the room is left unused (dB): nothing to win there.
const RISE_BELOW_SNR: f32 = 30.0;
/// A rise that was no better is not tried on the channel again for this long.
const RISE_AGAIN: Duration = Duration::from_secs(6 * 3600);
/// SNR of a sweep level that counts as reception (dB).
const FOUND_SNR: f32 = 1.0;
/// A sweep stops going further once a level is this much worse than the best (dB).
const WORSE_DB: f32 = 3.0;

/// A signal report for the optimiser (about 4 per second).
#[derive(Clone, Copy, Debug, Default)]
pub struct Report {
    pub synced: bool,
    /// Demodulator SNR (dB).
    pub snr: f32,
    /// SNR seen in the null symbol, also without synchronisation (dB).
    pub null_snr: f32,
    /// Bit error ratio of the FIC before its decoder, over the last second.
    pub ber: f32,
    /// Gain the device uses (its table index).
    pub gain: Option<usize>,
    /// Room the level leaves for more gain (dB), if known.
    pub room: Option<f32>,
}

/// What a gain was measured to give.
#[derive(Clone, Copy, Debug)]
struct Quality {
    /// Demodulator SNR (dB).
    snr: f32,
    /// Bit error ratio of the FIC before its decoder.
    ber: f32,
}

impl Quality {
    /// Better than `base` by more than a steady signal differs between two
    /// measurements: by the bit errors where there are enough to count,
    /// else by the SNR.
    fn better_than(&self, base: &Quality) -> bool {
        if self.ber.max(base.ber) >= BER_COUNTED {
            self.ber < base.ber * BER_BETTER
        } else {
            self.snr > base.snr + ACCEPT_DB
        }
    }

    /// Nothing to win by another gain.
    fn good(&self) -> bool {
        self.snr >= GOOD_SNR && self.ber < GOOD_BER
    }
}

impl std::fmt::Display for Quality {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "SNR {:.1} dB", self.snr)?;
        if self.ber >= BER_COUNTED {
            write!(f, ", FIC BER {:.1e}", self.ber)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Phase {
    /// Waiting before measuring the current ceiling.
    Settle(Instant),
    /// Measuring the SNR and the bit errors (`ber`) at the current ceiling,
    /// and the lowest and highest gain the device used meanwhile.
    Base { until: Instant, sum: f32, ber: f32, n: u32, lo: usize, hi: usize },
    /// Probe ceiling applied, waiting for the receiver to settle.
    ProbeSettle { until: Instant, base: Quality },
    /// Measuring at the probe ceiling; `off` once the gain was not the probe's.
    Probe { until: Instant, base: Quality, sum: f32, ber: f32, n: u32, off: bool },
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
    /// What the start level gave (its first measurement).
    at_start: Option<f32>,
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
    /// The probe is a rise to the gain the level allows (`RISE_ROOM_DB`).
    rising: bool,
    /// Channels where a rise was no better, and when.
    no_rise: HashMap<String, Instant>,
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
            rising: false,
            no_rise: HashMap::new(),
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
        self.rising = false;
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

    /// Feed a signal report; returns a ceiling to apply.
    pub fn on_signal(&mut self, report: Report, now: Instant) -> Option<Ceiling> {
        let Report { synced, snr, null_snr, ber, gain, room } = report;
        if self.gains.is_empty() || self.channel.is_none() {
            return None;
        }
        if self.sweep.is_some() {
            return self.sweep_step(if synced { snr } else { null_snr }, now);
        }
        if !synced || snr < POOR_SNR {
            let since = *self.trouble_since.get_or_insert(now);
            let again = self.last_sweep.is_none_or(|t| now.duration_since(t) >= SWEEP_AGAIN);
            // (a rise is measured to its end: a sweep would start from a gain the device is not at)
            if now.duration_since(since) >= SWEEP_AFTER && again && (!synced || !self.swept) && !self.rising {
                return self.start_sweep(now, synced, snr);
            }
        } else {
            self.trouble_since = None;
        }
        if !synced {
            if self.rising {
                // the step upsets the demodulator for a moment
                if matches!(self.phase, Phase::ProbeSettle { until, .. } if now < until) {
                    return None;
                }
                return self.rise_failed(None, now);
            }
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
                    self.phase = Phase::Base { until: now + MEASURE, sum: 0.0, ber: 0.0, n: 0, lo: g, hi: g };
                }
            }
            Phase::Base { until, sum, ber: errors, n, lo, hi } => {
                let g = gain.unwrap_or(self.ceiling);
                let (sum, errors, n, lo, hi) = (sum + snr, errors + ber, n + 1, lo.min(g), hi.max(g));
                if now < until || n < 6 {
                    self.phase = Phase::Base { until, sum, ber: errors, n, lo, hi };
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
                let base = Quality { snr: sum / n as f32, ber: errors / n as f32 };
                if lo != hi {
                    // the AGC moved the gain meanwhile: measure again
                    self.phase = Phase::Base { until: now + MEASURE, sum: 0.0, ber: 0.0, n: 0, lo: g, hi: g };
                } else if base.good() {
                    if !lowered {
                        if let Some(c) = self.start_rise(base, room, hi, now) {
                            return Some(c);
                        }
                    }
                    // good reception: nothing to win, a probe would be heard
                    self.learn();
                    self.phase = Phase::Wait(now + MAX_INTERVAL);
                } else if let Some(c) = (!lowered).then(|| self.start_rise(base, room, hi, now)).flatten() {
                    // poor reception far below what the level allows: most of the
                    // room at once, not steps of a dB or two whose gain the noise
                    // of a 3 s measurement hides (measured: stuck at 2.7 dB, SNR
                    // 10.5 dB, for a signal giving 21 dB at 22.9 dB)
                    return Some(c);
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
                        return if self.rising { self.rise_failed(None, now) } else { self.probe_failed(now) };
                    }
                    self.phase = Phase::Probe { until: now + MEASURE, base, sum: 0.0, ber: 0.0, n: 0, off: false };
                }
            }
            Phase::Probe { until, base, sum, ber: errors, n, off } => {
                let (sum, errors, n) = (sum + snr, errors + ber, n + 1);
                let off = off || gain.is_some_and(|g| g != self.probe);
                if now < until || n < 6 {
                    self.phase = Phase::Probe { until, base, sum, ber: errors, n, off };
                    return None;
                }
                let s = Quality { snr: sum / n as f32, ber: errors / n as f32 };
                if self.rising {
                    if off || !s.better_than(&base) {
                        return self.rise_failed(Some((base, s)), now);
                    }
                    log::info!(
                        "gain: {:.1} dB -> {:.1} dB, the signal level left room ({base} -> {s})",
                        self.gains[self.ceiling],
                        self.gains[self.probe]
                    );
                    self.rising = false;
                    self.ceiling = self.probe;
                    self.learn();
                    self.phase = Phase::Wait(now + MIN_INTERVAL);
                    return None;
                }
                if !off && s.better_than(&base) {
                    log::info!("gain: {:.1} dB -> {:.1} dB ({base} -> {s})", self.gains[self.ceiling], self.gains[self.probe]);
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

    /// Reception (good, or poor and far from what the level allows) at a ceiling
    /// the gain is at, with much room left by the level (`RISE_ROOM_DB`): the
    /// ceiling to try instead, most of the room up.
    fn start_rise(&mut self, base: Quality, room: Option<f32>, gain: usize, now: Instant) -> Option<Ceiling> {
        let room = room.filter(|&r| r >= RISE_ROOM_DB)?;
        if base.snr >= RISE_BELOW_SNR || gain != self.ceiling {
            return None;
        }
        let channel = self.channel.as_ref()?;
        if self.no_rise.get(channel).is_some_and(|&t| now.saturating_duration_since(t) < RISE_AGAIN) {
            return None;
        }
        let most = self.gains[self.ceiling] + room - RISE_MARGIN_DB;
        let target = (self.ceiling + 1..self.gains.len()).take_while(|&i| self.gains[i] <= most).last()?;
        self.probe = target;
        self.rising = true;
        self.phase = Phase::ProbeSettle { until: now + SETTLE, base };
        Some(Ceiling { index: target, jump: true })
    }

    /// The rise was no better (`measured`: before and after), lost the signal
    /// or the AGC took the gain back: the ceiling stays, and the channel is
    /// left alone for a long time.
    fn rise_failed(&mut self, measured: Option<(Quality, Quality)>, now: Instant) -> Option<Ceiling> {
        match measured {
            Some((base, s)) => {
                log::info!("gain: {:.1} dB kept, {:.1} dB was no better ({base} -> {s})", self.gains[self.ceiling], self.gains[self.probe])
            }
            None => log::info!("gain: {:.1} dB kept, {:.1} dB did not hold", self.gains[self.ceiling], self.gains[self.probe]),
        }
        self.rising = false;
        if let Some(ch) = &self.channel {
            self.no_rise.insert(ch.clone(), now);
        }
        self.phase = Phase::Wait(now + MAX_INTERVAL);
        Some(Ceiling { index: self.ceiling, jump: true })
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

    fn start_probe(&mut self, base: Quality, now: Instant) -> Option<Ceiling> {
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
            at_start: None,
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
        if sw.level == sw.start && sw.at_start.is_none() {
            sw.at_start = Some(sw.max);
        }
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
        let (start, sw_at_start) = (sw.start, sw.at_start);
        self.sweep = None;
        self.phase = Phase::Settle(now + SETTLE);
        self.trouble_since = None;
        // Another level only for reception clearly better than at the start: the
        // levels are measured one after another, and a transmitter that was off
        // (switched over, restarted) comes back at whichever level is measured
        // then, mostly a low one, with an SNR of a few dB on its way to sync.
        // (measured: 22.9 dB -> 0.0 dB by a sweep at SNR 2.2 dB, 21 dB lost.)
        let at_start = sw_at_start.unwrap_or(f32::MIN);
        if best >= FOUND_SNR && (best_level == start || best >= at_start + WORSE_DB) {
            log::info!("gain: {:.1} dB -> {:.1} dB by a sweep (SNR {best:.1} dB)", self.gains[start], self.gains[best_level]);
            self.ceiling = best_level;
            self.learn();
        } else if best >= FOUND_SNR {
            log::info!(
                "gain: {:.1} dB kept, the sweep found nothing clearly better (SNR {best:.1} dB at {:.1} dB)",
                self.gains[start],
                self.gains[best_level]
            );
            self.ceiling = start;
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

    /// A report without bit errors.
    fn report(synced: bool, snr: f32, null_snr: f32, gain: Option<usize>, room: Option<f32>) -> Report {
        Report { synced, snr, null_snr, ber: 0.0, gain, room }
    }

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

        /// The room the level leaves for more gain (dB), as the device says it.
        fn room(&self) -> Option<f32> {
            Some(GAINS[self.limit] - GAINS[self.gain] + 0.5)
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
            if let Some(c) = g.on_signal(report(synced, snr, null_snr, Some(dev.gain), dev.room()), t0 + Duration::from_millis(250 * k)) {
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
            if let Some(c) = g.on_signal(report(false, 0.0, -10.0, Some(ceiling), None), t0 + Duration::from_millis(250 * k)) {
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
            // (the level allows a few dB more, as with the AGC at its own limit)
            if let Some(c) = g.on_signal(report(true, snr, snr, Some(dev.gain), Some(3.0)), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        assert_eq!(dev.changes, 0, "the gain was changed {} times", dev.changes);
    }

    /// A channel with a ceiling learned before, on a device allowing gains up
    /// to `limit`, for `seconds`: the SNR by the gain, with the reports of the
    /// first `blind` quarters of a second after a change of the gain without
    /// synchronisation. Tuned to again at half time.
    fn run_learned(curve: impl Fn(f32) -> f32, learned: f32, limit: usize, seconds: u64, blind: u32) -> (GainSeeker, Device) {
        let mut g = GainSeeker::new();
        g.learn_db("5B", learned);
        let start = g.retune("5B", &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        let (mut changes, mut upset) = (0, 0);
        for k in 0..seconds * 4 {
            if k == seconds * 2 {
                let c = g.retune("5B", &GAINS).unwrap();
                dev.apply(Ceiling { index: c, jump: true });
            }
            if dev.changes != changes {
                changes = dev.changes;
                upset = blind;
            }
            let snr = curve(GAINS[dev.gain]);
            let synced = upset == 0;
            upset = upset.saturating_sub(1);
            let seen = if synced { (true, snr, snr) } else { (false, 0.0, -10.0) };
            if let Some(c) = g.on_signal(report(seen.0, seen.1, seen.2, Some(dev.gain), dev.room()), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        (g, dev)
    }

    /// A channel learned at `learned` dB on a device whose level allows gains up
    /// to `limit`: the SNR by the gain, or what `at(k)` says for report `k`.
    fn run_at(learned: f32, limit: usize, seconds: u64, at: impl Fn(u64, f32) -> (bool, f32)) -> (GainSeeker, Device, f32) {
        let mut g = GainSeeker::new();
        g.learn_db("5D", learned);
        let start = g.retune("5D", &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        let mut lowest = f32::MAX;
        for k in 0..seconds * 4 {
            let (synced, snr) = at(k, GAINS[dev.gain]);
            let null_snr = if synced { snr } else { 0.4 };
            if let Some(c) = g.on_signal(report(synced, snr, null_snr, Some(dev.gain), dev.room()), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
            lowest = lowest.min(GAINS[g.ceiling]);
        }
        (g, dev, lowest)
    }

    /// A transmitter switched over (off for 6 s) while the gain was right: a
    /// sweep in the gap must not leave the gain at a level where it came back
    /// (measured: 22.9 dB -> 0.0 dB by a sweep at SNR 2.2 dB).
    #[test]
    fn a_transmitter_off_for_a_moment_does_not_take_the_gain_down() {
        // the 8 bits' noise below, 21 dB from 20 dB of gain, the level allowing up to 29.7 dB
        let curve = |db: f32| (2.5 + db).min(21.0);
        let (g, _, lowest) = run_at(22.9, 16, 120, |k, db| match k {
            // off from 20 s to 26 s, then 2 s back at 2 dB on the way to sync
            80..104 => (false, 0.0),
            104..112 => (true, 2.2),
            _ => (true, curve(db)),
        });
        assert!(lowest >= 20.0, "the ceiling went down to {lowest:.1} dB");
        assert!(GAINS[g.ceiling] >= 20.0, "ceiling {:.1} dB", GAINS[g.ceiling]);
    }

    /// Poor reception at a gain far below what the level allows (left there by
    /// an old sweep): most of the room at once, within seconds, not a climb of
    /// probes a dB or two each.
    #[test]
    fn poor_reception_far_below_the_level_rises_at_once() {
        let curve = |db: f32| (2.5 + 3.0 * db).min(21.0);
        let (g, dev, _) = run_at(2.7, 16, 12, |_, db| (true, curve(db)));
        assert!(GAINS[g.ceiling] >= 15.0, "ceiling {:.1} dB", GAINS[g.ceiling]);
        assert_eq!(dev.over, 0, "the gain went above what the level allows");
    }

    #[test]
    fn a_ceiling_far_below_what_the_level_allows_goes_back_up() {
        // measured: 0 dB learned for a multiplex whose level allows 16.6 dB
        // (index 10), 17.5 dB of SNR there for the 8 bits, 37 dB with the gain
        let curve = |db: f32| (17.5 + db * 1.2).min(37.0);
        let (g, dev) = run_learned(curve, 0.0, 10, 120, 0);
        assert!((8.0..=16.6).contains(&GAINS[g.ceiling]), "ceiling {:.1} dB", GAINS[g.ceiling]);
        assert_eq!(dev.gain, g.ceiling);
        assert_eq!(dev.over, 0, "the gain went above what the level allows");
        assert_eq!(dev.changes, 1, "one step up, kept after tuning again");
        assert_eq!(g.learned.get("5B"), Some(&GAINS[g.ceiling]));
    }

    #[test]
    fn a_rise_waits_for_the_demodulator_upset_by_the_step() {
        let curve = |db: f32| (17.5 + db * 1.2).min(37.0);
        let (g, dev) = run_learned(curve, 0.0, 10, 120, 3);
        assert!(GAINS[g.ceiling] >= 8.0, "ceiling {:.1} dB", GAINS[g.ceiling]);
        assert_eq!(dev.changes, 1);
    }

    #[test]
    fn a_rise_that_is_no_better_is_not_tried_again() {
        // the SNR is the signal's own: more gain wins nothing
        let (g, dev) = run_learned(|_| 20.0, 0.0, 10, 1200, 0);
        assert_eq!(GAINS[g.ceiling], 0.0);
        assert_eq!(dev.gain, 0);
        // there and back, once: not again after 4 minutes, nor after tuning again
        assert_eq!(dev.changes, 2, "{} gain changes", dev.changes);
    }

    #[test]
    fn a_rise_that_loses_the_signal_goes_back() {
        // more gain overloads the tuner: nothing received above 5 dB
        let mut g = GainSeeker::new();
        g.learn_db("5B", 0.0);
        let start = g.retune("5B", &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit: 10, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        for k in 0..60 * 4 {
            let seen = if GAINS[dev.gain] > 5.0 { (false, 0.0, -10.0) } else { (true, 20.0, 20.0) };
            if let Some(c) = g.on_signal(report(seen.0, seen.1, seen.2, Some(dev.gain), dev.room()), t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        assert_eq!(dev.gain, 0);
        assert_eq!(GAINS[g.ceiling], 0.0);
        assert_eq!(dev.changes, 2, "{} gain changes", dev.changes);
    }

    /// A channel on a device allowing gains up to `limit`, from a ceiling
    /// learned before (else the default): the SNR and the bit error ratio of
    /// the FIC by the gain. With the gain changes of the last five minutes.
    fn run_quality(quality: impl Fn(f32) -> (f32, f32), learned: Option<f32>, limit: usize, seconds: u64) -> (GainSeeker, Device, u32) {
        let mut g = GainSeeker::new();
        if let Some(db) = learned {
            g.learn_db("11B", db);
        }
        let start = g.retune("11B", &GAINS).unwrap();
        let mut dev = Device { gain: start, ceiling: start, limit, ticks: 0, changes: 0, over: 0 };
        let t0 = Instant::now();
        let mut before = 0;
        for k in 0..seconds * 4 {
            if k == seconds.saturating_sub(300) * 4 {
                before = dev.changes;
            }
            let (snr, ber) = quality(GAINS[dev.gain]);
            let r = Report { synced: true, snr, null_snr: snr, ber, gain: Some(dev.gain), room: dev.room() };
            if let Some(c) = g.on_signal(r, t0 + Duration::from_millis(250 * k)) {
                dev.apply(c);
            }
            dev.agc();
        }
        let late = dev.changes - before;
        (g, dev, late)
    }

    #[test]
    fn a_gain_is_better_by_the_bit_errors_where_they_can_be_counted() {
        let q = |snr: f32, ber: f32| Quality { snr, ber };
        // no errors to count: the SNR
        assert!(q(12.6, 0.0).better_than(&q(12.0, 2e-4)));
        assert!(!q(12.3, 0.0).better_than(&q(12.0, 0.0)));
        // errors to count: they decide, whatever the SNR
        assert!(q(10.0, 3e-2).better_than(&q(11.0, 6e-2)));
        assert!(!q(14.0, 6e-2).better_than(&q(10.0, 6e-2)));
        assert!(!q(30.0, 5e-3).better_than(&q(17.0, 0.0)));
        assert!(q(17.0, 0.0).better_than(&q(30.0, 5e-3)));
        // not by what a steady signal differs between two measurements
        assert!(!q(10.0, 5e-2).better_than(&q(10.0, 6e-2)));
        assert!(q(20.0, 5e-3).good() && !q(20.0, 2e-2).good() && !q(14.0, 0.0).good());
    }

    #[test]
    fn bit_errors_decide_between_gains_of_the_same_snr() {
        // a tuner driven too hard by what it receives: the SNR (against the
        // null symbol) is the same at every gain, the errors come with the gain
        let (g, _, _) = run_quality(|db| (12.0, 2e-3 * 10f32.powf((db - 20.0) / 10.0)), None, GAINS.len() - 1, 600);
        assert!(GAINS[g.ceiling] <= 20.7, "ceiling {:.1} dB", GAINS[g.ceiling]);
    }

    #[test]
    fn a_good_snr_with_many_bit_errors_is_not_left_alone() {
        let (g, _, late) = run_quality(|db| (22.0, 3e-2 * 10f32.powf((db - 40.2) / 10.0)), None, GAINS.len() - 1, 900);
        assert!(GAINS[g.ceiling] < 34.0, "ceiling {:.1} dB", GAINS[g.ceiling]);
        // (and once the errors are few it is)
        assert_eq!(late, 0, "{late} gain changes in the last five minutes");
    }

    #[test]
    fn a_rise_that_brings_bit_errors_goes_back() {
        // more SNR with the gain, and errors that were not there
        let quality = |db: f32| ((17.5 + db * 1.2).min(37.0), if db > 5.0 { 5e-3 } else { 0.0 });
        let (g, dev, _) = run_quality(quality, Some(0.0), 10, 600);
        assert_eq!(GAINS[g.ceiling], 0.0);
        assert_eq!(dev.gain, 0);
        assert_eq!(dev.changes, 2, "{} gain changes", dev.changes);
    }

    #[test]
    fn a_few_bit_errors_at_a_good_snr_are_left_alone() {
        let (_, dev, _) = run_quality(|_| (20.0, 5e-3), Some(25.4), 14, 600);
        assert_eq!(dev.changes, 0);
    }

    #[test]
    fn room_is_left_unused_where_nothing_is_to_win() {
        // an SNR nobody hears the difference of
        let (_, dev) = run_learned(|_| 31.0, 0.0, 10, 600, 0);
        assert_eq!(dev.changes, 0);
        // little room: the gain is about where the AGC would have it
        let (_, dev) = run_learned(|_| 20.0, 8.7, 10, 600, 0);
        assert_eq!(dev.changes, 0);
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
