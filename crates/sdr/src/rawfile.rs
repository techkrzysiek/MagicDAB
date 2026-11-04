//! Raw IQ file playback: .uff files (XML header) and plain
//! u8 / s16 .raw / .iq recordings at 2.048 MS/s, paced in real time.

use crate::{convert_u8, DeviceStatus, GainMode, InputDevice, SampleCallback, SAMPLE_RATE};
use num_complex::Complex32;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub offset: u64,
    pub s16: bool,
    pub frequency_khz: Option<u32>,
    pub recorded: Option<String>,
    pub samples: u64,
}

/// Parse the header of a recording.
pub fn probe(path: &str) -> std::io::Result<FileInfo> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head)?;
    head.truncate(n);
    let mut info = FileInfo::default();
    if head.starts_with(b"<?xml") {
        let txt = String::from_utf8_lossy(&head);
        let attr = |tag: &str, name: &str| -> Option<String> {
            let i = txt.find(tag)?;
            let rest = &txt[i..];
            let end = rest.find('>')?;
            let el = &rest[..end];
            let j = el.find(&format!("{name}=\""))?;
            let v = &el[j + name.len() + 2..];
            Some(v[..v.find('"')?].to_string())
        };
        info.offset = attr("<Datablock ", "Offset").and_then(|v| v.parse().ok()).unwrap_or(2048);
        info.s16 = attr("<Channels ", "Bits").map(|b| b == "16").unwrap_or(false);
        info.frequency_khz = attr("<Frequency ", "Value").and_then(|v| v.parse().ok()).filter(|&f: &u32| f > 0);
        info.recorded = attr("<Time ", "Value");
    } else if path.to_lowercase().ends_with(".s16") || path.to_lowercase().contains("s16") {
        info.s16 = true;
    }
    let bytes_per_sample = if info.s16 { 4 } else { 2 };
    info.samples = len.saturating_sub(info.offset) / bytes_per_sample;
    Ok(info)
}

pub struct RawFile {
    path: String,
    looping: bool,
    stop: Arc<AtomicBool>,
    position: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
    info: Option<FileInfo>,
    frequency: u32,
    error: Option<String>,
}

impl RawFile {
    pub fn new(path: String, looping: bool) -> Self {
        let info = probe(&path).ok();
        RawFile {
            path,
            looping,
            stop: Arc::new(AtomicBool::new(false)),
            position: Arc::new(AtomicU64::new(0)),
            thread: None,
            info,
            frequency: 0,
            error: None,
        }
    }
}

impl InputDevice for RawFile {
    fn status(&self) -> DeviceStatus {
        let total = self.info.as_ref().map(|i| i.samples).unwrap_or(0);
        DeviceStatus {
            kind: "rawfile".into(),
            name: std::path::Path::new(&self.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            connected: self.thread.is_some(),
            frequency: self.info.as_ref().and_then(|i| i.frequency_khz).map(|f| f * 1000).unwrap_or(self.frequency),
            gain_db: None,
            gain_index: None,
            gains: Vec::new(),
            rf_level: None,
            error: self.error.clone(),
            position: Some(self.position.load(Ordering::Relaxed) as f64 / SAMPLE_RATE as f64),
            duration: Some(total as f64 / SAMPLE_RATE as f64),
            stalls: 0,
            last_stall_ms: None,
            stream_rate: None,
        }
    }

    fn start(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        self.stop();
        let info = probe(&self.path).map_err(|e| {
            let e = format!("cannot open {}: {e}", self.path);
            self.error = Some(e.clone());
            e
        })?;
        self.info = Some(info.clone());
        self.frequency = frequency;
        self.stop.store(false, Ordering::Relaxed);
        let stop = self.stop.clone();
        let position = self.position.clone();
        let path = self.path.clone();
        let looping = self.looping;
        self.thread = Some(
            std::thread::Builder::new()
                .name("rawfile".into())
                .spawn(move || {
                    let Ok(mut f) = File::open(&path) else { return };
                    let chunk = 16384usize; // samples per block (8 ms)
                    let bps = if info.s16 { 4 } else { 2 };
                    let mut buf = vec![0u8; chunk * bps];
                    let mut samples: Vec<Complex32> = Vec::with_capacity(chunk);
                    loop {
                        if f.seek(SeekFrom::Start(info.offset)).is_err() {
                            return;
                        }
                        position.store(0, Ordering::Relaxed);
                        let start = Instant::now();
                        let mut sent = 0u64;
                        loop {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            let n = match f.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n - n % bps,
                            };
                            if info.s16 {
                                samples.clear();
                                for c in buf[..n].chunks_exact(4) {
                                    samples.push(Complex32::new(
                                        i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0,
                                        i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0,
                                    ));
                                }
                            } else {
                                convert_u8(&buf[..n], &mut samples);
                            }
                            callback(&samples);
                            sent += samples.len() as u64;
                            position.store(sent, Ordering::Relaxed);
                            let due = Duration::from_secs_f64(sent as f64 / SAMPLE_RATE as f64);
                            let el = start.elapsed();
                            if due > el {
                                std::thread::sleep(due - el);
                            }
                        }
                        if !looping {
                            return;
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        self.error = None;
        Ok(())
    }

    fn tune(&mut self, frequency: u32) -> Result<(), String> {
        // a recording cannot be retuned; keep playing
        self.frequency = frequency;
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn set_gain_mode(&mut self, _mode: GainMode) {}
}

impl Drop for RawFile {
    fn drop(&mut self) {
        self.stop();
    }
}
