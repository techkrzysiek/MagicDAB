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
/// Longest line kept: a line may carry what a listener's page sent (its
/// errors, why it reconnected), and 1000 of them stay in memory.
const MAX_LINE: usize = 4096;

/// `text` cut to MAX_LINE bytes (at a character).
fn capped(mut text: String) -> String {
    if text.len() > MAX_LINE {
        let mut end = MAX_LINE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" ...");
    }
    text
}

/// One entry of the log stays one: control characters become spaces, and a
/// new line stays only when what follows is indented (a continuation, like
/// the stack of a page's error). Nothing a listener sends (a chat message,
/// why a page closed, a label from the air) reads as a line of its own.
fn one_entry(text: String) -> String {
    if !text.chars().any(|c| c.is_control()) {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' && chars.peek() == Some(&' ') {
            out.push('\n');
        } else if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

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
    let mut open = std::fs::OpenOptions::new();
    open.create(true).append(true);
    // (only its user reads it: the password made at the first start is in it,
    // and the addresses of the listeners)
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        open.mode(0o600);
        // (files an older version left readable by everyone)
        for old in [path.clone(), path.with_extension("log.1")] {
            if std::fs::metadata(&old).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                let _ = std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    open.open(path).ok()
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
            message: capped(one_entry(record.args().to_string())),
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

    #[test]
    fn an_entry_cannot_make_a_line_of_its_own() {
        let forged = one_entry("chat: hi\n2026-11-22 23:59:59 INFO  [login] client 7 logged in as admin\r".to_string());
        assert!(!forged.contains('\n') && !forged.contains('\r'));
        // (a stack stays readable, its lines indented)
        assert_eq!(one_entry("UI error: x\n    at f (a.js:1)".to_string()), "UI error: x\n    at f (a.js:1)");
    }

    #[test]
    fn a_long_line_is_cut() {
        assert_eq!(capped("short".into()), "short");
        let cut = capped("é".repeat(MAX_LINE));
        assert!(cut.len() <= MAX_LINE + 4 && cut.ends_with(" ..."));
        assert!(cut.trim_end_matches(" ...").chars().all(|c| c == 'é'));
    }
}
