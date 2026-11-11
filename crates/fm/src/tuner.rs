//! FM-DX-Tuner (e.g. the TEF668x headless USB tuner): XDR-GTK compatible
//! serial protocol. Commands end with a newline: `x` start, `T<kHz>` tune,
//! `Sa<kHz>` / `Sb<kHz>` / `Sc<kHz>` scan range and step, `S` scan.
//! Reports: `S<s|m|S|M><level>,<cci>,<aci>[,<bw>]` (level in dBf, s/S: pilot),
//! `P<pi>` with one `?` per error level, `R<B><C><D><errors>` (block A is the
//! PI; error levels of B, C, D in bits 0-1, 2-3, 4-5), `U<kHz>=<level>,...`
//! scan results, `T<kHz>` after tuning and `Z<n>` after switching the
//! antenna (`Z<n>` selects one, if the tuner has an antenna switch).

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    /// Stereo pilot present.
    pub stereo: bool,
    /// dBf
    pub level: f32,
    /// Co-channel interference (0-100).
    pub cci: i32,
    /// Adjacent channel interference (-1: not reported).
    pub aci: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TunerEvent {
    Connected(String),
    Disconnected,
    Tuned(u32),
    Antenna(u8),
    Signal(Signal),
    Pi {
        pi: u16,
        errors: u8,
    },
    /// One RDS group: blocks B, C, D and their error levels (0-3).
    Group {
        blocks: [u16; 3],
        errors: [u8; 3],
    },
    Scan(Vec<(u32, f32)>),
}

pub struct FmTuner {
    commands: Sender<String>,
}

impl FmTuner {
    /// Use the tuner on `port` ("" finds it) and keep it open.
    pub fn start(port: String, events: Sender<TunerEvent>) -> FmTuner {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("fm-tuner".into()).spawn(move || run(port, rx, events)).expect("fm tuner thread");
        FmTuner { commands: tx }
    }

    pub fn tune(&self, khz: u32) {
        let _ = self.commands.send(format!("T{khz}"));
    }

    pub fn antenna(&self, n: u8) {
        let _ = self.commands.send(format!("Z{n}"));
    }

    /// Signal levels over a range (the audio is muted meanwhile).
    pub fn scan(&self, from: u32, to: u32, step: u32) {
        for c in [format!("Sa{from}"), format!("Sb{to}"), format!("Sc{step}"), "S".to_string()] {
            let _ = self.commands.send(c);
        }
    }
}

/// Serial ports a tuner may be on (Linux: by id first; Windows: COM ports).
pub fn list_ports() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["/dev/serial/by-id", "/dev/hostdev/serial/by-id"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut v: Vec<String> = rd.flatten().map(|e| e.path().display().to_string()).collect();
            v.sort();
            out.extend(v);
        }
    }
    for dir in ["/dev", "/dev/hostdev"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut v: Vec<String> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("ttyACM") || n.starts_with("ttyUSB"))
                .map(|n| format!("{dir}/{n}"))
                .collect();
            v.sort();
            out.extend(v);
        }
    }
    #[cfg(windows)]
    out.extend(win::com_ports());
    out
}

/// The serial port of a TEF668x headless tuner.
#[cfg(windows)]
fn find_port() -> Option<PathBuf> {
    // no device names without SetupAPI: ask every free COM port
    win::com_ports().into_iter().find(|p| win::is_tuner(p)).map(PathBuf::from)
}

/// The serial port of a TEF668x headless tuner.
#[cfg(not(windows))]
fn find_port() -> Option<PathBuf> {
    // /dev/hostdev: the host's /dev in a container (re-plugging keeps working)
    for dir in ["/dev/serial/by-id", "/dev/hostdev/serial/by-id"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.contains("TEF668X") || name.contains("FMDX") {
                    return Some(e.path());
                }
            }
        }
    }
    ["/dev/ttyACM0", "/dev/hostdev/ttyACM0"].iter().map(PathBuf::from).find(|p| p.exists())
}

#[cfg(unix)]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open(path)?;
    // raw mode; reads return after 100 ms without data
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(f.as_raw_fd(), &mut t) == 0 {
            libc::cfmakeraw(&mut t);
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 1;
            libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t);
        }
    }
    Ok(f)
}

