//! Registry of connected web clients and message fan-out.

use magicdab_dab::receiver::ServiceKey;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Clone)]
pub enum Out {
    Text(Arc<str>),
    Binary(Arc<Vec<u8>>),
    /// End the connection with this close code and reason (1012: the program
    /// restarts, browsers reconnect; access.rs has the codes the interface
    /// knows).
    Close(u16, Arc<str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Opus,
    Pcm,
    /// Played by the program itself (speaker.rs): nothing is sent.
    Native,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Subscriptions {
    pub spectrum: bool,
    pub tii_pairs: bool,
    pub log: bool,
    /// Debug state of the DAB/FM linking.
    pub linking: bool,
    /// Statistics of the server (admins).
    pub stats: bool,
    /// The buffer and loudness of the program's own sound output.
    pub speaker: bool,
}

pub struct Client {
    pub id: u64,
    pub tx: mpsc::Sender<Out>,
    pub admin: bool,
    pub service: Option<ServiceKey>,
    pub audio: bool,
    pub codec: Codec,
    pub subs: Subscriptions,
    /// Real client address (forwarded headers behind tunnels / proxies).
    pub ip: String,
    /// On this computer (loopback, not through a proxy): may use the
    /// program's own sound output.
    pub local: bool,
    /// Short browser / OS description.
    pub agent: String,
    pub connected: Instant,
    pub dropped: u64,
    /// Receives the original encoded audio of its service (recording).
    pub record: bool,
    /// Token of the login for the chat (`/chat?token=`): an admin's, or on a
    /// private server that of a listener who gave the tuning password.
    pub chat_token: Option<String>,
    /// May tune while the tuner is locked (gave the tuning password).
    pub tuner: bool,
    /// Public host the client came through (the tunnel), empty when local.
    pub via: String,
    /// The receiver it listens to: 0 the main one, else the side slot of a
    /// multiplex the listener chose itself (radio/slots.rs).
    pub slot: u32,
    /// Bytes sent to it (statistics; counted by its connection).
    pub sent: Arc<AtomicU64>,
    /// At the gate: the radio thread has not let it in yet (every new
    /// connection), or the server is private and it gave no password
    /// (radio/gate.rs). It gets nothing but what `send_always` sends.
    pub gated: bool,
    /// The last command the listener sent by hand (radio/presence.rs).
    pub last_action: Instant,
    /// Asked whether it is still listening, and when.
    pub asked: Option<Instant>,
}

impl Client {
    /// Listens to the main receiver.
    pub fn on_main(&self) -> bool {
        self.slot == 0
    }
}

#[derive(Default)]
pub struct Hub {
    clients: Mutex<HashMap<u64, Client>>,
    next: AtomicU64,
    /// Bytes sent to all clients since the start (statistics).
    pub sent: AtomicU64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub id: u64,
    pub admin: bool,
    pub service: Option<u32>,
    pub audio: bool,
    pub codec: Codec,
    pub address: String,
    pub agent: String,
    pub connected_secs: u64,
    pub tuner: bool,
    pub via: String,
    pub slot: u32,
    /// Waits at the gate of a private server.
    pub gated: bool,
}

/// Who connected (from the HTTP upgrade request).
#[derive(Clone, Debug, Default)]
pub struct ClientMeta {
    pub ip: String,
    pub agent: String,
    pub via: String,
    pub local: bool,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Hub::default())
    }

    /// A new connection: its id, and the counter of the bytes sent to it. It
    /// waits at the gate until the radio thread lets it in (with its
    /// snapshot), so nothing reaches it before.
    pub fn register(&self, tx: mpsc::Sender<Out>, meta: &ClientMeta) -> (u64, Arc<AtomicU64>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let sent = Arc::new(AtomicU64::new(0));
        self.clients.lock().insert(
            id,
            Client {
                id,
                tx,
                admin: false,
                service: None,
                audio: false,
                codec: Codec::Opus,
                subs: Subscriptions::default(),
                ip: meta.ip.clone(),
                local: meta.local,
                agent: meta.agent.clone(),
                connected: Instant::now(),
                dropped: 0,
                record: false,
                chat_token: None,
                tuner: false,
                via: meta.via.clone(),
                slot: 0,
                sent: sent.clone(),
                gated: true,
                last_action: Instant::now(),
                asked: None,
            },
        );
        (id, sent)
    }

