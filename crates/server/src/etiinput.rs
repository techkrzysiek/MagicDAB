//! ETI / EDI input over TCP (e.g. the EDI or ETI TCP outputs of ODR-DabMux):
//! channels can be mapped to such streams instead of the tuner.

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use magicdab_dab::eti::StreamParser;
use magicdab_dab::receiver::EtiFrame;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub edi: bool,
    pub host: String,
    pub port: u16,
}

impl Source {
    /// `edi+tcp://host:port` (also `tcp://` and `edi://`) or `eti+tcp://host:port`.
    pub fn parse(url: &str) -> Option<Source> {
        let (scheme, rest) = url.trim().split_once("://")?;
        let edi = match scheme.to_ascii_lowercase().as_str() {
            "edi+tcp" | "edi" | "tcp" => true,
            "eti+tcp" | "eti" => false,
            _ => return None,
        };
        let (host, port) = rest.trim_end_matches('/').rsplit_once(':')?;
        Some(Source { edi, host: host.trim_matches(['[', ']']).to_string(), port: port.parse().ok()? })
    }
}

pub struct EtiInput {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub connected: Arc<AtomicBool>,
    /// Bytes received (statistics).
    pub bytes: Arc<AtomicU64>,
}

impl EtiInput {
    /// `delay`: the jitter buffer (as ODR-DabMod's): frames reach the
    /// receiver one CIF (24 ms) apart once that much is buffered, however
    /// unevenly TCP delivers them; zero passes them on as they come.
    pub fn start(src: Source, tx: Sender<EtiFrame>, delay: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let tx = if delay.is_zero() {
            tx
        } else {
            let (qtx, qrx) = crossbeam_channel::unbounded();
            let st = stop.clone();
            let kind = if src.edi { "EDI" } else { "ETI" };
            std::thread::Builder::new().name("eti-pacer".into()).spawn(move || pace(qrx, tx, delay, &st, kind)).expect("eti pacer thread");
            qtx
        };
        let connected = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let (st, co, by) = (stop.clone(), connected.clone(), bytes.clone());
        let thread = std::thread::Builder::new()
            .name("eti-input".into())
            .spawn(move || {
                let kind = if src.edi { "EDI" } else { "ETI" };
                while !st.load(Ordering::Relaxed) {
                    let addr = (src.host.as_str(), src.port).to_socket_addrs().ok().and_then(|mut a| a.next());
                    let stream = addr.and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).ok());
                    let Some(mut stream) = stream else {
                        log::warn!("{kind} input {}:{}: cannot connect", src.host, src.port);
                        for _ in 0..30 {
                            if st.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        continue;
                    };
                    log::info!("{kind} input connected to {}:{}", src.host, src.port);
                    co.store(true, Ordering::Relaxed);
                    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
                    let mut parser = StreamParser::new(src.edi);
                    let mut buf = vec![0u8; 65536];
                    let mut idle = 0;
                    while !st.load(Ordering::Relaxed) {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                idle = 0;
                                by.fetch_add(n as u64, Ordering::Relaxed);
                                parser.push(&buf[..n], |f| {
                                    // not after a retune: the frames are of another channel
                                    if !st.load(Ordering::Relaxed) {
                                        let _ = tx.try_send(f);
                                    }
                                });
                            }
                            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                                // a multiplexer sends a frame every 24 ms
                                idle += 1;
                                if idle > 20 {
                                    log::warn!("{kind} input {}:{}: no data for 10 s, reconnecting", src.host, src.port);
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    co.store(false, Ordering::Relaxed);
                    if !st.load(Ordering::Relaxed) {
                        log::info!("{kind} input {}:{} disconnected", src.host, src.port);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            })
            .expect("eti input thread");
        EtiInput { stop, thread: Some(thread), connected, bytes }
    }
}

const CIF: Duration = Duration::from_millis(24);

/// Passes the frames on one CIF apart once `delay` of them is buffered; when
/// the buffer runs dry it fills up again, and a buffer twice as full as it
/// should be (the source's clock faster than this one) is cut back.
fn pace(rx: Receiver<EtiFrame>, tx: Sender<EtiFrame>, delay: Duration, stop: &AtomicBool, kind: &str) {
    let target = ((delay.as_millis() / CIF.as_millis()) as usize).max(1);
    'fill: while !stop.load(Ordering::Relaxed) {
        while rx.len() < target {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        let mut k = 0u32;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let due = start + CIF * k;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            match rx.try_recv() {
                Ok(f) => {
                    let _ = tx.try_send(f);
                }
                Err(TryRecvError::Empty) => {
                    log::info!("{kind} input: the {} ms buffer ran empty, filling it again", delay.as_millis());
                    continue 'fill;
                }
                Err(TryRecvError::Disconnected) => return,
            }
            k += 1;
            if rx.len() > 2 * target {
                let n = rx.len() - target;
                for _ in 0..n {
                    let _ = rx.try_recv();
                }
                log::info!("{kind} input: {n} frames dropped to keep the {} ms buffer (the source runs ahead)", delay.as_millis());
            }
        }
    }
}

impl Drop for EtiInput {
    fn drop(&mut self) {
        // the thread ends by itself within a read timeout or a connect attempt;
        // waiting for it would hold up the radio thread (retune) for seconds
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(Source::parse("edi+tcp://mux.local:13000"), Some(Source { edi: true, host: "mux.local".into(), port: 13000 }));
        assert_eq!(Source::parse("eti+tcp://10.0.0.5:9200/"), Some(Source { edi: false, host: "10.0.0.5".into(), port: 9200 }));
        assert_eq!(Source::parse("tcp://[::1]:13000").map(|s| s.host), Some("::1".into()));
        assert_eq!(Source::parse("udp://x:1"), None);
    }
}
