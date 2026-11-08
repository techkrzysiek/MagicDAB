//! DX recordings: a multiplex heard on a channel for the first time is
//! recorded as ETI (the receiver gives the frames: receiver.rs,
//! RxCommand::RecordEti) into data/dxrec, a JSON file beside each saying
//! what it is, and a .side file with what the frames do not carry: TII, the
//! spectrum and the signal. Anyone with a link plays one back later on a
//! side receiver of its own (radio/slots.rs) and moves through it; the admin
//! also downloads or deletes them, and may load an .eti file of its own, or
//! an .edi one (an EDI stream over TCP as saved, e.g.
//! `nc mux 9201 > mux.edi`), for a day. radio/dxrec.rs decides when to
//! record.

use crossbeam_channel::{bounded, Sender};
use magicdab_dab::eti::{StreamParser, ETI_FRAME};
use magicdab_dab::receiver::EtiFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The folder in the data folder.
pub const DIR: &str = "dxrec";
/// ETI frames a second (one every 24 ms) and a minute.
pub const FRAMES_PER_SECOND: f64 = 1000.0 / 24.0;
pub const FRAMES_PER_MINUTE: u64 = 2500;
/// An uploaded file may be this big (about 14 minutes), and stays this long
/// unless it is being played.
pub const UPLOAD_MAX: u64 = 256 << 20;
pub const UPLOAD_KEEP: Duration = Duration::from_secs(24 * 3600);
/// Downloads go at this pace: the listeners' audio shares the uplink.
pub const DOWNLOAD_BYTES_PER_SECOND: u64 = 512 * 1024;
/// Downloads at once from one address and in all, and started per hour from
/// one address: a recording is big, the receiver's uplink small.
pub const DOWNLOADS_PER_ADDRESS: usize = 1;
pub const DOWNLOADS_AT_ONCE: usize = 2;
pub const DOWNLOADS_PER_HOUR: usize = 10;

#[derive(Default)]
struct Downloads {
    active: std::collections::HashMap<String, usize>,
    started: std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>,
}

static DOWNLOADS: std::sync::LazyLock<parking_lot::Mutex<Downloads>> = std::sync::LazyLock::new(Default::default);

/// A download going on; its place is free again when it is dropped (done,
/// or the browser went away).
pub struct DownloadSlot {
    address: String,
}

impl Drop for DownloadSlot {
    fn drop(&mut self) {
        let mut d = DOWNLOADS.lock();
        if let Some(n) = d.active.get_mut(&self.address) {
            *n -= 1;
            if *n == 0 {
                d.active.remove(&self.address);
            }
        }
    }
}

/// A place for a download to `address`, or how long to wait for one.
pub fn download_slot(address: &str) -> Result<DownloadSlot, Duration> {
    let hour = Duration::from_secs(3600);
    let now = std::time::Instant::now();
    let mut d = DOWNLOADS.lock();
    let starts = d.started.entry(address.to_string()).or_default();
    while starts.front().is_some_and(|t| now.duration_since(*t) > hour) {
        starts.pop_front();
    }
    if starts.len() >= DOWNLOADS_PER_HOUR {
        let oldest = starts.front().copied().unwrap_or(now);
        return Err(hour.saturating_sub(now.duration_since(oldest)).max(Duration::from_secs(1)));
    }
    let mine = d.active.get(address).copied().unwrap_or(0);
    let all: usize = d.active.values().sum();
    if mine >= DOWNLOADS_PER_ADDRESS || all >= DOWNLOADS_AT_ONCE {
        return Err(Duration::from_secs(60));
    }
    d.started.entry(address.to_string()).or_default().push_back(now);
    *d.active.entry(address.to_string()).or_default() += 1;
    Ok(DownloadSlot { address: address.to_string() })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MetaService {
    pub sid: String,
    pub label: String,
    pub bitrate: u16,
    pub codec: String,
}

/// What a recording is: the JSON file beside the .eti.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Meta {
    /// Unix time of the start.
    pub time: i64,
    /// The start in local time, for reading.
    pub started: String,
    pub channel: String,
    pub frequency: u32,
    /// As the change log writes them: "E2", "7E3A".
    pub ecc: String,
    pub eid: String,
    pub label: String,
    pub services: Vec<MetaService>,
    /// SNR at the start and its mean while recording (dB).
    pub snr: f32,
    pub snr_mean: f32,
    /// Mean FIB error ratio while recording.
    pub fib_error_rate: f32,
    /// Transmitters heard ("main-sub").
    pub tii: Vec<String>,
    /// Seconds, bytes and frames recorded.
    pub duration: f32,
    pub size: u64,
    pub frames: u64,
    /// Why: ensembleNew, ensembleId, manual; upload for a loaded file.
    pub reason: String,
    /// How it ended: complete, retuned, noSync, stopped, error; empty while
    /// it runs.
    pub ended: String,
    pub device: String,
    pub version: String,
}

/// The JSON file beside a recording.
pub fn sidecar(path: &Path) -> PathBuf {
    path.with_extension("json")
}

pub fn read_meta(path: &Path) -> Option<Meta> {
    serde_json::from_str(&std::fs::read_to_string(sidecar(path)).ok()?).ok()
}

