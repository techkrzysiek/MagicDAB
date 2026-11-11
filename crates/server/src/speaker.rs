//! The program's own sound output. The Android app, the Windows window and a
//! browser on this computer leave the station's sound to the program: the
//! decoded PCM goes straight to the sound card instead of being encoded, sent
//! over the WebSocket, decoded and resampled by the page (on a phone the
//! page's audio was the biggest single cost, about a third of a core).
//!
//! A thread of its own plays the buffer on the default output (AAudio on
//! Android, waveOut on Windows, ALSA on Linux), opened while someone listens.
//! The buffer is steered to its target the way the page's player does it
//! (web/src/audio/player.ts): a hair faster or slower, at most 0.3 %, through
//! a Kaiser-windowed sinc; the loudness meter is the page's too.

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(target_os = "android")]
mod aaudio;
#[cfg(target_os = "linux")]
mod alsa;
#[cfg(windows)]
mod waveout;

const RATE: usize = 48000;
/// Frames written to the output at once (20 ms).
const BLOCK: usize = 960;
/// An output that failed is tried again after this.
const RETRY: Duration = Duration::from_secs(5);
/// Nothing to play for this long (no signal): the output closes until sound
/// comes again (the phone's battery).
const IDLE_CLOSE: Duration = Duration::from_secs(5);

/// A sound output: interleaved stereo, 48 kHz, written blocking.
trait Output: Send {
    /// Plays the frames; blocks while the output has no room for them.
    fn write(&mut self, pcm: &[i16]) -> Result<(), String>;
    /// Frames the output buffers itself (taken at once after it opens).
    fn buffered(&self) -> usize;
}

fn open() -> Result<Box<dyn Output>, String> {
    #[cfg(target_os = "android")]
    return aaudio::open();
    #[cfg(target_os = "linux")]
    return alsa::open();
    #[cfg(windows)]
    return waveout::open();
    #[allow(unreachable_code)]
    Err("no sound output on this system".into())
}

/// The system has a sound output the program can use (whether a device
/// plays is found when it is opened).
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        #[cfg(target_os = "android")]
        return aaudio::available();
        #[cfg(target_os = "linux")]
        return alsa::available();
        #[cfg(windows)]
        return true;
        #[allow(unreachable_code)]
        false
    })
}

/// Loudness as the page's meter has it (LUFS momentary, short-term,
/// integrated; peak of the last 100 ms and its maximum, dBFS). Silence is
/// `None` (JSON null).
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Loudness {
    pub m: Option<f64>,
    pub s: Option<f64>,
    pub i: Option<f64>,
    pub peak: Option<f64>,
    pub max: Option<f64>,
}

/// What the page shows of the sound (and logs of its dropouts).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// Seconds in the buffer and its target.
    pub buffer: f32,
    pub target: f32,
    pub playing: bool,
    pub underruns: u32,
    pub meter: Option<Loudness>,
    /// Why nothing plays (no output device).
    pub error: Option<String>,
}

struct State {
    player: Player,
    meter: Meter,
    loudness: Option<Loudness>,
    gain: f32,
    /// Someone listens: the output is open while this is on.
    on: bool,
    quit: bool,
    error: Option<String>,
    /// What the log says about gaps (Gaps): when sound came from the receiver.
    pushed_at: Option<Instant>,
    gaps: Gaps,
}

/// Gaps heard, told apart in the log (at most a line in 10 s each): the
/// receiver sent nothing for a while, the buffer ran dry, or the station's own
/// sound was silent.
#[derive(Default)]
struct Gaps {
    late: Option<Instant>,
    dry_seen: u32,
    dry: Option<Instant>,
    silent_ms: u32,
    silent: Option<Instant>,
}

