//! Time-aligned DAB/FM linking of one service, like a car radio: the FM audio
//! runs through a delay line so that it lines up with the DAB audio (the
//! offset between both streams is found by correlating them) and the output
//! crossfades between the two. A short DAB buffer gives the look-ahead to fade
//! over before a DAB dropout reaches the output.
//!
//! Positions are frame indices (48 kHz stereo frames): DAB frame `j` carries
//! the same content as FM frame `j + offset`. Both are read at fractional
//! positions: the offset follows the FM tempo (clock drift, a broadcaster
//! changing its delay by slowing the FM down). A correction moves the stream
//! that is not heard, so the one heard never jumps. The DAB audio paces the
//! output while it is heard; the FM audio takes over when FM is faded in or
//! DAB stalls (the tuner keeps delivering when DAB drops out) and is played
//! as it comes, slowed down by up to 2 % only while DAB lacks look-ahead (as
//! the DSP of a car radio does). The DAB look-ahead moves to its target the
//! same way, never by a pause or a jump.

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use serde::Serialize;
use std::collections::VecDeque;
use std::time::Instant;

pub const RATE: usize = 48_000;
/// Decimation for the correlation (8 kHz).
const DEC: usize = 6;
const DEC_RATE: usize = RATE / DEC;
/// Correlation window.
const WINDOW_S: f32 = 4.0;
/// FM may come later than DAB by this much (a delay set on the FM side,
/// normally FM comes first): DAB then waits for it.
const BEHIND_S: f32 = 6.0;
/// Most the output speeds up or slows down to follow the FM or to move the
/// DAB look-ahead (as the DSP of a car radio does): 2 %.
const MAX_SLEW: f64 = 0.02;
/// The DAB look-ahead approaches its target at most 2 % faster or slower,
/// and the last bit of the way within about a second (no pitch step at the end).
const LOOKAHEAD_TAU_S: f64 = 1.0;
/// The FM tempo is estimated from the measurements of this much DAB (s).
const TEMPO_SPAN_S: f64 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Dab,
    Fm,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkerConfig {
    /// Longest delay between FM and DAB the FM buffer can absorb (s).
    pub max_delay: f32,
    /// DAB look-ahead (s).
    pub margin: f32,
    /// Crossfade (s).
    pub crossfade: f32,
    /// The FM audio brought to the loudness of DAB; off: as the tuner gives
    /// it (as many car radios do).
    pub match_level: bool,
}

impl Default for LinkerConfig {
    fn default() -> Self {
        LinkerConfig { max_delay: 5.0, margin: 0.4, crossfade: 0.15, match_level: true }
    }
}

/// Result of one correlation, for the debug view.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    /// FM ahead of DAB (ms).
    pub delay_ms: f32,
    /// Peak to second peak ratio.
    pub confidence: f32,
    /// DAB level minus FM level (dB).
    pub gain_db: f32,
    /// -1: the FM audio has the opposite polarity (corrected).
    pub polarity: f32,
    /// Correlation coefficient of the aligned audio (48 kHz, signed, L+R).
    pub correlation: f32,
    /// The same above about 1 kHz (it tells the polarity) and below 250 Hz
    /// (an FM processor's phase rotator turns the bass, up to inverting it).
    pub correlation_high: f32,
    pub correlation_low: f32,
    /// Of the side signal (L-R) above about 1 kHz: negative with the FM
    /// channels swapped (not corrected).
    pub correlation_side: f32,
    /// Correlation around the peak (±25 ms).
    pub curve: Vec<f32>,
    /// Coarse correlation over the whole search range.
    pub overview: Vec<f32>,
    pub accepted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkerStatus {
    pub source: Option<Source>,
    /// 0 = DAB, 1 = FM.
    pub fade: f32,
    pub aligned: bool,
    pub dab_aligned: bool,
    pub paced_by_fm: bool,
    /// FM plays without DAB to align it to (DAB has no audio).
    pub fm_only: bool,
    /// DAB look-ahead available (ms).
    pub dab_buffer_ms: f32,
    /// Delay applied to the FM audio (ms).
    pub fm_delay_ms: f32,
    /// FM ahead of DAB: median of the last accepted measurements (ms), steadier
    /// to read than a single one; negative when FM comes later.
    pub fm_ahead_ms: Option<f32>,
    /// What DAB waits for a late FM on top of its look-ahead (ms).
    pub dab_extra_ms: f32,
    pub drift_ppm: f32,
    pub gain_db: f32,
    pub polarity: f32,
    /// Polarity of the FM side signal (L-R): with `polarity` -1 and this +1
    /// the FM channels are swapped and inverted (corrected).
    pub side_polarity: f32,
    /// DAB stalls bridged by FM.
    pub stalls: u64,
    pub ab: bool,
    /// In A/B listening FM is on the right now (else DAB alone).
    pub ab_fm: bool,
}

/// Interleaved stereo frames from a start index.
struct Frames {
    data: VecDeque<f32>,
    start: i64,
}

impl Frames {
    fn new() -> Self {
        Frames { data: VecDeque::new(), start: 0 }
    }

    fn end(&self) -> i64 {
        self.start + (self.data.len() / 2) as i64
    }

    fn push(&mut self, pcm: &[f32]) {
        self.data.extend(pcm.iter().copied());
    }

    fn get(&self, i: i64) -> Option<(f32, f32)> {
        if i < self.start || i >= self.end() {
            return None;
        }
        let k = ((i - self.start) * 2) as usize;
        Some((self.data[k], self.data[k + 1]))
    }

    /// Frame at a fractional index (cubic interpolation; at the edges the
    /// nearest frames).
    fn get_f(&self, x: f64) -> Option<(f32, f32)> {
        let i = x.floor() as i64;
        let t = (x - i as f64) as f32;
        if t == 0.0 {
            return self.get(i);
        }
        let p1 = self.get(i)?;
        let Some(p2) = self.get(i + 1) else { return Some(p1) };
        let (p0, p3) = (self.get(i - 1).unwrap_or(p1), self.get(i + 2).unwrap_or(p2));
        // Catmull-Rom
        let c =
            |a: f32, b: f32, c: f32, d: f32| b + 0.5 * t * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a)));
        Some((c(p0.0, p1.0, p2.0, p3.0), c(p0.1, p1.1, p2.1, p3.1)))
    }

    /// Keep frames from `i` on.
    fn trim_before(&mut self, i: i64) {
        let n = (i - self.start).clamp(0, (self.data.len() / 2) as i64);
        self.data.drain(..(n * 2) as usize);
        self.start += n;
    }

    fn reset(&mut self, start: i64) {
        self.data.clear();
        self.start = start;
    }
}

/// Mono 8 kHz copy for the correlation.
struct Decimated {
    data: VecDeque<f32>,
    /// Index of `data[0]` (in decimated samples).
    start: i64,
    acc: f32,
    n: usize,
}

impl Decimated {
    fn new() -> Self {
        Decimated { data: VecDeque::new(), start: 0, acc: 0.0, n: 0 }
    }

    fn end(&self) -> i64 {
        self.start + self.data.len() as i64
    }

    fn push(&mut self, pcm: &[f32], keep: usize) {
        for f in pcm.chunks_exact(2) {
            self.acc += f[0] + f[1];
            self.n += 1;
            if self.n == DEC {
                self.data.push_back(self.acc / (2 * DEC) as f32);
                self.acc = 0.0;
                self.n = 0;
            }
        }
        while self.data.len() > keep {
            self.data.pop_front();
            self.start += 1;
        }
    }

    /// Samples `[from, to)` if available.
    fn slice(&self, from: i64, to: i64) -> Option<Vec<f32>> {
        if from < self.start || to > self.end() || from >= to {
            return None;
        }
        Some(self.data.range((from - self.start) as usize..(to - self.start) as usize).copied().collect())
    }

    fn reset(&mut self, start: i64) {
        self.data.clear();
        self.start = start;
        self.acc = 0.0;
        self.n = 0;
    }
}

