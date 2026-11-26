//! Transmitter Identification Information decoding from the null symbol
//! (EN 300 401 clause 14.8, transmission mode I).
//!
//! The TII signal consists of carrier pairs; both carriers of a pair carry the
//! same phase, so X[k] * conj(X[k+1]) accumulates coherently while noise does
//! not. The four 384 carrier blocks repeat the same comb/pattern and are summed.
//! Results are integrated over several frames before detection.
//!
//! Some transmitters give the second carrier of a pair the phase of the phase
//! reference symbol on that carrier instead of the first one's: their products
//! turn by 0, 90 or 180 degrees from block to block and cancel in the sum
//! (a recording with eight or more transmitters: none found, each block alone
//! 10 to 12 dB over the noise). The products are summed both ways, and each
//! comb takes the way it comes out stronger.

use crate::consts::{prs_phase_quadrant, T_U};
use num_complex::Complex32;
use serde::{Deserialize, Serialize};

pub const PAIRS: usize = 192;

/// Frames left out after a retune or a loss of synchronisation. The frequency
/// tracking takes 0.4 of the error per frame, so a fractional offset of half a
/// carrier needs about ten frames to come under 3 Hz; before that each carrier
/// leaks into its neighbours, and a strong transmitter came out with the codes
/// of the combs beside its own (up to four on each side, 17 to 30 dB lower,
/// only in the first result after tuning; a recording moved by 3.4 kHz shows
/// it too).
pub const SETTLE_FRAMES: usize = 10;

/// 8 bit pattern (b0 = MSB) of TII main identifier 0..69.
pub fn tii_pattern(main: u8) -> u8 {
    let mut n = 0;
    for v in 0u16..256 {
        if v.count_ones() == 4 {
            if n == main {
                return v as u8;
            }
            n += 1;
        }
    }
    0
}

