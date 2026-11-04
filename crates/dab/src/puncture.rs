//! Puncturing vectors and protection profiles (EN 300 401 clause 11).

use serde::Serialize;

/// Puncturing vectors PI_1 .. PI_24 (32 bits each, MSB first).
const PI: [u32; 24] = [
    0xC888_8888,
    0xC888_C888,
    0xC8C8_C888,
    0xC8C8_C8C8,
    0xCCC8_C8C8,
    0xCCC8_CCC8,
    0xCCCC_CCC8,
    0xCCCC_CCCC,
    0xECCC_CCCC,
    0xECCC_ECCC,
    0xECEC_ECCC,
    0xECEC_ECEC,
    0xEEEC_ECEC,
    0xEEEC_EEEC,
    0xEEEE_EEEC,
    0xEEEE_EEEE,
    0xFEEE_EEEE,
    0xFEEE_FEEE,
    0xFEFE_FEEE,
    0xFEFE_FEFE,
    0xFFFE_FEFE,
    0xFFFE_FFFE,
    0xFFFF_FFFE,
    0xFFFF_FFFF,
];
/// Tail puncturing vector PI_X (24 bits).
const PI_X: u32 = 0xCC_CCCC;

/// A depuncturing pattern: `true` for every mother code position that is transmitted.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub keep: Vec<bool>,
    pub transmitted: usize,
}

impl Pattern {
    fn new() -> Self {
        Pattern { keep: Vec::new(), transmitted: 0 }
    }

    fn push_blocks(&mut self, count: usize, pi: usize) {
        let vec = PI[pi - 1];
        for _ in 0..count * 4 {
            for b in (0..32).rev() {
                let k = (vec >> b) & 1 == 1;
                self.keep.push(k);
                self.transmitted += k as usize;
            }
        }
    }

    fn push_tail(&mut self) {
        for b in (0..24).rev() {
            let k = (PI_X >> b) & 1 == 1;
            self.keep.push(k);
            self.transmitted += k as usize;
        }
    }

    /// Number of decoded information bits.
    pub fn info_bits(&self) -> usize {
        self.keep.len() / 4 - 6
    }

    /// Expand punctured soft bits into the mother code, erasures are zero.
    pub fn depuncture(&self, input: &[i8], out: &mut Vec<i8>) {
        out.clear();
        out.reserve(self.keep.len());
        let mut it = input.iter();
        for &k in &self.keep {
            if k {
                out.push(*it.next().unwrap_or(&0));
            } else {
                out.push(0);
            }
        }
    }
}

