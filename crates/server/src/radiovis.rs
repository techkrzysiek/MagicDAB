//! RadioVIS (ETSI TS 101 499, slideshow over IP): pictures and texts of a
//! service pushed by the broadcaster over STOMP. The server is found with
//! RadioDNS (`_radiovis._tcp` SRV record of the service's FQDN); the topics are
//! `/topic/dab/<gcc>/<eid>/<sid>/<scids>/image` and `.../text` with the
//! messages `SHOW <url>` and `TEXT <text>`. Only the services listeners have
//! selected are followed, with one connection per STOMP server.

use crate::radio::Command;
use crate::radiodns::{cname, http_get, srv};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use magicdab_dab::receiver::ServiceKey;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// RadioDNS identity of a DAB service.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bearer {
    pub sid: u32,
    pub ecc: u8,
    pub eid: u16,
    pub scids: u8,
}

impl Bearer {
    fn gcc(&self) -> String {
        format!("{:x}{:02x}", (self.sid >> 12) & 0xF, self.ecc)
    }

    fn fqdn(&self) -> String {
        format!("{}.{:04x}.{:04x}.{}.dab.radiodns.org", self.scids, self.sid, self.eid, self.gcc())
    }

    fn topic(&self) -> String {
        format!("/topic/dab/{}/{:04x}/{:04x}/{}", self.gcc(), self.eid, self.sid, self.scids)
    }
}

pub struct VisSlide {
    pub url: String,
    /// Click-through link.
    pub link: Option<String>,
    pub mime: String,
    pub data: Vec<u8>,
}

type Server = (String, u16);

/// Topics a connection should be subscribed to: topic base -> service.
type Topics = Arc<Mutex<HashMap<String, ServiceKey>>>;

pub struct RadioVis {
    tx: Sender<Vec<(ServiceKey, Bearer)>>,
}

impl RadioVis {
    pub fn spawn(events: Sender<Command>) -> RadioVis {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("radiovis".into()).spawn(move || manager(rx, events)).expect("radiovis thread");
        RadioVis { tx }
    }

    /// Services to follow (replaces the previous set).
    pub fn want(&self, services: Vec<(ServiceKey, Bearer)>) {
        let _ = self.tx.send(services);
    }
}

struct Connection {
    topics: Topics,
    alive: Arc<AtomicBool>,
}

fn manager(rx: Receiver<Vec<(ServiceKey, Bearer)>>, events: Sender<Command>) {
    // RadioDNS answers: most services have no RadioVIS, so negative answers are kept too
    let mut dns: HashMap<Bearer, (Instant, Option<Server>)> = HashMap::new();
    let mut connections: HashMap<Server, Connection> = HashMap::new();
    let mut wanted: Vec<(ServiceKey, Bearer)> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(w) => wanted = w,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let mut desired: HashMap<Server, HashMap<String, ServiceKey>> = HashMap::new();
        for (key, bearer) in &wanted {
            let fresh = dns.get(bearer).is_some_and(|(t, _)| t.elapsed() < Duration::from_secs(3600));
            if !fresh {
                let server = cname(&bearer.fqdn()).and_then(|target| srv(&format!("_radiovis._tcp.{target}")));
                let server = server.map(|(host, port)| (host.trim_end_matches('.').to_string(), port));
                match &server {
                    Some((h, p)) => log::info!("RadioVIS: {:04X} at {h}:{p}", bearer.sid),
                    None => log::debug!("RadioVIS: none for {}", bearer.fqdn()),
                }
                dns.insert(bearer.clone(), (Instant::now(), server));
            }
            if let Some((_, Some(server))) = dns.get(bearer) {
                desired.entry(server.clone()).or_default().insert(bearer.topic(), *key);
            }
        }
        connections.retain(|_, c| c.alive.load(Ordering::Relaxed));
        for (server, c) in &connections {
            if !desired.contains_key(server) {
                c.topics.lock().unwrap().clear();
            }
        }
        for (server, topics) in desired {
            match connections.get(&server) {
                Some(c) => *c.topics.lock().unwrap() = topics,
                None => {
                    let c = Connection { topics: Arc::new(Mutex::new(topics)), alive: Arc::new(AtomicBool::new(true)) };
                    let (t, a, ev, s) = (c.topics.clone(), c.alive.clone(), events.clone(), server.clone());
                    std::thread::Builder::new()
                        .name("radiovis-stomp".into())
                        .spawn(move || {
                            connection(&s, &t, &ev);
                            a.store(false, Ordering::Relaxed);
                        })
                        .expect("radiovis connection thread");
                    connections.insert(server, c);
                }
            }
        }
    }
}