fn quiet_for(t: &Option<Instant>) -> bool {
    t.is_none_or(|t| t.elapsed() > Duration::from_secs(10))
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub struct Speaker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Speaker {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                player: Player::new(),
                meter: Meter::new(),
                loudness: None,
                gain: 1.0,
                on: false,
                quit: false,
                error: None,
                pushed_at: None,
                gaps: Gaps::default(),
            }),
            wake: Condvar::new(),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("speaker".into()).spawn(move || run(&s)).ok();
        Speaker { shared, thread }
    }

    /// Decoded sound of the station played (48 kHz stereo, interleaved).
    pub fn push(&self, pcm: &[f32]) {
        let mut st = self.shared.state.lock();
        if st.on {
            // (the receiver sends a DAB+ superframe every 120 ms)
            let now = Instant::now();
            if let Some(gap) = st.pushed_at.replace(now).map(|t| now - t) {
                if gap > Duration::from_millis(400) && st.player.playing && quiet_for(&st.gaps.late) {
                    st.gaps.late = Some(now);
                    log::info!(
                        "sound output: nothing from the receiver for {} ms (buffer {} ms)",
                        gap.as_millis(),
                        st.player.count * 1000 / RATE
                    );
                }
            }
            st.player.write(pcm);
            drop(st);
            self.shared.wake.notify_one();
        }
    }

    /// Plays from now on, a station anew (what was in the buffer goes).
    pub fn start(&self) {
        let mut st = self.shared.state.lock();
        st.on = true;
        st.pushed_at = None;
        st.player.flush();
        st.meter.reset();
        st.loudness = None;
        drop(st);
        self.shared.wake.notify_one();
    }

    /// Nobody listens: the output closes.
    pub fn stop(&self) {
        let mut st = self.shared.state.lock();
        st.on = false;
        st.player.flush();
        st.loudness = None;
        drop(st);
        self.shared.wake.notify_one();
    }

    /// The volume (a factor, 0 is mute).
    pub fn set_gain(&self, gain: f32) {
        self.shared.state.lock().gain = gain.clamp(0.0, 1.0);
    }

    /// The buffer's target in seconds.
    pub fn set_target(&self, seconds: f32) {
        self.shared.state.lock().player.target = (seconds.clamp(0.05, 3.0) * RATE as f32) as usize;
    }

    pub fn report(&self) -> Report {
        let st = self.shared.state.lock();
        Report {
            buffer: st.player.count as f32 / RATE as f32,
            target: st.player.target as f32 / RATE as f32,
            playing: st.player.playing,
            underruns: st.player.underruns,
            meter: st.loudness,
            error: st.error.clone(),
        }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.shared.state.lock().quit = true;
        self.shared.wake.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(sh: &Shared) {
    let mut out: Option<Box<dyn Output>> = None;
    let mut failed_at: Option<Instant> = None;
    let mut idle_since: Option<Instant> = None;
    let mut block = vec![0f32; BLOCK * 2];
    let mut pcm = vec![0i16; BLOCK * 2];
    loop {
        let mut st = sh.state.lock();
        if st.quit {
            break;
        }
        if !st.on || (out.is_none() && st.player.count == 0) {
            if out.is_some() {
                drop(st);
                out = None;
                log::debug!("sound output closed");
                continue;
            }
            sh.wake.wait(&mut st);
            continue;
        }
        let Some(o) = out.as_mut() else {
            if let Some(t) = failed_at.filter(|t| t.elapsed() < RETRY) {
                // (the sound meanwhile goes: it would only pile up)
                st.player.flush();
                sh.wake.wait_for(&mut st, RETRY.saturating_sub(t.elapsed()));
                continue;
            }
            drop(st);
            // the output's own buffer filled with silence first: it would take
            // that much of the buffer at once, and the sound came short
            let r = open().and_then(|mut o| {
                let silence = vec![0i16; o.buffered().min(RATE) * 2];
                o.write(&silence)?;
                Ok(o)
            });
            let mut st = sh.state.lock();
            match r {
                Ok(o) => {
                    log::info!("sound output open");
                    out = Some(o);
                    failed_at = None;
                    idle_since = None;
                    st.error = None;
                }
                Err(e) => {
                    if st.error.as_deref() != Some(e.as_str()) {
                        log::warn!("sound output: {e}");
                    }
                    st.error = Some(e);
                    failed_at = Some(Instant::now());
                }
            }
            continue;
        };
        let playing = st.player.render(&mut block);
        let n = st.player.underruns;
        if n > st.gaps.dry_seen && quiet_for(&st.gaps.dry) {
            st.gaps.dry_seen = n;
            st.gaps.dry = Some(Instant::now());
            log::info!("sound output: the buffer ran dry ({n} time(s) since the start)");
        }
        if playing {
            // the station's own silence (below -80 dBFS) of a quarter of a second and more
            if block.iter().all(|v| v.abs() < 1e-4) {
                st.gaps.silent_ms += (BLOCK * 1000 / RATE) as u32;
            } else {
                if st.gaps.silent_ms >= 250 && quiet_for(&st.gaps.silent) {
                    st.gaps.silent = Some(Instant::now());
                    log::info!("sound output: the station's sound was silent for {} ms", st.gaps.silent_ms);
                }
                st.gaps.silent_ms = 0;
            }
        }
        if playing {
            idle_since = None;
            if let Some(l) = st.meter.feed(&block) {
                st.loudness = Some(l);
            }
        } else if idle_since.get_or_insert_with(Instant::now).elapsed() > IDLE_CLOSE && st.player.count == 0 {
            drop(st);
            out = None;
            log::debug!("sound output closed (nothing to play)");
            continue;
        }
        let gain = st.gain * 32767.0;
        drop(st);
        for (p, s) in pcm.iter_mut().zip(&block) {
            *p = (s * gain).clamp(-32768.0, 32767.0) as i16;
        }
        if let Err(e) = o.write(&pcm) {
            // (a device gone: headphones pulled out, another output chosen; opened again)
            log::info!("sound output: {e}");
            out = None;
        }
    }
}

// ---------------------------------------------------------------- the buffer

const HALF: usize = 16;
const TAPS: usize = 32;
const PHASES: usize = 256;
/// Frames needed after the read position.
const AHEAD: usize = HALF + 1;
const MAX_ADJUST: f64 = 0.003;
/// Fades: a start, a skip ahead.
const FADE_START: u32 = 2048;
const FADE_SKIP: u32 = 256;
const SIZE: usize = RATE * 8;

fn bessel_i0(x: f64) -> f64 {
    let (mut s, mut t) = (1.0, 1.0);
    for k in 1..40 {
        let q = x / (2 * k) as f64;
        t *= q * q;
        s += t;
    }
    s
}

/// Kaiser-windowed sinc, `PHASES + 1` phases of `TAPS` taps.
fn kernel() -> Vec<f32> {
    let beta = 8.0;
    let norm = bessel_i0(beta);
    let mut h = vec![0f32; (PHASES + 1) * TAPS];
    for p in 0..=PHASES {
        let mut v = [0f64; TAPS];
        for (t, x) in v.iter_mut().enumerate() {
            // the tap's distance from the read position
            let d = t as f64 - (HALF - 1) as f64 - p as f64 / PHASES as f64;
            let u = d / HALF as f64;
            let w = if u.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - u * u).sqrt()) / norm };
            let pd = std::f64::consts::PI * d;
            *x = if d == 0.0 { 1.0 } else { pd.sin() / pd } * w;
        }
        let sum: f64 = v.iter().sum();
        for t in 0..TAPS {
            h[p * TAPS + t] = (v[t] / sum) as f32;
        }
    }
    h
}

