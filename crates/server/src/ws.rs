//! WebSocket endpoint: one connection per browser tab.

use crate::hub::{ClientMeta, Hub, Out};
use crate::radio::Command;
use axum::extract::ws::{Message, WebSocket};
use crossbeam_channel::Sender;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};

/// Commands a connection may send at once, and per second after that: above
/// what the interface sends (logos of a long station list come in a burst),
/// so that only a flood is cut off before it reaches the radio thread. An
/// admin's connection has more; all connections of one network of the
/// internet (an IPv4 address, an IPv6 /64) share a budget too, so 40 of them
/// are not 40 times as much (not the local network: a proxy there that is
/// not trusted, or Docker's gateway, has every listener behind it).
const BURST: f64 = 400.0;
const PER_SECOND: f64 = 40.0;
const ADMIN_BURST: f64 = 1000.0;
const ADMIN_PER_SECOND: f64 = 200.0;
const NETWORK_BURST: f64 = 1200.0;
const NETWORK_PER_SECOND: f64 = 120.0;
/// Commands with a large answer (pictures, documents, everything kept so far)
/// count as this many.
const HEAVY: f64 = 20.0;
/// Commands of a connection waiting for the radio thread at most; more are
/// ignored like a flood.
const QUEUED_MAX: usize = 200;
/// Commands ignored before the address is banned for a while (Settings >
/// Access): a flood, not a burst of the interface.
const FLOOD_BAN: u64 = 1000;
/// The longest command of a listener; an admin's may be longer (a list of
/// transmitters comes in parts of a few hundred kB).
const LISTENER_MAX: usize = 64 * 1024;

/// A budget of commands: what is left, refilled by the second up to `burst`.
struct Bucket {
    tokens: f64,
    refilled: Instant,
}

impl Bucket {
    fn new(burst: f64) -> Self {
        Bucket { tokens: burst, refilled: Instant::now() }
    }

    fn take(&mut self, now: Instant, cost: f64, burst: f64, per_second: f64) -> bool {
        self.tokens = (self.tokens + now.duration_since(self.refilled).as_secs_f64() * per_second).min(burst);
        self.refilled = now;
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }
}

/// The budgets of the networks (non-admin connections only).
static NETWORKS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Bucket>>> = std::sync::LazyLock::new(Default::default);

fn network_take(network: &str, now: Instant, cost: f64) -> bool {
    let mut nets = NETWORKS.lock().unwrap();
    if nets.len() > 10_000 {
        // (the full ones are as good as new)
        nets.retain(|_, b| now.duration_since(b.refilled).as_secs_f64() * NETWORK_PER_SECOND + b.tokens < NETWORK_BURST);
    }
    nets.entry(network.to_string()).or_insert_with(|| Bucket::new(NETWORK_BURST)).take(now, cost, NETWORK_BURST, NETWORK_PER_SECOND)
}

/// What a command costs of the budgets.
fn cost(v: &serde_json::Value) -> f64 {
    match v["type"].as_str().unwrap_or("") {
        "getGallery" | "getSpiDoc" | "getSpiDocs" | "listeners" => HEAVY,
        "subscribe" if v["journaline"].as_bool() == Some(true) || v["traffic"].as_bool() == Some(true) => HEAVY,
        _ => 1.0,
    }
}

/// Send one message; a peer that stopped reading (its TCP buffer stays full) is
/// given up after a while instead of holding the connection until the kernel does.
async fn send_to(sink: &mut SplitSink<WebSocket, Message>, m: Message) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(15), sink.send(m)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("sending failed: {e}")),
        Err(_) => Err("sending blocked for 15 s (connection dead)".into()),
    }
}

