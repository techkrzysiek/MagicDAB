//! Software gain control for 8 bit devices (RTL-SDR, RTL-TCP).
//!
//! Keeps the ADC input in a range with good dynamic range and very little
//! clipping by stepping through the tuner gain table.

pub struct SoftAgc {
    sum_power: f64,
    clipped: u64,
    samples: u64,
    hold: u32,
    pub mean_power: f32,
    /// Evaluations since the last step up, the wait before the next one, and
    /// how long that wait is: a step up that makes the ADC clip at once (a
    /// strong transmitter nearby) is tried again later and later (4 s, 8 s,
    /// ... 2 minutes), else the gain swings up and down every half second.
    since_up: u32,
    up_wait: u32,
    backoff: u32,
    /// Evaluations without a step down: after two quiet minutes the wait
    /// halves (the conditions may have changed).
    calm: u32,
}

pub enum AgcAction {
    None,
    Up,
    Down,
}

impl Default for SoftAgc {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftAgc {
    pub fn new() -> Self {
        SoftAgc { sum_power: 0.0, clipped: 0, samples: 0, hold: 0, mean_power: 0.0, since_up: u32::MAX, up_wait: 0, backoff: 0, calm: 0 }
    }

    /// Accumulate raw unsigned 8 bit IQ samples.
    pub fn feed(&mut self, buf: &[u8]) {
        let mut acc = 0u64;
        let mut clip = 0u64;
        for &b in buf {
            let v = b as i32 - 127;
            acc += (v * v) as u64;
            if b <= 1 || b >= 254 {
                clip += 1;
            }
        }
        self.sum_power += acc as f64 / (127.0 * 127.0);
        self.clipped += clip;
        self.samples += buf.len() as u64;
    }

    /// Evaluate about every 2^20 samples (0.25 s); returns the gain step to take.
    /// `up_ratio` is the power ratio of the next gain step up (1.0 if there is none).
    ///
    /// The AGC climbs towards the gain ceiling (set by the SNR based gain
    /// optimiser of the server) as long as the next step keeps the level clear
    /// of the ADC limits, and backs off when the ADC clips.
    pub fn evaluate(&mut self, up_ratio: f32) -> AgcAction {
        if self.samples < 1 << 20 {
            return AgcAction::None;
        }
        // power per component, full scale = 1
        let mean = (self.sum_power / self.samples as f64) as f32;
        let clip_ratio = self.clipped as f32 / self.samples as f32;
        self.mean_power = mean;
        self.sum_power = 0.0;
        self.clipped = 0;
        self.samples = 0;
        self.since_up = self.since_up.saturating_add(1);
        self.up_wait = self.up_wait.saturating_sub(1);
        if self.hold > 0 {
            self.hold -= 1;
            return AgcAction::None;
        }
        if clip_ratio > 2e-4 || mean > 0.08 {
            self.hold = 1;
            self.calm = 0;
            if self.since_up <= 3 {
                // the step up made it clip: the next one waits longer
                self.backoff = (self.backoff * 2).clamp(16, 480);
                self.up_wait = self.backoff;
            }
            return AgcAction::Down;
        }
        self.calm += 1;
        if self.calm >= 480 {
            self.calm = 0;
            self.backoff /= 2;
        }
        if mean * up_ratio < 0.05 && clip_ratio < 1e-5 && self.up_wait == 0 {
            self.hold = 1;
            self.since_up = 0;
            AgcAction::Up
        } else {
            AgcAction::None
        }
    }
}

/// Gain table index the AGC starts with (and its default ceiling): about 40 dB,
/// higher gains overload the R820T/R828D in areas with strong transmitters.
pub fn default_gain_index(gains_tenth_db: &[i32]) -> usize {
    gains_tenth_db.iter().enumerate().min_by_key(|(_, &g)| (g - 400).abs()).map(|(i, _)| i).unwrap_or(0)
}

/// Next AGC gain index given the action, the current index and the ceiling.
pub fn next_index(action: AgcAction, i: usize, ceiling: usize, len: usize) -> Option<usize> {
    let top = ceiling.min(len.saturating_sub(1));
    if i > top {
        return Some(top);
    }
    match action {
        AgcAction::Up if i < top => Some(i + 1),
        AgcAction::Down if i > 0 => Some(i - 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One evaluation period of samples at a level: `clip` of them at the ADC limits.
    fn period(agc: &mut SoftAgc, level: u8, clip: bool) {
        let mut buf = vec![127 + level; 1 << 20];
        if clip {
            for b in buf.iter_mut().step_by(1000) {
                *b = 255;
            }
        }
        agc.feed(&buf);
    }

    #[test]
    fn a_step_up_that_clips_is_not_tried_every_half_second() {
        // a strong transmitter nearby: index 6 is clean but quiet, index 7 clips
        let mut agc = SoftAgc::new();
        let mut i = 6usize;
        let mut changes = 0;
        for _ in 0..480 {
            period(&mut agc, 20, i >= 7);
            let a = agc.evaluate(1.2);
            if let Some(n) = next_index(a, i, 20, 29) {
                i = n;
                changes += 1;
            }
        }
        // two minutes: a few tries with growing waits instead of one a second
        assert!(changes <= 12, "{changes} gain changes in two minutes");
        assert!(i <= 7);
    }

    #[test]
    fn a_step_up_that_stays_clean_is_kept() {
        let mut agc = SoftAgc::new();
        let mut i = 3usize;
        for _ in 0..40 {
            period(&mut agc, 10, false);
            if let Some(n) = next_index(agc.evaluate(1.2), i, 8, 29) {
                i = n;
            }
        }
        assert_eq!(i, 8, "climbs to the ceiling");
    }
}
