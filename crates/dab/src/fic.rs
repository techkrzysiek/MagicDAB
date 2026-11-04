//! Fast Information Channel decoding: depuncturing, Viterbi decoding,
//! energy dispersal and FIB CRC check.

use crate::consts::FIC_BITS;
use crate::crc::{check_crc16, pack_bits, prbs};
use crate::puncture::{fic_pattern, Pattern};
use crate::viterbi::{encode, Viterbi};

pub const FIB_BYTES: usize = 32;

pub struct FicDecoder {
    pattern: Pattern,
    viterbi: Viterbi,
    prbs: Vec<u8>,
    mother: Vec<i8>,
    bits: Vec<u8>,
    bytes: Vec<u8>,
    /// The FIBs of the last frame as decoded, 32 bytes each with their CRC
    /// (12 in mode I: 3 per CIF), good or not; an ETI recording writes them.
    pub raw: Vec<u8>,
    pub fib_total: u64,
    pub fib_errors: u64,
    /// Received code bits (not punctured) and those the Viterbi decoder
    /// corrected: the bit error rate of the channel before error correction.
    pub coded_bits: u64,
    pub coded_errors: u64,
}

impl Default for FicDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FicDecoder {
    pub fn new() -> Self {
        FicDecoder {
            pattern: fic_pattern(),
            viterbi: Viterbi::new(),
            prbs: prbs(768),
            mother: Vec::new(),
            bits: Vec::new(),
            bytes: Vec::new(),
            raw: Vec::new(),
            fib_total: 0,
            fib_errors: 0,
            coded_bits: 0,
            coded_errors: 0,
        }
    }

    /// Decode the FIC of one frame; `on_fib` receives every FIB with a valid CRC
    /// (30 bytes of FIG data). Returns the number of FIBs with CRC errors.
    pub fn decode(&mut self, soft: &[i8], mut on_fib: impl FnMut(&[u8])) -> usize {
        debug_assert_eq!(soft.len(), FIC_BITS);
        let mut errors = 0;
        self.raw.clear();
        for block in soft.chunks_exact(2304) {
            self.pattern.depuncture(block, &mut self.mother);
            self.viterbi.decode(&self.mother, &mut self.bits);
            // the decoded bits encoded again against the received hard decisions
            // (soft > 0 is a 0; punctured bits are 0 and not counted)
            for (&c, &s) in encode(&self.bits).iter().zip(self.mother.iter()) {
                if s != 0 {
                    self.coded_bits += 1;
                    self.coded_errors += ((s < 0) != (c == 1)) as u64;
                }
            }
            for (b, p) in self.bits.iter_mut().zip(self.prbs.iter()) {
                *b ^= p;
            }
            pack_bits(&self.bits, &mut self.bytes);
            self.raw.extend_from_slice(&self.bytes);
            for fib in self.bytes.chunks_exact(FIB_BYTES) {
                self.fib_total += 1;
                if check_crc16(fib) {
                    on_fib(&fib[..30]);
                } else {
                    self.fib_errors += 1;
                    errors += 1;
                }
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_bit_errors_are_counted() {
        let pattern = fic_pattern();
        let mut seed = 7u32;
        let mut soft = Vec::with_capacity(FIC_BITS);
        // the 4 blocks of a mode I frame
        for _ in 0..4 {
            let bits: Vec<u8> = (0..768)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8 & 1
                })
                .collect();
            // punctured like on air, as soft bits (+ = 0)
            let coded = encode(&bits);
            soft.extend(coded.iter().zip(&pattern.keep).filter(|(_, &k)| k).map(|(&c, _)| if c == 1 { -64i8 } else { 64 }));
        }
        assert_eq!(soft.len(), FIC_BITS);
        // 30 channel bit errors spread over the frame
        for i in 0..30 {
            soft[i * 307 + 11] = -soft[i * 307 + 11];
        }
        let mut dec = FicDecoder::new();
        dec.decode(&soft, |_| {});
        assert_eq!(dec.coded_bits, FIC_BITS as u64);
        assert_eq!(dec.coded_errors, 30);
    }
}