pub struct Linker {
    cfg: LinkerConfig,
    dab: Frames,
    fm: Frames,
    dab_dec: Decimated,
    fm_dec: Decimated,
    /// Position of the output on the DAB timeline (fractional frames).
    pos: f64,
    /// DAB frames per output frame: 1 normally, a little more or less while
    /// the look-ahead moves to its target (as the DSP of a car radio does:
    /// the output never pauses or jumps for it).
    speed: f64,
    started: bool,
    /// The DAB look-ahead in force (frames): what is left of the DAB audio
    /// after each DAB block while DAB paces the output. It fills before the
    /// output starts and moves to its target by the speed; while FM paces it
    /// follows the look-ahead after each DAB block, so that DAB pacing
    /// continues from there (the bursts of DAB do not count).
    la: f64,
    /// FM frame = DAB frame + offset (fractional); it moves with the FM tempo.
    offset: Option<f64>,
    /// FM frames per DAB frame: 1 + the tempo difference (clock drift, or a
    /// broadcaster changing its delay by slowing the FM down).
    fm_rate: f64,
    /// Correction of the offset waiting for a crossfade to end.
    pending: f64,
    /// A large correction asked for once while FM is heard: a second
    /// measurement has to agree.
    big: Option<f64>,
    /// (DAB frame, offset) of the last accepted measurements: the FM tempo.
    tempo: VecDeque<(f64, f64)>,
    /// The DAB indices agree with `offset` (not after a DAB dropout).
    dab_aligned: bool,
    fm_valid: bool,
    paced_by_fm: bool,
    target: Source,
    fade: f32,
    gain: f32,
    /// -1 when the FM audio (its mid signal, L+R) is inverted against DAB.
    polarity: f32,
    /// The same for the side signal (L-R); mid and side corrected separately
    /// set right swapped channels too.
    side_polarity: f32,
    /// Measurements in a row that ask for the other polarity (mid, side): a
    /// polarity changes only after a few clear ones.
    pol_votes: (u32, u32),
    last_dab: Option<Instant>,
    /// Typical interval between DAB blocks (DAB+ decodes 120 ms superframes).
    dab_interval: f32,
    /// The last accepted delays (ms), for the median shown.
    recent: VecDeque<f32>,
    /// Extra DAB look-ahead (frames) while FM comes later than DAB.
    extra: f64,
    slips: u64,
    /// A/B listening: DAB left, FM right.
    pub ab: bool,
    /// The last output had FM on the right in A/B listening (else DAB alone:
    /// FM not aligned or not there yet).
    ab_fm: bool,
    /// FM may take over by itself when DAB stalls (automatic mode).
    pub allow_auto: bool,
    last_out: (f32, f32),
    /// Things worth logging (taken by the caller).
    pub events: Vec<String>,
}

/// Correlation coefficients of the aligned DAB and FM audio.
#[derive(Clone, Copy, Default)]
struct Bands {
    /// Mid signal (L+R).
    mid: f32,
    /// Mid above about 1 kHz: tells the polarity.
    high: f32,
    /// Mid below 250 Hz.
    low: f32,
    /// Side signal (L-R) above about 1 kHz.
    side: f32,
}

/// One correlation: FM frame = DAB frame `i` + `o`, found at FM tempo `rate`.
struct Corr {
    i: f64,
    o: f64,
    rate: f64,
    /// The FM's tempo between the refined points (finer than `rate`).
    local_rate: f64,
    confidence: f32,
    bands: Bands,
    gain_db: f32,
    delay_s: f32,
    curve: Vec<f32>,
    overview: Vec<f32>,
    /// The 48 kHz refinement found the peak and agreed with the tempo.
    refined: bool,
    /// The FM's tempo as the window's halves tell it, when they disagree.
    tempo_hint: Option<f64>,
}

impl Linker {
    pub fn new(cfg: LinkerConfig) -> Self {
        Linker {
            cfg,
            dab: Frames::new(),
            fm: Frames::new(),
            dab_dec: Decimated::new(),
            fm_dec: Decimated::new(),
            pos: 0.0,
            speed: 1.0,
            started: false,
            la: 0.0,
            offset: None,
            fm_rate: 1.0,
            pending: 0.0,
            big: None,
            tempo: VecDeque::new(),
            dab_aligned: false,
            fm_valid: false,
            paced_by_fm: false,
            target: Source::Dab,
            fade: 0.0,
            gain: 1.0,
            polarity: 1.0,
            side_polarity: 1.0,
            pol_votes: (0, 0),
            last_dab: None,
            dab_interval: 0.12,
            recent: VecDeque::new(),
            extra: 0.0,
            slips: 0,
            ab: false,
            ab_fm: false,
            allow_auto: true,
            last_out: (0.0, 0.0),
            events: Vec::new(),
        }
    }

    /// Target of the DAB look-ahead (frames).
    fn lookahead(&self) -> f64 {
        self.cfg.margin as f64 * RATE as f64 + self.extra
    }

    fn keep_dec(&self) -> usize {
        ((self.cfg.max_delay + BEHIND_S + WINDOW_S + 3.0) * DEC_RATE as f32) as usize
    }

    /// Offset at DAB frame `i` (it moves with the FM tempo).
    fn offset_at(&self, cur: f64, i: f64) -> f64 {
        cur + (self.fm_rate - 1.0) * (i - self.pos)
    }

