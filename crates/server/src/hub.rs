//! Registry of connected web clients and message fan-out.

use magicdab_dab::receiver::ServiceKey;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Opus through /audio/<SId> (AudioTaps), not this connection: it
    /// counts as a listener, the bytes go one copy for all (a proxy).
    Shared,
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
    /// Journaline objects of the multiplex (radio/rx.rs).
    pub journaline: bool,
    /// TPEG traffic messages of the multiplex (radio/rx.rs).
    pub traffic: bool,
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
    /// The logins its page keeps for its next connections (sessions.rs): the
    /// admin's and the one with the tuning password, each as (token, its
    /// name in the sessions).
    pub session: Option<(String, String)>,
    pub tune_session: Option<(String, String)>,
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
    /// Reports of its page for the log (`diag`) in the minute since.
    pub reports: (Instant, u32),
}

/// Reports a page may send for the log in a minute (it sends one when it
/// reconnects, one every 15 s while its sound drops out, its errors).
const REPORTS_A_MINUTE: u32 = 20;

impl Client {
    /// Another report of its page may go to the log.
    pub fn may_report(&mut self) -> bool {
        if self.reports.0.elapsed() >= std::time::Duration::from_secs(60) {
            self.reports = (Instant::now(), 0);
        }
        self.reports.1 += 1;
        self.reports.1 <= REPORTS_A_MINUTE
    }

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
    /// Tickets for a download each (`ticket`): what, its name, since when.
    tickets: Mutex<HashMap<String, (String, String, Instant)>>,
    /// The Opus audio of the main receiver's stations for /audio/<SId>.
    pub taps: Arc<AudioTaps>,
}

/// Opus frames of the main receiver's stations (as BIN_AUDIO, the same bytes
/// for every listener) for the sockets of /audio/<SId>: a proxy in front of
/// the server passes one copy to all the listeners of a station. Who
/// listens is told over /ws (Codec::Shared), so the station is decoded and
/// they are counted as they are now.
#[derive(Default)]
pub struct AudioTaps {
    senders: Mutex<HashMap<u32, tokio::sync::broadcast::Sender<Arc<Vec<u8>>>>>,
    /// The audio stations of the main receiver's multiplex (radio/streams.rs):
    /// no other SId gets a channel, so made-up ones cannot fill the memory.
    stations: Mutex<HashSet<u32>>,
    /// Sockets open, in all and by address.
    open: AtomicUsize,
    by_address: Mutex<HashMap<IpAddr, usize>>,
}

/// Frames a socket of /audio may fall behind before it skips ahead (5 s).
const TAP_BACKLOG: usize = 250;

/// Sockets of /audio at once (a proxy needs one per station it passes on),
/// and of one address of the internet (the local network has no limit).
const TAP_SOCKETS_MAX: usize = 256;
const TAP_SOCKETS_PER_ADDRESS: usize = 32;

/// Why a socket of /audio was refused.
pub enum TapRefused {
    /// Not a station of the multiplex.
    Unknown,
    /// Too many sockets, in all or from the address.
    Busy,
}

/// A socket of /audio: its frames, and its place given back when dropped
/// (the channel of the station goes with its last socket).
pub struct Tap {
    taps: Arc<AudioTaps>,
    sid: u32,
    address: IpAddr,
    rx: Option<tokio::sync::broadcast::Receiver<Arc<Vec<u8>>>>,
}

impl Tap {
    pub fn rx(&mut self) -> &mut tokio::sync::broadcast::Receiver<Arc<Vec<u8>>> {
        self.rx.as_mut().expect("receiver until dropped")
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        drop(self.rx.take());
        self.taps.open.fetch_sub(1, Ordering::Relaxed);
        {
            let mut by = self.taps.by_address.lock();
            if let Some(n) = by.get_mut(&self.address) {
                *n -= 1;
                if *n == 0 {
                    by.remove(&self.address);
                }
            }
        }
        let mut senders = self.taps.senders.lock();
        if senders.get(&self.sid).is_some_and(|tx| tx.receiver_count() == 0) {
            senders.remove(&self.sid);
        }
    }
}

impl AudioTaps {
    /// The audio stations of the multiplex now (the radio thread, every second).
    pub fn set_stations(&self, stations: HashSet<u32>) {
        *self.stations.lock() = stations;
    }

    pub fn subscribe(self: &Arc<Self>, sid: u32, address: IpAddr) -> Result<Tap, TapRefused> {
        if !self.stations.lock().contains(&sid) {
            return Err(TapRefused::Unknown);
        }
        {
            let mut by = self.by_address.lock();
            let n = by.entry(address).or_insert(0);
            if self.open.load(Ordering::Relaxed) >= TAP_SOCKETS_MAX || (*n >= TAP_SOCKETS_PER_ADDRESS && !crate::access::is_local(address))
            {
                if *n == 0 {
                    by.remove(&address);
                }
                return Err(TapRefused::Busy);
            }
            *n += 1;
            self.open.fetch_add(1, Ordering::Relaxed);
        }
        let rx = self.senders.lock().entry(sid).or_insert_with(|| tokio::sync::broadcast::channel(TAP_BACKLOG).0).subscribe();
        Ok(Tap { taps: self.clone(), sid, address, rx: Some(rx) })
    }

