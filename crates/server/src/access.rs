//! Who may use the receiver: banned addresses and networks (by the admin, or
//! for a while after a flood), refused Tor exit nodes and autonomous systems,
//! the address of a client behind the tunnel, wrong tuning passwords and the
//! channels listeners may tune to.
//!
//! Every HTTP request passes `admit` (main.rs): a refused address gets 403 on
//! any path, so neither the page nor a WebSocket (/ws, /chat). Open connections
//! follow `events` and close themselves when their address is kicked or becomes
//! refused (a new ban, a fresher Tor list, fresher prefixes of a refused AS).
//!
//! A refused AS is turned into the prefixes it announces (RIPEstat), kept in
//! the data folder: every address is checked against them here, with no
//! lookup per address that a flood of new addresses could outrun.
//!
//! The bans live here while the program runs (the WebSocket tasks ban floods by
//! themselves, without the radio thread); the radio thread copies them into
//! the settings.

use crate::config::{AccessConfig, Ban};
use crate::servicelist::now as unix_now;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// WebSocket close codes the interface knows: kicked by an admin (it may
/// connect again by hand), no answer to "are you still listening?"
/// (radio/presence.rs), refused (banned, Tor, a refused network).
pub const CLOSE_KICKED: u16 = 4001;
pub const CLOSE_IDLE: u16 = 4002;
pub const CLOSE_REFUSED: u16 = 4003;

const TOR_LIST_URL: &str = "https://check.torproject.org/torbulkexitlist";
/// The Tor exit list is fetched again after this, sooner after a failure.
const TOR_REFRESH: Duration = Duration::from_secs(3600);
const TOR_RETRY: Duration = Duration::from_secs(600);
/// An older copy from an earlier run is not used (the exits change).
const TOR_CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 86400);
/// The prefixes a refused AS announces (RIPEstat sees the whole BGP table, not
/// only the RIPE region), fetched again after this, sooner after a failure.
const PREFIXES_URL: &str = "https://stat.ripe.net/data/announced-prefixes/data.json";
const PREFIXES_REFRESH: Duration = Duration::from_secs(86400);
const PREFIXES_RETRY: Duration = Duration::from_secs(600);
/// Prefixes of an earlier run older than this are not used.
const PREFIXES_CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 86400);
/// The network of a listener's address, for the admins' list of listeners
/// (Team Cymru DNS, nothing is refused by it): kept for a day, a failed lookup
/// tried again after 10 minutes, at most this many lookups at once.
const ASN_TTL: Duration = Duration::from_secs(86400);
const ASN_RETRY: Duration = Duration::from_secs(600);
const ASN_CACHE_MAX: usize = 20_000;
const ASN_LOOKUPS_MAX: usize = 16;

/// IPv4 clients of a dual-stack socket look like ::ffff:1.2.3.4.
pub fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

/// This machine or a private network (also CGNAT, link-local, unique local):
/// a proxy in front of the program, or a listener at home. Never refused for
/// Tor or its network, never banned automatically.
pub fn is_local(ip: IpAddr) -> bool {
    match normalize(ip) {
        IpAddr::V4(a) => {
            let o = a.octets();
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || (o[0] == 100 && o[1] & 0xC0 == 64)
        }
        IpAddr::V6(a) => {
            let s = a.segments()[0];
            a.is_loopback() || a.is_unspecified() || s & 0xFE00 == 0xFC00 || s & 0xFFC0 == 0xFE80
        }
    }
}

/// "1.2.3.4", "1.2.3.4:5678", "[2001:db8::1]:443", "2001:db8::1".
fn parse_addr(s: &str) -> Option<IpAddr> {
    let s = s.trim().trim_matches('"');
    if let Ok(a) = s.parse::<IpAddr>() {
        return Some(normalize(a));
    }
    if let Ok(a) = s.parse::<std::net::SocketAddr>() {
        return Some(normalize(a.ip()));
    }
    s.strip_prefix('[').and_then(|r| r.split(']').next()).and_then(|a| a.parse().ok()).map(normalize)
}

/// The client's address. Forwarded headers count only when the request came
/// from a proxy (frpc on this machine, a reverse proxy in the network): from
/// the right, the first address that is not a proxy is the client's, so what a
/// client writes into X-Forwarded-For itself (it stays on the left) changes
/// nothing - a banned client cannot come back with a made-up address.
pub fn client_addr<'a>(
    forwarded_for: impl Iterator<Item = &'a str>,
    real_ip: Option<&str>,
    peer: IpAddr,
    is_proxy: impl Fn(IpAddr) -> bool,
) -> IpAddr {
    let peer = normalize(peer);
    if !is_proxy(peer) {
        return peer;
    }
    let chain: Vec<IpAddr> = forwarded_for.flat_map(|h| h.split(',')).filter_map(parse_addr).collect();
    if let Some(a) = chain.iter().rev().find(|a| !is_proxy(**a)) {
        return *a;
    }
    // only proxies and this network: the first one (a listener at home behind a proxy)
    chain.first().copied().or_else(|| real_ip.and_then(parse_addr)).unwrap_or(peer)
}

/// An address or a network (CIDR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(a) => {
            let m = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
            IpAddr::V4((u32::from(a) & m).into())
        }
        IpAddr::V6(a) => {
            let m = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix as u32) };
            IpAddr::V6((u128::from(a) & m).into())
        }
    }
}

/// An address as a number of one space: IPv4 as ::ffff:a.b.c.d.
fn key(ip: IpAddr) -> u128 {
    match normalize(ip) {
        IpAddr::V4(a) => 0xFFFF_0000_0000 | u32::from(a) as u128,
        IpAddr::V6(a) => u128::from(a),
    }
}