/// The page's AudioWorklet player (web/src/audio/player.ts) in Rust.
struct Player {
    l: Vec<f32>,
    r: Vec<f32>,
    h: Vec<f32>,
    /// Read position (a frame and the part of the next one), write position,
    /// frames from the one to the other.
    rd: usize,
    frac: f64,
    wr: usize,
    count: usize,
    /// Plays from a full buffer (unlike the page's player, which starts a new
    /// station with less: here a tenth of a second sooner is not worth the
    /// dropouts the output's bursts bring at the start, measured on a phone).
    playing: bool,
    target: usize,
    level: f64,
    ratio: f64,
    fade: u32,
    fade_len: u32,
    fade_out: u32,
    skip: usize,
    underruns: u32,
}

impl Player {
    fn new() -> Self {
        Player {
            l: vec![0.0; SIZE],
            r: vec![0.0; SIZE],
            h: kernel(),
            rd: 0,
            frac: 0.0,
            wr: 0,
            count: 0,
            playing: false,
            target: RATE * 3 / 10,
            level: 0.0,
            ratio: 1.0,
            fade: FADE_START,
            fade_len: FADE_START,
            fade_out: 0,
            skip: 0,
            underruns: 0,
        }
    }

    fn flush(&mut self) {
        self.rd = 0;
        self.wr = 0;
        self.count = 0;
        self.frac = 0.0;
        self.playing = false;
        self.fade_out = 0;
        self.skip = 0;
        self.l.fill(0.0);
        self.r.fill(0.0);
    }

