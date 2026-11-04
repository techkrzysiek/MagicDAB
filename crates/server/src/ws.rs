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

/// Commands a connection may send at once, and per second after that: far above
/// what the interface sends (logos of a long station list come in a burst), so
/// that only a flood is cut off before it reaches the radio thread.
const BURST: f64 = 1000.0;
const PER_SECOND: f64 = 200.0;
/// Commands ignored before the address is banned for a while (Settings >
/// Access): a flood, not a burst of the interface.
const FLOOD_BAN: u64 = 1000;

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

    let (mut tokens, mut refilled, mut dropped) = (BURST, Instant::now(), 0u64);
    let reason = loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(msg)) => {
                    let now = Instant::now();
                    *last_seen.lock().unwrap() = now;
                    match msg {
                        Message::Text(t) => {
                            tokens = (tokens + now.duration_since(refilled).as_secs_f64() * PER_SECOND).min(BURST);
                            refilled = now;
                            if tokens < 1.0 {
                                dropped += 1;
                                if dropped == 1 || dropped % 10_000 == 0 {
                                    log::warn!("client {id}: too many commands, {dropped} ignored");
                                }
                                // (its connections close once banned)
                                if dropped == FLOOD_BAN && !hub.is_admin(id) {
                                    if let Some(ip) = ip {
                                        crate::access::get().auto_ban(ip, &format!("flood of commands ({FLOOD_BAN} ignored)"));
                                    }
                                }
                                continue;
                            }
                            tokens -= 1.0;
                            match serde_json::from_str::<serde_json::Value>(&t) {
                                Ok(v) => {
                                    let _ = radio.send(Command::Message { client: id, msg: v });
                                }
                                Err(e) => log::debug!("client {id}: invalid message: {e}"),
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
