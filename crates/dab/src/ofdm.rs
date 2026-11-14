//! OFDM demodulator for DAB transmission mode I.
//!
//! The demodulator pulls baseband samples from a [`SampleSource`], acquires
//! frame synchronisation using the null symbol, the phase reference symbol
//! (time and integer frequency offset) and the cyclic prefix (fractional
//! frequency offset), and produces soft bits of the FIC and the MSC for every
//! transmission frame.

use crate::consts::*;
use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Blocking source of baseband samples at 2.048 MS/s.
pub trait SampleSource {
    /// Fill `buf` completely; return `false` when the stream has ended.
    fn read(&mut self, buf: &mut [Complex32]) -> bool;
}

/// Output of one demodulated transmission frame.
pub struct Frame {
    /// FIC soft bits (3 symbols).
    pub fic: Vec<i8>,
    /// MSC soft bits (72 symbols = 4 CIFs).
    pub msc: Vec<i8>,
    /// FFT of the null symbol in bin order (used by the TII decoder).
    pub null_fft: Vec<Complex32>,
    pub snr_db: f32,
    pub freq_offset_hz: f32,
    /// Average power spectrum of the frame (bin order) if requested.
    pub spectrum: Option<Vec<f32>>,
    /// Power spectrum of the null symbol (bin order) if requested.
    pub null_spectrum: Option<Vec<f32>>,
    /// DQPSK points (differential products of a few symbols, normalised to
    /// their mean amplitude) if the spectrum was requested.
    pub constellation: Option<Vec<Complex32>>,
    /// Smoothed power delay profile (T_U taps from the start of the FFT
    /// window), the window advance and the noise power of one tap, if requested.
    pub impulse: Option<(Vec<f32>, usize, f32)>,
    /// The MSC symbols demodulated (MscSymbols): the others are zeros.
    pub msc_mask: [u64; 2],
}

/// Symbols of the frame whose points make the constellation (MSC symbols);
/// every fourth carrier of them.
const CONSTELLATION_SYMBOLS: [usize; 3] = [5, 30, 60];

/// Which of the 72 MSC symbols of a frame are demodulated (Demodulator::msc_symbols).
pub struct MscSymbols([std::sync::atomic::AtomicU64; 2]);

impl MscSymbols {
    pub fn all() -> Self {
        MscSymbols([std::sync::atomic::AtomicU64::new(u64::MAX), std::sync::atomic::AtomicU64::new(u64::MAX)])
    }

    /// The symbols that carry the CUs of these sub-channels ((start, size) in
    /// CUs) in each of the four CIFs; `all`: every symbol (an ETI recording).
    pub fn set(&self, subchannels: impl Iterator<Item = (usize, usize)>, all: bool) {
        let mut m = [0u64; 2];
        if all {
            m = [u64::MAX; 2];
        } else {
            const PER_CIF: usize = MSC_SYMBOLS / CIFS_PER_FRAME;
            const CUS_PER_SYMBOL: usize = BITS_PER_SYMBOL / CU_BITS;
            for (start, size) in subchannels.filter(|&(_, n)| n > 0) {
                let (first, last) = (start / CUS_PER_SYMBOL, ((start + size - 1) / CUS_PER_SYMBOL).min(PER_CIF - 1));
                for c in 0..CIFS_PER_FRAME {
                    for s in first..=last {
                        let i = c * PER_CIF + s;
                        m[i / 64] |= 1 << (i % 64);
                    }
                }
            }
        }
        self.0[0].store(m[0], Ordering::Relaxed);
        self.0[1].store(m[1], Ordering::Relaxed);
    }

    /// All the symbols of a sub-channel were demodulated in a frame of this
    /// mask (a decoder started after it was made waits for the next frame).
    pub fn covers(mask: &[u64; 2], start: usize, size: usize) -> bool {
        const PER_CIF: usize = MSC_SYMBOLS / CIFS_PER_FRAME;
        const CUS_PER_SYMBOL: usize = BITS_PER_SYMBOL / CU_BITS;
        if size == 0 {
            return true;
        }
        let (first, last) = (start / CUS_PER_SYMBOL, ((start + size - 1) / CUS_PER_SYMBOL).min(PER_CIF - 1));
        (0..CIFS_PER_FRAME).all(|c| {
            (first..=last).all(|s| {
                let i = c * PER_CIF + s;
                (mask[i / 64] >> (i % 64)) & 1 == 1
            })
        })
    }

    /// Every symbol.
    pub fn is_all(mask: &[u64; 2]) -> bool {
        mask[0] == u64::MAX && mask[1] & ((1 << (MSC_SYMBOLS - 64)) - 1) == (1 << (MSC_SYMBOLS - 64)) - 1
    }

    fn load(&self) -> [u64; 2] {
        [self.0[0].load(Ordering::Relaxed), self.0[1].load(Ordering::Relaxed)]
    }
}

/// Events of the demodulator.
pub trait FrameSink {
    fn frame(&mut self, frame: &Frame);
    /// Synchronisation state changed. `signal` tells whether a null symbol was seen.
    fn sync_state(&mut self, synced: bool, signal: bool);
    /// Signal to noise ratio seen in the depth of the null symbol inside the
    /// channel, also without synchronisation (reported before `sync_state`
    /// while acquiring).
    fn null_depth(&mut self, _snr_db: f32) {}
    /// Called regularly when no frame could be produced, allows the caller to stop.
    fn idle(&mut self) {}
    /// Spectrum while not synchronised (empty channel, no DAB signal).
    fn spectrum(&mut self, _spectrum: &[f32], _freq_offset: f32) {}
}

/// Advance of the FFT window into the guard interval (protects against pre-echoes).
fn window_advance() -> usize {
    use std::sync::OnceLock;
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_ADV").ok().and_then(|v| v.parse().ok()).unwrap_or(48))
}
/// Maximum integer carrier offset searched during acquisition (+-35 kHz).
const MAX_CARRIER_OFFSET: i32 = 35;
/// The frequency correction stays this close to the tuned frequency (a tuner
/// is off by tens of ppm, under 30 kHz in Band III). Without a limit, each
/// acquisition moved it by up to MAX_CARRIER_OFFSET more and the tracking
/// followed: on an empty channel it wandered off to a neighbouring multiplex
/// folded into the band (a HackRF at 2.048 MS/s shows the one 1.712 MHz up
/// at -336 kHz) and the channel got that ensemble.
const MAX_NCO_HZ: f32 = (MAX_CARRIER_OFFSET + 5) as f32 * CARRIER_SPACING;
/// Smoothing of the soft bit reference level (per symbol, about 8 frames).
const SOFT_REF_ALPHA: f32 = 1.0 / 600.0;

struct SampleBuffer {
    data: Vec<Complex32>,
    start: usize,
    end: usize,
    // numerically controlled oscillator for frequency correction
    phasor: Complex32,
    step: Complex32,
    nco_freq: f32,
    since_norm: usize,
    /// Time spent on the frequency correction (profile).
    mix_time: Duration,
}

