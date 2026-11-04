//! Rational polyphase resampler for interleaved stereo audio (to 48 kHz).

pub struct Resampler {
    up: usize,
    down: usize,
    taps: usize,
    coeffs: Vec<f32>,
    hist: Vec<f32>,
    /// Position of the next output sample in the upsampled domain, relative to `hist`.
    pos: usize,
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let g = gcd(in_rate as usize, out_rate as usize);
        let up = out_rate as usize / g;
        let down = in_rate as usize / g;
        let taps = 32;
        let n = taps * up;
        // cutoff relative to the upsampled rate
        let fc = 0.5 * (1.0f64 / up as f64).min(1.0 / down as f64) * 0.9;
        let mut coeffs = vec![0f32; n];
        let mid = (n as f64 - 1.0) / 2.0;
        for (i, c) in coeffs.iter_mut().enumerate() {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 { 1.0 } else { (2.0 * std::f64::consts::PI * fc * x).sin() / (2.0 * std::f64::consts::PI * fc * x) };
            // Blackman window
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            *c = (2.0 * fc * sinc * w * up as f64) as f32;
        }
        Resampler { up, down, taps, coeffs, hist: vec![0.0; 2 * taps], pos: (taps - 1) * up }
    }

    pub fn is_passthrough(&self) -> bool {
        self.up == 1 && self.down == 1
    }

    /// Resample interleaved stereo input, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        self.hist.extend_from_slice(input);
        let frames = self.hist.len() / 2;
        loop {
            let base = self.pos / self.up;
            if base >= frames {
                break;
            }
            let phase = self.pos % self.up;
            let mut l = 0f32;
            let mut r = 0f32;
            for j in 0..self.taps {
                if base < j {
                    break;
                }
                let c = self.coeffs[phase + j * self.up];
                let idx = (base - j) * 2;
                l += c * self.hist[idx];
                r += c * self.hist[idx + 1];
            }
            out.push(l);
            out.push(r);
            self.pos += self.down;
        }
        // drop consumed input, keep the filter history
        let base = self.pos / self.up;
        if base > self.taps {
            let drop = base - self.taps;
            self.hist.drain(..drop * 2);
            self.pos -= drop * self.up;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio() {
        let mut r = Resampler::new(32_000, 48_000);
        let input: Vec<f32> = (0..32_000 * 2).map(|i| ((i / 2) as f32 * 0.01).sin()).collect();
        let mut out = Vec::new();
        for chunk in input.chunks(1920) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() < 100, "{frames}");
    }
}
