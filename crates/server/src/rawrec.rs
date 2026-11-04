//! Raw IQ recording in the .uff format
//! (2048 byte XML header followed by unsigned 8 bit I/Q samples).

use num_complex::Complex32;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Folder of the recordings in the data folder.
pub const DIR: &str = "raw";

/// A recording's file name as it may be asked for (/raw/<name>).
pub fn safe_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = (5..=160).contains(&n.len())
        && n.ends_with(".uff")
        && !n.starts_with('.')
        && !n.contains("..")
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
    ok.then(|| n.to_string())
}

/// The recordings in `dir`, newest first: name, bytes, unix time.
pub fn list(dir: &std::path::Path, max: usize) -> Vec<(String, u64, u64)> {
    let mut v: Vec<(String, u64, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let m = e.metadata().ok()?;
            let time = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            safe_name(&name).map(|n| (n, m.len(), time))
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.2));
    v.truncate(max);
    v
}

pub struct RawRecorder {
    file: File,
    pub path: PathBuf,
    pub samples: u64,
    pub limit: u64,
    freq_khz: u32,
    started: String,
    device: String,
    buf: Vec<u8>,
}

fn header(device: &str, time: &str, freq_khz: u32, samples: u64) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<SDR>\n <Recorder Name=\"MagicDAB\" Version=\"v{}\"/>\n <Device Model=\"{}\" Name=\"{}\"/>\n <Time Unit=\"UTC\" Value=\"{}\"/>\n <Sample>\n  <Samplerate Unit=\"Hz\" Value=\"2048000\"/>\n  <Channels Bits=\"8\" Container=\"uint8\" Ordering=\"LSB\">\n   <Channel Value=\"I\"/>\n   <Channel Value=\"Q\"/>\n  </Channels>\n </Sample>\n <Datablocks>\n  <Datablock Count=\"{}\" Number=\"1\" Offset=\"2048\" Unit=\"Channel\">\n   <Frequency Unit=\"kHz\" Value=\"{}\"/>\n   <Modulation Value=\"DAB\"/>\n  </Datablock>\n </Datablocks>\n</SDR>\n",
        env!("CARGO_PKG_VERSION"),
        device,
        device,
        time,
        samples * 2,
        freq_khz
    );
    let mut h = xml.into_bytes();
    h.resize(2048, 0);
    h
}

impl RawRecorder {
    pub fn start(dir: PathBuf, channel: &str, freq_khz: u32, seconds: u32, device: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let now = chrono::Utc::now();
        let name = format!("{}_{}.uff", crate::localtime::now().format("%Y-%m-%d_%H%M%S"), channel);
        let path = dir.join(name);
        let mut file = File::create(&path)?;
        let started = now.format("%Y-%m-%d %H:%M:%S").to_string();
        file.write_all(&header(device, &started, freq_khz, 0))?;
        Ok(RawRecorder {
            file,
            path,
            samples: 0,
            limit: seconds as u64 * 2_048_000,
            freq_khz,
            started,
            device: device.to_string(),
            buf: Vec::with_capacity(65536),
        })
    }

    /// Append samples; returns false when the recording is complete.
    pub fn push(&mut self, s: &[Complex32]) -> bool {
        let take = ((self.limit - self.samples) as usize).min(s.len());
        self.buf.clear();
        for c in &s[..take] {
            self.buf.push((c.re * 128.0 + 127.4).round().clamp(0.0, 255.0) as u8);
            self.buf.push((c.im * 128.0 + 127.4).round().clamp(0.0, 255.0) as u8);
        }
        if self.file.write_all(&self.buf).is_err() {
            return false;
        }
        self.samples += take as u64;
        self.samples < self.limit
    }

    pub fn finish(mut self) -> PathBuf {
        let h = header(&self.device, &self.started, self.freq_khz, self.samples);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = self.file.write_all(&h);
        self.path
    }
}
