//! DX recordings: a multiplex heard on a channel for the first time is
//! recorded as ETI (the receiver gives the frames: receiver.rs,
//! RxCommand::RecordEti) into data/dxrec, a JSON file beside each saying
//! what it is. The admin plays one back later on a side receiver of its own
//! (radio/slots.rs), downloads or deletes it, and may load an .eti file of
//! its own, or an .edi one (an EDI stream over TCP as saved, e.g.
//! `nc mux 9201 > mux.edi`), for a day. radio/dxrec.rs decides when to
//! record.

use crossbeam_channel::{bounded, Sender};
use magicdab_dab::eti::{StreamParser, ETI_FRAME};
use magicdab_dab::receiver::EtiFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
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

/// Bytes of the recordings.
pub fn used(dir: &Path) -> u64 {
    files(dir).iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
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
    let mut all: Vec<(i64, u64, PathBuf)> = files(dir)
        .into_iter()
        .filter(|p| keep != Some(p.as_path()))
        .filter_map(|p| Some((time_of(&p), std::fs::metadata(&p).ok()?.len(), p)))
        .collect();
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

/// Deletes a recording and the JSON beside it.
pub fn remove(path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sidecar(path));
    std::fs::remove_file(path)
}

// ---------------------------------------------------------------- writing

enum Msg {
    Frame(Vec<u8>),
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
            while let Ok(m) = rx.recv() {
                match m {
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
/// the pace of the air, in a loop, on a thread of its own.
pub struct Player {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub name: String,
    /// Frames of the file (an .eti: its size, padded frames; an .edi: as
    /// counted when it was loaded).
    pub frames: u64,
    /// Frames played of this pass, and passes done.
    position: Arc<AtomicU64>,
    loops: Arc<AtomicU64>,
}

impl Player {
    pub fn start(path: &Path, tx: Sender<EtiFrame>) -> std::io::Result<Player> {
        let len = std::fs::metadata(path)?.len();
        let edi = is_edi(path);
        let frames = if edi { read_meta(path).map_or(0, |m| m.frames) } else { len / ETI_FRAME as u64 };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stop = Arc::new(AtomicBool::new(false));
        let position = Arc::new(AtomicU64::new(0));
        let loops = Arc::new(AtomicU64::new(0));
        let (st, pos, lp, p) = (stop.clone(), position.clone(), loops.clone(), path.to_path_buf());
        let thread = std::thread::Builder::new().name("eti-file".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let Ok(mut f) = File::open(&p) else { return };
                let mut parser = StreamParser::new(edi);
                let mut pending: std::collections::VecDeque<EtiFrame> = Default::default();
                let start = Instant::now();
                let mut n = 0u64;
                pos.store(0, Ordering::Relaxed);
                loop {
                    if st.load(Ordering::Relaxed) {
                        return;
                    }
                    let read = match f.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(r) => r,
                    };
                    parser.push(&buf[..read], |fr| pending.push_back(fr));
                    while let Some(fr) = pending.pop_front() {
                        if st.load(Ordering::Relaxed) {
                            return;
                        }
                        // frame n is due 24 ms after the one before
                        let due = Duration::from_secs_f64(n as f64 / FRAMES_PER_SECOND);
                        let el = start.elapsed();
                        if due > el {
                            std::thread::sleep(due - el);
                        }
                        let _ = tx.send_timeout(fr, Duration::from_millis(200));
                        n += 1;
                        pos.store(n, Ordering::Relaxed);
                    }
                }
                if n == 0 {
                    log::warn!("{}: no {} in the file", p.display(), if edi { "EDI packets" } else { "ETI frames" });
                    return;
                }
                lp.fetch_add(1, Ordering::Relaxed);
            }
        })?;
        Ok(Player { stop, thread: Some(thread), name, frames, position, loops })
    }

    /// Seconds into the file, seconds of the file, passes done.
    pub fn progress(&self) -> (f64, f64, u64) {
        (
            self.position.load(Ordering::Relaxed) as f64 / FRAMES_PER_SECOND,
            self.frames as f64 / FRAMES_PER_SECOND,
            self.loops.load(Ordering::Relaxed),
        )
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
        let p = Player::start(&path, tx).unwrap();
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
        let p = Player::start(&path, tx).unwrap();
        assert_eq!(p.frames, 20);
        for n in 0..25 {
            let f = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!((f.fic[0] as usize, f.streams[0].0), (n % 20, 2));
        }
        drop(p);
        let _ = std::fs::remove_dir_all(d);
    }
}
