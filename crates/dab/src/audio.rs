//! Audio service decoding: DAB+ (AAC) and DAB (MP2) including PAD.

use crate::dabplus::{AacFormat, DabPlusStats, SuperframeDecoder};
use crate::dls::{DlsDecoder, DynamicLabel};
use crate::mot::{MotDecoder, MotObject, ObjectProgress};
use crate::pad::{dabplus_pad, mp2_pad, PadDecoder, PadEvent};
use magicdab_codecs::{AacDecoder, Mp2Decoder, Resampler};
use serde::Serialize;

pub const OUTPUT_RATE: u32 = 48_000;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioFormat {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub sbr: bool,
    pub ps: bool,
    pub stereo: bool,
}

/// Highest sample a decoded unit may hold (+18 dBFS): a corrupted unit decodes
/// to anything. Not much lower: a station sending an overdriven programme
/// decodes far above full scale all the time (one did with half of the units
/// above +6 dBFS, up to +9.3 dBFS), and radios clip that, not drop it.
const MAX_DECODED: f32 = 8.0;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStats {
    pub frames: u64,
    pub frame_errors: u64,
    pub au_ok: u64,
    pub au_errors: u64,
    pub rs_corrected_bytes: u64,
    pub rs_corrected_bits: u64,
    pub rs_uncorrectable: u64,
    pub rs_codewords: u64,
    pub superframes: u64,
    pub audio_bytes: u64,
    pub pad_bytes: u64,
    pub decode_errors: u64,
}

/// Minimal MSB-first bit writer.
struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    acc: u64,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), bits: 0, acc: 0 }
    }

    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u64;
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            let pad = 8 - self.bits;
            self.put(0, pad);
        }
        self.out
    }
}

/// Wrap an AAC access unit of DAB+ into a LOAS/LATM frame with explicit
/// AudioSpecificConfig (960 sample frames, SBR/PS signalled), playable by
/// common players.
pub fn loas_frame(fmt: &AacFormat, au: &[u8]) -> Vec<u8> {
    let core_sfi: u32 = match (fmt.dac_48k, fmt.sbr) {
        (true, true) => 6,
        (true, false) => 3,
        (false, true) => 8,
        (false, false) => 5,
    };
    let ext_sfi: u32 = if fmt.dac_48k { 3 } else { 5 };
    let channels: u32 = if fmt.stereo { 2 } else { 1 };
    let mut w = BitWriter::new();
    // AudioMuxElement(1): useSameStreamMux = 0, StreamMuxConfig
    w.put(0, 1); // useSameStreamMux
    w.put(0, 1); // audioMuxVersion
    w.put(1, 1); // allStreamsSameTimeFraming
    w.put(0, 6); // numSubFrames
    w.put(0, 4); // numProgram
    w.put(0, 3); // numLayer
                 // AudioSpecificConfig
    if fmt.sbr {
        w.put(if fmt.ps { 29 } else { 5 }, 5); // explicit hierarchical signalling
        w.put(core_sfi, 4);
        w.put(channels, 4);
        w.put(ext_sfi, 4);
        w.put(2, 5); // AAC LC
    } else {
        w.put(2, 5);
        w.put(core_sfi, 4);
        w.put(channels, 4);
    }
    w.put(1, 1); // frameLengthFlag: 960
    w.put(0, 1); // dependsOnCoreCoder
    w.put(0, 1); // extensionFlag
    w.put(0, 3); // frameLengthType
    w.put(0xFF, 8); // latmBufferFullness
    w.put(0, 1); // otherDataPresent
    w.put(0, 1); // crcCheckPresent
                 // PayloadLengthInfo
    let mut len = au.len();
    while len >= 255 {
        w.put(255, 8);
        len -= 255;
    }
    w.put(len as u32, 8);
    for &b in au {
        w.put(b as u32, 8);
    }
    let payload = w.finish();
    let mut out = Vec::with_capacity(payload.len() + 3);
    let l = payload.len() as u32;
    out.push(0x56);
    out.push(0xE0 | ((l >> 8) & 0x1F) as u8);
    out.push(l as u8);
    out.extend_from_slice(&payload);
    out
}

pub enum ServiceEvent {
    /// Original encoded audio for recording: LOAS/LATM AAC or an MP2 frame.
    Encoded {
        data: Vec<u8>,
        aac: bool,
    },
    /// 48 kHz interleaved stereo samples.
    Audio(Vec<f32>),
    Format(AudioFormat),
    Label(DynamicLabel),
    /// MOT object received via X-PAD (`app_type` = start X-PAD AppTy).
    XpadObject {
        app_type: u8,
        object: MotObject,
    },
}