pub async fn handle(socket: WebSocket, hub: Arc<Hub>, radio: Sender<Command>, meta: ClientMeta) {
    let (tx, mut rx) = mpsc::channel::<Out>(512);
    let (id, sent) = hub.register(tx, &meta);
    log::info!(
        "client {id} connected: {} ({}){}",
        meta.ip,
        if meta.agent.is_empty() { "?" } else { &meta.agent },
        if meta.via.is_empty() { String::new() } else { format!(" via {}", meta.via) }
    );
    let _ = radio.send(Command::Connected(id));
    let (mut sink, mut stream) = socket.split();

    // log streaming for admins who subscribed
    let mut logs = crate::logbuf::subscribe();
    let hub_log = hub.clone();
    let hub_sent = hub.clone();
    // anything received from the browser (pongs too): dead connections behind
    // tunnels and proxies are noticed by the pings
    let last_seen = Arc::new(Mutex::new(Instant::now()));
    let seen = last_seen.clone();
    let broken = Arc::new(Notify::new());
    let writer_end = Arc::new(Mutex::new(None::<String>));
    let (broken_w, end_w) = (broken.clone(), writer_end.clone());
    // kicked, banned, or of a network refused later
    let ip: Option<std::net::IpAddr> = meta.ip.parse().ok();
    let mut access_events = crate::access::get().events();

    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        let end = loop {
            tokio::select! {
                ev = access_events.recv() => {
                    let Some(ip) = ip else { continue };
                    if let Some((code, why)) = crate::access::get().on_event(ev, ip) {
                        let frame = axum::extract::ws::CloseFrame { code, reason: why.into() };
                        let _ = send_to(&mut sink, Message::Close(Some(frame))).await;
                        break Some(format!("closed by the server ({why})"));
                    }
                }
                msg = rx.recv() => {
                    let Some(msg) = msg else { break None };
                    // (what goes out: the statistics of the bandwidth)
                    let size = match &msg {
                        Out::Text(t) => t.len(),
                        Out::Binary(b) => b.len(),
                        Out::Close(..) => 0,
                    } as u64;
                    sent.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
                    hub_sent.sent.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
                    let m = match msg {
                        Out::Text(t) => Message::Text(t.to_string().into()),
                        Out::Binary(b) => Message::Binary(b.as_ref().clone().into()),
                        Out::Close(code, reason) => {
                            let frame = axum::extract::ws::CloseFrame { code, reason: reason.to_string().into() };
                            let _ = send_to(&mut sink, Message::Close(Some(frame))).await;
                            break Some(format!("closed by the server ({reason})"));
                        }
                    };
                    if let Err(e) = send_to(&mut sink, m).await {
                        break Some(e);
                    }
                }
                line = logs.recv() => {
                    if let Ok(line) = line {
                        let wants = hub_log.update(id, |c| c.subs.log && c.admin).unwrap_or(false);
                        if wants {
                            let v = serde_json::json!({"type": "log", "line": line});
                            if let Err(e) = send_to(&mut sink, Message::Text(v.to_string().into())).await {
                                break Some(e);
                            }
                        }
                    }
                }
                _ = ping.tick() => {
                    let quiet = seen.lock().unwrap().elapsed();
                    if quiet > Duration::from_secs(60) {
                        break Some(format!("no answer for {} s (connection dead)", quiet.as_secs()));
                    }
                    if let Err(e) = send_to(&mut sink, Message::Ping(Vec::new().into())).await {
                        break Some(e);
                    }
                }
            }
        };
        if let Some(e) = end {
            *end_w.lock().unwrap() = Some(e);
            broken_w.notify_one();
        }
    });

    let mut own = Bucket::new(ADMIN_BURST);
    let mut dropped = 0u64;
    let network = crate::access::network(&meta.ip);
    let shared_budget = !ip.is_some_and(crate::access::is_local);
    let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reason = loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(msg)) => {
                    let now = Instant::now();
                    *last_seen.lock().unwrap() = now;
                    match msg {
                        Message::Text(t) => {
                            let admin = hub.is_admin(id);
                            let take = |own: &mut Bucket, c: f64| {
                                if admin {
                                    own.take(now, c, ADMIN_BURST, ADMIN_PER_SECOND)
                                } else {
                                    own.take(now, c, BURST, PER_SECOND) && (!shared_budget || network_take(&network, now, c))
                                }
                            };
                            // (a flood goes before it costs a parse)
                            let mut allowed = take(&mut own, 1.0) && (admin || queued.load(std::sync::atomic::Ordering::Relaxed) < QUEUED_MAX);
                            let mut parsed = None;
                            if allowed {
                                if t.len() > LISTENER_MAX && !admin {
                                    log::debug!("client {id}: a command of {} bytes ignored", t.len());
                                } else {
                                    match serde_json::from_str::<serde_json::Value>(&t) {
                                        Ok(v) => parsed = Some(v),
                                        Err(e) => log::debug!("client {id}: invalid message: {e}"),
                                    }
                                }
                                let extra = parsed.as_ref().map_or(0.0, cost) - 1.0;
                                if extra > 0.0 {
                                    allowed = take(&mut own, extra);
                                }
                            }
                            if !allowed {
                                dropped += 1;
                                if dropped == 1 || dropped % 10_000 == 0 {
                                    log::warn!("client {id}: too many commands, {dropped} ignored");
                                }
                                // (its connections close once banned)
                                if dropped == FLOOD_BAN && !admin {
                                    if let Some(ip) = ip {
                                        crate::access::get().auto_ban(ip, &format!("flood of commands ({FLOOD_BAN} ignored)"));
                                    }
                                }
                                continue;
                            }
                            if let Some(v) = parsed {
                                queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let _ = radio.send(Command::Message { client: id, msg: v, queued: queued.clone() });
                            }
                        }
                        Message::Close(f) => {
                            break match f {
                                Some(f) if !f.reason.is_empty() => format!("closed by the browser (code {}, {})", f.code, f.reason.as_str()),
                                Some(f) => format!("closed by the browser (code {})", f.code),
                                None => "closed by the browser".to_string(),
                            };
                        }
                        _ => {}
                    }
                }
                Some(Err(e)) => break format!("connection error: {e}"),
                None => break "connection lost without closing (network or tunnel)".to_string(),
            },
            _ = broken.notified() => break writer_end.lock().unwrap().take().unwrap_or_default(),
        }
    };
    let session = hub.session(id);
    let (dropped, slot) = hub.update(id, |c| (c.dropped, c.slot)).unwrap_or((0, 0));
    hub.unregister(id);
    writer.abort();
    let _ = radio.send(Command::Gone { id, session, reason, dropped, slot });
}

/// Short browser and OS description from a User-Agent header.
pub fn short_agent(ua: &str) -> String {
    let version = |name: &str| {
        ua.split(name).nth(1).map(|r| r.trim_start_matches('/').split(|c: char| !c.is_ascii_digit()).next().unwrap_or("").to_string())
    };
    let browser = if let Some(v) = version("Edg") {
        format!("Edge {v}")
    } else if let Some(v) = version("OPR") {
        format!("Opera {v}")
    } else if let Some(v) = version("SamsungBrowser") {
        format!("Samsung {v}")
    } else if let Some(v) = version("Firefox") {
        format!("Firefox {v}")
    } else if let Some(v) = version("Chrome") {
        format!("Chrome {v}")
    } else if ua.contains("Safari") {
        format!("Safari {}", version("Version").unwrap_or_default())
    } else if ua.is_empty() {
        String::new()
    } else {
        ua.split(' ').next().unwrap_or("").to_string()
    };
    let os = if ua.contains("Android") {
        "Android"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Mac OS") {
        "macOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        ""
    };
    match (browser.is_empty(), os.is_empty()) {
        (false, false) => format!("{browser}, {os}"),
        (false, true) => browser,
        (true, false) => os.to_string(),
        _ => String::new(),
    }
}
