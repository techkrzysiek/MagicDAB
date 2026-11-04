//! Transmitter Identification Information decoding from the null symbol
//! (EN 300 401 clause 14.8, transmission mode I).
//!
//! The TII signal consists of carrier pairs; both carriers of a pair carry the
//! same phase, so X[k] * conj(X[k+1]) accumulates coherently while noise does
//! not. The four 384 carrier blocks repeat the same comb/pattern and are summed.
//! Results are integrated over several frames before detection.

use crate::consts::T_U;
use num_complex::Complex32;
use serde::{Deserialize, Serialize};

pub const PAIRS: usize = 192;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TiiMode {
    Sensitive,
    Reliable,
}

pub struct TiiDecoder {
    acc: Vec<Complex32>,
    frames: usize,
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
        TiiDecoder {
            acc: vec![Complex32::new(0.0, 0.0); PAIRS],
            frames: 0,
            integration: 16,
            mode: TiiMode::Sensitive,
            last_pairs: vec![0.0; PAIRS],
        }
    }

    pub fn reset(&mut self) {
        self.acc.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        self.frames = 0;
    }

    /// Feed the FFT of a null symbol; returns detected codes after each integration period.
    pub fn push(&mut self, null_fft: &[Complex32]) -> Option<Vec<TiiCode>> {
        debug_assert_eq!(null_fft.len(), T_U);
        const STARTS: [usize; 4] = [T_U - 768, T_U - 384, 1, 385];
        for &k in STARTS.iter() {
            for i in 0..PAIRS {
                self.acc[i] += null_fft[k + 2 * i] * null_fft[k + 2 * i + 1].conj();
            }
        }
        self.frames += 1;
        if self.frames < self.integration {
            return None;
        }
        // magnitude rather than the real part: the absolute phase of a pair depends
        // on the FFT window position inside the null symbol
        let mags: Vec<f32> = self.acc.iter().map(|c| c.norm()).collect();
        self.last_pairs = mags.iter().map(|m| m / self.frames as f32).collect();
        self.reset();
        Some(self.detect(&mags))
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
                found.push(TiiCode { main, sub: comb as u8, level_db: 0.0, strength, margin_db: 10.0 * (fourth / noise).log10() });
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
}