    /// (id, last action, asked) of the listeners let in who are not admins
    /// (radio/presence.rs).
    pub fn presence(&self) -> Vec<(u64, Instant, Option<Instant>)> {
        self.clients.lock().values().filter(|c| !c.gated && !c.admin).map(|c| (c.id, c.last_action, c.asked)).collect()
    }

    /// Bytes sent so far to each client, and whether it came through the tunnel.
    pub fn sent_by_client(&self) -> Vec<(u64, u64, bool)> {
        self.clients.lock().values().map(|c| (c.id, c.sent.load(Ordering::Relaxed), !c.via.is_empty())).collect()
    }

    pub fn unregister(&self, id: u64) {
        self.clients.lock().remove(&id);
    }

    pub fn update<R>(&self, id: u64, f: impl FnOnce(&mut Client) -> R) -> Option<R> {
        self.clients.lock().get_mut(&id).map(f)
    }

    /// Clients let in (not those at the gate of a private server).
    pub fn count(&self) -> usize {
        self.clients.lock().values().filter(|c| !c.gated).count()
    }

    pub fn is_gated(&self, id: u64) -> bool {
        self.clients.lock().get(&id).is_some_and(|c| c.gated)
    }

    /// The service a logged in admin of a receiver has chosen (one with the
    /// sound on first).
    pub fn admin_service(&self, slot: u32) -> Option<ServiceKey> {
        let clients = self.clients.lock();
        let admins: Vec<&Client> = clients.values().filter(|c| c.slot == slot && c.admin && !c.gated && c.service.is_some()).collect();
        admins.iter().find(|c| c.audio).or(admins.first()).and_then(|c| c.service)
    }

    /// Clients receiving audio.
    pub fn audio_count(&self) -> usize {
        self.clients.lock().values().filter(|c| c.audio).count()
    }

    pub fn is_admin(&self, id: u64) -> bool {
        self.clients.lock().get(&id).map(|c| c.admin).unwrap_or(false)
    }

    /// The admin, or a listener with the tuning password.
    pub fn may_tune(&self, id: u64) -> bool {
        self.clients.lock().get(&id).is_some_and(|c| c.admin || c.tuner)
    }

    /// (address, agent, seconds connected, service, audio) of a client.
    pub fn session(&self, id: u64) -> Option<(String, String, u64, Option<u32>, bool)> {
        self.clients
            .lock()
            .get(&id)
            .map(|c| (c.ip.clone(), c.agent.clone(), c.connected.elapsed().as_secs(), c.service.map(|k| k.sid), c.audio))
    }

    pub fn list(&self) -> Vec<ClientInfo> {
        self.clients
            .lock()
            .values()
            .map(|c| ClientInfo {
                id: c.id,
                admin: c.admin,
                service: c.service.map(|k| k.sid),
                audio: c.audio,
                codec: c.codec,
                address: c.ip.clone(),
                agent: c.agent.clone(),
                connected_secs: c.connected.elapsed().as_secs(),
                tuner: c.tuner,
                via: c.via.clone(),
                slot: c.slot,
                gated: c.gated,
            })
            .collect()
    }

    /// The slot a client listens to (0: the main receiver).
    pub fn slot_of(&self, id: u64) -> u32 {
        self.clients.lock().get(&id).map_or(0, |c| c.slot)
    }

    /// Clients per slot.
    pub fn slot_counts(&self) -> HashMap<u32, usize> {
        let mut m = HashMap::new();
        for c in self.clients.lock().values() {
            *m.entry(c.slot).or_insert(0) += 1;
        }
        m
    }

    /// Clients of a slot (let in: those at the gate listen to nothing).
    pub fn ids_in(&self, slot: u32) -> Vec<u64> {
        self.clients.lock().values().filter(|c| c.slot == slot && !c.gated).map(|c| c.id).collect()
    }

    /// Services of a slot that should be decoded: (key, wants opus, wants pcm).
    pub fn wanted_services(&self, slot: u32) -> Vec<(ServiceKey, bool, bool)> {
        let mut map: HashMap<ServiceKey, (bool, bool)> = HashMap::new();
        for c in self.clients.lock().values().filter(|c| c.slot == slot) {
            if let Some(k) = c.service {
                let e = map.entry(k).or_insert((false, false));
                if c.audio {
                    match c.codec {
                        Codec::Opus => e.0 = true,
                        Codec::Pcm => e.1 = true,
                        Codec::Native => {}
                    }
                }
            }
        }
        map.into_iter().map(|(k, (o, p))| (k, o, p)).collect()
    }

