//! rtl_tcp client.

use crate::agc::{default_gain_index, next_index, SoftAgc};
use crate::{convert_u8, DeviceStatus, GainMode, InputDevice, SampleCallback, SAMPLE_RATE};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// R820T gain table in tenths of dB, used when the server does not tell us more.
const R820T_GAINS: [i32; 29] = [
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364, 372, 386, 402, 421, 434, 439, 445, 480, 496,
];

struct Shared {
    /// None until connected.
    stream: Mutex<Option<TcpStream>>,
    stop: AtomicBool,
    gain_index: AtomicUsize,
    /// Highest gain index the software AGC may use.
    gain_ceiling: AtomicUsize,
    software_agc: AtomicBool,
    mean_power_bits: AtomicU32,
    connected: AtomicBool,
    stalls: AtomicU32,
    last_stall_ms: AtomicU32,
    /// Received sample rate (S/s) over the last measuring period.
    rate: AtomicU32,
    /// What the server is to be set to, sent (again) once it is connected.
    frequency: AtomicU32,
    ppm: AtomicI32,
    mode: Mutex<GainMode>,
    /// Why the connection failed (made on the reading thread).
    error: Mutex<Option<String>>,
}

impl Shared {
    fn command(&self, cmd: u8, param: u32) {
        let mut buf = [0u8; 5];
        buf[0] = cmd;
        buf[1..].copy_from_slice(&param.to_be_bytes());
        if let Ok(mut s) = self.stream.lock() {
            if let Some(s) = s.as_mut() {
                let _ = s.write_all(&buf);
            }
        }
    }

    fn apply_gain(&self) {
        let mode = self.mode.lock().map(|m| m.clone()).unwrap_or(GainMode::Software);
        match mode {
            GainMode::Hardware => {
                self.software_agc.store(false, Ordering::Relaxed);
                self.command(0x03, 0);
                self.command(0x08, 1);
            }
            GainMode::Software => {
                self.software_agc.store(true, Ordering::Relaxed);
                self.command(0x08, 0);
                self.command(0x03, 1);
                self.set_gain_index(self.gain_index.load(Ordering::Relaxed).min(self.gain_ceiling.load(Ordering::Relaxed)));
            }
            GainMode::Manual { index } => {
                self.software_agc.store(false, Ordering::Relaxed);
                self.command(0x08, 0);
                self.command(0x03, 1);
                self.set_gain_index(index);
            }
        }
    }

    /// The connection is up: the server gets everything set so far.
    fn connected(&self, stream: TcpStream) {
        if let Ok(mut s) = self.stream.lock() {
            *s = Some(stream);
        }
        self.command(0x02, SAMPLE_RATE);
        self.command(0x01, self.frequency.load(Ordering::Relaxed));
        let ppm = self.ppm.load(Ordering::Relaxed);
        if ppm != 0 {
            self.command(0x05, ppm as u32);
        }
        self.apply_gain();
        self.connected.store(true, Ordering::Relaxed);
    }

    fn set_gain_index(&self, i: usize) {
        self.gain_index.store(i, Ordering::Relaxed);
        self.command(0x04, R820T_GAINS[i.min(R820T_GAINS.len() - 1)] as u32);
    }
}

pub struct RtlTcp {
    host: String,
    port: u16,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
    frequency: u32,
    mode: GainMode,
    ppm: i32,
    error: Option<String>,
    ceiling: Option<usize>,
}

impl RtlTcp {
    pub fn new(host: String, port: u16) -> Self {
        RtlTcp { host, port, shared: None, thread: None, frequency: 0, mode: GainMode::Software, ppm: 0, error: None, ceiling: None }
    }

    fn apply_gain(&self) {
        let Some(sh) = &self.shared else { return };
        if let Ok(mut m) = sh.mode.lock() {
            *m = self.mode.clone();
        }
        sh.apply_gain();
    }
}

/// Connect and read the dongle info header: the stream, a second handle for
/// reading, and samples that came instead of the header.
fn connect(host: &str, port: u16) -> Result<(TcpStream, TcpStream, Vec<u8>), String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("rtl_tcp {host}: {e}"))?
        .next()
        .ok_or_else(|| "cannot resolve rtl_tcp host".to_string())?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("rtl_tcp connect failed: {e}"))?;
    let mut header = [0u8; 12];
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.read_exact(&mut header).map_err(|e| format!("rtl_tcp header: {e}"))?;
    // some servers/proxies omit the 12 byte dongle info header and start with samples
    let leading: Vec<u8> = if &header[..4] == b"RTL0" { Vec::new() } else { header.to_vec() };
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    Ok((stream, reader, leading))
}

