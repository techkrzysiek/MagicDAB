//! Soft decision Viterbi decoder for the DAB mother code
//! (K = 7, rate 1/4, polynomials 133, 171, 145, 133 octal).
//!
//! Soft inputs are `i8`, positive values mean bit 0, negative bit 1 and zero
//! is an erasure (punctured position).

const POLYS: [u8; 4] = [0o133, 0o171, 0o145, 0o133];
const STATES: usize = 64;

pub struct Viterbi {
    /// Expected output bits (4 bit pattern) of each butterfly with input 0
    /// from its even predecessor (register value 2i): the other three
    /// branches of the butterfly are its complement or itself, as every
    /// polynomial of the code has its first and last tap (see decode).
    outputs: [u8; 32],
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

/// Expected output bits for a 7 bit register value (bit 6 the newest input
/// bit a_i, bit 0 a_{i-6}).
fn output_bits(w: u8) -> u8 {
    let mut o = 0u8;
    for (j, &p) in POLYS.iter().enumerate() {
        o |= parity(w & p) << (3 - j);
    }
    o
}

impl Viterbi {
    pub fn new() -> Self {
        let mut outputs = [0u8; 32];
        for (i, out) in outputs.iter_mut().enumerate() {
            *out = output_bits(2 * i as u8);
        }
        Viterbi { outputs, decisions: Vec::new() }
    }