pub fn write_meta(path: &Path, meta: &Meta) {
    if let Ok(s) = serde_json::to_string_pretty(meta) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(tmp, sidecar(path));
        }
    }
}

/// A file name of the folder as a client may give it: no path, an .eti or
/// an .edi.
pub fn safe_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = (5..=160).contains(&n.len())
        && (n.ends_with(".eti") || n.ends_with(".edi"))
        && !n.starts_with('.')
        && !n.contains("..")
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
    ok.then(|| n.to_string())
}

/// The name an uploaded file gets: "upload_<time>_<its name>.eti" (or .edi).
pub fn upload_name(original: &str, edi: bool) -> String {
    let file = original.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = file.strip_suffix(".eti").or_else(|| file.strip_suffix(".edi")).unwrap_or(file);
    let stem: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(40).collect();
    let stem = if stem.is_empty() { "file".to_string() } else { stem };
    format!("upload_{}_{stem}.{}", crate::localtime::now().format("%Y-%m-%d_%H%M%S"), if edi { "edi" } else { "eti" })
}

/// An .edi file: EDI AF packets, not ETI frames.
pub fn is_edi(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "edi")
}

pub fn is_upload(name: &str) -> bool {
    name.starts_with("upload_")
}

/// Whether a start of a file holds an ETI(NI) frame (more than one frame of
/// bytes is needed to be sure).
pub fn looks_like_eti(head: &[u8]) -> bool {
    let mut found = false;
    StreamParser::new(false).push(head, |_| found = true);
    found
}

/// Whether a start of a file holds an EDI AF packet with an ETI frame.
pub fn looks_like_edi(head: &[u8]) -> bool {
    let mut found = false;
    StreamParser::new(true).push(head, |_| found = true);
    found
}

/// A recording of the folder for the list: the JSON beside it, the name and
/// the size of the file.
fn entry(path: &Path) -> Option<Value> {
    let name = path.file_name()?.to_str()?.to_string();
    let len = std::fs::metadata(path).ok()?.len();
    let mut meta = read_meta(path)
        .unwrap_or_else(|| Meta { reason: if is_upload(&name) { "upload".into() } else { String::new() }, ..Default::default() });
    if meta.time == 0 {
        meta.time = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }
    // (an .edi file says nothing of its length by its size)
    if meta.frames == 0 && !is_edi(path) {
        meta.frames = len / ETI_FRAME as u64;
        meta.duration = meta.frames as f32 / FRAMES_PER_SECOND as f32;
    }
    meta.size = len;
    let mut v = serde_json::to_value(meta).ok()?;
    v["name"] = Value::String(name);
    // TII, spectrum and signal to play back with it
    v["side"] = Value::Bool(side_path(path).is_file());
    Some(v)
}

/// The recordings, the newest first.
pub fn list(dir: &Path) -> Vec<Value> {
    let mut v: Vec<Value> = files(dir).iter().filter_map(|p| entry(p)).collect();
    v.sort_by_key(|e| std::cmp::Reverse(e["time"].as_i64().unwrap_or(0)));
    v
}

/// The .eti and .edi files of the folder.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "eti" || x == "edi") && p.is_file()).collect()
}

/// Bytes of a recording with what was heard beside its frames.
fn bytes_of(path: &Path) -> Option<u64> {
    let side = std::fs::metadata(side_path(path)).map_or(0, |m| m.len());
    Some(std::fs::metadata(path).ok()?.len() + side)
}

/// Bytes of the recordings.
pub fn used(dir: &Path) -> u64 {
    files(dir).iter().filter_map(|p| bytes_of(p)).sum()
}

