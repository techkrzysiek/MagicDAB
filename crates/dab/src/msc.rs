//! MSC sub-channel decoding: CIF extraction, time deinterleaving,
//! depuncturing, Viterbi decoding and energy dispersal.

use crate::consts::{CIF_BITS, CU_BITS};
use crate::crc::{pack_bits, prbs};
use crate::ensemble::Subchannel;
use crate::puncture::{subchannel_pattern, Pattern};
use crate::viterbi::Viterbi;

const INTERLEAVE_MAP: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

pub struct SubchannelDecoder {
    pub subch: Subchannel,
    pattern: Pattern,
    buffers: Vec<Vec<i8>>,
    index: usize,
    filled: usize,
    deinterleaved: Vec<i8>,
    mother: Vec<i8>,
    bits: Vec<u8>,
    prbs: Vec<u8>,
    viterbi: Viterbi,
    pub out: Vec<u8>,
}

impl SubchannelDecoder {
    pub fn new(subch: &Subchannel) -> Option<Self> {
        let pattern = subchannel_pattern(subch.protection, subch.size, subch.bitrate)?;
        let n = subch.size as usize * CU_BITS;
        let info = pattern.info_bits();
        Some(SubchannelDecoder {
            subch: subch.clone(),
            pattern,
            buffers: vec![vec![0; n]; 16],
            index: 0,
            filled: 0,
            deinterleaved: vec![0; n],
            mother: Vec::new(),
            bits: Vec::new(),
            prbs: prbs(info),
            viterbi: Viterbi::new(),
            out: Vec::new(),
        })
    }

    /// Feed one CIF worth of soft bits (55296 values). Returns the decoded
    /// logical frame (bitrate * 3 bytes) once the deinterleaver is primed.
    pub fn push_cif(&mut self, cif: &[i8]) -> Option<&[u8]> {
        debug_assert_eq!(cif.len(), CIF_BITS);
        let start = self.subch.start as usize * CU_BITS;
        let n = self.subch.size as usize * CU_BITS;
        if start + n > cif.len() {
            return None;
        }
        let input = &cif[start..start + n];
        for i in 0..n {
            self.deinterleaved[i] = self.buffers[(self.index + INTERLEAVE_MAP[i & 15]) & 15][i];
        }
        self.buffers[self.index].copy_from_slice(input);
        self.index = (self.index + 1) & 15;
        if self.filled < 16 {
            self.filled += 1;
            return None;
        }
        self.pattern.depuncture(&self.deinterleaved, &mut self.mother);
        self.viterbi.decode(&self.mother, &mut self.bits);
        for (b, p) in self.bits.iter_mut().zip(self.prbs.iter()) {
            *b ^= p;
        }
        pack_bits(&self.bits, &mut self.out);
        Some(&self.out)
    }
}
