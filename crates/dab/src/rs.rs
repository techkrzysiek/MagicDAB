//! Reed-Solomon RS(120,110) decoder for DAB+ superframes
//! (shortened RS(255,245), GF(2^8) with x^8+x^4+x^3+x^2+1, roots alpha^0..alpha^9).

const NROOTS: usize = 10;
const N: usize = 255;

pub struct ReedSolomon {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Default for ReedSolomon {
    fn default() -> Self {
        Self::new()
    }
}

impl ReedSolomon {
    pub fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        ReedSolomon { exp, log }
    }

    #[inline]
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }

    #[inline]
    fn div(&self, a: u8, b: u8) -> u8 {
        if a == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] as usize + 255 - self.log[b as usize] as usize) % 255]
        }
    }

    /// Correct a codeword in place (first byte = highest degree coefficient).
    /// Returns the number of corrected symbols or `None` if uncorrectable.
    pub fn decode(&self, cw: &mut [u8]) -> Option<usize> {
        let len = cw.len();
        debug_assert!(len > NROOTS && len <= N);
        // syndromes
        let mut synd = [0u8; NROOTS];
        let mut all_zero = true;
        for (j, s) in synd.iter_mut().enumerate() {
            let a = self.exp[j];
            let mut v = 0u8;
            for &c in cw.iter() {
                v = self.mul(v, a) ^ c;
            }
            *s = v;
            all_zero &= v == 0;
        }
        if all_zero {
            return Some(0);
        }
        // Berlekamp-Massey
        let mut lambda = [0u8; NROOTS + 1];
        let mut b = [0u8; NROOTS + 1];
        lambda[0] = 1;
        b[0] = 1;
        let mut l = 0usize;
        let mut m = 1usize;
        let mut bb = 1u8;
        for r in 0..NROOTS {
            let mut delta = synd[r];
            for i in 1..=l {
                delta ^= self.mul(lambda[i], synd[r - i]);
            }
            if delta == 0 {
                m += 1;
            } else if 2 * l <= r {
                let t = lambda;
                let coef = self.div(delta, bb);
                for i in m..=NROOTS {
                    lambda[i] ^= self.mul(coef, b[i - m]);
                }
                l = r + 1 - l;
                b = t;
                bb = delta;
                m = 1;
            } else {
                let coef = self.div(delta, bb);
                for i in m..=NROOTS {
                    lambda[i] ^= self.mul(coef, b[i - m]);
                }
                m += 1;
            }
        }
        if l == 0 || l > NROOTS / 2 {
            return None;
        }
        // Chien search over the positions of the (shortened) codeword
        let mut positions = Vec::with_capacity(l);
        for idx in 0..len {
            let degree = len - 1 - idx;
            // evaluate lambda at alpha^-degree
            let xinv = self.exp[(255 - degree % 255) % 255];
            let mut v = 0u8;
            let mut xp = 1u8;
            for &c in lambda.iter().take(l + 1) {
                v ^= self.mul(c, xp);
                xp = self.mul(xp, xinv);
            }
            if v == 0 {
                positions.push(idx);
            }
        }
        if positions.len() != l {
            return None;
        }
        // error evaluator omega = (S * lambda) mod x^NROOTS
        let mut omega = [0u8; NROOTS];
        for i in 0..NROOTS {
            let mut v = 0u8;
            for j in 0..=i.min(l) {
                v ^= self.mul(synd[i - j], lambda[j]);
            }
            omega[i] = v;
        }
        // Forney (fcr = 0): e = X * omega(X^-1) / lambda'(X^-1)
        for &idx in &positions {
            let degree = len - 1 - idx;
            let x = self.exp[degree % 255];
            let xinv = self.exp[(255 - degree % 255) % 255];
            let mut num = 0u8;
            let mut xp = 1u8;
            for &o in omega.iter() {
                num ^= self.mul(o, xp);
                xp = self.mul(xp, xinv);
            }
            // formal derivative: odd terms only
            let mut den = 0u8;
            let mut i = 1;
            while i <= l {
                let mut p = 1u8;
                for _ in 0..(i - 1) {
                    p = self.mul(p, xinv);
                }
                den ^= self.mul(lambda[i], p);
                i += 2;
            }
            if den == 0 {
                return None;
            }
            let e = self.mul(x, self.div(num, den));
            cw[idx] ^= e;
        }
        Some(positions.len())
    }

    /// Systematic encoder (used by tests): `cw[..len-10]` data, parity written to the end.
    pub fn encode(&self, cw: &mut [u8]) {
        let len = cw.len();
        let k = len - NROOTS;
        // generator polynomial
        let mut gen = vec![1u8];
        for i in 0..NROOTS {
            let root = self.exp[i];
            let mut next = vec![0u8; gen.len() + 1];
            for (j, &g) in gen.iter().enumerate() {
                next[j] ^= g;
                next[j + 1] ^= self.mul(g, root);
            }
            gen = next;
        }
        let mut rem = vec![0u8; NROOTS];
        for i in 0..k {
            let fb = cw[i] ^ rem[0];
            rem.rotate_left(1);
            rem[NROOTS - 1] = 0;
            if fb != 0 {
                for j in 0..NROOTS {
                    rem[j] ^= self.mul(gen[j + 1], fb);
                }
            }
        }
        cw[k..].copy_from_slice(&rem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_up_to_five_errors() {
        let rs = ReedSolomon::new();
        let mut cw = [0u8; 120];
        for (i, b) in cw.iter_mut().enumerate().take(110) {
            *b = (i * 37 + 11) as u8;
        }
        rs.encode(&mut cw);
        let orig = cw;
        assert_eq!(rs.decode(&mut cw.clone()), Some(0));
        for errors in 1..=5 {
            let mut c = orig;
            for e in 0..errors {
                c[e * 23 + 3] ^= 0x5A + e as u8;
            }
            assert_eq!(rs.decode(&mut c), Some(errors));
            assert_eq!(c, orig);
        }
        let mut c = orig;
        for e in 0..7 {
            c[e * 13] ^= 0xFF;
        }
        let r = rs.decode(&mut c);
        assert!(r.is_none() || c != orig);
    }
}