fn time_of(path: &Path) -> i64 {
    read_meta(path).map(|m| m.time).filter(|t| *t > 0).unwrap_or_else(|| {
        std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

/// Removes the oldest recordings until the folder holds at most `max` bytes;
/// `keep` (the one being written) stays. The names removed.
pub fn enforce_cap(dir: &Path, max: u64, keep: Option<&Path>) -> Vec<String> {
    let mut all: Vec<(i64, u64, PathBuf)> =
        files(dir).into_iter().filter(|p| keep != Some(p.as_path())).filter_map(|p| Some((time_of(&p), bytes_of(&p)?, p))).collect();
    let mut total = used(dir);
    all.sort();
    let mut removed = Vec::new();
    for (_, len, p) in all {
        if total <= max {
            break;
        }
        if remove(&p).is_ok() {
            total -= len;
            removed.push(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        }
    }
    removed
}

/// Uploads older than UPLOAD_KEEP that are not being played go.
pub fn prune_uploads(dir: &Path, in_use: &[String]) -> Vec<String> {
    let now = crate::servicelist::now();
    let mut removed = Vec::new();
    for p in files(dir) {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !is_upload(&name) || in_use.contains(&name) {
            continue;
        }
        if now - time_of(&p) > UPLOAD_KEEP.as_secs() as i64 && remove(&p).is_ok() {
            removed.push(name);
        }
    }
    removed
}

/// Deletes a recording and the files beside it.
pub fn remove(path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sidecar(path));
    let _ = std::fs::remove_file(side_path(path));
    std::fs::remove_file(path)
}

// ---------------------------------------------------------------- beside the frames

/// The frames carry no TII, spectrum or signal (those come from the I/Q
/// samples): what the receiver heard of them while recording goes into
/// `<name>.side`: this, then records of the frame they came with (u32 LE),
/// their kind (u8), their length (u32 LE) and their bytes.
const SIDE_MAGIC: &[u8; 8] = b"MDXSIDE1";

pub fn side_path(path: &Path) -> PathBuf {
    path.with_extension("side")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideKind {
    /// {"codes": [[main, sub, level dB, margin dB], ...], "pairs": [...]}
    Tii = 1,
    /// The spectrum message, packed (pack_spectrum).
    Spectrum = 2,
    /// The signal message of the main receiver.
    Signal = 3,
}

impl SideKind {
    const ALL: [SideKind; 3] = [SideKind::Tii, SideKind::Spectrum, SideKind::Signal];

    fn from_u8(k: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|x| *x as u8 == k)
    }
}

/// A record of the .side file played back.
#[derive(Clone, Debug, PartialEq)]
pub enum Side {
    Tii(Value),
    /// The header and the payload of the spectrum message (hub BIN_SPECTRUM).
    Spectrum(Value, Vec<u8>),
    Signal(Value),
}

/// Values of the spectrum message (f32 LE): spectrum and null symbol in dB
/// (bins each), constellation points (re, im), the delay profile in dB (taps).
fn spectrum_layout(h: &Value) -> Option<(usize, usize, usize)> {
    let bins = h["bins"].as_u64()? as usize;
    let points = h["points"].as_u64()? as usize;
    let taps = h["taps"].as_u64()? as usize;
    Some((bins, points, taps))
}

/// The spectrum message in about a sixth of its bytes: the dB values in
/// hundredths and the constellation in 1/4096 as i16, deflated.
pub fn pack_spectrum(h: &Value, payload: &[u8]) -> Option<Vec<u8>> {
    let (bins, points, taps) = spectrum_layout(h)?;
    if payload.len() != (2 * bins + 2 * points + taps) * 4 {
        return None;
    }
    let head = serde_json::to_vec(h).ok()?;
    let mut raw = Vec::with_capacity(4 + head.len() + payload.len() / 2);
    raw.extend_from_slice(&(head.len() as u32).to_le_bytes());
    raw.extend_from_slice(&head);
    let constellation = 2 * bins..2 * bins + 2 * points;
    for (i, c) in payload.chunks_exact(4).enumerate() {
        let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let q = if constellation.contains(&i) { v * 4096.0 } else { v * 100.0 };
        // (a NaN becomes 0)
        raw.extend_from_slice(&(q.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&raw).ok()?;
    enc.finish().ok()
}

pub fn unpack_spectrum(bytes: &[u8]) -> Option<(Value, Vec<u8>)> {
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(bytes).take(8 << 20).read_to_end(&mut raw).ok()?;
    let hl = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let h: Value = serde_json::from_slice(raw.get(4..4 + hl)?).ok()?;
    let (bins, points, taps) = spectrum_layout(&h)?;
    let values = raw.get(4 + hl..)?;
    if values.len() != (2 * bins + 2 * points + taps) * 2 {
        return None;
    }
    let constellation = 2 * bins..2 * bins + 2 * points;
    let mut payload = Vec::with_capacity(values.len() * 2);
    for (i, c) in values.chunks_exact(2).enumerate() {
        let q = i16::from_le_bytes([c[0], c[1]]) as f32;
        let v = if constellation.contains(&i) { q / 4096.0 } else { q / 100.0 };
        payload.extend_from_slice(&v.to_le_bytes());
    }
    Some((h, payload))
}

/// The records of a .side file: where each is, read when due.
struct SideReader {
    file: File,
    /// (frame, kind, offset of the bytes, length)
    index: Vec<(u64, SideKind, u64, u32)>,
}

impl SideReader {
    fn open(path: &Path) -> Option<SideReader> {
        let mut file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic).ok()?;
        if &magic != SIDE_MAGIC {
            return None;
        }
        let mut index = Vec::new();
        let mut at = 8u64;
        let mut head = [0u8; 9];
        // (a record cut short at the end, e.g. the program ended, is left out)
        while at + 9 <= len && file.read_exact(&mut head).is_ok() {
            let frame = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as u64;
            let n = u32::from_le_bytes([head[5], head[6], head[7], head[8]]);
            if at + 9 + n as u64 > len {
                break;
            }
            if let Some(kind) = SideKind::from_u8(head[4]) {
                index.push((frame, kind, at + 9, n));
            }
            at += 9 + n as u64;
            if file.seek(SeekFrom::Start(at)).is_err() {
                break;
            }
        }
        Some(SideReader { file, index })
    }

    fn read(&mut self, i: usize) -> Option<Side> {
        let &(_, kind, offset, n) = self.index.get(i)?;
        let mut bytes = vec![0u8; n as usize];
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        self.file.read_exact(&mut bytes).ok()?;
        match kind {
            SideKind::Tii => serde_json::from_slice(&bytes).ok().map(Side::Tii),
            SideKind::Spectrum => unpack_spectrum(&bytes).map(|(h, p)| Side::Spectrum(h, p)),
            SideKind::Signal => serde_json::from_slice(&bytes).ok().map(Side::Signal),
        }
    }

    /// Sends the records up to frame `frame` from record `from` on; the next
    /// one to send.
    fn send_until(&mut self, from: usize, frame: u64, tx: &Sender<Side>) -> usize {
        let mut i = from;
        while self.index.get(i).is_some_and(|r| r.0 <= frame) {
            if let Some(s) = self.read(i) {
                // (a radio thread behind loses some: the next ones come soon)
                let _ = tx.try_send(s);
            }
            i += 1;
        }
        i
    }

    /// After a move to `frame`: the last record of each kind before it (what
    /// was shown then), and the next one to send.
    fn seek(&mut self, frame: u64, tx: &Sender<Side>) -> usize {
        let next = self.index.partition_point(|r| r.0 < frame);
        for kind in SideKind::ALL {
            if let Some(j) = self.index[..next].iter().rposition(|r| r.1 == kind) {
                if let Some(s) = self.read(j) {
                    let _ = tx.try_send(s);
                }
            }
        }
        next
    }
}

// ---------------------------------------------------------------- writing

enum Msg {
    Frame(Vec<u8>),
    /// A record of the .side file (the frame it came with, its kind, bytes).
    Side(u64, SideKind, Vec<u8>),
    /// The spectrum message, packed on the writer's thread.
    Spectrum(u64, Value, Vec<u8>),
    Finish(Meta),
}

/// Writes the frames of a recording on a thread of its own: the radio thread
/// never waits for the disk.
pub struct Writer {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
    pub frames: u64,
    pub bytes: u64,
    /// Frames the thread could not take (the disk too slow for long).
    pub dropped: u64,
    failed: Arc<AtomicBool>,
    pub meta: Meta,
}

impl Writer {
    /// Starts a recording in `dir`; the JSON beside it says so at once.
    pub fn start(dir: &Path, mut meta: Meta) -> std::io::Result<Writer> {
        std::fs::create_dir_all(dir)?;
        let when = crate::localtime::at(chrono::DateTime::from_timestamp(meta.time, 0).unwrap_or_default());
        meta.started = when.format("%Y-%m-%d %H:%M:%S %:z").to_string();
        meta.version = crate::radio::VERSION.to_string();
        let path = dir.join(format!("{}_{}_{}.eti", when.format("%Y-%m-%d_%H%M%S"), meta.channel, meta.eid));
        let file = File::create(&path)?;
        write_meta(&path, &meta);
        let (tx, rx) = bounded::<Msg>(4096);
        let failed = Arc::new(AtomicBool::new(false));
        let (fl, p) = (failed.clone(), path.clone());
        let thread = std::thread::Builder::new().name("dxrec-writer".into()).spawn(move || {
            let mut out = BufWriter::with_capacity(256 * 1024, file);
            let mut written = 0u64;
            // (made with the first record: a receiver that hears none, e.g. a stream, has none)
            let mut side: Option<BufWriter<File>> = None;
            let mut side_failed = false;
            let put_side = |side: &mut Option<BufWriter<File>>, side_failed: &mut bool, frame: u64, kind: SideKind, bytes: &[u8]| {
                if *side_failed {
                    return;
                }
                if side.is_none() {
                    match File::create(side_path(&p)) {
                        Ok(f) => {
                            let mut w = BufWriter::with_capacity(64 * 1024, f);
                            *side_failed = w.write_all(SIDE_MAGIC).is_err();
                            *side = Some(w);
                        }
                        Err(e) => {
                            log::warn!("DX recording {}: no TII and spectrum beside it: {e}", p.display());
                            *side_failed = true;
                            return;
                        }
                    }
                }
                let Some(w) = side.as_mut() else { return };
                let mut head = [0u8; 9];
                head[..4].copy_from_slice(&(frame.min(u32::MAX as u64) as u32).to_le_bytes());
                head[4] = kind as u8;
                head[5..].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
                if w.write_all(&head).and_then(|_| w.write_all(bytes)).is_err() {
                    *side_failed = true;
                }
            };
            while let Ok(m) = rx.recv() {
                match m {
                    Msg::Side(frame, kind, bytes) => put_side(&mut side, &mut side_failed, frame, kind, &bytes),
                    Msg::Spectrum(frame, h, payload) => {
                        if let Some(bytes) = pack_spectrum(&h, &payload) {
                            put_side(&mut side, &mut side_failed, frame, SideKind::Spectrum, &bytes);
                        }
                    }
                    Msg::Frame(f) => {
                        if fl.load(Ordering::Relaxed) {
                            continue;
                        }
                        if let Err(e) = out.write_all(&f) {
                            log::warn!("DX recording {}: cannot write: {e}", p.display());
                            fl.store(true, Ordering::Relaxed);
                        } else {
                            written += f.len() as u64;
                        }
                    }
                    Msg::Finish(mut meta) => {
                        let _ = out.flush();
                        if let Some(w) = side.as_mut() {
                            let _ = w.flush();
                        }
                        meta.size = written;
                        if fl.load(Ordering::Relaxed) {
                            meta.ended = "error".into();
                        }
                        write_meta(&p, &meta);
                        return;
                    }
                }
            }
            let _ = out.flush();
            if let Some(w) = side.as_mut() {
                let _ = w.flush();
            }
        })?;
        Ok(Writer { tx: Some(tx), thread: Some(thread), path, frames: 0, bytes: 0, dropped: 0, failed, meta })
    }

    pub fn push(&mut self, frame: Vec<u8>) {
        self.bytes += frame.len() as u64;
        self.frames += 1;
        if let Some(tx) = &self.tx {
            if tx.try_send(Msg::Frame(frame)).is_err() {
                self.dropped += 1;
            }
        }
    }

    /// What the receiver heard with the frames so far (TII, signal).
    pub fn side(&mut self, kind: SideKind, bytes: Vec<u8>) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::Side(self.frames, kind, bytes));
        }
    }

    /// The spectrum message of the receiver now.
    pub fn spectrum(&mut self, h: Value, payload: Vec<u8>) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::Spectrum(self.frames, h, payload));
        }
    }

    /// The disk failed: the recording is over.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn seconds(&self) -> f32 {
        self.frames as f32 / FRAMES_PER_SECOND as f32
    }

    /// Ends the recording; the thread finishes the file and the JSON beside
    /// it by itself (`ended` says how it ended).
    pub fn finish(mut self, ended: &str) -> Meta {
        let mut meta = self.meta.clone();
        meta.ended = ended.to_string();
        meta.frames = self.frames;
        meta.duration = self.seconds();
        meta.size = self.bytes;
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Finish(meta.clone()));
        }
        self.thread.take();
        meta
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // (a recording not finished, e.g. the program ends: the file as it is)
        if let Some(tx) = self.tx.take() {
            let mut meta = self.meta.clone();
            meta.ended = "stopped".into();
            meta.frames = self.frames;
            meta.duration = self.seconds();
            let _ = tx.send(Msg::Finish(meta));
        }
    }
}