impl SampleBuffer {
    fn new() -> Self {
        SampleBuffer {
            data: vec![Complex32::new(0.0, 0.0); 4 * T_F],
            start: 0,
            end: 0,
            phasor: Complex32::new(1.0, 0.0),
            step: Complex32::new(1.0, 0.0),
            nco_freq: 0.0,
            since_norm: 0,
            mix_time: Duration::ZERO,
        }
    }

    fn available(&self) -> usize {
        self.end - self.start
    }

    fn set_frequency(&mut self, hz: f32) {
        self.nco_freq = hz;
        let w = -2.0 * PI * hz / SAMPLE_RATE as f32;
        self.step = Complex32::new(w.cos(), w.sin());
    }

    /// Make sure at least `n` samples are buffered, reading from `src`.
    fn fill(&mut self, src: &mut dyn SampleSource, n: usize) -> bool {
        if self.available() >= n {
            return true;
        }
        if self.start + n > self.data.len() {
            self.data.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if n > self.data.len() {
                self.data.resize(n, Complex32::new(0.0, 0.0));
            }
        }
        let need = n - self.available();
        let from = self.end;
        if !src.read(&mut self.data[from..from + need]) {
            return false;
        }
        // frequency correction
        let t = Instant::now();
        let mut ph = self.phasor;
        let step = self.step;
        for s in self.data[from..from + need].iter_mut() {
            *s *= ph;
            ph *= step;
            self.since_norm += 1;
            if self.since_norm >= 4096 {
                ph /= ph.norm();
                self.since_norm = 0;
            }
        }
        self.phasor = ph;
        self.end += need;
        self.mix_time += t.elapsed();
        true
    }

    fn slice(&self, offset: usize, len: usize) -> &[Complex32] {
        &self.data[self.start + offset..self.start + offset + len]
    }

    fn consume(&mut self, n: usize) {
        self.start = (self.start + n).min(self.end);
    }

    fn clear(&mut self) {
        self.start = 0;
        self.end = 0;
    }
}

/// Where the demodulator's time goes (MAGICDAB_PROFILE=1: a log line every
/// 10 s, MAGICDAB_PROFILE=N: every N s; in % of one processor), to find what
/// is too slow on small computers.
#[derive(Default)]
struct Profile {
    on: bool,
    /// Seconds between the log lines.
    every: u64,
    fft: Duration,
    setup: Duration,
    chest: Duration,
    symbols: Duration,
    tail: Duration,
    sink: Duration,
    track: Duration,
    acquire: Duration,
    frames: u32,
    acquisitions: u32,
    since: Option<Instant>,
}

impl Profile {
    fn report(&mut self, mix: &mut Duration) {
        let since = *self.since.get_or_insert_with(Instant::now);
        let wall = since.elapsed();
        if wall < Duration::from_secs(self.every) {
            return;
        }
        let pc = |d: Duration| 100.0 * d.as_secs_f64() / wall.as_secs_f64();
        log::info!(
            "OFDM profile, % of a processor: mix {:.1}, fft {:.1}, frame setup {:.1}, channel estimate {:.1}, symbols {:.1}, after symbols {:.1}, timing {:.1}, acquisition {:.1} ({} times); decoders {:.1}; {} frames",
            pc(*mix),
            pc(self.fft),
            pc(self.setup),
            pc(self.chest),
            pc(self.symbols),
            pc(self.tail),
            pc(self.track),
            pc(self.acquire),
            self.acquisitions,
            pc(self.sink),
            self.frames
        );
        *mix = Duration::ZERO;
        *self = Profile { on: true, every: self.every, since: Some(Instant::now()), ..Default::default() };
    }
}

pub struct OfdmDemodulator {
    prof: Profile,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    prs: Vec<Complex32>,
    /// (QPSK symbol index n, FFT bin) of the frequency deinterleaving, in
    /// the order of the bins: the soft bits are computed walking the carriers
    /// in memory order (cache friendly, which small ARM cores need) and
    /// written to their deinterleaved place.
    deinterleave: Vec<(usize, usize)>,
    /// Active carrier bins.
    active: Vec<usize>,
    buf: SampleBuffer,
    synced: bool,
    lost_counter: u32,
    snr: f32,
    soft_ref: f32,
    soft_mode: u8,
    snr_mode: u8,
    /// Smoothed received power per carrier (indexed by FFT bin).
    carrier_power: Vec<f32>,
    /// Smoothed noise power per carrier from the null symbol.
    noise_power: f32,
    /// Differential products of the current symbol (indexed by FFT bin).
    zbuf: Vec<Complex32>,
    /// Remove the common phase error of every symbol (tuner phase noise).
    cpe: bool,
    /// Channel impulse response of the current phase reference symbol.
    cir: Vec<Complex32>,
    /// Smoothed power delay profile (aligned to the FFT window).
    pdp: Vec<f32>,
    pdp_frames: u32,
    /// Window shift applied after the current frame (keeps `pdp` aligned).
    pdp_shift: i32,
    /// Power delay profile shown in the signal view: of every frame (`pdp`
    /// follows the channel estimate, coherent mode only), aligned the same way.
    view_pdp: Vec<f32>,
    view_frames: u32,
    view_shift: i32,
    /// Channel estimate per FFT bin from the phase reference symbol.
    hest: Vec<Complex32>,
    /// Matched filter outputs of the previous / current symbol (coherent mode).
    cprev: Vec<Complex32>,
    ccur: Vec<Complex32>,
    /// Phase of the phase reference symbol in multiples of pi/4 per FFT bin.
    prs_m: Vec<u8>,
    /// Symbols demodulated coherently / differentially (statistics).
    pub coherent_symbols: u64,
    pub differential_symbols: u64,
    pub spectrum_requested: Arc<AtomicBool>,
    /// The MSC symbols (bit i: symbol i of the 72) whose bits someone uses
    /// (the receiver's sub-channel decoders, receiver.rs): the others are
    /// not demodulated, their bits stay erased (zeros). All by default.
    pub msc_symbols: Arc<MscSymbols>,
    symbols: Vec<Vec<Complex32>>,
    frame: Frame,
    /// Short time spectra for the power envelope inside the channel (acquisition).
    env_fft: Arc<dyn Fft<f32>>,
    env_win: Vec<f32>,
    env_bins: Vec<usize>,
    env_buf: Vec<Complex32>,
    env_scratch: Vec<Complex32>,
}

/// Envelope of the power inside the channel: 256 point spectra every 64 samples.
const ENV_N: usize = 256;
const ENV_HOP: usize = 64;

impl Default for OfdmDemodulator {
    fn default() -> Self {
        Self::new()
    }
}

