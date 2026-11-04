//! RadioDNS service information lookup (ETSI TS 103 270 / TS 102 818):
//! DNS CNAME + SRV resolution, SI.xml download and logo retrieval.
//! Runs in a background thread and reports logos back to the radio thread.

use crate::radio::Command;
use crossbeam_channel::{Receiver, Sender};
use std::net::UdpSocket;
use std::time::Duration;

pub struct Request {
    pub sid: u32,
    pub ecc: u8,
    pub eid: u16,
    pub scids: u8,
    /// Other ensembles the service was received on: broadcasters often register
    /// only some of them, the information is the same.
    pub other_eids: Vec<u16>,
    /// Stations received here: of the further services of a document only
    /// these are fetched (a large broadcaster's lists about a thousand).
    pub known: std::sync::Arc<std::collections::HashSet<u32>>,
}

/// Downloads of logos are paced: a document names hundreds, and they share
/// the receiver's line with the listeners.
const LOGO_PACE: Duration = Duration::from_millis(250);

/// A downloaded SI.xml document.
pub struct SiDocument {
    pub url: String,
    pub host: String,
    pub xml: String,
}

pub struct LogoResult {
    pub sid: u32,
    pub ecc: u8,
    pub width: u16,
    pub height: u16,
    pub mime: Option<String>,
    pub url: String,
    pub data: Vec<u8>,
}

// ------------------------------------------------------------------ DNS

fn nameserver() -> String {
    if let Ok(s) = std::fs::read_to_string("/etc/resolv.conf") {
        for l in s.lines() {
            let l = l.trim();
            if let Some(ns) = l.strip_prefix("nameserver") {
                let ns = ns.trim();
                if ns.contains(':') && !ns.contains('.') {
                    return format!("[{ns}]:53");
                }
                return format!("{ns}:53");
            }
        }
    }
    "1.1.1.1:53".to_string()
}

fn encode_name(name: &str, out: &mut Vec<u8>) {
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn read_name(msg: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *msg.get(i + 1)? as usize;
            if end.is_none() {
                end = Some(i + 2);
            }
            i = ptr;
            jumps += 1;
            if jumps > 20 {
                return None;
            }
            continue;
        }
        labels.push(String::from_utf8_lossy(msg.get(i + 1..i + 1 + len)?).to_string());
        i += 1 + len;
    }
    Some((labels.join("."), end.unwrap_or(i)))
}

/// Answers of the given record type: (type, data offset, data length).
fn query(name: &str, qtype: u16) -> Option<(Vec<u8>, Vec<(u16, usize, usize)>)> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let id: u16 = rand::random();
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    encode_name(name, &mut q);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]);
    sock.send_to(&q, nameserver()).ok()?;
    let mut buf = vec![0u8; 4096];
    let (n, _) = sock.recv_from(&mut buf).ok()?;
    let msg = buf[..n].to_vec();
    if msg.len() < 12 || u16::from_be_bytes([msg[0], msg[1]]) != id {
        return None;
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        let (_, e) = read_name(&msg, i)?;
        i = e + 4;
    }
    let mut answers = Vec::new();
    for _ in 0..an {
        let (_, e) = read_name(&msg, i)?;
        i = e;
        let t = u16::from_be_bytes([*msg.get(i)?, *msg.get(i + 1)?]);
        let rdlen = u16::from_be_bytes([*msg.get(i + 8)?, *msg.get(i + 9)?]) as usize;
        answers.push((t, i + 10, rdlen));
        i += 10 + rdlen;
    }
    Some((msg, answers))
}

pub(crate) fn cname(name: &str) -> Option<String> {
    let (msg, ans) = query(name, 5)?;
    ans.iter().find(|a| a.0 == 5).and_then(|a| read_name(&msg, a.1).map(|(n, _)| n))
}

pub(crate) fn srv(name: &str) -> Option<(String, u16)> {
    let (msg, ans) = query(name, 33)?;
    let a = ans.iter().find(|a| a.0 == 33)?;
    let port = u16::from_be_bytes([*msg.get(a.1 + 4)?, *msg.get(a.1 + 5)?]);
    let (target, _) = read_name(&msg, a.1 + 6)?;
    Some((target, port))
}