struct Frame {
    command: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Frame {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// Take the next complete STOMP frame from `buf`.
fn next_frame(buf: &mut Vec<u8>) -> Option<Frame> {
    // end of lines between frames (heart-beats)
    let Some(start) = buf.iter().position(|&b| b != b'\n' && b != b'\r') else {
        buf.clear();
        return None;
    };
    let (head_end, body_start) = match (find(buf, b"\n\n", start), find(buf, b"\r\n\r\n", start)) {
        (Some(a), Some(b)) if b < a => (b, b + 4),
        (Some(a), _) => (a, a + 2),
        (None, Some(b)) => (b, b + 4),
        (None, None) => return None,
    };
    let head = String::from_utf8_lossy(&buf[start..head_end]).to_string();
    let mut lines = head.lines();
    let command = lines.next().unwrap_or("").trim().to_string();
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let length = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse::<usize>().ok());
    let (body_end, end) = match length.map(|n| body_start.checked_add(n)) {
        // (a length beyond any buffer: the connection ends at its size limit)
        Some(Some(e)) if buf.len() > e => (e, e + 1),
        Some(_) => return None,
        None => {
            let nul = buf[body_start..].iter().position(|&b| b == 0)? + body_start;
            (nul, nul + 1)
        }
    };
    let body = buf[body_start..body_end].to_vec();
    buf.drain(..end);
    Some(Frame { command, headers, body })
}

fn send_frame(s: &mut TcpStream, command: &str, headers: &[(&str, &str)]) -> std::io::Result<()> {
    let mut f = format!("{command}\n");
    for (k, v) in headers {
        f.push_str(&format!("{k}:{v}\n"));
    }
    f.push('\n');
    let mut b = f.into_bytes();
    b.push(0);
    s.write_all(&b)
}

/// One STOMP server: keeps the subscriptions in line with `topics` and
/// returns once nothing has been wanted for a minute.
fn connection(server: &Server, topics: &Topics, events: &Sender<Command>) {
    let (host, port) = server;
    let mut idle_since: Option<Instant> = None;
    loop {
        let addr = (host.as_str(), *port).to_socket_addrs().ok().and_then(|mut a| a.next());
        match addr.and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10)).ok()) {
            Some(stream) => {
                session(stream, server, topics, events, &mut idle_since);
                if idle_since.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
                    return;
                }
            }
            None => log::warn!("RadioVIS: cannot connect to {host}:{port}"),
        }
        // reconnect later, unless nobody needs the server any more
        for _ in 0..30 {
            std::thread::sleep(Duration::from_secs(1));
            if topics.lock().unwrap().is_empty() {
                return;
            }
        }
    }
}