impl OfdmDemodulator {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(T_U);
        let ifft = planner.plan_fft_inverse(T_U);
        let scratch_len = fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len());
        let env_fft = planner.plan_fft_forward(ENV_N);
        let env_scratch = vec![Complex32::new(0.0, 0.0); env_fft.get_inplace_scratch_len()];
        // bins of 8 kHz within ±768 kHz, without the DC offset of the tuner
        let edge = (K / 2 * ENV_N / T_U) as i32;
        let env_bins = (2..=edge).flat_map(|k| [k as usize, ENV_N - k as usize]).collect();
        let mut deinterleave: Vec<(usize, usize)> = frequency_interleaver().into_iter().map(carrier_bin).enumerate().collect();
        deinterleave.sort_by_key(|&(_, bin)| bin);
        let active = carriers().map(carrier_bin).collect();
        OfdmDemodulator {
            prof: {
                let n: u64 = std::env::var("MAGICDAB_PROFILE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
                Profile { on: n > 0, every: if n > 1 { n } else { 10 }, ..Default::default() }
            },
            fft,
            ifft,
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
            prs: prs_bins(),
            deinterleave,
            active,
            buf: SampleBuffer::new(),
            synced: false,
            lost_counter: 0,
            snr: 0.0,
            soft_ref: 0.0,
            snr_mode: std::env::var("MAGICDAB_SNRMODE").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            carrier_power: vec![0.0; T_U],
            noise_power: 0.0,
            zbuf: vec![Complex32::new(0.0, 0.0); T_U],
            cpe: std::env::var("MAGICDAB_CPE").map(|v| v != "0").unwrap_or(true),
            soft_mode: std::env::var("MAGICDAB_SOFTMODE").ok().and_then(|v| v.parse().ok()).unwrap_or(3),
            cir: vec![Complex32::new(0.0, 0.0); T_U],
            pdp: vec![0.0; T_U],
            pdp_frames: 0,
            pdp_shift: 0,
            view_pdp: vec![0.0; T_U],
            view_frames: 0,
            view_shift: 0,
            hest: vec![Complex32::new(0.0, 0.0); T_U],
            cprev: vec![Complex32::new(0.0, 0.0); T_U],
            ccur: vec![Complex32::new(0.0, 0.0); T_U],
            prs_m: {
                let mut m = vec![0u8; T_U];
                for k in carriers() {
                    m[carrier_bin(k)] = (2 * prs_phase_quadrant(k)) as u8;
                }
                m
            },
            coherent_symbols: 0,
            differential_symbols: 0,
            spectrum_requested: Arc::new(AtomicBool::new(false)),
            msc_symbols: Arc::new(MscSymbols::all()),
            symbols: vec![vec![Complex32::new(0.0, 0.0); T_U]; 2],
            frame: Frame {
                fic: vec![0; FIC_BITS],
                msc: vec![0; MSC_SYMBOLS * BITS_PER_SYMBOL],
                null_fft: vec![Complex32::new(0.0, 0.0); T_U],
                msc_mask: [u64::MAX; 2],
                snr_db: 0.0,
                freq_offset_hz: 0.0,
                spectrum: None,
                null_spectrum: None,
                constellation: None,
                impulse: None,
            },
            env_fft,
            env_win: (0..ENV_N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / ENV_N as f32).cos()).collect(),
            env_bins,
            env_buf: vec![Complex32::new(0.0, 0.0); ENV_N],
            env_scratch,
        }
    }

    pub fn frequency_offset(&self) -> f32 {
        self.buf.nco_freq
    }

    /// Reset synchronisation, e.g. after retuning.
    pub fn reset(&mut self) {
        self.synced = false;
        self.buf.clear();
        self.buf.set_frequency(0.0);
        self.lost_counter = 0;
        self.snr = 0.0;
        self.soft_ref = 0.0;
        self.carrier_power.iter_mut().for_each(|p| *p = 0.0);
        self.noise_power = 0.0;
        self.pdp_frames = 0;
        self.pdp_shift = 0;
        self.view_frames = 0;
    }

    /// Run until the source ends or `stop` is set.
    pub fn run(&mut self, src: &mut dyn SampleSource, sink: &mut dyn FrameSink, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            let ok = if self.synced {
                self.process_frame(src, sink)
            } else {
                let t = Instant::now();
                let ok = self.acquire(src, sink);
                self.prof.acquire += t.elapsed();
                self.prof.acquisitions += 1;
                ok
            };
            if !ok {
                break;
            }
            if self.prof.on {
                self.prof.report(&mut self.buf.mix_time);
            }
        }
    }

    fn fft_window(&mut self, offset: usize, out_idx: usize) {
        let t = Instant::now();
        let s = self.buf.slice(offset, T_U);
        let out = &mut self.symbols[out_idx];
        out.copy_from_slice(s);
        self.fft.process_with_scratch(out, &mut self.scratch);
        self.prof.fft += t.elapsed();
    }

    /// Search the null symbol, estimate integer frequency offset and timing.
    fn acquire(&mut self, src: &mut dyn SampleSource, sink: &mut dyn FrameSink) -> bool {
        let need = T_F + T_NULL + T_S;
        if !self.buf.fill(src, need) {
            return false;
        }
        if self.spectrum_requested.load(Ordering::Relaxed) {
            // average spectrum of the raw samples (channel without a DAB signal)
            const N: usize = 16;
            let mut acc = vec![0f32; T_U];
            for k in 0..N {
                self.fft_window(k * (need - T_U) / N, 1);
                for (a, v) in acc.iter_mut().zip(self.symbols[1].iter()) {
                    *a += v.norm_sqr() / N as f32;
                }
            }
            sink.spectrum(&acc, self.buf.nco_freq);
        }
        // Power inside the channel (±768 kHz): neighbouring channels and the
        // products of an overloaded tuner outside it do not fill the null symbol.
        let hops = (need - ENV_N) / ENV_HOP;
        let mut pw = vec![0f32; hops];
        let data = self.buf.slice(0, need);
        for (h, p) in pw.iter_mut().enumerate() {
            let s = &data[h * ENV_HOP..h * ENV_HOP + ENV_N];
            for ((o, v), w) in self.env_buf.iter_mut().zip(s).zip(&self.env_win) {
                *o = v * w;
            }
            self.env_fft.process_with_scratch(&mut self.env_buf, &mut self.env_scratch);
            *p = self.env_bins.iter().map(|&k| self.env_buf[k].norm_sqr()).sum();
        }
        let mean = pw.iter().sum::<f32>() / hops as f32;
        // windows of the spectra that fit into the null symbol (96 samples to spare)
        let wlen = (T_NULL - ENV_N) / ENV_HOP + 1;
        let spare = T_NULL - ((wlen - 1) * ENV_HOP + ENV_N);
        let search = (T_F / ENV_HOP).min(hops - wlen - (T_S / ENV_HOP) - 1);
        let mut acc: f32 = pw[..wlen].iter().sum();
        let mut best = (acc, 0usize);
        for i in 1..search {
            acc += pw[i + wlen - 1] - pw[i - 1];
            if acc < best.0 {
                best = (acc, i);
            }
        }
        let null_level = best.0 / wlen as f32;
        // (S + N) / N between the frame and its null symbol
        if null_level > 0.0 && mean > null_level {
            sink.null_depth(10.0 * (mean / null_level - 1.0).log10());
        } else {
            sink.null_depth(-30.0);
        }
        if mean <= 0.0 || null_level > 0.5 * mean {
            // no DAB signal (no null symbol)
            sink.sync_state(false, false);
            sink.idle();
            self.buf.consume(T_F);
            return true;
        }
        let null_start = (best.1 * ENV_HOP).saturating_sub(spare / 2);

        // PRS spectrum
        let prs_off = null_start + T_NULL + T_G - window_advance();
        self.fft_window(prs_off, 0);

        // integer carrier offset by differential correlation with the reference
        let y = &self.symbols[0];
        let mut best_m = 0i32;
        let mut best_c = 0f32;
        let mut sum_c = 0f32;
        for m in -MAX_CARRIER_OFFSET..=MAX_CARRIER_OFFSET {
            let mut c = Complex32::new(0.0, 0.0);
            for k in carriers() {
                if k == -1 || k == 768 {
                    continue;
                }
                let k2 = if k == -1 { 1 } else { k + 1 };
                let r = self.prs[carrier_bin(k2)] * self.prs[carrier_bin(k)].conj();
                let b1 = ((k + m).rem_euclid(T_U as i32)) as usize;
                let b2 = ((k2 + m).rem_euclid(T_U as i32)) as usize;
                let d = y[b2] * y[b1].conj();
                c += d * r.conj();
            }
            let v = c.norm();
            sum_c += v;
            if v > best_c {
                best_c = v;
                best_m = m;
            }
        }
        let avg_c = (sum_c - best_c) / (2 * MAX_CARRIER_OFFSET) as f32;
        if best_c < 3.0 * avg_c {
            sink.sync_state(false, true);
            sink.idle();
            self.buf.consume(T_F);
            return true;
        }
        if best_m != 0 {
            let f = self.buf.nco_freq + best_m as f32 * CARRIER_SPACING;
            // (not past what a tuner is off: the search starts again from the tuned frequency)
            self.buf.set_frequency(if f.abs() > MAX_NCO_HZ { 0.0 } else { f });
            // samples in the buffer were corrected with the old frequency
            self.buf.clear();
            return true;
        }

        // fine timing from the channel impulse response
        let tau = self.impulse_response_peak();
        let start = null_start as i64 + tau as i64 - window_advance() as i64;
        if start < 0 {
            self.buf.consume(T_F);
            return true;
        }
        self.buf.consume(start as usize);
        self.synced = true;
        self.lost_counter = 0;
        self.pdp_frames = 0;
        self.pdp_shift = 0;
        self.view_frames = 0;
        sink.sync_state(true, true);
        true
    }

    /// Position (samples, may be negative) of the strongest path of the
    /// channel impulse response computed from `symbols[0]` (PRS spectrum).
    fn impulse_response_peak(&mut self) -> i32 {
        let mut h = std::mem::take(&mut self.cir);
        h.iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0));
        for &b in &self.active {
            h[b] = self.symbols[0][b] * self.prs[b].conj();
        }
        self.ifft.process_with_scratch(&mut h, &mut self.scratch);
        let mut best = (0f32, 0usize);
        for (i, v) in h.iter().enumerate() {
            let p = v.norm_sqr();
            if p > best.0 {
                best = (p, i);
            }
        }
        if std::env::var("CIRDEBUG").is_ok() {
            // strongest peaks (positions relative to the window start)
            let mut peaks: Vec<(f32, i32)> =
                h.iter().enumerate().map(|(i, v)| (v.norm_sqr(), if i >= T_U / 2 { i as i32 - T_U as i32 } else { i as i32 })).collect();
            peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut shown: Vec<(f32, i32)> = Vec::new();
            for p in peaks {
                if shown.iter().all(|s| (s.1 - p.1).abs() > 6) {
                    shown.push(p);
                }
                if shown.len() == 4 {
                    break;
                }
            }
            eprintln!(
                "CIR {}",
                shown.iter().map(|(p, i)| format!("{}:{:.1}dB", i, 10.0 * (p / best.0).log10())).collect::<Vec<_>>().join(" ")
            );
        }
        self.cir = h;
        let i = best.1 as i32;
        if i >= T_U as i32 / 2 {
            i - T_U as i32
        } else {
            i
        }
    }

    /// Window shift (samples, positive = later) with the least intersymbol
    /// interference according to the smoothed power delay profile. A path at
    /// CIR index `a` (window relative) is free of interference for
    /// 0 <= a <= T_G; outside, its interference grows with the overlap. Among
    /// equally good positions the smallest shift wins, so a single path keeps
    /// the window advance.
    fn best_window_shift(&self, noise: f32) -> Option<i32> {
        best_window_shift(&self.pdp, noise)
    }

    /// Strongest tap of `self.cir` relative to the median tap power.
    fn cir_peak_ratio(&self) -> f32 {
        let mut p: Vec<f32> = self.cir.iter().map(|v| v.norm_sqr()).collect();
        let max = p.iter().cloned().fold(0.0, f32::max);
        let mid = p.len() / 2;
        p.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        if p[mid] > 0.0 {
            max / p[mid]
        } else {
            f32::INFINITY
        }
    }

    /// Channel estimate from the phase reference symbol. The raw estimate is
    /// taken to the time domain (`self.cir`), taps that do not stand out of the
    /// noise in the smoothed power delay profile are removed (taps that do are
    /// Wiener weighted) and the result is taken back to the frequency domain.
    /// A few Papoulis-Gerchberg iterations extrapolate the estimate into the
    /// unused carriers, so the band edges are not biased by the truncation.
    fn estimate_channel(&mut self, noise: f32) {
        let n = T_U;
        if self.pdp_frames > 0 && self.pdp_shift != 0 {
            let s = self.pdp_shift.rem_euclid(n as i32) as usize;
            self.pdp.rotate_left(s);
        }
        let alpha = if self.pdp_frames == 0 { 1.0 } else { 0.15f32.max(1.0 / (self.pdp_frames + 1) as f32) };
        for (p, h) in self.pdp.iter_mut().zip(self.cir.iter()) {
            *p += (h.norm_sqr() - *p) * alpha;
        }
        self.pdp_frames += 1;
        // noise power of one tap of the unnormalised transform
        let floor = K as f32 * noise;
        let th = chest_th();
        let weights: Vec<f32> = self
            .pdp
            .iter()
            .map(|&p| {
                if th <= 0.0 {
                    1.0
                } else if p > th * floor {
                    1.0 - floor / p
                } else {
                    0.0
                }
            })
            .collect();
        let mut x = self.cir.clone();
        let mut est = vec![Complex32::new(0.0, 0.0); n];
        for it in 0..3 {
            if it > 0 {
                // known carriers keep the measurement, the others take the extrapolation
                for (i, v) in x.iter_mut().enumerate() {
                    *v = est[i];
                }
                for &b in &self.active {
                    x[b] = self.symbols[0][b] * self.prs[b].conj();
                }
                self.ifft.process_with_scratch(&mut x, &mut self.scratch);
            }
            for (v, w) in x.iter_mut().zip(weights.iter()) {
                *v *= *w / n as f32;
            }
            est.copy_from_slice(&x);
            self.fft.process_with_scratch(&mut est, &mut self.scratch);
        }
        for &b in &self.active {
            self.hest[b] = est[b];
        }
    }

    fn process_frame(&mut self, src: &mut dyn SampleSource, sink: &mut dyn FrameSink) -> bool {
        if !self.buf.fill(src, T_F + T_S) {
            return false;
        }
        let want_spectrum = self.spectrum_requested.load(Ordering::Relaxed);
        let adv = window_advance();
        let t_frame = Instant::now();
        let fft_before = self.prof.fft;

        // phase reference symbol and timing
        self.fft_window(T_NULL + T_G - adv, 0);
        let mut tau = self.impulse_response_peak();
        let mut timing_error = tau - adv as i32;
        // Offset of the frame in the buffer. A big timing jump (lost samples, a
        // different transmitter of the SFN becoming the strongest) is applied to
        // this frame right away instead of losing two or three frames.
        let mut base: isize = 0;
        if timing_error.abs() > REALIGN_MIN {
            // the smoothed power delay profile no longer matches
            self.pdp_frames = 0;
        }
        if timing_error.abs() > REALIGN_MIN && timing_error.abs() < (T_U / 2) as i32 - 100 {
            let d = timing_error as isize;
            if !self.buf.fill(src, T_F + T_S + d.max(0) as usize) {
                return false;
            }
            self.fft_window(((T_NULL + T_G - adv) as isize + d) as usize, 0);
            let tau2 = self.impulse_response_peak();
            if (tau2 - adv as i32).abs() <= 4 && self.cir_peak_ratio() > 100.0 {
                log::debug!("timing jump of {d} samples, frame realigned");
                base = d;
                tau = tau2;
                timing_error = tau2 - adv as i32;
            } else {
                self.fft_window(T_NULL + T_G - adv, 0);
                self.impulse_response_peak();
            }
        }
        let at = |x: usize| (x as isize + base) as usize;

        // null symbol level vs signal level, null symbol spectrum (TII, noise
        // floor); skipped when a realignment moved the null symbol out of the buffer
        let null_valid = base >= -(T_NULL as isize - T_U as isize - 64);
        if null_valid {
            let s = self.buf.slice(at(T_NULL - T_U - 64), T_U);
            self.frame.null_fft.copy_from_slice(s);
            self.fft.process_with_scratch(&mut self.frame.null_fft, &mut self.scratch);
        }

        // fractional frequency offset from the cyclic prefix
        let mut cp = Complex32::new(0.0, 0.0);
        for l in 0..L {
            let s0 = at(T_NULL + l * T_S);
            let g = self.buf.slice(s0 + T_G / 2, T_G / 2 - adv);
            let u = self.buf.slice(s0 + T_G / 2 + T_U, T_G / 2 - adv);
            for (a, b) in g.iter().zip(u.iter()) {
                cp += a * b.conj();
            }
        }
        let fine_offset = -cp.arg() / (2.0 * PI) * CARRIER_SPACING;

        // noise floor from the null symbol (median of active carriers, TII carriers are a minority)
        let mut noise: Vec<f32> = self.active.iter().map(|&b| self.frame.null_fft[b].norm_sqr()).collect();
        let mid = noise.len() / 2;
        noise.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        let noise_pw = noise[mid] / std::f32::consts::LN_2;
        // the null symbol against the phase reference symbol inside the channel
        let (null_pw, sig_pw) = if null_valid {
            (noise_pw, self.active.iter().map(|&b| self.symbols[0][b].norm_sqr()).sum::<f32>() / self.active.len() as f32)
        } else {
            (0.0, 1.0)
        };
        if self.noise_power <= 0.0 {
            self.noise_power = noise_pw;
        } else {
            self.noise_power += (noise_pw - self.noise_power) * 0.2;
        }
        if self.carrier_power[self.active[0]] <= 0.0 {
            for &b in &self.active {
                self.carrier_power[b] = self.symbols[0][b].norm_sqr();
            }
        }
        let coherent = self.soft_mode >= 3 && self.noise_power > 0.0;
        self.prof.setup += t_frame.elapsed().saturating_sub(self.prof.fft - fft_before);
        let t_chest = Instant::now();
        if coherent {
            self.estimate_channel(self.noise_power);
            // the phase reference symbol is known: c = |H|^2 * PRS
            for &b in &self.active {
                self.cprev[b] = self.symbols[0][b] * self.hest[b].conj();
            }
        }
        let mut theta = 0f32;
        self.prof.chest += t_chest.elapsed();
        let t_symbols = Instant::now();
        let fft_symbols = self.prof.fft;

        // data symbols
        let mut spectrum = if want_spectrum {
            let mut v = vec![0f32; T_U];
            for (i, s) in self.symbols[0].iter().enumerate() {
                v[i] += s.norm_sqr();
            }
            Some(v)
        } else {
            None
        };
        let mut sig_power = 0f64;
        let mut constellation: Option<Vec<Complex32>> = None;
        // The symbols demodulated: the FIC and the MSC symbols whose bits are
        // used (the station heard, data services; all of them for an ETI
        // recording or the spectrum view). A phone saves most of the work of
        // this loop: one station of 192 kbit/s lies in 4 of the 18 symbols of a CIF.
        let msc_mask = if want_spectrum || all_symbols() { [u64::MAX; 2] } else { self.msc_symbols.load() };
        let wanted = |l: usize| {
            l <= FIC_SYMBOLS || {
                let i = l - 1 - FIC_SYMBOLS;
                (msc_mask[i / 64] >> (i % 64)) & 1 == 1
            }
        };
        self.frame.msc_mask = msc_mask;
        let mut detected = 0usize;
        for l in 1..L {
            let full = wanted(l);
            // the symbol before a wanted one is its phase reference: transformed
            // and (coherent detection) compensated, its bits not detected
            let reference = !full && l + 1 < L && wanted(l + 1);
            if !full {
                let o = (l - 1 - FIC_SYMBOLS) * BITS_PER_SYMBOL;
                self.frame.msc[o..o + BITS_PER_SYMBOL].fill(0);
                if !reference {
                    continue;
                }
            }
            let cur = l & 1;
            let prev = cur ^ 1;
            self.fft_window(at(T_NULL + l * T_S + T_G - adv), cur);
            let (a, b) = self.symbols.split_at(1);
            let (ycur, yprev) = if cur == 1 { (&b[0], &a[0]) } else { (&a[0], &b[0]) };
            if reference {
                if coherent {
                    let mut cc4 = Complex32::new(0.0, 0.0);
                    for &bin in &self.active {
                        let c = ycur[bin] * self.hest[bin].conj();
                        self.ccur[bin] = c;
                        let a = c.norm();
                        if a > 0.0 {
                            let c2 = c * c;
                            cc4 += c2 * c2 / (a * a * a);
                        }
                    }
                    let mut phi = (if l & 1 == 1 { -cc4 } else { cc4 }).arg() / 4.0;
                    let q = std::f32::consts::FRAC_PI_2;
                    phi += ((theta - phi) / q).round() * q;
                    theta = phi;
                    let rot = Complex32::from_polar(1.0, -theta);
                    for &bin in &self.active {
                        self.ccur[bin] *= rot;
                    }
                    std::mem::swap(&mut self.cprev, &mut self.ccur);
                }
                continue;
            }
            detected += 1;
            if let Some(sp) = spectrum.as_mut() {
                for (i, s) in ycur.iter().enumerate() {
                    sp[i] += s.norm_sqr();
                }
            }
            // differential demodulation, common phase error removal and normalisation
            let mut sum_abs = 0f32;
            let mut sum_pw = 0f32;
            let mut c4 = Complex32::new(0.0, 0.0);
            for &bin in &self.active {
                let z = ycur[bin] * yprev[bin].conj();
                self.zbuf[bin] = z;
                let a = z.norm();
                sum_abs += a;
                sum_pw += ycur[bin].norm_sqr();
                if a > 0.0 {
                    // fourth power removes the QPSK modulation, weight by amplitude
                    let z2 = z * z;
                    c4 += z2 * z2 / (a * a * a);
                }
            }
            if self.cpe {
                // QPSK points sit at odd multiples of 45 degrees, z^4 points to 180 degrees
                let delta = (-c4).arg() / 4.0;
                let rot = Complex32::from_polar(1.0, -delta);
                for &bin in &self.active {
                    self.zbuf[bin] *= rot;
                }
            }
            sig_power += sum_pw as f64;
            if want_spectrum && CONSTELLATION_SYMBOLS.contains(&l) && sum_abs > 0.0 {
                let mean = sum_abs / K as f32;
                constellation.get_or_insert_with(Vec::new).extend(self.active.iter().step_by(4).map(|&b| self.zbuf[b] / mean));
            }
            // Soft bit scaling uses a long term reference level so that symbols
            // hit by a fade get proportionally less reliable soft bits, which
            // the time interleaver and Viterbi decoder can then exploit.
            let mean_abs = sum_abs / K as f32;
            if self.soft_ref <= 0.0 {
                self.soft_ref = mean_abs;
            } else {
                self.soft_ref += (mean_abs - self.soft_ref) * SOFT_REF_ALPHA;
            }
            let reference = match self.soft_mode {
                0 => mean_abs,
                _ => self.soft_ref,
            };
            let scale = if reference > 0.0 { 64.0 / reference } else { 0.0 };
            let out: &mut [i8] = if l <= FIC_SYMBOLS {
                let o = (l - 1) * BITS_PER_SYMBOL;
                &mut self.frame.fic[o..o + BITS_PER_SYMBOL]
            } else {
                let o = (l - 1 - FIC_SYMBOLS) * BITS_PER_SYMBOL;
                &mut self.frame.msc[o..o + BITS_PER_SYMBOL]
            };
            let zb = &self.zbuf;
            if coherent {
                let rho_diff = c4.norm() / sum_abs.max(1e-20);
                // matched filter with the channel estimate, common phase error
                // from the fourth power (symbol l carries odd multiples of 45
                // degrees when l is odd, multiples of 90 degrees otherwise)
                let mut cc4 = Complex32::new(0.0, 0.0);
                let mut csum = 0f32;
                for &bin in &self.active {
                    let c = ycur[bin] * self.hest[bin].conj();
                    self.ccur[bin] = c;
                    let a = c.norm();
                    csum += a;
                    if a > 0.0 {
                        let c2 = c * c;
                        cc4 += c2 * c2 / (a * a * a);
                    }
                }
                let rho_coh = cc4.norm() / csum.max(1e-20);
                let mut phi = (if l & 1 == 1 { -cc4 } else { cc4 }).arg() / 4.0;
                // resolve the 90 degree ambiguity against the previous symbol
                let q = std::f32::consts::FRAC_PI_2;
                phi += ((theta - phi) / q).round() * q;
                theta = phi;
                let rot = Complex32::from_polar(1.0, -theta);
                for &bin in &self.active {
                    self.ccur[bin] *= rot;
                }
                let s2 = self.noise_power;
                for &bin in &self.active {
                    let p = &mut self.carrier_power[bin];
                    *p += (ycur[bin].norm_sqr() - *p) * 0.1;
                }
                // Coherent detection needs a clearly better constellation than
                // differential detection: on static channels its coherence is about
                // twice as high, while long SFN channels (paths beyond the guard
                // interval), fast fading or a broken estimate after a timing jump
                // favour the previous symbol as the reference.
                // Mode 4 forces coherent, mode 5 differential detection (for tests).
                let use_coh = match self.soft_mode {
                    4 => true,
                    5 => false,
                    _ => rho_coh >= coh_margin() * rho_diff,
                };
                // A symbol without any QPSK structure (random phases give about
                // 0.03) is noise, e.g. after lost samples: erase it instead of
                // feeding confident garbage to the Viterbi decoder.
                let rho = rho_coh.max(rho_diff);
                let trust = ((rho - 0.06) / 0.06).clamp(0.0, 1.0);
                if use_coh {
                    self.coherent_symbols += 1;
                    let g = soft_gain() * 2.0 / s2 * trust;
                    let pp = (l - 1) & 1;
                    for &(n, bin) in &self.deinterleave {
                        let cc = self.ccur[bin];
                        let b = [proj(cc, pp ^ 1), proj(cc, (pp ^ 1) + 2), proj(cc, (pp ^ 1) + 4), proj(cc, (pp ^ 1) + 6)];
                        // best metric for the phase steps of 45, 135, 225 and 315 degrees
                        let mut best = [f32::NEG_INFINITY; 4];
                        if l == 1 {
                            let i0 = (self.prs_m[bin] >> 1) as usize;
                            for (di, m) in best.iter_mut().enumerate() {
                                *m = b[(i0 + di) & 3];
                            }
                        } else {
                            let cp = self.cprev[bin];
                            let a = [proj(cp, pp), proj(cp, pp + 2), proj(cp, pp + 4), proj(cp, pp + 6)];
                            for (i, &ai) in a.iter().enumerate() {
                                for (di, m) in best.iter_mut().enumerate() {
                                    let v = ai + b[(i + di + pp) & 3];
                                    if v > *m {
                                        *m = v;
                                    }
                                }
                            }
                        }
                        let llr_i = best[0].max(best[3]) - best[1].max(best[2]);
                        let llr_q = best[0].max(best[1]) - best[2].max(best[3]);
                        out[n] = (llr_i * g).clamp(-127.0, 127.0).round() as i8;
                        out[n + K] = (llr_q * g).clamp(-127.0, 127.0).round() as i8;
                    }
                } else {
                    // Non-coherent detection of the symbol pair (unknown channel
                    // phase): p(Y1, Y2 | D) ~ I0(2 |H| / s^2 * |Y1 + Y2 D*|) with
                    // |Y1 + Y2 D*|^2 = |Y1|^2 + |Y2|^2 + 2 Re(z D*), max-log. The
                    // square root makes large noisy products count less, and the
                    // scale matches the coherent LLRs.
                    // The signal power of the symbol is used for all carriers: per
                    // carrier estimates are too noisy at low SNR (and the channel
                    // estimate may be broken after a timing jump).
                    self.differential_symbols += 1;
                    let h2 = (sum_pw / K as f32 - s2).max(0.1 * s2);
                    let g = soft_gain() * 2.0 / s2 * trust;
                    let hmode = hmode();
                    for &(n, bin) in &self.deinterleave {
                        let z = zb[bin];
                        let a = ycur[bin].norm_sqr() + yprev[bin].norm_sqr();
                        let hk = match hmode {
                            1 => (self.carrier_power[bin] - s2).max(0.1 * s2).sqrt(),
                            2 => self.hest[bin].norm(),
                            3 => (0.5 * a - s2).max(0.1 * s2).sqrt(),
                            _ => h2.sqrt(),
                        };
                        let g = g * hk;
                        let f = |m: f32| (a + 2.0 * m).max(0.0).sqrt();
                        let (p1, p3, p5, p7) = (proj(z, 1), proj(z, 3), proj(z, 5), proj(z, 7));
                        let llr_i = f(p1.max(p7)) - f(p3.max(p5));
                        let llr_q = f(p1.max(p3)) - f(p5.max(p7));
                        out[n] = (llr_i * g).clamp(-127.0, 127.0).round() as i8;
                        out[n + K] = (llr_q * g).clamp(-127.0, 127.0).round() as i8;
                    }
                }
                std::mem::swap(&mut self.cprev, &mut self.ccur);
            } else if self.soft_mode == 2 && self.noise_power > 0.0 {
                // channel state aware LLR for DQPSK with a noisy reference:
                // LLR ~ Re(z) * |H|^2 / (2 |H|^2 s^2 + s^4)
                let s2 = self.noise_power;
                let s4 = s2 * s2;
                for &bin in &self.active {
                    let p = &mut self.carrier_power[bin];
                    *p += (ycur[bin].norm_sqr() - *p) * 0.15;
                }
                for &(n, bin) in &self.deinterleave {
                    let z = zb[bin];
                    let h2 = (self.carrier_power[bin] - s2).max(0.05 * s2);
                    let w = 20.0 * h2 / (2.0 * h2 * s2 + s4);
                    out[n] = (z.re * w).clamp(-127.0, 127.0) as i8;
                    out[n + K] = (z.im * w).clamp(-127.0, 127.0) as i8;
                }
            } else {
                for &(n, bin) in &self.deinterleave {
                    let z = zb[bin];
                    out[n] = (z.re * scale).clamp(-127.0, 127.0) as i8;
                    out[n + K] = (z.im * scale).clamp(-127.0, 127.0) as i8;
                }
            }
            let _ = prev;
        }
        self.prof.symbols += t_symbols.elapsed().saturating_sub(self.prof.fft - fft_symbols);
        let t_tail = Instant::now();

        // Displayed SNR follows the common convention of comparing the carrier
        // power with the total null symbol power (TII carriers included).
        let null_mean: f32 = self.active.iter().map(|&b| self.frame.null_fft[b].norm_sqr()).sum::<f32>() / K as f32;
        let noise_pw = if self.snr_mode == 1 { noise_pw } else { null_mean };
        let carrier_pw = (sig_power / (detected.max(1) * K) as f64) as f32;
        // a perfectly clean signal (e.g. a software modulator) has no noise in the null symbol
        let snr = if carrier_pw <= 0.0 {
            0.0
        } else if noise_pw <= carrier_pw * 1e-6 {
            60.0
        } else if carrier_pw > noise_pw {
            (10.0 * ((carrier_pw - noise_pw) / noise_pw).log10()).min(60.0)
        } else {
            0.0
        };
        self.snr = if self.snr == 0.0 { snr } else { 0.8 * self.snr + 0.2 * snr };

        self.frame.snr_db = self.snr;
        self.frame.freq_offset_hz = self.buf.nco_freq + fine_offset;
        self.frame.spectrum = spectrum.map(|mut v| {
            let n = L as f32;
            for x in v.iter_mut() {
                *x /= n;
            }
            v
        });
        self.frame.null_spectrum = if want_spectrum { Some(self.frame.null_fft.iter().map(|c| c.norm_sqr()).collect()) } else { None };
        self.frame.constellation = constellation;
        self.frame.impulse = if want_spectrum {
            if self.view_frames > 0 && self.view_shift != 0 {
                self.view_pdp.rotate_left(self.view_shift.rem_euclid(T_U as i32) as usize);
            }
            // a big timing jump starts it anew
            let fresh = self.view_frames == 0 || base != 0;
            for (v, h) in self.view_pdp.iter_mut().zip(self.cir.iter()) {
                let p = h.norm_sqr();
                *v = if fresh { p } else { *v + (p - *v) * 0.25 };
            }
            self.view_frames += 1;
            Some((self.view_pdp.clone(), adv, K as f32 * self.noise_power))
        } else {
            self.view_frames = 0;
            None
        };
        if std::env::var("COHDEBUG").is_ok()
            && self.differential_symbols > 0
            && (self.coherent_symbols + self.differential_symbols) % 7500 < 75
        {
            eprintln!("COH {} DIFF {}", self.coherent_symbols, self.differential_symbols);
        }
        if std::env::var("OFDMDEBUG").is_ok() {
            eprintln!(
                "OFDM tau {} terr {} fine {:.1} nco {:.1} snr {:.1} nullratio {:.3}",
                tau,
                timing_error,
                fine_offset,
                self.buf.nco_freq,
                self.snr,
                null_pw / sig_pw.max(1e-12)
            );
        }
        self.prof.tail += t_tail.elapsed();
        let t_sink = Instant::now();
        sink.frame(&self.frame);
        self.prof.sink += t_sink.elapsed();
        self.prof.frames += 1;
        let t_track = Instant::now();

        // frequency tracking
        let f = self.buf.nco_freq + 0.4 * fine_offset;
        self.buf.set_frequency(f.clamp(-MAX_NCO_HZ, MAX_NCO_HZ));

        // synchronisation supervision
        let null_ok = sig_pw > 0.0 && null_pw < 0.5 * sig_pw;
        if !null_ok || timing_error.abs() > T_G as i32 {
            self.lost_counter += 1;
        } else {
            self.lost_counter = 0;
        }
        if self.lost_counter >= 8 {
            self.synced = false;
            sink.sync_state(false, null_ok);
            self.buf.consume(T_F);
            return true;
        }

        // Timing tracking: with a settled power delay profile the window goes to
        // the position with the least intersymbol interference, otherwise it
        // follows the strongest path. Big errors are corrected at once.
        let track = if coherent && smart_timing() && self.pdp_frames >= 4 && timing_error.abs() <= REALIGN_MIN {
            self.best_window_shift(self.noise_power).unwrap_or(timing_error)
        } else {
            timing_error
        };
        let adj = if track.abs() > 16 { track.clamp(-(T_G as i32), T_G as i32) } else { track.signum() * ((track.abs() + 1) / 2) };
        let consume = (T_F as i64 + base as i64 + adj as i64).max(1) as usize;
        self.pdp_shift = adj;
        self.view_shift = adj;
        self.buf.consume(consume);
        self.prof.track += t_track.elapsed();
        true
    }
}