/// TXT records of a name (the strings of each record joined); None when the
/// lookup failed, empty when there are none.
pub(crate) fn txt(name: &str) -> Option<Vec<String>> {
    let (msg, ans) = query(name, 16)?;
    Some(ans.iter().filter(|a| a.0 == 16).filter_map(|&(_, at, len)| txt_strings(msg.get(at..at + len)?)).collect())
}

/// Character strings of TXT data (each a length byte and the text).
fn txt_strings(data: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let n = data[i] as usize;
        out.push_str(&String::from_utf8_lossy(data.get(i + 1..i + 1 + n)?));
        i += 1 + n;
    }
    Some(out)
}

// ------------------------------------------------------------------ XML

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

fn tags(xml: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('>') else { break };
        let body = &rest[..end];
        rest = &rest[end + 1..];
        if body.starts_with('?') || body.starts_with('!') {
            continue;
        }
        let closing = body.starts_with('/');
        let body = body.trim_start_matches('/').trim_end_matches('/');
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_string();
        let name = name.rsplit(':').next().unwrap_or("").to_string();
        let mut attrs = Vec::new();
        let mut a = parts.next().unwrap_or("");
        while let Some(eq) = a.find('=') {
            let key = a[..eq].trim().to_string();
            let after = a[eq + 1..].trim_start();
            let q = after.chars().next().unwrap_or('"');
            if q != '"' && q != '\'' {
                break;
            }
            let Some(close) = after[1..].find(q) else { break };
            attrs.push((key, unescape(&after[1..1 + close])));
            a = &after[close + 2..];
        }
        out.push(Tag { name, closing, attrs });
    }
    out
}

/// Logos (url, width, height, mime) of the service with the given bearer in an SI document.
fn logos_for_bearer(xml: &str, bearer: &str) -> Vec<(String, u16, u16, Option<String>)> {
    let bearer = bearer.to_ascii_lowercase();
    logos_where(xml, |id| id == bearer)
}

/// Logos of the service whose bearer ids (lower case) satisfy `matches`.
fn logos_where(xml: &str, matches: impl Fn(&str) -> bool) -> Vec<(String, u16, u16, Option<String>)> {
    let mut result = Vec::new();
    let mut in_service = false;
    let mut matched = false;
    let mut current = Vec::new();
    for t in tags(xml) {
        match (t.name.as_str(), t.closing) {
            ("service", false) => {
                in_service = true;
                matched = false;
                current.clear();
            }
            ("service", true) => {
                if matched {
                    result.append(&mut current);
                }
                in_service = false;
            }
            ("bearer", false) if in_service => {
                if t.attrs.iter().any(|(k, v)| k == "id" && matches(&v.to_ascii_lowercase())) {
                    matched = true;
                }
            }
            ("multimedia", false) if in_service => {
                let get = |k: &str| t.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                if let Some(url) = get("url") {
                    let w = get("width").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let h = get("height").and_then(|v| v.parse().ok()).unwrap_or(0);
                    current.push((url, w, h, get("mimeValue")));
                }
            }
            _ => {}
        }
    }
    result
}