    /// What the program's own sound output plays (slot, service): the station
    /// of the newest client on this computer that asked for it.
    pub fn speaker_service(&self) -> Option<(u32, ServiceKey)> {
        let clients = self.clients.lock();
        let c = clients.values().filter(|c| c.audio && c.codec == Codec::Native && c.local && !c.gated).max_by_key(|c| c.id)?;
        Some((c.slot, c.service?))
    }

    pub fn is_local(&self, id: u64) -> bool {
        self.clients.lock().get(&id).is_some_and(|c| c.local)
    }

    /// The token belongs to a client logged in as admin (chat).
    pub fn is_admin_token(&self, token: &str) -> bool {
        self.clients.lock().values().any(|c| c.admin && c.chat_token.as_deref() == Some(token))
    }

    /// The token belongs to a client let in (the chat of a private server).
    pub fn is_token_in(&self, token: &str) -> bool {
        self.clients.lock().values().any(|c| !c.gated && c.chat_token.as_deref() == Some(token))
    }

    /// Free places in a client's send queue (a client that does not read fills it).
    pub fn queue_free(&self, id: u64) -> usize {
        self.clients.lock().get(&id).map(|c| c.tx.capacity()).unwrap_or(0)
    }

    /// Send to a client let in (one at the gate gets nothing this way).
    pub fn send(&self, id: u64, msg: Out) {
        if let Some(c) = self.clients.lock().get_mut(&id).filter(|c| !c.gated) {
            if c.tx.try_send(msg).is_err() {
                c.dropped += 1;
            }
        }
    }

    /// Messages that could not be queued for a client so far.
    pub fn dropped(&self, id: u64) -> Option<u64> {
        self.clients.lock().get(&id).map(|c| c.dropped)
    }

    /// Send, telling whether it was queued (a client that stopped reading has
    /// a full queue).
    pub fn try_send(&self, id: u64, msg: Out) -> bool {
        match self.clients.lock().get_mut(&id).filter(|c| !c.gated) {
            Some(c) => c.tx.try_send(msg).is_ok(),
            None => false,
        }
    }

    /// Send to a client at the gate too: the gate's hello and the answers to
    /// its passwords, nothing of the receiver.
    pub fn send_always(&self, id: u64, msg: Out) {
        if let Some(c) = self.clients.lock().get_mut(&id) {
            if c.tx.try_send(msg).is_err() {
                c.dropped += 1;
            }
        }
    }

    /// Send to every client let in that matches `filter`.
    pub fn broadcast(&self, msg: Out, filter: impl Fn(&Client) -> bool) {
        for c in self.clients.lock().values_mut() {
            if !c.gated && filter(c) && c.tx.try_send(msg.clone()).is_err() {
                c.dropped += 1;
            }
        }
    }

    pub fn broadcast_all(&self, msg: Out) {
        self.broadcast(msg, |_| true);
    }

    /// Send to the listeners of the main receiver (messages about its multiplex).
    pub fn broadcast_main(&self, msg: Out) {
        self.broadcast(msg, |c| c.on_main());
    }

    /// Services of a slot whose original audio is recorded by at least one client.
    pub fn recorded_services(&self, slot: u32) -> Vec<ServiceKey> {
        let mut v: Vec<ServiceKey> =
            self.clients.lock().values().filter(|c| c.record && c.slot == slot).filter_map(|c| c.service).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn any(&self, f: impl Fn(&Client) -> bool) -> bool {
        self.clients.lock().values().any(f)
    }

    pub fn ids(&self) -> Vec<u64> {
        self.clients.lock().keys().copied().collect()
    }
}

pub fn text(v: &serde_json::Value) -> Out {
    Out::Text(Arc::from(v.to_string()))
}

/// Binary message: type byte, u32 LE length of a JSON header, header, payload.
pub fn binary(kind: u8, header: &serde_json::Value, payload: &[u8]) -> Out {
    let h = header.to_string();
    let mut v = Vec::with_capacity(5 + h.len() + payload.len());
    v.push(kind);
    v.extend_from_slice(&(h.len() as u32).to_le_bytes());
    v.extend_from_slice(h.as_bytes());
    v.extend_from_slice(payload);
    Out::Binary(Arc::new(v))
}

pub const BIN_AUDIO: u8 = 1;
pub const BIN_IMAGE: u8 = 2;
pub const BIN_SPECTRUM: u8 = 3;
pub const BIN_LOGO: u8 = 4;
pub const BIN_ENCODED: u8 = 5;