/// Networks as sorted address ranges, overlapping and adjacent ones merged
/// (a binary search per address).
fn ranges<'a>(nets: impl Iterator<Item = &'a Net>) -> Vec<(u128, u128)> {
    let mut v: Vec<(u128, u128)> = nets.map(Net::range).collect();
    v.sort_unstable();
    let mut out: Vec<(u128, u128)> = Vec::with_capacity(v.len());
    for (start, end) in v {
        match out.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

fn in_ranges(ranges: &[(u128, u128)], ip: IpAddr) -> bool {
    let k = key(ip);
    let i = ranges.partition_point(|r| r.0 <= k);
    i > 0 && ranges[i - 1].1 >= k
}

impl Net {
    /// The first and the last address, as `key`.
    fn range(&self) -> (u128, u128) {
        let host = match self.addr {
            IpAddr::V4(_) => u32::MAX.checked_shr(self.prefix as u32).unwrap_or(0) as u128,
            IpAddr::V6(_) => u128::MAX.checked_shr(self.prefix as u32).unwrap_or(0),
        };
        let start = key(self.addr);
        (start, start | host)
    }

    /// "203.0.113.7", "203.0.113.0/24", "2001:db8::/48" (host bits are dropped).
    pub fn parse(s: &str) -> Option<Net> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim().parse::<u8>().ok()?)),
            None => (s, None),
        };
        let raw = a.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok()?;
        let addr = normalize(raw);
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            // ::ffff:1.2.3.0/120 is 1.2.3.0/24
            Some(p) if raw.is_ipv6() && addr.is_ipv4() => p.checked_sub(96)?,
            Some(p) => p,
            None => max,
        };
        (prefix <= max).then(|| Net { addr: mask(addr, prefix), prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        ip.is_ipv4() == self.addr.is_ipv4() && mask(ip, self.prefix) == self.addr
    }
}

impl std::fmt::Display for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let max = if self.addr.is_ipv4() { 32 } else { 128 };
        if self.prefix == max {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix)
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Close the connections of this address (kicked by an admin).
    Kick(IpAddr),
    /// Bans or refused networks changed: connections check their address again.
    Recheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Banned,
    Tor,
    Network,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Banned => "banned",
            Refusal::Tor => "Tor exit node",
            Refusal::Network => "refused network",
        }
    }
}

/// The network of an address, as Team Cymru knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AsnInfo {
    pub asns: Vec<u32>,
    pub prefix: String,
    pub country: String,
}

/// DNS name of the Team Cymru origin lookup of an address.
pub fn cymru_name(ip: IpAddr) -> String {
    match normalize(ip) {
        IpAddr::V4(a) => {
            let o = a.octets();
            format!("{}.{}.{}.{}.origin.asn.cymru.com", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(a) => {
            let mut s = String::with_capacity(96);
            for b in a.octets().iter().rev() {
                s.push_str(&format!("{:x}.{:x}.", b & 0xF, b >> 4));
            }
            s + "origin6.asn.cymru.com"
        }
    }
}

/// Team Cymru answers "13335 | 1.1.1.0/24 | AU | apnic | 2011-08-11", one record
/// per announced prefix, several origins separated by spaces.
pub fn parse_cymru(records: &[String]) -> AsnInfo {
    let mut info = AsnInfo::default();
    for r in records {
        let fields: Vec<&str> = r.split('|').map(str::trim).collect();
        for asn in fields[0].split_whitespace().filter_map(|x| x.parse::<u32>().ok()) {
            if !info.asns.contains(&asn) {
                info.asns.push(asn);
            }
        }
        if info.prefix.is_empty() {
            info.prefix = fields.get(1).unwrap_or(&"").to_string();
        }
        if info.country.is_empty() {
            info.country = fields.get(2).unwrap_or(&"").to_string();
        }
    }
    info
}

/// An AS number of the settings: 13335, "13335" or "AS13335".
pub fn parse_asn(v: &serde_json::Value) -> Option<u32> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_u64()?,
        serde_json::Value::String(s) => {
            let s = s.trim();
            let s = s.strip_prefix("AS").or_else(|| s.strip_prefix("as")).or_else(|| s.strip_prefix("As")).unwrap_or(s);
            s.trim().parse::<u64>().ok()?
        }
        _ => return None,
    };
    u32::try_from(n).ok().filter(|&n| n > 0)
}

/// The prefixes of an answer of RIPEstat's announced-prefixes
/// (`{"data": {"prefixes": [{"prefix": "203.0.113.0/24"}, ...]}}`).
pub fn parse_announced(v: &serde_json::Value) -> Option<Vec<Net>> {
    let list = v.get("data")?.get("prefixes")?.as_array()?;
    Some(list.iter().filter_map(|p| p.get("prefix")?.as_str()).filter(|p| p.contains('/')).filter_map(Net::parse).collect())
}

/// Addresses of the Tor bulk exit list (one per line).
pub fn parse_tor_list(text: &str) -> HashSet<IpAddr> {
    text.lines().map(str::trim).filter(|l| !l.starts_with('#')).filter_map(|l| l.parse::<IpAddr>().ok()).map(normalize).collect()
}

/// A listener may tune to the channel (`allowed` empty: to every one).
pub fn channel_allowed(allowed: &[String], channel: &str) -> bool {
    allowed.is_empty() || allowed.iter().any(|c| c.eq_ignore_ascii_case(channel))
}