/// Logos of every service with a DAB bearer (GCC form) in an SI document:
/// (ECC, SId, url, width, height, mime).
fn all_dab_logos(xml: &str) -> Vec<(u8, u32, String, u16, u16, Option<String>)> {
    let mut result = Vec::new();
    let mut ids: Vec<(u8, u32)> = Vec::new();
    let mut current: Vec<(String, u16, u16, Option<String>)> = Vec::new();
    for t in tags(xml) {
        match (t.name.as_str(), t.closing) {
            ("service", false) => {
                ids.clear();
                current.clear();
            }
            ("service", true) => {
                for &(ecc, sid) in &ids {
                    for (url, w, h, mime) in &current {
                        result.push((ecc, sid, url.clone(), *w, *h, mime.clone()));
                    }
                }
            }
            ("bearer", false) => {
                let id = t.attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.to_ascii_lowercase()).unwrap_or_default();
                // dab:<gcc>.<eid>.<sid>.<scids>, gcc = country digit + ECC
                let parts: Vec<&str> = id.strip_prefix("dab:").unwrap_or("").split('.').collect();
                if parts.len() == 4 && parts[0].len() == 3 {
                    if let (Ok(ecc), Ok(sid)) = (u8::from_str_radix(&parts[0][1..], 16), u32::from_str_radix(parts[2], 16)) {
                        if !ids.contains(&(ecc, sid)) {
                            ids.push((ecc, sid));
                        }
                    }
                }
            }
            ("multimedia", false) => {
                let get = |k: &str| t.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                if let Some(url) = get("url") {
                    let w = get("width").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let h = get("height").and_then(|v| v.parse().ok()).unwrap_or(0);
                    current.push((url, w, h, get("mimeValue")));
                }
            }
            _ => {}
        }
    }
    result
}

pub(crate) fn http_get(url: &str) -> Option<Vec<u8>> {
    // (a stalled server must not stop the lookups of all other services)
    let mut resp = ureq::get(url)
        .header("User-Agent", "MagicDAB/0.1")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .call()
        .ok()?;
    resp.body_mut().with_config().limit(4 << 20).read_to_vec().ok()
}

/// SI.xml downloads of the last minutes (one document serves many services).
type SiCache = std::collections::HashMap<String, (std::time::Instant, Vec<u8>)>;

/// SI documents whose logos have all been downloaded (once per run).
static COMPLETE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn lookup(req: &Request, cache: &mut SiCache, docs: &mut Vec<SiDocument>) -> Vec<LogoResult> {
    let gcc = format!("{:x}{:02x}", (req.sid >> 12) & 0xF, req.ecc);
    // the ensemble received, then the others the service is known from
    let mut found = None;
    for eid in std::iter::once(req.eid).chain(req.other_eids.iter().copied()) {
        let fqdn = format!("{}.{:04x}.{:04x}.{}.dab.radiodns.org", req.scids, req.sid, eid, gcc);
        if let Some(t) = cname(&fqdn) {
            found = Some((eid, t));
            break;
        }
        log::debug!("RadioDNS: no CNAME for {fqdn}");
    }
    let Some((eid, target)) = found else { return Vec::new() };
    if eid != req.eid {
        log::info!("RadioDNS: {:04X} has no entry for ensemble {:04X}, found through {eid:04X}", req.sid, req.eid);
    }
    let Some((host, port)) = srv(&format!("_radiospi._tcp.{target}")).or_else(|| srv(&format!("_radioepg._tcp.{target}"))) else {
        log::debug!("RadioDNS: {target} has no SPI service");
        return Vec::new();
    };
    let host = host.trim_end_matches('.');
    let scheme = if port == 443 { "https" } else { "http" };
    let base = if (scheme == "http" && port == 80) || (scheme == "https" && port == 443) {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    };
    let si_url = format!("{base}/radiodns/spi/3.1/SI.xml");
    let si = match cache.get(&si_url) {
        Some((t, d)) if t.elapsed() < Duration::from_secs(600) => d.clone(),
        _ => {
            let Some(d) = http_get(&si_url) else {
                log::info!("RadioDNS: cannot download SI.xml from {base}");
                return Vec::new();
            };
            cache.insert(si_url.clone(), (std::time::Instant::now(), d.clone()));
            docs.push(SiDocument { url: si_url.clone(), host: host.to_string(), xml: String::from_utf8_lossy(&d).to_string() });
            d
        }
    };
    let xml = String::from_utf8_lossy(&si);
    let bearer = format!("dab:{}.{:04x}.{:04x}.{}", gcc, eid, req.sid, req.scids);
    let mut refs = logos_for_bearer(&xml, &bearer);
    if refs.is_empty() {
        // the document may list the service on other ensembles only
        let (head, tail) = (format!("dab:{gcc}."), format!(".{:04x}.{}", req.sid, req.scids));
        refs = logos_where(&xml, |id| id.len() == bearer.len() && id.starts_with(&head) && id.ends_with(&tail));
    }
    log::info!("RadioDNS: {} logo(s) for {:04X} from {base}", refs.len(), req.sid);
    let mut out = Vec::new();
    for (url, w, h, mime) in refs {
        let full = if url.starts_with("http") { url.clone() } else { format!("{base}/{}", url.trim_start_matches('/')) };
        std::thread::sleep(LOGO_PACE);
        if let Some(data) = http_get(&full) {
            out.push(LogoResult { sid: req.sid, ecc: req.ecc, width: w, height: h, mime, url: full, data });
        }
    }
    // the logos of all other services of the document too, so that the SPI view
    // shows them from the local cache
    let first = {
        let mut done = COMPLETE.lock().unwrap();
        if done.contains(&si_url) {
            false
        } else {
            done.push(si_url.clone());
            true
        }
    };
    if first {
        let mut fetched = 0;
        for (ecc, sid, url, w, h, mime) in all_dab_logos(&xml) {
            if (sid == req.sid && ecc == req.ecc) || !req.known.contains(&sid) {
                continue;
            }
            let full = if url.starts_with("http") { url.clone() } else { format!("{base}/{}", url.trim_start_matches('/')) };
            std::thread::sleep(LOGO_PACE);
            if let Some(data) = http_get(&full) {
                fetched += 1;
                out.push(LogoResult { sid, ecc, width: w, height: h, mime, url: full, data });
            }
        }
        log::info!("RadioDNS: {fetched} logo(s) of further services from {base}");
    }
    out
}