fn pattern_to_main(p: u8) -> Option<u8> {
    let mut n = 0;
    for v in 0u16..256 {
        if v.count_ones() == 4 {
            if v as u8 == p {
                return Some(n);
            }
            n += 1;
        }
    }
    None
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TiiCode {
    pub main: u8,
    pub sub: u8,
    /// Level relative to the strongest transmitter in dB (0 = strongest).
    pub level_db: f32,
    /// Absolute detection metric (for debugging / sorting).
    pub strength: f32,
    /// Ratio of the weakest pattern carrier over the noise level in dB.
    pub margin_db: f32,
    /// The transmitter's own SNR in dB, from its TII carriers: their power over
    /// the mean power of the null symbol, the scale of the multiplex SNR (which
    /// adds up all transmitters of a single frequency network, also those that
    /// send no TII). None where it was not measured (older DX recordings).
    pub snr_db: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TiiMode {
    Sensitive,
    Reliable,
}

pub struct TiiDecoder {
    acc: Vec<Complex32>,
    /// The same with the phase reference difference of each pair taken out
    /// (the second carrier with a phase of its own).
    acc_own: Vec<Complex32>,
    /// That difference, per block and pair (block * PAIRS + pair).
    turn: Vec<Complex32>,
    /// Summed power of each pair (both carriers, the four blocks).
    pair_power: Vec<f64>,
    /// Summed mean carrier power of the null symbols (the active carriers).
    null_power: f64,
    frames: usize,
    /// Frames still to leave out (`SETTLE_FRAMES` after a restart).
    settle: usize,
    pub integration: usize,
    pub mode: TiiMode,
    /// Last integrated pair magnitudes (192 values), for the spectrum plot.
    pub last_pairs: Vec<f32>,
}

impl Default for TiiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TiiDecoder {
    pub fn new() -> Self {
        // (carriers of the blocks: -768, -384, 1, 385; a quadrant of pi/2 each)
        let turn = [-768i32, -384, 1, 385]
            .iter()
            .flat_map(|&k| (0..PAIRS as i32).map(move |i| k + 2 * i))
            .map(|k| match (prs_phase_quadrant(k) - prs_phase_quadrant(k + 1)) & 3 {
                0 => Complex32::new(1.0, 0.0),
                1 => Complex32::new(0.0, -1.0),
                2 => Complex32::new(-1.0, 0.0),
                _ => Complex32::new(0.0, 1.0),
            })
            .collect();
        TiiDecoder {
            acc: vec![Complex32::new(0.0, 0.0); PAIRS],
            acc_own: vec![Complex32::new(0.0, 0.0); PAIRS],
            turn,
            pair_power: vec![0.0; PAIRS],
            null_power: 0.0,
            frames: 0,
            settle: SETTLE_FRAMES,
            integration: 16,
            mode: TiiMode::Sensitive,
            last_pairs: vec![0.0; PAIRS],
        }
    }

    pub fn reset(&mut self) {
        self.acc.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        self.acc_own.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        self.pair_power.iter_mut().for_each(|p| *p = 0.0);
        self.null_power = 0.0;
        self.frames = 0;
    }

    /// Another signal (a retune, synchronisation regained): the frames of the
    /// next moments are left out until the frequency has settled.
    pub fn restart(&mut self) {
        self.reset();
        self.settle = SETTLE_FRAMES;
    }

    /// Feed the FFT of a null symbol; returns detected codes after each integration period.
    pub fn push(&mut self, null_fft: &[Complex32]) -> Option<Vec<TiiCode>> {
        debug_assert_eq!(null_fft.len(), T_U);
        if self.settle > 0 {
            self.settle -= 1;
            return None;
        }
        const STARTS: [usize; 4] = [T_U - 768, T_U - 384, 1, 385];
        for (j, &k) in STARTS.iter().enumerate() {
            for i in 0..PAIRS {
                let (a, b) = (null_fft[k + 2 * i], null_fft[k + 2 * i + 1]);
                let p = a * b.conj();
                self.acc[i] += p;
                self.acc_own[i] += p * self.turn[j * PAIRS + i];
                self.pair_power[i] += (a.norm_sqr() + b.norm_sqr()) as f64;
            }
        }
        // (the four blocks are the 1536 active carriers)
        let pw: f64 = STARTS.iter().flat_map(|&k| null_fft[k..k + 384].iter()).map(|c| c.norm_sqr() as f64).sum();
        self.null_power += pw / (4.0 * 384.0);
        self.frames += 1;
        if self.frames < self.integration {
            return None;
        }
        // magnitude rather than the real part: the absolute phase of a pair depends
        // on the FFT window position inside the null symbol
        let mags: Vec<f32> = self.acc.iter().map(|c| c.norm()).collect();
        let mags_own: Vec<f32> = self.acc_own.iter().map(|c| c.norm()).collect();
        self.last_pairs = mags.iter().zip(&mags_own).map(|(a, b)| a.max(*b) / self.frames as f32).collect();
        // (per carrier, averaged over the blocks and frames)
        let n = (8 * self.frames) as f64;
        let carriers: Vec<f32> = self.pair_power.iter().map(|p| (p / n) as f32).collect();
        let null_mean = (self.null_power / self.frames as f64) as f32;
        self.reset();
        // (a comb gives one code: the stronger of the two ways)
        let mut codes = self.detect(&mags);
        for c in self.detect(&mags_own) {
            match codes.iter_mut().find(|x| x.sub == c.sub) {
                Some(x) if x.strength >= c.strength => {}
                Some(x) => *x = c,
                None => codes.push(c),
            }
        }
        if let Some(max) = codes.iter().map(|c| c.strength).reduce(f32::max) {
            for c in codes.iter_mut() {
                c.level_db = 10.0 * (c.strength / max).log10();
            }
        }
        codes.sort_by(|a, b| b.strength.total_cmp(&a.strength));
        for c in codes.iter_mut() {
            c.snr_db = own_snr(c.main, c.sub, &carriers, null_mean);
        }
        Some(codes)
    }

    fn detect(&self, mags: &[f32]) -> Vec<TiiCode> {
        // noise: median of all pair magnitudes (most pairs are unused)
        let mut sorted = mags.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let noise = sorted[PAIRS / 2].max(1e-12);
        let (min_ratio, min_gap) = match self.mode {
            TiiMode::Sensitive => (4.0, 1.8),
            TiiMode::Reliable => (8.0, 3.0),
        };
        let mut found = Vec::new();
        for comb in 0..24usize {
            let v: Vec<f32> = (0..8).map(|b| mags[comb + 24 * b]).collect();
            let mut idx: Vec<usize> = (0..8).collect();
            idx.sort_by(|&a, &b| v[b].total_cmp(&v[a]));
            let fourth = v[idx[3]];
            let fifth = v[idx[4]];
            if fourth < min_ratio * noise || fourth < min_gap * fifth.max(noise) {
                continue;
            }
            let mut pattern = 0u8;
            for &b in &idx[..4] {
                pattern |= 0x80 >> b;
            }
            if let Some(main) = pattern_to_main(pattern) {
                let strength: f32 = idx[..4].iter().map(|&b| v[b]).sum::<f32>() / 4.0;
                found.push(TiiCode {
                    main,
                    sub: comb as u8,
                    level_db: 0.0,
                    strength,
                    margin_db: 10.0 * (fourth / noise).log10(),
                    snr_db: None,
                });
            }
        }
        if let Some(max) = found.iter().map(|c| c.strength).reduce(f32::max) {
            for c in found.iter_mut() {
                c.level_db = 10.0 * (c.strength / max).log10();
            }
        }
        found.sort_by(|a, b| b.strength.total_cmp(&a.strength));
        found
    }
}

/// Power of a TII carrier over a data carrier of the same transmitter, in dB.
/// Measured: two recordings of different networks (one transmitter, and one
/// strong with two weak) and one of them with added noise from 20 to 7 dB SNR,
/// against the multiplex SNR: 3.0 to 3.7 dB lower, the same at every level.
pub const TII_CARRIER_DB: f32 = -3.0;

/// A transmitter's own SNR from its TII carriers: their mean power (the pairs
/// of its pattern) less the noise there (the median pair: most pairs are
/// unused), over the mean power of the null symbol. Power rather than the
/// coherent pair sums of the detection: those lose part of the signal where
/// the channel phase drifts, and the loss depends on the channel.
fn own_snr(main: u8, sub: u8, carriers: &[f32], null_mean: f32) -> Option<f32> {
    if null_mean <= 0.0 || sub >= 24 {
        return None;
    }
    let pattern = tii_pattern(main);
    let own: f32 = (0..8).filter(|b| pattern & (0x80 >> b) != 0).map(|b| carriers[sub as usize + 24 * b]).sum::<f32>() / 4.0;
    let mut sorted = carriers.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let carrier = own - sorted[PAIRS / 2];
    if carrier <= 0.0 {
        return None;
    }
    let snr = 10.0 * (carrier / null_mean).log10() - TII_CARRIER_DB;
    Some(snr.clamp(-20.0, 60.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert_eq!(tii_pattern(0), 0b0000_1111);
        assert_eq!(tii_pattern(1), 0b0001_0111);
        assert_eq!(tii_pattern(69), 0b1111_0000);
        for m in 0..70 {
            assert_eq!(pattern_to_main(tii_pattern(m)), Some(m));
        }
    }

    /// After a restart the first frames are left out: a result only after
    /// the settling and a whole integration period.
    #[test]
    fn frames_after_a_restart_left_out() {
        let fft = vec![Complex32::new(1.0, 0.0); T_U];
        let mut dec = TiiDecoder::new();
        for _ in 0..3 {
            dec.push(&fft);
        }
        dec.restart();
        for i in 0..SETTLE_FRAMES + dec.integration - 1 {
            assert!(dec.push(&fft).is_none(), "a result at frame {i}");
        }
        assert!(dec.push(&fft).is_some());
    }

    /// Null symbols with one code over unit noise, its pairs as `own` says:
    /// the second carrier with the phase reference phase of its own carrier,
    /// else with that of the first; a phase of the frame and a delay over all.
    fn code_frames(main: u8, sub: usize, power: f32, own: bool, frames: usize) -> Option<Vec<TiiCode>> {
        let mut seed = 0x9e37_79b9u64;
        let mut uniform = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 11) as f64 / (1u64 << 53) as f64).max(1e-12)
        };
        let tau = std::f32::consts::TAU;
        let mut dec = TiiDecoder::new();
        let mut codes = None;
        for _ in 0..SETTLE_FRAMES + frames {
            let mut fft = vec![Complex32::new(0.0, 0.0); T_U];
            let frame = (tau as f64 * uniform()) as f32;
            for (j, k0) in [-768i32, -384, 1, 385].into_iter().enumerate() {
                let bin0 = [T_U - 768, T_U - 384, 1, 385][j];
                for bin in bin0..bin0 + 384 {
                    let (r, ph) = ((-uniform().ln()).sqrt() as f32, (tau as f64 * uniform()) as f32);
                    fft[bin] = Complex32::from_polar(r, ph);
                }
                for b in (0..8).filter(|b| tii_pattern(main) & (0x80 >> b) != 0) {
                    let i = sub + 24 * b;
                    let k = k0 + 2 * i as i32;
                    let phase = |q: i32| q as f32 * std::f32::consts::FRAC_PI_2;
                    let second = if own { prs_phase_quadrant(k + 1) } else { prs_phase_quadrant(k) };
                    // (a delay of 12 samples: the phase turns from carrier to carrier)
                    let delay = |k: i32| -tau * 12.0 * k as f32 / T_U as f32;
                    fft[bin0 + 2 * i] += Complex32::from_polar(power.sqrt(), frame + phase(prs_phase_quadrant(k)) + delay(k));
                    fft[bin0 + 2 * i + 1] += Complex32::from_polar(power.sqrt(), frame + phase(second) + delay(k + 1));
                }
            }
            codes = dec.push(&fft).or(codes);
        }
        codes
    }

    /// Both ways of giving the second carrier of a pair its phase are found.
    #[test]
    fn a_code_with_either_phase_of_the_second_carrier() {
        for own in [false, true] {
            let codes = code_frames(37, 6, 1.5, own, 16).expect("a result");
            assert_eq!(codes.len(), 1, "own {own}: {codes:?}");
            assert_eq!((codes[0].main, codes[0].sub), (37, 6), "own {own}");
        }
    }

    /// A code at a known carrier power over noise comes out with its own SNR.
    #[test]
    fn own_snr_of_a_code() {
        let mut seed = 0x1234_5678u64;
        let mut uniform = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 11) as f64 / (1u64 << 53) as f64).max(1e-12)
        };
        let (main, sub, tii_power) = (5u8, 3usize, 10.0f32);
        let mut dec = TiiDecoder::new();
        let mut codes = None;
        for _ in 0..SETTLE_FRAMES + dec.integration {
            let mut fft = vec![Complex32::new(0.0, 0.0); T_U];
            for k in [T_U - 768, T_U - 384, 1, 385] {
                for bin in k..k + 384 {
                    // unit noise power per carrier
                    let (r, ph) = ((-uniform().ln()).sqrt() as f32, (2.0 * std::f64::consts::PI * uniform()) as f32);
                    fft[bin] = Complex32::from_polar(r, ph);
                }
                for b in (0..8).filter(|b| tii_pattern(main) & (0x80 >> b) != 0) {
                    let i = sub + 24 * b;
                    let c = Complex32::from_polar(tii_power.sqrt(), (2.0 * std::f64::consts::PI * uniform()) as f32);
                    fft[k + 2 * i] += c;
                    fft[k + 2 * i + 1] += c;
                }
            }
            codes = dec.push(&fft).or(codes);
        }
        let codes = codes.expect("a result after the integration");
        let c = codes.iter().find(|c| c.main == main && c.sub as usize == sub).expect("the code found");
        // its carriers over the null symbol (noise plus the 32 TII carriers), on the data carrier scale
        let null_mean = 1.0 + 32.0 * tii_power / 1536.0;
        let want = 10.0 * (tii_power / null_mean).log10() - TII_CARRIER_DB;
        let got = c.snr_db.expect("own SNR");
        assert!((got - want).abs() < 0.7, "own SNR {got:.2} dB, want {want:.2} dB");
    }
}