impl InputDevice for RtlTcp {
    fn status(&self) -> DeviceStatus {
        let gains: Vec<f32> = R820T_GAINS.iter().map(|&g| g as f32 / 10.0).collect();
        let (gi, _mean, connected) = match &self.shared {
            Some(sh) => (
                Some(sh.gain_index.load(Ordering::Relaxed)),
                f32::from_bits(sh.mean_power_bits.load(Ordering::Relaxed)),
                sh.connected.load(Ordering::Relaxed),
            ),
            None => (None, 0.0, false),
        };
        let gain_db = gi.and_then(|i| gains.get(i).copied());
        DeviceStatus {
            kind: "rtltcp".into(),
            // not the server address: every listener sees the status, and rtl_tcp has no password
            name: "rtl_tcp".into(),
            connected,
            frequency: self.frequency,
            gain_db: if matches!(self.mode, GainMode::Hardware) { None } else { gain_db },
            gain_index: gi,
            gains,
            // no calibrated RF level estimate for this driver
            rf_level: None,
            error: self.error.clone().or_else(|| self.shared.as_ref().and_then(|sh| sh.error.lock().ok().and_then(|e| e.clone()))),
            position: None,
            duration: None,
            stalls: self.shared.as_ref().map(|sh| sh.stalls.load(Ordering::Relaxed)).unwrap_or(0),
            last_stall_ms: self.shared.as_ref().map(|sh| sh.last_stall_ms.load(Ordering::Relaxed)).filter(|&v| v > 0),
            stream_rate: self.shared.as_ref().map(|sh| sh.rate.load(Ordering::Relaxed) as f32 / 1000.0).filter(|&v| v > 0.0),
        }
    }

    fn start(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        self.stop();
        // rtl_tcp on this machine answers (or refuses) at once, and a refusal is
        // retried soon (a driver app still starting); a name lookup or a server
        // far away can take seconds that must not hold up the radio thread
        let now = if crate::local_host(&self.host) {
            Some(connect(&self.host, self.port).inspect_err(|e| self.error = Some(e.clone()))?)
        } else {
            None
        };
        let start_index = default_gain_index(&R820T_GAINS);
        let ceiling = self.ceiling.unwrap_or(start_index);
        let shared = Arc::new(Shared {
            stream: Mutex::new(None),
            stop: AtomicBool::new(false),
            gain_index: AtomicUsize::new(start_index.min(ceiling)),
            gain_ceiling: AtomicUsize::new(ceiling),
            software_agc: AtomicBool::new(true),
            mean_power_bits: AtomicU32::new(0),
            connected: AtomicBool::new(false),
            stalls: AtomicU32::new(0),
            last_stall_ms: AtomicU32::new(0),
            rate: AtomicU32::new(0),
            frequency: AtomicU32::new(frequency),
            ppm: AtomicI32::new(self.ppm),
            mode: Mutex::new(self.mode.clone()),
            error: Mutex::new(None),
        });
        self.shared = Some(shared.clone());
        self.frequency = frequency;
        let (host, port) = (self.host.clone(), self.port);
        let sh = shared;
        self.thread = Some(
            std::thread::Builder::new()
                .name("rtltcp".into())
                .spawn(move || {
                    let (reader, leading) = match now.map_or_else(|| connect(&host, port), Ok) {
                        // (stopped while connecting)
                        Ok(_) if sh.stop.load(Ordering::Relaxed) => return,
                        Ok((stream, reader, leading)) => {
                            sh.connected(stream);
                            (reader, leading)
                        }
                        Err(e) => {
                            log::warn!("{e}");
                            if let Ok(mut err) = sh.error.lock() {
                                *err = Some(e);
                            }
                            return;
                        }
                    };
                    let mut reader = reader;
                    reader.set_read_timeout(Some(Duration::from_millis(500))).ok();
                    let mut buf = vec![0u8; 32768];
                    let mut samples = Vec::new();
                    let mut agc = SoftAgc::new();
                    let mut odd: Option<u8> = None;
                    if !leading.is_empty() {
                        let mut s = Vec::new();
                        convert_u8(&leading, &mut s);
                        callback(&s);
                    }
                    let mut last_data = std::time::Instant::now();
                    let mut period = (std::time::Instant::now(), 0u64);
                    let mut last_log: Option<std::time::Instant> = None;
                    while !sh.stop.load(Ordering::Relaxed) {
                        let n = match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => continue,
                            Err(_) => break,
                        };
                        // interruptions of the stream (network, server) and the real rate
                        let gap = last_data.elapsed();
                        last_data = std::time::Instant::now();
                        if gap > Duration::from_millis(300) {
                            sh.stalls.fetch_add(1, Ordering::Relaxed);
                            sh.last_stall_ms.store(gap.as_millis() as u32, Ordering::Relaxed);
                            if last_log.map(|t| t.elapsed() > Duration::from_secs(10)).unwrap_or(true) {
                                last_log = Some(std::time::Instant::now());
                                log::warn!("rtl_tcp: no samples for {} ms", gap.as_millis());
                            }
                        }
                        period.1 += n as u64;
                        let el = period.0.elapsed();
                        if el >= Duration::from_secs(5) {
                            let rate = (period.1 as f64 / 2.0 / el.as_secs_f64()) as u32;
                            sh.rate.store(rate, Ordering::Relaxed);
                            if rate < SAMPLE_RATE * 95 / 100 && last_log.map(|t| t.elapsed() > Duration::from_secs(10)).unwrap_or(true) {
                                last_log = Some(std::time::Instant::now());
                                log::warn!("rtl_tcp: stream delivers {} kS/s (expected {})", rate / 1000, SAMPLE_RATE / 1000);
                            }
                            period = (std::time::Instant::now(), 0);
                        }
                        // keep I/Q pairs aligned across reads
                        let mut data: Vec<u8> = Vec::with_capacity(n + 1);
                        if let Some(b) = odd.take() {
                            data.push(b);
                        }
                        data.extend_from_slice(&buf[..n]);
                        if data.len() % 2 == 1 {
                            odd = data.pop();
                        }
                        convert_u8(&data, &mut samples);
                        callback(&samples);
                        agc.feed(&data);
                        let i = sh.gain_index.load(Ordering::Relaxed);
                        let up_ratio = match (R820T_GAINS.get(i), R820T_GAINS.get(i + 1)) {
                            (Some(&a), Some(&b)) => 10f32.powf((b - a) as f32 / 100.0),
                            _ => 1.0,
                        };
                        let action = agc.evaluate(up_ratio);
                        sh.mean_power_bits.store(agc.mean_power.to_bits(), Ordering::Relaxed);
                        if sh.software_agc.load(Ordering::Relaxed) {
                            let ceiling = sh.gain_ceiling.load(Ordering::Relaxed);
                            if let Some(n) = next_index(action, i, ceiling, R820T_GAINS.len()) {
                                sh.set_gain_index(n);
                            }
                        }
                    }
                    sh.connected.store(false, Ordering::Relaxed);
                })
                .map_err(|e| e.to_string())?,
        );
        self.error = None;
        Ok(())
    }

    fn tune(&mut self, frequency: u32) -> Result<(), String> {
        let Some(sh) = &self.shared else { return Err("not connected".into()) };
        sh.command(0x01, frequency);
        self.frequency = frequency;
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(sh) = self.shared.take() {
            sh.stop.store(true, Ordering::Relaxed);
            let connected = match sh.stream.lock() {
                Ok(s) => s.as_ref().map(|s| s.shutdown(Shutdown::Both)).is_some(),
                Err(_) => false,
            };
            // a thread still connecting (up to 10 s) ends by itself; one reading
            // stops within its read timeout
            if let Some(t) = self.thread.take() {
                if connected {
                    let _ = t.join();
                }
            }
        }
    }

    fn set_gain_mode(&mut self, mode: GainMode) {
        self.mode = mode;
        self.apply_gain();
    }

    fn set_gain_ceiling(&mut self, index: Option<usize>, jump: bool) {
        self.ceiling = index;
        let Some(sh) = &self.shared else { return };
        let c = index.unwrap_or_else(|| default_gain_index(&R820T_GAINS)).min(R820T_GAINS.len() - 1);
        sh.gain_ceiling.store(c, Ordering::Relaxed);
        let i = sh.gain_index.load(Ordering::Relaxed);
        // up to a higher ceiling the AGC climbs as far as the signal level allows
        if sh.software_agc.load(Ordering::Relaxed) && (i > c || (jump && i != c)) {
            sh.set_gain_index(c);
        }
    }

    fn set_ppm(&mut self, ppm: i32) {
        self.ppm = ppm;
        if let Some(sh) = &self.shared {
            sh.command(0x05, ppm as u32);
        }
    }
}