    fn write(&mut self, d: &[f32]) {
        let n = d.len() / 2;
        if self.count + n > SIZE / 2 {
            // overflow: the oldest sound goes
            let drop = (self.count + n - SIZE / 2).min(self.count);
            self.rd = (self.rd + drop) % SIZE;
            self.count -= drop;
        }
        let mut w = self.wr;
        for f in d.chunks_exact(2).take(SIZE / 2) {
            self.l[w] = f[0];
            self.r[w] = f[1];
            w += 1;
            if w == SIZE {
                w = 0;
            }
        }
        self.wr = w;
        self.count += n.min(SIZE / 2);
    }

    /// A block of sound (interleaved); false (silence) while the buffer fills.
    fn render(&mut self, out: &mut [f32]) -> bool {
        let n = out.len() / 2;
        if !self.playing {
            if self.count < self.target + AHEAD {
                out.fill(0.0);
                return false;
            }
            self.playing = true;
            self.fade = 0;
            self.fade_len = FADE_START;
            self.level = self.count as f64;
            self.ratio = 1.0;
        }
        if (self.count as f64) < (n as f64 * self.ratio + self.frac).ceil() + AHEAD as f64 {
            self.playing = false;
            self.underruns += 1;
            out.fill(0.0);
            return false;
        }
        // the level over about a second (the sound comes in bursts of a DAB+
        // superframe, 120 ms) against the target
        self.level += (self.count as f64 - self.level) * (n as f64 / RATE as f64);
        let err = (self.level - self.target as f64) / RATE as f64;
        // far too much (all of a stall at once): skip ahead, faded out and in
        if err > 1.0 && self.fade_out == 0 && self.skip == 0 {
            self.skip = self.count.saturating_sub(self.target);
            self.fade_out = FADE_SKIP;
        }
        self.ratio = 1.0 + (err * 0.03).clamp(-MAX_ADJUST, MAX_ADJUST);
        let (mut rd, mut f, mut count) = (self.rd, self.frac, self.count);
        let mut c = [0f32; TAPS];
        let (mut xl, mut xr) = ([0f32; TAPS], [0f32; TAPS]);
        for frame in out.chunks_exact_mut(2) {
            let p = f * PHASES as f64;
            let pi = (p as usize).min(PHASES - 1);
            let a = (p - pi as f64) as f32;
            let h0 = &self.h[pi * TAPS..(pi + 1) * TAPS];
            let h1 = &self.h[(pi + 1) * TAPS..(pi + 2) * TAPS];
            for t in 0..TAPS {
                c[t] = h0[t] + (h1[t] - h0[t]) * a;
            }
            let j = (rd + SIZE - (HALF - 1)) % SIZE;
            let (l, r) = if j + TAPS <= SIZE {
                (&self.l[j..j + TAPS], &self.r[j..j + TAPS])
            } else {
                for t in 0..TAPS {
                    xl[t] = self.l[(j + t) % SIZE];
                    xr[t] = self.r[(j + t) % SIZE];
                }
                (&xl[..], &xr[..])
            };
            let (mut yl, mut yr) = (0f32, 0f32);
            for t in 0..TAPS {
                yl += c[t] * l[t];
                yr += c[t] * r[t];
            }
            let mut g = 1.0;
            if self.fade < self.fade_len {
                g = self.fade as f32 / self.fade_len as f32;
                self.fade += 1;
            }
            if self.fade_out > 0 {
                g *= self.fade_out as f32 / FADE_SKIP as f32;
                self.fade_out -= 1;
                if self.fade_out == 0 {
                    let skip = self.skip.min(count.saturating_sub(AHEAD + n));
                    rd = (rd + skip) % SIZE;
                    count -= skip;
                    self.level -= skip as f64;
                    self.skip = 0;
                    self.fade = 0;
                    self.fade_len = FADE_SKIP;
                }
            }
            frame[0] = yl * g;
            frame[1] = yr * g;
            f += self.ratio;
            while f >= 1.0 {
                f -= 1.0;
                rd += 1;
                if rd == SIZE {
                    rd = 0;
                }
                count = count.saturating_sub(1);
            }
        }
        self.rd = rd;
        self.frac = f;
        self.count = count;
        true
    }
}