/// Pattern of one FIC block in mode I (2304 bits -> 768 information bits).
pub fn fic_pattern() -> Pattern {
    let mut p = Pattern::new();
    p.push_blocks(21, 16);
    p.push_blocks(3, 15);
    p.push_tail();
    debug_assert_eq!(p.transmitted, 2304);
    p
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Protection {
    /// Unequal error protection, table index from FIG 0/1 short form.
    Uep { index: u8 },
    /// Equal error protection, `option` 0 = set A, 1 = set B, `level` 0..3 (1..4).
    Eep { option: u8, level: u8 },
}

/// UEP table (EN 300 401 table 6): size CU, protection level, bitrate, L1..L4, PI1..PI4.
const UEP: [(u16, u8, u16, [u8; 4], [u8; 4]); 64] = [
    (16, 5, 32, [3, 4, 17, 0], [5, 3, 2, 0]),
    (21, 4, 32, [3, 3, 18, 0], [11, 6, 5, 0]),
    (24, 3, 32, [3, 4, 14, 3], [15, 9, 6, 8]),
    (29, 2, 32, [3, 4, 14, 3], [22, 13, 8, 13]),
    (35, 1, 32, [3, 5, 13, 3], [24, 17, 12, 17]),
    (24, 5, 48, [4, 3, 26, 3], [5, 4, 2, 3]),
    (29, 4, 48, [3, 4, 26, 3], [9, 6, 4, 6]),
    (35, 3, 48, [3, 4, 26, 3], [15, 10, 6, 9]),
    (42, 2, 48, [3, 4, 26, 3], [24, 14, 8, 15]),
    (52, 1, 48, [3, 5, 25, 3], [24, 18, 13, 18]),
    (29, 5, 56, [6, 10, 23, 3], [5, 4, 2, 3]),
    (35, 4, 56, [6, 10, 23, 3], [9, 6, 4, 5]),
    (42, 3, 56, [6, 12, 21, 3], [16, 7, 6, 9]),
    (52, 2, 56, [6, 10, 23, 3], [23, 13, 8, 13]),
    (32, 5, 64, [6, 9, 31, 2], [5, 3, 2, 3]),
    (42, 4, 64, [6, 9, 33, 0], [11, 6, 5, 0]),
    (48, 3, 64, [6, 12, 27, 3], [16, 8, 6, 9]),
    (58, 2, 64, [6, 10, 29, 3], [23, 13, 8, 13]),
    (70, 1, 64, [6, 11, 28, 3], [24, 18, 12, 18]),
    (40, 5, 80, [6, 10, 41, 3], [6, 3, 2, 3]),
    (52, 4, 80, [6, 10, 41, 3], [11, 6, 5, 6]),
    (58, 3, 80, [6, 11, 40, 3], [16, 8, 6, 7]),
    (70, 2, 80, [6, 10, 41, 3], [23, 13, 8, 13]),
    (84, 1, 80, [6, 10, 41, 3], [24, 17, 12, 18]),
    (48, 5, 96, [7, 9, 53, 3], [5, 4, 2, 4]),
    (58, 4, 96, [7, 10, 52, 3], [9, 6, 4, 6]),
    (70, 3, 96, [6, 12, 51, 3], [16, 9, 6, 10]),
    (84, 2, 96, [6, 10, 53, 3], [22, 12, 9, 12]),
    (104, 1, 96, [6, 13, 50, 3], [24, 18, 13, 19]),
    (58, 5, 112, [14, 17, 50, 3], [5, 4, 2, 5]),
    (70, 4, 112, [11, 21, 49, 3], [9, 6, 4, 8]),
    (84, 3, 112, [11, 23, 47, 3], [16, 8, 6, 9]),
    (104, 2, 112, [11, 21, 49, 3], [23, 12, 9, 14]),
    (64, 5, 128, [12, 19, 62, 3], [5, 3, 2, 4]),
    (84, 4, 128, [11, 21, 61, 3], [11, 6, 5, 7]),
    (96, 3, 128, [11, 22, 60, 3], [16, 9, 6, 10]),
    (116, 2, 128, [11, 21, 61, 3], [22, 12, 9, 14]),
    (140, 1, 128, [11, 20, 62, 3], [24, 17, 13, 19]),
    (80, 5, 160, [11, 19, 87, 3], [5, 4, 2, 4]),
    (104, 4, 160, [11, 23, 83, 3], [11, 6, 5, 9]),
    (116, 3, 160, [11, 24, 82, 3], [16, 8, 6, 11]),
    (140, 2, 160, [11, 21, 85, 3], [22, 11, 9, 13]),
    (168, 1, 160, [11, 22, 84, 3], [24, 18, 12, 19]),
    (96, 5, 192, [11, 20, 110, 3], [6, 4, 2, 5]),
    (116, 4, 192, [11, 22, 108, 3], [10, 6, 4, 9]),
    (140, 3, 192, [11, 24, 106, 3], [16, 10, 6, 11]),
    (168, 2, 192, [11, 20, 110, 3], [22, 13, 9, 13]),
    (208, 1, 192, [11, 21, 109, 3], [24, 20, 13, 24]),
    (116, 5, 224, [12, 22, 131, 3], [8, 6, 2, 6]),
    (140, 4, 224, [12, 26, 127, 3], [12, 8, 4, 11]),
    (168, 3, 224, [11, 20, 134, 3], [16, 10, 7, 9]),
    (208, 2, 224, [11, 22, 132, 3], [24, 16, 10, 15]),
    (232, 1, 224, [11, 24, 130, 3], [24, 20, 12, 20]),
    (128, 5, 256, [11, 24, 154, 3], [6, 5, 2, 5]),
    (168, 4, 256, [11, 24, 154, 3], [12, 9, 5, 10]),
    (192, 3, 256, [11, 27, 151, 3], [16, 10, 7, 10]),
    (232, 2, 256, [11, 22, 156, 3], [24, 14, 10, 13]),
    (280, 1, 256, [11, 26, 152, 3], [24, 19, 14, 18]),
    (160, 5, 320, [11, 26, 200, 3], [8, 5, 2, 6]),
    (208, 4, 320, [11, 25, 201, 3], [13, 9, 5, 10]),
    (280, 2, 320, [11, 26, 200, 3], [24, 17, 9, 17]),
    (192, 5, 384, [11, 27, 247, 3], [8, 6, 2, 7]),
    (280, 3, 384, [11, 24, 250, 3], [16, 9, 7, 10]),
    (416, 1, 384, [12, 28, 245, 3], [24, 20, 14, 23]),
];

/// Size in CU, bitrate in kbit/s and protection level (1..5) of a UEP table entry.
pub fn uep_info(index: u8) -> Option<(u16, u16, u8)> {
    UEP.get(index as usize).map(|e| (e.0, e.2, e.1))
}

/// Bitrate in kbit/s of an EEP sub-channel of `size` CUs.
pub fn eep_bitrate(option: u8, level: u8, size: u16) -> Option<u16> {
    let size = size as u32;
    let (unit, cu) = match (option, level) {
        (0, 0) => (8, 12),
        (0, 1) => (8, 8),
        (0, 2) => (8, 6),
        (0, 3) => (8, 4),
        (1, 0) => (32, 27),
        (1, 1) => (32, 21),
        (1, 2) => (32, 18),
        (1, 3) => (32, 15),
        _ => return None,
    };
    if size == 0 || size % cu != 0 {
        return None;
    }
    Some((size / cu * unit) as u16)
}

/// Code rate as a human readable fraction for EEP profiles.
pub fn eep_code_rate(option: u8, level: u8) -> &'static str {
    match (option, level) {
        (0, 0) => "1/4",
        (0, 1) => "3/8",
        (0, 2) => "1/2",
        (0, 3) => "3/4",
        (1, 0) => "4/9",
        (1, 1) => "4/7",
        (1, 2) => "2/3",
        (1, 3) => "4/5",
        _ => "?",
    }
}