impl Drop for RtlTcp {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn counting() -> (SampleCallback, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        (
            Arc::new(move |s: &[num_complex::Complex32]| {
                c.fetch_add(s.len(), Ordering::Relaxed);
            }),
            n,
        )
    }

    #[test]
    fn a_server_far_away_does_not_hold_up_the_start() {
        // (a documentation address: nothing answers there)
        let mut tcp = RtlTcp::new("192.0.2.1".into(), 1234);
        let t = Instant::now();
        tcp.start(229_072_000, counting().0).unwrap();
        assert!(!tcp.status().connected);
        tcp.tune(227_360_000).unwrap();
        tcp.stop();
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    }

    #[test]
    fn a_local_server_is_set_up_and_streams() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            s.write_all(b"RTL0\0\0\0\x05\0\0\0\x1d").unwrap();
            s.write_all(&[128u8; 4096]).unwrap();
            // sample rate first, then the frequency
            let mut cmd = [0u8; 10];
            s.read_exact(&mut cmd).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            cmd
        });
        let (cb, samples) = counting();
        let mut tcp = RtlTcp::new("127.0.0.1".into(), port);
        tcp.start(229_072_000, cb).unwrap();
        let cmd = peer.join().unwrap();
        assert_eq!(cmd[0], 0x02);
        assert_eq!(u32::from_be_bytes(cmd[1..5].try_into().unwrap()), SAMPLE_RATE);
        assert_eq!(cmd[5], 0x01);
        assert_eq!(u32::from_be_bytes(cmd[6..10].try_into().unwrap()), 229_072_000);
        assert!(samples.load(Ordering::Relaxed) >= 2048);
        tcp.stop();
    }

    #[test]
    fn a_local_server_not_running_is_an_error_at_once() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut tcp = RtlTcp::new("127.0.0.1".into(), port);
        let t = Instant::now();
        assert!(tcp.start(229_072_000, counting().0).is_err());
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(tcp.status().error.is_some());
    }
}