// ---------------------------------------------------------------- playing

/// Plays an .eti or .edi file into a receiver (its ETI frame channel) at
/// the pace of the air, in a loop, on a thread of its own, with what was
/// heard beside the frames (the .side file) at their time; moves to any
/// point of it when asked (seek).
pub struct Player {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub name: String,
    /// What the JSON beside it says.
    pub meta: Option<Meta>,
    /// The .side file is there.
    pub side: bool,
    /// Frames of the file (an .eti: its size, padded frames; an .edi: as
    /// counted when it was loaded).
    pub frames: u64,
    /// Frames played of this pass, and passes done.
    position: Arc<AtomicU64>,
    loops: Arc<AtomicU64>,
    /// The frame to go to (u64::MAX: none asked for).
    seek: Arc<AtomicU64>,
}

impl Player {
    pub fn start(path: &Path, tx: Sender<EtiFrame>, side_tx: Option<Sender<Side>>) -> std::io::Result<Player> {
        let len = std::fs::metadata(path)?.len();
        let edi = is_edi(path);
        let meta = read_meta(path);
        let frames = if edi { meta.as_ref().map_or(0, |m| m.frames) } else { len / ETI_FRAME as u64 };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut side = SideReader::open(&side_path(path)).filter(|_| side_tx.is_some());
        let has_side = side.is_some();
        let stop = Arc::new(AtomicBool::new(false));
        let position = Arc::new(AtomicU64::new(0));
        let loops = Arc::new(AtomicU64::new(0));
        let seek = Arc::new(AtomicU64::new(u64::MAX));
        let (st, pos, lp, sk, p) = (stop.clone(), position.clone(), loops.clone(), seek.clone(), path.to_path_buf());
        let mut f = File::open(path)?;
        let thread = std::thread::Builder::new().name("eti-file".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut parser = StreamParser::new(edi);
            let mut pending: std::collections::VecDeque<EtiFrame> = Default::default();
            // the next frame, the one played at `start`, the next side record
            let (mut n, mut base, mut start, mut next_side) = (0u64, 0u64, Instant::now(), 0usize);
            let mut played = false;
            while !st.load(Ordering::Relaxed) {
                let to = sk.swap(u64::MAX, Ordering::Relaxed);
                // (an .edi without its count of frames plays only from the start)
                if to != u64::MAX && frames > 0 {
                    let to = to.min(frames - 1);
                    let byte = if edi { len.checked_mul(to).map_or(0, |b| b / frames.max(1)) } else { to * ETI_FRAME as u64 };
                    if f.seek(SeekFrom::Start(byte)).is_ok() {
                        parser = StreamParser::new(edi);
                        pending.clear();
                        (n, base, start) = (to, to, Instant::now());
                        pos.store(n, Ordering::Relaxed);
                        if let (Some(r), Some(tx)) = (side.as_mut(), side_tx.as_ref()) {
                            next_side = r.seek(to, tx);
                        }
                    }
                }
                let Some(fr) = pending.pop_front() else {
                    match f.read(&mut buf) {
                        Ok(r) if r > 0 => parser.push(&buf[..r], |fr| pending.push_back(fr)),
                        _ => {
                            if !played {
                                log::warn!("{}: no {} in the file", p.display(), if edi { "EDI packets" } else { "ETI frames" });
                                return;
                            }
                            // the end: the file again
                            if f.seek(SeekFrom::Start(0)).is_err() {
                                return;
                            }
                            parser = StreamParser::new(edi);
                            (n, base, start, next_side) = (0, 0, Instant::now(), 0);
                            pos.store(0, Ordering::Relaxed);
                            lp.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    continue;
                };
                // frame n is due 24 ms after the one before
                let due = Duration::from_secs_f64((n - base) as f64 / FRAMES_PER_SECOND);
                let el = start.elapsed();
                if due > el {
                    std::thread::sleep(due - el);
                }
                let _ = tx.send_timeout(fr, Duration::from_millis(200));
                played = true;
                n += 1;
                pos.store(n, Ordering::Relaxed);
                // what the receiver heard with this frame
                if let (Some(r), Some(tx)) = (side.as_mut(), side_tx.as_ref()) {
                    next_side = r.send_until(next_side, n, tx);
                }
            }
        })?;
        Ok(Player { stop, thread: Some(thread), name, meta, side: has_side, frames, position, loops, seek })
    }

    /// Seconds into the file, seconds of the file, passes done.
    pub fn progress(&self) -> (f64, f64, u64) {
        (
            self.position.load(Ordering::Relaxed) as f64 / FRAMES_PER_SECOND,
            self.frames as f64 / FRAMES_PER_SECOND,
            self.loops.load(Ordering::Relaxed),
        )
    }

    /// Plays on from `seconds` into the file.
    pub fn seek(&self, seconds: f64) {
        let frame = (seconds.max(0.0) * FRAMES_PER_SECOND).round() as u64;
        self.seek.store(frame, Ordering::Relaxed);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn downloads_are_limited_per_address() {
        // (addresses of this test only: the limits are shared by the process)
        let a = download_slot("198.51.100.1").expect("the first");
        assert!(download_slot("198.51.100.1").is_err(), "one at a time per address");
        let b = download_slot("198.51.100.2").expect("another address");
        assert!(download_slot("198.51.100.3").is_err(), "two at once in all");
        drop(a);
        drop(b);
        for _ in 0..DOWNLOADS_PER_HOUR - 1 {
            drop(download_slot("198.51.100.1").expect("within the hour's count"));
        }
        assert!(download_slot("198.51.100.1").is_err(), "ten an hour per address");
        drop(download_slot("198.51.100.4").expect("others still may"));
    }

    use super::*;
    use magicdab_dab::eti::{build_af, build_eti, parse_eti, Stream};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("magicdab-dxrec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn af(n: u8) -> Vec<u8> {
        let fic = vec![n; 96];
        let data: Vec<u8> = (0..144).map(|i| i as u8 ^ n).collect();
        build_af(n as u16, n, &fic, &[Stream { scid: 2, sad: 0, tpl: 0x22, data: &data }])
    }

    fn frame(n: u8) -> Vec<u8> {
        let fic = vec![n; 96];
        let data: Vec<u8> = (0..144).map(|i| i as u8 ^ n).collect();
        build_eti(n, &fic, &[Stream { scid: 2, sad: 0, tpl: 0x22, data: &data }])
    }

    #[test]
    fn names_of_the_folder_only() {
        assert_eq!(safe_name(" 2026-10-30_061512_8C_6D2B.eti "), Some("2026-10-30_061512_8C_6D2B.eti".into()));
        assert_eq!(safe_name("upload_2026-10-30_061512_odr-mux.eti"), Some("upload_2026-10-30_061512_odr-mux.eti".into()));
        assert_eq!(safe_name("upload_2026-10-30_061512_odr-mux.edi"), Some("upload_2026-10-30_061512_odr-mux.edi".into()));
        for bad in
            ["../x.eti", "a/b.eti", "x.uff", "", ".eti", ".hidden.eti", "a b.eti", "x\n.eti", "ą.eti", &format!("{}.eti", "a".repeat(200))]
        {
            assert_eq!(safe_name(bad), None, "{bad:?}");
        }
        let up = upload_name("C:\\Users\\k\\Desktop\\mój mux (1).eti", false);
        assert!(up.starts_with("upload_") && up.ends_with("_mjmux1.eti"), "{up}");
        assert!(is_upload(&up));
        assert!(!is_upload("2026-10-30_061512_8C_6D2B.eti"));
        assert!(upload_name("", false).ends_with("_file.eti"));
        // (the format is what the file holds, whatever its name says)
        assert!(upload_name("mux.eti", true).ends_with("_mux.edi"));
        assert!(upload_name("mux.edi", false).ends_with("_mux.eti"));
    }

    #[test]
    fn a_file_must_hold_an_eti_frame() {
        let mut bytes = vec![0u8; 100];
        bytes.extend(frame(1));
        bytes.extend(frame(2));
        assert!(looks_like_eti(&bytes));
        assert!(!looks_like_eti(&vec![0x55u8; 20000]));
        assert!(!looks_like_eti(b"<?xml version=\"1.0\"?><SDR>"));
        assert!(!looks_like_edi(&bytes));
        let mut edi = vec![0u8; 100];
        edi.extend(af(1));
        assert!(looks_like_edi(&edi));
        assert!(!looks_like_eti(&edi));
    }

    #[test]
    fn the_folder_stays_under_its_cap_and_old_uploads_go() {
        let d = dir("cap");
        let mut names = Vec::new();
        for (i, size) in [(1, 10u64), (2, 20), (3, 30), (4, 40)] {
            let p = d.join(format!("rec{i}.eti"));
            std::fs::write(&p, vec![0u8; size as usize]).unwrap();
            write_meta(&p, &Meta { time: 1_700_000_000 + i, ..Default::default() });
            names.push(p);
        }
        assert_eq!(used(&d), 100);
        // the oldest go until the folder (the one being written included) fits
        let removed = enforce_cap(&d, 45, Some(&names[3]));
        assert_eq!(removed, vec!["rec1.eti", "rec2.eti", "rec3.eti"]);
        assert!(!sidecar(&names[0]).exists());
        assert_eq!(used(&d), 40);
        // the one being written stays whatever the cap
        assert!(enforce_cap(&d, 30, Some(&names[3])).is_empty());
        assert!(enforce_cap(&d, 1000, None).is_empty());
        // an upload of yesterday goes unless it is played, one of today stays
        let old = d.join("upload_old_x.eti");
        std::fs::write(&old, b"x").unwrap();
        write_meta(&old, &Meta { time: crate::servicelist::now() - 90_000, reason: "upload".into(), ..Default::default() });
        let fresh = d.join("upload_new_y.eti");
        std::fs::write(&fresh, b"y").unwrap();
        write_meta(&fresh, &Meta { time: crate::servicelist::now() - 100, reason: "upload".into(), ..Default::default() });
        assert!(prune_uploads(&d, &["upload_old_x.eti".to_string()]).is_empty());
        assert_eq!(prune_uploads(&d, &[]), vec!["upload_old_x.eti"]);
        assert!(fresh.exists());
        let list = list(&d);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["name"], "upload_new_y.eti");
        assert_eq!(list[0]["reason"], "upload");
        assert_eq!(list[1]["name"], "rec4.eti");
        assert_eq!(list[1]["size"], 40);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn written_frames_play_back_at_the_pace_of_the_air() {
        let d = dir("play");
        let meta = Meta {
            time: 1_761_800_000,
            channel: "8C".into(),
            eid: "6D2B".into(),
            ecc: "E2".into(),
            label: "Test".into(),
            reason: "manual".into(),
            ..Default::default()
        };
        let mut w = Writer::start(&d, meta).unwrap();
        assert!(w.path.file_name().unwrap().to_str().unwrap().ends_with("_8C_6D2B.eti"));
        // the JSON beside it exists from the start
        assert_eq!(read_meta(&w.path).unwrap().channel, "8C");
        for n in 0..50u8 {
            w.push(frame(n));
        }
        assert_eq!(w.frames, 50);
        let path = w.path.clone();
        let meta = w.finish("complete");
        assert_eq!(meta.frames, 50);
        assert!((meta.duration - 1.2).abs() < 0.01);
        // (the thread finishes the file by itself)
        let t0 = Instant::now();
        while std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) < 50 * ETI_FRAME as u64 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 50 * ETI_FRAME as u64);
        while read_meta(&path).is_none_or(|m| m.ended.is_empty()) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let m = read_meta(&path).unwrap();
        assert_eq!((m.ended.as_str(), m.frames, m.size), ("complete", 50, 50 * ETI_FRAME as u64));
        let listed = list(&d);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["size"], 50 * ETI_FRAME as u64);
        assert_eq!(listed[0]["label"], "Test");
        // played back: 50 frames take 1.2 s, then the file again
        let (tx, rx) = bounded::<EtiFrame>(200);
        let p = Player::start(&path, tx, None).unwrap();
        assert_eq!(p.frames, 50);
        let t0 = Instant::now();
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first.fic, vec![0u8; 96]);
        assert_eq!(first.streams[0].0, 2);
        let mut n = 1;
        while n < 60 {
            let f = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(f.fic[0] as usize, n % 50);
            n += 1;
        }
        let took = t0.elapsed();
        assert!(took > Duration::from_millis(1300) && took < Duration::from_millis(2500), "{took:?}");
        let (at, total, loops) = p.progress();
        assert!((total - 1.2).abs() < 0.01);
        assert!(loops >= 1 && at < 1.0, "{at} {loops}");
        drop(p);
        // the raw frames parse as the ETI input does
        let bytes = std::fs::read(&path).unwrap();
        assert!(parse_eti(&bytes[ETI_FRAME * 7..ETI_FRAME * 8]).is_some_and(|f| f.fic[0] == 7));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn an_edi_file_plays_back_its_packets() {
        let d = dir("edi");
        let path = d.join("upload_2026-10-30_061512_mux.edi");
        let bytes: Vec<u8> = (0..20u8).flat_map(af).collect();
        std::fs::write(&path, &bytes).unwrap();
        // (its length is not its size: counted when it was loaded)
        assert_eq!(list(&d)[0]["frames"], 0);
        write_meta(&path, &Meta { reason: "upload".into(), frames: 20, ..Default::default() });
        assert_eq!(list(&d)[0]["frames"], 20);
        let (tx, rx) = bounded::<EtiFrame>(50);
        let p = Player::start(&path, tx, None).unwrap();
        assert_eq!(p.frames, 20);
        for n in 0..25 {
            let f = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!((f.fic[0] as usize, f.streams[0].0), (n % 20, 2));
        }
        drop(p);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_spectrum_is_kept_to_a_hundredth_of_a_db() {
        let (bins, points, taps) = (64usize, 12usize, 16usize);
        let h = serde_json::json!({"bins": bins, "points": points, "taps": taps, "offset": 12.5, "null": true});
        let mut values = Vec::new();
        for i in 0..2 * bins {
            values.push(-80.0 + i as f32 * 0.731);
        }
        for i in 0..2 * points {
            values.push(if i % 2 == 0 { 0.6 } else { -1.2345 });
        }
        for i in 0..taps {
            values.push(-200.0 + i as f32 * 3.3);
        }
        let payload: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let packed = pack_spectrum(&h, &payload).unwrap();
        assert!(packed.len() < payload.len(), "{} of {}", packed.len(), payload.len());
        let (h2, back) = unpack_spectrum(&packed).unwrap();
        assert_eq!(h2, h);
        assert_eq!(back.len(), payload.len());
        for (i, (a, b)) in values.iter().zip(back.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))).enumerate() {
            let tol = if (2 * bins..2 * bins + 2 * points).contains(&i) { 1.0 / 8192.0 } else { 0.0051 };
            assert!((a - b).abs() <= tol, "{i}: {a} {b}");
        }
        // a message that does not add up is not kept
        assert!(pack_spectrum(&h, &payload[4..]).is_none());
        assert!(unpack_spectrum(&packed[..packed.len() / 2]).is_none());
    }

    #[test]
    fn what_was_heard_beside_the_frames_plays_with_them_and_after_a_seek() {
        let d = dir("side");
        let meta = Meta { time: 1_761_800_000, channel: "8C".into(), eid: "6D2B".into(), ..Default::default() };
        let mut w = Writer::start(&d, meta).unwrap();
        let tii = |n: u8| serde_json::to_vec(&serde_json::json!({"codes": [[1, n, 0.0, 9.5]], "pairs": []})).unwrap();
        let (bins, points, taps) = (8usize, 2usize, 4usize);
        let h = serde_json::json!({"bins": bins, "points": points, "taps": taps});
        let payload: Vec<u8> = (0..2 * bins + 2 * points + taps).flat_map(|i| (i as f32 * -1.5).to_le_bytes()).collect();
        for n in 0..100u8 {
            match n {
                10 => w.side(SideKind::Tii, tii(1)),
                20 => w.side(SideKind::Signal, serde_json::to_vec(&serde_json::json!({"type": "signal", "snr": 7.5})).unwrap()),
                50 => w.spectrum(h.clone(), payload.clone()),
                80 => w.side(SideKind::Tii, tii(2)),
                _ => {}
            }
            w.push(frame(n));
        }
        let path = w.path.clone();
        w.finish("complete");
        let t0 = Instant::now();
        while read_meta(&path).is_none_or(|m| m.ended.is_empty()) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(side_path(&path).is_file());
        assert_eq!(list(&d)[0]["side"], true);
        // the folder counts it with the recording
        assert!(used(&d) > 100 * ETI_FRAME as u64);
        let (tx, rx) = bounded::<EtiFrame>(4);
        let (stx, srx) = bounded::<Side>(16);
        let p = Player::start(&path, tx, Some(stx)).unwrap();
        assert!(p.side);
        // the records come after the frames they came with
        let mut seen = Vec::new();
        for n in 0..25 {
            let f = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(f.fic[0] as usize, n);
            seen.extend(srx.try_iter().map(|s| (n, s)));
        }
        std::thread::sleep(Duration::from_millis(50));
        seen.extend(srx.try_iter().map(|s| (25, s)));
        assert_eq!(seen.len(), 2, "{seen:?}");
        // (the player is a few frames ahead of the reader: the channel holds 4)
        assert!(matches!(&seen[0].1, Side::Tii(v) if v["codes"][0][1] == 1) && seen[0].0 >= 4, "{seen:?}");
        assert!(matches!(&seen[1].1, Side::Signal(v) if v["snr"] == 7.5) && seen[1].0 >= 14, "{seen:?}");
        // moved to 2.4 s (frame 60): what was shown then at once, the frames from there
        p.seek(60.0 / FRAMES_PER_SECOND);
        let mut n = 0;
        while n != 60 {
            n = rx.recv_timeout(Duration::from_secs(2)).unwrap().fic[0] as usize;
        }
        for want in 61..70 {
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap().fic[0] as usize, want);
        }
        let after: Vec<Side> = srx.try_iter().collect();
        assert_eq!(after.len(), 3, "{after:?}");
        assert!(matches!(&after[0], Side::Tii(v) if v["codes"][0][1] == 1));
        assert!(matches!(&after[1], Side::Spectrum(h2, p2) if *h2 == h && p2.len() == payload.len()));
        assert!(matches!(&after[2], Side::Signal(_)));
        let (at, total, _) = p.progress();
        assert!(at > 1.6 && at < 1.9 && (total - 2.4).abs() < 0.01, "{at} {total}");
        // the second TII after frame 80
        for _ in 70..82 {
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        let late: Vec<Side> = srx.try_iter().collect();
        assert!(matches!(late.as_slice(), [Side::Tii(v)] if v["codes"][0][1] == 2), "{late:?}");
        drop(p);
        // deleted with the recording
        remove(&path).unwrap();
        assert!(!side_path(&path).exists());
        let _ = std::fs::remove_dir_all(d);
    }
}
