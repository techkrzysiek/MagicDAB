//! Chat of the listeners, compatible with the one of FM-DX-Webserver: the same
//! WebSocket path (`/chat`) and messages, so its clients and tools work here.
//!
//! Server to client: `{"type":"clientIp","ip","admin"}` first, then the history
//! (every message with `"history": true`), then each new message
//! `{"nickname","message","id","time","admin"?}`, and `{"type":"clear"}` /
//! `{"type":"delete","id"}` from an admin. Client to server:
//! `{"nickname","message"}`, an admin also `{"type":"clear"}` and
//! `{"type":"delete","id"}`. The nickname comes back HTML escaped (clients of
//! FM-DX-Webserver put it into the page as HTML), the message as it was sent.
//!
//! The address of a sender goes to admins only; every listener gets its own
//! address in `clientIp`. An admin is recognised by the token of the login of
//! this interface (`/chat?token=...`), not by a session cookie.

use crate::hub::Hub;
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Messages kept for listeners who come later, and the lengths kept of a
/// nickname and a message (as FM-DX-Webserver).
const HISTORY: usize = 50;
const NICKNAME_MAX: usize = 32;
const MESSAGE_MAX: usize = 255;
/// Bigger frames are ignored.
pub const PAYLOAD_MAX: usize = 512;

pub struct Chat {
    history: Mutex<VecDeque<Value>>,
    next_id: AtomicU64,
    tx: broadcast::Sender<Value>,
}

impl Chat {
    pub fn new() -> Arc<Self> {
        Arc::new(Chat { history: Mutex::new(VecDeque::new()), next_id: AtomicU64::new(0), tx: broadcast::channel(256).0 })
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            c => out.push(c),
        }
    }
    out
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// A chat message from what a listener sent, or None if there is nothing to say.
fn message(v: &Value, ip: &str, id: u64, admin: bool) -> Option<Value> {
    let nickname: String = escape_html(text_of(&v["nickname"])?.trim()).chars().take(NICKNAME_MAX).collect();
    let text: String = text_of(&v["message"])?.trim().chars().take(MESSAGE_MAX).collect();
    if text.is_empty() {
        return None;
    }
    let mut m = json!({
        "nickname": nickname,
        "message": text,
        "ip": ip,
        "id": id,
        "time": crate::localtime::now().format("%Y-%m-%d %H:%M").to_string(),
    });
    if admin {
        m["admin"] = json!(true);
    }
    Some(m)
}

/// A message as one listener gets it: the address for admins only.
fn for_client(m: &Value, admin: bool) -> Message {
    if admin || m.get("ip").is_none() {
        return Message::Text(m.to_string().into());
    }
    let mut m = m.clone();
    if let Some(o) = m.as_object_mut() {
        o.remove("ip");
    }
    Message::Text(m.to_string().into())
}

async fn send(sink: &mut SplitSink<WebSocket, Message>, m: Message) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(15), sink.send(m)).await, Ok(Ok(())))
}