    /// Median of the last accepted delays (ms; FM ahead of DAB).
    fn median_ahead(&self) -> Option<f32> {
        if self.recent.is_empty() {
            return None;
        }
        let mut v: Vec<f32> = self.recent.iter().copied().collect();
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[v.len() / 2])
    }

    /// FM later than DAB: DAB waits as much longer, so that switching stays
    /// seamless instead of repeating what was heard (as car radios do); the
    /// look-ahead grows by slowing the output a little, never by a pause, and
    /// goes back to normal the same way once FM comes first.
    fn adjust_lookahead(&mut self) {
        let Some(ahead) = self.median_ahead() else { return };
        let ahead_s = ahead / 1000.0;
        let need_s = if ahead_s < -(self.cfg.margin - 0.1) { -ahead_s + 0.3 - self.cfg.margin } else { 0.0 };
        let need = need_s as f64 * RATE as f64;
        if (need - self.extra).abs() < RATE as f64 / 5.0 {
            return;
        }
        self.extra = need;
        self.events.push(if need > 0.0 {
            format!("FM {:.1} s behind DAB: DAB waits {:.1} s more (the output slows down to get there)", -ahead_s, need_s)
        } else {
            "FM ahead of DAB again: normal look-ahead".into()
        });
    }

    pub fn source(&self) -> Source {
        if self.fade >= 0.5 {
            Source::Fm
        } else {
            Source::Dab
        }
    }

    /// Where the output is going (it fades there when possible).
    pub fn target(&self) -> Source {
        self.target
    }

    pub fn aligned(&self) -> bool {
        self.offset.is_some() && self.dab_aligned
    }

    /// The decision of the follower; the fade starts when it is possible.
    pub fn want(&mut self, s: Source) {
        self.target = s;
    }

    /// A linked FM station is tuned and verified.
    pub fn fm_valid(&self) -> bool {
        self.fm_valid
    }

    /// A linked FM station is (no longer) tuned and verified.
    pub fn set_fm_valid(&mut self, valid: bool) {
        if valid == self.fm_valid {
            return;
        }
        self.fm_valid = valid;
        if !valid {
            self.fm.reset(0);
            self.fm_dec.reset(0);
            self.offset = None;
            self.fm_rate = 1.0;
            self.pending = 0.0;
            self.big = None;
            self.tempo.clear();
            self.target = Source::Dab;
            if self.fade > 0.0 {
                self.events.push("FM lost while it was heard".into());
            }
            self.fade = 0.0;
            self.paced_by_fm = false;
        }
    }

    /// DAB audio of the service; returns the output when DAB paces it.
    pub fn push_dab(&mut self, pcm: &[f32], now: Instant) -> Vec<f32> {
        let since = self.last_dab.map(|t| now.duration_since(t).as_secs_f32());
        let gap = since.is_some_and(|d| d > 0.4);
        // the decoder hands over several AUs at once: the period of the bursts counts
        if let Some(d) = since.filter(|&d| d > 0.01 && d <= 0.4) {
            self.dab_interval = self.dab_interval * 0.9 + d * 0.1;
        }
        self.last_dab = Some(now);
        if !self.started {
            self.started = true;
            self.dab.reset(0);
            self.dab_dec.reset(0);
            self.pos = 0.0;
            self.la = self.lookahead();
        } else if gap || (self.paced_by_fm && !self.dab_aligned && self.dab.end() as f64 <= self.pos) {
            // DAB comes back after a dropout: its content jumped, place it
            // after the look-ahead and find the offset again (aligned DAB
            // behind the output is waited for: FM slows down for it)
            let at = (self.pos + self.lookahead()).ceil() as i64;
            self.dab.reset(at);
            self.dab_dec.reset(at.div_euclid(DEC as i64));
            if self.dab_aligned && self.offset.is_some() {
                self.events.push("DAB back after a dropout, aligning again".into());
            }
            self.dab_aligned = false;
            self.tempo.clear();
        }
        self.dab.push(pcm);
        let keep = self.keep_dec();
        self.dab_dec.push(pcm, keep);
        if self.paced_by_fm && self.fade == 0.0 && self.target == Source::Dab && self.dab_aligned {
            self.paced_by_fm = false;
        }
        if self.paced_by_fm {
            // (checked before: DAB pacing takes over with the look-ahead
            // after the last block, not with this block's part of a burst)
            self.la = self.dab.end() as f64 - self.pos;
            self.trim_dab();
            return Vec::new();
        }
        // DAB paces the output: what is owed beyond the look-ahead in force,
        // so as many frames as arrive (real time), none while the look-ahead
        // fills (or when FM paced ahead of this burst)
        let owed = self.dab.end() as f64 - self.pos - self.la;
        let n = (owed + 1e-6).max(0.0) as usize;
        if n == 0 {
            self.trim_dab();
            return Vec::new();
        }
        // the look-ahead moves to its target by playing DAB a little slower
        // or faster: n frames move it by n * (1 - speed)
        let s = self.lookahead_slew(n);
        self.speed = 1.0 - s;
        let out = self.render(n);
        self.la += n as f64 * s;
        if (self.lookahead() - self.la).abs() < 1.0 {
            self.la = self.lookahead();
        }
        self.trim_dab();
        out
    }

    /// Slew of the DAB look-ahead for the next `n` output frames (frames
    /// gained per output frame; negative: DAB faster): at most 2 %, near the
    /// target less and less, the change of speed spread over a second so
    /// that the pitch never steps.
    fn lookahead_slew(&self, n: usize) -> f64 {
        let d = self.lookahead() - self.la;
        if d.abs() < 1.0 {
            return 0.0;
        }
        let most = d.abs() / n as f64;
        let want = (d / (LOOKAHEAD_TAU_S * RATE as f64)).clamp(-MAX_SLEW, MAX_SLEW).clamp(-most, most);
        let s = self.ramp_to(want, n);
        // never past the target within these frames
        if s * d > 0.0 {
            s.clamp(-most, most)
        } else {
            s
        }
    }

    /// FM audio of the linked station; returns the output when FM paces it.
    pub fn push_fm(&mut self, pcm: &[f32], now: Instant) -> Vec<f32> {
        if !self.fm_valid {
            return Vec::new();
        }
        if self.fm.data.is_empty() && self.fm_dec.data.is_empty() {
            self.fm.reset(0);
            self.fm_dec.reset(0);
        }
        self.fm.push(pcm);
        let keep = self.keep_dec();
        self.fm_dec.push(pcm, keep);
        let cap = (self.keep_dec() * DEC) as i64;
        let end = self.fm.end();
        self.fm.trim_before(end - cap);
        if !self.started {
            // no DAB audio at all and FM wanted (DAB without audio): FM plays
            // as it comes, not aligned (there is nothing to align it with);
            // DAB coming later is placed and aligned as after a dropout
            let lead = (0.3 * RATE as f32) as i64;
            if self.target != Source::Fm || self.last_dab.is_some() || self.fm.end() - self.fm.start < 2 * lead {
                return Vec::new();
            }
            self.started = true;
            self.dab.reset(0);
            self.dab_dec.reset(0);
            self.pos = 0.0;
            self.la = self.lookahead();
            self.dab_aligned = false;
            self.offset = Some((self.fm.end() - lead) as f64);
            self.paced_by_fm = true;
            self.fade = 1.0;
            self.speed = 1.0;
            self.events.push("DAB without audio: FM plays as it comes (not aligned)".into());
        }
        // DAB stalls: FM continues the output and fades in while the look-ahead lasts
        let stall_after = (self.dab_interval * 2.0).max(0.15);
        let stalled = self.last_dab.is_none_or(|t| now.duration_since(t).as_secs_f32() > stall_after);
        let stalled = stalled && self.allow_auto;
        if stalled && self.offset.is_some() && self.target == Source::Dab && self.fade < 1.0 {
            self.events.push("DAB stalls, fading over to FM".into());
            self.slips += 1;
            self.target = Source::Fm;
        }
        let mut n = pcm.len() / 2;
        if !self.paced_by_fm {
            if self.fade > 0.0 || (stalled && self.offset.is_some()) {
                self.paced_by_fm = true;
                // the output takes up the real time since the last DAB block:
                // a stall noticed late would otherwise be lost for good (the
                // player's buffer, and DAB a longer look-ahead later)
                if let Some(t) = self.last_dab {
                    n = n.max((now.duration_since(t).as_secs_f64() * RATE as f64).round() as usize);
                }
            } else {
                return Vec::new();
            }
        }
        // FM paces: its frames are used as they arrive, slowed down a little
        // while DAB lacks look-ahead
        let s = if stalled { self.ramp_to(0.0, n) } else { self.fm_slew(n) };
        self.speed = (1.0 - s) / self.fm_rate;
        let out = self.render(n);
        self.trim_dab();
        out
    }

    /// Slow-down of the FM heard for the next `n` frames (FM paces): none
    /// while the DAB look-ahead is on its target, else up to 2 % until it is
    /// (DAB placed anew after a dropout, an FM speeding up).
    fn fm_slew(&self, n: usize) -> f64 {
        let d = self.lookahead() - self.la;
        let want = if d > 1.0 { (d / (LOOKAHEAD_TAU_S * RATE as f64)).min(MAX_SLEW) } else { 0.0 };
        self.ramp_to(want, n)
    }

    /// The current slew of the stream heard (1 - its frames per output
    /// frame) moved towards `want` as far as `n` frames allow: the speed
    /// changes by at most 2 % a second, so the pitch never steps.
    fn ramp_to(&self, want: f64, n: usize) -> f64 {
        let cur = if self.paced_by_fm { 1.0 - self.speed * self.fm_rate } else { 1.0 - self.speed };
        let ramp = MAX_SLEW * n as f64 / RATE as f64;
        (cur + (want - cur).clamp(-ramp, ramp)).clamp(-MAX_SLEW, MAX_SLEW)
    }

    fn trim_dab(&mut self) {
        // some history for re-alignment (and for refining against a late FM)
        let keep_from = self.pos as i64 - RATE as i64 - ((BEHIND_S + WINDOW_S) * RATE as f32) as i64;
        self.dab.trim_before(keep_from);
    }

    fn render(&mut self, n: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(n * 2);
        let step = 1.0 / (self.cfg.crossfade * RATE as f32).max(1.0);
        for _ in 0..n {
            let dab = if self.dab_aligned || self.offset.is_none() { self.dab.get_f(self.pos) } else { None };
            let fm = self.offset.and_then(|o| self.fm.get_f(self.pos + o)).map(|f| self.fm_frame(f));
            // fades only between available streams
            let goal = match self.target {
                Source::Fm if fm.is_some() => 1.0,
                Source::Dab if dab.is_some() => 0.0,
                _ => self.fade,
            };
            if self.fade < goal {
                self.fade = (self.fade + step).min(goal);
            } else if self.fade > goal {
                self.fade = (self.fade - step).max(goal);
            }
            self.ab_fm = self.ab && dab.is_some() && fm.is_some();
            let frame = match (dab, fm) {
                (Some(d), Some(f)) if self.ab => ((d.0 + d.1) * 0.5, (f.0 + f.1) * 0.5),
                (Some(d), Some(f)) => {
                    let x = self.fade;
                    (d.0 * (1.0 - x) + f.0 * x, d.1 * (1.0 - x) + f.1 * x)
                }
                (Some(d), None) => {
                    if self.fade > 0.0 {
                        self.fade = 0.0;
                    }
                    d
                }
                (None, Some(f)) => {
                    if self.fade < 1.0 {
                        // no DAB left to fade from: short ramp from the last output
                        if self.fade < 0.5 {
                            self.events.push("DAB ran out before the fade, switched to FM".into());
                        }
                        self.fade = 1.0;
                    }
                    f
                }
                (None, None) => (self.last_out.0 * 0.99, self.last_out.1 * 0.99),
            };
            self.last_out = frame;
            out.push(frame.0);
            out.push(frame.1);
            self.pos += self.speed;
            if let Some(o) = self.offset.as_mut() {
                // the FM tempo
                *o += (self.fm_rate - 1.0) * self.speed;
            }
        }
        self.correct();
        out
    }

    /// The pending correction of the offset, made where it is not heard: FM
    /// moves while DAB is heard, DAB while FM is heard (the FM heard goes on
    /// as it is, the DAB look-ahead changes); during a crossfade it waits.
    fn correct(&mut self) {
        let e = self.pending;
        if e == 0.0 || (self.fade > 0.0 && self.fade < 1.0) {
            return;
        }
        if let Some(o) = self.offset.as_mut() {
            *o += e;
            if self.fade >= 1.0 {
                self.pos -= e;
                self.la += e;
            }
        }
        self.pending = 0.0;
    }

    /// Correlate the DAB and FM audio; updates the offset when the result is clear.
    pub fn measure(&mut self, now: Instant) -> Option<Measurement> {
        if !self.fm_valid || !self.started {
            return None;
        }
        // Normally FM comes first: the newest DAB is matched (quick after
        // tuning). An FM that comes later has no match there yet: an older
        // stretch of DAB is matched when FM was late or nothing matched.
        let late = self.median_ahead().is_some_and(|a| a < 0.0);
        let backs = if late { [BEHIND_S, 0.0] } else { [0.0, BEHIND_S] };
        let mut best: Option<Corr> = None;
        for b in backs {
            if let Some(c) = self.correlate_near(now, b, self.fm_rate) {
                let ok = self.acceptable(&c);
                if ok || best.as_ref().is_none_or(|x| c.confidence > x.confidence) {
                    best = Some(c);
                }
                if ok {
                    break;
                }
            }
        }
        // Nothing clear while linked, and no hint from the window's halves
        // (too far off for them): the FM tempo may have changed a lot (a
        // delay changed by slowing the FM down): other tempos.
        if self.offset.is_some() && !best.as_ref().is_some_and(|c| self.acceptable(c)) {
            let b = backs[0];
            // (the FM's own tempo first: a delay change is over)
            let rates = [0.0, 0.005, -0.005, 0.01, -0.01, 0.02, -0.02].map(|d| if d == 0.0 { 1.0 } else { self.fm_rate * (1.0 + d) });
            for r in rates.into_iter().filter(|r| (r - self.fm_rate).abs() > 1e-6) {
                if let Some(c) = self.correlate_near(now, b, r) {
                    if self.acceptable(&c) && best.as_ref().is_none_or(|x| !self.acceptable(x) || c.confidence > x.confidence) {
                        best = Some(c);
                    }
                }
            }
        }
        let c = best?;
        let accepted = self.acceptable(&c);
        let polarity = if c.bands.high < 0.0 { -1.0 } else { 1.0 };
        let m = Measurement {
            delay_ms: c.delay_s * 1000.0,
            confidence: c.confidence,
            gain_db: c.gain_db,
            polarity,
            correlation: c.bands.mid,
            correlation_high: c.bands.high,
            correlation_low: c.bands.low,
            correlation_side: c.bands.side,
            curve: c.curve.iter().map(|v| v * polarity).collect(),
            overview: c.overview.iter().map(|v| v * polarity).collect(),
            accepted,
        };
        if accepted {
            self.recent.push_back(m.delay_ms);
            if self.recent.len() > 9 {
                self.recent.pop_front();
            }
            self.vote_polarity(c.bands.high, c.bands.side);
            self.apply(&c);
            self.adjust_lookahead();
        }
        Some(m)
    }

    /// A correlation at tempo `rate`, or near it: at a tempo that is not the
    /// FM's the window's halves tell the FM's tempo, tried in up to three steps.
    fn correlate_near(&self, now: Instant, back: f32, rate: f64) -> Option<Corr> {
        let mut c = self.correlate(now, back, rate)?;
        for _ in 0..3 {
            match c.tempo_hint {
                Some(r) if !self.acceptable(&c) => c = self.correlate(now, back, r)?,
                _ => break,
            }
        }
        Some(c)
    }

    fn acceptable(&self, c: &Corr) -> bool {
        c.refined && c.confidence >= 2.5 && c.delay_s >= -BEHIND_S && c.delay_s <= self.cfg.max_delay
    }

    /// One correlation of DAB from `back` seconds before its newest second
    /// with the FM around it (FM up to max_delay before and `back` + 1 s
    /// after), the FM taken at tempo `rate` (resampled to the DAB tempo).
    fn correlate(&self, now: Instant, back: f32, rate: f64) -> Option<Corr> {
        let w = (WINDOW_S * DEC_RATE as f32) as i64;
        let win_end = self.dab_dec.end() - DEC_RATE as i64 - (back * DEC_RATE as f32) as i64;
        let win_start = win_end - w;
        let x = self.dab_dec.slice(win_start, win_end)?;
        let seg_end = self.fm_dec.end();
        let seg_start = (seg_end - ((self.cfg.max_delay + back + WINDOW_S + 1.5) * DEC_RATE as f32) as i64).max(self.fm_dec.start);
        let raw = self.fm_dec.slice(seg_start, seg_end)?;
        // the FM segment at the DAB tempo: sample k is FM at seg_start + k * rate
        let y: Vec<f32> = if (rate - 1.0).abs() < 1e-6 {
            raw
        } else {
            let len = ((raw.len() - 1) as f64 / rate) as usize;
            (0..len)
                .map(|k| {
                    let p = k as f64 * rate;
                    let i = p as usize;
                    let t = (p - i as f64) as f32;
                    raw[i] * (1.0 - t) + raw[(i + 1).min(raw.len() - 1)] * t
                })
                .collect()
        };
        if y.len() < x.len() + DEC_RATE / 2 {
            return None;
        }
        if energy(&x) < 1e-6 || energy(&y) < 1e-6 {
            return None;
        }
        // the window's halves each against the FM: together the window's
        // correlation, and both must peak at the same lag (at a tempo that is
        // not the FM's they part, the lag moving across the window)
        let h = x.len() / 2;
        let rs = gcc_phat(&[&x[..h], &x[h..]], &y);
        let r: Vec<f32> = (0..=y.len() - x.len()).map(|k| 0.5 * (rs[0][k] + rs[1][k + h])).collect();
        // an inverted FM gives a negative peak
        let (best, peak) = r.iter().map(|v| v.abs()).enumerate().fold((0, f32::MIN), |a, (i, v)| if v > a.1 { (i, v) } else { a });
        let guard = DEC_RATE / 100;
        let second = r.iter().enumerate().filter(|(i, _)| i.abs_diff(best) > guard).map(|(_, v)| v.abs()).fold(f32::MIN, f32::max);
        let confidence = if second > 0.0 { peak / second } else { 0.0 };
        // each half's peak near the window's (up to 100 ms away) and how clear it is
        let half = |k: usize, shift: usize| -> (i64, f32) {
            let rk = &rs[k];
            let (lo, hi) = ((best + shift).saturating_sub(DEC_RATE / 10), (best + shift + DEC_RATE / 10).min(rk.len() - 1));
            let p = (lo..=hi).max_by(|&a, &b| rk[a].abs().total_cmp(&rk[b].abs())).unwrap_or(best + shift);
            let second = rk.iter().enumerate().filter(|(i, _)| i.abs_diff(p) > guard).map(|(_, v)| v.abs()).fold(f32::MIN, f32::max);
            (p as i64 - shift as i64, if second > 0.0 { rk[p].abs() / second } else { 0.0 })
        };
        let ((b1, c1), (b2, c2)) = (half(0, 0), half(1, h));
        // (real audio puts the halves' peaks a few samples apart: 8 samples
        // in 2 s are 0.05 %; finer the refinement below decides)
        let halves_agree = (b1 - best as i64).abs() <= 8 && (b2 - best as i64).abs() <= 8;
        // halves apart, each clear: the lag moves with the FM's tempo
        let tempo_hint = (!halves_agree && (b2 - b1).abs() > 8 && c1 >= 1.5 && c2 >= 1.5)
            .then(|| rate * (1.0 + (b2 - b1) as f64 / h as f64))
            .filter(|r| (0.97..=1.03).contains(r));
        // offset at the DAB window start, at 48 kHz
        let i0 = (win_start * DEC as i64) as f64;
        let o0 = (seg_start as f64 + best as f64 * rate - win_start as f64) * DEC as f64;
        // refined at 48 kHz at a quarter and at three quarters of the window:
        // both must find their peak and agree with the tempo (at a tempo that
        // is not the FM's the coarse peak is smeared and both are off)
        let q = (w * DEC as i64) as f64 / 4.0;
        let at = |k: f64| self.refine(i0 + k * q, o0 + (rate - 1.0) * k * q);
        let pair = match (halves_agree, at(1.0), at(3.0)) {
            // (clock drift of 100 ppm moves the offset by 10 frames in 2 s)
            (true, Some(a), Some(b)) if ((b.0 - a.0) - (rate - 1.0) * 2.0 * q).abs() <= 12.0 => Some((a, b)),
            _ => None,
        };
        let refined = pair.is_some();
        // the FM's tempo between both points, and the offset at the newest
        // DAB that the FM has too (the window's is seconds old: a tempo a
        // little off would put it off by as many frames)
        let local_rate = pair.map_or(rate, |(a, b)| 1.0 + (b.1 - a.1) / (2.0 * q));
        let (i, o, bands) = match pair {
            Some((a, b)) => {
                let (ic, oc) = (i0 + 2.0 * q, (a.0 + b.0) / 2.0);
                let half = (RATE / 8) as f64;
                let newest = (self.dab.end() as f64 - RATE as f64 / 10.0).min(self.fm.end() as f64 - oc - 16.0) - half;
                let n = (newest > ic).then(|| self.refine(newest, oc + (local_rate - 1.0) * (newest - ic))).flatten();
                let (i, o) = n.map_or((ic, oc), |n| (newest.round(), n.0));
                let avg = |f: fn(&Bands) -> f32| (f(&a.2) + f(&b.2)) / 2.0;
                (i, o, Bands { mid: avg(|b| b.mid), high: avg(|b| b.high), low: avg(|b| b.low), side: avg(|b| b.side) })
            }
            None => {
                (i0 + 2.0 * q, o0 + (rate - 1.0) * 2.0 * q, Bands { mid: r[best].signum(), high: r[best].signum(), ..Default::default() })
            }
        };
        // levels of the matched windows
        let off = best as i64 + seg_start - win_start;
        let fm_match = self.fm_dec.slice(win_start + off, win_end + off);
        let gain_db = fm_match.map(|f| 10.0 * (energy(&x) / energy(&f).max(1e-12)).log10()).unwrap_or(0.0);
        let o_end = o + (rate - 1.0) * (self.dab.end() as f64 - i);
        let delay_s = ((self.fm.end() as f64 - (self.dab.end() as f64 + o_end)) / RATE as f64) as f32
            + self.last_dab.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        let lo = best.saturating_sub(200);
        let curve: Vec<f32> = r[lo..(best + 200).min(r.len())].to_vec();
        let stride = (r.len() / 300).max(1);
        let sign = r[best].signum();
        let overview: Vec<f32> = r.chunks(stride).map(|c| c.iter().map(|v| v * sign).fold(f32::MIN, f32::max) * sign).collect();
        Some(Corr { i, o, rate, local_rate, confidence, bands, gain_db, delay_s, curve, overview, refined, tempo_hint })
    }

    /// The offset at 48 kHz within 12 frames of `guess`, on a quarter second
    /// of DAB around frame `at`: in frames and between frames (for the tempo),
    /// with the correlation there in bands; none when the best is at the edge
    /// (the guess was off).
    fn refine(&self, at: f64, guess: f64) -> Option<(f64, f64, Bands)> {
        let n = (RATE / 4) as i64;
        let start = at.round() as i64 - n / 2;
        let end = start + n;
        let guess = guess.round() as i64;
        // the lag from the mid signal's first difference: sharp, and not
        // dominated by the bass (which an FM processor may have turned)
        let mid = |f: &Frames, i: i64| Some(f.get(i).map(|(l, r)| l + r)? - f.get(i - 1).map(|(l, r)| l + r)?);
        let coef = |x: &dyn Fn(&Frames, i64) -> Option<f32>, o: i64| -> Option<f32> {
            let (mut acc, mut e1, mut e2) = (0f64, 0f64, 0f64);
            for i in start..end {
                let a = x(&self.dab, i)? as f64;
                let b = x(&self.fm, i + o)? as f64;
                acc += a * b;
                e1 += a * a;
                e2 += b * b;
            }
            Some((acc / (e1 * e2).max(1e-12).sqrt()) as f32)
        };
        const R: usize = 12;
        let mut c = [0f32; 2 * R + 1];
        for (k, v) in c.iter_mut().enumerate() {
            *v = coef(&mid, guess + k as i64 - R as i64)?;
        }
        let k = (0..c.len()).max_by(|&a, &b| c[a].abs().total_cmp(&c[b].abs()))?;
        if k == 0 || k == 2 * R {
            return None;
        }
        let o = guess + k as i64 - R as i64;
        let sign = c[k].signum();
        let (a, b, d) = (c[k - 1] * sign, c[k] * sign, c[k + 1] * sign);
        let curve = a - 2.0 * b + d;
        let frac = if curve < 0.0 { (0.5 * (a - d) / curve).clamp(-0.5, 0.5) as f64 } else { 0.0 };
        Some((o as f64, o as f64 + frac, self.bands(start, end, o)?))
    }

    /// Signed correlation coefficients of DAB frames `[start, end)` and the FM
    /// frames `o` later: of the mid signal (L+R) as a whole, above about
    /// 1 kHz and below 250 Hz, and of the side signal (L-R) above 1 kHz. The
    /// polarity is told by the upper band: an FM processor's phase rotator
    /// turns the bass by up to 180°, which as a whole looks inverted.
    fn bands(&self, start: i64, end: i64, o: i64) -> Option<Bands> {
        let (mut dm, mut fm, mut ds, mut fs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in start..end {
            let (d, f) = (self.dab.get(i)?, self.fm.get(i + o)?);
            dm.push(d.0 + d.1);
            fm.push(f.0 + f.1);
            ds.push(d.0 - d.1);
            fs.push(f.0 - f.1);
        }
        let coef = |a: &[f32], b: &[f32]| {
            let (mut acc, mut e1, mut e2) = (0f64, 0f64, 0f64);
            for (&x, &y) in a.iter().zip(b) {
                acc += x as f64 * y as f64;
                e1 += x as f64 * x as f64;
                e2 += y as f64 * y as f64;
            }
            (acc / (e1 * e2).max(1e-12).sqrt()) as f32
        };
        // first difference: +6 dB an octave, the band around 1 to 10 kHz counts
        let high = |x: &[f32]| x.windows(2).map(|w| w[1] - w[0]).collect::<Vec<f32>>();
        let low = |x: &[f32]| {
            let a = 1.0 - (-2.0 * std::f32::consts::PI * 250.0 / RATE as f32).exp();
            let mut s = 0.0;
            x.iter()
                .map(|v| {
                    s += a * (v - s);
                    s
                })
                .collect::<Vec<f32>>()
        };
        Some(Bands {
            mid: coef(&dm, &fm),
            high: coef(&high(&dm), &high(&fm)),
            low: coef(&low(&dm), &low(&fm)),
            side: coef(&high(&ds), &high(&fs)),
        })
    }

    /// The polarity of the FM audio (its mid signal above about 1 kHz)
    /// changes after three clear measurements asking for it, weak ones in
    /// between not counting; the first measurement sets it. The side signal
    /// (L-R) is only watched: inverted, the FM has its channels swapped, which
    /// is reported and not corrected (the tuner or the FM chain is wrong).
    fn vote_polarity(&mut self, mid: f32, side: f32) {
        let first = self.offset.is_none();
        let want = |c: f32, cur: f32, votes: &mut u32, first: bool| -> Option<f32> {
            if c.abs() < 0.25 {
                return None;
            }
            let sign = if c < 0.0 { -1.0 } else { 1.0 };
            if sign == cur {
                *votes = 0;
                return None;
            }
            *votes += 1;
            (*votes >= 3 || first).then_some(sign)
        };
        let (pm, ps) = (self.polarity, self.side_polarity);
        // (the side signal relative to the corrected mid)
        if let Some(p) = want(mid, self.polarity, &mut self.pol_votes.0, first) {
            self.polarity = p;
            self.pol_votes.0 = 0;
        }
        if let Some(p) = want(side * self.polarity, self.side_polarity, &mut self.pol_votes.1, first) {
            self.side_polarity = p;
            self.pol_votes.1 = 0;
        }
        if pm != self.polarity {
            self.events.push(if self.polarity < 0.0 {
                "FM audio inverted against DAB, corrected".into()
            } else {
                "FM audio polarity normal".into()
            });
        }
        if ps != self.side_polarity {
            self.events.push(if self.side_polarity < 0.0 {
                "FM channels swapped against DAB (left is right): not corrected, the tuner or the FM chain is wrong".into()
            } else {
                "FM channels as on DAB".into()
            });
        }
    }

    /// FM frame with its polarity corrected (both channels; swapped channels
    /// stay as they are).
    fn fm_frame(&self, f: (f32, f32)) -> (f32, f32) {
        let k = self.polarity * self.gain;
        (f.0 * k, f.1 * k)
    }

    /// Loudness matching on or off (Settings > FM and linking).
    pub fn set_match_level(&mut self, on: bool) {
        self.cfg.match_level = on;
        if !on {
            self.gain = 1.0;
        }
    }

    fn apply(&mut self, c: &Corr) {
        // the TEF delivers about -22 dBFS, DAB is often 10 dB louder
        if self.cfg.match_level {
            // averaged (one measurement follows the music's dynamics), at most
            // 1 dB a step
            let target = c.gain_db.clamp(-18.0, 18.0);
            let cur = 20.0 * self.gain.max(1e-6).log10();
            let db = if self.offset.is_some() { cur + ((target - cur) * 0.1).clamp(-1.0, 1.0) } else { target };
            self.gain = 10f32.powf(db / 20.0);
        }
        if (c.rate - self.fm_rate).abs() > 1e-7 {
            // a tempo search found the FM's tempo
            self.fm_rate = c.local_rate.clamp(0.97, 1.03);
            if (c.rate - 1.0).abs() > 1e-4 || (self.fm_rate - 1.0).abs() > 1e-4 {
                self.events.push(format!("FM tempo {:+.2} % against DAB", (self.fm_rate - 1.0) * 100.0));
            }
            self.tempo.clear();
        }
        let ms = |d: f64| d * 1000.0 / RATE as f64;
        match self.offset {
            None => {
                self.offset = Some(c.o + (self.fm_rate - 1.0) * (self.pos - c.i));
                self.dab_aligned = true;
                self.events.push(format!("aligned: FM {:+.0} ms ahead", c.delay_s * 1000.0));
            }
            Some(cur) if !self.dab_aligned => {
                // DAB placed anew after a dropout: shift it onto the FM timeline
                // (FM may be heard, it stays continuous)
                let d = (c.o - self.offset_at(cur, c.i)).round() as i64;
                self.dab.start += d;
                self.dab_dec.start += (d as f64 / DEC as f64).round() as i64;
                // the look-ahead in force moves with it: as many frames are owed
                self.la += d as f64;
                self.dab_aligned = true;
                self.tempo.clear();
                self.events.push(format!("DAB aligned again ({:+.1} ms)", ms(d as f64)));
            }
            Some(cur) => {
                let e = c.o - self.offset_at(cur, c.i);
                if e.abs() > 0.01 * RATE as f64 {
                    // a step, or a tempo not followed: the history does not
                    // tell the tempo any more
                    self.tempo.clear();
                }
                self.tempo.push_back((c.i, c.o));
                while self.tempo.front().is_some_and(|t| c.i - t.0 > TEMPO_SPAN_S * RATE as f64) {
                    self.tempo.pop_front();
                }
                self.estimate_tempo();
                if self.fade > 0.0 && e.abs() > 0.05 * RATE as f64 && !self.big.is_some_and(|b| (b - e).abs() < 0.01 * RATE as f64) {
                    // FM heard: a large step needs a second measurement agreeing
                    self.big = Some(e);
                } else {
                    self.big = None;
                    self.pending = e;
                    self.correct();
                }
                if e.abs() > 0.01 * RATE as f64 {
                    self.events.push(format!("offset changed by {:+.1} ms", ms(e)));
                }
            }
        }
    }

    /// The FM tempo from the offsets of the last measurements: the median of
    /// the slopes between pairs at least 3 s apart (clock drift, or a delay
    /// being changed by slowing the FM down).
    fn estimate_tempo(&mut self) {
        let pts: Vec<(f64, f64)> = self.tempo.iter().copied().collect();
        if pts.len() < 3 || pts[pts.len() - 1].0 - pts[0].0 < 6.0 * RATE as f64 {
            return;
        }
        let mut slopes = Vec::new();
        for (k, a) in pts.iter().enumerate() {
            for b in &pts[k + 1..] {
                if b.0 - a.0 > 3.0 * RATE as f64 {
                    slopes.push((b.1 - a.1) / (b.0 - a.0));
                }
            }
        }
        if slopes.is_empty() {
            return;
        }
        slopes.sort_by(|a, b| a.total_cmp(b));
        self.fm_rate = (1.0 + slopes[slopes.len() / 2]).clamp(0.97, 1.03);
    }

    pub fn status(&self) -> LinkerStatus {
        LinkerStatus {
            source: self.started.then(|| self.source()),
            fade: self.fade,
            aligned: self.offset.is_some(),
            dab_aligned: self.dab_aligned,
            paced_by_fm: self.paced_by_fm,
            fm_only: self.started && !self.dab_aligned && self.last_dab.is_none(),
            dab_buffer_ms: ((self.dab.end() as f64 - self.pos).max(0.0) * 1000.0 / RATE as f64) as f32,
            fm_delay_ms: self.offset.map_or(0.0, |o| ((self.fm.end() as f64 - (self.pos + o)) * 1000.0 / RATE as f64) as f32),
            fm_ahead_ms: self.median_ahead(),
            dab_extra_ms: (self.extra * 1000.0 / RATE as f64) as f32,
            drift_ppm: ((self.fm_rate - 1.0) * 1e6) as f32,
            gain_db: 20.0 * self.gain.log10(),
            polarity: self.polarity,
            side_polarity: self.side_polarity,
            stalls: self.slips,
            ab: self.ab,
            ab_fm: self.ab && self.ab_fm,
        }
    }
}

