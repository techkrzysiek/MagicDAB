//! Side slots: receivers of the multiplexes listeners chose themselves, next
//! to the main receiver (docs/multi-mux.md). The main receiver is "slot 0",
//! the rest of radio.rs. A side slot decodes a channel mapped to an ETI / EDI
//! stream or an IQ recording for its own listeners: they get its ensemble,
//! signal, time, DLS, slides, data services and audio. What is about "the"
//! receiver (the change log, the service list, dynamic labels, TII, scans,
//! RadioDNS, linking, /api, the gallery, raw recording, the spectrum) stays
//! with the main one.

use super::{
    data_progress_json, dls_json, dls_lines_json, format_json, note_data_progress, send_service_state, service_label, signal_json,
    slide_category, slide_header, slide_key, slide_progress_json, slideshow_app, stats_json, time_json, xpad_ua, DataProgress, Radio,
    ServiceState,
};
use crate::audio::AudioHub;
use crate::etiinput::EtiInput;
use crate::figcache::{self, FigCache};
use crate::fmradio::FM_SID;
use crate::hub::{binary, text, Out, BIN_ENCODED, BIN_IMAGE};
use crate::view::{self, LongLabels};
use crossbeam_channel::Receiver;
use magicdab_dab::consts::{channel_frequency, BAND_III};
use magicdab_dab::dls::DynamicLabel;
use magicdab_dab::ensemble::{DabTime, Ensemble};
use magicdab_dab::receiver::{ReceiverHandle, RxCommand, RxEvent, ServiceKey, SignalInfo};
use magicdab_sdr::{DeviceConfig, DeviceStatus, InputDevice};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Side slots playing recordings at once: each one demodulates OFDM, about a
/// quarter of a core of a small two-core server.
pub const MAX_RECORDINGS: usize = 2;

/// A slot nobody listens to stays open this long: someone hopping between
/// multiplexes comes back to it without starting it again.
pub const LINGER: Duration = Duration::from_secs(60);

/// A slot nobody listened to for this long makes room for another one when
/// all are taken (not at once: hopping among more multiplexes than fit would
/// start and stop receivers all the time).
pub const SPARE_AFTER: Duration = Duration::from_secs(10);

/// A playback of a DX recording ends soon after its admin left it (a
/// temporary thing; a reload of the page comes back to the main receiver).
pub const PLAYBACK_LINGER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// ETI / EDI stream: the FIC and the services listened to.
    Stream,
    /// IQ recording: OFDM demodulation too.
    Recording,
    /// A DX recording (an .eti file, dxrec.rs) an admin plays back for a
    /// while; not a channel listeners join.
    Playback,
}

impl Kind {
    fn linger(self) -> Duration {
        if self == Kind::Playback {
            PLAYBACK_LINGER
        } else {
            LINGER
        }
    }
}

pub struct Entry<T> {
    pub id: u32,
    pub channel: String,
    pub kind: Kind,
    /// Since when nobody listens.
    pub idle_since: Option<Instant>,
    pub slot: T,
}

/// Where a listener tuning to a channel goes.
#[derive(Debug, PartialEq)]
pub enum Place {
    /// The main receiver is on the channel.
    Main,
    /// The side slot of the channel.
    Slot(u32),
    /// A new side slot, after closing `evict` (nobody listens to it) for room.
    New { evict: Option<u32> },
    /// All side slots are in use.
    Full,
    /// As many recordings play as the processor takes.
    FullRecordings,
}

/// The side slots: one per channel, shared by all its listeners. Who
/// listens where is in the hub (`Client::slot`).
pub struct Table<T> {
    entries: Vec<Entry<T>>,
    next: u32,
    /// Last look for unused slots.
    pub swept: Instant,
}

impl<T> Table<T> {
    pub fn new() -> Self {
        Table { entries: Vec::new(), next: 1, swept: Instant::now() }
    }

    /// Where a listener tuning to `channel` goes; `main`: the channel of the
    /// main receiver (none while it scans: it moves on), `limit`: side slots
    /// at once.
    pub fn place(&self, channel: &str, main: Option<&str>, kind: Kind, limit: usize, now: Instant) -> Place {
        if main.is_some_and(|m| m.eq_ignore_ascii_case(channel)) {
            return Place::Main;
        }
        if let Some(e) = self.find(channel) {
            return Place::Slot(e.id);
        }
        let recordings = self.entries.iter().filter(|e| e.kind == Kind::Recording).count();
        let recordings_full = kind == Kind::Recording && recordings >= MAX_RECORDINGS;
        // (an admin's playback of a DX recording takes no place of the listeners')
        let full = self.entries.iter().filter(|e| e.kind != Kind::Playback).count() >= limit;
        if !full && !recordings_full {
            return Place::New { evict: None };
        }
        // one nobody listens to makes room, the longest unused first
        let spare = self
            .entries
            .iter()
            .filter(|e| e.idle_since.is_some_and(|t| now.duration_since(t) >= SPARE_AFTER))
            .filter(|e| !recordings_full || e.kind == Kind::Recording)
            .min_by_key(|e| e.idle_since);
        match spare {
            Some(e) if limit > 0 => Place::New { evict: Some(e.id) },
            _ if recordings_full => Place::FullRecordings,
            _ => Place::Full,
        }
    }

    /// The id the next slot gets.
    pub fn next_id(&self) -> u32 {
        self.next
    }

    pub fn open(&mut self, channel: &str, kind: Kind, slot: T) -> u32 {
        let id = self.next;
        self.next += 1;
        self.entries.push(Entry { id, channel: channel.to_string(), kind, idle_since: None, slot });
        id
    }

    /// The slot of a channel (a playback is nobody's channel).
    pub fn find(&self, channel: &str) -> Option<&Entry<T>> {
        self.entries.iter().find(|e| e.kind != Kind::Playback && e.channel.eq_ignore_ascii_case(channel))
    }