    /// A frame of station `sid` to its sockets, if it has any.
    pub fn send(&self, sid: u32, frame: &Arc<Vec<u8>>) {
        let mut senders = self.senders.lock();
        if let Some(tx) = senders.get(&sid) {
            if tx.send(frame.clone()).is_err() {
                senders.remove(&sid);
            }
        }
    }
}

/// Different stations of one network (an IPv4 address, an IPv6 /64) decoded
/// at most for it; an admin and the local network have no limit (a proxy
/// there not trusted, or Docker's gateway, has every listener behind it).
const STATIONS_PER_NETWORK: usize = 3;

/// An address of this computer or the local network (access::is_local).
fn local_network(ip: &str) -> bool {
    ip.parse::<IpAddr>().is_ok_and(crate::access::is_local)
}

/// A ticket opens its download for this long, once.
const TICKET_FOR: std::time::Duration = std::time::Duration::from_secs(60);

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
                session: None,
                tune_session: None,
                via: meta.via.clone(),
                slot: 0,
                sent: sent.clone(),
                gated: true,
                last_action: Instant::now(),
                asked: None,
                reports: (Instant::now(), 0),
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

    /// Connections in all (also those at the gate of a private server).
    pub fn total(&self) -> usize {
        self.clients.lock().len()
    }

    /// Connections of an address.
    pub fn of_address(&self, ip: &str) -> usize {
        self.clients.lock().values().filter(|c| c.ip == ip).count()
    }

    /// A ticket for one download of `name` (`what`: raw, dxrec) within a
    /// minute, for an admin's page: the address of a download is kept by
    /// proxies and the tunnel server in their logs, and a ticket is worth
    /// nothing once used, unlike the token of the login.
    pub fn ticket(&self, what: &str, name: &str) -> String {
        let ticket: String = rand::random::<[u8; 16]>().iter().map(|b| format!("{b:02x}")).collect();
        let mut t = self.tickets.lock();
        t.retain(|_, (_, _, since)| since.elapsed() < TICKET_FOR);
        t.insert(ticket.clone(), (what.to_string(), name.to_string(), Instant::now()));
        ticket
    }

    /// The ticket is one for this download: used up now.
    pub fn take_ticket(&self, ticket: &str, what: &str, name: &str) -> bool {
        let mut t = self.tickets.lock();
        t.retain(|_, (_, _, since)| since.elapsed() < TICKET_FOR);
        let ok = t.get(ticket).is_some_and(|(w, n, _)| w == what && n == name);
        if ok {
            t.remove(ticket);
        }
        ok
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
        let clients = self.clients.lock();
        // those who listen first (the longest connected first), then the pages
        // that only look: a few silent pages cannot push a listener out of
        // the stations decoded (settings.max_services)
        let mut order: Vec<&Client> = clients.values().filter(|c| c.slot == slot && c.service.is_some()).collect();
        order.sort_by_key(|c| (!c.audio, c.connected, c.id));
        let mut out: Vec<(ServiceKey, bool, bool)> = Vec::new();
        let mut by_network: HashMap<String, Vec<ServiceKey>> = HashMap::new();
        for c in order {
            let Some(k) = c.service else { continue };
            let mine = by_network.entry(crate::access::network(&c.ip)).or_default();
            if !mine.contains(&k) {
                if mine.len() >= STATIONS_PER_NETWORK && !c.admin && !local_network(&c.ip) {
                    continue;
                }
                mine.push(k);
            }
            let i = match out.iter().position(|(x, _, _)| *x == k) {
                Some(i) => i,
                None => {
                    out.push((k, false, false));
                    out.len() - 1
                }
            };
            if c.audio {
                match c.codec {
                    Codec::Opus | Codec::Shared => out[i].1 = true,
                    Codec::Pcm => out[i].2 = true,
                    Codec::Native => {}
                }
            }
        }
        out
    }

    /// The clients of a receiver that listen (sound on), with their station.
    pub fn listeners(&self, slot: u32) -> Vec<(u64, ServiceKey)> {
        self.clients.lock().values().filter(|c| c.slot == slot && c.audio && !c.gated).filter_map(|c| Some((c.id, c.service?))).collect()
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
        self.clients.lock().values().any(|c| c.admin && c.chat_token.as_deref().is_some_and(|t| same_token(t, token)))
    }

    /// The token belongs to a client let in (the chat of a private server).
    pub fn is_token_in(&self, token: &str) -> bool {
        self.clients.lock().values().any(|c| !c.gated && c.chat_token.as_deref().is_some_and(|t| same_token(t, token)))
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

/// A token given is the one kept (compared in a time that tells nothing of it).
fn same_token(kept: &str, given: &str) -> bool {
    !given.is_empty() && crate::config::same(kept.as_bytes(), given.as_bytes())
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