fn energy(x: &[f32]) -> f32 {
    x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32
}

/// Generalised cross-correlation with phase transform of each `x` against
/// `y`: `r[t]` for `y[t..]` against `x`, `t` in `0..=y.len()-x.len()`.
fn gcc_phat(xs: &[&[f32]], y: &[f32]) -> Vec<Vec<f32>> {
    let n = (xs.iter().map(|x| x.len()).max().unwrap_or(0) + y.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let pad = |v: &[f32]| -> Vec<Complex32> {
        v.iter().map(|&s| Complex32::new(s, 0.0)).chain(std::iter::repeat(Complex32::new(0.0, 0.0))).take(n).collect()
    };
    let mut b = pad(y);
    fwd.process(&mut b);
    xs.iter()
        .map(|x| {
            let mut a = pad(x);
            fwd.process(&mut a);
            for (p, q) in a.iter_mut().zip(&b) {
                let c = p.conj() * q;
                let m = c.norm();
                *p = if m > 1e-12 { c / m } else { Complex32::new(0.0, 0.0) };
            }
            inv.process(&mut a);
            a[..=(y.len() - x.len())].iter().map(|c| c.re / n as f32).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    thread_local! {
        /// Polarity of the FM audio in the tests.
        static FM_SIGN: std::cell::RefCell<f32> = const { std::cell::RefCell::new(1.0) };
        /// FM with its channels swapped.
        static FM_SWAP: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
        /// A 90 Hz tone of this level in the programme (much bass).
        static BASS: std::cell::RefCell<f32> = const { std::cell::RefCell::new(0.0) };
        /// FM with its bass inverted, as by the phase rotator of an FM processor.
        static FM_BASS_FLIP: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
    }

    /// Programme audio: filtered noise with a changing envelope, stereo.
    fn programme(len: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let mut lp = 0.0f32;
        let mut out = Vec::with_capacity(len * 2);
        for i in 0..len {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let w = ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5;
            lp = lp * 0.6 + w * 0.4;
            let env = 0.3 + 0.25 * ((i as f32 / RATE as f32) * 2.1).sin().abs() + 0.2 * ((i as f32 / RATE as f32) * 0.37).sin();
            let bass = BASS.with(|b| *b.borrow()) * (2.0 * std::f32::consts::PI * 90.0 * i as f32 / RATE as f32).sin();
            let v = lp * env + bass;
            out.push(v);
            out.push(v * 0.8);
        }
        out
    }

    /// Feeds DAB in 120 ms bursts and FM in 10 ms blocks; FM leads by `lead`
    /// seconds (negative: comes later) and is quieter. `dab_gap` drops DAB for a while.
    fn run(lead: f32, secs: f32, dab_gap: Option<(f32, f32)>, decide: impl FnMut(&mut Linker, f32)) -> (Linker, Vec<f32>, Vec<f32>) {
        run_drift(lead, secs, dab_gap, 0.0, decide)
    }

    /// As `run`, the FM content advancing `ppm` faster than the DAB content.
    fn run_drift(
        lead: f32,
        secs: f32,
        dab_gap: Option<(f32, f32)>,
        ppm: f64,
        decide: impl FnMut(&mut Linker, f32),
    ) -> (Linker, Vec<f32>, Vec<f32>) {
        run_tempo(lead, secs, dab_gap, |_| ppm, decide)
    }

    /// As `run`, the FM content advancing `ppm(t)` faster than the DAB content
    /// (a tempo that changes: a delay changed by slowing the FM down).
    fn run_tempo(
        lead: f32,
        secs: f32,
        dab_gap: Option<(f32, f32)>,
        ppm: impl Fn(f32) -> f64,
        mut decide: impl FnMut(&mut Linker, f32),
    ) -> (Linker, Vec<f32>, Vec<f32>) {
        let total = ((secs + lead.abs() + 1.0) * RATE as f32) as usize;
        let prog = programme(total, 7);
        let lead_f = (lead.max(0.0) * RATE as f32) as usize;
        let lag_f = ((-lead).max(0.0) * RATE as f32) as usize;
        let mut l = Linker::new(LinkerConfig::default());
        l.set_fm_valid(true);
        let t0 = Instant::now();
        let mut out = Vec::new();
        let (mut dab_pos, mut fm_pos) = (lag_f, lead_f);
        let mut fm_x = lead_f as f64;
        let block = RATE / 100;
        let burst = RATE * 12 / 100;
        let mut step = 0usize;
        let mut bass_lp = [0f32; 2];
        let lp_a = 1.0 - (-2.0 * std::f32::consts::PI * 250.0 / RATE as f32).exp();
        while dab_pos + burst < total - lead_f && fm_pos + block + 1 < total && (fm_x as usize) + block + 2 < total {
            let t = step as f32 * 0.01;
            let now = t0 + Duration::from_secs_f32(t);
            // FM: 10 ms, quieter by 6 dB (inverted with a negative `ppm` sign
            // convention below), its clock off by `ppm`
            let mut f = Vec::with_capacity(block * 2);
            let rate = 1.0 + ppm(t) * 1e-6;
            for _ in 0..block {
                let x = fm_x;
                fm_x += rate;
                if (x as usize) + 2 >= total {
                    break;
                }
                let (i, fr) = (x.floor() as usize, (x - x.floor()) as f32);
                for ch in 0..2 {
                    let c = if FM_SWAP.with(|s| *s.borrow()) { 1 - ch } else { ch };
                    let mut v = prog[i * 2 + c] * (1.0 - fr) + prog[(i + 1) * 2 + c] * fr;
                    if FM_BASS_FLIP.with(|b| *b.borrow()) {
                        bass_lp[ch] += lp_a * (v - bass_lp[ch]);
                        v -= 2.0 * bass_lp[ch];
                    }
                    f.push(v * 0.5 * FM_SIGN.with(|s| *s.borrow()));
                }
            }
            out.extend(l.push_fm(&f, now));
            fm_pos += block;
            // DAB: a 120 ms burst every 120 ms, unless dropped
            if step % 12 == 0 {
                let dropped = dab_gap.is_some_and(|(a, b)| t >= a && t < b);
                if !dropped {
                    for au in prog[dab_pos * 2..(dab_pos + burst) * 2].chunks(burst / 3) {
                        out.extend(l.push_dab(au, now));
                    }
                }
                dab_pos += burst;
            }
            if step % 200 == 100 {
                l.measure(now);
            }
            decide(&mut l, t);
            step += 1;
        }
        (l, out, prog)
    }

    /// Largest jump between output samples (clicks).
    fn max_jump(out: &[f32]) -> f32 {
        out.chunks_exact(2).collect::<Vec<_>>().windows(2).map(|w| (w[1][0] - w[0][0]).abs()).fold(0.0, f32::max)
    }

    /// Correlation of the DAB audio with the FM audio where the linker has
    /// them together, just before the output (1: aligned; the FM of the tests
    /// is interpolated after a tempo change, so not sample for sample).
    fn alignment(l: &Linker) -> f32 {
        let o = l.offset.unwrap();
        let (mut acc, mut e1, mut e2) = (0f64, 0f64, 0f64);
        for k in 0..4000 {
            let p = (l.pos - 4000.0 + k as f64).round();
            let d = l.dab.get_f(p).unwrap().0 as f64;
            let f = l.fm_frame(l.fm.get_f(p + l.offset_at(o, p)).unwrap()).0 as f64;
            acc += d * f;
            e1 += d * d;
            e2 += f * f;
        }
        (acc / (e1 * e2).sqrt()) as f32
    }

    /// Offset of the output against the programme (in frames), by correlation.
    fn lag_of(out: &[f32], prog: &[f32], at: usize) -> i64 {
        let n = RATE / 5;
        let o: Vec<f32> = out[at * 2..(at + n) * 2].chunks(2).map(|c| c[0]).collect();
        let mut best = (0i64, f32::MIN);
        for lag in -(RATE as i64 / 2)..(RATE as i64 / 2) {
            let s = at as i64 - lag;
            if s < 0 || (s as usize + n) * 2 > prog.len() {
                continue;
            }
            let p = |k: usize| prog[(s as usize + k) * 2];
            let c: f32 = (0..n).step_by(4).map(|k| o[k] * p(k)).sum();
            if c > best.1 {
                best = (lag, c);
            }
        }
        best.0
    }

    #[test]
    fn finds_the_delay() {
        let (l, _, _) = run(2.2, 14.0, None, |_, _| {});
        assert!(l.aligned());
        let st = l.status();
        assert!((st.gain_db - 6.0).abs() < 1.0, "gain {}", st.gain_db);
        let mut l = l;
        let m = l.measure(Instant::now() + Duration::from_secs(20)).unwrap();
        assert!(m.confidence > 2.5);
    }

    #[test]
    fn seamless_switch_and_back() {
        // switch to FM at 12 s and back to DAB at 20 s
        let (l, out, prog) = run(1.7, 26.0, None, |l, t| {
            if (12.0..20.0).contains(&t) {
                l.want(Source::Fm);
            } else {
                l.want(Source::Dab);
            }
        });
        assert!(l.aligned());
        assert_eq!(l.status().stalls, 0, "DAB was never missing");
        // the content timeline does not jump: same lag before, during and after FM
        let a = lag_of(&out, &prog, RATE * 10);
        let b = lag_of(&out, &prog, RATE * 15);
        let c = lag_of(&out, &prog, RATE * 23);
        assert!((a - b).abs() <= 2 && (a - c).abs() <= 2, "lags {a} {b} {c}");
        assert!(max_jump(&out[RATE * 2 * 6..]) < 0.5, "click {}", max_jump(&out[RATE * 2 * 6..]));
    }

    #[test]
    fn late_fm_is_found_and_dab_waits_for_it() {
        // FM 1.5 s later than DAB (some broadcasters delay FM that much):
        // found, DAB waits that much longer, and FM takes over at 22 s
        // without repeating what was heard
        // (DAB slows down by 2 % to wait 1.4 s more: ~70 s)
        let (l, out, prog) = run(-1.5, 110.0, None, |l, t| {
            l.want(if t >= 95.0 { Source::Fm } else { Source::Dab });
        });
        assert!(l.aligned());
        let st = l.status();
        let ahead = st.fm_ahead_ms.unwrap();
        assert!((ahead + 1500.0).abs() < 60.0, "FM ahead {ahead} ms");
        assert!(st.dab_extra_ms > 1000.0, "DAB waits {} ms more", st.dab_extra_ms);
        assert_eq!(l.source(), Source::Fm);
        // DAB, so the output, starts 1.5 s into the programme and fell behind
        // by what DAB waits more (it slowed down to get there)
        let behind = (RATE * 3 / 2) as i64 - (st.dab_extra_ms as f64 * RATE as f64 / 1000.0).round() as i64;
        let prog = &prog[behind as usize * 2..];
        // no pause on the way: the output never went silent
        let silent = out[RATE * 2 * 3..].chunks(RATE / 50 * 2).filter(|c| c.iter().all(|v| v.abs() < 1e-6)).count();
        assert_eq!(silent, 0, "silent 20 ms blocks");
        // the same programme time just before and after FM took over: both
        // are read at the position of the output (the lag moved by the slower
        // start, it does not jump at the switch)
        let a = lag_of(&out, prog, RATE * 92);
        let b = lag_of(&out, prog, RATE * 100);
        assert!(a.abs() <= 4 && (a - b).abs() <= 4, "lags {a} {b}");
        assert!(max_jump(&out[RATE * 2 * 90..]) < 0.5, "click {}", max_jump(&out[RATE * 2 * 90..]));
    }

    #[test]
    fn inverted_fm_is_corrected() {
        FM_SIGN.with(|s| *s.borrow_mut() = -1.0);
        let (l, out, prog) = run(2.0, 22.0, None, |l, t| {
            l.want(if (12.0..18.0).contains(&t) { Source::Fm } else { Source::Dab });
        });
        FM_SIGN.with(|s| *s.borrow_mut() = 1.0);
        assert!(l.aligned());
        assert_eq!(l.status().polarity, -1.0);
        // FM heard from 12 s to 18 s: the same as the programme, not inverted
        let err = (14 * RATE..16 * RATE).map(|i| (out[i * 2] - prog[i * 2]).abs()).fold(0.0f32, f32::max);
        assert!(err < 0.01, "error {err}");
        assert!(max_jump(&out[RATE * 2 * 6..]) < 0.5);
    }

    #[test]
    fn swapped_fm_channels_are_reported_and_left_alone() {
        // FM left = -DAB right, FM right = -DAB left: the inversion is
        // corrected, the swapped channels are only reported
        FM_SIGN.with(|s| *s.borrow_mut() = -1.0);
        FM_SWAP.with(|s| *s.borrow_mut() = true);
        let (l, out, prog) = run(2.0, 22.0, None, |l, t| {
            l.want(if (12.0..18.0).contains(&t) { Source::Fm } else { Source::Dab });
        });
        FM_SIGN.with(|s| *s.borrow_mut() = 1.0);
        FM_SWAP.with(|s| *s.borrow_mut() = false);
        let st = l.status();
        assert_eq!((st.polarity, st.side_polarity), (-1.0, -1.0));
        // FM heard from 12 s to 18 s: not inverted, the channels still swapped
        let err = (14 * RATE..16 * RATE)
            .map(|i| (out[i * 2] - prog[i * 2 + 1]).abs().max((out[i * 2 + 1] - prog[i * 2]).abs()))
            .fold(0.0f32, f32::max);
        assert!(err < 0.01, "error {err}");
    }

    #[test]
    fn bass_turned_by_an_fm_processor_is_not_taken_for_an_inverted_fm() {
        // an FM processor's phase rotator turns the bass (here: inverts it
        // below about 250 Hz) and leaves the rest: with much bass the audio as
        // a whole correlates negatively, yet the FM is not inverted
        BASS.with(|b| *b.borrow_mut() = 1.0);
        FM_BASS_FLIP.with(|b| *b.borrow_mut() = true);
        let (mut l, _, _) = run(2.0, 16.0, None, |_, _| {});
        let m = l.measure(Instant::now() + Duration::from_secs(20));
        BASS.with(|b| *b.borrow_mut() = 0.0);
        FM_BASS_FLIP.with(|b| *b.borrow_mut() = false);
        assert!(l.aligned());
        let m = m.unwrap();
        assert!(
            m.correlation < 0.0 && m.correlation_low < -0.5 && m.correlation_high > 0.5,
            "{} {} {}",
            m.correlation,
            m.correlation_low,
            m.correlation_high
        );
        assert_eq!((l.status().polarity, l.status().side_polarity), (1.0, 1.0));
    }

    #[test]
    fn follows_clock_drift() {
        // FM 60 ppm fast, heard from 40 s to 50 s: while FM is heard the
        // programme follows its clock (0.6 ms in 10 s), DAB stays aligned to it
        // and takes over again without a click
        let (l, out, prog) = run_drift(1.9, 58.0, None, 60.0, |l, t| {
            l.want(if (40.0..50.0).contains(&t) { Source::Fm } else { Source::Dab });
        });
        assert!(l.aligned());
        assert_eq!(l.source(), Source::Dab);
        let a = lag_of(&out, &prog, RATE * 30);
        let b = lag_of(&out, &prog, RATE * 55);
        let drift = (60e-6 * 10.0 * RATE as f64) as i64;
        assert!(a.abs() <= 2 && (b - a).abs() <= drift + 4, "lags {a} {b}");
        assert!(max_jump(&out[RATE * 2 * 6..]) < 0.5, "click {}", max_jump(&out[RATE * 2 * 6..]));
    }

    #[test]
    fn follows_a_delay_being_changed_by_slowing_the_fm() {
        // the broadcaster raises the FM delay by 50 ms by slowing it down by
        // 0.5 % from 20 s to 30 s while FM is heard (16 s to 44 s): the FM
        // heard goes on as it comes, DAB follows it and takes over
        // seamlessly, then catches up the 50 ms by playing a little faster
        let mut settled = (1.0f32, 0.0f32);
        let (l, out, prog) = run_tempo(
            2.0,
            60.0,
            None,
            |t| if (20.0..30.0).contains(&t) { -5000.0 } else { 0.0 },
            |l, t| {
                l.want(if (16.0..44.0).contains(&t) { Source::Fm } else { Source::Dab });
                // DAB, not heard, follows the FM heard: exactly once the tempo has
                // settled (a large step waits for a second measurement agreeing)
                if (42.0..43.9).contains(&t) && alignment(l) < settled.0 {
                    settled = (alignment(l), t);
                }
            },
        );
        assert!(settled.0 > 0.95, "DAB off the FM: correlation {} at {} s", settled.0, settled.1);
        assert!(l.aligned());
        assert_eq!(l.source(), Source::Dab);
        assert!(max_jump(&out[RATE * 2 * 14..]) < 0.5, "click {}", max_jump(&out[RATE * 2 * 14..]));
        let a = lag_of(&out, &prog, RATE * 12);
        // the FM heard: its content came 50 ms later
        let b = lag_of(&out, &prog, RATE * 40);
        assert!((b - a - 2400).abs() <= 48, "lags {a} {b} (frames)");
        // and DAB, after taking over, is back to its look-ahead
        let e = lag_of(&out, &prog, RATE * 56);
        assert!((e - a).abs() <= 4, "lags {a} {e}");
        assert!((l.status().dab_buffer_ms - 400.0).abs() < 1.0);
    }

    #[test]
    fn fm_plays_when_dab_has_no_audio() {
        // DAB without audio for the first 12 s (an encoder sending nothing
        // valid), FM wanted: FM plays as it comes; DAB coming later is aligned
        let (l, out, _) = run(1.0, 30.0, Some((0.0, 12.0)), |l, _| l.want(Source::Fm));
        let silent = out[RATE * 2 * 2..RATE * 2 * 12].chunks(RATE / 50 * 2).filter(|c| c.iter().all(|v| v.abs() < 1e-6)).count();
        assert_eq!(silent, 0, "silent 20 ms blocks while DAB had no audio");
        assert!(l.aligned(), "DAB aligned once it has audio");
        assert!(!l.status().fm_only);
        assert!(max_jump(&out[RATE * 2 * 2..]) < 0.5, "click {}", max_jump(&out[RATE * 2 * 2..]));
    }

    #[test]
    fn dab_dropout_is_bridged_by_fm() {
        // DAB missing from 14 s to 18 s: the output continues from FM, aligned
        let (l, out, prog) = run(2.5, 30.0, Some((14.0, 18.0)), |l, t| {
            if t > 24.0 && l.aligned() {
                l.want(Source::Dab);
            }
        });
        let err = |from: f32, to: f32| {
            let (a, b) = ((from * RATE as f32) as usize, (to * RATE as f32) as usize);
            (a..b).map(|i| (out[i * 2] - prog[i * 2]).abs()).fold(0.0f32, f32::max)
        };
        // the gap is bridged by the level matched FM audio, on the same timeline
        for (a, b) in [(10.0, 12.0), (15.5, 17.5), (26.0, 28.0)] {
            assert!(err(a, b) < 0.01, "error {} from {a} s", err(a, b));
        }
        let a = lag_of(&out, &prog, RATE * 12);
        let b = lag_of(&out, &prog, RATE * 16);
        let c = lag_of(&out, &prog, RATE * 27);
        assert!((a - b).abs() <= 2, "lags {a} {b}");
        assert!((a - c).abs() <= 2, "lags {a} {c}");
        assert!(l.aligned());
        assert_eq!(l.source(), Source::Dab);
        assert!(max_jump(&out[RATE * 2 * 6..]) < 0.5);
    }
}