/// Start the worker thread; returns the request channel.
pub fn spawn(results: Sender<Command>) -> Sender<Request> {
    let (tx, rx): (Sender<Request>, Receiver<Request>) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("radiodns".into())
        .spawn(move || {
            let mut cache = SiCache::new();
            for req in rx {
                let mut docs = Vec::new();
                let logos = lookup(&req, &mut cache, &mut docs);
                for d in docs {
                    if results.send(Command::RadioDnsDoc(d)).is_err() {
                        return;
                    }
                }
                for logo in logos {
                    if results.send(Command::RadioDnsLogo(logo)).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("radiodns thread");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si_logos() {
        let xml = r#"<serviceInformation><services><service><bearer id="dab:ce1.c7e9.d4a2.0"/>
            <mediaDescription><multimedia url="http://x/a.png" width="32" height="32" mimeValue="image/png"/></mediaDescription>
            </service><service><bearer id="dab:ce1.c7e9.d4a3.0"/><mediaDescription><multimedia url="b.png" width="600" height="600"/></mediaDescription></service></services></serviceInformation>"#;
        let l = logos_for_bearer(xml, "dab:ce1.c7e9.d4a3.0");
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].1, 600);
    }
    #[test]
    fn txt_data() {
        assert_eq!(txt_strings(b"\x0513335\x03 | ").as_deref(), Some("13335 | "));
        assert_eq!(txt_strings(b"").as_deref(), Some(""));
        // a string longer than the data
        assert_eq!(txt_strings(b"\x09short"), None);
    }

    #[test]
    fn service_listed_on_other_ensembles() {
        // the station lists two of its ensembles (1001, 1002), not the third (1003)
        let xml = r#"<serviceInformation><services><service><bearer id="dab:4e2.1001.4e2b.0"/><bearer id="dab:4e2.1002.4e2b.0"/>
            <mediaDescription><multimedia url="https://radiodns.example.org/logo/600x600.png" width="600" height="600"/></mediaDescription>
            </service></services></serviceInformation>"#;
        assert!(logos_for_bearer(xml, "dab:4e2.1003.4e2b.0").is_empty());
        let bearer = "dab:4e2.1003.4e2b.0";
        let l = logos_where(xml, |id| id.len() == bearer.len() && id.starts_with("dab:4e2.") && id.ends_with(".4e2b.0"));
        assert_eq!(l.len(), 1);
    }
}
