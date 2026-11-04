//! Soft decision Viterbi decoder for the DAB mother code
//! (K = 7, rate 1/4, polynomials 133, 171, 145, 133 octal).
//!
//! Soft inputs are `i8`, positive values mean bit 0, negative bit 1 and zero
//! is an erasure (punctured position).

const POLYS: [u8; 4] = [0o133, 0o171, 0o145, 0o133];
const STATES: usize = 64;

pub struct Viterbi {
    /// Expected output bits (4 bit pattern) for every 7 bit register value.
    outputs: [u8; 128],
    metrics: [i32; STATES],
    next: [i32; STATES],
    decisions: Vec<u64>,
}

impl Default for Viterbi {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn parity(x: u8) -> u8 {
    (x.count_ones() & 1) as u8
}

impl Viterbi {
    pub fn new() -> Self {
        let mut outputs = [0u8; 128];
        for (w, out) in outputs.iter_mut().enumerate() {
            // bit 6 = newest input bit a_i, bit 0 = a_{i-6}
            let mut o = 0u8;
            for (j, &p) in POLYS.iter().enumerate() {
                o |= parity(w as u8 & p) << (3 - j);
            }
            *out = o;
        }
        Viterbi { outputs, metrics: [0; STATES], next: [0; STATES], decisions: Vec::new() }
    }

    /// Decode `soft` (length 4 * (n + 6)) into `n` bits. The encoder is
    /// assumed to start and end in state zero (6 tail bits).
    pub fn decode(&mut self, soft: &[i8], out: &mut Vec<u8>) {
        let steps = soft.len() / 4;
        out.clear();
        if steps < 6 {
            return;
        }
        self.metrics = [i32::MIN / 2; STATES];
        self.metrics[0] = 0;
        self.decisions.clear();
        self.decisions.reserve(steps);

        let mut bm = [0i32; 16];
        for step in 0..steps {
            let s = &soft[step * 4..step * 4 + 4];
            let r = [s[0] as i32, s[1] as i32, s[2] as i32, s[3] as i32];
            for (pattern, m) in bm.iter_mut().enumerate() {
                // expected bit 0 -> +r, expected bit 1 -> -r
                let mut acc = 0;
                for j in 0..4 {
                    let bit = (pattern >> (3 - j)) & 1;
                    acc += if bit == 0 { r[j] } else { -r[j] };
                }
                *m = acc;
            }
            let mut dec = 0u64;
            // butterflies: predecessors 2i, 2i+1 -> successors i (input 0), i+32 (input 1)
            for i in 0..32 {
                let p0 = 2 * i;
                let p1 = 2 * i + 1;
                // successor i, input bit 0: w = (0 << 6) | p
                let m00 = self.metrics[p0] + bm[self.outputs[p0] as usize];
                let m01 = self.metrics[p1] + bm[self.outputs[p1] as usize];
                // successor i + 32, input bit 1: w = (1 << 6) | p
                let m10 = self.metrics[p0] + bm[self.outputs[64 | p0] as usize];
                let m11 = self.metrics[p1] + bm[self.outputs[64 | p1] as usize];
                if m01 > m00 {
                    self.next[i] = m01;
                    dec |= 1 << i;
                } else {
                    self.next[i] = m00;
                }
                if m11 > m10 {
                    self.next[i + 32] = m11;
                    dec |= 1 << (i + 32);
                } else {
                    self.next[i + 32] = m10;
                }
            }
            std::mem::swap(&mut self.metrics, &mut self.next);
            // keep metrics bounded
            if step & 63 == 63 {
                let max = *self.metrics.iter().max().unwrap();
                for m in self.metrics.iter_mut() {
                    *m -= max;
                }
            }
            self.decisions.push(dec);
        }

        // trace back from state 0 (terminated trellis)
        let mut bits = vec![0u8; steps];
        let mut state = 0usize;
        for step in (0..steps).rev() {
            bits[step] = (state >> 5) as u8;
            let b = ((self.decisions[step] >> state) & 1) as usize;
            state = ((state << 1) & 0x3f) | b;
        }
        out.extend_from_slice(&bits[..steps - 6]);
    }
}

/// Reference encoder, used by tests.
pub fn encode(bits: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((bits.len() + 6) * 4);
    let mut reg: u8 = 0;
    for &b in bits.iter().chain([0u8; 6].iter()) {
        reg = ((b & 1) << 6) | (reg >> 1);
        for &p in POLYS.iter() {
            out.push(parity(reg & p));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_errors() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let bits: Vec<u8> = (0..768).map(|_| rnd() & 1).collect();
        let coded = encode(&bits);
        let mut soft: Vec<i8> = coded.iter().map(|&b| if b == 0 { 60 } else { -60 }).collect();
        // flip a few symbols and erase others
        for i in (0..soft.len()).step_by(37) {
            soft[i] = -soft[i];
        }
        for i in (5..soft.len()).step_by(11) {
            soft[i] = 0;
        }
        let mut v = Viterbi::new();
        let mut out = Vec::new();
        v.decode(&soft, &mut out);
        assert_eq!(out, bits);
    }
}
