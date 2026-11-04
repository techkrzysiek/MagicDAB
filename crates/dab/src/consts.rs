//! DAB transmission mode I constants, band III channel table, phase reference
//! symbol and frequency interleaving (ETSI EN 300 401 clause 14).

use num_complex::Complex32;
use std::f32::consts::FRAC_PI_2;

/// Sample rate of the baseband signal.
pub const SAMPLE_RATE: u32 = 2_048_000;
/// Useful symbol length (FFT size).
pub const T_U: usize = 2048;
/// Guard interval length.
pub const T_G: usize = 504;
/// Total symbol length.
pub const T_S: usize = T_U + T_G;
/// Null symbol length.
pub const T_NULL: usize = 2656;
/// Transmission frame length (96 ms).
pub const T_F: usize = 196_608;
/// Number of active carriers.
pub const K: usize = 1536;
/// OFDM symbols per frame, excluding the null symbol.
pub const L: usize = 76;
/// Number of FIC symbols (mode I).
pub const FIC_SYMBOLS: usize = 3;
/// Number of MSC symbols (mode I).
pub const MSC_SYMBOLS: usize = 72;
/// Soft bits carried by one OFDM symbol.
pub const BITS_PER_SYMBOL: usize = 2 * K;
/// Number of CIFs per transmission frame (mode I).
pub const CIFS_PER_FRAME: usize = 4;
/// Capacity units in one CIF.
pub const CIF_CUS: usize = 864;
/// Bits per capacity unit.
pub const CU_BITS: usize = 64;
/// Bits per CIF.
pub const CIF_BITS: usize = CIF_CUS * CU_BITS;
/// FIC soft bits per frame.
pub const FIC_BITS: usize = FIC_SYMBOLS * BITS_PER_SYMBOL;
/// Carrier spacing in Hz.
pub const CARRIER_SPACING: f32 = 1000.0;

/// DAB band III channel (name, centre frequency in kHz).
pub const BAND_III: &[(&str, u32)] = &[
    ("5A", 174_928),
    ("5B", 176_640),
    ("5C", 178_352),
    ("5D", 180_064),
    ("6A", 181_936),
    ("6B", 183_648),
    ("6C", 185_360),
    ("6D", 187_072),
    ("7A", 188_928),
    ("7B", 190_640),
    ("7C", 192_352),
    ("7D", 194_064),
    ("8A", 195_936),
    ("8B", 197_648),
    ("8C", 199_360),
    ("8D", 201_072),
    ("9A", 202_928),
    ("9B", 204_640),
    ("9C", 206_352),
    ("9D", 208_064),
    ("10A", 209_936),
    ("10N", 210_096),
    ("10B", 211_648),
    ("10C", 213_360),
    ("10D", 215_072),
    ("11A", 216_928),
    ("11N", 217_088),
    ("11B", 218_640),
    ("11C", 220_352),
    ("11D", 222_064),
    ("12A", 223_936),
    ("12N", 224_096),
    ("12B", 225_648),
    ("12C", 227_360),
    ("12D", 229_072),
    ("13A", 230_784),
    ("13B", 232_496),
    ("13C", 234_208),
    ("13D", 235_776),
    ("13E", 237_488),
    ("13F", 239_200),
];

pub fn channel_frequency(name: &str) -> Option<u32> {
    BAND_III.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|&(_, f)| f)
}

pub fn channel_name(freq_khz: u32) -> Option<&'static str> {
    BAND_III.iter().find(|&&(_, f)| f == freq_khz).map(|&(n, _)| n)
}

/// FFT bin index of carrier `k` (-768..=768, k != 0).
#[inline]
pub fn carrier_bin(k: i32) -> usize {
    if k < 0 {
        (k + T_U as i32) as usize
    } else {
        k as usize
    }
}

/// Iterator over active carrier indices in ascending order.
pub fn carriers() -> impl Iterator<Item = i32> {
    (-(K as i32) / 2..=(K as i32) / 2).filter(|&k| k != 0)
}

