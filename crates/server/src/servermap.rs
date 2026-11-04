//! Public map of MagicDAB servers (https://dab.dxserver.eu): a server its admin lists registers there, keeps
//! its entry alive with heartbeats and removes it when the listing is
//! switched off. The requests run on a thread of their own; the radio thread
//! only hands over snapshots and keeps the entry's token in the settings.

use crate::changes::ChangeEvent;
use crate::config::Settings;
use crate::public::PublicInfo;
use crate::servicelist::{now, DataEntry, FormatEntry, ServiceList, SubchEntry};
use crate::spi::SpiStore;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use magicdab_sdr::DeviceConfig;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The map of an empty setting (followed, so a later version can move it).
pub const DEFAULT_URL: &str = "https://dab.dxserver.eu";
/// Heartbeats until the map names its own interval.
const HEARTBEAT: Duration = Duration::from_secs(300);
const MIN_HEARTBEAT: Duration = Duration::from_secs(60);
const MAX_HEARTBEAT: Duration = Duration::from_secs(3600);
/// A change (name, address, another multiplex) goes out before the next
/// heartbeat once it has lasted this long, not for every station a listener
/// flips through...
const SETTLE: Duration = Duration::from_secs(30);
/// ...and the previous request is this old.
const MIN_GAP: Duration = Duration::from_secs(60);
/// The station list after any request (the map takes a PUT per 20 s).
const STATIONS_AFTER: Duration = Duration::from_secs(20);
/// No request may hang: a map may accept connections and never answer.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Removals tried before the entry is left to expire on the map.
const REMOVE_TRIES: u32 = 3;
/// Snapshots for the requests while listed; the first one after the start
/// once the receiver had time to tune.
const SNAPSHOT_EVERY: Duration = Duration::from_secs(10);
const STARTUP: Duration = Duration::from_secs(15);
/// Nothing to do: woken by new settings or snapshots.
const IDLE: Duration = Duration::from_secs(3600);
const MAX_ENSEMBLES: usize = 100;
const MAX_TII: usize = 16;
/// Stations per ensemble in the station list.
const MAX_STATIONS: usize = 64;
/// Data services and sub-channels of a multiplex in the station list (DAB has
/// at most 64 sub-channels).
const MAX_DATA: usize = 32;
const MAX_SUBCHANNELS: usize = 64;
/// Bodies over this are sent compressed (the station list: tens of KB of
/// repetitive JSON).
const GZIP_FROM: usize = 1024;
/// Scan reports and clips (the map's API, "Scan reports", "Clips"):
/// queued, sent between the entry's own requests, one at least every 10 s.
const UPLOAD_GAP: Duration = Duration::from_secs(10);
const UPLOADS_QUEUED: usize = 30;
const UPLOAD_TRIES: u32 = 3;
/// SPI and the programme guide: what the map takes (the map's API, "SPI").
const LOGO_MAX_BYTES: usize = 128 << 10;
const LOGOS_PER_RUN: usize = 60;
const EPG_AHEAD: i64 = 2 * 86400;
const EPG_PER_SERVICE: usize = 150;
/// The programme guide goes again after so long even unchanged (its window moves on).
const EPG_AGAIN_S: i64 = 6 * 3600;

/// Listing on the map (Settings > Webserver).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MapConfig {
    pub enabled: bool,
    /// Address of the map; empty: the built-in one.
    pub url: String,
    /// Public address of this server; empty: the tunnel's.
    pub public_url: String,
    /// Our entry, issued at the first registration (its token proves it is
    /// ours); kept out of the web interface.
    pub entry: Option<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// The map that issued it.
    pub map: String,
    pub id: String,
    pub token: String,
}

/// The fields the interface may change (the entry comes from the map only).
pub fn apply(c: &mut MapConfig, v: &Value) {
    if let Some(b) = v["enabled"].as_bool() {
        c.enabled = b;
    }
    if let Some(u) = v["url"].as_str() {
        let u = u.trim().trim_end_matches('/');
        c.url = if u == DEFAULT_URL { String::new() } else { u.to_string() };
    }
    if let Some(u) = v["publicUrl"].as_str() {
        c.public_url = u.trim().trim_end_matches('/').to_string();
    }
}

/// What the settings ask for, worked out on the radio thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub enabled: bool,
    /// The map in use.
    pub map: String,
    /// Address of this server on the map.
    pub url: Option<String>,
    /// Why it cannot be listed (a text key of the interface).
    pub problem: Option<&'static str>,
}

impl Target {
    pub fn from_settings(s: &Settings) -> Target {
        let c = &s.server_map;
        let map = match c.url.trim().trim_end_matches('/') {
            "" => DEFAULT_URL.to_string(),
            u => u.to_string(),
        };
        let own = c.public_url.trim().trim_end_matches('/');
        let tunnel = s.tunnel.enabled.then(|| s.tunnel.url()).flatten();
        let url = if own.is_empty() { tunnel } else { Some(own.to_string()) };
        // (the token goes only over https, or to this computer for tests)
        let problem = if !crate::update::secure(&map) {
            Some("bad_map_url")
        } else if s.location.is_none() {
            Some("no_location")
        } else {
            match &url {
                None => Some("no_public_url"),
                Some(u) => check_public(u).err(),
            }
        };
        // a private server (Settings > Access) is nobody's to find
        let problem = problem.or((!s.public_server).then_some("private_server"));
        Target { enabled: c.enabled, map, url, problem }
    }

    /// Nothing - entry, heartbeats, station list - goes to the map unless
    /// this holds: listing on, a public server, a public address, a location.
    pub fn listed(&self) -> bool {
        self.enabled && self.problem.is_none() && self.url.is_some()
    }
}

/// An address others can open: http(s) and a host outside the local network
/// (no query: the map appends /static_data to check it).
fn check_public(url: &str) -> Result<(), &'static str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).ok_or("bad_public_url")?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || url.len() > 200
        || url.contains(['?', '#'])
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("bad_public_url");
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { authority }),
    };
    if host.is_empty() {
        return Err("bad_public_url");
    }
    if is_local(host) {
        return Err("local_public_url");
    }
    Ok(())
}

fn is_local(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    match h.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => {
            let o = a.octets();
            // (100.64/10: carrier-grade NAT, also Tailscale)
            a.is_private() || a.is_loopback() || a.is_link_local() || a.is_unspecified() || (o[0] == 100 && o[1] & 0xC0 == 64)
        }
        Ok(std::net::IpAddr::V6(a)) => {
            let s = a.segments()[0];
            a.is_loopback() || a.is_unspecified() || s & 0xFE00 == 0xFC00 || s & 0xFFC0 == 0xFE80
        }
        Err(_) => !h.contains('.') || [".local", ".lan", ".home", ".internal", ".localhost", ".home.arpa"].iter().any(|d| h.ends_with(d)),
    }
}

/// What the map is told, collected on the radio thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub name: String,
    pub description: String,
    pub location: Option<(f64, f64)>,
    pub tuner: &'static str,
    pub locked: bool,
    pub listeners: usize,
    pub scanning: bool,
    pub now: Option<Now>,
    pub ensembles: Vec<Received>,
}

/// The multiplex tuned now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Now {
    pub channel: String,
    pub eid: Option<u16>,
    pub ensemble: String,
    pub service: String,
    pub snr: Option<f32>,
    pub source: &'static str,
}

/// An ensemble of the service list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Received {
    pub channel: String,
    pub eid: u16,
    pub ecc: u8,
    pub label: String,
    pub short: String,
    pub services: usize,
    pub snr: f32,
    pub last_seen: i64,
    pub source: &'static str,
    pub tii: Vec<String>,
    /// Its radio stations, for the station list.
    pub stations: Vec<Station>,
    pub frequency: u32,
    pub subchannels: Vec<SubchEntry>,
    pub data: Vec<DataEntry>,
}

/// A radio station of the station list.
#[derive(Clone, Debug, Default, PartialEq, Hash)]
pub struct Station {
    pub sid: u32,
    pub ecc: u8,
    pub label: String,
    pub short: String,
    pub codec: String,
    pub bitrate: u16,
    pub protection: String,
    pub pty: Option<u8>,
    pub language: Option<u8>,
    pub subch: Option<u8>,
    pub ca: bool,
    pub apps: Vec<u16>,
    pub format: Option<FormatEntry>,
}

impl Snapshot {
    pub fn build(s: &Settings, p: &PublicInfo, list: &ServiceList, scanning: bool) -> Snapshot {
        // (during a scan the receiver hops through the band: see merge)
        let now = p.channel.clone().filter(|_| !scanning).map(|channel| Now {
            source: source(s, &channel),
            eid: p.eid,
            ensemble: p.ensemble.trim().to_string(),
            service: p.label.trim().to_string(),
            snr: p.synced.then_some(p.snr),
            channel,
        });
        let mut ens: Vec<&crate::servicelist::EnsembleEntry> = list.ensembles.iter().collect();
        if ens.len() > MAX_ENSEMBLES {
            // the ones received last
            ens.sort_by_key(|e| std::cmp::Reverse(e.last_seen));
            ens.truncate(MAX_ENSEMBLES);
            ens.sort_by_key(|e| (e.frequency, e.eid));
        }
        Snapshot {
            name: s.station_name.trim().to_string(),
            description: s.description.clone(),
            location: s.location.as_ref().map(|l| (l.lat, l.lon)),
            tuner: match s.device {
                DeviceConfig::RtlSdr { .. } => "rtlsdr",
                DeviceConfig::RtlTcp { .. } => "rtltcp",
                DeviceConfig::RawFile { .. } => "file",
                DeviceConfig::None => "none",
                DeviceConfig::Dtr100 => "dtr100",
            },
            locked: s.tuner_locked,
            listeners: p.listeners,
            scanning,
            now,
            ensembles: ens
                .into_iter()
                .map(|e| Received {
                    channel: e.channel.clone(),
                    eid: e.eid,
                    ecc: e.ecc,
                    label: e.label.trim().to_string(),
                    short: e.short.trim().to_string(),
                    services: e.services.len(),
                    snr: e.snr,
                    last_seen: e.last_seen,
                    source: source(s, &e.channel),
                    tii: e.tii.iter().take(MAX_TII).cloned().collect(),
                    stations: e
                        .services
                        .iter()
                        .take(MAX_STATIONS)
                        .map(|v| Station {
                            sid: v.sid,
                            ecc: v.ecc.unwrap_or(e.ecc),
                            label: v.label.trim().to_string(),
                            short: v.short.trim().to_string(),
                            codec: v.codec.clone(),
                            bitrate: v.bitrate,
                            protection: v.protection.clone(),
                            pty: v.pty,
                            language: v.language,
                            subch: v.subch,
                            ca: v.ca,
                            apps: v.apps.clone(),
                            format: v.format.clone(),
                        })
                        .collect(),
                    frequency: e.frequency,
                    subchannels: e.subchannels.iter().take(MAX_SUBCHANNELS).cloned().collect(),
                    data: e.data.iter().take(MAX_DATA).cloned().collect(),
                })
                .collect(),
        }
    }
}

