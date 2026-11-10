//! Radio controller: owns the input device and the receiver, keeps the
//! current state, executes client requests and pushes updates to clients.
//! Runs in its own thread; all state is accessed from that thread only.

use crate::audio::AudioHub;
use crate::changes::{self, ChangeTracker};
use crate::config::{self, Location, Settings, Store};
use crate::dtr100input::{self, Dtr100Input};
use crate::etiinput::{self, EtiInput};
use crate::figcache::{self, FigCache};
use crate::fmradio::{FmRadio, FM_KEY, FM_SID};
use crate::gainseek::GainSeeker;
use crate::hub::{self, binary, text, Hub, BIN_ENCODED, BIN_IMAGE, BIN_LOGO, BIN_SPECTRUM};
use crate::linker::{Linker, LinkerConfig};
use crate::linking::{Action, Follower};
use crate::radiodns;
use crate::radiovis;
use crate::rawrec::RawRecorder;
use crate::servicelist::{now, FormatEntry, ServiceList};
use crate::spi::SpiStore;
use crate::txdb::{distance_azimuth, TxDb};
use crate::view::{self, LongLabels};
use crossbeam_channel::{Receiver, Sender};
use magicdab_dab::audio::{AudioFormat, AudioStats};
use magicdab_dab::consts::{channel_frequency, BAND_III, T_G};
use magicdab_dab::dls::DynamicLabel;
use magicdab_dab::ensemble::{DabTime, Ensemble};
use magicdab_dab::receiver::{ReceiverHandle, RxCommand, RxEvent, ServiceKey, SignalInfo};
use magicdab_dab::tii::{TiiCode, TiiMode};
use magicdab_sdr::{DeviceConfig, GainMode, InputDevice};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod access;
mod clips;
mod device;
mod dxrec;
mod gate;
mod incoming;
mod newmux;
mod outgoing;
mod presence;
mod rx;
mod scan;
mod settings;
mod slides;
mod slots;
mod stats;
mod streams;

use outgoing::{
    data_progress_json, dls_json, dls_lines_json, duration, format_json, note_data_progress, send_service_state, service_label,
    signal_json, slide_progress_json, stats_json, text_json_vis, time_json,
};
use rx::{slideshow_app, xpad_ua};
use slides::{load_slide_cache, slide_category, slide_header, slide_key};

pub enum Command {
    Connected(u64),
    /// A client went away: why, how many messages could not be sent to it,
    /// the slot it listened to.
    Gone {
        id: u64,
        session: Option<(String, String, u64, Option<u32>, bool)>,
        reason: String,
        dropped: u64,
        slot: u32,
    },
    Message {
        client: u64,
        msg: Value,
    },
    RadioDnsLogo(radiodns::LogoResult),
    RadioDnsDoc(radiodns::SiDocument),
    VisSlide {
        key: ServiceKey,
        slide: radiovis::VisSlide,
    },
    VisText {
        key: ServiceKey,
        text: String,
    },
    Fm(magicdab_fm::tuner::TunerEvent),
    /// Result of an update check (newer version or none) or an error.
    UpdateChecked(Result<Option<crate::update::Available>, String>),
    /// An update was installed (or failed).
    UpdateInstalled(Result<(), String>),
    /// Audio of the FM tuner (48 kHz interleaved stereo).
    FmAudio(Vec<f32>),
    Shutdown,
}

/// Source code of this program (release builds: MAGICDAB_SOURCE_URL).
pub const SOURCE_URL: &str = match option_env!("MAGICDAB_SOURCE_URL") {
    Some(u) => u,
    None => "",
};