// ----------------------------------------------------------------- the meter

/// Loudness as in broadcasting (ITU-R BS.1770 / EBU R128), the page's meter
/// (web/src/audio/player.ts): K-weighted mean square of both channels over
/// 400 ms (momentary), 3 s (short-term) and since the station was chosen
/// (integrated, gated at -70 LUFS and 10 LU below the mean), and the sample
/// peak; every 100 ms.
struct Meter {
    /// K-weighting for 48 kHz: shelf (head) and high-pass (RLB).
    f: [[f64; 5]; 2],
    z: [[[f64; 2]; 2]; 2],
    blocks: std::collections::VecDeque<f64>,
    sum: f64,
    n: usize,
    peak: f32,
    max: f32,
    /// 400 ms blocks above -70 LUFS: 0.1 LU from -70 to +5 (count, power).
    hist: Vec<f64>,
    hist_p: Vec<f64>,
}

impl Meter {
    fn new() -> Self {
        Meter {
            f: [
                [1.53512485958697, -2.69169618940638, 1.19839281085285, -1.69065929318241, 0.73248077421585],
                [1.0, -2.0, 1.0, -1.99004745483398, 0.99007225036621],
            ],
            z: [[[0.0; 2]; 2]; 2],
            blocks: Default::default(),
            sum: 0.0,
            n: 0,
            peak: 0.0,
            max: 0.0,
            hist: vec![0.0; 750],
            hist_p: vec![0.0; 750],
        }
    }

    fn reset(&mut self) {
        *self = Meter::new();
    }

    fn kw(&mut self, ch: usize, mut x: f64) -> f64 {
        for s in 0..2 {
            let [b0, b1, b2, a1, a2] = self.f[s];
            let z = &mut self.z[ch][s];
            let y = b0 * x + z[0];
            z[0] = b1 * x - a1 * y + z[1];
            z[1] = b2 * x - a2 * y;
            x = y;
        }
        x
    }

    fn integrated(&self) -> Option<f64> {
        let (n, p): (f64, f64) = (self.hist.iter().sum(), self.hist_p.iter().sum());
        if n == 0.0 {
            return None;
        }
        let gate = -0.691 + 10.0 * (p / n).log10() - 10.0;
        let (mut n2, mut p2) = (0.0, 0.0);
        for i in 0..750 {
            if -70.0 + i as f64 / 10.0 >= gate {
                n2 += self.hist[i];
                p2 += self.hist_p[i];
            }
        }
        (n2 > 0.0).then(|| -0.691 + 10.0 * (p2 / n2).log10())
    }