pub async fn handle(
    socket: WebSocket,
    chat: Arc<Chat>,
    hub: Arc<Hub>,
    ip: String,
    token: Option<String>,
    public: Arc<parking_lot::RwLock<crate::public::PublicInfo>>,
) {
    let is_admin = || token.as_deref().is_some_and(|t| hub.is_admin_token(t));
    // a server turned private, or its listener let out again (radio/gate.rs):
    // the chat ends as it was refused at the door
    let shut_out = || public.read().private && !token.as_deref().is_some_and(|t| hub.is_token_in(t));
    let refused =
        || Message::Close(Some(axum::extract::ws::CloseFrame { code: crate::access::CLOSE_REFUSED, reason: "private server".into() }));
    let (mut sink, mut stream) = socket.split();
    let mut rx = chat.tx.subscribe();
    let admin = is_admin();
    if !send(&mut sink, Message::Text(json!({"type": "clientIp", "ip": ip, "admin": admin}).to_string().into())).await {
        return;
    }
    let history: Vec<Value> = chat.history.lock().iter().cloned().collect();
    for mut m in history {
        m["history"] = json!(true);
        if !send(&mut sink, for_client(&m, admin)).await {
            return;
        }
    }
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    let mut last_seen = Instant::now();
    // antispam as FM-DX-Webserver: 8 messages within 20 ms, or the same one more
    // than 5 times within a second, end the connection
    let mut recent: VecDeque<Instant> = VecDeque::new();
    let mut same: VecDeque<(Instant, String)> = VecDeque::new();
    let addr: Option<std::net::IpAddr> = ip.parse().ok();
    let mut access_events = crate::access::get().events();
    loop {
        tokio::select! {
            ev = access_events.recv() => {
                let Some(addr) = addr else { continue };
                if let Some((code, why)) = crate::access::get().on_event(ev, addr) {
                    let _ = send(&mut sink, Message::Close(Some(axum::extract::ws::CloseFrame { code, reason: why.into() }))).await;
                    break;
                }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                let now = Instant::now();
                last_seen = now;
                let Message::Text(text) = msg else { continue };
                if text.len() > PAYLOAD_MAX {
                    continue;
                }
                recent.retain(|t| now.duration_since(*t) <= Duration::from_millis(20));
                recent.push_back(now);
                same.retain(|(t, _)| now.duration_since(*t) <= Duration::from_secs(1));
                same.push_back((now, text.to_string()));
                if recent.len() >= 8 || same.iter().filter(|(_, s)| s.as_str() == text.as_str()).count() > 5 {
                    log::warn!("chat: {ip} is spamming, connection closed");
                    let frame = axum::extract::ws::CloseFrame { code: 1008, reason: "Spamming detected".into() };
                    let _ = send(&mut sink, Message::Close(Some(frame))).await;
                    // and banned for a while (Settings > Access)
                    if let Some(addr) = addr.filter(|_| !is_admin()) {
                        crate::access::get().auto_ban(addr, "chat spam");
                    }
                    break;
                }
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    if !send(&mut sink, Message::Text(json!({"error": "Invalid message format"}).to_string().into())).await {
                        break;
                    }
                    continue;
                };
                match v["type"].as_str() {
                    Some(kind @ ("clear" | "delete")) => {
                        if !is_admin() {
                            continue;
                        }
                        let id = v["id"].as_u64();
                        {
                            let mut h = chat.history.lock();
                            if kind == "clear" {
                                h.clear();
                            } else if let Some(i) = h.iter().position(|m| m["id"].as_u64() == id && id.is_some()) {
                                h.remove(i);
                            } else {
                                continue;
                            }
                        }
                        log::info!("chat: {kind} by an admin ({ip})");
                        let _ = chat.tx.send(if kind == "clear" { json!({"type": "clear"}) } else { json!({"type": "delete", "id": id}) });
                    }
                    _ => {
                        let id = chat.next_id.load(Ordering::Relaxed);
                        let Some(m) = message(&v, &ip, id, is_admin()) else { continue };
                        chat.next_id.fetch_add(1, Ordering::Relaxed);
                        log::info!("chat: {} ({ip}): {}", m["nickname"].as_str().unwrap_or(""), m["message"].as_str().unwrap_or(""));
                        {
                            let mut h = chat.history.lock();
                            h.push_back(m.clone());
                            while h.len() > HISTORY {
                                h.pop_front();
                            }
                        }
                        let _ = chat.tx.send(m);
                    }
                }
            }
            m = rx.recv() => match m {
                Ok(m) => {
                    if shut_out() {
                        let _ = send(&mut sink, refused()).await;
                        break;
                    }
                    if !send(&mut sink, for_client(&m, is_admin())).await {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if shut_out() {
                    let _ = send(&mut sink, refused()).await;
                    break;
                }
                // proxies and tunnels end quiet connections; dead ones are noticed
                if last_seen.elapsed() > Duration::from_secs(75) || !send(&mut sink, Message::Ping(Vec::new().into())).await {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_as_fm_dx_webserver_makes_them() {
        let m = message(
            &json!({"nickname": "<b>Tom</b> & Jerry 0123456789 0123456789", "message": "  hello  ", "extra": 1}),
            "1.2.3.4",
            7,
            false,
        )
        .unwrap();
        assert_eq!(m["nickname"], "&lt;b&gt;Tom&lt;/b&gt; &amp; Jer");
        assert_eq!(m["message"], "hello");
        assert_eq!((m["id"].as_u64(), m["ip"].as_str(), m.get("admin"), m.get("extra")), (Some(7), Some("1.2.3.4"), None, None));
        assert_eq!(m["time"].as_str().map(|t| t.len()), Some(16));
        assert!(message(&json!({"message": "no nickname"}), "", 0, false).is_none());
        assert!(message(&json!({"nickname": "a", "message": " "}), "", 0, false).is_none());
        let long = "x".repeat(300);
        assert_eq!(
            message(&json!({"nickname": "a", "message": long}), "", 0, true).unwrap()["message"].as_str().map(|s| s.len()),
            Some(255)
        );
        // the address for admins only
        let Message::Text(t) = for_client(&m, false) else { panic!() };
        assert!(!t.contains("1.2.3.4"));
        let Message::Text(t) = for_client(&m, true) else { panic!() };
        assert!(t.contains("1.2.3.4"));
    }
}