/// Version; release builds set MAGICDAB_VERSION (e.g. 0.3.123, from the commits).
pub const VERSION: &str = match option_env!("MAGICDAB_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Milliseconds on a monotonic clock, for timestamps shared with device threads.
fn now_ms() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Wrong admin passwords allowed from one address and from everyone together
/// before logins are refused, and for how long.
const LOGIN_TRIES: u32 = 8;
const LOGIN_TRIES_ALL: usize = 40;
const LOGIN_LOCK: Duration = Duration::from_secs(15 * 60);

/// Service a client has selected, on which channel, and whether it plays
/// the audio, since when.
struct Activity {
    sid: Option<u32>,
    channel: Option<String>,
    audio: bool,
    since: Instant,
}

#[derive(Default)]
struct ServiceState {
    dls: Option<DynamicLabel>,
    slide: Option<(Value, Arc<Vec<u8>>)>,
    /// Slide being received.
    slide_progress: Option<magicdab_dab::mot::ObjectProgress>,
    /// RadioVIS picture and text.
    vis_slide: Option<(Value, Arc<Vec<u8>>)>,
    vis_text: Option<String>,
    format: Option<(AudioFormat, u16)>,
    stats: Option<AudioStats>,
}

#[derive(PartialEq)]
enum ScanPhase {
    Settle,
    Wait,
    /// A multiplex never heard here is being recorded (radio/dxrec.rs): the
    /// scan stays on it and moves on when the recording ends.
    Hold,
    /// Clips of stations for the server map (radio/clips.rs).
    Clips,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    Fast,
    Normal,
    Precise,
}

impl ScanMode {
    fn dwell(self) -> Duration {
        match self {
            ScanMode::Fast => Duration::from_secs(4),
            ScanMode::Normal => Duration::from_secs(8),
            ScanMode::Precise => Duration::from_secs(16),
        }
    }
}

/// Continuous DX scanning (TII monitoring), does not modify the service list.
struct DxScan {
    mode: ScanMode,
    cycle: u32,
    cycles: u32,
    tii: HashMap<(u8, u8), f32>,
    snr: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DxResult {
    time: i64,
    channel: String,
    frequency: u32,
    ueid: String,
    eid: u16,
    label: String,
    services: usize,
    snr: f64,
    main: Option<u8>,
    sub: Option<u8>,
    level: Option<f64>,
    location: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    power: Option<f32>,
    distance: Option<f64>,
    azimuth: Option<f64>,
    country: Option<String>,
}

struct BandScan {
    channels: Vec<String>,
    index: usize,
    phase: ScanPhase,
    since: Instant,
    found: usize,
    silent: bool,
    return_to: Option<(String, Option<ServiceKey>)>,
    tii: Vec<String>,
    dx: Option<DxScan>,
    /// The channel is being tried a second time at another gain (a strong
    /// neighbour may overload the tuner at the usual one).
    other_gain: bool,
    /// For the server map's scan report: when it began, what each channel
    /// gave, what the current one gave so far.
    started: i64,
    report: Vec<Value>,
    found_here: Vec<Value>,
}

pub struct Radio {
    hub: Arc<Hub>,
    data_dir: PathBuf,
    store: Store,
    pub settings: Settings,
    device: Box<dyn InputDevice>,
    /// What `device` was opened for: the configured tuner, or a recording
    /// mapped to the current channel.
    input: DeviceConfig,
    /// The tuner is given on the command line (it is used again at the next start).
    device_from_args: bool,
    device_running: bool,
    /// When the input device last delivered samples (now_ms).
    samples_at: Arc<AtomicU64>,
    last_service_check: Instant,
    gain_seek: GainSeeker,
    /// Channel requested on the command line (`--channel`).
    channel_arg: Option<String>,
    /// The only channel this input can be tuned to (rtl_tcp, recordings).
    fixed_channel: Option<String>,
    fig_cache: FigCache,
    last_fig_observe: Instant,
    /// ETI / EDI stream feeding the receiver (mapped channel).
    eti_input: Option<EtiInput>,
    /// The DTR-100 dongle as the tuner (dtr100input.rs); it feeds the receiver
    /// like a stream.
    dtr100: Option<Dtr100Input>,
    /// Is the computer fast enough for the receiver (lost samples)?
    overload: Overload,
    /// Reception progress of SPI carousels.
    data_progress: DataProgress,
    tunnel: crate::tunnel::Tunnel,
    /// Listing on the public server map (its requests on a thread of their own).
    server_map: crate::servermap::ServerMap,
    /// DLS "clear display" commands waiting to see whether a new text follows.
    pending_clears: HashMap<ServiceKey, Instant>,
    listen_port: u16,
    last_tunnel_status: String,
    /// Snapshot for the plain HTTP endpoints (/api, /static_data, link previews).
    public: Arc<parking_lot::RwLock<crate::public::PublicInfo>>,
    last_public: Instant,
    rx: ReceiverHandle,
    channel: Option<String>,
    frequency: u32,
    ensemble: Arc<Ensemble>,
    labels: Arc<LongLabels>,
    ensemble_msg: Option<hub::Out>,
    ensemble_changed: Instant,
    awaiting_retune: bool,
    services: HashMap<ServiceKey, ServiceState>,
    decoding: HashSet<ServiceKey>,
    wants: HashMap<ServiceKey, (bool, bool)>,
    audio: AudioHub,
    signal: SignalInfo,
    time: Option<DabTime>,
    tii: Value,
    tii_codes: Vec<TiiCode>,
    service_list: ServiceList,
    /// Unsaved changes of the received ensemble: (first, last).
    list_dirty: Option<(Instant, Instant)>,
    /// Services of the multiplex changing their label all the time.
    dyn_labels: crate::dynlabels::DynamicLabels,
    /// Slides of the services seen during this session (Slides view).
    gallery: crate::gallery::Gallery,
    txdb: TxDb,
    main_service: Option<ServiceKey>,
    scan: Option<BandScan>,
    idle_since: Option<Instant>,
    last_idle_scan: Option<Instant>,
    /// Stations recorded for the server map, and when each was last.
    clips: HashMap<ServiceKey, crate::clips::ClipRecorder>,
    clip_times: crate::clips::ClipTimes,
    /// What every client receives, for the activity log.
    activity: HashMap<u64, Activity>,
    /// New clients that get the main service unless they restore their own
    /// selection first (reconnects after a restart or a network change).
    initial_pending: HashMap<u64, Instant>,
    spectrum_on: bool,
    last_status: Instant,
    settings_dirty: bool,
    spi: SpiStore,
    radiodns: Option<Sender<radiodns::Request>>,
    radiovis: Option<radiovis::RadioVis>,
    fm: Option<FmRadio>,
    /// For threads started later (the FM tuner switched on, updates).
    commands_tx: Option<Sender<Command>>,
    /// Updates: the last check, its result, an installation in progress.
    update_checked: Option<Instant>,
    update_available: Option<crate::update::Available>,
    update_error: Option<String>,
    update_busy: bool,
    /// DAB/FM service following of the main service.
    follower: Follower,
    linker: Option<Linker>,
    /// A/B listening asked for by the admin: kept for a linker made anew
    /// (another station, a station of another multiplex).
    link_ab: bool,
    last_linking_sent: Instant,
    /// What the listeners of the main service hear (told to all on changes).
    link_source: Value,
    /// Services RadioVIS follows.
    vis_wanted: Vec<(ServiceKey, radiovis::Bearer)>,
    /// Last decoded audio per service (gaps are logged).
    last_audio: HashMap<ServiceKey, Instant>,
    /// Synchronisation lost (logged when it comes back after a while).
    sync_lost_at: Option<Instant>,
    /// RadioDNS lookups per ensemble and service.
    radiodns_done: HashMap<(u16, u32), Instant>,
    logos_dirty: bool,
    /// The last new logo, and the last list of them sent to everybody.
    logos_changed: Option<Instant>,
    logos_sent: Option<Instant>,
    dx_results: Vec<DxResult>,
    dx_dirty: bool,
    recording: Vec<ServiceKey>,
    changes: ChangeTracker,
    /// The change log of the last 30 days went to the server map (once a run).
    changes_mapped: bool,
    /// When SPI and the programme guide were last offered to the server map.
    spi_mapped_at: Option<Instant>,
    /// Last slide of every service per multiplex (one ensemble may carry the
    /// slideshow of a station, another not), kept across retunes and restarts
    /// (data/slides).
    slide_cache: HashMap<String, (Value, Arc<Vec<u8>>)>,
    /// Slideshow objects that are not pictures, already reported.
    rejected: HashSet<String>,
    /// Wrong admin passwords per address (count, first) and of everyone
    /// (times): password guessing is stopped for a while.
    login_failures: HashMap<String, (u32, Instant)>,
    login_failures_all: VecDeque<Instant>,
    /// Wrong tuning passwords (limited as the admin login).
    tune_failures: crate::access::Attempts,
    /// Transmitter list arriving in parts (client, text so far).
    txdb_upload: Option<(u64, String)>,
    tuned_at: Instant,
    last_observe: Instant,
    last_reconnect: Instant,
    /// Why the input device could not be started (tried again, logged once).
    device_error: Option<String>,
    raw_recorder: Arc<parking_lot::Mutex<Option<RawRecorder>>>,
    raw_done: Arc<parking_lot::Mutex<Option<PathBuf>>>,
    /// Icecast style streams (icy.rs, radio/streams.rs): the stations listened
    /// to, their encoders, and the station the receiver is locked to.
    streams: Arc<crate::icy::Streams>,
    streams_wanted: HashSet<u32>,
    stream_encoders: HashMap<ServiceKey, (magicdab_codecs::opus::OpusEncoder, Vec<f32>)>,
    streams_policy_at: Instant,
    service_lock: Option<u32>,
    /// Side slots: multiplexes listeners chose themselves, each on a receiver
    /// of its own next to this main one (radio/slots.rs).
    slots: slots::Table<slots::Slot>,
    /// Statistics for admins (radio/stats.rs).
    stats: stats::State,
    /// ETI recordings of multiplexes heard for the first time (radio/dxrec.rs).
    dxrec: dxrec::State,
    /// A multiplex never heard here, listened to for its report (radio/newmux.rs).
    newmux: Option<newmux::Probe>,
}

/// Round to one decimal for display.
fn r1(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}

fn can_tune(hub: &Hub, settings: &Settings, client: u64) -> bool {
    hub.may_tune(client) || !settings.tuner_locked
}

impl Radio {
    pub fn new(
        hub: Arc<Hub>,
        data_dir: PathBuf,
        settings_override: Option<DeviceConfig>,
        fixed_channel: Option<String>,
        listen_port: u16,
        public: Arc<parking_lot::RwLock<crate::public::PublicInfo>>,
        streams: Arc<crate::icy::Streams>,
        service_lock: Option<u32>,
    ) -> Self {
        let store = Store::new(&data_dir);
        let mut settings = store.load();
        let device_from_args = settings_override.is_some();
        if let Some(d) = settings_override {
            settings.device = d;
        }
        if let Ok(pw) = std::env::var("MAGICDAB_ADMIN_PASSWORD") {
            if !pw.is_empty() {
                settings.admin_password_hash = Some(config::hash_password(&pw));
            }
        }
        if settings.admin_password_hash.is_none() {
            let pw: String = (0..10).map(|_| (b'a' + rand::random::<u8>() % 26) as char).collect();
            log::warn!("No admin password configured. Generated admin password: {pw}  (change it in Settings)");
            settings.admin_password_hash = Some(config::hash_password(&pw));
        }
        store.save(&settings);
        crate::access::get().load(&settings.access);
        let service_list = ServiceList::load(&data_dir);
        let fig_cache = FigCache::load(&data_dir);
        let data_dir_for_tunnel = data_dir.clone();
        let server_map = crate::servermap::ServerMap::start(settings.server_map.entry.clone(), crate::servermap::Identity::load(&data_dir));
        let clip_times = crate::clips::ClipTimes::load(&data_dir);
        let txdb = TxDb::load(&data_dir);
        let gain_seek = GainSeeker::load(&data_dir.join("gains.json"));
        let dyn_labels = crate::dynlabels::DynamicLabels::load(&data_dir.join("dynlabels.json"));
        if txdb.count > 0 {
            log::info!("transmitter database: {} entries", txdb.count);
        }
        let device = magicdab_sdr::open(&settings.device);
        let input = settings.device.clone();
        let rx = ReceiverHandle::start();
        rx.send(RxCommand::TiiMode(settings.tii_mode));
        rx.send(RxCommand::DataServices(settings.spi));
        rx.send(RxCommand::BackgroundPad(settings.background_pad));
        let spi = SpiStore::load(&data_dir);
        let changes = ChangeTracker::load(&data_dir);
        let slide_cache = load_slide_cache(&data_dir);
        let audio = AudioHub::new(settings.opus_bitrate);
        let main_service = service_lock.or(settings.last_service).map(|sid| ServiceKey { sid, scids: 0 });
        let follower = Follower::new(&data_dir, settings.linking.enabled, settings.linking.mode);
        Radio {
            hub,
            data_dir,
            store,
            settings,
            device,
            input,
            device_from_args,
            device_running: false,
            samples_at: Arc::new(AtomicU64::new(0)),
            last_service_check: Instant::now(),
            rx,
            channel: None,
            frequency: 0,
            ensemble: Arc::new(Ensemble::default()),
            labels: Arc::new(LongLabels::new()),
            ensemble_msg: None,
            ensemble_changed: Instant::now(),
            awaiting_retune: false,
            services: HashMap::new(),
            decoding: HashSet::new(),
            wants: HashMap::new(),
            audio,
            signal: SignalInfo::default(),
            time: None,
            tii: json!({"type": "tii", "codes": []}),
            tii_codes: Vec::new(),
            service_list,
            list_dirty: None,
            dyn_labels,
            gallery: Default::default(),
            txdb,
            main_service,
            scan: None,
            gain_seek,
            channel_arg: fixed_channel.filter(|c| channel_frequency(c).is_some()),
            fixed_channel: None,
            fig_cache,
            last_fig_observe: Instant::now(),
            eti_input: None,
            dtr100: None,
            overload: Overload::default(),
            data_progress: HashMap::new(),
            tunnel: crate::tunnel::Tunnel::new(&data_dir_for_tunnel),
            server_map,
            pending_clears: HashMap::new(),
            listen_port,
            last_tunnel_status: String::new(),
            public,
            last_public: Instant::now(),
            idle_since: None,
            last_idle_scan: None,
            clips: HashMap::new(),
            clip_times,
            activity: HashMap::new(),
            initial_pending: HashMap::new(),
            spectrum_on: false,
            last_status: Instant::now(),
            settings_dirty: false,
            spi,
            radiodns: None,
            radiovis: None,
            fm: None,
            commands_tx: None,
            update_checked: None,
            update_available: None,
            update_error: None,
            update_busy: false,
            follower,
            linker: None,
            link_ab: false,
            last_linking_sent: Instant::now(),
            link_source: Value::Null,
            vis_wanted: Vec::new(),
            last_audio: HashMap::new(),
            sync_lost_at: None,
            radiodns_done: HashMap::new(),
            logos_dirty: false,
            logos_changed: None,
            logos_sent: None,
            dx_results: Vec::new(),
            dx_dirty: false,
            recording: Vec::new(),
            changes,
            changes_mapped: false,
            // (the first look 5 min after the start)
            spi_mapped_at: Instant::now().checked_sub(Duration::from_secs(1500)),
            slide_cache,
            rejected: HashSet::new(),
            login_failures: HashMap::new(),
            login_failures_all: VecDeque::new(),
            tune_failures: crate::access::Attempts::new(LOGIN_TRIES, LOGIN_TRIES_ALL, LOGIN_LOCK),
            txdb_upload: None,
            tuned_at: Instant::now(),
            last_observe: Instant::now(),
            last_reconnect: Instant::now(),
            device_error: None,
            raw_recorder: Arc::new(parking_lot::Mutex::new(None)),
            raw_done: Arc::new(parking_lot::Mutex::new(None)),
            streams,
            streams_wanted: HashSet::new(),
            stream_encoders: HashMap::new(),
            streams_policy_at: Instant::now(),
            service_lock,
            slots: slots::Table::new(),
            stats: stats::State::new(),
            dxrec: dxrec::State::new(),
            newmux: None,
        }
    }

    pub fn run(mut self, commands: Receiver<Command>) {
        let port = self.listen_port;
        self.tunnel.apply(&self.settings.tunnel, port);
        self.note_tunnel_server();
        self.server_map.configure(crate::servermap::Target::from_settings(&self.settings), false);
        self.fixed_channel = self.compute_fixed_channel();
        if let Some(ch) = self.fixed_channel.clone() {
            log::info!("input with a single channel: staying on {ch}");
            // entries of other channels (e.g. the same stream found on every
            // channel by earlier scans) are meaningless for such an input
            let n = self.service_list.ensembles.len();
            self.service_list.ensembles.retain(|e| e.channel == ch);
            if self.service_list.ensembles.len() != n {
                self.service_list.save(&self.data_dir);
            }
        }
        let start_channel = self
            .fixed_channel
            .clone()
            .or_else(|| {
                // without a tuner: the last channel if it is mapped, else the first mapped one
                if matches!(self.settings.device, DeviceConfig::None) {
                    let maps = self.mapped_channels();
                    self.settings.last_channel.clone().filter(|c| maps.contains(c)).or_else(|| maps.first().cloned())
                } else {
                    None
                }
            })
            .or_else(|| self.settings.last_channel.clone())
            .or_else(|| self.service_list.ensembles.first().map(|e| e.channel.clone()))
            .unwrap_or_else(|| "5C".into());
        self.tune(&start_channel);
        // (without a working input - a stick not plugged in, a driver not
        // started yet - it would find nothing: it starts once the input works)
        if self.service_list.ensembles.is_empty()
            && self.fixed_channel.is_none()
            && (self.device_running || self.eti_input.is_some() || self.dtr100.is_some())
        {
            log::info!("service list is empty, starting band scan");
            self.start_scan(false);
        }
        let events = self.rx.events.clone();
        loop {
            // the receivers of the side slots come and go
            let side = self.slots.receivers();
            let mut sel = crossbeam_channel::Select::new();
            sel.recv(&events);
            sel.recv(&commands);
            for (_, r) in &side {
                sel.recv(r);
            }
            if let Ok(op) = sel.select_timeout(Duration::from_millis(200)) {
                match op.index() {
                    0 => match op.recv(&events) {
                        Ok(e) => self.on_rx(e),
                        // (restarts the program, see the panic hook)
                        Err(_) => panic!("the receiver thread ended"),
                    },
                    1 => match op.recv(&commands) {
                        Ok(Command::Shutdown) | Err(_) => break,
                        Ok(c) => self.on_command(c),
                    },
                    i => {
                        let (id, r) = &side[i - 2];
                        match op.recv(r) {
                            Ok(e) => self.on_slot_rx(*id, e),
                            Err(_) => self.close_slot(*id, "its receiver stopped", false),
                        }
                    }
                }
            }
            self.tick();
        }
        self.device.stop();
        self.save_state();
    }

    // ------------------------------------------------------------------ client commands

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            // (let in with its snapshot, or kept at the gate of a private server)
            Command::Connected(id) => self.on_connected(id),
            Command::Gone { id, session, reason, dropped, slot } => {
                self.initial_pending.remove(&id);
                let act = self.activity.remove(&id);
                if let Some((ip, agent, secs, sid, audio)) = session {
                    let spent =
                        act.filter(|a| a.sid == sid).map(|a| format!(" for {}", duration(a.since.elapsed().as_secs()))).unwrap_or_default();
                    let listened = match (sid, audio) {
                        (Some(s), true) => format!(", listened to {}{spent}", self.service_name_in(slot, s)),
                        (Some(s), false) => format!(", viewed {}{spent}", self.service_name_in(slot, s)),
                        _ => String::new(),
                    };
                    let lost = if dropped > 0 { format!(", {dropped} messages not sent (connection too slow)") } else { String::new() };
                    log::info!("client {id} disconnected after {}: {ip} ({agent}){listened}; {reason}{lost}", duration(secs));
                }
                self.update_decoding();
                self.broadcast_listeners();
            }
            Command::Message { client, msg } => self.on_message(client, msg),
            Command::RadioDnsLogo(l) => {
                if self.spi.add_logo(l.sid, l.ecc, l.width, l.height, &l.data, "RadioDNS", &l.url, l.mime.as_deref()) {
                    self.logos_dirty = true;
                    self.logos_changed = Some(Instant::now());
                }
            }
            Command::RadioDnsDoc(d) => {
                let kind = if d.xml.contains("<serviceInformation") { "serviceInformation" } else { "document" };
                self.spi.add_doc("RadioDNS", &format!("rdns:{}", d.url), kind, &d.url, &d.host, d.xml.len(), &d.xml);
            }
            Command::VisSlide { key, slide } => {
                // still followed (not for a service of an earlier multiplex)
                if !self.vis_wanted.iter().any(|(k, _)| *k == key) {
                    return;
                }
                let name = slide.url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string();
                let h = json!({
                    "kind": "slide",
                    "source": "RadioVIS",
                    "sid": key.sid,
                    "scids": key.scids,
                    "contentName": name,
                    "mime": slide.mime,
                    "size": slide.data.len(),
                    "clickThroughUrl": slide.link,
                    "url": slide.url,
                    "received": now(),
                });
                let data = Arc::new(slide.data);
                self.hub.broadcast(binary(BIN_IMAGE, &h, &data), |c| c.service == Some(key) && c.on_main());
                self.services.entry(key).or_default().vis_slide = Some((h, data));
            }
            Command::VisText { key, text } => {
                if !self.vis_wanted.iter().any(|(k, _)| *k == key) {
                    return;
                }
                self.hub.broadcast(text_json_vis(key, &text), |c| c.service == Some(key) && c.on_main());
                self.services.entry(key).or_default().vis_text = Some(text);
            }
            Command::UpdateChecked(r) => {
                self.update_busy = false;
                match r {
                    Ok(a) => {
                        if let Some(a) = &a {
                            if self.update_available.as_ref().map(|x| &x.version) != Some(&a.version) {
                                log::info!("update available: {} (running {VERSION})", a.version);
                            }
                        }
                        self.update_available = a;
                        self.update_error = None;
                    }
                    Err(e) => {
                        log::warn!("update check: {e}");
                        self.update_error = Some(e);
                    }
                }
                self.broadcast_update();
            }
            Command::UpdateInstalled(r) => match r {
                Ok(()) => {
                    log::info!("restarting after the update");
                    self.hub.broadcast_all(text(&json!({"type": "notice", "level": "info", "message": "update_restart"})));
                    crate::update::request_restart();
                }
                Err(e) => {
                    log::warn!("update failed: {e}");
                    self.update_busy = false;
                    self.update_error = Some(e);
                    self.broadcast_update();
                }
            },
            Command::Fm(e) => {
                let Some(fm) = self.fm.as_mut() else { return };
                let connected = matches!(e, magicdab_fm::tuner::TunerEvent::Connected(_));
                let scan = matches!(e, magicdab_fm::tuner::TunerEvent::Scan(_));
                if let magicdab_fm::tuner::TunerEvent::Antenna(a) = e {
                    if self.settings.fm.antenna != a {
                        self.settings.fm.antenna = a;
                        self.settings_dirty = true;
                    }
                }
                if let Some(khz) = fm.on_event(e) {
                    if self.settings.fm.khz != khz {
                        self.settings.fm.khz = khz;
                        self.settings_dirty = true;
                    }
                }
                if connected {
                    fm.antennas = self.settings.fm.antennas.clamp(1, 4);
                    if fm.antennas > 1 {
                        fm.tuner.antenna(self.settings.fm.antenna.min(fm.antennas - 1));
                    }
                    // (a new tuner starts at the bottom of the band: what it shows is where it is)
                    fm.tuner.tune(if self.settings.fm.khz > 0 { self.settings.fm.khz } else { 87_500 });
                }
                if scan {
                    self.hub.broadcast_all(text(&fm.scan_json()));
                }
            }
            Command::FmAudio(pcm) => {
                let (o, p) = self.wants.get(&FM_KEY).copied().unwrap_or((false, false));
                self.audio.push(&self.hub, FM_KEY, &pcm, o, p);
                if let (Some(l), Some(key)) = (self.linker.as_mut(), self.follower.service()) {
                    let out = l.push_fm(&pcm, Instant::now());
                    if !out.is_empty() {
                        let (o, p) = self.wants.get(&key).copied().unwrap_or((false, false));
                        self.audio.push(&self.hub, key, &out, o, p);
                    }
                }
            }
            Command::Shutdown => {}
        }
    }

    /// The main service and the listeners on a station that is not on this
    /// multiplex (remembered by a page or the settings from another day, and
    /// accepted while the ensemble was still coming in) move to one that is.
    fn check_services(&mut self) {
        if self.last_service_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_service_check = Instant::now();
        let ens = self.ensemble.clone();
        let settled = self.scan.is_none() && self.signal.synced && self.tuned_at.elapsed() > Duration::from_secs(15) && ens.eid.is_some();
        if !settled || ens.services.is_empty() {
            return;
        }
        let here = |k: &ServiceKey| *k == FM_KEY || ens.services.contains_key(&k.sid);
        let mut changed = false;
        // (the station a receiver is locked to stays, even while it is off the air)
        if let Some(m) = self.main_service.filter(|m| !here(m) && self.service_lock.is_none()) {
            let first = ens.services.values().find(|s| s.is_audio()).map(|s| ServiceKey { sid: s.sid, scids: 0 });
            log::info!(
                "main service {:X} is not on this multiplex: {}",
                m.sid,
                first.map(|k| self.service_name(k.sid)).unwrap_or_else(|| "none".into())
            );
            self.main_service = first;
            self.settings.last_service = first.map(|k| k.sid);
            self.settings_dirty = true;
            changed = true;
        }
        for id in self.hub.ids_in(0) {
            if self.hub.update(id, |c| c.service.is_some_and(|k| !here(&k))).unwrap_or(false) {
                let main = self.main_service;
                self.hub.update(id, |c| c.service = main);
                self.send_initial_service(id);
                changed = true;
            }
        }
        if changed {
            self.update_decoding();
        }
    }

    fn send_initial_service(&self, client: u64) {
        if let Some(m) = self.main_service {
            self.hub.send(client, text(&json!({"type": "select", "sid": m.sid, "scids": m.scids, "reason": "initial"})));
        }
    }

    /// State of the input for log lines about interruptions.
    fn input_state(&self) -> String {
        if let Some(d) = self.dtr100.as_ref().filter(|_| self.dongle_active()) {
            let st = d.status();
            return match (st.connected, st.locked, st.cer) {
                (false, _, _) => format!("DTR-100 not connected ({})", st.error.unwrap_or_default()),
                (true, None, _) => "DTR-100 without an ensemble".to_string(),
                (true, Some(_), cer) => format!("DTR-100 locked, CER {}", cer.map(|c| c.to_string()).unwrap_or_else(|| "-".into())),
            };
        }
        match &self.eti_input {
            Some(e) if e.connected.load(std::sync::atomic::Ordering::Relaxed) => "EDI/ETI input connected".to_string(),
            Some(_) => "EDI/ETI input disconnected".to_string(),
            None if self.signal.synced => format!("signal synchronised, SNR {:.1} dB", self.signal.snr),
            None => "no synchronisation".to_string(),
        }
    }

    /// DAB/FM linking of the main service: one step of the follower.
    fn step_linking(&mut self) {
        let fm_ok = self.fm.as_ref().is_some_and(|f| f.connected);
        // The one FM tuner backs up the service of the logged in admin;
        // nobody logged in: no search, the FM tuner is
        // left alone (a visitor looking at a station used to move it, band
        // scan and all).
        let key = if self.follower.enabled && fm_ok && self.scan.is_none() {
            self.hub.admin_service(0).filter(|k| *k != FM_KEY && self.ensemble.services.contains_key(&k.sid))
        } else {
            None
        };
        if key != self.follower.service() {
            self.linker = key.map(|_| {
                let mut l = Linker::new(LinkerConfig {
                    max_delay: self.settings.linking.max_delay_ms as f32 / 1000.0,
                    match_level: self.settings.linking.match_level,
                    ..Default::default()
                });
                l.ab = self.link_ab;
                l
            });
        }
        // slow FIGs (0/6 links, 0/21 frequencies) from the cache count at once
        let merged = self.channel.as_deref().and_then(|c| self.fig_cache.merged(c, &self.ensemble, &self.labels)).map(|m| m.0);
        self.follower.set_service(key, merged.as_ref().unwrap_or(&self.ensemble));
        let Some(fm) = self.fm.as_ref() else {
            let src = json!({"type": "linkInfo", "sid": null, "enabled": false});
            if src != self.link_source {
                self.hub.broadcast_main(text(&src));
                self.link_source = src;
            }
            return;
        };
        let view = fm.view();
        let in_use = self.hub.any(|c| c.service == Some(FM_KEY));
        for a in self.follower.step(Instant::now(), &view, in_use, self.linker.as_mut()) {
            match a {
                Action::Tune(khz) => fm.tuner.tune(khz),
                Action::Antenna(n) => fm.tuner.antenna(n),
                Action::Scan => fm.tuner.scan(87_500, 108_000, 100),
            }
        }
        self.follower.save_if_dirty();
        let src = self.follower.summary(&view, self.linker.as_ref());
        if src != self.link_source {
            self.hub.broadcast_main(text(&src));
            self.link_source = src;
        }
        // (the linking view is the admin's only)
        if self.last_linking_sent.elapsed() > Duration::from_millis(1000) && self.hub.any(|c| c.subs.linking && c.admin) {
            self.last_linking_sent = Instant::now();
            let v = self.follower.json(&view, self.linker.as_ref());
            self.hub.broadcast(text(&v), |c| c.subs.linking && c.admin);
        }
    }

    fn update_json(&self) -> Value {
        json!({
            "type": "update",
            "current": VERSION,
            "platform": crate::update::platform(),
            "container": crate::update::in_container(),
            "url": crate::update::url(&self.settings.update_url),
            "customUrl": self.settings.update_url,
            "defaultUrl": crate::update::DEFAULT_URL,
            "auto": self.settings.update_auto,
            "available": self.update_available,
            "error": self.update_error,
            "busy": self.update_busy,
            "checked": self.update_checked.map(|t| t.elapsed().as_secs()),
        })
    }

    fn broadcast_update(&self) {
        self.hub.broadcast(text(&self.update_json()), |c| c.admin);
    }

    fn install_update(&mut self) {
        let (Some(a), Some(tx)) = (self.update_available.clone(), self.commands_tx.clone()) else { return };
        self.update_busy = true;
        std::thread::spawn(move || {
            let _ = tx.send(Command::UpdateInstalled(crate::update::install(&a)));
        });
    }

    fn check_update(&mut self) {
        let (Some(tx), url) = (self.commands_tx.clone(), crate::update::url(&self.settings.update_url)) else { return };
        self.update_checked = Some(Instant::now());
        self.update_busy = true;
        std::thread::spawn(move || {
            let _ = tx.send(Command::UpdateChecked(crate::update::check(&url, VERSION)));
        });
    }

    fn client_ip(&self, client: u64) -> String {
        self.hub.session(client).map(|s| s.0).unwrap_or_default()
    }

    /// "Label (SId)" of a service of the current ensemble.
    /// Stations whose logos a listener can see: those of the service list and
    /// of the multiplex heard now.
    fn logo_sids(&self) -> HashSet<u32> {
        let mut sids: HashSet<u32> = self.service_list.ensembles.iter().flat_map(|e| e.services.iter().map(|x| x.sid)).collect();
        sids.extend(self.ensemble.services.keys());
        // (the logos of the multiplexes, by their EId)
        sids.extend(self.service_list.ensembles.iter().map(|e| e.eid as u32));
        sids.extend(self.ensemble.eid.map(u32::from));
        sids
    }

    fn service_name(&self, sid: u32) -> String {
        if sid == FM_SID {
            return self.fm.as_ref().map(|f| f.name()).unwrap_or_else(|| "FM".into());
        }
        service_label(&self.ensemble, sid)
    }

    /// Log what a client receives when it changed: the service it selected and
    /// whether it plays the audio, with the time spent on the previous one.
    fn log_activity(&mut self, client: u64) {
        let Some((ip, _, _, sid, audio)) = self.hub.session(client) else { return };
        // (a listener of a side slot: its multiplex)
        let slot = self.hub.slot_of(client);
        let channel = self.channel_of(client);
        let prev = self.activity.get(&client);
        if prev.is_some_and(|p| p.sid == sid && p.audio == audio && p.channel == channel) {
            return;
        }
        let name = |s: Option<u32>| s.map(|s| self.service_name_in(slot, s)).unwrap_or_else(|| "-".into());
        let ch = channel.clone().unwrap_or_default();
        let msg = match prev {
            Some(p) => {
                let spent = format!("{} {}", if p.audio { "listened" } else { "viewed" }, duration(p.since.elapsed().as_secs()));
                if p.sid != sid || p.channel != channel {
                    format!("{} → {} [{ch}]{} ({spent})", name(p.sid), name(sid), if audio { "" } else { ", no audio" })
                } else if audio {
                    format!("listening to {} [{ch}]", name(sid))
                } else {
                    format!("stopped {} ({spent})", name(sid))
                }
            }
            None if audio => format!("listening to {} [{ch}]", name(sid)),
            None => format!("viewing {} [{ch}]", name(sid)),
        };
        log::info!("client {client} ({ip}): {msg}");
        self.activity.insert(client, Activity { sid, channel, audio, since: Instant::now() });
    }

    fn broadcast_listeners(&self) {
        self.hub.broadcast_all(text(&json!({"type": "listeners", "count": self.hub.count()})));
    }

    /// Record differences of the received ensemble (mux diff) and notify.
    fn observe_changes(&mut self) {
        let Some(ch) = self.channel.clone() else { return };
        let long = self.tuned_at.elapsed() > Duration::from_secs(120);
        // services with dynamic labels by their usual name: no change every few seconds
        let ens = self.dyn_labels.stable(&self.ensemble);
        if let Some(mut ev) = self.changes.observe(&ch, &ens, long, &self.dyn_labels.sids(), &self.settings.notify_mute) {
            log::info!("ensemble changes on {}: {} item(s)", ch, ev.changes.len());
            // a new station, another line-up: recorded (radio/dxrec.rs); a new
            // multiplex has its recording in its report (radio/newmux.rs)
            let recording = self.dxrec_on_changes(&ev);
            if !ev.changes.iter().any(|c| c.kind == "ensembleNew") {
                ev.recording = recording;
            }
            self.hub.broadcast_all(text(&json!({"type": "change", "event": ev})));
            // (Discord gets the changes of the multiplex together, 5 minutes later
            // - tick - but a new multiplex at once: an opening may not last)
            let discord = if ev.changes.iter().any(|c| c.kind == "ensembleNew") {
                Some(changes::post_discord(&self.settings.discord_webhook, &self.settings.station_name, None, &ev))
            } else {
                self.changes.defer(&ev);
                None
            };
            // (its signal and transmitters a minute later, into that message)
            if let Some(d) = discord {
                self.newmux_start(&ev, d);
            }
            // (the server map: multiplexes received on air only)
            if crate::servermap::source(&self.settings, &ch) == "air" {
                self.server_map.send_changes([&ev]);
            }
        }
    }

    /// The SI of the stations of the multiplex as just received (spi.rs):
    /// its changes into the change log and, 5 minutes later, to Discord
    /// (not to the server map: it compares the structure).
    fn observe_si(&mut self) {
        let seen = std::mem::take(&mut self.spi.si_seen);
        let logos = std::mem::take(&mut self.spi.logo_seen);
        let Some(ch) = self.channel.clone().filter(|_| !(seen.is_empty() && logos.is_empty()) && !self.awaiting_retune) else { return };
        if let Some(ev) = self.changes.observe_si(&ch, &self.ensemble, seen, logos, &self.settings.notify_mute) {
            log::info!("SI changes on {}: {} item(s)", ch, ev.changes.len());
            self.hub.broadcast_all(text(&json!({"type": "change", "event": ev})));
            self.changes.defer(&ev);
        }
    }

    fn broadcast_raw_state(&self) {
        let v = self.raw_state();
        self.hub.broadcast(text(&v), |c| c.admin);
    }

    /// The recording made now and the last ones (to download: /raw/<name>).
    fn raw_state(&self) -> Value {
        let st = self.raw_recorder.lock().as_ref().map(|r| {
            (
                r.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                r.samples as f64 / 2_048_000.0,
                r.limit as f64 / 2_048_000.0,
            )
        });
        let files: Vec<Value> = crate::rawrec::list(&self.data_dir.join(crate::rawrec::DIR), 10)
            .into_iter()
            .filter(|(n, _, _)| st.as_ref().is_none_or(|s| &s.0 != n))
            .map(|(name, size, time)| json!({"name": name, "size": size, "time": time}))
            .collect();
        match st {
            Some((f, pos, len)) => json!({"type": "rawRecord", "active": true, "file": f, "position": pos, "length": len, "files": files}),
            None => json!({"type": "rawRecord", "active": false, "files": files}),
        }
    }

    // ------------------------------------------------------------------ periodic work

    fn tick(&mut self) {
        self.check_overload();
        self.streams_tick();
        if self.scan.is_some() {
            self.scan_step();
        }
        self.stats_tick();
        if let Some(v) = self.fm.as_mut().and_then(|f| f.due()) {
            self.hub.broadcast_all(text(&v));
        }
        self.step_linking();
        let due = self.update_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(12 * 3600));
        if due && !crate::update::url(&self.settings.update_url).is_empty() && !self.update_busy && !crate::update::in_container() {
            self.check_update();
        }
        // by itself when no one listens (once per check: not again after an error)
        if self.settings.update_auto
            && self.update_available.is_some()
            && self.update_error.is_none()
            && !self.update_busy
            && self.hub.audio_count() == 0
        {
            log::info!("installing update {} (no listeners)", self.update_available.as_ref().map_or("", |a| a.version.as_str()));
            self.install_update();
            self.broadcast_update();
        }
        let due: Vec<u64> =
            self.initial_pending.iter().filter(|(_, t)| t.elapsed() > Duration::from_millis(600)).map(|(&c, _)| c).collect();
        for client in due {
            self.initial_pending.remove(&client);
            if self.hub.session(client).is_some_and(|s| s.3.is_none()) {
                self.send_initial_service(client);
            }
        }
        if self.last_fig_observe.elapsed() > Duration::from_secs(60) {
            // keeps the cache confirmed while nothing changes in the ensemble
            self.observe_fig_cache();
        }
        self.fig_cache.save_if_due();
        self.spi.save_if_due();
        self.check_services();
        self.side_tick();
        self.playback_tick();
        self.dxrec_tick();
        self.newmux_tick();
        let due: Vec<ServiceKey> =
            self.pending_clears.iter().filter(|(_, t)| t.elapsed() > Duration::from_secs(3)).map(|(k, _)| *k).collect();
        for key in due {
            self.pending_clears.remove(&key);
            self.publish_label(key, DynamicLabel::default());
        }
        if self.last_public.elapsed() > Duration::from_secs(1) {
            self.last_public = Instant::now();
            self.update_public();
        }
        let ts = self.tunnel.status();
        let key = format!("{}|{}", ts.state, ts.message);
        if key != self.last_tunnel_status {
            self.last_tunnel_status = key;
            self.hub.broadcast(text(&json!({"type": "tunnelStatus", "status": ts})), |c| c.admin);
        }
        // the public server map: what it is told, the entry it issued
        if self.server_map.snapshot_due() {
            let s = crate::servermap::Snapshot::build(&self.settings, &self.public.read(), &self.service_list, self.scan.is_some());
            // SPI (names, logos) and the programme guide of the stations heard, looked
            // at every 30 min (the first time 5 min after the start): only a change goes
            let due = self.spi_mapped_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1800));
            if due && self.server_map.listed() {
                self.spi_mapped_at = Some(Instant::now());
                if let Some(u) = crate::servermap::spi_upload(&self.spi, &s) {
                    self.server_map.send_spi(u);
                }
                if let Some((body, key)) = crate::servermap::epg_upload(&self.spi, &s, crate::servicelist::now()) {
                    self.server_map.send_epg(body, key);
                }
            }
            self.server_map.update(s);
            // the change log of the last 30 days, once a run (the map keeps
            // each event once): its history then has what came before it
            if !self.changes_mapped && self.server_map.listed() {
                self.changes_mapped = true;
                let since = crate::servicelist::now() - 30 * 86400;
                let settings = &self.settings;
                self.server_map.send_changes(
                    self.changes.recent.iter().filter(|e| e.time >= since && crate::servermap::source(settings, &e.channel) == "air"),
                );
            }
        }
        if let Some(entry) = self.server_map.entry_changed() {
            self.settings.server_map.entry = entry;
            self.settings_dirty = true;
        }
        if let Some(st) = self.server_map.status_changed() {
            self.hub.broadcast(text(&json!({"type": "serverMap", "status": st})), |c| c.admin);
        }
        let due = self.changes.due_for_discord();
        if !due.is_empty() {
            let base = crate::servermap::Target::from_settings(&self.settings).url;
            for ev in due {
                changes::post_discord(&self.settings.discord_webhook, &self.settings.station_name, base.as_deref(), &ev);
            }
        }
        if self.spi.docs_dirty {
            self.spi.docs_dirty = false;
            self.hub.broadcast_all(text(&self.spi.docs_json()));
        }
        // remember the received ensemble in the service list once it is stable
        if let Some((first, last)) = self.list_dirty {
            // stable for 5 s, or every 30 s while it keeps changing
            if (last.elapsed() > Duration::from_secs(5) || first.elapsed() > Duration::from_secs(30)) && self.scan.is_none() {
                self.list_dirty = None;
                if let Some(ch) = self.channel.clone() {
                    let ens = self.dyn_labels.stable(&self.ensemble);
                    let mut changed = self.service_list.update(&ch, self.frequency, &ens, self.signal.snr);
                    // (audio decoded before the multiplex was in the list)
                    if let Some(eid) = ens.eid {
                        for (k, s) in &self.services {
                            if let (0, Some((f, _))) = (k.scids, s.format.as_ref()) {
                                changed |= self.service_list.set_format(&ch, eid, k.sid, &FormatEntry::of(f));
                            }
                        }
                    }
                    if changed {
                        self.service_list.save(&self.data_dir);
                        self.hub.broadcast_all(text(&self.service_list_json()));
                        // (logos of stations new to the list)
                        self.logos_dirty = true;
                        self.logos_changed = Some(Instant::now());
                    }
                }
                self.observe_changes();
            }
        }
        // idle background scan
        // (a recording or the streams of a server without a tuner: nothing to scan)
        let no_band = matches!(self.settings.device, DeviceConfig::RawFile { .. } | DeviceConfig::None);
        if self.settings.idle_scan.enabled && self.scan.is_none() && !no_band {
            // the tuner is free while nobody listens (an open page does not use
            // it, a stream's player does)
            if !self.hub.any(|c| c.audio) && self.streams_wanted.is_empty() {
                let since = *self.idle_since.get_or_insert_with(Instant::now);
                let idle = Duration::from_secs(self.settings.idle_scan.idle_minutes as u64 * 60);
                let interval = Duration::from_secs(self.settings.idle_scan.interval_minutes as u64 * 60);
                let due = self.last_idle_scan.map(|t| t.elapsed() >= interval).unwrap_or(true);
                if since.elapsed() >= idle && due {
                    self.last_idle_scan = Some(Instant::now());
                    self.start_scan(true);
                }
            } else {
                self.idle_since = None;
            }
        }
        // (the lock is let go before the recorder's is taken: the sample thread
        // holds that one when it hands a finished recording over - a guard of an
        // `if let` would live through the whole if/else and deadlock both)
        let finished = self.raw_done.lock().take();
        if let Some(p) = finished {
            log::info!("raw recording finished: {}", p.display());
            self.broadcast_raw_state();
        } else if self.raw_recorder.lock().is_some() && self.last_status.elapsed() > Duration::from_secs(1) {
            self.broadcast_raw_state();
        }
        if self.dx_dirty {
            self.dx_dirty = false;
            self.hub.broadcast_all(text(&self.dx_json()));
        }
        // (the whole list goes to everybody: once a burst of logos is over -
        // RadioDNS brings hundreds within seconds - and not more often than
        // every half minute)
        let quiet = self.logos_changed.is_some_and(|t| t.elapsed() > Duration::from_secs(5));
        if self.logos_dirty && quiet && self.logos_sent.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.logos_dirty = false;
            self.logos_sent = Some(Instant::now());
            self.hub.broadcast_all(text(&self.spi.list_json(&self.logo_sids())));
            self.hub.broadcast_all(text(&self.spi.epg_index_json()));
        }
        if self.scan.is_none() && self.ensemble.eid.is_some() && self.last_observe.elapsed() > Duration::from_secs(150) {
            self.last_observe = Instant::now();
            self.observe_changes();
        }
        // reconnect devices whose stream ended or that deliver no samples (stick
        // unplugged or reset, USB stall, server gone), and network devices that
        // deliver only empty samples
        if self.device_running && !matches!(self.input, DeviceConfig::RawFile { .. } | DeviceConfig::None) {
            let connected = self.device.status().connected;
            let quiet = now_ms().saturating_sub(self.samples_at.load(Ordering::Relaxed)) > 3000;
            if quiet && self.signal.signal {
                // the receiver waits for samples: its last signal state would stay on screen
                self.signal = SignalInfo::default();
                self.hub.broadcast_main(text(&self.signal_json()));
            }
            // (a stick behind a local rtl_tcp may be tuned to an empty channel)
            let stalled = !self.signal.signal && self.tuned_at.elapsed() > Duration::from_secs(30) && self.scan.is_none();
            let remote_tcp = matches!(self.settings.device, DeviceConfig::RtlTcp { .. }) && !self.settings.device.local_rtltcp();
            if (!connected || quiet || (stalled && remote_tcp)) && self.last_reconnect.elapsed() > Duration::from_secs(20) {
                self.last_reconnect = Instant::now();
                let why = if !connected {
                    "disconnected"
                } else if quiet {
                    "delivers no samples"
                } else {
                    "delivers no signal"
                };
                log::warn!("input device {why} - reconnecting");
                self.device.stop();
                let f = self.frequency;
                self.start_device(f);
                self.rx.queue.reset();
                self.awaiting_retune = true;
                self.tuned_at = Instant::now();
            }
        }
        // a device that could not be started (not plugged in, no driver yet, server down);
        // a local rtl_tcp refuses at once until its driver is up: tried more often
        let retry = Duration::from_secs(if self.settings.device.local_rtltcp() { 2 } else { 10 });
        if !self.device_running
            && self.device_error.is_some()
            && self.eti_input.is_none()
            && self.scan.is_none()
            && self.frequency > 0
            && self.last_reconnect.elapsed() > retry
        {
            self.last_reconnect = Instant::now();
            let f = self.frequency;
            self.start_device(f);
            if self.device_running {
                self.apply_gain_ceiling();
                self.rx.queue.reset();
                self.awaiting_retune = true;
                self.tuned_at = Instant::now();
                // the first start had no input to scan with
                if self.service_list.ensembles.is_empty() && self.fixed_channel.is_none() {
                    log::info!("service list is empty, starting band scan");
                    self.start_scan(false);
                }
            }
        }
        if self.last_status.elapsed() > Duration::from_secs(2) {
            self.last_status = Instant::now();
            // a label that stopped scrolling: the service is shown as it is again
            let settled = self.dyn_labels.settle(Instant::now());
            if !settled.is_empty() {
                for sid in &settled {
                    log::info!("the label of {} no longer changes: shown as it is", self.service_name(*sid));
                }
                self.ensemble_msg = None;
                let m = self.ensemble_message();
                self.hub.broadcast_main(m);
                let now = Instant::now();
                self.list_dirty = Some((self.list_dirty.map_or(now, |d| d.0), now));
            }
            if self.signal.frames == 0 || !self.device_running {
                // keep clients informed while there is no signal
                self.hub.broadcast_main(text(&self.signal_json()));
            }
            // bans of floods (the WebSocket tasks) and ended ones (the upkeep)
            if crate::access::get().take_changed() {
                self.sync_bans();
            }
            self.presence_tick();
            if self.settings_dirty {
                self.settings_dirty = false;
                self.store.save(&self.settings);
            }
            if self.gain_seek.learned_dirty {
                self.gain_seek.save(&self.data_dir.join("gains.json"));
            }
            self.dyn_labels.save(&self.data_dir.join("dynlabels.json"), false);
        }
    }

    fn save_state(&mut self) {
        self.spi.flush();
        self.dyn_labels.save(&self.data_dir.join("dynlabels.json"), true);
        self.store.save(&self.settings);
        self.service_list.save(&self.data_dir);
    }
}