    /// Interleaved stereo; the loudness when 100 ms are full.
    fn feed(&mut self, pcm: &[f32]) -> Option<Loudness> {
        for f in pcm.chunks_exact(2) {
            for (c, &v) in f.iter().enumerate() {
                let a = v.abs();
                self.peak = self.peak.max(a);
                self.max = self.max.max(a);
                let y = self.kw(c, v as f64);
                self.sum += y * y;
            }
            self.n += 1;
        }
        if self.n < RATE / 10 {
            return None;
        }
        self.blocks.push_back(self.sum / self.n as f64);
        if self.blocks.len() > 30 {
            self.blocks.pop_front();
        }
        let lufs = |k: usize| {
            let b: Vec<f64> = self.blocks.iter().rev().take(k).copied().collect();
            let ms = b.iter().sum::<f64>() / b.len() as f64;
            (ms > 0.0).then(|| -0.691 + 10.0 * ms.log10())
        };
        let m = lufs(4);
        // a gating block every 100 ms (400 ms long, 75 % overlap)
        if let Some(m) = m.filter(|&m| self.blocks.len() >= 4 && m >= -70.0) {
            let i = (((m + 70.0) * 10.0).floor() as usize).min(749);
            self.hist[i] += 1.0;
            self.hist_p[i] += 10f64.powf((m + 0.691) / 10.0);
        }
        let db = |x: f32| (x > 0.0).then(|| 20.0 * (x as f64).log10());
        let l = Loudness { m, s: lufs(30), i: self.integrated(), peak: db(self.peak), max: db(self.max) };
        self.sum = 0.0;
        self.n = 0;
        self.peak = 0.0;
        Some(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, from: usize) -> Vec<f32> {
        (from..from + frames)
            .flat_map(|i| {
                let v = (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / RATE as f32).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn the_buffer_plays_what_comes_and_holds_its_target() {
        let mut p = Player::new();
        p.target = RATE / 4;
        let mut out = vec![0f32; BLOCK * 2];
        let mut fed = 0;
        let mut played = 0;
        // the sound comes a little faster than it is played (another clock),
        // in bursts of a superframe
        for step in 0..3000 {
            if step % 6 == 0 {
                let n = 5760 + 3;
                p.write(&tone(n, fed));
                fed += n;
            }
            if p.render(&mut out) {
                played += 1;
            }
        }
        assert!(played > 2900, "played {played} blocks");
        assert_eq!(p.underruns, 0);
        let level = p.count as f64 / RATE as f64;
        assert!((level - 0.25).abs() < 0.12, "level {level}");
        assert!(p.ratio > 1.0, "plays faster to keep up: {}", p.ratio);
    }

    #[test]
    fn a_dropout_is_counted_and_it_plays_again() {
        let mut p = Player::new();
        p.target = RATE / 4;
        let mut out = vec![0f32; BLOCK * 2];
        p.write(&tone(RATE / 2, 0));
        let mut blocks = 0;
        while p.render(&mut out) {
            blocks += 1;
        }
        assert!(blocks >= 24, "{blocks}");
        assert_eq!(p.underruns, 1);
        p.write(&tone(RATE / 2, 0));
        assert!(p.render(&mut out));
    }

    #[test]
    fn the_sinc_passes_a_tone_unchanged() {
        let mut p = Player::new();
        p.target = RATE / 4;
        p.write(&tone(RATE, 0));
        let mut out = vec![0f32; BLOCK * 2];
        // past the fade in
        for _ in 0..4 {
            p.render(&mut out);
        }
        let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
    }

    /// The whole way out: thread, output, writes (ALSA with a file output:
    /// `ALSA_CONFIG_PATH` pointing to a `pcm.!default { type file ... }`).
    #[test]
    #[ignore = "needs a sound output"]
    fn a_tone_reaches_the_output() {
        let s = Speaker::new();
        s.set_target(0.2);
        s.start();
        for i in 0..100 {
            s.push(&tone(BLOCK, i * BLOCK));
            std::thread::sleep(Duration::from_millis(20));
        }
        let r = s.report();
        assert!(r.error.is_none(), "{:?}", r.error);
        assert!(r.playing, "{r:?}");
        assert!(r.meter.and_then(|m| m.m).is_some(), "{r:?}");
        s.stop();
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn the_meter_measures_a_full_scale_sine_at_0_lufs_in_both_channels() {
        let mut m = Meter::new();
        let pcm: Vec<f32> = (0..RATE)
            .flat_map(|i| {
                let v = (i as f32 * 2.0 * std::f32::consts::PI * 997.0 / RATE as f32).sin();
                [v, v]
            })
            .collect();
        let mut last = None;
        for c in pcm.chunks(BLOCK * 2) {
            if let Some(l) = m.feed(c) {
                last = Some(l);
            }
        }
        let l = last.unwrap();
        // (a 997 Hz sine at full scale: -3.01 LUFS in one channel, 0 in both)
        let m = l.m.unwrap();
        assert!((m - 0.0).abs() < 0.7, "momentary {m}");
        assert!(l.peak.unwrap().abs() < 0.01);
    }
}