/// Where a channel comes from: the air, an ETI/EDI stream or a recording (a
/// DX map must not take a stream for reception).
pub(crate) fn source(s: &Settings, channel: &str) -> &'static str {
    match s.channel_maps.iter().find(|m| m.channel.eq_ignore_ascii_case(channel)) {
        Some(m) if m.file().is_some() => "file",
        Some(m) if crate::etiinput::Source::parse(&m.url).is_some() => "stream",
        _ => match s.device {
            DeviceConfig::RawFile { .. } => "file",
            DeviceConfig::None => "stream",
            _ => "air",
        },
    }
}

/// During a band scan the map keeps the multiplex the receiver returns to.
fn merge(prev: Option<&Snapshot>, mut s: Snapshot) -> Snapshot {
    if s.scanning && s.now.is_none() {
        s.now = prev.and_then(|p| p.now.clone());
    }
    s
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn round(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

/// The entry as the map gets it (the map's API, "The entry").
pub fn payload(t: &Target, s: &Snapshot) -> Value {
    // (about 1 km: the exact location stays here, for the TII distances)
    let (lat, lon) = s.location.map(|(a, b)| (round(a, 2), round(b, 2))).unzip();
    let name = if s.name.is_empty() { "MagicDAB" } else { s.name.as_str() };
    json!({
        "name": clip(name, 64),
        "description": clip(&s.description, 1000),
        "url": t.url,
        "lat": lat,
        "lon": lon,
        "software": "MagicDAB",
        "version": crate::radio::VERSION,
        "tuner": s.tuner,
        "locked": s.locked,
        "listeners": s.listeners,
        "scanning": s.scanning,
        "now": s.now.as_ref().map(|n| json!({
            "channel": n.channel,
            "eid": n.eid.map(|e| format!("{e:04X}")),
            "ensemble": clip(&n.ensemble, 32),
            "service": clip(&n.service, 32),
            "snr": n.snr.map(|v| round(v as f64, 1)),
            "source": n.source,
        })),
        "ensembles": s.ensembles.iter().map(|e| json!({
            "channel": e.channel,
            "eid": format!("{:04X}", e.eid),
            "ecc": format!("{:02X}", e.ecc),
            "label": clip(&e.label, 32),
            "services": e.services,
            "snr": round(e.snr as f64, 1),
            "lastSeen": e.last_seen,
            "source": e.source,
            "tii": e.tii,
        })).collect::<Vec<_>>(),
        "stations": version(stations_key(s)),
    })
}

/// The station list as the map gets it (the map's API, "Station list").
pub fn stations_payload(s: &Snapshot) -> Value {
    json!({
        "version": version(stations_key(s)),
        "ensembles": s.ensembles.iter().map(|e| json!({
            "channel": e.channel,
            "eid": format!("{:04X}", e.eid),
            "ecc": format!("{:02X}", e.ecc),
            "label": clip(&e.label, 32),
            "short": clip(&e.short, 8),
            "source": e.source,
            "snr": round(e.snr as f64, 1),
            "lastSeen": e.last_seen,
            "frequency": e.frequency,
            "services": e.stations.iter().map(|v| json!({
                "sid": format!("{:04X}", v.sid),
                "ecc": format!("{:02X}", v.ecc),
                "label": clip(&v.label, 32),
                "short": clip(&v.short, 8),
                "codec": v.codec,
                "bitrate": v.bitrate,
                "protection": (!v.protection.is_empty()).then_some(&v.protection),
                "pty": v.pty,
                "language": v.language,
                "subch": v.subch,
                "ca": v.ca,
                "apps": v.apps,
                "format": v.format.as_ref().map(|f| json!({"codec": f.codec, "sampleRate": f.sample_rate, "stereo": f.stereo})),
            })).collect::<Vec<_>>(),
            "subchannels": e.subchannels.iter().map(|c| json!({
                "id": c.id,
                "start": c.start,
                "size": c.size,
                "protection": (!c.protection.is_empty()).then_some(&c.protection),
                "bitrate": c.bitrate,
            })).collect::<Vec<_>>(),
            "data": e.data.iter().map(|d| json!({
                "sid": format!("{:08X}", d.sid),
                "label": clip(&d.label, 32),
                "short": clip(&d.short, 8),
                "subch": d.subch,
                "packet": d.packet,
                "apps": d.apps,
                "ca": d.ca,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// What the station list is judged by: not the signal or the time of the
/// last reception, or every scan would send it again.
pub fn stations_key(s: &Snapshot) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in &s.ensembles {
        (&e.channel, e.eid, e.ecc, &e.label, &e.short, e.source, &e.stations).hash(&mut h);
        // (not the TII codes heard: the entry carries them)
        (e.frequency, &e.subchannels, &e.data).hash(&mut h);
    }
    h.finish()
}

/// The version of a station list, as the map gets and returns it.
fn version(key: u64) -> String {
    format!("{key:016x}")
}

/// What makes a change worth an early update: not the listener count, the
/// SNR or the station heard.
fn key(t: &Target, s: &Snapshot) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let loc = s.location.map(|(a, b)| ((round(a, 2) * 100.0) as i64, (round(b, 2) * 100.0) as i64));
    (&s.name, &s.description, &t.url, loc, s.locked, s.tuner).hash(&mut h);
    s.now.as_ref().map(|n| (&n.channel, n.eid, &n.ensemble)).hash(&mut h);
    for e in &s.ensembles {
        (&e.channel, e.eid, &e.label, e.source).hash(&mut h);
    }
    h.finish()
}

// ---------------------------------------------------------------- schedule

/// Our entry as the schedule sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Held {
    None,
    /// Issued by the map in use.
    Here,
    /// Issued by a map no longer in use.
    Elsewhere,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Register,
    Update,
    /// The station list.
    Stations,
    Remove,
    Wait(Duration),
}

/// When requests are made: heartbeats, early updates of changes, the station
/// list, backoff after errors.
#[derive(Debug)]
pub struct Schedule {
    interval: Duration,
    /// The last heartbeat, the last request of any kind, the last station list.
    last_sent: Option<Instant>,
    last_request: Option<Instant>,
    last_stations: Option<Instant>,
    sent_key: Option<u64>,
    /// Content that differs from the one sent, and since when.
    changed: Option<(u64, Instant)>,
    /// The station list the map holds (sent, or named in its answers), and
    /// one of ours that differs, since when.
    stations_held: Option<u64>,
    stations_changed: Option<(u64, Instant)>,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            interval: HEARTBEAT,
            last_sent: None,
            last_request: None,
            last_stations: None,
            sent_key: None,
            changed: None,
            stations_held: None,
            stations_changed: None,
            failures: 0,
            retry_at: None,
        }
    }
}

impl Schedule {
    /// The next request or how long to wait for it; `key` is the content to
    /// send (None: no snapshot yet), `stations` that of the station list.
    pub fn next(&mut self, now: Instant, listed: bool, held: Held, key: Option<u64>, stations: Option<u64>) -> Step {
        if let Some(t) = self.retry_at.filter(|t| *t > now) {
            return Step::Wait(t - now);
        }
        let key = match (held, listed, key) {
            (Held::Elsewhere, _, _) | (Held::Here, false, _) => return Step::Remove,
            (_, false, _) | (_, true, None) => return Step::Wait(IDLE),
            (Held::None, true, Some(_)) => return Step::Register,
            (Held::Here, true, Some(k)) => k,
        };
        let Some(last) = self.last_sent else { return Step::Update };
        let due = last + self.interval;
        if now >= due {
            return Step::Update;
        }
        let gap = self.last_request.unwrap_or(last) + MIN_GAP;
        let mut wait = due;
        if self.sent_key == Some(key) {
            self.changed = None;
        } else {
            let since = match self.changed {
                Some((k, t)) if k == key => t,
                _ => {
                    self.changed = Some((key, now));
                    now
                }
            };
            let early = (since + SETTLE).max(gap);
            if now >= early {
                return Step::Update;
            }
            wait = wait.min(early);
        }
        // the station list, once the entry is there: when it is not the one
        // the map holds, once that has settled
        match stations.filter(|k| Some(*k) != self.stations_held) {
            Some(k) => {
                let since = match self.stations_changed {
                    Some((s, t)) if s == k => t,
                    _ => {
                        self.stations_changed = Some((k, now));
                        now
                    }
                };
                // (a gap of its own: with a heartbeat every MIN_GAP the gap of
                // all requests would never open before the next heartbeat)
                let gap = self.last_request.map_or(now, |t| t + STATIONS_AFTER).max(self.last_stations.map_or(now, |t| t + MIN_GAP));
                let early = (since + SETTLE).max(gap);
                if now >= early {
                    return Step::Stations;
                }
                wait = wait.min(early);
            }
            None => self.stations_changed = None,
        }
        Step::Wait(wait - now)
    }

    /// The map took the request; `interval` is the heartbeat it asks for.
    pub fn sent(&mut self, now: Instant, key: Option<u64>, interval: Option<Duration>) {
        if let Some(i) = interval {
            self.interval = i.clamp(MIN_HEARTBEAT, MAX_HEARTBEAT);
        }
        self.last_sent = Some(now);
        self.last_request = Some(now);
        self.sent_key = key;
        self.changed = None;
        self.failures = 0;
        self.retry_at = None;
    }

    /// The map took the station list.
    pub fn stations_sent(&mut self, now: Instant, key: u64) {
        self.last_request = Some(now);
        self.last_stations = Some(now);
        self.stations_held = Some(key);
        self.stations_changed = None;
        self.failures = 0;
        self.retry_at = None;
    }

    /// The map named the station list it holds (None: none).
    pub fn map_holds(&mut self, key: Option<u64>) {
        self.stations_held = key;
    }

    /// The map does not take this station list: again once it changes.
    pub fn stations_refused(&mut self, key: u64) {
        self.stations_held = Some(key);
        self.stations_changed = None;
    }

    /// A request failed: the next one after the backoff, or when the map
    /// says. `jitter` (0..1) spreads the servers after an outage of the map.
    pub fn failed(&mut self, now: Instant, retry_after: Option<Duration>, jitter: f64) -> Duration {
        self.failures += 1;
        let d = match retry_after {
            Some(d) => d.clamp(Duration::from_secs(10), MAX_HEARTBEAT),
            None => backoff(self.failures, jitter),
        };
        self.retry_at = Some(now + d);
        d
    }

    /// New settings: the admin waits for the result, no backoff.
    pub fn retry_now(&mut self) {
        self.retry_at = None;
    }
}

/// 30 s, 1, 2, 4 … minutes up to half an hour, plus up to 20 %.
pub fn backoff(failures: u32, jitter: f64) -> Duration {
    let secs = (30u64 << failures.saturating_sub(1).min(6)).min(1800);
    Duration::from_secs_f64(secs as f64 * (1.0 + jitter.clamp(0.0, 1.0) * 0.2))
}

// ---------------------------------------------------------------- answers

#[derive(Debug, PartialEq)]
struct Failure {
    /// network, missing, server, busy, refused, response (texts of the interface)
    kind: &'static str,
    detail: String,
    retry_after: Option<Duration>,
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Ok,
    /// The map no longer knows the entry or its token.
    Lost,
    Failed(Failure),
}

fn classify(status: u16, body: &Value, retry_after: Option<Duration>) -> Outcome {
    let code = body["error"].as_str().filter(|c| !c.is_empty());
    let fail = |kind, detail| Outcome::Failed(Failure { kind, detail, retry_after: None });
    match status {
        200..=299 => Outcome::Ok,
        401 | 403 | 404 if matches!(code, Some("unknown_server" | "bad_token")) => Outcome::Lost,
        429 => Outcome::Failed(Failure { kind: "busy", detail: "HTTP 429".into(), retry_after }),
        503 if retry_after.is_some() => Outcome::Failed(Failure { kind: "busy", detail: "HTTP 503".into(), retry_after }),
        // the API's own answer: its message is for the admin
        400..=499 if code.is_some() => {
            fail("refused", body["message"].as_str().map(|m| clip(m, 200)).unwrap_or_else(|| code.unwrap_or_default().to_string()))
        }
        500..=599 => fail("server", format!("HTTP {status}")),
        // a proxy's 404 page, a redirect: no map service there (yet)
        _ => fail("missing", format!("HTTP {status}")),
    }
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_token(t: &str) -> bool {
    (16..=256).contains(&t.len()) && t.chars().all(|c| c.is_ascii_graphic())
}

struct Reply {
    status: u16,
    body: Value,
    retry_after: Option<Duration>,
}

#[derive(Clone, Copy, PartialEq)]
enum Method {
    Post,
    Put,
    Delete,
}

/// `compress`: a large body goes gzipped.
fn request(method: Method, url: &str, token: Option<&str>, body: Option<&Value>, compress: bool) -> Result<Reply, Failure> {
    let agent = format!("MagicDAB/{}", crate::radio::VERSION);
    let auth = token.map(|t| format!("Bearer {t}"));
    // (no redirects: the map answers at its address, and a token never
    // follows one)
    let sent = if method == Method::Delete {
        let mut r = ureq::delete(url).header("User-Agent", agent.as_str()).header("Accept", "application/json");
        if let Some(a) = &auth {
            r = r.header("Authorization", a.as_str());
        }
        r.config().timeout_global(Some(TIMEOUT)).http_status_as_error(false).max_redirects(0).build().call()
    } else {
        let mut r = if method == Method::Post { ureq::post(url) } else { ureq::put(url) };
        r = r.header("User-Agent", agent.as_str()).header("Accept", "application/json");
        if let Some(a) = &auth {
            r = r.header("Authorization", a.as_str());
        }
        let mut data = body.map(|b| b.to_string()).unwrap_or_default().into_bytes();
        if compress && data.len() > GZIP_FROM {
            r = r.header("Content-Encoding", "gzip");
            data = gzip(&data);
        }
        r.config()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .content_type("application/json")
            .send(data.as_slice())
    };
    let mut resp = sent.map_err(|e| Failure { kind: "network", detail: e.to_string(), retry_after: None })?;
    let status = resp.status().as_u16();
    let retry_after =
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()).map(Duration::from_secs);
    let data = resp.body_mut().with_config().limit(64 << 10).read_to_vec().unwrap_or_default();
    Ok(Reply { status, body: serde_json::from_slice(&data).unwrap_or(Value::Null), retry_after })
}

/// Raw bytes (a clip, a logo), answered like the JSON requests.
fn request_bytes(method: Method, url: &str, token: &str, content_type: &str, data: &[u8]) -> Result<Reply, Failure> {
    let agent = format!("MagicDAB/{}", crate::radio::VERSION);
    let auth = format!("Bearer {token}");
    let sent = if method == Method::Put { ureq::put(url) } else { ureq::post(url) }
        .header("User-Agent", agent.as_str())
        .header("Accept", "application/json")
        .header("Authorization", auth.as_str())
        .config()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .content_type(content_type)
        .send(data);
    let mut resp = sent.map_err(|e| Failure { kind: "network", detail: e.to_string(), retry_after: None })?;
    let status = resp.status().as_u16();
    let retry_after =
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()).map(Duration::from_secs);
    let data = resp.body_mut().with_config().limit(64 << 10).read_to_vec().unwrap_or_default();
    Ok(Reply { status, body: serde_json::from_slice(&data).unwrap_or(Value::Null), retry_after })
}

fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // (writing into a Vec does not fail)
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

// ---------------------------------------------------------------- the thread

/// What the settings show about the listing.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// off, unavailable, connecting, listed, error, removing
    pub state: &'static str,
    /// Why it cannot be listed.
    pub problem: Option<&'static str>,
    /// The map in use, the built-in one, the address listed.
    pub map: String,
    pub default_map: &'static str,
    pub url: Option<String>,
    /// The entry and its page on the map.
    pub id: Option<String>,
    pub page: Option<String>,
    /// The map shows the entry (false: still checking its address).
    pub visible: Option<bool>,
    /// What the map says about the entry.
    pub notice: Option<String>,
    /// Unix times of the last successful request, of the last error and of the next try.
    pub last_ok: Option<i64>,
    pub error_kind: Option<&'static str>,
    pub error: Option<String>,
    pub error_at: Option<i64>,
    pub retry_at: Option<i64>,
    /// Unix time the station list was last sent.
    pub stations_at: Option<i64>,
}

#[derive(Default)]
struct Shared {
    /// None until the radio thread starts: nothing is done before.
    target: Option<Target>,
    snapshot: Option<Snapshot>,
    /// An entry issued or given up, for the settings.
    entry: Option<Option<Entry>>,
    /// The admin applied the map settings: no waiting for a backoff.
    retry: bool,
    status: Status,
    status_seq: u64,
    uploads: std::collections::VecDeque<Upload>,
    /// Keys of the SPI and the programme guide the map has (none: send them).
    spi_sent: Option<u64>,
    epg_sent: Option<u64>,
}

/// A clip of a station for the map (crate::clips): Ogg Opus.
pub struct Clip {
    pub channel: String,
    pub eid: u16,
    pub sid: u32,
    pub time: i64,
    pub seconds: f32,
    pub dls: String,
    pub data: Vec<u8>,
    /// Decoder stalls and lost audio units while recording (0: clean).
    pub glitches: u32,
    /// The slide the station showed then (mime, image), sent after the clip.
    pub slide: Option<(String, Arc<Vec<u8>>)>,
}

enum Upload {
    Scan(Value, u32),
    Clip(Clip, u32),
    /// Events of the change log (the map's API, "Changes").
    Changes(Value, u32),
    Spi(SpiUpload, u32),
    /// The programme guide and its key.
    Epg(Value, u64, u32),
}

/// SPI of the stations heard (the map's API, "SPI"): the body with the
/// hashes of the logos, and the files they are of (the map asks for those it lacks).
pub struct SpiUpload {
    body: Value,
    key: u64,
    files: Vec<LogoFile>,
}

struct LogoFile {
    sha256: String,
    mime: String,
    path: PathBuf,
}

/// The radio stations of the station list received from the air, once each (ECC, SId).
fn air_services(s: &Snapshot) -> Vec<(u8, u32)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in s.ensembles.iter().filter(|e| e.source == "air") {
        for st in &e.stations {
            if st.sid <= 0xFFFF && seen.insert((st.ecc, st.sid)) {
                out.push((st.ecc, st.sid));
            }
        }
    }
    out
}

fn hash_of(v: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// SPI of the stations received from the air: names, description and two logos
/// (a square one of about 128 px for lists, 320x240 for the station), each named
/// by the SHA-256 of its file. None when there is nothing to tell.
pub fn spi_upload(spi: &SpiStore, s: &Snapshot) -> Option<SpiUpload> {
    let mut services = Vec::new();
    let mut files: Vec<LogoFile> = Vec::new();
    for (ecc, sid) in air_services(s) {
        let sent = logos_to_send(spi, ecc, sid, &mut files);
        let text = spi.text(ecc, sid);
        let field = |f: fn(&crate::spi::ServiceText) -> &Option<String>, max: usize| {
            text.and_then(|t| f(t).as_deref()).map(|x| clip(x.trim(), max)).unwrap_or_default()
        };
        let (medium, long, description) = (field(|t| &t.medium_name, 32), field(|t| &t.long_name, 128), field(|t| &t.description, 1200));
        if sent.is_empty() && medium.is_empty() && long.is_empty() && description.is_empty() {
            continue;
        }
        services.push(json!({
            "ecc": format!("{ecc:02X}"), "sid": format!("{sid:04X}"),
            "mediumName": medium, "longName": long, "description": description, "logos": sent,
        }));
    }
    // the multiplexes' own logos (SPI keeps them by EId)
    let mut ensembles = Vec::new();
    let mut seen = HashSet::new();
    for e in s.ensembles.iter().filter(|e| e.source == "air") {
        if !seen.insert((e.ecc, e.eid)) {
            continue;
        }
        let sent = logos_to_send(spi, e.ecc, e.eid as u32, &mut files);
        if !sent.is_empty() {
            ensembles.push(json!({ "ecc": format!("{:02X}", e.ecc), "eid": format!("{:04X}", e.eid), "logos": sent }));
        }
    }
    if services.is_empty() && ensembles.is_empty() {
        return None;
    }
    let body = json!({ "services": services, "ensembles": ensembles });
    let key = hash_of(&body.to_string());
    Some(SpiUpload { body, key, files })
}

/// The logos of a station (or of a multiplex, by its EId) for the map: a square
/// one of about 128 px and the 320x240 one, by the SHA-256 of their files (added
/// to `files` for the map to ask for).
fn logos_to_send(spi: &SpiStore, ecc: u8, id: u32, files: &mut Vec<LogoFile>) -> Vec<Value> {
    let logos: Vec<&crate::spi::LogoVariant> =
        spi.spi_logos(ecc, id).into_iter().filter(|v| v.mime == "image/png" || v.mime == "image/jpeg").collect();
    let square = logos
        .iter()
        .filter(|v| v.width == v.height && v.width >= 96)
        .min_by_key(|v| v.width)
        .or_else(|| logos.iter().filter(|v| v.width == v.height).max_by_key(|v| v.width))
        .copied();
    let wide = logos.iter().find(|v| v.width == 320 && v.height == 240).copied();
    let mut sent = Vec::new();
    for v in square.into_iter().chain(wide) {
        let path = spi.logo_path(v);
        let Ok(data) = std::fs::read(&path) else { continue };
        if data.len() > LOGO_MAX_BYTES {
            continue;
        }
        let sha: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        sent.push(json!({"width": v.width, "height": v.height, "mime": v.mime, "sha256": sha}));
        if !files.iter().any(|f| f.sha256 == sha) {
            files.push(LogoFile { sha256: sha, mime: v.mime.clone(), path });
        }
    }
    sent
}

/// The programme guide of the stations received from the air, from an hour ago
/// to two days ahead, and its key: the guide itself and a period of 6 hours, so
/// that it goes when it changes and, as the window moves on, now and then.
/// (The guide is kept by SId: a SId heard with two ECCs, two countries, is left out.)
pub fn epg_upload(spi: &SpiStore, s: &Snapshot, now: i64) -> Option<(Value, u64)> {
    let services = air_services(s);
    let mut eccs: HashMap<u32, HashSet<u8>> = HashMap::new();
    for (ecc, sid) in &services {
        eccs.entry(*sid).or_default().insert(*ecc);
    }
    let mut out = Vec::new();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (now / EPG_AGAIN_S).hash(&mut h);
    for (ecc, sid) in services.into_iter().filter(|(_, sid)| eccs[sid].len() == 1) {
        let all = spi.programmes(sid);
        let programmes: Vec<Value> = all
            .iter()
            .filter(|p| p.start + p.duration > now - 3600 && p.start < now + EPG_AHEAD && !p.name.trim().is_empty())
            .take(EPG_PER_SERVICE)
            .map(|p| {
                json!({
                    "start": p.start, "duration": p.duration.clamp(0, 2 * 86400), "name": clip(p.name.trim(), 128),
                    "description": clip(p.description.as_deref().unwrap_or("").trim(), 600),
                })
            })
            .collect();
        if programmes.is_empty() {
            continue;
        }
        (ecc, sid, all.len(), all.last().map(|p| (p.start, &p.name))).hash(&mut h);
        out.push(json!({ "ecc": format!("{ecc:02X}"), "sid": format!("{sid:04X}"), "programmes": programmes }));
    }
    (!out.is_empty()).then(|| (json!({ "services": out }), h.finish()))
}

/// A text for the address of a request (RFC 3986 unreserved stays).
fn query_text(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// This installation on the map (the map's API, "Identity"): a UUID
/// that stays across registrations and entries, so that the map can tell a
/// server's data apart (and exclude it), and the secret key that proves it.
/// Kept in data/instance.json, never shown in the interface.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Identity {
    pub uuid: String,
    pub key: String,
}

impl Identity {
    pub fn load(dir: &std::path::Path) -> Identity {
        let path = dir.join("instance.json");
        if let Some(i) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Identity>(&s).ok()) {
            if valid_uuid(&i.uuid) && i.key.len() >= 32 {
                return i;
            }
        }
        let mut b: [u8; 16] = rand::random();
        // version 4, variant 10 (RFC 9562)
        b[6] = 0x40 | (b[6] & 0x0F);
        b[8] = 0x80 | (b[8] & 0x3F);
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let uuid = format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
        let key: String = rand::random::<[u8; 32]>().iter().map(|x| format!("{x:02x}")).collect();
        let i = Identity { uuid, key };
        if let Ok(s) = serde_json::to_string(&i) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
                }
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        log::info!("server map: this installation is {}", i.uuid);
        i
    }
}

fn valid_uuid(u: &str) -> bool {
    u.len() == 36 && u.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// The radio thread's side.
pub struct ServerMap {
    shared: Arc<Mutex<Shared>>,
    wake: Sender<()>,
    started: Instant,
    next_snapshot: Option<Instant>,
    status_seen: u64,
}

impl ServerMap {
    pub fn start(entry: Option<Entry>, identity: Identity) -> ServerMap {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (wake, woken) = crossbeam_channel::bounded(1);
        let worker = Worker { shared: shared.clone(), entry, identity, ..Worker::default() };
        if let Err(e) = std::thread::Builder::new().name("servermap".into()).spawn(move || worker.run(woken)) {
            log::warn!("server map: cannot start its thread: {e}");
        }
        ServerMap { shared, wake, started: Instant::now(), next_snapshot: None, status_seen: 0 }
    }

    /// The settings changed (or the program started); `retry`: the admin
    /// applied the map's own settings and waits for the result.
    pub fn configure(&mut self, target: Target, retry: bool) {
        let listed = target.listed();
        {
            let mut s = self.shared.lock();
            if s.target.as_ref() == Some(&target) && !retry {
                return;
            }
            s.target = Some(target);
            s.retry |= retry;
        }
        // a new listing at once, the first one after the start when tuned
        let first = self.next_snapshot.unwrap_or((self.started + STARTUP).max(Instant::now()));
        self.next_snapshot = listed.then_some(first);
        let _ = self.wake.try_send(());
    }

    /// Time for a snapshot (every 10 s while listed).
    pub fn snapshot_due(&mut self) -> bool {
        match self.next_snapshot {
            Some(t) if Instant::now() >= t => {
                self.next_snapshot = Some(Instant::now() + SNAPSHOT_EVERY);
                true
            }
            _ => false,
        }
    }

    pub fn update(&self, s: Snapshot) {
        {
            let mut g = self.shared.lock();
            let s = merge(g.snapshot.as_ref(), s);
            g.snapshot = Some(s);
        }
        let _ = self.wake.try_send(());
    }

    /// Whether the server is listed: only then are scan reports and clips worth making.
    pub fn listed(&self) -> bool {
        self.shared.lock().target.as_ref().is_some_and(|t| t.listed())
    }

    /// The report of a whole band scan, to the map (when listed).
    pub fn send_scan(&self, report: Value) {
        self.queue(Upload::Scan(report, 0));
    }

    /// A clip of a station, to the map (when listed).
    pub fn send_clip(&self, clip: Clip) {
        self.queue(Upload::Clip(clip, 0));
    }

    /// Events of the change log, to the map (when listed): the history of a
    /// multiplex gets what the server noticed, at the time it did (also what
    /// happened before the map knew the multiplex).
    pub fn send_changes<'a>(&self, events: impl IntoIterator<Item = &'a ChangeEvent>) {
        let events: Vec<Value> = events.into_iter().filter_map(change_event).collect();
        for part in events.chunks(100) {
            self.queue(Upload::Changes(json!({ "events": part }), 0));
        }
    }

    /// SPI of the stations heard, to the map (when listed), unless it has it.
    pub fn send_spi(&self, u: SpiUpload) {
        {
            let g = self.shared.lock();
            if g.spi_sent == Some(u.key) || g.uploads.iter().any(|x| matches!(x, Upload::Spi(q, _) if q.key == u.key)) {
                return;
            }
        }
        self.queue(Upload::Spi(u, 0));
    }

    /// The programme guide of the stations heard, to the map (when listed), unless it has it.
    pub fn send_epg(&self, body: Value, key: u64) {
        {
            let g = self.shared.lock();
            if g.epg_sent == Some(key) || g.uploads.iter().any(|x| matches!(x, Upload::Epg(_, k, _) if *k == key)) {
                return;
            }
        }
        self.queue(Upload::Epg(body, key, 0));
    }

    fn queue(&self, u: Upload) {
        {
            let mut g = self.shared.lock();
            if !g.target.as_ref().is_some_and(|t| t.listed()) {
                return;
            }
            // (a map out of reach for long: the oldest give way)
            while g.uploads.len() >= UPLOADS_QUEUED {
                g.uploads.pop_front();
            }
            g.uploads.push_back(u);
        }
        let _ = self.wake.try_send(());
    }

    /// The entry issued or given up since the last call, for the settings.
    pub fn entry_changed(&self) -> Option<Option<Entry>> {
        self.shared.lock().entry.take()
    }

    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    /// The status when it changed since the last call.
    pub fn status_changed(&mut self) -> Option<Status> {
        let g = self.shared.lock();
        if g.status_seq == self.status_seen {
            return None;
        }
        self.status_seen = g.status_seq;
        Some(g.status.clone())
    }

    /// The entry id `/static_data` shows (the map checks the address with it).
    pub fn listed_id(s: &Settings) -> Option<String> {
        let map = Target::from_settings(s).map;
        s.server_map.entry.as_ref().filter(|e| s.server_map.enabled && e.map == map).map(|e| e.id.clone())
    }
}

/// Requests to the map, on the thread "servermap".
#[derive(Default)]
struct Worker {
    shared: Arc<Mutex<Shared>>,
    entry: Option<Entry>,
    schedule: Schedule,
    target: Target,
    /// From the last answer of the map.
    visible: Option<bool>,
    notice: Option<String>,
    page: Option<String>,
    last_ok: Option<i64>,
    /// The last error: kind, detail, when; the next try.
    error: Option<(&'static str, String, i64)>,
    retry_at: Option<i64>,
    /// Errors are logged once per kind.
    logged: Option<String>,
    removals: u32,
    /// When the station list was last sent.
    stations_at: Option<i64>,
    identity: Identity,
    /// Scan reports and clips wait until then; a refusal is logged once per kind.
    next_upload: Option<Instant>,
    upload_logged: Option<&'static str>,
}

impl Worker {
    fn run(mut self, wake: Receiver<()>) {
        loop {
            let (target, snapshot, retry) = {
                let mut s = self.shared.lock();
                (s.target.clone(), s.snapshot.clone(), std::mem::take(&mut s.retry))
            };
            // (a stored entry would look like one of another map)
            let Some(target) = target else {
                if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(IDLE) {
                    return;
                }
                continue;
            };
            if target != self.target {
                self.target = target;
                self.schedule.retry_now();
                self.error = None;
                self.retry_at = None;
                self.removals = 0;
            } else if retry {
                self.schedule.retry_now();
                self.retry_at = None;
            }
            let held = match &self.entry {
                None => Held::None,
                Some(e) if e.map == self.target.map => Held::Here,
                Some(_) => Held::Elsewhere,
            };
            let key = snapshot.as_ref().map(|s| key(&self.target, s));
            let stations = snapshot.as_ref().map(stations_key);
            let step = self.schedule.next(Instant::now(), self.target.listed(), held, key, stations);
            self.publish(held);
            match (step, &snapshot) {
                (Step::Register, Some(s)) => self.register(s, key),
                (Step::Update, Some(s)) => self.update(s, key),
                (Step::Stations, Some(s)) => self.stations(s),
                (Step::Remove, _) => self.remove(),
                (step, _) => {
                    let mut d = if let Step::Wait(d) = step { d } else { IDLE };
                    // scan reports and clips in the pauses of the entry's requests
                    if held == Held::Here && self.target.listed() && !self.shared.lock().uploads.is_empty() {
                        let now = Instant::now();
                        match self.next_upload {
                            Some(t) if t > now => d = d.min(t - now),
                            _ => {
                                self.upload();
                                continue;
                            }
                        }
                    }
                    if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(d) {
                        return;
                    }
                }
            }
        }
    }

    fn register(&mut self, s: &Snapshot, key: Option<u64>) {
        let url = format!("{}/api/v1/servers", self.target.map);
        // (the key only here: it binds the installation's UUID to the entry)
        let mut body = payload(&self.target, s);
        body["uuid"] = json!(self.identity.uuid);
        body["uuidKey"] = json!(self.identity.key);
        let reply = request(Method::Post, &url, None, Some(&body), false);
        match reply.map(|r| (classify(r.status, &r.body, r.retry_after), r)) {
            Ok((Outcome::Ok, r)) => {
                let id = r.body["id"].as_str().filter(|i| valid_id(i));
                let token = r.body["token"].as_str().filter(|t| valid_token(t));
                if let (Some(id), Some(token)) = (id, token) {
                    log::info!("server map: {} listed on {} as {id}", self.target.url.as_deref().unwrap_or(""), self.target.map);
                    self.set_entry(Some(Entry { map: self.target.map.clone(), id: id.into(), token: token.into() }));
                    self.ok(&r, key);
                } else {
                    self.fail(Failure { kind: "response", detail: "no id or token in the answer".into(), retry_after: None });
                }
            }
            Ok((Outcome::Lost, r)) => self.fail(Failure { kind: "refused", detail: format!("HTTP {}", r.status), retry_after: None }),
            Ok((Outcome::Failed(f), _)) | Err(f) => self.fail(f),
        }
    }

    fn update(&mut self, s: &Snapshot, key: Option<u64>) {
        let Some(e) = self.entry.clone() else { return };
        let url = format!("{}/api/v1/servers/{}", e.map, e.id);
        let mut body = payload(&self.target, s);
        body["uuid"] = json!(self.identity.uuid);
        let reply = request(Method::Put, &url, Some(&e.token), Some(&body), false);
        match reply.map(|r| (classify(r.status, &r.body, r.retry_after), r)) {
            Ok((Outcome::Ok, r)) => self.ok(&r, key),
            Ok((Outcome::Lost, _)) => {
                log::info!("server map: {} no longer knows entry {}, registering again", e.map, e.id);
                self.set_entry(None);
            }
            Ok((Outcome::Failed(f), _)) | Err(f) => self.fail(f),
        }
    }

    /// The station list (the map's API, "Station list").
    fn stations(&mut self, s: &Snapshot) {
        let Some(e) = self.entry.clone() else { return };
        let key = stations_key(s);
        let url = format!("{}/api/v1/servers/{}/stations", e.map, e.id);
        let reply = request(Method::Put, &url, Some(&e.token), Some(&stations_payload(s)), true);
        match reply.map(|r| (classify(r.status, &r.body, r.retry_after), r)) {
            Ok((Outcome::Ok, _)) => {
                log::info!("server map: station list sent to {} ({} multiplexes)", e.map, s.ensembles.len());
                self.schedule.stations_sent(Instant::now(), key);
                self.stations_at = Some(now());
                self.error = None;
                self.retry_at = None;
            }
            Ok((Outcome::Lost, _)) => {
                log::info!("server map: {} no longer knows entry {}, registering again", e.map, e.id);
                self.set_entry(None);
            }
            // the map does not take this list (too large, a field it minds):
            // the entry is fine, the list goes again once it changes
            Ok((Outcome::Failed(f), _)) if f.kind == "refused" => {
                log::warn!("server map: {} refused the station list: {}", e.map, f.detail);
                self.schedule.stations_refused(key);
            }
            Ok((Outcome::Failed(f), _)) | Err(f) => self.fail(f),
        }
    }

    /// The first queued scan report or clip. A refused one is dropped (logged
    /// once per kind), a passing problem gives it a few more tries.
    fn upload(&mut self) {
        let Some(e) = self.entry.clone() else { return };
        let Some(u) = self.shared.lock().uploads.pop_front() else { return };
        let (reply, what) = match &u {
            Upload::Scan(v, _) => {
                (request(Method::Put, &format!("{}/api/v1/servers/{}/scan", e.map, e.id), Some(&e.token), Some(v), true), "scan report")
            }
            Upload::Clip(c, _) => {
                let url = format!(
                    "{}/api/v1/servers/{}/clips?channel={}&eid={:04X}&sid={:04X}&time={}&seconds={:.1}&glitches={}&dls={}",
                    e.map,
                    e.id,
                    query_text(&c.channel),
                    c.eid,
                    c.sid,
                    c.time,
                    c.seconds,
                    c.glitches,
                    query_text(&c.dls)
                );
                (request_bytes(Method::Post, &url, &e.token, "audio/ogg", &c.data), "clip")
            }
            Upload::Changes(v, _) => {
                (request(Method::Post, &format!("{}/api/v1/servers/{}/changes", e.map, e.id), Some(&e.token), Some(v), true), "change log")
            }
            Upload::Spi(u, _) => {
                (request(Method::Put, &format!("{}/api/v1/servers/{}/spi", e.map, e.id), Some(&e.token), Some(&u.body), true), "SPI")
            }
            Upload::Epg(v, _, _) => {
                (request(Method::Put, &format!("{}/api/v1/servers/{}/epg", e.map, e.id), Some(&e.token), Some(v), true), "programme guide")
            }
        };
        self.next_upload = Some(Instant::now() + UPLOAD_GAP);
        match reply.map(|r| (classify(r.status, &r.body, r.retry_after), r)) {
            Ok((Outcome::Ok, r)) => {
                match &u {
                    Upload::Clip(c, _) => self.clip_slide(&e, c),
                    Upload::Spi(s, _) => {
                        if self.logos(&e, s, &r.body) {
                            self.shared.lock().spi_sent = Some(s.key);
                        }
                    }
                    Upload::Epg(_, key, _) => self.shared.lock().epg_sent = Some(*key),
                    _ => {}
                }
                log::debug!("server map: {what} sent");
            }
            Ok((Outcome::Lost, _)) => {
                log::info!("server map: {} no longer knows entry {}, registering again", e.map, e.id);
                self.set_entry(None);
            }
            Ok((Outcome::Failed(f), _)) if f.kind == "refused" => {
                if self.upload_logged != Some(what) {
                    log::warn!("server map: {} refused a {what}: {}", e.map, f.detail);
                    self.upload_logged = Some(what);
                }
                // (not again until it changes)
                match &u {
                    Upload::Spi(s, _) => self.shared.lock().spi_sent = Some(s.key),
                    Upload::Epg(_, key, _) => self.shared.lock().epg_sent = Some(*key),
                    _ => {}
                }
            }
            Ok((Outcome::Failed(f), _)) | Err(f) => {
                if let Some(wait) = f.retry_after {
                    self.next_upload = Some(Instant::now() + wait.clamp(UPLOAD_GAP, Duration::from_secs(3600)));
                } else {
                    self.next_upload = Some(Instant::now() + Duration::from_secs(60));
                }
                let again = match u {
                    Upload::Scan(v, n) if n + 1 < UPLOAD_TRIES => Some(Upload::Scan(v, n + 1)),
                    Upload::Clip(c, n) if n + 1 < UPLOAD_TRIES => Some(Upload::Clip(c, n + 1)),
                    Upload::Changes(v, n) if n + 1 < UPLOAD_TRIES => Some(Upload::Changes(v, n + 1)),
                    Upload::Spi(s, n) if n + 1 < UPLOAD_TRIES => Some(Upload::Spi(s, n + 1)),
                    Upload::Epg(v, k, n) if n + 1 < UPLOAD_TRIES => Some(Upload::Epg(v, k, n + 1)),
                    _ => None,
                };
                if let Some(u) = again {
                    self.shared.lock().uploads.push_front(u);
                }
            }
        }
    }

    /// The slide of a clip, after the clip (the map's API, "Clips"); a
    /// refusal costs the clip nothing.
    fn clip_slide(&mut self, e: &Entry, c: &Clip) {
        let Some((mime, data)) = &c.slide else { return };
        let url = format!(
            "{}/api/v1/servers/{}/clips/slide?channel={}&eid={:04X}&sid={:04X}&time={}",
            e.map,
            e.id,
            query_text(&c.channel),
            c.eid,
            c.sid,
            c.time
        );
        if let Ok(Outcome::Failed(f)) | Err(f) =
            request_bytes(Method::Put, &url, &e.token, mime, data).map(|r| classify(r.status, &r.body, r.retry_after))
        {
            log::debug!("server map: the slide of a clip not sent: {}", f.detail);
        }
    }

    /// The logo files the map asked for after the SPI (`missing`), one by one; true
    /// when it has them all now (else the SPI goes again with the next run).
    fn logos(&mut self, e: &Entry, s: &SpiUpload, answer: &Value) -> bool {
        let missing: Vec<&str> = answer["missing"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let mut sent = 0;
        for sha in missing.iter().take(LOGOS_PER_RUN) {
            let Some(f) = s.files.iter().find(|f| f.sha256 == *sha) else { continue };
            let Ok(data) = std::fs::read(&f.path) else { continue };
            std::thread::sleep(Duration::from_millis(300));
            let url = format!("{}/api/v1/servers/{}/logos/{}", e.map, e.id, f.sha256);
            match request_bytes(Method::Put, &url, &e.token, &f.mime, &data).map(|r| classify(r.status, &r.body, r.retry_after)) {
                Ok(Outcome::Ok) => sent += 1,
                Ok(Outcome::Failed(f)) | Err(f) => {
                    log::info!("server map: a logo not sent to {}: {}", e.map, f.detail);
                    return false;
                }
                Ok(Outcome::Lost) => return false,
            }
        }
        if sent > 0 {
            log::info!("server map: {sent} station logos sent to {}", e.map);
        }
        missing.len() <= LOGOS_PER_RUN
    }

    fn remove(&mut self) {
        let Some(e) = self.entry.clone() else { return };
        let url = format!("{}/api/v1/servers/{}", e.map, e.id);
        match request(Method::Delete, &url, Some(&e.token), None, false).map(|r| classify(r.status, &r.body, r.retry_after)) {
            Ok(Outcome::Ok | Outcome::Lost) => {
                log::info!("server map: removed from {}", e.map);
                self.set_entry(None);
                self.error = None;
                self.retry_at = None;
            }
            Ok(Outcome::Failed(f)) | Err(f) => {
                self.removals += 1;
                if self.removals < REMOVE_TRIES {
                    self.fail(f);
                } else {
                    log::info!("server map: cannot remove entry {} from {} ({}), it expires there by itself", e.id, e.map, f.detail);
                    self.set_entry(None);
                    self.error = None;
                    self.retry_at = None;
                }
            }
        }
    }

    /// A new entry, or none: the schedule starts afresh, the radio thread
    /// saves it in the settings.
    fn set_entry(&mut self, entry: Option<Entry>) {
        self.entry = entry.clone();
        self.schedule = Schedule::default();
        self.removals = 0;
        if self.entry.is_none() {
            (self.visible, self.notice, self.page, self.last_ok, self.stations_at) = (None, None, None, None, None);
        }
        let mut g = self.shared.lock();
        g.entry = Some(entry);
        // (a new entry gets SPI and the guide again)
        (g.spi_sent, g.epg_sent) = (None, None);
    }

    fn ok(&mut self, r: &Reply, key: Option<u64>) {
        self.schedule.sent(Instant::now(), key, r.body["heartbeat"].as_u64().map(Duration::from_secs));
        // the station list the map holds, when it says (null: none, lost)
        if let Some(v) = r.body.get("stations") {
            self.schedule.map_holds(v.as_str().and_then(|v| u64::from_str_radix(v, 16).ok()));
        }
        self.visible = r.body["visible"].as_bool();
        let notice = r.body["notice"].as_str().map(|n| clip(n.trim(), 300)).filter(|n| !n.is_empty());
        if notice.is_some() && notice != self.notice {
            log::info!("server map: {}", notice.as_deref().unwrap_or_default());
        }
        self.notice = notice;
        // (a link of the map itself, nothing else)
        let map = &self.target.map;
        self.page = r.body["page"]
            .as_str()
            .filter(|p| p.strip_prefix(map.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '#', '?'])))
            .map(String::from);
        self.last_ok = Some(now());
        self.error = None;
        self.retry_at = None;
        if self.logged.take().is_some() {
            log::info!("server map: {map} answers again");
        }
    }

    fn fail(&mut self, f: Failure) {
        let d = self.schedule.failed(Instant::now(), f.retry_after, rand::random());
        let what = match f.kind {
            "network" => "cannot be reached",
            "missing" => "has no map service (yet)",
            "server" => "has a problem",
            "busy" => "asks to wait",
            "refused" => "refused the entry",
            _ => "gave an unexpected answer",
        };
        let msg = format!("server map: {} {what}: {}; next try in {} s", self.target.map, f.detail, d.as_secs());
        let kind = format!("{}:{}", f.kind, f.detail);
        // each kind once: a map that is not there yet must not fill the log
        if self.logged.as_deref() == Some(kind.as_str()) {
            log::debug!("{msg}");
        } else if matches!(f.kind, "network" | "missing" | "busy") {
            log::info!("{msg}");
        } else {
            log::warn!("{msg}");
        }
        self.logged = Some(kind);
        self.error = Some((f.kind, f.detail, now()));
        self.retry_at = Some(now() + d.as_secs() as i64);
    }

    fn publish(&self, held: Held) {
        let t = &self.target;
        let state = if held == Held::Elsewhere || (held == Held::Here && !t.listed()) {
            "removing"
        } else if !t.enabled {
            "off"
        } else if t.problem.is_some() {
            "unavailable"
        } else if self.error.is_some() {
            "error"
        } else if held == Held::Here && self.last_ok.is_some() {
            "listed"
        } else {
            "connecting"
        };
        let status = Status {
            state,
            problem: t.problem.filter(|_| t.enabled),
            map: t.map.clone(),
            default_map: DEFAULT_URL,
            url: t.url.clone(),
            id: self.entry.as_ref().filter(|_| held == Held::Here).map(|e| e.id.clone()),
            page: self.page.clone(),
            visible: self.visible,
            notice: self.notice.clone(),
            last_ok: self.last_ok,
            error_kind: self.error.as_ref().map(|e| e.0),
            error: self.error.as_ref().map(|e| e.1.clone()),
            error_at: self.error.as_ref().map(|e| e.2),
            retry_at: self.retry_at,
            stations_at: self.stations_at.filter(|_| held == Held::Here),
        };
        let mut s = self.shared.lock();
        if s.status != status {
            s.status = status;
            s.status_seq += 1;
        }
    }
}

/// A change log event as the map takes it: the log keeps the ECC in front of
/// the EId ("E25B7A"), the map has them apart.
fn change_event(ev: &ChangeEvent) -> Option<Value> {
    let mut v = serde_json::to_value(ev).ok()?;
    if ev.eid.len() == 6 && ev.eid.is_ascii() {
        v["ecc"] = json!(&ev.eid[..2]);
        v["eid"] = json!(&ev.eid[2..]);
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChannelMap, Location};
    use crate::servicelist::{EnsembleEntry, ServiceEntry};

    /// A PNG of this size (only its header counts: crate::spi::image_size).
    fn png(w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut d = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        d.extend(w.to_be_bytes());
        d.extend(h.to_be_bytes());
        d.extend([fill; 64]);
        d
    }

    #[test]
    fn spi_of_the_stations_heard() {
        let dir = std::env::temp_dir().join(format!("magicdab-mapspi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut spi = SpiStore::load(&dir);
        // squares of 128 and 32, the 320x240 one; the same SId of another country;
        // a station of a stream (not on the map)
        spi.add_logo(0x4A91, 0xE2, 128, 128, &png(128, 128, 1), "SPI", "a", Some("image/png"));
        spi.add_logo(0x4A91, 0xE2, 32, 32, &png(32, 32, 2), "SPI", "b", Some("image/png"));
        spi.add_logo(0x4A91, 0xE2, 320, 240, &png(320, 240, 3), "SPI", "c", Some("image/png"));
        spi.add_logo(0x4A91, 0xE1, 128, 128, &png(128, 128, 4), "SPI", "d", Some("image/png"));
        spi.add_logo(0x4A92, 0xE2, 128, 128, &png(128, 128, 5), "SPI", "e", Some("image/png"));
        spi.set_text(0xE2, 0x4A91, crate::spi::ServiceText { long_name: Some("Radio Alfa".into()), ..Default::default() });
        let station = |sid| Station { sid, ecc: 0xE2, label: "PR".into(), ..Default::default() };
        spi.add_logo(0x4B6E, 0xE2, 128, 128, &png(128, 128, 6), "SPI", "f", Some("image/png"));
        let s = Snapshot {
            ensembles: vec![
                Received {
                    channel: "11B".into(),
                    eid: 0x4B6E,
                    ecc: 0xE2,
                    source: "air",
                    stations: vec![station(0x4A91)],
                    ..Default::default()
                },
                Received { channel: "10C".into(), ecc: 0xE2, source: "stream", stations: vec![station(0x4A92)], ..Default::default() },
            ],
            ..Default::default()
        };
        let u = spi_upload(&spi, &s).unwrap();
        let svc = u.body["services"].as_array().unwrap();
        assert_eq!(svc.len(), 1);
        assert_eq!(
            (svc[0]["ecc"].as_str(), svc[0]["sid"].as_str(), svc[0]["longName"].as_str()),
            (Some("E2"), Some("4A91"), Some("Radio Alfa"))
        );
        let sizes: Vec<(u64, u64)> =
            svc[0]["logos"].as_array().unwrap().iter().map(|l| (l["width"].as_u64().unwrap(), l["height"].as_u64().unwrap())).collect();
        assert_eq!(sizes, [(128, 128), (320, 240)]);
        assert_eq!(u.files.len(), 3);
        // the multiplex's own logo
        assert_eq!(
            (u.body["ensembles"][0]["eid"].as_str(), u.body["ensembles"][0]["logos"][0]["width"].as_u64()),
            (Some("4B6E"), Some(128))
        );
        let named: Vec<&Value> =
            svc[0]["logos"].as_array().unwrap().iter().chain(u.body["ensembles"][0]["logos"].as_array().unwrap()).collect();
        assert!(u.files.iter().all(|f| f.sha256.len() == 64 && named.iter().any(|l| l["sha256"] == f.sha256)));
        // unchanged: the same key, nothing goes again; no guide: nothing
        assert_eq!(spi_upload(&spi, &s).unwrap().key, u.key);
        assert!(epg_upload(&spi, &s, 1_800_000_000).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn settings() -> Settings {
        let mut s = Settings {
            station_name: "Example DAB".into(),
            location: Some(Location { lat: 52.229_74, lon: 21.012_18 }),
            ..Default::default()
        };
        s.tuner_locked = true;
        s.tunnel.enabled = true;
        s.tunnel.subdomain = "example-dab".into();
        s.server_map.enabled = true;
        s.channel_maps = vec![
            ChannelMap { channel: "10C".into(), url: "edi+tcp://127.0.0.1:9201".into() },
            ChannelMap { channel: "8C".into(), url: "file:///recordings/8c.uff".into() },
        ];
        s
    }

    fn ensemble(channel: &str, frequency: u32, eid: u16, last_seen: i64) -> EnsembleEntry {
        let svc = ServiceEntry {
            sid: 0x4A91,
            scids: 0,
            label: "Radio".into(),
            short: "Radio".into(),
            pty: Some(10),
            codec: "DAB+".into(),
            bitrate: 96,
            ecc: None,
            protection: "EEP 3-A".into(),
            language: Some(15),
            subch: Some(1),
            ca: false,
            apps: vec![2, 7],
            format: Some(FormatEntry { codec: "HE-AAC v2".into(), sample_rate: 48000, stereo: true }),
        };
        EnsembleEntry {
            channel: channel.into(),
            frequency,
            eid,
            ecc: 0xE2,
            label: format!("MUX {channel} "),
            short: String::new(),
            services: vec![svc.clone(), svc],
            last_seen,
            snr: 12.345,
            tii: (0..20).map(|i| format!("1-{i}")).collect(),
            subchannels: vec![SubchEntry { id: 1, start: 0, size: 72, protection: "EEP 3-A".into(), bitrate: 96 }],
            data: vec![DataEntry {
                sid: 0xE1C0_0001,
                label: "EPG".into(),
                short: String::new(),
                subch: Some(9),
                packet: Some(1),
                apps: vec![7],
                ca: false,
            }],
        }
    }

    fn public() -> PublicInfo {
        PublicInfo {
            channel: Some("12C".into()),
            eid: Some(0x6B3D),
            ensemble: "MUX ONE    ".into(),
            label: "Radio Example".into(),
            synced: true,
            snr: 18.44,
            listeners: 3,
            ..Default::default()
        }
    }

    fn list() -> ServiceList {
        ServiceList {
            ensembles: vec![ensemble("5A", 174928, 0x1a2b, 100), ensemble("8C", 199360, 0x1001, 50), ensemble("10C", 213360, 0xce15, 200)],
        }
    }

    #[test]
    fn payload_of_a_tunnelled_server() {
        let s = settings();
        let t = Target::from_settings(&s);
        assert_eq!(
            t,
            Target { enabled: true, map: DEFAULT_URL.into(), url: Some("https://example-dab.dxserver.eu".into()), problem: None }
        );
        let p = payload(&t, &Snapshot::build(&s, &public(), &list(), false));
        assert_eq!(p["name"], "Example DAB");
        assert_eq!(p["url"], "https://example-dab.dxserver.eu");
        // about 1 km, not the receiver's exact place
        assert_eq!((p["lat"].as_f64(), p["lon"].as_f64()), (Some(52.23), Some(21.01)));
        assert_eq!(p["software"], "MagicDAB");
        assert_eq!(p["tuner"], "rtlsdr");
        assert_eq!(p["locked"], true);
        assert_eq!(p["listeners"], 3);
        assert_eq!(p["scanning"], false);
        assert_eq!(
            p["now"],
            json!({"channel": "12C", "eid": "6B3D", "ensemble": "MUX ONE", "service": "Radio Example", "snr": 18.4, "source": "air"})
        );
        let e = p["ensembles"].as_array().unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!(
            e[0],
            json!({"channel": "5A", "eid": "1A2B", "ecc": "E2", "label": "MUX 5A", "services": 2, "snr": 12.3, "lastSeen": 100, "source": "air", "tii": (0..16).map(|i| format!("1-{i}")).collect::<Vec<_>>()})
        );
        // streams and recordings are not reception
        assert_eq!(e[1]["source"], "file");
        assert_eq!(e[2]["source"], "stream");
        // nothing about the listeners but their number, no addresses
        let text = p.to_string();
        assert!(!text.contains("127.0.0.1") && !text.contains("token"));
    }

    #[test]
    fn payload_limits() {
        let mut s = settings();
        s.station_name = "Ł".repeat(100);
        s.description = "x".repeat(2000);
        s.device = DeviceConfig::None;
        let mut l = ServiceList::default();
        for i in 0..150 {
            l.ensembles.push(ensemble(&format!("{i}"), 170_000 + i as u32, i as u16, i as i64));
        }
        let mut p = public();
        p.synced = false;
        p.channel = None;
        let v = payload(&Target::from_settings(&s), &Snapshot::build(&s, &p, &l, false));
        assert_eq!(v["name"].as_str().unwrap().chars().count(), 64);
        assert_eq!(v["description"].as_str().unwrap().len(), 1000);
        assert_eq!(v["tuner"], "none");
        assert!(v["now"].is_null());
        // the 100 received last, in the order of the band
        let e = v["ensembles"].as_array().unwrap();
        assert_eq!(e.len(), 100);
        assert_eq!(e[0]["lastSeen"], 50);
        assert_eq!(e[99]["lastSeen"], 149);
        // an unnamed station still has a name
        s.station_name = " ".into();
        assert_eq!(payload(&Target::from_settings(&s), &Snapshot::build(&s, &p, &l, false))["name"], "MagicDAB");
    }

    #[test]
    fn scan_keeps_the_multiplex() {
        let s = settings();
        let before = Snapshot::build(&s, &public(), &list(), false);
        let mut hopping = public();
        hopping.channel = Some("5B".into());
        let scan = Snapshot::build(&s, &hopping, &list(), true);
        assert!(scan.now.is_none());
        let merged = merge(Some(&before), scan);
        assert!(merged.scanning);
        assert_eq!(merged.now.as_ref().map(|n| n.channel.as_str()), Some("12C"));
        // and after the scan the multiplex it is on
        assert_eq!(merge(Some(&merged), Snapshot::build(&s, &hopping, &list(), false)).now.unwrap().channel, "5B");
    }

    #[test]
    fn early_updates_only_for_what_matters() {
        let s = settings();
        let t = Target::from_settings(&s);
        let k = key(&t, &Snapshot::build(&s, &public(), &list(), false));
        let mut p = public();
        p.listeners = 7;
        p.snr = 3.0;
        p.label = "Another station".into();
        assert_eq!(key(&t, &Snapshot::build(&s, &p, &list(), false)), k);
        p.channel = Some("5A".into());
        assert_ne!(key(&t, &Snapshot::build(&s, &p, &list(), false)), k);
        let mut renamed = s.clone();
        renamed.station_name = "Example DX".into();
        assert_ne!(key(&t, &Snapshot::build(&renamed, &public(), &list(), false)), k);
    }

    #[test]
    fn targets() {
        let problem = |f: &dyn Fn(&mut Settings)| {
            let mut s = settings();
            f(&mut s);
            Target::from_settings(&s).problem
        };
        assert_eq!(problem(&|_| {}), None);
        assert_eq!(problem(&|s| s.location = None), Some("no_location"));
        assert_eq!(problem(&|s| s.tunnel.enabled = false), Some("no_public_url"));
        assert_eq!(problem(&|s| s.public_server = false), Some("private_server"));
        assert_eq!(problem(&|s| s.server_map.public_url = "ftp://example.org".into()), Some("bad_public_url"));
        assert_eq!(problem(&|s| s.server_map.public_url = "https://".into()), Some("bad_public_url"));
        assert_eq!(problem(&|s| s.server_map.public_url = "https://user@example.org".into()), Some("bad_public_url"));
        assert_eq!(problem(&|s| s.server_map.public_url = "https://example.org/?dab".into()), Some("bad_public_url"));
        assert_eq!(problem(&|s| s.server_map.public_url = "https://example.org/dab".into()), None);
        for local in [
            "http://192.168.1.5:8080",
            "http://10.0.0.253",
            "http://localhost:8080",
            "http://[::1]:8080",
            "http://raspberrypi:8080",
            "https://dab.local",
            "http://100.101.102.103",
        ] {
            assert_eq!(problem(&|s| s.server_map.public_url = local.into()), Some("local_public_url"), "{local}");
        }
        assert_eq!(problem(&|s| s.server_map.url = "http://example.org".into()), Some("bad_map_url"));
        // a map on this computer for tests
        assert_eq!(problem(&|s| s.server_map.url = "http://127.0.0.1:8950".into()), None);
        // an own address wins over the tunnel's
        let mut s = settings();
        s.server_map.public_url = "http://203.0.113.7:8080/".into();
        let t = Target::from_settings(&s);
        assert_eq!((t.url.as_deref(), t.problem), (Some("http://203.0.113.7:8080"), None));
        s.server_map.enabled = false;
        assert!(!Target::from_settings(&s).listed());
    }

    #[test]
    fn interface_cannot_set_the_entry() {
        let mut c =
            MapConfig { entry: Some(Entry { map: DEFAULT_URL.into(), id: "abc".into(), token: "secret".into() }), ..Default::default() };
        apply(
            &mut c,
            &json!({"enabled": true, "url": "https://dab.dxserver.eu/", "publicUrl": " https://x.example.org/ ", "entry": null, "token": "evil"}),
        );
        assert!(c.enabled);
        assert_eq!(c.url, "");
        assert_eq!(c.public_url, "https://x.example.org");
        assert_eq!(c.entry.unwrap().token, "secret");
        // settings of older versions
        let s: Settings = serde_json::from_str("{\"stationName\": \"x\"}").unwrap();
        assert_eq!(s.server_map, MapConfig::default());
    }

    #[test]
    fn heartbeats_and_early_updates() {
        let secs = Duration::from_secs;
        let t0 = Instant::now();
        let mut s = Schedule::default();
        // nothing to send yet, or not listed
        assert_eq!(s.next(t0, true, Held::None, None, None), Step::Wait(IDLE));
        assert_eq!(s.next(t0, false, Held::None, Some(1), None), Step::Wait(IDLE));
        assert_eq!(s.next(t0, true, Held::None, Some(1), None), Step::Register);
        // an entry from before the start: a heartbeat at once
        assert_eq!(s.next(t0, true, Held::Here, Some(1), None), Step::Update);
        s.sent(t0, Some(1), Some(secs(300)));
        assert_eq!(s.next(t0 + secs(10), true, Held::Here, Some(1), None), Step::Wait(secs(290)));
        assert_eq!(s.next(t0 + secs(300), true, Held::Here, Some(1), None), Step::Update);
        // a change goes out once it lasted 30 s...
        assert_eq!(s.next(t0 + secs(70), true, Held::Here, Some(2), None), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(100), true, Held::Here, Some(2), None), Step::Update);
        s.sent(t0 + secs(100), Some(2), None);
        // ...and a minute after the previous request
        assert_eq!(s.next(t0 + secs(110), true, Held::Here, Some(3), None), Step::Wait(secs(50)));
        // gone back before it settled: nothing until the heartbeat
        assert_eq!(s.next(t0 + secs(120), true, Held::Here, Some(2), None), Step::Wait(secs(280)));
        // flipping through stations restarts the wait
        assert_eq!(s.next(t0 + secs(200), true, Held::Here, Some(3), None), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(220), true, Held::Here, Some(4), None), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(250), true, Held::Here, Some(4), None), Step::Update);
        // the map's interval, within limits
        s.sent(t0, Some(4), Some(secs(5)));
        assert_eq!(s.next(t0 + secs(1), true, Held::Here, Some(4), None), Step::Wait(secs(59)));
        s.sent(t0, Some(4), Some(secs(86400)));
        assert_eq!(s.next(t0 + secs(1), true, Held::Here, Some(4), None), Step::Wait(secs(3599)));
        // the entry is removed when the listing is not wanted, or the map changed
        assert_eq!(s.next(t0, false, Held::Here, Some(4), None), Step::Remove);
        assert_eq!(s.next(t0, true, Held::Elsewhere, Some(4), None), Step::Remove);
    }

    #[test]
    fn backoff_after_errors() {
        let secs = Duration::from_secs;
        assert_eq!([1, 2, 3, 4, 5, 6, 7, 8, 30].map(|n| backoff(n, 0.0).as_secs()), [30, 60, 120, 240, 480, 960, 1800, 1800, 1800]);
        assert_eq!(backoff(1, 1.0), secs(36));
        let t0 = Instant::now();
        let mut s = Schedule::default();
        assert_eq!(s.failed(t0, None, 0.0), secs(30));
        assert_eq!(s.next(t0 + secs(10), true, Held::None, Some(1), None), Step::Wait(secs(20)));
        assert_eq!(s.next(t0 + secs(30), true, Held::None, Some(1), None), Step::Register);
        assert_eq!(s.failed(t0 + secs(30), None, 0.0), secs(60));
        // backoff also holds a removal back
        assert_eq!(s.next(t0 + secs(31), false, Held::Here, Some(1), None), Step::Wait(secs(59)));
        // the map's Retry-After, within limits
        assert_eq!(s.failed(t0, Some(secs(600)), 0.0), secs(600));
        assert_eq!(s.failed(t0, Some(secs(0)), 0.0), secs(10));
        assert_eq!(s.failed(t0, Some(secs(86400)), 0.0), secs(3600));
        // new settings: at once
        s.retry_now();
        assert_eq!(s.next(t0, true, Held::None, Some(1), None), Step::Register);
        // a success ends the backoff
        s.failed(t0, None, 0.0);
        s.sent(t0, Some(1), None);
        assert_eq!(s.failed(t0, None, 0.0), secs(30));
    }

    #[test]
    fn answers() {
        let busy = Some(Duration::from_secs(120));
        assert_eq!(classify(201, &json!({"id": "a"}), None), Outcome::Ok);
        assert_eq!(classify(204, &Value::Null, None), Outcome::Ok);
        assert_eq!(classify(404, &json!({"error": "unknown_server"}), None), Outcome::Lost);
        assert_eq!(classify(401, &json!({"error": "bad_token"}), None), Outcome::Lost);
        let failed = |status, body: Value, retry| match classify(status, &body, retry) {
            Outcome::Failed(f) => (f.kind, f.detail, f.retry_after),
            o => panic!("{o:?}"),
        };
        // no API there: a proxy's page, a redirect
        assert_eq!(failed(404, Value::Null, None), ("missing", "HTTP 404".into(), None));
        assert_eq!(failed(301, Value::Null, None), ("missing", "HTTP 301".into(), None));
        assert_eq!(
            failed(400, json!({"error": "bad_request", "message": "lat must be between -90 and 90"}), None).1,
            "lat must be between -90 and 90"
        );
        assert_eq!(failed(403, json!({"error": "refused"}), None), ("refused", "refused".into(), None));
        assert_eq!(failed(429, Value::Null, busy), ("busy", "HTTP 429".into(), busy));
        assert_eq!(failed(503, Value::Null, busy).0, "busy");
        assert_eq!(failed(503, Value::Null, None).0, "server");
        assert!(valid_id("k7m2x9qd4p") && valid_id("a-b_C"));
        assert!(!valid_id("") && !valid_id("../x") && !valid_id("a b") && !valid_id(&"a".repeat(65)));
        assert!(valid_token("hY3o0rQe5c1vXbqk2m9TzJ4dW8sL7nAf6gUpRiE0yBw"));
        assert!(!valid_token("short") && !valid_token("with a space in the token") && !valid_token("line\nbreak-in-the-token"));
    }

    #[test]
    fn the_station_list_follows_the_entry() {
        let secs = Duration::from_secs;
        let t0 = Instant::now();
        let mut s = Schedule::default();
        // the entry first
        assert_eq!(s.next(t0, true, Held::Here, Some(1), Some(10)), Step::Update);
        s.sent(t0, Some(1), None);
        // then the list, once it lasted 30 s (and 20 s after the entry)
        assert_eq!(s.next(t0 + secs(10), true, Held::Here, Some(1), Some(10)), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(40), true, Held::Here, Some(1), Some(10)), Step::Stations);
        s.stations_sent(t0 + secs(40), 10);
        assert_eq!(s.next(t0 + secs(41), true, Held::Here, Some(1), Some(10)), Step::Wait(secs(259)));
        // a change of the list goes out once it lasted 30 s...
        assert_eq!(s.next(t0 + secs(100), true, Held::Here, Some(1), Some(11)), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(130), true, Held::Here, Some(1), Some(11)), Step::Stations);
        // ...unless it went back
        assert_eq!(s.next(t0 + secs(130), true, Held::Here, Some(1), Some(10)), Step::Wait(secs(170)));
        // the heartbeat comes first
        assert_eq!(s.next(t0 + secs(300), true, Held::Here, Some(1), Some(11)), Step::Update);
        s.sent(t0 + secs(300), Some(1), None);
        // the map lost the list: ours again; it holds ours: nothing
        s.map_holds(None);
        assert_eq!(s.next(t0 + secs(360), true, Held::Here, Some(1), Some(10)), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(390), true, Held::Here, Some(1), Some(10)), Step::Stations);
        s.map_holds(Some(10));
        assert_eq!(s.next(t0 + secs(390), true, Held::Here, Some(1), Some(10)), Step::Wait(secs(210)));
        // refused: not again until it changes
        s.stations_refused(12);
        assert_eq!(s.next(t0 + secs(400), true, Held::Here, Some(1), Some(12)), Step::Wait(secs(200)));
        assert_eq!(s.next(t0 + secs(400), true, Held::Here, Some(1), Some(13)), Step::Wait(secs(30)));
        // the entry's early update also keeps a minute from the list's request
        s.stations_sent(t0 + secs(430), 13);
        assert_eq!(s.next(t0 + secs(440), true, Held::Here, Some(2), Some(13)), Step::Wait(secs(50)));
        assert_eq!(s.next(t0 + secs(490), true, Held::Here, Some(2), Some(13)), Step::Update);
        // a new entry starts afresh: the list goes to it too
        assert_eq!(Schedule::default().stations_held, None);
        // a map asking for a heartbeat every minute still gets the list
        let mut s = Schedule::default();
        assert_eq!(s.next(t0, true, Held::Here, Some(1), Some(5)), Step::Update);
        s.sent(t0, Some(1), Some(secs(60)));
        assert_eq!(s.next(t0 + secs(10), true, Held::Here, Some(1), Some(5)), Step::Wait(secs(30)));
        assert_eq!(s.next(t0 + secs(40), true, Held::Here, Some(1), Some(5)), Step::Stations);
    }

    #[test]
    fn the_station_list_is_judged_without_the_signal() {
        let s = settings();
        let snap = Snapshot::build(&s, &public(), &list(), false);
        let k = stations_key(&snap);
        let mut l = list();
        for e in &mut l.ensembles {
            e.snr = 1.0;
            e.last_seen += 1000;
            e.tii.clear();
        }
        assert_eq!(stations_key(&Snapshot::build(&s, &public(), &l, false)), k);
        l.ensembles[0].services[0].bitrate = 64;
        assert_ne!(stations_key(&Snapshot::build(&s, &public(), &l, false)), k);
        // the entry does not change with the list, nor the list with a listener
        let mut p = public();
        p.listeners = 9;
        assert_eq!(stations_key(&Snapshot::build(&s, &p, &list(), false)), k);
        assert_eq!(key(&Target::from_settings(&s), &Snapshot::build(&s, &public(), &l, false)), key(&Target::from_settings(&s), &snap));
        assert_eq!(version(0x9f2c4b1e7a3d5c80), "9f2c4b1e7a3d5c80");
        assert_eq!(payload(&Target::from_settings(&s), &snap)["stations"], version(k));
    }

    struct Request {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Value,
        compressed: bool,
    }

    /// A map on this computer: keeps the requests, answers as told (200 `{}`
    /// when not told).
    struct FakeMap {
        url: String,
        requests: Arc<Mutex<Vec<Request>>>,
        replies: Arc<Mutex<std::collections::VecDeque<(u16, String)>>>,
    }

    impl FakeMap {
        fn start() -> FakeMap {
            use std::io::{BufRead, Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests: Arc<Mutex<Vec<Request>>> = Default::default();
            let replies: Arc<Mutex<std::collections::VecDeque<(u16, String)>>> = Default::default();
            let (rq, rp) = (requests.clone(), replies.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = std::io::BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let mut words = line.split_whitespace();
                    let (method, path) = (words.next().unwrap_or("").to_string(), words.next().unwrap_or("").to_string());
                    let mut headers = Vec::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = line.split_once(':') {
                            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                        }
                    }
                    let header = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str());
                    let len: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let compressed = header("content-encoding") == Some("gzip");
                    let mut raw = vec![0u8; len];
                    let _ = reader.read_exact(&mut raw);
                    let mut text = Vec::new();
                    if compressed {
                        let _ = flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut text);
                    } else {
                        text = raw;
                    }
                    let body = serde_json::from_slice(&text).unwrap_or(Value::Null);
                    rq.lock().push(Request { method, path, headers, body, compressed });
                    let (status, reply) = rp.lock().pop_front().unwrap_or((200, "{}".into()));
                    let _ = (&stream).write_all(format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes());
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            FakeMap { url, requests, replies }
        }

        fn reply(&self, status: u16, body: Value) {
            self.replies.lock().push_back((status, body.to_string()));
        }

        fn took(&self) -> Request {
            self.requests.lock().remove(0)
        }
    }

    #[test]
    fn the_station_list_goes_to_the_map() {
        let secs = Duration::from_secs;
        let map = FakeMap::start();
        let mut s = settings();
        s.server_map.url = map.url.clone();
        let target = Target::from_settings(&s);
        assert!(target.listed(), "{:?}", target.problem);
        let mut l = list();
        for e in &mut l.ensembles {
            for i in 1..8u32 {
                e.services.push(ServiceEntry { sid: 0x4A91 + i, label: format!("Radio {i}"), ..e.services[0].clone() });
            }
        }
        let snap = Snapshot::build(&s, &public(), &l, false);
        let (k, stations) = (key(&target, &snap), stations_key(&snap));
        let mut w = Worker { target, ..Default::default() };
        // the registration tells the version of the list; the map holds none
        map.reply(
            201,
            json!({"id": "k7m2x9qd4p", "token": "hY3o0rQe5c1vXbqk2m9TzJ4dW8sL7nAf6gUpRiE0yBw", "heartbeat": 300, "stations": null}),
        );
        w.register(&snap, Some(k));
        let r = map.took();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.compressed), ("POST", "/api/v1/servers", false));
        assert_eq!(r.body["stations"], version(stations));
        assert!(w.entry.is_some());
        let t = Instant::now();
        assert!(matches!(w.schedule.next(t + secs(10), true, Held::Here, Some(k), Some(stations)), Step::Wait(_)));
        assert_eq!(w.schedule.next(t + secs(61), true, Held::Here, Some(k), Some(stations)), Step::Stations);
        // the list: compressed, with the token
        map.reply(200, json!({"id": "k7m2x9qd4p", "stations": version(stations)}));
        w.stations(&snap);
        let r = map.took();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.compressed), ("PUT", "/api/v1/servers/k7m2x9qd4p/stations", true));
        assert!(r.headers.contains(&("authorization".to_string(), "Bearer hY3o0rQe5c1vXbqk2m9TzJ4dW8sL7nAf6gUpRiE0yBw".to_string())));
        assert!(r.headers.iter().any(|(h, v)| h == "user-agent" && v.starts_with("MagicDAB/")));
        assert_eq!(r.body["version"], version(stations));
        let e = &r.body["ensembles"];
        assert_eq!(e.as_array().unwrap().len(), 3);
        assert_eq!(
            (
                e[0]["channel"].as_str(),
                e[0]["eid"].as_str(),
                e[0]["ecc"].as_str(),
                e[0]["label"].as_str(),
                e[0]["source"].as_str(),
                e[0]["lastSeen"].as_i64()
            ),
            (Some("5A"), Some("1A2B"), Some("E2"), Some("MUX 5A"), Some("air"), Some(100))
        );
        assert_eq!(e[0]["services"].as_array().unwrap().len(), 9);
        assert_eq!(
            e[0]["services"][0],
            json!({"sid": "4A91", "ecc": "E2", "label": "Radio", "short": "Radio", "codec": "DAB+", "bitrate": 96, "protection": "EEP 3-A", "pty": 10, "language": 15,
                "subch": 1, "ca": false, "apps": [2, 7], "format": {"codec": "HE-AAC v2", "sampleRate": 48000, "stereo": true}})
        );
        assert_eq!(e[0]["frequency"], 174928);
        assert_eq!(e[0]["subchannels"], json!([{"id": 1, "start": 0, "size": 72, "protection": "EEP 3-A", "bitrate": 96}]));
        assert_eq!(
            e[0]["data"],
            json!([{"sid": "E1C00001", "label": "EPG", "short": "", "subch": 9, "packet": 1, "apps": [7], "ca": false}])
        );
        assert_eq!(e[1]["source"], "file");
        assert!(w.stations_at.is_some());
        assert_eq!(w.schedule.stations_held, Some(stations));
        assert!(matches!(w.schedule.next(t + secs(120), true, Held::Here, Some(k), Some(stations)), Step::Wait(_)));
        // a heartbeat answered without a list: the map lost it, ours again
        map.reply(200, json!({"id": "k7m2x9qd4p", "stations": null}));
        w.update(&snap, Some(k));
        assert_eq!(map.took().path, "/api/v1/servers/k7m2x9qd4p");
        assert_eq!(w.schedule.stations_held, None);
        // refused: given up until the list changes, no backoff, the entry is fine
        map.reply(413, json!({"error": "too_large", "message": "too many stations"}));
        w.stations(&snap);
        map.took();
        assert_eq!(w.schedule.stations_held, Some(stations));
        assert!(w.schedule.retry_at.is_none() && w.error.is_none());
        // the map forgot the entry: registration again, the list to the new one
        map.reply(404, json!({"error": "unknown_server"}));
        w.stations(&snap);
        map.took();
        assert!(w.entry.is_none());
        assert_eq!(w.shared.lock().entry, Some(None));
        assert!(w.stations_at.is_none());
    }

    #[test]
    fn change_events_have_the_eid_and_the_ecc_apart() {
        let ev = ChangeEvent { time: 1, channel: "12A".into(), eid: "E25B7A".into(), ensemble: "MUX TWO".into(), changes: vec![] };
        let v = change_event(&ev).unwrap();
        assert_eq!((v["eid"].as_str(), v["ecc"].as_str()), (Some("5B7A"), Some("E2")));
    }
}