    /// Playbacks of DX recordings running.
    pub fn playbacks(&self) -> usize {
        self.entries.iter().filter(|e| e.kind == Kind::Playback).count()
    }

    pub fn get(&self, id: u32) -> Option<&Entry<T>> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Entry<T>> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// Closes a slot now (its listeners are the caller's business).
    pub fn take(&mut self, id: u32) -> Option<Entry<T>> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(i))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry<T>> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry<T>> {
        self.entries.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Notes which slots nobody listens to (`listeners`: clients per slot) and
    /// closes those unused for `LINGER`: they are returned.
    pub fn sweep(&mut self, listeners: &HashMap<u32, usize>, now: Instant) -> Vec<Entry<T>> {
        for e in self.entries.iter_mut() {
            if listeners.get(&e.id).copied().unwrap_or(0) > 0 {
                e.idle_since = None;
            } else if e.idle_since.is_none() {
                e.idle_since = Some(now);
            }
        }
        // (Rust 1.85, as in Debian 13, has no extract_if)
        let (closed, open) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| e.idle_since.is_some_and(|t| now.duration_since(t) >= e.kind.linger()));
        self.entries = open;
        closed
    }
}

/// The input of a side slot.
enum Input {
    Stream(EtiInput),
    Recording(Box<dyn InputDevice>),
    /// A DX recording played back (dxrec.rs).
    File(crate::dxrec::Player),
}

/// The receiver of a side slot and the state of the multiplex it decodes.
pub struct Slot {
    /// Address of the channel map it plays (a changed one closes the slot).
    url: String,
    frequency: u32,
    rx: ReceiverHandle,
    input: Input,
    ensemble: Arc<Ensemble>,
    labels: Arc<LongLabels>,
    ensemble_msg: Option<Out>,
    ensemble_changed: Instant,
    services: HashMap<ServiceKey, ServiceState>,
    decoding: HashSet<ServiceKey>,
    wants: HashMap<ServiceKey, (bool, bool)>,
    recording: Vec<ServiceKey>,
    audio: AudioHub,
    signal: SignalInfo,
    time: Option<DabTime>,
    data_progress: DataProgress,
    pending_clears: HashMap<ServiceKey, Instant>,
    opened: Instant,
}

impl Slot {
    /// What the statistics read: the receiver's time, the encoders, the bytes
    /// of a stream, the stations.
    pub fn stats_parts(&self) -> (&ReceiverHandle, &AudioHub, Option<u64>, &Ensemble) {
        let bytes = match &self.input {
            Input::Stream(e) => Some(e.bytes.load(std::sync::atomic::Ordering::Relaxed)),
            Input::Recording(_) | Input::File(_) => None,
        };
        (&self.rx, &self.audio, bytes, &self.ensemble)
    }

    /// The DX recording played back here.
    pub fn playback_name(&self) -> Option<&str> {
        match &self.input {
            Input::File(p) => Some(p.name.as_str()),
            _ => None,
        }
    }

    fn device(&self) -> DeviceStatus {
        match &self.input {
            Input::Recording(d) => d.status(),
            // (as the main receiver shows a stream without a tuner)
            Input::Stream(_) => DeviceStatus { kind: "none".into(), name: "-".into(), connected: true, ..Default::default() },
            Input::File(p) => {
                let (position, duration, _) = p.progress();
                DeviceStatus {
                    kind: "etifile".into(),
                    name: p.name.clone(),
                    connected: true,
                    frequency: self.frequency * 1000,
                    position: Some(position),
                    // (unknown for an .edi copied into the folder by hand)
                    duration: (duration > 0.0).then_some(duration),
                    ..Default::default()
                }
            }
        }
    }

    fn signal_json(&self) -> Value {
        signal_json(&self.signal, &self.device())
    }

    fn ensemble_message(&mut self, channel: &str, fig: &FigCache) -> Out {
        if let Some(m) = &self.ensemble_msg {
            return m.clone();
        }
        let live = view::ensemble(&self.ensemble, &self.labels, Some(channel), self.frequency);
        // the slow FIGs the main receiver learned of this multiplex
        let mut v = match fig.merged(channel, &self.ensemble, &self.labels) {
            Some((ens, labels, time)) => {
                let mut m = view::ensemble(&ens, &labels, Some(channel), self.frequency);
                figcache::flag_cached(&mut m, &live, time);
                m
            }
            None => live,
        };
        v["type"] = json!("ensemble");
        let m = text(&v);
        self.ensemble_msg = Some(m.clone());
        m
    }

    /// The first audio service (by SId: the same one every time).
    fn first_service(&self) -> Option<ServiceKey> {
        self.ensemble.services.values().filter(|s| s.is_audio()).map(|s| ServiceKey { sid: s.sid, scids: 0 }).min()
    }
}

impl Table<Slot> {
    /// The event channels of the receivers (the radio thread waits on them too).
    pub fn receivers(&self) -> Vec<(u32, Receiver<RxEvent>)> {
        self.entries.iter().map(|e| (e.id, e.slot.rx.events.clone())).collect()
    }

    /// A setting to every receiver.
    pub fn send_all(&self, cmd: impl Fn() -> RxCommand) {
        for e in &self.entries {
            e.slot.rx.send(cmd());
        }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) {
        for e in self.entries.iter_mut() {
            e.slot.audio.set_bitrate(bitrate);
        }
    }
}

/// Stops the receiver and the input of a closed slot: stopping waits for their
/// threads, not on the radio thread.
fn stop(slot: Slot) {
    let Slot { rx, input, .. } = slot;
    let _ = std::thread::Builder::new().name("slot-stop".into()).spawn(move || drop((input, rx)));
}

