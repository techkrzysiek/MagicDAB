//! Logger writing to stderr and keeping recent lines for the web UI log view.
//!
//! stderr is written by a thread of its own: a full pipe (a slow log driver)
//! must not stop the radio or the receiver thread in the middle of a log line.

use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct Logger {
    level: log::LevelFilter,
}

static LINES: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
static CHANNEL: OnceLock<broadcast::Sender<LogLine>> = OnceLock::new();
const MAX_LINES: usize = 1000;
/// Lines for stderr; when the writer falls behind, lines are dropped (and counted).
static STDERR: OnceLock<SyncSender<String>> = OnceLock::new();
static QUEUED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// The log also goes into this file (the Windows application has no console).
static FILE: OnceLock<std::path::PathBuf> = OnceLock::new();
/// Size of the log file before it is moved to `.1` (the one before is dropped).
const FILE_MAX: u64 = 2 << 20;

/// Write the log into `path` too; before `init`.
pub fn to_file(path: std::path::PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = FILE.set(path);
}

fn open_file() -> Option<std::fs::File> {
    let path = FILE.get()?;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > FILE_MAX) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level && !metadata.target().starts_with("hyper") && !metadata.target().starts_with("tower")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            time: crate::localtime::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            level: record.level().to_string(),
            target: record.target().split("::").last().unwrap_or("").to_string(),
            message: record.args().to_string(),
        };
        let text = format!("{} {:5} [{}] {}", line.time, line.level, line.target, line.message);
        match STDERR.get() {
            Some(tx) => {
                if tx.try_send(text).is_ok() {
                    QUEUED.fetch_add(1, Ordering::Relaxed);
                } else {
                    DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
            None => eprintln!("{text}"),
        }
        let mut lines = LINES.lock();
        if lines.len() >= MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(line.clone());
        drop(lines);
        if let Some(tx) = CHANNEL.get() {
            let _ = tx.send(line);
        }
    }

    fn flush(&self) {}
}

/// The end of the log file of the previous run as log lines: the Log view
/// shows what happened before a restart (e.g. one that did not come back).
fn previous_run() -> Vec<LogLine> {
    let Some(text) = FILE.get().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    let tail: Vec<&str> = text.lines().rev().take(300).collect();
    let mut lines: Vec<LogLine> = tail.into_iter().rev().filter_map(parse_line).collect();
    if !lines.is_empty() {
        lines.push(LogLine {
            time: String::new(),
            level: "INFO".into(),
            target: "log".into(),
            message: "---- the lines above are from the previous run ----".into(),
        });
    }
    lines
}

/// "2026-10-30 04:08:40 INFO  [tunnel] text" (as written to the file).
fn parse_line(l: &str) -> Option<LogLine> {
    let time = l.get(..19)?.to_string();
    let rest = l.get(19..)?.trim_start();
    let (level, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (target, message) = rest.strip_prefix('[')?.split_once("] ")?;
    Some(LogLine { time, level: level.to_string(), target: target.to_string(), message: message.to_string() })
}

pub fn init(level: log::LevelFilter) {
    let _ = CHANNEL.set(broadcast::channel(256).0);
    LINES.lock().extend(previous_run());
    let (tx, rx) = sync_channel::<String>(4096);
    let started = std::thread::Builder::new().name("log".into()).spawn(move || {
        let mut err = std::io::stderr();
        let mut file = open_file();
        let mut lines = 0u32;
        for text in rx {
            let dropped = DROPPED.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                let _ = writeln!(err, "({dropped} log lines dropped: stderr too slow)");
            }
            let _ = writeln!(err, "{text}");
            if let Some(f) = file.as_mut() {
                let _ = writeln!(f, "{text}");
                // now and then: a file grown too big starts anew
                lines += 1;
                if lines % 500 == 0 && f.metadata().is_ok_and(|m| m.len() > FILE_MAX) {
                    file = open_file();
                }
            }
            WRITTEN.fetch_add(1, Ordering::Relaxed);
        }
    });
    if started.is_ok() {
        let _ = STDERR.set(tx);
    }
    let logger: &'static Logger = Box::leak(Box::new(Logger { level }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}

/// Wait (a second at most) until the queued lines are on stderr: before the
/// program exits or restarts.
pub fn flush() {
    let deadline = Instant::now() + Duration::from_secs(1);
    while WRITTEN.load(Ordering::Relaxed) < QUEUED.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn recent() -> Vec<LogLine> {
    LINES.lock().iter().cloned().collect()
}

pub fn subscribe() -> broadcast::Receiver<LogLine> {
    CHANNEL.get().expect("logger initialised").subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_of_the_file_are_read_back() {
        let l = parse_line("2026-10-30 04:08:40 INFO  [tunnel] tunnel: online at https://x").unwrap();
        assert_eq!(
            (l.time.as_str(), l.level.as_str(), l.target.as_str(), l.message.as_str()),
            ("2026-10-30 04:08:40", "INFO", "tunnel", "tunnel: online at https://x")
        );
        assert!(parse_line("(3 log lines dropped: stderr too slow)").is_none());
    }
}