/// Build the depuncturing pattern for a sub-channel.
pub fn subchannel_pattern(prot: Protection, size_cu: u16, bitrate: u16) -> Option<Pattern> {
    let mut p = Pattern::new();
    match prot {
        Protection::Uep { index } => {
            let e = UEP.get(index as usize)?;
            for i in 0..4 {
                if e.3[i] > 0 {
                    p.push_blocks(e.3[i] as usize, e.4[i] as usize);
                }
            }
        }
        Protection::Eep { option, level } => {
            let (l1, l2, pi1, pi2) = if option == 0 {
                let n = bitrate as usize / 8;
                match level {
                    0 => (6 * n - 3, 3, 24, 23),
                    1 => {
                        if n == 1 {
                            (5, 1, 13, 12)
                        } else {
                            (2 * n - 3, 4 * n + 3, 14, 13)
                        }
                    }
                    2 => (6 * n - 3, 3, 8, 7),
                    3 => (4 * n - 3, 2 * n + 3, 3, 2),
                    _ => return None,
                }
            } else {
                let n = bitrate as usize / 32;
                match level {
                    0 => (24 * n - 3, 3, 10, 9),
                    1 => (24 * n - 3, 3, 6, 5),
                    2 => (24 * n - 3, 3, 4, 3),
                    3 => (24 * n - 3, 3, 2, 1),
                    _ => return None,
                }
            };
            p.push_blocks(l1, pi1);
            p.push_blocks(l2, pi2);
        }
    }
    p.push_tail();
    if p.transmitted > size_cu as usize * 64 || p.info_bits() != bitrate as usize * 24 {
        return None;
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_vectors_have_expected_weight() {
        for (i, v) in PI.iter().enumerate() {
            assert_eq!(v.count_ones() as usize, i + 9);
        }
    }

    #[test]
    fn uep_table_is_consistent() {
        for i in 0..64u8 {
            let (size, bitrate, _) = uep_info(i).unwrap();
            let p = subchannel_pattern(Protection::Uep { index: i }, size, bitrate).unwrap();
            assert!(size as usize * 64 - p.transmitted <= 8, "index {i}");
        }
    }

    #[test]
    fn eep_sizes() {
        for option in 0..2u8 {
            for level in 0..4u8 {
                for n in 1..=12u16 {
                    let unit = if option == 0 { 8 } else { 32 };
                    let bitrate = n * unit;
                    let cu = [[12, 8, 6, 4], [27, 21, 18, 15]][option as usize][level as usize] * n;
                    if let Some(p) = subchannel_pattern(Protection::Eep { option, level }, cu, bitrate) {
                        assert_eq!(p.transmitted, cu as usize * 64, "opt {option} lvl {level} n {n}");
                    } else {
                        panic!("no pattern for opt {option} lvl {level} n {n}");
                    }
                }
            }
        }
    }
}