/// Lost input of the receiver (Radio::check_overload).
#[derive(Default)]
struct Overload {
    at: Option<Instant>,
    pushed: u64,
    dropped: u64,
    frames: u64,
    /// When input was lost the last time.
    since: Option<Instant>,
    /// % of the input lost then, and the receiver thread's CPU (% of a processor).
    lost: f32,
    cpu: Option<f32>,
    logged: Option<Instant>,
    sampler: crate::sysstats::Sampler,
}

impl Overload {
    /// Lost input within the last 30 s.
    fn active(&self) -> bool {
        self.since.is_some_and(|t| t.elapsed() < Duration::from_secs(30))
    }
}

/// Reception of the SPI carousels of a multiplex: (SId, SCId) -> (UA type, progress, complete logged).
type DataProgress = HashMap<(u32, u16), (u16, magicdab_dab::mot::CarouselProgress, bool)>;

pub fn spawn(mut radio: Radio) -> (Sender<Command>, std::thread::JoinHandle<()>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    radio.radiodns = Some(radiodns::spawn(tx.clone()));
    radio.radiovis = Some(radiovis::RadioVis::spawn(tx.clone()));
    if radio.settings.fm.enabled && crate::fmradio::AVAILABLE {
        radio.fm = Some(FmRadio::start(radio.settings.fm.port.clone(), radio.settings.fm.audio.clone(), tx.clone()));
    }
    radio.commands_tx = Some(tx.clone());
    let h = std::thread::Builder::new().name("radio".into()).spawn(move || radio.run(rx)).expect("radio thread");
    (tx, h)
}