/// The channel as the band plan writes it ("10c" -> "10C").
fn band_channel(ch: &str) -> Option<String> {
    BAND_III.iter().find(|(n, _)| n.eq_ignore_ascii_case(ch)).map(|(n, _)| n.to_string())
}

fn select_json(key: ServiceKey, reason: &str) -> Out {
    text(&json!({"type": "select", "sid": key.sid, "scids": key.scids, "reason": reason}))
}

impl Radio {
    /// Listeners may choose their own multiplex: the setting, room for a side
    /// slot, channels mapped to streams or recordings, an input that is not
    /// fixed to one channel.
    pub(super) fn own_multiplex_on(&self) -> bool {
        self.settings.listener_multiplex
            && self.settings.listener_multiplex_limit > 0
            && self.fixed_channel.is_none()
            && !self.mapped_channels().is_empty()
    }

    /// A tune of `client` to `channel` goes to a receiver of the listener's
    /// own: listeners (the admin moves the main one), mapped channels.
    pub(super) fn own_multiplex(&self, client: u64, channel: &str) -> bool {
        self.own_multiplex_on() && !self.hub.is_admin(client) && self.mapped_channels().iter().any(|c| c.eq_ignore_ascii_case(channel))
    }

    /// The side slot a client listens to (none: the main receiver).
    fn side_of(&self, client: u64) -> Option<u32> {
        let id = self.hub.slot_of(client);
        (id != 0 && self.slots.get(id).is_some()).then_some(id)
    }

    /// The channel of a slot (0: the main receiver).
    pub(super) fn slot_channel(&self, slot: u32) -> Option<String> {
        match self.slots.get(slot) {
            Some(e) if slot != 0 => Some(e.channel.clone()),
            _ => self.channel.clone(),
        }
    }

    /// The channel a client listens to.
    pub(super) fn channel_of(&self, client: u64) -> Option<String> {
        self.slot_channel(self.hub.slot_of(client))
    }

    /// "Label (SId)" of a service on the multiplex of a slot.
    pub(super) fn service_name_in(&self, slot: u32, sid: u32) -> String {
        match self.slots.get(slot) {
            Some(e) if slot != 0 && sid != FM_SID => service_label(&e.slot.ensemble, sid),
            _ => self.service_name(sid),
        }
    }