/// Scale of LLRs to soft bits.
fn soft_gain() -> f32 {
    use std::sync::OnceLock;
    static V: OnceLock<f32> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_SOFTGAIN").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0))
}

fn chest_th() -> f32 {
    use std::sync::OnceLock;
    static V: OnceLock<f32> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_CHESTTH").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0))
}

/// MAGICDAB_ALL_SYMBOLS=1: every MSC symbol demodulated (comparisons).
fn all_symbols() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_ALL_SYMBOLS").is_ok_and(|v| v == "1"))
}

fn hmode() -> u8 {
    use std::sync::OnceLock;
    static V: OnceLock<u8> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_HMODE").ok().and_then(|v| v.parse().ok()).unwrap_or(2))
}

fn smart_timing() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_SMARTTIMING").map(|v| v != "0").unwrap_or(true))
}

/// Timing errors (samples) beyond which the current frame is realigned.
const REALIGN_MIN: i32 = 64;

/// Coherent detection is used while its fourth power coherence is at least
/// this multiple of the coherence of differential detection.
fn coh_margin() -> f32 {
    use std::sync::OnceLock;
    static V: OnceLock<f32> = OnceLock::new();
    *V.get_or_init(|| std::env::var("MAGICDAB_COHMARGIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.4))
}

/// Re(c * exp(-j m pi/4)).
#[inline]
fn proj(c: Complex32, m: usize) -> f32 {
    const R: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match m & 7 {
        0 => c.re,
        1 => (c.re + c.im) * R,
        2 => c.im,
        3 => (c.im - c.re) * R,
        4 => -c.re,
        5 => -(c.re + c.im) * R,
        6 => -c.im,
        _ => (c.re - c.im) * R,
    }
}

/// The window shift with the least intersymbol interference for a power
/// delay profile (`pdp`, T_U taps from the start of the FFT window, the second
/// half meaning negative delays) and the noise power of a tap: the cost of a
/// shift `s` is the power of the paths outside the guard interval after it,
/// times how far outside. Paths are the taps above 3x the noise floor.
///
/// The cost is piecewise linear in `s`, so it comes from prefix sums over the
/// delays (linear time; the sum over the paths for every shift took 15 % of a
/// Raspberry Pi 3's processor at a good signal, hundreds of paths).
fn best_window_shift(pdp: &[f32], noise: f32) -> Option<i32> {
    let floor = K as f32 * noise;
    let n = pdp.len() as i32;
    let half = n / 2;
    // power and power x delay by delay, delays -half..half
    let mut pw = vec![0f64; n as usize];
    let mut any = false;
    for (i, &p) in pdp.iter().enumerate() {
        if p > 3.0 * floor {
            let a = if i as i32 >= half { i as i32 - n } else { i as i32 };
            pw[(a + half) as usize] += (p - floor) as f64;
            any = true;
        }
    }
    if !any {
        return None;
    }
    // prefix sums: P[k] = sum of power of delays < k - half, A[k] the same of power x delay
    let mut pp = vec![0f64; n as usize + 1];
    let mut pa = vec![0f64; n as usize + 1];
    for k in 0..n as usize {
        let a = k as i32 - half;
        pp[k + 1] = pp[k] + pw[k];
        pa[k + 1] = pa[k] + pw[k] * a as f64;
    }
    let total = pp[n as usize];
    let total_a = pa[n as usize];
    // (index of the first delay >= d, clamped)
    let idx = |d: i32| (d + half).clamp(0, n) as usize;
    let g = T_G as i32;
    let cost = |s: i32| -> f64 {
        // paths before the window: delay a < s, (s - a) each
        let k = idx(s);
        let before = s as f64 * pp[k] - pa[k];
        // paths after the guard: delay a > s + T_G, (a - s - T_G) each
        let k = idx(s + g + 1);
        let after = (total_a - pa[k]) - (s + g) as f64 * (total - pp[k]);
        before + after
    };
    let costs: Vec<(i32, f64)> = (-g..=g).map(|s| (s, cost(s))).collect();
    let min = costs.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    // smallest shift within a tolerance (all power overlapping by one sample,
    // about -33 dB of interference) of the best position
    costs.iter().filter(|c| c.1 <= min + total).min_by_key(|c| c.0.abs()).map(|c| c.0)
}

#[cfg(test)]
mod window_tests {
    use super::*;

    // the version of before (a sum over the paths for every shift), as it was
    fn old_shift(pdp: &[f32], noise: f32) -> Option<i32> {
        let floor = K as f32 * noise;
        let paths: Vec<(i32, f32)> = pdp
            .iter()
            .enumerate()
            .filter(|(_, &p)| p > 3.0 * floor)
            .map(|(i, &p)| (if i >= T_U / 2 { i as i32 - T_U as i32 } else { i as i32 }, p - floor))
            .collect();
        if paths.is_empty() {
            return None;
        }
        let cost = |s: i32| -> f32 {
            paths
                .iter()
                .map(|&(a, p)| {
                    let a = a - s;
                    let over = if a < 0 {
                        -a
                    } else if a > T_G as i32 {
                        a - T_G as i32
                    } else {
                        0
                    };
                    p * over as f32
                })
                .sum()
        };
        let total: f32 = paths.iter().map(|p| p.1).sum();
        let costs: Vec<(i32, f32)> = (-(T_G as i32)..=(T_G as i32)).map(|s| (s, cost(s))).collect();
        let min = costs.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
        // smallest shift within a tolerance (all power overlapping by one sample,
        // about -33 dB of interference) of the best position
        costs.iter().filter(|c| c.1 <= min + total).min_by_key(|c| c.0.abs()).map(|c| c.0)
    }

    #[test]
    fn the_window_shift_is_the_one_of_before() {
        // pseudo-random profiles: a few strong paths (pre-echoes, echoes beyond
        // the guard interval), a noise floor
        let mut x = 12345u32;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32
        };
        for case in 0..300 {
            let noise = 0.01 + rnd() * 0.05;
            let floor = K as f32 * noise;
            let mut pdp: Vec<f32> = (0..T_U).map(|_| floor * (0.2 + rnd())).collect();
            let paths = 1 + (rnd() * 40.0) as usize;
            for _ in 0..paths {
                let d = (rnd() * T_U as f32) as usize % T_U;
                pdp[d] += floor * (2.0 + rnd() * 3000.0);
            }
            if case % 7 == 0 {
                // an empty profile now and then
                pdp.iter_mut().for_each(|p| *p = floor);
            }
            assert_eq!(best_window_shift(&pdp, noise), old_shift(&pdp, noise), "case {case}");
        }
    }
}