enum Kind {
    DabPlus { sf: SuperframeDecoder, aac: Option<AacDecoder>, format: Option<AacFormat> },
    Mp2 { buf: Vec<u8>, dec: Mp2Decoder },
}

pub struct AudioDecoder {
    kind: Kind,
    pad: PadDecoder,
    dls: DlsDecoder,
    mot: Vec<(u8, MotDecoder)>,
    resampler: Option<(u32, Resampler)>,
    pcm: Vec<f32>,
    stereo: Vec<f32>,
    format: AudioFormat,
    pub stats: AudioStats,
    pad_events: Vec<PadEvent>,
    mute_frames: u32,
    fade_in: bool,
    /// Emit the original encoded audio (for recording).
    pub record: bool,
    /// Only PAD (DLS, slideshow) is decoded, no audio (background decoding).
    pub pad_only: bool,
}

const MP2_BITRATES_V1: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
const MP2_BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

impl AudioDecoder {
    pub fn new(dabplus: bool, bitrate: u16) -> Self {
        let kind = if dabplus {
            Kind::DabPlus { sf: SuperframeDecoder::new(bitrate), aac: None, format: None }
        } else {
            Kind::Mp2 { buf: Vec::new(), dec: Mp2Decoder::new() }
        };
        let mut d = AudioDecoder {
            kind,
            pad: PadDecoder::new(),
            dls: DlsDecoder::new(),
            mot: Vec::new(),
            resampler: None,
            pcm: Vec::new(),
            stereo: Vec::new(),
            format: AudioFormat::default(),
            stats: AudioStats::default(),
            pad_events: Vec::new(),
            mute_frames: 2,
            fade_in: true,
            record: false,
            pad_only: false,
        };
        // MOT slideshow is almost always carried with X-PAD AppTy 12
        d.register_xpad_app(12);
        d
    }

    pub fn register_xpad_app(&mut self, app_type: u8) {
        if !self.mot.iter().any(|(a, _)| *a == app_type) {
            self.pad.register_app(app_type);
            self.mot.push((app_type, MotDecoder::new()));
        }
    }

    pub fn dls(&self) -> &DynamicLabel {
        self.dls.current()
    }

    pub fn dabplus_stats(&self) -> Option<DabPlusStats> {
        match &self.kind {
            Kind::DabPlus { sf, .. } => Some(sf.stats),
            _ => None,
        }
    }

    /// MOT objects being received via X-PAD: (X-PAD AppTy, progress).
    pub fn xpad_receiving(&self) -> Vec<(u8, ObjectProgress)> {
        self.mot.iter().filter_map(|(a, m)| m.receiving().map(|p| (*a, p))).collect()
    }

    fn emit_pcm(&mut self, rate: u32, channels: u8, out: &mut Vec<ServiceEvent>) {
        if self.pcm.is_empty() {
            return;
        }
        // to stereo
        self.stereo.clear();
        if channels == 1 {
            for &s in &self.pcm {
                self.stereo.push(s);
                self.stereo.push(s);
            }
        } else {
            let ch = channels as usize;
            for f in self.pcm.chunks_exact(ch) {
                self.stereo.push(f[0]);
                self.stereo.push(f[1]);
            }
        }
        self.pcm.clear();
        if self.mute_frames > 0 {
            self.mute_frames -= 1;
            self.stereo.iter_mut().for_each(|s| *s = 0.0);
            self.fade_in = true;
        } else if self.fade_in {
            // smooth start after muted frames (avoids clicks)
            self.fade_in = false;
            let n = self.stereo.len() / 2;
            for i in 0..n {
                let g = i as f32 / n as f32;
                self.stereo[2 * i] *= g;
                self.stereo[2 * i + 1] *= g;
            }
        }
        let need_new = !matches!(&self.resampler, Some((r, _)) if *r == rate);
        if need_new {
            self.resampler = Some((rate, Resampler::new(rate, OUTPUT_RATE)));
        }
        let mut res = Vec::with_capacity(self.stereo.len() * 2);
        self.resampler.as_mut().unwrap().1.process(&self.stereo, &mut res);
        out.push(ServiceEvent::Audio(res));
    }