/// Channels of the settings: known ones, written as in the band plan, once each.
pub fn clean_channels<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if let Some((name, _)) = magicdab_dab::consts::BAND_III.iter().find(|(c, _)| c.eq_ignore_ascii_case(n.trim())) {
            if !out.iter().any(|x| x == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Wrong passwords per address and of everyone together: guessing stops for a
/// while (forwarded addresses can be made up, so everyone counts too).
pub struct Attempts {
    tries: u32,
    tries_all: usize,
    lock: Duration,
    per_ip: HashMap<String, (u32, Instant)>,
    all: VecDeque<Instant>,
}

impl Attempts {
    pub fn new(tries: u32, tries_all: usize, lock: Duration) -> Self {
        Attempts { tries, tries_all, lock, per_ip: HashMap::new(), all: VecDeque::new() }
    }

    pub fn locked(&mut self, ip: &str, now: Instant) -> bool {
        let lock = self.lock;
        self.per_ip.retain(|_, (_, first)| now.duration_since(*first) < lock);
        while self.all.front().is_some_and(|t| now.duration_since(*t) >= lock) {
            self.all.pop_front();
        }
        self.per_ip.get(ip).is_some_and(|(n, _)| *n >= self.tries) || self.all.len() >= self.tries_all
    }

    /// A wrong password: the count of the address and of everyone.
    pub fn failed(&mut self, ip: &str, now: Instant) -> (u32, usize) {
        let e = self.per_ip.entry(ip.to_string()).or_insert((0, now));
        e.0 += 1;
        self.all.push_back(now);
        (e.0, self.all.len())
    }

    pub fn succeeded(&mut self, ip: &str) {
        self.per_ip.remove(ip);
    }
}

#[derive(Default)]
struct Rules {
    bans: Vec<(Net, Ban)>,
    block_tor: bool,
    blocked_asns: Vec<u32>,
    auto_ban_hours: u32,
}

/// The address space of the refused ASes.
#[derive(Default)]
struct Networks {
    /// AS: when its prefixes were fetched (Unix time), and the prefixes.
    asns: HashMap<u32, (i64, Vec<Net>)>,
    /// All of them, for `in_ranges`.
    ranges: Vec<(u128, u128)>,
    /// When the fetch of an AS failed last.
    failed: HashMap<u32, Instant>,
    error: Option<String>,
}

#[derive(Default)]
struct TorExits {
    addrs: HashSet<IpAddr>,
    /// Unix time of the list.
    updated: Option<i64>,
    tried: Option<Instant>,
    error: Option<String>,
}

pub struct Access {
    rules: RwLock<Rules>,
    /// Bans changed outside the radio thread (floods, ends): to be saved.
    changed: AtomicBool,
    tor: RwLock<TorExits>,
    networks: RwLock<Networks>,
    /// The settings were loaded: the refused ASes are known.
    loaded: AtomicBool,
    /// Network of each address looked up (None: the lookup failed), and when.
    asn: Mutex<HashMap<IpAddr, (Instant, Option<AsnInfo>)>>,
    pending: Mutex<HashSet<IpAddr>>,
    /// Addresses of the tunnel server: a proxy in forwarded headers, not a client.
    tunnel_server: RwLock<Vec<IpAddr>>,
    /// When a refusal of an address was logged.
    refused_logged: Mutex<HashMap<IpAddr, Instant>>,
    events: broadcast::Sender<Event>,
    wake: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
}

/// The rules of this program (one receiver, one set of rules).
pub fn get() -> &'static Access {
    static ACCESS: OnceLock<Access> = OnceLock::new();
    ACCESS.get_or_init(Access::new)
}

impl Access {
    fn new() -> Self {
        Access {
            rules: RwLock::new(Rules { auto_ban_hours: AccessConfig::default().auto_ban_hours, ..Default::default() }),
            changed: AtomicBool::new(false),
            tor: RwLock::new(TorExits::default()),
            networks: RwLock::new(Networks::default()),
            loaded: AtomicBool::new(false),
            asn: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashSet::new()),
            tunnel_server: RwLock::new(Vec::new()),
            refused_logged: Mutex::new(HashMap::new()),
            events: broadcast::channel(64).0,
            wake: crossbeam_channel::bounded(1),
        }
    }

    /// The bans and rules of the settings (at the start).
    pub fn load(&self, cfg: &AccessConfig) {
        self.rules.write().bans = cfg.bans.iter().filter_map(|b| Some((Net::parse(&b.address)?, b.clone()))).collect();
        self.loaded.store(true, Ordering::Relaxed);
        self.configure(cfg);
    }

    /// The rules of the settings but the bans (they are kept here).
    pub fn configure(&self, cfg: &AccessConfig) {
        {
            let mut r = self.rules.write();
            r.block_tor = cfg.block_tor;
            r.blocked_asns = cfg.blocked_asns.clone();
            r.auto_ban_hours = cfg.auto_ban_hours;
        }
        // (the Tor list and the prefixes of new ASes are fetched at once)
        let _ = self.wake.0.try_send(());
        self.recheck();
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Open connections check their address again.
    pub fn recheck(&self) {
        let _ = self.events.send(Event::Recheck);
    }

    /// Close the connections of an address (it may connect again).
    pub fn kick(&self, ip: IpAddr) {
        let _ = self.events.send(Event::Kick(normalize(ip)));
    }

    pub fn bans(&self) -> Vec<Ban> {
        self.rules.read().bans.iter().map(|(_, b)| b.clone()).collect()
    }

    /// Ban an address or a network (an earlier ban of it is replaced); its
    /// connections are closed. The address is written in its usual form.
    pub fn ban(&self, mut ban: Ban) -> Option<Ban> {
        let net = Net::parse(&ban.address)?;
        ban.address = net.to_string();
        {
            let mut r = self.rules.write();
            r.bans.retain(|(n, _)| *n != net);
            r.bans.push((net, ban.clone()));
        }
        self.changed.store(true, Ordering::Relaxed);
        self.recheck();
        Some(ban)
    }

    pub fn unban(&self, address: &str) -> bool {
        let Some(net) = Net::parse(address) else { return false };
        let removed = {
            let mut r = self.rules.write();
            let n = r.bans.len();
            r.bans.retain(|(x, _)| *x != net);
            r.bans.len() != n
        };
        if removed {
            self.changed.store(true, Ordering::Relaxed);
        }
        removed
    }

    /// Ban a flooding client for a while (IPv6: its /64, one subscriber, inside
    /// which it could take a new address). Not one of this network: there it is
    /// more likely a mistake than an attack, and the admin would be locked out.
    pub fn auto_ban(&self, ip: IpAddr, reason: &str) -> bool {
        let ip = normalize(ip);
        let hours = self.rules.read().auto_ban_hours;
        if hours == 0 || is_local(ip) || self.refusal(ip) == Some(Refusal::Banned) {
            return false;
        }
        let prefix = if ip.is_ipv4() { 32 } else { 64 };
        let net = Net { addr: mask(ip, prefix), prefix };
        let now = unix_now();
        log::warn!("{net} banned for {hours} h: {reason}");
        let ban =
            Ban { address: net.to_string(), reason: reason.to_string(), since: now, until: Some(now + hours as i64 * 3600), auto: true };
        self.ban(ban).is_some()
    }

    /// A refused request is worth a log line now and then (a refused browser
    /// keeps trying).
    pub fn log_refusal(&self, ip: IpAddr, refusal: Refusal) {
        let mut logged = self.refused_logged.lock();
        if logged.get(&ip).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            return;
        }
        if logged.len() > 1000 {
            logged.retain(|_, t| t.elapsed() < Duration::from_secs(600));
        }
        logged.insert(ip, Instant::now());
        log::info!("{ip} refused: {}", refusal.reason());
    }

    /// Bans that ended are forgotten.
    fn prune(&self, now: i64) {
        let mut r = self.rules.write();
        let n = r.bans.len();
        r.bans.retain(|(_, b)| {
            if !b.active(now) {
                log::info!("the ban of {} ended", b.address);
            }
            b.active(now)
        });
        if r.bans.len() != n {
            self.changed.store(true, Ordering::Relaxed);
        }
    }

    /// The bans changed since the last call (to be saved in the settings).
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }

    /// Why an address is refused, by what is known now: bans, the Tor list and
    /// the prefixes of the refused ASes.
    pub fn refusal(&self, ip: IpAddr) -> Option<Refusal> {
        let ip = normalize(ip);
        // (the program's own window, frpc)
        if ip.is_loopback() {
            return None;
        }
        let now = unix_now();
        let r = self.rules.read();
        if r.bans.iter().any(|(n, b)| b.active(now) && n.contains(ip)) {
            return Some(Refusal::Banned);
        }
        if is_local(ip) {
            return None;
        }
        if r.block_tor && self.tor.read().addrs.contains(&ip) {
            return Some(Refusal::Tor);
        }
        if !r.blocked_asns.is_empty() && in_ranges(&self.networks.read().ranges, ip) {
            return Some(Refusal::Network);
        }
        None
    }

    /// What an open connection of `ip` does on an event of `events`: close with
    /// this code and reason, or go on.
    pub fn on_event(&self, ev: Result<Event, broadcast::error::RecvError>, ip: IpAddr) -> Option<(u16, &'static str)> {
        match ev {
            Ok(Event::Kick(a)) => (a == normalize(ip)).then_some((CLOSE_KICKED, "kicked by an admin")),
            // (missed events: checked anyway)
            Ok(Event::Recheck) | Err(broadcast::error::RecvError::Lagged(_)) => self.refusal(ip).map(|r| (CLOSE_REFUSED, r.reason())),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// The network of an address, if looked up already.
    pub fn asn_of(&self, ip: IpAddr) -> Option<AsnInfo> {
        self.asn.lock().get(&normalize(ip)).and_then(|(_, i)| i.clone())
    }

    fn asn_fresh(&self, ip: IpAddr) -> bool {
        match self.asn.lock().get(&ip) {
            Some((at, Some(_))) => at.elapsed() < ASN_TTL,
            Some((at, None)) => at.elapsed() < ASN_RETRY,
            None => false,
        }
    }

    /// Look up the network of a public address in the background, for the
    /// next list of listeners (one lookup per address at a time; none while
    /// too many run).
    pub fn lookup(&'static self, ip: IpAddr) {
        let ip = normalize(ip);
        if is_local(ip) || self.asn_fresh(ip) {
            return;
        }
        {
            let mut pending = self.pending.lock();
            if pending.len() >= ASN_LOOKUPS_MAX || !pending.insert(ip) {
                return;
            }
        }
        // (a thread: the radio thread asks, outside of the async runtime)
        let spawned = std::thread::Builder::new().name("asn".into()).spawn(move || {
            let info = crate::radiodns::txt(&cymru_name(ip)).map(|r| parse_cymru(&r));
            if info.is_none() {
                log::debug!("AS lookup of {ip} failed");
            }
            {
                let mut cache = self.asn.lock();
                if cache.len() >= ASN_CACHE_MAX {
                    cache.retain(|_, (at, _)| at.elapsed() < ASN_RETRY);
                    if cache.len() >= ASN_CACHE_MAX {
                        cache.clear();
                    }
                }
                cache.insert(ip, (Instant::now(), info));
            }
            self.pending.lock().remove(&ip);
        });
        if spawned.is_err() {
            self.pending.lock().remove(&ip);
        }
    }

    /// The tunnel server is a proxy in front of frpc: its address in forwarded
    /// headers is not a client's (resolved in the background; "" when off).
    pub fn set_tunnel_host(&'static self, host: &str) {
        let host = host.trim().to_string();
        if host.is_empty() {
            self.tunnel_server.write().clear();
            return;
        }
        let _ = std::thread::Builder::new().name("tunnel-dns".into()).spawn(move || {
            let addrs: Vec<IpAddr> =
                (host.as_str(), 0).to_socket_addrs().map(|a| a.map(|s| normalize(s.ip())).collect()).unwrap_or_default();
            if !addrs.is_empty() {
                *self.tunnel_server.write() = addrs;
            }
        });
    }

    /// A proxy in front of the program: this network or the tunnel server.
    pub fn is_proxy(&self, ip: IpAddr) -> bool {
        is_local(ip) || self.tunnel_server.read().contains(&normalize(ip))
    }

    /// The Tor exit list for the settings: addresses, when, the last error.
    pub fn tor_status(&self) -> serde_json::Value {
        let t = self.tor.read();
        serde_json::json!({"count": t.addrs.len(), "updated": t.updated, "error": t.error})
    }

    /// The prefixes of the refused ASes for the settings: ASes, prefixes, the
    /// oldest fetch, the last error.
    pub fn networks_status(&self) -> serde_json::Value {
        let n = self.networks.read();
        let prefixes: usize = n.asns.values().map(|(_, p)| p.len()).sum();
        let updated = n.asns.values().map(|(t, _)| *t).min();
        serde_json::json!({"asns": n.asns.len(), "prefixes": prefixes, "updated": updated, "error": n.error})
    }

    /// Fetch the prefixes of the refused ASes that are missing or a day old (a
    /// failed one again after 10 minutes), forget those of ASes no longer
    /// refused, and keep them in the data folder.
    fn refresh_networks(&self, cache: &Path) {
        if !self.loaded.load(Ordering::Relaxed) {
            return;
        }
        let wanted = self.rules.read().blocked_asns.clone();
        let now = unix_now();
        let mut changed = false;
        let due: Vec<u32> = {
            let mut n = self.networks.write();
            let before = n.asns.len();
            n.asns.retain(|a, _| wanted.contains(a));
            n.failed.retain(|a, _| wanted.contains(a));
            changed |= n.asns.len() != before;
            if wanted.is_empty() {
                n.error = None;
            }
            wanted
                .iter()
                .copied()
                .filter(|a| {
                    let fresh = n.asns.get(a).is_some_and(|(t, _)| now - t < PREFIXES_REFRESH.as_secs() as i64);
                    !fresh && !n.failed.get(a).is_some_and(|t| t.elapsed() < PREFIXES_RETRY)
                })
                .collect()
        };
        for asn in due {
            match fetch_prefixes(asn) {
                Ok(nets) => {
                    let mut n = self.networks.write();
                    let old = n.asns.get(&asn).map(|(_, p)| p.len());
                    if old != Some(nets.len()) {
                        log::info!("AS{asn}: {} prefixes refused", nets.len());
                    }
                    n.asns.insert(asn, (unix_now(), nets));
                    n.failed.remove(&asn);
                    n.error = None;
                    changed = true;
                }
                Err(e) => {
                    // (the last prefixes of the AS stay)
                    let e = format!("AS{asn}: {e}");
                    let mut n = self.networks.write();
                    if n.error.as_deref() != Some(e.as_str()) {
                        log::warn!("prefixes of {e}");
                    }
                    n.failed.insert(asn, Instant::now());
                    n.error = Some(e);
                }
            }
        }
        if changed {
            let text = {
                let mut n = self.networks.write();
                let r = ranges(n.asns.values().flat_map(|(_, p)| p.iter()));
                n.ranges = r;
                let saved: serde_json::Map<String, serde_json::Value> = n
                    .asns
                    .iter()
                    .map(|(a, (t, p))| {
                        let p: Vec<String> = p.iter().map(Net::to_string).collect();
                        (a.to_string(), serde_json::json!({"updated": t, "prefixes": p}))
                    })
                    .collect();
                serde_json::Value::Object(saved).to_string()
            };
            let _ = std::fs::write(cache, text);
            self.recheck();
        }
    }

    /// The prefixes of an earlier run, those not older than a week.
    fn load_networks(&self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(serde_json::Value::Object(saved)) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let now = unix_now();
        let mut n = self.networks.write();
        for (a, v) in saved {
            let (Ok(asn), Some(t)) = (a.parse::<u32>(), v["updated"].as_i64()) else { continue };
            if now - t > PREFIXES_CACHE_MAX_AGE.as_secs() as i64 {
                continue;
            }
            let nets = v["prefixes"].as_array().map(|p| p.iter().filter_map(|x| Net::parse(x.as_str()?)).collect()).unwrap_or_default();
            n.asns.insert(asn, (t, nets));
        }
        let r = ranges(n.asns.values().flat_map(|(_, p)| p.iter()));
        n.ranges = r;
    }

    fn tor_due(&self) -> bool {
        if !self.rules.read().block_tor {
            return false;
        }
        let t = self.tor.read();
        let fresh = t.updated.is_some_and(|u| unix_now() - u < TOR_REFRESH.as_secs() as i64);
        !fresh && !t.tried.is_some_and(|i| i.elapsed() < TOR_RETRY)
    }

    fn refresh_tor(&self, cache: &Path) {
        self.tor.write().tried = Some(Instant::now());
        match fetch_tor() {
            Ok((addrs, text)) => {
                let first = self.tor.read().updated.is_none();
                let n = addrs.len();
                {
                    let mut t = self.tor.write();
                    t.addrs = addrs;
                    t.updated = Some(unix_now());
                    t.error = None;
                }
                let _ = std::fs::write(cache, text);
                if first {
                    log::info!("Tor exit list: {n} addresses");
                } else {
                    log::debug!("Tor exit list: {n} addresses");
                }
                self.recheck();
            }
            Err(e) => {
                // (the last list stays; without one nobody is refused for Tor)
                let again = self.tor.read().error.as_deref() == Some(e.as_str());
                if !again {
                    log::warn!("Tor exit list not available: {e}");
                }
                self.tor.write().error = Some(e);
            }
        }
    }

    /// The copy of an earlier run, unless it is old.
    fn load_tor_cache(&self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).unwrap_or(Duration::MAX);
        if age > TOR_CACHE_MAX_AGE {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let mut t = self.tor.write();
        t.addrs = parse_tor_list(&text);
        t.updated = Some(unix_now() - age.as_secs() as i64);
    }
}

/// The list and its text (for the copy in the data folder).
fn fetch_tor() -> Result<(HashSet<IpAddr>, String), String> {
    // (no request may hang the upkeep: a server that accepts but never answers)
    let mut resp = ureq::get(TOR_LIST_URL)
        .header("User-Agent", "MagicDAB")
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let data = resp.body_mut().with_config().limit(8 << 20).read_to_vec().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&data).to_string();
    let addrs = parse_tor_list(&text);
    // (an error page of a proxy is not a list)
    if addrs.len() < 50 {
        return Err(format!("{} addresses only", addrs.len()));
    }
    Ok((addrs, text))
}