#[cfg(windows)]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    win::open(&path.to_string_lossy())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io::{Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct CommTimeouts {
        read_interval: u32,
        read_multiplier: u32,
        read_constant: u32,
        write_multiplier: u32,
        write_constant: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetCommTimeouts(h: *mut c_void, t: *const CommTimeouts) -> i32;
        fn EscapeCommFunction(h: *mut c_void, func: u32) -> i32;
        fn QueryDosDeviceW(name: *const u16, target: *mut u16, max: u32) -> u32;
    }

    const SETRTS: u32 = 3;
    const SETDTR: u32 = 5;

    /// A COM port: reads return after 100 ms without data, DTR and RTS on
    /// (the tuner sends audio only while its port is open).
    pub fn open(name: &str) -> std::io::Result<std::fs::File> {
        let dev = if name.starts_with(r"\\") { name.to_string() } else { format!(r"\\.\{name}") };
        let f = std::fs::OpenOptions::new().read(true).write(true).open(dev)?;
        let h: *mut c_void = f.as_raw_handle();
        let t = CommTimeouts { read_interval: u32::MAX, read_multiplier: 0, read_constant: 100, write_multiplier: 0, write_constant: 1000 };
        unsafe {
            SetCommTimeouts(h, &t);
            EscapeCommFunction(h, SETDTR);
            EscapeCommFunction(h, SETRTS);
        }
        Ok(f)
    }

    /// COM ports of the system (in use or not).
    pub fn com_ports() -> Vec<String> {
        let mut buf = vec![0u16; 65536];
        let n = unsafe { QueryDosDeviceW(std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        let mut ports: Vec<String> = buf[..n.min(buf.len())]
            .split(|&c| c == 0)
            .map(String::from_utf16_lossy)
            .filter(|s| s.starts_with("COM") && s[3..].chars().all(|c| c.is_ascii_digit()) && s.len() > 3)
            .collect();
        ports.sort_by_key(|p| p[3..].parse::<u32>().unwrap_or(0));
        ports.dedup();
        ports
    }

    /// Whether an FM-DX-Tuner answers on the port (signal reports after `x`).
    pub fn is_tuner(name: &str) -> bool {
        let Ok(mut f) = open(name) else { return false };
        if f.write_all(b"x\n").is_err() {
            return false;
        }
        let t = Instant::now();
        let mut text = Vec::new();
        let mut buf = [0u8; 256];
        while t.elapsed() < Duration::from_millis(1500) {
            match f.read(&mut buf) {
                Ok(n) => text.extend_from_slice(&buf[..n]),
                Err(_) => return false,
            }
            let s = String::from_utf8_lossy(&text);
            if s.lines().any(|l| l.len() > 2 && l.starts_with('S') && matches!(l.as_bytes()[1], b's' | b'm' | b'S' | b'M')) {
                log::info!("FM tuner found on {name}");
                return true;
            }
        }
        false
    }
}

#[cfg(not(any(unix, windows)))]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).write(true).open(path)
}