fn session(mut s: TcpStream, (host, port): &Server, topics: &Topics, events: &Sender<Command>, idle_since: &mut Option<Instant>) {
    if send_frame(&mut s, "CONNECT", &[("accept-version", "1.0,1.1,1.2"), ("host", host)]).is_err() {
        return;
    }
    s.set_read_timeout(Some(Duration::from_secs(1))).ok();
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16384];
    // destination -> subscription id
    let mut subscribed: HashMap<String, String> = HashMap::new();
    // last picture per topic base: repetitions (e.g. after reconnecting) are not fetched again
    let mut last_url: HashMap<String, String> = HashMap::new();
    let mut next_id = 0u32;
    let mut connected = false;
    let mut last_data = Instant::now();
    loop {
        if connected {
            let want = topics.lock().unwrap().clone();
            if want.is_empty() {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(60) {
                    let _ = send_frame(&mut s, "DISCONNECT", &[]);
                    return;
                }
            } else {
                *idle_since = None;
            }
            let dests: Vec<String> = want.keys().flat_map(|t| [format!("{t}/image"), format!("{t}/text")]).collect();
            for d in &dests {
                if !subscribed.contains_key(d) {
                    let id = next_id.to_string();
                    next_id += 1;
                    if send_frame(&mut s, "SUBSCRIBE", &[("destination", d), ("id", &id), ("ack", "auto")]).is_err() {
                        return;
                    }
                    subscribed.insert(d.clone(), id);
                }
            }
            let gone: Vec<String> = subscribed.keys().filter(|d| !dests.contains(d)).cloned().collect();
            for d in gone {
                if let Some(id) = subscribed.remove(&d) {
                    let _ = send_frame(&mut s, "UNSUBSCRIBE", &[("destination", &d), ("id", &id)]);
                }
                last_url.remove(d.trim_end_matches("/image").trim_end_matches("/text"));
            }
        }
        // servers without heart-beats: reconnect when quiet for long (a dead
        // connection is not noticed otherwise; the current slide comes again)
        if last_data.elapsed() > Duration::from_secs(900) {
            return;
        }
        match s.read(&mut chunk) {
            Ok(0) => {
                log::info!("RadioVIS: {host}:{port} closed the connection");
                return;
            }
            Ok(n) => {
                last_data = Instant::now();
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 1 << 20 {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => return,
        }
        while let Some(f) = next_frame(&mut buf) {
            match f.command.as_str() {
                "CONNECTED" => {
                    connected = true;
                    log::info!("RadioVIS: connected to {host}:{port}");
                }
                "ERROR" => {
                    log::warn!("RadioVIS: {host}:{port}: {}", f.header("message").unwrap_or(""));
                    return;
                }
                "MESSAGE" => handle_message(&f, topics, events, &mut last_url),
                _ => {}
            }
        }
    }
}

fn handle_message(f: &Frame, topics: &Topics, events: &Sender<Command>, last_url: &mut HashMap<String, String>) {
    let Some(dest) = f.header("destination") else { return };
    let Some((base, kind)) = dest.rsplit_once('/') else { return };
    let Some(key) = topics.lock().unwrap().get(base).copied() else { return };
    let body = String::from_utf8_lossy(&f.body);
    let body = body.trim();
    match (kind, body.split_once(' ')) {
        ("image", Some(("SHOW", url))) => {
            let url = url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) || last_url.get(base).is_some_and(|u| u == url) {
                return;
            }
            let Some(data) = http_get(url) else {
                log::info!("RadioVIS: cannot download {url}");
                return;
            };
            let mime = crate::spi::sniff_mime(&data);
            if !mime.starts_with("image/") {
                log::info!("RadioVIS: {url} is not a picture");
                return;
            }
            last_url.insert(base.to_string(), url.to_string());
            let link = f.header("link").filter(|l| !l.is_empty()).map(|l| l.to_string());
            let _ = events.send(Command::VisSlide { key, slide: VisSlide { url: url.to_string(), link, mime: mime.to_string(), data } });
        }
        ("text", Some(("TEXT", text))) => {
            let _ = events.send(Command::VisText { key, text: text.trim().to_string() });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_names() {
        let b = Bearer { sid: 0xD4A2, ecc: 0xE1, eid: 0xC7E9, scids: 0 };
        assert_eq!(b.fqdn(), "0.d4a2.c7e9.de1.dab.radiodns.org");
        assert_eq!(b.topic(), "/topic/dab/de1/c7e9/d4a2/0");
    }

    #[test]
    fn frames() {
        let mut buf = b"\nCONNECTED\nversion:1.0\n\n\0MESSAGE\ndestination:/topic/x/text\ncontent-length:8\n\nTEXT a\0b\0MESS".to_vec();
        let f = next_frame(&mut buf).unwrap();
        assert_eq!(f.command, "CONNECTED");
        let f = next_frame(&mut buf).unwrap();
        assert_eq!((f.command.as_str(), f.header("destination")), ("MESSAGE", Some("/topic/x/text")));
        assert_eq!(f.body, b"TEXT a\0b");
        assert!(next_frame(&mut buf).is_none());
        assert_eq!(buf, b"MESS");
    }
}