    /// A station chosen by a client: of its side slot's multiplex, or of
    /// another multiplex the listener listens to on a receiver of its own.
    /// False: the main receiver's business, as without side slots.
    pub(super) fn select_side(&mut self, client: u64, key: ServiceKey, channel: Option<&str>) -> bool {
        if let Some(id) = self.side_of(client) {
            let here = self.slots.get(id).is_some_and(|e| channel.is_none_or(|c| e.channel.eq_ignore_ascii_case(c)));
            if here {
                self.select_in_slot(client, id, key);
                return true;
            }
        }
        let Some(ch) = channel else { return false };
        // the main receiver's channel (not while it scans) is listened to there
        let main = self.scan.is_none() && self.channel.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(ch));
        if main || !self.own_multiplex(client, ch) {
            return false;
        }
        if !self.channel_allowed(client, ch) {
            self.notice(client, "warning", "channel_not_allowed");
        } else {
            self.join_channel(client, ch, Some(key));
        }
        true
    }

    /// A listener of a side slot chose a station of its multiplex.
    fn select_in_slot(&mut self, client: u64, id: u32, key: ServiceKey) {
        self.initial_pending.remove(&client);
        let Some(e) = self.slots.get(id) else { return };
        // e.g. a station remembered from another multiplex: the first one of
        // this multiplex instead, once its ensemble is complete
        let stable = e.slot.ensemble.eid.is_some() && e.slot.ensemble_changed.elapsed() > Duration::from_secs(3);
        if stable && !e.slot.ensemble.services.contains_key(&key.sid) {
            if let Some(first) = e.slot.first_service() {
                self.hub.send(client, select_json(first, "initial"));
            }
            return;
        }
        self.hub.update(client, |c| c.service = Some(key));
        self.update_decoding();
        self.send_slot_service_data(client, id, key);
        self.log_activity(client);
    }

    /// A listener tunes to a mapped channel on a receiver of its own: the main
    /// one when it is there, else the side slot of the channel, opened when
    /// needed. False when there is no room (the listener stays and is told).
    pub(super) fn join_channel(&mut self, client: u64, channel: &str, service: Option<ServiceKey>) -> bool {
        let Some(ch) = band_channel(channel) else { return false };
        let kind = if self.mapped_file(&ch).is_some() { Kind::Recording } else { Kind::Stream };
        let main = if self.scan.is_some() { None } else { self.channel.as_deref() };
        let limit = self.settings.listener_multiplex_limit;
        let ip = self.client_ip(client);
        let to = match self.slots.place(&ch, main, kind, limit, Instant::now()) {
            Place::Main => 0,
            Place::Slot(id) => id,
            Place::New { evict } => {
                if let Some(id) = evict {
                    self.close_slot(id, "nobody listened, room for another one", false);
                }
                let id = self.slots.next_id();
                match self.open_slot(&ch, id) {
                    Ok(slot) => {
                        self.slots.open(&ch, kind, slot);
                        let what = if kind == Kind::Recording { "recording" } else { "stream" };
                        log::info!(
                            "client {client} ({ip}) listens to {ch} on a receiver of its own ({what}; side receivers: {} of {limit})",
                            self.slots.len()
                        );
                        id
                    }
                    Err(e) => {
                        log::warn!("{ch}: no side receiver for client {client} ({ip}): {e}");
                        self.notice(client, "error", "mux_unavailable");
                        return false;
                    }
                }
            }
            Place::Full => {
                log::info!("client {client} ({ip}) cannot listen to {ch}: all {limit} side receivers in use");
                self.notice(client, "warning", "mux_slots_full");
                return false;
            }
            Place::FullRecordings => {
                log::info!("client {client} ({ip}) cannot listen to {ch}: {MAX_RECORDINGS} recordings play already");
                self.notice(client, "warning", "mux_recordings_full");
                return false;
            }
        };
        self.move_client(client, to, service);
        true
    }

    /// Moves a client to a slot (0: the main receiver) with the station it
    /// chose there (none: it picks one, as after tuning).
    pub(super) fn move_client(&mut self, client: u64, to: u32, service: Option<ServiceKey>) {
        let from = self.hub.slot_of(client);
        if from == to {
            if let (Some(k), true) = (service, to != 0) {
                self.select_in_slot(client, to, k);
            }
            return;
        }
        self.initial_pending.remove(&client);
        self.hub.update(client, |c| {
            c.slot = to;
            c.service = service;
        });
        self.send_slot_view(client);
        if let Some(k) = service {
            if to == 0 {
                self.send_service_data(client, k);
            } else {
                self.send_slot_service_data(client, to, k);
            }
        }
        self.update_decoding();
        self.log_activity(client);
    }

    /// A listener of a side slot comes back to the main receiver (it tuned
    /// it, or chose a station of it); the caller sets what it listens to.
    pub(super) fn back_to_main(&mut self, client: u64) {
        if self.hub.slot_of(client) == 0 {
            return;
        }
        self.hub.update(client, |c| c.slot = 0);
        self.send_slot_view(client);
    }

    /// What a client shows of the receiver it listens to now: the state, the
    /// ensemble, the signal, the time, TII and data services, the DLS lines.
    fn send_slot_view(&mut self, client: u64) {
        let id = self.hub.slot_of(client);
        let state = match self.slots.get(id) {
            Some(e) if id != 0 => self.slot_state_json(e),
            _ => {
                let ens = self.ensemble_message();
                for m in [text(&self.state_json()), ens, text(&self.signal_json()), text(&self.time_json()), text(&self.tii)] {
                    self.hub.send(client, m);
                }
                self.hub.send(client, text(&self.data_progress_json()));
                self.hub.send(client, text(&dls_lines_json(&self.services)));
                if !self.link_source.is_null() {
                    self.hub.send(client, text(&self.link_source));
                }
                return;
            }
        };
        // (DAB/FM linking is the main receiver's)
        let link = json!({"type": "linkInfo", "sid": null, "enabled": self.link_source["enabled"]});
        let Some(e) = self.slots.get_mut(id) else { return };
        let ens = e.slot.ensemble_message(&e.channel, &self.fig_cache);
        let s = &e.slot;
        let tii =
            json!({"type": "tii", "codes": [], "known": [], "eid": s.ensemble.eid.map(|e| format!("{e:04X}")), "frequency": s.frequency});
        for m in [
            text(&state),
            ens,
            text(&s.signal_json()),
            text(&time_json(s.time, &s.ensemble)),
            text(&tii),
            text(&data_progress_json(Some(e.channel.as_str()), &s.ensemble, &s.data_progress)),
            text(&dls_lines_json(&s.services)),
            text(&link),
        ] {
            self.hub.send(client, m);
        }
    }

    fn send_slot_service_data(&self, client: u64, id: u32, key: ServiceKey) {
        let Some(e) = self.slots.get(id) else { return };
        let cached = e.slot.ensemble.eid.and_then(|eid| self.slide_cache.get(&slide_key(&e.channel, eid, key.sid)));
        send_service_state(&self.hub, client, key, e.slot.services.get(&key), cached);
    }

    /// A receiver for a mapped channel, with the settings of the main one.
    fn open_slot(&self, ch: &str, id: u32) -> Result<Slot, String> {
        let frequency = channel_frequency(ch).ok_or("unknown channel")?;
        let url = self.settings.channel_maps.iter().find(|m| m.channel.eq_ignore_ascii_case(ch)).map(|m| m.url.clone()).unwrap_or_default();
        let rx = ReceiverHandle::start();
        rx.send(RxCommand::TiiMode(self.settings.tii_mode));
        rx.send(RxCommand::DataServices(self.settings.spi));
        // (only the station listened to: the texts and slides of every other
        // station cost two thirds of a side receiver)
        rx.send(RxCommand::BackgroundPad(false));
        let input = if let Some(src) = self.mapped_source(ch) {
            rx.set_eti_mode(true);
            // (the receiver takes up the stream at its next start)
            rx.queue.reset();
            Input::Stream(EtiInput::start(src, rx.eti.clone(), Duration::from_millis(self.settings.eti_delay_ms as u64)))
        } else if let Some(path) = self.mapped_file(ch) {
            // the recording in a loop, as the main receiver plays it
            let mut dev = magicdab_sdr::open(&DeviceConfig::RawFile { path, looping: true });
            let queue = rx.queue.clone();
            dev.start(frequency * 1000, Arc::new(move |s| queue.push(s)))?;
            Input::Recording(dev)
        } else {
            return Err("not mapped to a stream or a recording".into());
        };
        Ok(Slot {
            url,
            frequency,
            rx,
            input,
            ensemble: Arc::new(Ensemble::default()),
            labels: Arc::new(LongLabels::new()),
            ensemble_msg: None,
            ensemble_changed: Instant::now(),
            services: HashMap::new(),
            decoding: HashSet::new(),
            wants: HashMap::new(),
            recording: Vec::new(),
            audio: AudioHub::for_slot(self.settings.opus_bitrate, id),
            signal: SignalInfo::default(),
            time: None,
            data_progress: DataProgress::new(),
            pending_clears: HashMap::new(),
            opened: Instant::now(),
        })
    }

    /// A receiver playing a DX recording (an .eti file) for an admin, with
    /// the settings of the main one; `ch` is the channel it was recorded on.
    pub(super) fn open_playback_slot(&self, ch: &str, id: u32, path: &std::path::Path) -> Result<Slot, String> {
        let frequency = channel_frequency(ch).ok_or("unknown channel")?;
        let rx = ReceiverHandle::start();
        rx.send(RxCommand::TiiMode(self.settings.tii_mode));
        rx.send(RxCommand::DataServices(self.settings.spi));
        rx.send(RxCommand::BackgroundPad(false));
        rx.set_eti_mode(true);
        rx.queue.reset();
        let player = crate::dxrec::Player::start(path, rx.eti.clone()).map_err(|e| e.to_string())?;
        Ok(Slot {
            url: player.name.clone(),
            frequency,
            rx,
            input: Input::File(player),
            ensemble: Arc::new(Ensemble::default()),
            labels: Arc::new(LongLabels::new()),
            ensemble_msg: None,
            ensemble_changed: Instant::now(),
            services: HashMap::new(),
            decoding: HashSet::new(),
            wants: HashMap::new(),
            recording: Vec::new(),
            audio: AudioHub::for_slot(self.settings.opus_bitrate, id),
            signal: SignalInfo::default(),
            time: None,
            data_progress: DataProgress::new(),
            pending_clears: HashMap::new(),
            opened: Instant::now(),
        })
    }

    /// Closes a side slot. Its listeners go back to the main receiver: with
    /// what they listened to when it is on their multiplex (`keep`), else
    /// with the main service.
    pub(super) fn close_slot(&mut self, id: u32, why: &str, keep: bool) {
        let Some(e) = self.slots.take(id) else { return };
        let clients = self.hub.ids_in(id);
        let moved = if clients.is_empty() { String::new() } else { format!(", {} listener(s) on the main receiver now", clients.len()) };
        log::info!("{}: side receiver closed ({why}){moved}", e.channel);
        stop(e.slot);
        let main = self.main_service;
        for c in clients {
            let service = self.hub.update(c, |x| {
                x.slot = 0;
                if !keep {
                    x.service = main;
                }
                x.service
            });
            self.send_slot_view(c);
            match (service.flatten(), keep) {
                (Some(k), true) => self.send_service_data(c, k),
                // (as when the main receiver tunes: they follow it)
                (Some(k), false) => self.hub.send(c, select_json(k, "tuned")),
                _ => {}
            }
            self.log_activity(c);
        }
        self.update_decoding();
    }

    /// After a change of the settings: side slots no longer allowed close
    /// (own multiplexes off, the channel map changed, a channel listeners
    /// may not tune to any more).
    pub(super) fn check_slots(&mut self) {
        let on = self.own_multiplex_on();
        let stale: Vec<(u32, &str)> = self
            .slots
            .iter()
            .filter_map(|e| {
                // (a playback is the admin's, not of the channel map)
                if e.kind == Kind::Playback {
                    return None;
                }
                let url = self.settings.channel_maps.iter().find(|m| m.channel.eq_ignore_ascii_case(&e.channel)).map(|m| m.url.as_str());
                if !on {
                    Some((e.id, "listeners' own multiplexes are off"))
                } else if url != Some(e.slot.url.as_str()) {
                    Some((e.id, "its channel map changed"))
                } else if !crate::access::channel_allowed(&self.settings.access.allowed_channels, &e.channel) {
                    Some((e.id, "listeners may not tune to it"))
                } else {
                    None
                }
            })
            .collect();
        for (id, why) in stale {
            self.close_slot(id, why, false);
        }
    }

    /// The services the listeners of each side slot want, decoded there.
    pub(super) fn update_side_decoding(&mut self) {
        let max = self.settings.max_services.max(1);
        for e in self.slots.iter_mut() {
            let (id, s) = (e.id, &mut e.slot);
            let wanted = self.hub.wanted_services(id);
            s.wants = wanted.iter().map(|(k, o, p)| (*k, (*o, *p))).collect();
            let mut keys: Vec<ServiceKey> = wanted.iter().map(|w| w.0).filter(|k| s.ensemble.services.contains_key(&k.sid)).collect();
            keys.sort();
            keys.truncate(max);
            let set: HashSet<ServiceKey> = keys.into_iter().collect();
            for k in set.difference(&s.decoding) {
                s.rx.send(RxCommand::StartService(*k));
            }
            for k in s.decoding.difference(&set) {
                s.rx.send(RxCommand::StopService(*k));
                s.audio.remove(k);
            }
            s.decoding = set;
            let rec = self.hub.recorded_services(id);
            for k in rec.iter().filter(|k| !s.recording.contains(k)) {
                s.rx.send(RxCommand::RecordEncoded(*k, true));
            }
            for k in s.recording.iter().filter(|k| !rec.contains(k)) {
                s.rx.send(RxCommand::RecordEncoded(*k, false));
            }
            s.recording = rec;
        }
    }

    /// The state message of a side slot: the main receiver's with this one's
    /// channel and input (`mainChannel` says where the main one is).
    fn slot_state_json(&self, e: &Entry<Slot>) -> Value {
        let mut v = self.state_json();
        let s = &e.slot;
        v["channel"] = json!(e.channel);
        v["frequency"] = json!(s.frequency);
        v["device"] = json!(s.device());
        v["input"] = json!(if matches!(s.input, Input::Recording(_)) { "rf" } else { "eti" });
        v["etiConnected"] = match &s.input {
            Input::Stream(i) => json!(i.connected.load(Ordering::Relaxed)),
            Input::File(_) => json!(true),
            Input::Recording(_) => Value::Null,
        };
        // the DX recording played back here (dxrec.rs)
        v["playback"] = json!(s.playback_name());
        // (a scan moves only the main receiver)
        v["scan"] = Value::Null;
        v["slot"] = json!(e.id);
        v
    }

    pub(super) fn broadcast_side_state(&self) {
        for e in self.slots.iter() {
            let id = e.id;
            self.hub.broadcast(text(&self.slot_state_json(e)), |c| c.slot == id);
        }
    }

    fn slot_publish_label(&mut self, id: u32, key: ServiceKey, label: DynamicLabel) {
        let Some(e) = self.slots.get_mut(id) else { return };
        e.slot.pending_clears.remove(&key);
        let m = text(&dls_json(key, &label));
        let all = text(&json!({"type": "dlsAll", "sid": key.sid, "text": label.text}));
        e.slot.services.entry(key).or_default().dls = Some(label);
        self.hub.broadcast(m, |c| c.slot == id && c.service == Some(key));
        self.hub.broadcast(all, |c| c.slot == id && c.service != Some(key));
    }

    /// Side slots: one receiver is enough when the main one tunes to the
    /// channel of a side slot; slots nobody listened to for a while close;
    /// DLS clears; stations that are not on the multiplex of their slot.
    pub(super) fn side_tick(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        if self.scan.is_none() {
            if let Some(id) = self.channel.as_deref().and_then(|c| self.slots.find(c)).map(|e| e.id) {
                self.close_slot(id, "the main receiver tuned to it", true);
            }
        }
        if self.slots.swept.elapsed() < Duration::from_secs(1) {
            return;
        }
        let now = Instant::now();
        self.slots.swept = now;
        for e in self.slots.sweep(&self.hub.slot_counts(), now) {
            log::info!("{}: side receiver closed (nobody listened for {} s)", e.channel, e.kind.linger().as_secs());
            stop(e.slot);
        }
        // "clear display" with no new text after it (as on the main receiver)
        let mut clears = Vec::new();
        for e in self.slots.iter() {
            clears.extend(e.slot.pending_clears.iter().filter(|(_, t)| t.elapsed() > Duration::from_secs(3)).map(|(k, _)| (e.id, *k)));
        }
        for (id, key) in clears {
            self.slot_publish_label(id, key, DynamicLabel::default());
        }
        // listeners on a station their multiplex does not carry (remembered
        // from another one) get its first station
        let mut changed = false;
        for e in self.slots.iter() {
            let s = &e.slot;
            let ens = &s.ensemble;
            let settled = s.signal.synced && s.opened.elapsed() > Duration::from_secs(15) && ens.eid.is_some() && !ens.services.is_empty();
            if !settled {
                continue;
            }
            let first = s.first_service();
            for c in self.hub.ids_in(e.id) {
                if self.hub.update(c, |x| x.service.is_some_and(|k| !ens.services.contains_key(&k.sid))).unwrap_or(false) {
                    self.hub.update(c, |x| x.service = first);
                    if let Some(k) = first {
                        self.hub.send(c, select_json(k, "initial"));
                    }
                    changed = true;
                }
            }
        }
        if changed {
            self.update_decoding();
        }
    }

    /// An event of the receiver of a side slot.
    pub(super) fn on_slot_rx(&mut self, id: u32, ev: RxEvent) {
        let Some(Entry { channel, slot: s, kind, .. }) = self.slots.get_mut(id) else { return };
        let channel = channel.as_str();
        match ev {
            RxEvent::Signal(sig) => {
                s.signal = sig;
                self.hub.broadcast(text(&s.signal_json()), |c| c.slot == id);
            }
            RxEvent::Ensemble(ens, labels) => {
                let identified = s.ensemble.eid.is_none() && ens.eid.is_some();
                s.ensemble = ens;
                s.labels = labels;
                s.ensemble_msg = None;
                s.ensemble_changed = Instant::now();
                let m = s.ensemble_message(channel, &self.fig_cache);
                self.hub.broadcast(m, |c| c.slot == id);
                // a recording played back: its first station plays until the
                // admin chooses one
                if *kind == Kind::Playback {
                    if let Some(first) = s.first_service() {
                        for c in self.hub.ids_in(id) {
                            if self.hub.session(c).is_some_and(|x| x.3.is_none()) {
                                self.hub.send(c, select_json(first, "tuned"));
                            }
                        }
                    }
                }
                if identified {
                    // the last slide of the stations of this multiplex, until one comes
                    for c in self.hub.ids_in(id) {
                        let Some(sid) = self.hub.session(c).and_then(|x| x.3) else { continue };
                        let live = s.services.iter().any(|(k, st)| k.sid == sid && st.slide.is_some());
                        let cached = s.ensemble.eid.and_then(|eid| self.slide_cache.get(&slide_key(channel, eid, sid)));
                        if let (false, Some((h, data))) = (live, cached) {
                            self.hub.send(c, binary(BIN_IMAGE, h, data));
                        }
                    }
                }
                self.update_side_decoding();
            }
            RxEvent::Time(t) => {
                s.time = Some(t);
                self.hub.broadcast(text(&time_json(s.time, &s.ensemble)), |c| c.slot == id);
            }
            RxEvent::Audio { key, pcm } => {
                let (o, p) = s.wants.get(&key).copied().unwrap_or((false, false));
                s.audio.push(&self.hub, key, &pcm, o, p);
            }
            RxEvent::AudioFormat { key, format, bitrate } => {
                let m = text(&format_json(key, &format, bitrate));
                s.services.entry(key).or_default().format = Some((format, bitrate));
                self.hub.broadcast(m, |c| c.slot == id && c.service == Some(key));
            }
            RxEvent::Label { key, label } => {
                if label.text.trim().is_empty() {
                    // (a new text often follows a "clear display" at once)
                    s.pending_clears.insert(key, Instant::now());
                    return;
                }
                self.slot_publish_label(id, key, label);
            }
            RxEvent::Stats { key, stats, .. } => {
                let m = text(&stats_json(key, &stats));
                s.services.entry(key).or_default().stats = Some(stats);
                self.hub.broadcast(m, |c| c.slot == id && c.service == Some(key));
            }
            RxEvent::Encoded { key, data, aac } => {
                let h = json!({"sid": key.sid, "aac": aac});
                self.hub.broadcast(binary(BIN_ENCODED, &h, &data), |c| c.slot == id && c.record && c.service == Some(key));
            }
            RxEvent::DataObject { ua_type, object, .. } => {
                // logos and programme guides of the shared store
                if ua_type == 0x007 && self.spi.on_object(&s.ensemble, &object) {
                    self.logos_dirty = true;
                }
            }
            RxEvent::DataProgress { sid, scid, ua_type, progress } => {
                note_data_progress(&mut s.data_progress, channel, sid, scid, ua_type, progress);
                self.hub.broadcast(text(&data_progress_json(Some(channel), &s.ensemble, &s.data_progress)), |c| c.slot == id);
            }
            RxEvent::XpadProgress { key, objects } => {
                let app = slideshow_app(&s.ensemble, key);
                let st = s.services.entry(key).or_default();
                // (a repetition of the picture shown is no new slide)
                let progress = objects.into_iter().find(|(a, _)| *a == app).map(|(_, p)| p).filter(|p| {
                    !st.slide.as_ref().is_some_and(|(h, _)| {
                        h["transportId"] == p.transport_id && h["size"] == p.size && h["contentName"] == p.content_name.as_str()
                    })
                });
                if progress != st.slide_progress {
                    let m = slide_progress_json(key, progress.as_ref());
                    st.slide_progress = progress;
                    self.hub.broadcast(text(&m), |c| c.slot == id && c.service == Some(key));
                }
            }
            RxEvent::XpadObject { key, app_type, object } => {
                if xpad_ua(&s.ensemble, key, app_type) == Some(0x007) {
                    if self.spi.on_object(&s.ensemble, &object) {
                        self.logos_dirty = true;
                    }
                    return;
                }
                // only real pictures (the main receiver keeps the odd ones for inspection)
                if !object.is_image()
                    || app_type != slideshow_app(&s.ensemble, key)
                    || crate::spi::sniff_mime(&object.body) == "application/octet-stream"
                {
                    return;
                }
                let shown = s.services.get(&key).and_then(|st| st.slide.as_ref());
                if shown.is_some_and(|(h, data)| {
                    h["transportId"] == object.transport_id && h["contentName"] == object.content_name.as_str() && **data == object.body
                }) {
                    return;
                }
                let catslide = slide_category(&object).filter(|&(cat, slide)| cat != 0 && slide != 0);
                let h = slide_header(key, &object, catslide);
                let data = Arc::new(object.body);
                self.hub.broadcast(binary(BIN_IMAGE, &h, &data), |c| c.slot == id && c.service == Some(key));
                s.services.entry(key).or_default().slide = Some((h.clone(), data.clone()));
                // the last slide of the station on this multiplex, for whoever tunes there
                if let Some(k) = s.ensemble.eid.map(|eid| slide_key(channel, eid, key.sid)) {
                    self.store_slide_key(k, &h, &data);
                }
            }
            RxEvent::Retuned | RxEvent::Tii { .. } | RxEvent::Spectrum { .. } | RxEvent::ServiceStopped { .. } | RxEvent::Eti(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{Hub, Out};

    fn listeners(v: &[(u32, usize)]) -> HashMap<u32, usize> {
        v.iter().copied().collect()
    }

    #[test]
    fn join_the_main_receiver_or_the_slot_of_the_channel() {
        let mut t: Table<()> = Table::new();
        let now = Instant::now();
        assert_eq!(t.place("10C", Some("10C"), Kind::Stream, 4, now), Place::Main);
        assert_eq!(t.place("10c", Some("10C"), Kind::Stream, 4, now), Place::Main);
        assert_eq!(t.place("8C", Some("10C"), Kind::Recording, 4, now), Place::New { evict: None });
        let id = t.open("8C", Kind::Recording, ());
        assert_eq!(id, 1);
        // one slot per channel, shared by its listeners
        assert_eq!(t.place("8C", Some("10C"), Kind::Recording, 4, now), Place::Slot(id));
        assert_eq!(t.place("8c", None, Kind::Recording, 4, now), Place::Slot(id));
        // while the main receiver scans its channel is not one to join
        assert_eq!(t.place("10C", None, Kind::Stream, 4, now), Place::New { evict: None });
        // ids are not used again
        assert_eq!(t.open("10C", Kind::Stream, ()), 2);
        t.take(2);
        assert_eq!(t.open("10C", Kind::Stream, ()), 3);
    }

    #[test]
    fn a_slot_closes_a_while_after_its_last_listener_left() {
        let mut t: Table<()> = Table::new();
        let id = t.open("8C", Kind::Recording, ());
        let t0 = Instant::now();
        assert!(t.sweep(&listeners(&[(id, 2)]), t0).is_empty());
        assert_eq!(t.get(id).unwrap().idle_since, None);
        // the last one left: kept for LINGER
        assert!(t.sweep(&listeners(&[(0, 3)]), t0 + Duration::from_secs(1)).is_empty());
        assert!(t.sweep(&HashMap::new(), t0 + Duration::from_secs(59)).is_empty());
        // someone came back: no flapping
        assert!(t.sweep(&listeners(&[(id, 1)]), t0 + Duration::from_secs(60)).is_empty());
        assert_eq!(t.get(id).unwrap().idle_since, None);
        // gone again, now for good
        let t1 = t0 + Duration::from_secs(70);
        assert!(t.sweep(&HashMap::new(), t1).is_empty());
        assert!(t.sweep(&HashMap::new(), t1 + LINGER - Duration::from_millis(1)).is_empty());
        let closed = t.sweep(&HashMap::new(), t1 + LINGER);
        assert_eq!(closed.iter().map(|e| e.channel.as_str()).collect::<Vec<_>>(), ["8C"]);
        assert!(t.is_empty());
        assert_eq!(t.place("8C", Some("10C"), Kind::Recording, 4, t1 + LINGER), Place::New { evict: None });
    }

    #[test]
    fn limits_of_side_slots_and_recordings() {
        let mut t: Table<()> = Table::new();
        let now = Instant::now();
        let place = |t: &Table<()>, ch: &str, kind, at| t.place(ch, Some("10C"), kind, 4, at);
        let a = t.open("8C", Kind::Recording, ());
        let b = t.open("5C", Kind::Recording, ());
        // two recordings at most, streams still fit
        assert_eq!(place(&t, "5D", Kind::Recording, now), Place::FullRecordings);
        assert_eq!(place(&t, "11D", Kind::Stream, now), Place::New { evict: None });
        let c = t.open("11D", Kind::Stream, ());
        let d = t.open("12A", Kind::Stream, ());
        assert_eq!(place(&t, "12B", Kind::Stream, now), Place::Full);
        // all four listened to: no room, whatever the kind
        assert!(t.sweep(&listeners(&[(a, 1), (b, 1), (c, 1), (d, 1)]), now).is_empty());
        assert_eq!(place(&t, "5D", Kind::Recording, now), Place::FullRecordings);
        assert_eq!(place(&t, "12B", Kind::Stream, now), Place::Full);
        // a recording nobody listens to makes room for a recording or a
        // stream, once unused for a while (no starting and stopping receivers
        // for someone hopping around)
        t.sweep(&listeners(&[(a, 1), (c, 1), (d, 1)]), now);
        assert_eq!(place(&t, "5D", Kind::Recording, now + Duration::from_secs(9)), Place::FullRecordings);
        assert_eq!(place(&t, "12B", Kind::Stream, now + Duration::from_secs(9)), Place::Full);
        let later = now + SPARE_AFTER;
        assert_eq!(place(&t, "5D", Kind::Recording, later), Place::New { evict: Some(b) });
        assert_eq!(place(&t, "12B", Kind::Stream, later), Place::New { evict: Some(b) });
        // an unused stream makes room only for a stream
        t.sweep(&listeners(&[(a, 1), (b, 1), (d, 1)]), now);
        assert_eq!(place(&t, "5D", Kind::Recording, later), Place::FullRecordings);
        assert_eq!(place(&t, "12B", Kind::Stream, later), Place::New { evict: Some(c) });
        // the longest unused goes first
        t.sweep(&listeners(&[(a, 1), (b, 1)]), now + Duration::from_secs(5));
        assert_eq!(place(&t, "12B", Kind::Stream, later + Duration::from_secs(5)), Place::New { evict: Some(c) });
        // no side slots at all
        let empty: Table<()> = Table::new();
        assert_eq!(empty.place("8C", Some("10C"), Kind::Recording, 0, now), Place::Full);
        assert_eq!(empty.place("10C", Some("10C"), Kind::Stream, 0, now), Place::Main);
    }

    #[test]
    fn the_main_receiver_tuning_to_a_side_slot_takes_its_listeners() {
        let hub = Hub::new();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Out>(16);
        let meta = Default::default();
        let (a, b, c) = (hub.register(tx.clone(), &meta).0, hub.register(tx.clone(), &meta).0, hub.register(tx, &meta).0);
        let mut t: Table<()> = Table::new();
        let id = t.open("8C", Kind::Recording, ());
        // (let in, as the radio thread does with a new connection)
        for x in [a, b, c] {
            hub.update(x, |cl| cl.gated = false);
        }
        for x in [b, c] {
            hub.update(x, |cl| cl.slot = id);
        }
        assert_eq!(hub.slot_counts(), listeners(&[(0, 1), (id, 2)]));
        // the main receiver on 8C: the slot of 8C is taken out, its listeners move
        let e = t.take(t.find("8C").unwrap().id).unwrap();
        for x in hub.ids_in(e.id) {
            hub.update(x, |cl| cl.slot = 0);
        }
        assert!(t.find("8C").is_none());
        assert_eq!(hub.slot_counts(), listeners(&[(0, 3)]));
        assert_eq!(hub.slot_of(a), 0);
        assert_eq!(t.place("8C", Some("8C"), Kind::Recording, 4, Instant::now()), Place::Main);
    }

    #[test]
    fn the_same_station_on_two_multiplexes_reaches_its_own_listeners() {
        let hub = Hub::new();
        let meta = Default::default();
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel::<Out>(64);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel::<Out>(64);
        let (a, b) = (hub.register(tx_a, &meta).0, hub.register(tx_b, &meta).0);
        let key = ServiceKey { sid: 0x1234, scids: 0 };
        for (x, slot) in [(a, 0), (b, 7)] {
            hub.update(x, |c| {
                c.gated = false;
                c.slot = slot;
                c.service = Some(key);
                c.audio = true;
                c.codec = crate::hub::Codec::Pcm;
            });
        }
        assert_eq!(hub.wanted_services(0), vec![(key, false, true)]);
        assert_eq!(hub.wanted_services(7), vec![(key, false, true)]);
        let mut main = AudioHub::new(192_000);
        let mut side = AudioHub::for_slot(192_000, 7);
        let frame = vec![0.25f32; magicdab_codecs::opus::FRAME * 2];
        main.push(&hub, key, &frame, false, true);
        side.push(&hub, key, &frame, false, true);
        side.push(&hub, key, &frame, false, true);
        let count = |rx: &mut tokio::sync::mpsc::Receiver<Out>| std::iter::from_fn(|| rx.try_recv().ok()).count();
        // one frame of the main receiver to A, two of the side slot to B
        assert_eq!(count(&mut rx_a), 1);
        assert_eq!(count(&mut rx_b), 2);
    }
}