    /// Decode `soft` (length 4 * (n + 6)) into `n` bits. The encoder is
    /// assumed to start and end in state zero (6 tail bits).
    ///
    /// Every polynomial has its first and last tap, so the four branches of
    /// the butterfly of predecessors 2i, 2i+1 and successors i, i+32 expect
    /// the bits b, !b, !b, b: one branch metric per butterfly (the metric of
    /// the complement is its negative). The add-compare-select is then the
    /// same for all 32 butterflies, which the compiler turns into vector
    /// instructions (NEON, SSE).
    ///
    /// The path metrics are 16 bit, twice as many in a vector register (on
    /// a Raspberry Pi 3B+ a fifth faster than 32 bit, the same on x86). They
    /// wrap around and are compared by their difference, which stays far
    /// below 2^15 (the 64 states lie a few branch metrics of at most 508
    /// apart), so they need no normalisation; the states not reachable yet
    /// start 8192 below state 0 instead of minus infinity.
    pub fn decode(&mut self, soft: &[i8], out: &mut Vec<u8>) {
        let steps = soft.len() / 4;
        out.clear();
        if steps < 6 {
            return;
        }
        let mut m = [-8192i16; STATES];
        m[0] = 0;
        self.decisions.clear();
        self.decisions.reserve(steps);
        let mut bm = [0i16; 16];
        let mut even = [0i16; 32];
        let mut odd = [0i16; 32];
        let mut b = [0i16; 32];
        let mut take = [0u8; STATES];
        for step in 0..steps {
            let s = &soft[step * 4..step * 4 + 4];
            let r = [s[0] as i16, s[1] as i16, s[2] as i16, s[3] as i16];
            for (pattern, x) in bm.iter_mut().enumerate() {
                // expected bit 0 -> +r, expected bit 1 -> -r
                let mut acc = 0i16;
                for (j, &rj) in r.iter().enumerate() {
                    acc += if (pattern >> (3 - j)) & 1 == 0 { rj } else { -rj };
                }
                *x = acc;
            }
            for i in 0..32 {
                even[i] = m[2 * i];
                odd[i] = m[2 * i + 1];
                b[i] = bm[self.outputs[i] as usize];
            }
            // successor i: input 0 (from 2i: +b, from 2i+1: -b);
            // successor i + 32: input 1 (from 2i: -b, from 2i+1: +b)
            for i in 0..32 {
                let (m00, m01) = (even[i].wrapping_add(b[i]), odd[i].wrapping_sub(b[i]));
                let (m10, m11) = (even[i].wrapping_sub(b[i]), odd[i].wrapping_add(b[i]));
                let (t0, t1) = (m01.wrapping_sub(m00) > 0, m11.wrapping_sub(m10) > 0);
                m[i] = if t0 { m01 } else { m00 };
                m[i + 32] = if t1 { m11 } else { m10 };
                take[i] = t0 as u8;
                take[i + 32] = t1 as u8;
            }
            let mut dec = 0u64;
            for (i, &t) in take.iter().enumerate() {
                dec |= (t as u64) << i;
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

    // the decoder of before (four branch metrics per butterfly), as it was
    struct OldViterbi {
        /// Expected output bits (4 bit pattern) for every 7 bit register value.
        outputs: [u8; 128],
        metrics: [i32; STATES],
        next: [i32; STATES],
        decisions: Vec<u64>,
    }

    impl OldViterbi {
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
            OldViterbi { outputs, metrics: [0; STATES], next: [0; STATES], decisions: Vec::new() }
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

    #[test]
    fn the_butterfly_has_one_branch_metric() {
        for i in 0..32u8 {
            let o = output_bits(2 * i);
            assert_eq!(output_bits(2 * i + 1), o ^ 0xF);
            assert_eq!(output_bits(64 | (2 * i)), o ^ 0xF);
            assert_eq!(output_bits(64 | (2 * i + 1)), o);
        }
    }

    #[test]
    fn the_same_bits_as_the_decoder_of_before() {
        let mut seed = 777u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u16
        };
        let (mut new, mut old) = (Viterbi::new(), OldViterbi::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for case in 0..60 {
            let n = 100 + (rnd() as usize % 3000);
            let bits: Vec<u8> = (0..n).map(|_| (rnd() & 1) as u8).collect();
            // noisy soft bits, erasures, now and then pure noise
            let soft: Vec<i8> = encode(&bits)
                .iter()
                .map(|&x| {
                    let v = if x == 0 { 40 } else { -40 } + (rnd() % 161) as i32 - 80;
                    let v = if case % 10 == 9 { (rnd() % 255) as i32 - 127 } else { v };
                    if rnd() % 7 == 0 {
                        0
                    } else {
                        v.clamp(-127, 127) as i8
                    }
                })
                .collect();
            new.decode(&soft, &mut a);
            old.decode(&soft, &mut b);
            assert_eq!(a, b, "case {case}");
        }
    }

    #[test]
    fn the_same_bits_also_saturated_erased_and_noise() {
        let mut seed = 4242u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u16
        };
        let (mut v, mut old) = (Viterbi::new(), OldViterbi::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for case in 0..200 {
            let n = 50 + (rnd() as usize % 4000);
            let bits: Vec<u8> = (0..n).map(|_| (rnd() & 1) as u8).collect();
            // clean, noisy, saturated, erased and pure noise
            let soft: Vec<i8> = encode(&bits)
                .iter()
                .map(|&x| {
                    let clean: i32 = if x == 0 { 127 } else { -127 };
                    let v = match case % 5 {
                        0 => clean,
                        1 => clean / 3 + (rnd() % 161) as i32 - 80,
                        2 => clean + (rnd() % 255) as i32 - 127,
                        3 if rnd() % 3 == 0 => 0,
                        3 => clean / 2,
                        _ => (rnd() % 255) as i32 - 127,
                    };
                    v.clamp(-127, 127) as i8
                })
                .collect();
            v.decode(&soft, &mut a);
            old.decode(&soft, &mut b);
            assert_eq!(a, b, "case {case}");
        }
    }

    /// Both decoders on this processor (`cargo test --release -- --ignored --nocapture timing`).
    #[test]
    #[ignore = "a measurement"]
    fn timing_of_both_decoders() {
        let bits: Vec<u8> = (0..3072).map(|i| ((i * 7 + i / 3) & 1) as u8).collect();
        let soft: Vec<i8> = encode(&bits).iter().map(|&x| if x == 0 { 50 } else { -50 }).collect();
        let (mut new, mut old) = (Viterbi::new(), OldViterbi::new());
        let mut out = Vec::new();
        for round in 0..2 {
            let t = std::time::Instant::now();
            for _ in 0..400 {
                new.decode(&soft, &mut out);
            }
            let tn = t.elapsed();
            let t = std::time::Instant::now();
            for _ in 0..400 {
                old.decode(&soft, &mut out);
            }
            let to = t.elapsed();
            println!("round {round}: new {:?}, old {:?} per 3072 bits", tn / 400, to / 400);
        }
    }

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