    fn handle_pad(&mut self, xpad: &[u8], exact: bool, fpad: [u8; 2], out: &mut Vec<ServiceEvent>) {
        self.pad_events.clear();
        let mut events = std::mem::take(&mut self.pad_events);
        self.pad.process(xpad, exact, fpad, &mut events);
        // (MP2: the X-PAD the field took, not the window it was looked for in)
        self.stats.pad_bytes += self.pad.used as u64 + 2;
        for ev in events.drain(..) {
            match ev {
                PadEvent::Dls(dg) => {
                    if let Some(label) = self.dls.process(&dg) {
                        out.push(ServiceEvent::Label(label));
                    }
                }
                PadEvent::DataGroup { app_type, data } => {
                    if std::env::var("MOTDEBUG").is_ok() {
                        let dg = crate::mot::parse_data_group(&data);
                        eprintln!(
                            "XPAD DG app {} len {} parsed {:?}",
                            app_type,
                            data.len(),
                            dg.map(|d| (d.dg_type, d.segment, d.last, d.transport_id, d.data.len()))
                        );
                    }
                    if let Some((_, mot)) = self.mot.iter_mut().find(|(a, _)| *a == app_type) {
                        let mut objs = Vec::new();
                        mot.process(&data, &mut objs);
                        for object in objs {
                            out.push(ServiceEvent::XpadObject { app_type, object });
                        }
                    }
                }
            }
        }
        self.pad_events = events;
    }

    /// Feed one decoded logical frame of the sub-channel.
    pub fn push(&mut self, frame: &[u8], out: &mut Vec<ServiceEvent>) {
        self.stats.frames += 1;
        match &mut self.kind {
            Kind::DabPlus { .. } => self.push_dabplus(frame, out),
            Kind::Mp2 { .. } => self.push_mp2(frame, out),
        }
    }

