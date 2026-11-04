//! Clips of stations for the server map: a few seconds of a station's audio,
//! recorded during the background scan, as Ogg Opus (what browsers play; raw
//! DAB+ AAC with its 960-sample frames is not). The map keeps the last seven
//! of each station (the map's API).

use magicdab_codecs::opus::{OpusEncoder, FRAME};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Length of a clip, and the audio skipped first while the decoder settles.
pub const SECONDS: usize = 12;
const WARMUP_SAMPLES: usize = 48_000 * 3 / 2 * 2;
/// (enough to judge the sound of a DAB+ station by: at 48 kbps it was the
/// clip one heard)
const BITRATE: i32 = 128_000;
/// Opus' look-ahead at 48 kHz: the decoder drops it (OpusHead pre-skip).
pub(crate) const PRE_SKIP: u16 = 312;
/// Packets per Ogg page (one second).
const PACKETS_PER_PAGE: usize = 50;

/// CRC-32 of Ogg pages: polynomial 0x04C11DB7, not reflected, no final xor.
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            }
            *v = r;
        }
        t
    });
    data.iter().fold(0u32, |crc, &b| (crc << 8) ^ table[((crc >> 24) as u8 ^ b) as usize])
}

/// An Ogg stream of one logical bitstream, written into memory (also the
/// streams of icy.rs).
pub(crate) struct Ogg {
    serial: u32,
    sequence: u32,
    pub out: Vec<u8>,
}

impl Ogg {
    pub(crate) fn new() -> Self {
        Ogg { serial: rand::random(), sequence: 0, out: Vec::new() }
    }

    /// One page holding whole packets; `flags`: 2 first page, 4 last page.
    pub(crate) fn page(&mut self, packets: &[&[u8]], granule: u64, flags: u8) {
        let mut lacing = Vec::new();
        for p in packets {
            lacing.extend(std::iter::repeat_n(255u8, p.len() / 255));
            lacing.push((p.len() % 255) as u8);
        }
        debug_assert!(lacing.len() <= 255);
        let start = self.out.len();
        self.out.extend_from_slice(b"OggS");
        self.out.push(0);
        self.out.push(flags);
        self.out.extend_from_slice(&granule.to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.sequence.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(lacing.len() as u8);
        self.out.extend_from_slice(&lacing);
        for p in packets {
            self.out.extend_from_slice(p);
        }
        let crc = crc32(&self.out[start..]);
        self.out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
    }
}

/// Ogg Opus file of 48 kHz stereo packets of 20 ms.
pub fn ogg_opus(packets: &[Vec<u8>], title: &str) -> Vec<u8> {
    let mut ogg = Ogg::new();
    opus_headers(&mut ogg, title);
    let pages: Vec<&[Vec<u8>]> = packets.chunks(PACKETS_PER_PAGE).collect();
    let mut granule = PRE_SKIP as u64;
    for (i, chunk) in pages.iter().enumerate() {
        granule += (chunk.len() * FRAME) as u64;
        let refs: Vec<&[u8]> = chunk.iter().map(|p| p.as_slice()).collect();
        ogg.page(&refs, granule, if i + 1 == pages.len() { 4 } else { 0 });
    }
    ogg.out
}

/// The two header pages of an Ogg Opus stream (48 kHz stereo) with its title.
pub(crate) fn opus_headers(ogg: &mut Ogg, title: &str) {
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(2);
    head.extend_from_slice(&PRE_SKIP.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    ogg.page(&[&head], 0, 2);
    let vendor = format!("MagicDAB {}", crate::radio::VERSION);
    let comment = format!("TITLE={}", title.chars().take(64).collect::<String>());
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor.as_bytes());
    tags.extend_from_slice(&1u32.to_le_bytes());
    tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    tags.extend_from_slice(comment.as_bytes());
    ogg.page(&[&tags], 0, 0);
}

/// A clip being recorded from a station's decoded audio (48 kHz stereo).
pub struct ClipRecorder {
    pub channel: String,
    pub eid: u16,
    pub sid: u32,
    pub label: String,
    /// Unix time of the first second kept.
    pub time: i64,
    skip: usize,
    pending: Vec<f32>,
    encoder: OpusEncoder,
    packets: Vec<Vec<u8>>,
    /// When audio came last (wall clock), and the stalls of the decoder while
    /// recording: gaps of half a second, and audio units lost (the map marks
    /// such a clip: it does not sound like the station).
    last_feed: Option<std::time::Instant>,
    pub stalls: u32,
    au_errors: Option<(u64, u64)>,
    /// 20 ms frames with sound in them (above -50 dBFS): a clip of a muted
    /// decoder (a weak multiplex) is silence and is not sent.
    loud_frames: usize,
}

impl ClipRecorder {
    pub fn new(channel: &str, eid: u16, sid: u32, label: &str) -> Option<ClipRecorder> {
        let encoder = OpusEncoder::new(BITRATE).map_err(|e| log::warn!("clip: {e}")).ok()?;
        Some(ClipRecorder {
            channel: channel.into(),
            eid,
            sid,
            label: label.into(),
            time: 0,
            skip: WARMUP_SAMPLES,
            pending: Vec::new(),
            encoder,
            packets: Vec::new(),
            last_feed: None,
            stalls: 0,
            au_errors: None,
            loud_frames: 0,
        })
    }