// Phase reference symbol, table 23 of EN 300 401: (k_min, i, n), k' = k_min.
const PRS_TABLE: [(i32, usize, i32); 48] = [
    (-768, 0, 1),
    (-736, 1, 2),
    (-704, 2, 0),
    (-672, 3, 1),
    (-640, 0, 3),
    (-608, 1, 2),
    (-576, 2, 2),
    (-544, 3, 3),
    (-512, 0, 2),
    (-480, 1, 1),
    (-448, 2, 2),
    (-416, 3, 3),
    (-384, 0, 1),
    (-352, 1, 2),
    (-320, 2, 3),
    (-288, 3, 3),
    (-256, 0, 2),
    (-224, 1, 2),
    (-192, 2, 2),
    (-160, 3, 1),
    (-128, 0, 1),
    (-96, 1, 3),
    (-64, 2, 1),
    (-32, 3, 2),
    (1, 0, 3),
    (33, 3, 1),
    (65, 2, 1),
    (97, 1, 1),
    (129, 0, 2),
    (161, 3, 2),
    (193, 2, 1),
    (225, 1, 0),
    (257, 0, 2),
    (289, 3, 2),
    (321, 2, 3),
    (353, 1, 3),
    (385, 0, 0),
    (417, 3, 2),
    (449, 2, 1),
    (481, 1, 3),
    (513, 0, 3),
    (545, 3, 3),
    (577, 2, 3),
    (609, 1, 0),
    (641, 0, 3),
    (673, 3, 0),
    (705, 2, 1),
    (737, 1, 1),
];

const PRS_H: [[i32; 32]; 4] = [
    [0, 2, 0, 0, 0, 0, 1, 1, 2, 0, 0, 0, 2, 2, 1, 1, 0, 2, 0, 0, 0, 0, 1, 1, 2, 0, 0, 0, 2, 2, 1, 1],
    [0, 3, 2, 3, 0, 1, 3, 0, 2, 1, 2, 3, 2, 3, 3, 0, 0, 3, 2, 3, 0, 1, 3, 0, 2, 1, 2, 3, 2, 3, 3, 0],
    [0, 0, 0, 2, 0, 2, 1, 3, 2, 2, 0, 2, 2, 0, 1, 3, 0, 0, 0, 2, 0, 2, 1, 3, 2, 2, 0, 2, 2, 0, 1, 3],
    [0, 1, 2, 1, 0, 3, 3, 2, 2, 3, 2, 1, 2, 1, 3, 2, 0, 1, 2, 1, 0, 3, 3, 2, 2, 3, 2, 1, 2, 1, 3, 2],
];

/// Phase of the phase reference symbol on carrier `k` in multiples of pi/2.
pub fn prs_phase_quadrant(k: i32) -> i32 {
    for &(kmin, i, n) in PRS_TABLE.iter() {
        if k >= kmin && k < kmin + 32 {
            return (PRS_H[i][(k - kmin) as usize] + n) & 3;
        }
    }
    panic!("carrier {k} outside of mode I range")
}

/// Phase reference symbol in FFT bin order (inactive bins are zero).
pub fn prs_bins() -> Vec<Complex32> {
    let mut v = vec![Complex32::new(0.0, 0.0); T_U];
    for k in carriers() {
        let phi = prs_phase_quadrant(k) as f32 * FRAC_PI_2;
        v[carrier_bin(k)] = Complex32::from_polar(1.0, phi);
    }
    v
}

/// Frequency interleaving table: QPSK symbol n is carried on carrier `table[n]`.
pub fn frequency_interleaver() -> Vec<i32> {
    let mut table = Vec::with_capacity(K);
    let mut a = 0usize;
    for _ in 0..T_U {
        if (256..=1792).contains(&a) && a != 1024 {
            table.push(a as i32 - 1024);
        }
        a = (13 * a + 511) % T_U;
    }
    debug_assert_eq!(table.len(), K);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaver_is_permutation() {
        let t = frequency_interleaver();
        let mut seen = std::collections::HashSet::new();
        for &k in &t {
            assert!(k != 0 && k.abs() <= 768);
            assert!(seen.insert(k));
        }
        assert_eq!(seen.len(), K);
    }

    #[test]
    fn channel_lookup() {
        assert_eq!(channel_frequency("12c"), Some(227_360));
        assert_eq!(channel_name(178_352), Some("5C"));
    }
}