    fn push_dabplus(&mut self, frame: &[u8], out: &mut Vec<ServiceEvent>) {
        let mut aus: Vec<(AacFormat, Vec<u8>, bool)> = Vec::new();
        if let Kind::DabPlus { sf, .. } = &mut self.kind {
            sf.push(frame, |fmt, au| aus.push((fmt, au.data.to_vec(), au.crc_ok)));
            let st = sf.stats;
            self.stats.au_ok = st.au_ok;
            self.stats.au_errors = st.au_errors;
            self.stats.rs_corrected_bytes = st.rs_corrected_bytes;
            self.stats.rs_corrected_bits = st.rs_corrected_bits;
            self.stats.rs_uncorrectable = st.rs_uncorrectable;
            self.stats.rs_codewords = st.rs_codewords;
            self.stats.superframes = st.superframes;
        }
        if self.pad_only {
            for (_, au, ok) in aus {
                if ok {
                    if let Some((xpad, fpad)) = dabplus_pad(&au) {
                        let xpad = xpad.to_vec();
                        self.handle_pad(&xpad, true, fpad, out);
                    }
                }
            }
            return;
        }
        for (fmt, au, ok) in aus {
            // (re)create the AAC decoder on format change
            let mut new_format = None;
            if let Kind::DabPlus { aac, format, .. } = &mut self.kind {
                if *format != Some(fmt) || aac.is_none() {
                    match AacDecoder::new(&fmt.audio_specific_config()) {
                        Ok(d) => {
                            *aac = Some(d);
                            *format = Some(fmt);
                            new_format = Some(fmt);
                            self.mute_frames = 2;
                        }
                        Err(e) => {
                            log::warn!("AAC decoder init failed: {e}");
                            continue;
                        }
                    }
                }
            }
            if let Some(f) = new_format {
                self.format = AudioFormat {
                    codec: f.codec_name().to_string(),
                    sample_rate: f.sample_rate(),
                    channels: if f.stereo || f.ps { 2 } else { 1 },
                    sbr: f.sbr,
                    ps: f.ps,
                    stereo: f.stereo || f.ps,
                };
                out.push(ServiceEvent::Format(self.format.clone()));
            }
            if !ok {
                // Conceal with silence of the access unit's duration (a superframe is
                // 120 ms): a continuous timeline keeps the players from running dry
                // and re-buffering. The next good unit fades in.
                let samples = fmt.sample_rate() as usize * 120 / 1000 / fmt.num_aus();
                self.pcm.extend(std::iter::repeat_n(0.0, samples * 2));
                self.emit_pcm(fmt.sample_rate(), 2, out);
                self.fade_in = true;
                continue;
            }
            if self.record {
                out.push(ServiceEvent::Encoded { data: loas_frame(&fmt, &au), aac: true });
            }
            if let Some((xpad, fpad)) = dabplus_pad(&au) {
                let xpad = xpad.to_vec();
                self.handle_pad(&xpad, true, fpad, out);
            }
            self.stats.audio_bytes += au.len() as u64;
            if let Ok(path) = std::env::var("MAGICDAB_DUMP_AU") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = f.write_all(&(au.len() as u16).to_be_bytes());
                    let _ = f.write_all(&au);
                }
            }
            let before = self.pcm.len();
            let res = if let Kind::DabPlus { aac: Some(dec), .. } = &mut self.kind { Some(dec.decode(&au, &mut self.pcm)) } else { None };
            match res {
                Some(Ok(info)) => {
                    // a unit decoded far above full scale is a corrupted one that
                    // got past the checks (+25 dBFS was seen on a listener's
                    // meter): concealed like a bad one, not played as a bang
                    let peak = self.pcm[before.min(self.pcm.len())..].iter().fold(0f32, |m, &v| m.max(v.abs()));
                    if peak > MAX_DECODED {
                        self.pcm.truncate(before);
                        self.stats.decode_errors += 1;
                        log::debug!("AAC: a unit decoded to {:+.1} dBFS, concealed", 20.0 * peak.log10());
                        let samples = fmt.sample_rate() as usize * 120 / 1000 / fmt.num_aus();
                        self.pcm.extend(std::iter::repeat_n(0.0, samples * 2));
                        self.emit_pcm(fmt.sample_rate(), 2, out);
                        // (the decoder's state may be off for the next unit too)
                        self.mute_frames = self.mute_frames.max(1);
                        self.fade_in = true;
                        continue;
                    }
                    if info.channels > 0 && !self.pcm.is_empty() {
                        self.emit_pcm(info.sample_rate, info.channels, out);
                    }
                }
                Some(Err(e)) => {
                    self.stats.decode_errors += 1;
                    self.pcm.clear();
                    log::debug!("AAC decode error: {e}");
                }
                None => {}
            }
        }
    }

    fn push_mp2(&mut self, frame: &[u8], out: &mut Vec<ServiceEvent>) {
        let mut frames: Vec<Vec<u8>> = Vec::new();
        if let Kind::Mp2 { buf, .. } = &mut self.kind {
            buf.extend_from_slice(frame);
            loop {
                // find a Layer II header
                let mut i = 0;
                while i + 4 <= buf.len() && !(buf[i] == 0xFF && (buf[i + 1] & 0xF6) == 0xF4) {
                    i += 1;
                }
                if i > 0 {
                    buf.drain(..i);
                }
                if buf.len() < 4 {
                    break;
                }
                let mpeg1 = buf[1] & 0x08 != 0;
                let br_idx = (buf[2] >> 4) as usize;
                let sr_idx = ((buf[2] >> 2) & 0x03) as usize;
                let padding = ((buf[2] >> 1) & 1) as usize;
                if br_idx == 0 || br_idx == 15 || sr_idx == 3 {
                    buf.drain(..1);
                    continue;
                }
                let br = if mpeg1 { MP2_BITRATES_V1[br_idx] } else { MP2_BITRATES_V2[br_idx] };
                let sr = if mpeg1 { [44100, 48000, 32000][sr_idx] } else { [22050, 24000, 16000][sr_idx] };
                let len = (144 * br * 1000 / sr) as usize + padding;
                if buf.len() < len {
                    break;
                }
                frames.push(buf[..len].to_vec());
                buf.drain(..len);
            }
        }
        for f in frames {
            let mpeg1 = f[1] & 0x08 != 0;
            let br_idx = (f[2] >> 4) as usize;
            let mono = (f[3] >> 6) == 3;
            let br = if mpeg1 { MP2_BITRATES_V1[br_idx] } else { MP2_BITRATES_V2[br_idx] };
            let scf_crc = if mpeg1 && br < if mono { 56 } else { 112 } { 2 } else { 4 };
            if self.record {
                out.push(ServiceEvent::Encoded { data: f.clone(), aac: false });
            }
            if let Some((xpad, fpad)) = mp2_pad(&f, scf_crc) {
                let xpad = xpad.to_vec();
                self.handle_pad(&xpad, false, fpad, out);
            }
            self.stats.audio_bytes += f.len() as u64;
            if self.pad_only {
                continue;
            }
            let info = if let Kind::Mp2 { dec, .. } = &mut self.kind { dec.decode(&f, &mut self.pcm) } else { None };
            match info {
                Some(info) => {
                    let fmt = AudioFormat {
                        codec: "MP2".into(),
                        sample_rate: info.sample_rate,
                        channels: info.channels,
                        sbr: false,
                        ps: false,
                        stereo: info.channels == 2,
                    };
                    if fmt != self.format {
                        self.format = fmt.clone();
                        out.push(ServiceEvent::Format(fmt));
                    }
                    self.stats.au_ok += 1;
                    self.emit_pcm(info.sample_rate, info.channels, out);
                }
                None => {
                    self.stats.au_errors += 1;
                    self.stats.decode_errors += 1;
                }
            }
        }
    }
}