    pub fn feed(&mut self, pcm: &[f32]) {
        if self.done() {
            return;
        }
        let mut pcm = pcm;
        if self.skip > 0 {
            let n = self.skip.min(pcm.len());
            self.skip -= n;
            pcm = &pcm[n..];
            if self.skip > 0 || pcm.is_empty() {
                return;
            }
        }
        if self.time == 0 {
            self.time = crate::servicelist::now();
        }
        let now = std::time::Instant::now();
        if self.last_feed.is_some_and(|t| now.duration_since(t).as_secs_f32() > 0.5) {
            self.stalls += 1;
        }
        self.last_feed = Some(now);
        self.pending.extend_from_slice(pcm);
        while self.pending.len() >= FRAME * 2 && !self.done() {
            let frame: Vec<f32> = self.pending.drain(..FRAME * 2).collect();
            let power = frame.iter().map(|v| v * v).sum::<f32>() / frame.len() as f32;
            if power > 1e-5 {
                self.loud_frames += 1;
            }
            if let Some(p) = self.encoder.encode(&frame) {
                self.packets.push(p.to_vec());
            }
        }
    }

    pub fn done(&self) -> bool {
        self.packets.len() >= SECONDS * 50
    }

    /// The decoder's count of lost audio units, while recording.
    pub fn note_errors(&mut self, au_errors: u64) {
        if self.time == 0 || self.done() {
            return;
        }
        let first = self.au_errors.map_or(au_errors, |(a, _)| a);
        self.au_errors = Some((first, au_errors));
    }

    /// Hardly any sound in it (a quarter of the frames at most): not worth sending.
    pub fn silent(&self) -> bool {
        self.loud_frames * 4 < self.packets.len()
    }

    /// Stalls and lost audio units while recording (0: a clean clip).
    pub fn glitches(&self) -> u32 {
        self.stalls + self.au_errors.map_or(0, |(a, b)| b.saturating_sub(a) as u32)
    }

    pub fn seconds(&self) -> f32 {
        self.packets.len() as f32 / 50.0
    }

    pub fn finish(self) -> Vec<u8> {
        ogg_opus(&self.packets, &self.label)
    }
}

/// When each station was last recorded (data/clips.json), so that a restart
/// does not record them all again.
#[derive(Default)]
pub struct ClipTimes {
    times: HashMap<String, i64>,
    path: PathBuf,
}

impl ClipTimes {
    pub fn load(dir: &Path) -> ClipTimes {
        let path = dir.join("clips.json");
        let times = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        ClipTimes { times, path }
    }

    fn key(channel: &str, eid: u16, sid: u32) -> String {
        format!("{channel}/{eid:04X}/{sid:04X}")
    }

    pub fn last(&self, channel: &str, eid: u16, sid: u32) -> i64 {
        self.times.get(&Self::key(channel, eid, sid)).copied().unwrap_or(0)
    }

    pub fn set(&mut self, channel: &str, eid: u16, sid: u32, time: i64) {
        self.times.insert(Self::key(channel, eid, sid), time);
        // (stations of the last two months only)
        let old = time - 60 * 86400;
        self.times.retain(|_, t| *t > old);
        if let Ok(s) = serde_json::to_string(&self.times) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogg_pages_are_well_formed() {
        let packets: Vec<Vec<u8>> = (0..120).map(|i| vec![i as u8; 100 + i * 3]).collect();
        let data = ogg_opus(&packets, "Radio");
        // pages: OpusHead, OpusTags, 3 of audio (50 + 50 + 20 packets)
        let mut pos = 0;
        let mut pages = Vec::new();
        while pos < data.len() {
            assert_eq!(&data[pos..pos + 4], b"OggS");
            let segments = data[pos + 26] as usize;
            let body: usize = data[pos + 27..pos + 27 + segments].iter().map(|&b| b as usize).sum();
            let len = 27 + segments + body;
            let mut page = data[pos..pos + len].to_vec();
            let crc = u32::from_le_bytes(page[22..26].try_into().unwrap());
            page[22..26].copy_from_slice(&[0; 4]);
            assert_eq!(crc32(&page), crc);
            let granule = u64::from_le_bytes(data[pos + 6..pos + 14].try_into().unwrap());
            pages.push((data[pos + 5], granule));
            pos += len;
        }
        assert_eq!(pos, data.len());
        assert_eq!(pages, vec![(2, 0), (0, 0), (0, 312 + 50 * 960), (0, 312 + 100 * 960), (4, 312 + 120 * 960)]);
        assert_eq!(&data[28..36], b"OpusHead");
    }

    /// A real clip: two seconds of a tone through the Opus encoder (written
    /// to $CLIP_TEST_OUT when set, to try it in a browser).
    #[test]
    fn a_clip_of_a_tone() {
        let mut r = ClipRecorder::new("11B", 0x4B6E, 0x4A91, "Test").unwrap();
        let mut t = 0f32;
        while !r.done() {
            let pcm: Vec<f32> = (0..1920 * 5)
                .map(|i| {
                    if i % 2 == 0 {
                        t += 1.0 / 48_000.0;
                    }
                    (t * 440.0 * std::f32::consts::TAU).sin() * 0.3
                })
                .collect();
            r.feed(&pcm);
        }
        assert!(r.time > 0);
        assert_eq!(r.seconds(), SECONDS as f32);
        let data = r.finish();
        assert!(data.len() > 10_000 && data.len() < 200_000, "{} bytes", data.len());
        if let Ok(p) = std::env::var("CLIP_TEST_OUT") {
            std::fs::write(p, &data).unwrap();
        }
    }

    #[test]
    fn the_ogg_crc_is_the_known_one() {
        // (CRC-32/MPEG-2 without its initial value and final xor, as Ogg uses it)
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0x89A1897F);
    }
}
