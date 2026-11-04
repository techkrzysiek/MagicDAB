//! DAB+ superframe synchronisation, Reed-Solomon correction and access unit
//! extraction (ETSI TS 102 563).

use crate::crc::{check_crc16, check_firecode};
use crate::rs::ReedSolomon;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AacFormat {
    /// DAC rate: false = 32 kHz, true = 48 kHz.
    pub dac_48k: bool,
    pub sbr: bool,
    pub stereo: bool,
    pub ps: bool,
    pub mpeg_surround: u8,
}

impl AacFormat {
    pub fn sample_rate(&self) -> u32 {
        if self.dac_48k {
            48_000
        } else {
            32_000
        }
    }

    pub fn num_aus(&self) -> usize {
        match (self.dac_48k, self.sbr) {
            (false, true) => 2,
            (false, false) => 4,
            (true, true) => 3,
            (true, false) => 6,
        }
    }

    pub fn codec_name(&self) -> &'static str {
        match (self.sbr, self.ps) {
            (true, true) => "HE-AAC v2",
            (true, false) => "HE-AAC",
            _ => "AAC-LC",
        }
    }

    /// AudioSpecificConfig with 960 sample frames (implicit SBR/PS signalling).
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let core_sr_index: u8 = match (self.dac_48k, self.sbr) {
            (true, true) => 6,   // 24 kHz
            (true, false) => 3,  // 48 kHz
            (false, true) => 8,  // 16 kHz
            (false, false) => 5, // 32 kHz
        };
        let channels: u8 = if self.stereo { 2 } else { 1 };
        [(2 << 3) | (core_sr_index >> 1), ((core_sr_index & 1) << 7) | (channels << 3) | 0b100]
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DabPlusStats {
    pub superframes: u64,
    pub sync_losses: u64,
    pub rs_corrected_bytes: u64,
    pub rs_corrected_bits: u64,
    pub rs_uncorrectable: u64,
    pub rs_codewords: u64,
    pub au_ok: u64,
    pub au_errors: u64,
}

pub struct AccessUnit<'a> {
    pub data: &'a [u8],
    pub crc_ok: bool,
}

pub struct SuperframeDecoder {
    frame_len: usize,
    window: Vec<u8>,
    frames: usize,
    synced: bool,
    fails: u32,
    rs: ReedSolomon,
    sf: Vec<u8>,
    pub format: Option<AacFormat>,
    pub stats: DabPlusStats,
}

impl SuperframeDecoder {
    pub fn new(bitrate: u16) -> Self {
        let frame_len = bitrate as usize * 3;
        SuperframeDecoder {
            frame_len,
            window: Vec::with_capacity(frame_len * 5),
            frames: 0,
            synced: false,
            fails: 0,
            rs: ReedSolomon::new(),
            sf: vec![0; frame_len * 5],
            format: None,
            stats: DabPlusStats::default(),
        }
    }

    fn rs_correct(&mut self, count_stats: bool) -> bool {
        let s = self.frame_len / 24; // bitrate / 8
        let mut cw = [0u8; 120];
        let mut ok = true;
        for j in 0..s {
            for k in 0..120 {
                cw[k] = self.sf[j + s * k];
            }
            match self.rs.decode(&mut cw) {
                Some(n) => {
                    if n > 0 {
                        for k in 0..120 {
                            if count_stats {
                                self.stats.rs_corrected_bits += (self.sf[j + s * k] ^ cw[k]).count_ones() as u64;
                            }
                            self.sf[j + s * k] = cw[k];
                        }
                    }
                    if count_stats {
                        self.stats.rs_corrected_bytes += n as u64;
                    }
                }
                None => {
                    ok = false;
                    if count_stats {
                        self.stats.rs_uncorrectable += 1;
                    }
                }
            }
            if count_stats {
                self.stats.rs_codewords += 1;
            }
        }
        ok
    }

    /// Feed one logical frame (bitrate * 3 bytes); `on_au` receives the access
    /// units of every complete superframe.
    pub fn push(&mut self, frame: &[u8], mut on_au: impl FnMut(AacFormat, AccessUnit)) {
        if frame.len() != self.frame_len {
            return;
        }
        if self.window.len() == 5 * self.frame_len {
            self.window.drain(..self.frame_len);
        }
        self.window.extend_from_slice(frame);
        self.frames += 1;
        if self.window.len() < 5 * self.frame_len {
            return;
        }
        if self.synced && self.frames % 5 != 0 {
            return;
        }
        self.sf.copy_from_slice(&self.window);
        let mut found = check_firecode(&self.sf);
        let mut corrected = false;
        if !found {
            // errors in the header may be correctable
            self.rs_correct(false);
            corrected = true;
            found = check_firecode(&self.sf);
        }
        if !found {
            if std::env::var("SFDEBUG").is_ok() {
                eprintln!("SF {} firecode fail synced={} frames {}", self.stats.superframes, self.synced, self.frames);
            }
            if self.synced {
                self.fails += 1;
                if self.fails > 3 {
                    self.synced = false;
                    self.stats.sync_losses += 1;
                }
            }
            if !self.synced {
                return;
            }
            // keep the superframe grid but drop this one; its access units are
            // reported as lost so the audio timeline stays continuous
            self.stats.superframes += 1;
            if let Some(f) = self.format {
                self.stats.au_errors += f.num_aus() as u64;
                for _ in 0..f.num_aus() {
                    on_au(f, AccessUnit { data: &[], crc_ok: false });
                }
            }
            return;
        }
        if !self.synced {
            self.synced = true;
            self.frames = 5;
        }
        self.fails = 0;
        if !corrected {
            self.sf.copy_from_slice(&self.window);
            self.rs_correct(true);
        } else {
            // recount on the original data for statistics
            self.sf.copy_from_slice(&self.window);
            self.rs_correct(true);
        }
        self.stats.superframes += 1;

        let h = self.sf[2];
        let format =
            AacFormat { dac_48k: h & 0x40 != 0, sbr: h & 0x20 != 0, stereo: h & 0x10 != 0, ps: h & 0x08 != 0, mpeg_surround: h & 0x07 };
        self.format = Some(format);
        let num = format.num_aus();
        let mut starts = [0usize; 7];
        starts[0] = match num {
            2 => 5,
            3 => 6,
            4 => 8,
            _ => 11,
        };
        // 12 bit AU start addresses from byte 3
        for n in 1..num {
            let bit = (n - 1) * 12;
            let byte = 3 + bit / 8;
            let v = ((self.sf[byte] as usize) << 8) | self.sf[byte + 1] as usize;
            starts[n] = if bit % 8 == 0 { v >> 4 } else { v & 0x0FFF };
        }
        let data_len = self.frame_len / 24 * 110;
        starts[num] = data_len;
        for n in 0..num {
            let (a, b) = (starts[n], starts[n + 1]);
            if a >= b || b > data_len || b - a < 2 {
                self.stats.au_errors += 1;
                on_au(format, AccessUnit { data: &[], crc_ok: false });
                continue;
            }
            let au = &self.sf[a..b];
            let ok = check_crc16(au);
            if !ok && std::env::var("SFDEBUG").is_ok() {
                eprintln!("SF {} AU {} CRC error (frames {})", self.stats.superframes, n, self.frames);
            }
            if ok {
                self.stats.au_ok += 1;
            } else {
                self.stats.au_errors += 1;
            }
            on_au(format, AccessUnit { data: &au[..au.len() - 2], crc_ok: ok });
        }
    }

    pub fn is_synced(&self) -> bool {
        self.synced
    }
}