/// The prefixes of an AS from RIPEstat.
fn fetch_prefixes(asn: u32) -> Result<Vec<Net>, String> {
    let url = format!("{PREFIXES_URL}?resource=AS{asn}&sourceapp=magicdab");
    let mut resp = ureq::get(&url)
        .header("User-Agent", "MagicDAB")
        .config()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let data = resp.body_mut().with_config().limit(32 << 20).read_to_vec().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    parse_announced(&v).ok_or_else(|| "no prefix list in the answer".to_string())
}

/// Whether a new request may come in.
pub fn admit(ip: IpAddr) -> Option<Refusal> {
    get().refusal(ip)
}

/// The upkeep: the Tor exit list while Tor is refused (fetched again every
/// hour) and the prefixes of the refused ASes (every day), both kept in the
/// data folder for the next start; ended bans, old lookups.
pub fn start(data_dir: &Path) {
    let a = get();
    let cache = data_dir.join("tor-exits.txt");
    let networks = data_dir.join("as-prefixes.json");
    a.load_tor_cache(&cache);
    a.load_networks(&networks);
    let _ = std::thread::Builder::new().name("access".into()).spawn(move || loop {
        if a.tor_due() {
            a.refresh_tor(&cache);
        }
        a.refresh_networks(&networks);
        a.prune(unix_now());
        a.asn.lock().retain(|_, (at, _)| at.elapsed() < ASN_TTL);
        let _ = a.wake.1.recv_timeout(Duration::from_secs(60));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn networks() {
        let n = Net::parse(" 203.0.113.77/24 ").unwrap();
        assert_eq!(n.to_string(), "203.0.113.0/24");
        assert!(n.contains(ip("203.0.113.1")) && n.contains(ip("::ffff:203.0.113.200")));
        assert!(!n.contains(ip("203.0.114.1")) && !n.contains(ip("2001:db8::1")));
        let h = Net::parse("198.51.100.7").unwrap();
        assert_eq!(h.to_string(), "198.51.100.7");
        assert!(h.contains(ip("198.51.100.7")) && !h.contains(ip("198.51.100.8")));
        let v6 = Net::parse("2001:db8:aa::1/48").unwrap();
        assert_eq!(v6.to_string(), "2001:db8:aa::/48");
        assert!(v6.contains(ip("2001:db8:aa:ffff::5")) && !v6.contains(ip("2001:db8:ab::1")));
        assert_eq!(Net::parse("[2001:db8::1]").unwrap().to_string(), "2001:db8::1");
        assert_eq!(Net::parse("::ffff:192.0.2.1").unwrap().to_string(), "192.0.2.1");
        assert_eq!(Net::parse("::ffff:192.0.2.0/120").unwrap().to_string(), "192.0.2.0/24");
        assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        for bad in ["", "1.2.3", "1.2.3.4/33", "2001:db8::/129", "host.example", "1.2.3.4/x", "::ffff:1.2.3.4/90"] {
            assert!(Net::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn local_addresses() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.5",
            "100.64.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.0.1",
        ] {
            assert!(is_local(ip(a)), "{a}");
        }
        for a in ["8.8.8.8", "100.128.0.1", "172.32.0.1", "2a00:1450::1", "198.51.100.7"] {
            assert!(!is_local(ip(a)), "{a}");
        }
    }

    #[test]
    fn address_behind_proxies() {
        let proxy = |a: IpAddr| is_local(a) || a == ip("198.51.100.7");
        let lo = ip("127.0.0.1");
        let addr = |xff: &[&str], real: Option<&str>, peer: IpAddr| client_addr(xff.iter().copied(), real, peer, proxy);
        // frps adds the address it saw, the TLS proxy in front of it its own
        assert_eq!(addr(&["203.0.113.5, 127.0.0.1"], None, lo), ip("203.0.113.5"));
        assert_eq!(addr(&["203.0.113.5, 198.51.100.7"], None, lo), ip("203.0.113.5"));
        // a made-up address in front of the real one changes nothing
        assert_eq!(addr(&["1.1.1.1, 203.0.113.5, 127.0.0.1"], None, lo), ip("203.0.113.5"));
        assert_eq!(addr(&["1.1.1.1", "203.0.113.5"], None, lo), ip("203.0.113.5"));
        assert_eq!(addr(&["junk, 203.0.113.5:443"], None, lo), ip("203.0.113.5"));
        assert_eq!(addr(&["[2001:db8::7]:5000, ::ffff:127.0.0.1"], None, lo), ip("2001:db8::7"));
        // from the internet without a proxy: the headers are the client's own
        assert_eq!(addr(&["1.1.1.1"], Some("1.1.1.1"), ip("198.51.100.9")), ip("198.51.100.9"));
        // at home: directly, or through a proxy of the network
        assert_eq!(addr(&[], None, ip("192.168.1.20")), ip("192.168.1.20"));
        assert_eq!(addr(&["192.168.1.20, 172.17.0.1"], None, ip("172.17.0.2")), ip("192.168.1.20"));
        assert_eq!(addr(&[], Some("203.0.113.8"), lo), ip("203.0.113.8"));
        assert_eq!(addr(&[], None, ip("::ffff:127.0.0.1")), lo);
    }

    #[test]
    fn bans_and_their_end() {
        let a = Access::new();
        let now = unix_now();
        a.load(&AccessConfig {
            bans: vec![
                Ban { address: "198.51.100.0/24".into(), since: now, ..Default::default() },
                Ban { address: "203.0.113.9".into(), until: Some(now - 1), auto: true, ..Default::default() },
                Ban { address: "not an address".into(), ..Default::default() },
            ],
            ..Default::default()
        });
        assert_eq!(a.refusal(ip("198.51.100.200")), Some(Refusal::Banned));
        assert_eq!(a.refusal(ip("::ffff:198.51.100.1")), Some(Refusal::Banned));
        // ended: let in, and forgotten by the upkeep
        assert_eq!(a.refusal(ip("203.0.113.9")), None);
        assert_eq!(a.bans().len(), 2);
        let _ = a.take_changed();
        a.prune(now);
        assert_eq!(a.bans().len(), 1);
        assert!(a.take_changed() && !a.take_changed());
        // a ban replaces an earlier one of the same network
        let b = a.ban(Ban { address: "198.51.100.33/24".into(), reason: "again".into(), ..Default::default() }).unwrap();
        assert_eq!((b.address.as_str(), a.bans().len()), ("198.51.100.0/24", 1));
        assert!(a.ban(Ban { address: "nonsense".into(), ..Default::default() }).is_none());
        // this machine is never refused (the program's own window, frpc)
        a.ban(Ban { address: "0.0.0.0/0".into(), ..Default::default() }).unwrap();
        assert_eq!(a.refusal(ip("127.0.0.1")), None);
        assert_eq!(a.refusal(ip("192.168.0.2")), Some(Refusal::Banned));
        assert!(a.unban("0.0.0.0/0") && !a.unban("0.0.0.0/0"));
        assert!(a.unban(" 198.51.100.0/24"));
        assert!(a.bans().is_empty());
    }

    #[test]
    fn automatic_bans() {
        let a = Access::new();
        a.load(&AccessConfig::default());
        assert!(a.auto_ban(ip("203.0.113.50"), "flood"));
        let b = &a.bans()[0];
        assert!(b.auto && b.until.unwrap() - b.since == 24 * 3600 && b.reason == "flood");
        assert_eq!(a.refusal(ip("203.0.113.50")), Some(Refusal::Banned));
        // banned already, at home, switched off
        assert!(!a.auto_ban(ip("203.0.113.50"), "flood"));
        assert!(!a.auto_ban(ip("192.168.1.2"), "flood"));
        // IPv6: the /64 of the client
        assert!(a.auto_ban(ip("2001:db8:5:6:a:b:c:d"), "flood"));
        assert_eq!(a.bans()[1].address, "2001:db8:5:6::/64");
        assert_eq!(a.refusal(ip("2001:db8:5:6::99")), Some(Refusal::Banned));
        assert_eq!(a.refusal(ip("2001:db8:5:7::1")), None);
        a.configure(&AccessConfig { auto_ban_hours: 0, ..Default::default() });
        assert!(!a.auto_ban(ip("203.0.113.51"), "flood"));
        assert_eq!(a.bans().len(), 2);
    }

    #[test]
    fn tor_and_networks() {
        let a = Access::new();
        a.tor.write().addrs = parse_tor_list("# list\n185.220.101.1\n 2001:db8::77 \nbad line\n");
        assert_eq!(a.tor.read().addrs.len(), 2);
        assert_eq!(a.refusal(ip("185.220.101.1")), None);
        a.configure(&AccessConfig { block_tor: true, blocked_asns: vec![64500], ..Default::default() });
        assert_eq!(a.refusal(ip("185.220.101.1")), Some(Refusal::Tor));
        assert_eq!(a.refusal(ip("2001:db8::77")), Some(Refusal::Tor));
        // the prefixes of the refused AS
        let nets = parse_announced(&serde_json::json!({"status": "ok", "data": {"prefixes": [
            {"prefix": "203.0.113.0/25", "timelines": []}, {"prefix": "2001:db8:40::/44"}, {"prefix": "bad"}, {"prefix": "198.51.100.7"}
        ]}}))
        .unwrap();
        assert_eq!(nets.len(), 2);
        assert!(parse_announced(&serde_json::json!({"status": "error"})).is_none());
        {
            let mut n = a.networks.write();
            n.ranges = ranges(nets.iter());
            n.asns.insert(64500, (unix_now(), nets));
        }
        assert_eq!(a.refusal(ip("203.0.113.1")), Some(Refusal::Network));
        assert_eq!(a.refusal(ip("::ffff:203.0.113.127")), Some(Refusal::Network));
        assert_eq!(a.refusal(ip("2001:db8:4f:1::1")), Some(Refusal::Network));
        assert_eq!(a.refusal(ip("203.0.113.200")), None);
        assert_eq!(a.refusal(ip("2001:db8:50::1")), None);
        // events of open connections
        assert_eq!(a.on_event(Ok(Event::Recheck), ip("203.0.113.1")).map(|c| c.0), Some(CLOSE_REFUSED));
        assert_eq!(a.on_event(Ok(Event::Recheck), ip("203.0.113.200")), None);
        assert_eq!(a.on_event(Ok(Event::Kick(ip("203.0.113.2"))), ip("::ffff:203.0.113.2")).map(|c| c.0), Some(CLOSE_KICKED));
        assert_eq!(a.on_event(Ok(Event::Kick(ip("203.0.113.9"))), ip("203.0.113.2")), None);
        // switched off: nothing refused
        a.configure(&AccessConfig::default());
        assert_eq!(a.refusal(ip("185.220.101.1")), None);
        assert_eq!(a.refusal(ip("203.0.113.1")), None);
    }

    #[test]
    fn address_ranges() {
        let nets: Vec<Net> = ["192.0.2.0/25", "192.0.2.128/25", "192.0.2.64/26", "198.51.100.9", "0.0.0.0/0", "2001:db8::/32"]
            .iter()
            .map(|n| Net::parse(n).unwrap())
            .collect();
        let r = ranges(nets[..4].iter());
        // the two halves and the quarter inside: one range
        assert_eq!(r.len(), 2);
        assert!(in_ranges(&r, ip("192.0.2.0")) && in_ranges(&r, ip("192.0.2.255")) && in_ranges(&r, ip("198.51.100.9")));
        assert!(!in_ranges(&r, ip("192.0.3.0")) && !in_ranges(&r, ip("198.51.100.8")) && !in_ranges(&r, ip("2001:db8::1")));
        assert!(!in_ranges(&[], ip("192.0.2.1")));
        let all = ranges(nets[4..].iter());
        assert!(in_ranges(&all, ip("255.255.255.255")) && in_ranges(&all, ip("2001:db8:ffff::1")) && !in_ranges(&all, ip("2001:db9::1")));
    }

    #[test]
    fn cymru_lookups() {
        assert_eq!(cymru_name(ip("1.2.3.4")), "4.3.2.1.origin.asn.cymru.com");
        assert_eq!(cymru_name(ip("::ffff:1.2.3.4")), "4.3.2.1.origin.asn.cymru.com");
        let n = cymru_name(ip("2001:db8::1"));
        assert!(n.starts_with("1.0.0.0.0.0.0.0.") && n.ends_with(".8.b.d.0.1.0.0.2.origin6.asn.cymru.com"), "{n}");
        assert_eq!(n.split('.').count(), 32 + 4);
        let info = parse_cymru(&["13335 | 1.1.1.0/24 | AU | apnic | 2011-08-11".to_string()]);
        assert_eq!(info, AsnInfo { asns: vec![13335], prefix: "1.1.1.0/24".into(), country: "AU".into() });
        let info = parse_cymru(&["4808 4837 | 1.2.4.0/24 | CN | apnic |".to_string(), "4837 | 1.2.0.0/16 | CN | apnic |".to_string()]);
        assert_eq!(info.asns, vec![4808, 4837]);
        assert_eq!(info.prefix, "1.2.4.0/24");
        assert_eq!(parse_cymru(&[]), AsnInfo::default());
        assert_eq!(parse_cymru(&["garbage".to_string()]).asns, Vec::<u32>::new());
        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(parse_asn(&v("13335")), Some(13335));
        assert_eq!(parse_asn(&v("\" AS 13335\"")), Some(13335));
        assert_eq!(parse_asn(&v("\"as8075\"")), Some(8075));
        for bad in ["0", "-5", "\"ASx\"", "4294967296", "null", "1.5"] {
            assert_eq!(parse_asn(&v(bad)), None, "{bad}");
        }
    }

    #[test]
    fn channels_of_the_listeners() {
        assert!(channel_allowed(&[], "12C"));
        let allowed = clean_channels(["5a", "12c", "12C", "13X", " 9a "].into_iter());
        assert_eq!(allowed, vec!["5A", "12C", "9A"]);
        assert!(channel_allowed(&allowed, "12c") && channel_allowed(&allowed, "5A"));
        assert!(!channel_allowed(&allowed, "5B"));
    }

    #[test]
    fn password_guessing() {
        let mut a = Attempts::new(3, 5, Duration::from_secs(60));
        let t = Instant::now();
        assert!(!a.locked("1.1.1.1", t));
        for _ in 0..3 {
            a.failed("1.1.1.1", t);
        }
        assert!(a.locked("1.1.1.1", t) && !a.locked("2.2.2.2", t));
        // everyone together
        a.failed("2.2.2.2", t);
        a.failed("3.3.3.3", t);
        assert!(a.locked("4.4.4.4", t));
        // a quarter of an hour later (here a minute): open again
        let later = t + Duration::from_secs(61);
        assert!(!a.locked("1.1.1.1", later) && !a.locked("4.4.4.4", later));
        a.failed("2.2.2.2", later);
        a.succeeded("2.2.2.2");
        assert_eq!(a.failed("2.2.2.2", later), (1, 2));
    }
}