fn run(port: String, commands: Receiver<String>, events: Sender<TunerEvent>) {
    let mut warned = false;
    let mut frequency: Option<u32> = None;
    let mut antenna: Option<u8> = None;
    loop {
        let path = if port.is_empty() { find_port() } else { Some(PathBuf::from(&port)) };
        let file = path.as_ref().map(|p| (p.clone(), open(p)));
        let (path, mut f) = match file {
            Some((p, Ok(f))) => (p, f),
            other => {
                if !warned {
                    match other {
                        Some((p, Err(e))) => log::warn!("FM tuner {}: {e}", p.display()),
                        _ => log::debug!("FM tuner: none found"),
                    }
                    warned = true;
                }
                // commands for a missing tuner are pointless later
                loop {
                    match commands.try_recv() {
                        Ok(c) => {
                            if let Some(k) = c.strip_prefix('T').and_then(|k| k.parse().ok()) {
                                frequency = Some(k);
                            }
                            if let Some(a) = c.strip_prefix('Z').and_then(|a| a.parse().ok()) {
                                antenna = Some(a);
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        warned = false;
        log::info!("FM tuner on {}", path.display());
        let _ = events.send(TunerEvent::Connected(path.display().to_string()));
        let mut ok = f.write_all(b"x\n").is_ok();
        if let Some(a) = antenna {
            ok &= f.write_all(format!("Z{a}\n").as_bytes()).is_ok();
        }
        if let Some(k) = frequency {
            ok &= f.write_all(format!("T{k}\n").as_bytes()).is_ok();
        }
        let mut line = Vec::new();
        let mut buf = [0u8; 1024];
        while ok {
            // (a disconnected channel means FM was switched off; checking that
            // with a separate try_recv used to swallow the command waiting)
            loop {
                match commands.try_recv() {
                    Ok(c) => {
                        if let Some(k) = c.strip_prefix('T').and_then(|k| k.parse().ok()) {
                            frequency = Some(k);
                        }
                        if let Some(a) = c.strip_prefix('Z').and_then(|a| a.parse().ok()) {
                            antenna = Some(a);
                        }
                        if f.write_all(format!("{c}\n").as_bytes()).is_err() {
                            ok = false;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        // FM switched off: the tuner too
                        let _ = f.write_all(b"X\n");
                        log::info!("FM tuner switched off");
                        let _ = events.send(TunerEvent::Disconnected);
                        return;
                    }
                }
            }
            match f.read(&mut buf) {
                // the tuner reports its signal every 66 ms: nothing for long means it is gone
                Ok(0) => {}
                Ok(n) => {
                    for &b in &buf[..n] {
                        if b == b'\n' {
                            if let Some(e) = parse(&String::from_utf8_lossy(&line)) {
                                let _ = events.send(e);
                            }
                            line.clear();
                        } else if line.len() < 16384 {
                            line.push(b);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => ok = false,
            }
        }
        log::warn!("FM tuner on {} lost", path.display());
        let _ = events.send(TunerEvent::Disconnected);
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn parse(line: &str) -> Option<TunerEvent> {
    let line = line.trim();
    let mut chars = line.chars();
    match chars.next()? {
        'S' => {
            let pilot = match chars.next()? {
                's' | 'S' => true,
                'm' | 'M' => false,
                _ => return None,
            };
            let mut it = chars.as_str().split(',');
            let level = it.next()?.parse().ok()?;
            let cci = it.next().and_then(|v| v.parse().ok()).unwrap_or(-1);
            let aci = it.next().and_then(|v| v.parse().ok()).unwrap_or(-1);
            Some(TunerEvent::Signal(Signal { stereo: pilot, level, cci, aci }))
        }
        'P' => {
            let rest = chars.as_str();
            let pi = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
            Some(TunerEvent::Pi { pi, errors: rest[4..].chars().filter(|&c| c == '?').count().min(3) as u8 })
        }
        'R' => {
            let h = chars.as_str();
            if h.len() < 14 {
                return None;
            }
            // (get: noise on the line may put multi-byte characters anywhere)
            let word = |i: usize| h.get(i..i + 4).and_then(|s| u16::from_str_radix(s, 16).ok());
            let e = u8::from_str_radix(h.get(12..14)?, 16).ok()?;
            Some(TunerEvent::Group { blocks: [word(0)?, word(4)?, word(8)?], errors: [e & 3, (e >> 2) & 3, (e >> 4) & 3] })
        }
        'U' => {
            let points: Vec<(u32, f32)> = chars
                .as_str()
                .split(',')
                .filter_map(|p| p.split_once('='))
                .filter_map(|(f, l)| Some((f.parse().ok()?, l.parse().ok()?)))
                .collect();
            (!points.is_empty()).then_some(TunerEvent::Scan(points))
        }
        'T' => chars.as_str().split(',').next()?.parse().ok().map(TunerEvent::Tuned),
        'Z' => chars.as_str().trim().parse().ok().map(TunerEvent::Antenna),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(parse("Ss94.07,0,-1"), Some(TunerEvent::Signal(Signal { stereo: true, level: 94.07, cci: 0, aci: -1 })));
        assert_eq!(parse("Mm17.2,100,-1,0"), None);
        assert_eq!(parse("Sm17.2,100,-1,0"), Some(TunerEvent::Signal(Signal { stereo: false, level: 17.2, cci: 100, aci: -1 })));
        assert_eq!(parse("P5A2C"), Some(TunerEvent::Pi { pi: 0x5A2C, errors: 0 }));
        assert_eq!(parse("P5A2C??"), Some(TunerEvent::Pi { pi: 0x5A2C, errors: 2 }));
        assert_eq!(parse("R056FC1C2202134"), Some(TunerEvent::Group { blocks: [0x056F, 0xC1C2, 0x2021], errors: [0, 1, 3] }));
        assert_eq!(parse("U87500=12.5,87600=30.25"), Some(TunerEvent::Scan(vec![(87500, 12.5), (87600, 30.25)])));
        assert_eq!(parse("T105000,100"), Some(TunerEvent::Tuned(105000)));
        assert_eq!(parse("Z2"), Some(TunerEvent::Antenna(2)));
    }

    #[test]
    fn noise_on_the_line() {
        let mut x = 0x1234_5678_9abc_def0u64;
        for _ in 0..200_000 {
            let mut b = Vec::new();
            for _ in 0..(x % 24) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b.push(x as u8);
            }
            // a valid start now and then, noise after it
            if x % 3 == 0 {
                b.splice(0..0, *b"R");
            }
            let _ = parse(&String::from_utf8_lossy(&b));
        }
        assert_eq!(parse("R0\u{FFFD}6FC1C2202134xx"), None);
    }
}
