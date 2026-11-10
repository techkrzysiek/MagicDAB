//! Receiver core: ties the OFDM demodulator, FIC/FIG decoding, MSC
//! sub-channel decoding, audio decoders and the TII decoder together.
//!
//! The receiver runs in its own thread, pulls samples from a [`SampleQueue`]
//! and reports everything through [`RxEvent`]s.

use crate::audio::{AudioDecoder, AudioFormat, AudioStats, ServiceEvent};
use crate::consts::{CIFS_PER_FRAME, CIF_BITS};
use crate::dls::DynamicLabel;
use crate::ensemble::{DabTime, Ensemble};
use crate::etirec::EtiRecorder;
use crate::fic::FicDecoder;
use crate::fig::FigParser;
use crate::mot::MotDecoder;
use crate::mot::MotObject;
use crate::msc::SubchannelDecoder;
use crate::ofdm::{Frame, FrameSink, OfdmDemodulator, SampleSource};
use crate::packet::PacketDecoder;
use crate::tii::{TiiCode, TiiDecoder, TiiMode};
use crossbeam_channel::{Receiver, Sender};
use num_complex::Complex32;
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Identifies an audio service component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
pub struct ServiceKey {
    pub sid: u32,
    pub scids: u8,
}

/// Where the receiver's time went since it started (the server's statistics
/// read it now and then and take the differences).
#[derive(Clone, Debug, Default)]
pub struct Timing {
    /// CPU time of the receiver thread, all of it (OFDM demodulation is what
    /// is left after the parts below).
    pub thread: Duration,
    /// FIC decoding and the ensemble information.
    pub fic: Duration,
    /// Per service listened to: its subchannel (time deinterleaving, Viterbi,
    /// Reed-Solomon) and its audio (AAC or MP2 decoding, PAD).
    pub services: HashMap<ServiceKey, (Duration, Duration)>,
    /// Texts and slides of the other services (background decoding).
    pub background: Duration,
    /// Data services (SPI, packet mode).
    pub data: Duration,
    pub tii: Duration,
    /// The ETI recording: the sub-channels nothing else decodes, the frames.
    pub recording: Duration,
}

/// CPU time of the calling thread.
pub fn thread_cpu_time() -> Option<Duration> {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0 {
            return Some(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32));
        }
        None
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentThread() -> isize;
            fn GetThreadTimes(
                thread: isize,
                created: *mut FileTime,
                exited: *mut FileTime,
                kernel: *mut FileTime,
                user: *mut FileTime,
            ) -> i32;
        }
        let zero = || FileTime { low: 0, high: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
        if unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) } != 0 {
            // (in 100 ns)
            let ns = |f: &FileTime| (((f.high as u64) << 32) | f.low as u64) * 100;
            return Some(Duration::from_nanos(ns(&kernel) + ns(&user)));
        }
        None
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// Time of one frame's parts, added to the shared [`Timing`] at its end.
#[derive(Default)]
struct FrameTime {
    fic: Duration,
    services: Vec<(ServiceKey, Duration, Duration)>,
    background: Duration,
    data: Duration,
    tii: Duration,
    recording: Duration,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalInfo {
    /// "rf" for the tuner, "eti" for an ETI / EDI stream.
    pub input: String,
    pub synced: bool,
    pub signal: bool,
    pub snr: f32,
    /// SNR seen in the depth of the null symbol inside the channel, also
    /// without synchronisation (gain control of an overloaded tuner).
    pub null_snr: f32,
    pub freq_offset: f32,
    pub fib_total: u64,
    pub fib_errors: u64,
    /// FIB error ratio over the last second.
    pub fib_error_rate: f32,
    /// Bit error ratio of the FIC before the Viterbi decoder, last second.
    pub fic_ber: f32,
    pub frames: u64,
}

pub enum RxEvent {
    Signal(SignalInfo),
    /// The ensemble database changed (or was cleared after retuning).
    Ensemble(Arc<Ensemble>, Arc<HashMap<(u8, u32, u8), String>>),
    Time(DabTime),
    Tii {
        codes: Vec<TiiCode>,
        pairs: Vec<f32>,
    },
    Spectrum {
        spectrum: Vec<f32>,
        null: Vec<f32>,
        freq_offset: f32,
        /// DQPSK points (empty without synchronisation).
        constellation: Vec<Complex32>,
        /// Power delay profile from the window start, window advance and the
        /// noise power of one tap (empty without synchronisation).
        impulse: Vec<f32>,
        advance: usize,
        noise: f32,
    },
    Audio {
        key: ServiceKey,
        pcm: Vec<f32>,
    },
    AudioFormat {
        key: ServiceKey,
        format: AudioFormat,
        bitrate: u16,
    },
    Label {
        key: ServiceKey,
        label: DynamicLabel,
    },
    XpadObject {
        key: ServiceKey,
        app_type: u8,
        object: MotObject,
    },
    /// Original encoded audio (LOAS AAC or MP2 frame).
    Encoded {
        key: ServiceKey,
        data: Vec<u8>,
        aac: bool,
    },
    /// MOT object of a packet mode data component (SPI, slideshow ...).
    DataObject {
        sid: u32,
        scid: u16,
        ua_type: u16,
        object: MotObject,
    },
    /// MOT objects (slides) being received via X-PAD: (X-PAD AppTy, progress);
    /// sent when it changes.
    XpadProgress {
        key: ServiceKey,
        objects: Vec<(u8, crate::mot::ObjectProgress)>,
    },
    /// Reception progress of a directory mode data service (SPI carousel).
    DataProgress {
        sid: u32,
        scid: u16,
        ua_type: u16,
        progress: crate::mot::CarouselProgress,
    },
    Stats {
        key: ServiceKey,
        stats: AudioStats,
        frame: u64,
    },
    /// Decoding of a service stopped (not in ensemble any more etc.).
    ServiceStopped {
        key: ServiceKey,
    },
    /// One ETI(NI) frame (6144 bytes) of the multiplex while it is recorded
    /// (RxCommand::RecordEti): the FIC and every sub-channel of one CIF.
    Eti(Vec<u8>),
    Retuned,
}

/// One 24 ms frame of an ETI / EDI stream: the multiplex as produced by a
/// DAB multiplexer, before channel coding (no OFDM, no Viterbi decoding).
pub struct EtiFrame {
    /// FIBs of the FIC (multiple of 32 bytes, with CRC).
    pub fic: Vec<u8>,
    /// Sub-channel id and the data of one CIF (a logical frame).
    pub streams: Vec<(u8, Vec<u8>)>,
}

pub enum RxCommand {
    StartService(ServiceKey),
    StopService(ServiceKey),
    Spectrum(bool),
    TiiMode(TiiMode),
    TiiIntegration(usize),
    /// Decode SPI data services of the ensemble automatically.
    DataServices(bool),
    /// Emit the original encoded audio of a service (recording).
    RecordEncoded(ServiceKey, bool),
    /// Decode PAD (DLS, slideshow) of all audio services in the background.
    BackgroundPad(bool),
    /// Record the multiplex: every sub-channel decoded, ETI frames as events.
    RecordEti(bool),
}

// ---------------------------------------------------------------------------

struct QueueState {
    data: VecDeque<Complex32>,
    generation: u64,
    closed: bool,
}

/// Bounded sample FIFO between the input device and the receiver thread.
pub struct SampleQueue {
    state: Mutex<QueueState>,
    cond: Condvar,
    capacity: usize,
    pub overflows: std::sync::atomic::AtomicU64,
    /// Samples taken in, and thrown away because the receiver fell behind
    /// (a computer too slow for it).
    pub pushed: std::sync::atomic::AtomicU64,
    pub dropped: std::sync::atomic::AtomicU64,
}

impl SampleQueue {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(SampleQueue {
            state: Mutex::new(QueueState { data: VecDeque::with_capacity(capacity), generation: 0, closed: false }),
            cond: Condvar::new(),
            capacity,
            overflows: Default::default(),
            pushed: Default::default(),
            dropped: Default::default(),
        })
    }

    /// Push samples from the input device (drops the oldest samples on overflow).
    pub fn push(&self, samples: &[Complex32]) {
        let mut st = self.state.lock();
        if st.data.len() + samples.len() > self.capacity {
            let drop = (st.data.len() + samples.len() - self.capacity).min(st.data.len());
            st.data.drain(..drop);
            self.overflows.fetch_add(1, Ordering::Relaxed);
            self.dropped.fetch_add(drop as u64, Ordering::Relaxed);
        }
        self.pushed.fetch_add(samples.len() as u64, Ordering::Relaxed);
        st.data.extend(samples.iter().copied());
        drop(st);
        self.cond.notify_one();
    }

    /// Discard buffered samples and signal the receiver to resynchronise.
    pub fn reset(&self) {
        let mut st = self.state.lock();
        st.data.clear();
        st.generation += 1;
        drop(st);
        self.cond.notify_all();
    }

    pub fn close(&self) {
        let mut st = self.state.lock();
        st.closed = true;
        st.generation += 1;
        drop(st);
        self.cond.notify_all();
    }

    pub fn len(&self) -> usize {
        self.state.lock().data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn generation(&self) -> u64 {
        self.state.lock().generation
    }
}

struct QueueSource {
    queue: Arc<SampleQueue>,
    generation: u64,
}

impl SampleSource for QueueSource {
    fn read(&mut self, buf: &mut [Complex32]) -> bool {
        let mut filled = 0;
        let mut st = self.queue.state.lock();
        while filled < buf.len() {
            if st.generation != self.generation || st.closed {
                return false;
            }
            let n = (buf.len() - filled).min(st.data.len());
            if n == 0 {
                self.queue.cond.wait_for(&mut st, Duration::from_millis(200));
                continue;
            }
            for (dst, src) in buf[filled..filled + n].iter_mut().zip(st.data.drain(..n)) {
                *dst = src;
            }
            filled += n;
        }
        true
    }
}

// ---------------------------------------------------------------------------

struct ActiveService {
    key: ServiceKey,
    subch_id: u8,
    subch: Option<SubchannelDecoder>,
    audio: Option<AudioDecoder>,
    bitrate: u16,
    last_stats: Instant,
    /// X-PAD reception progress last reported.
    xpad_progress: Vec<(u8, crate::mot::ObjectProgress)>,
}

impl ActiveService {
    fn report_xpad_progress(&mut self, events: &Sender<RxEvent>) {
        let Some(audio) = self.audio.as_ref() else { return };
        let objects = audio.xpad_receiving();
        if objects != self.xpad_progress {
            self.xpad_progress = objects.clone();
            let _ = events.send(RxEvent::XpadProgress { key: self.key, objects });
        }
    }
}

struct PacketChannel {
    subch_id: u8,
    subch: Option<SubchannelDecoder>,
    packets: PacketDecoder,
    /// packet address -> (MOT decoder, SId, SCId, UA type)
    components: HashMap<u16, (MotDecoder, u32, u16, u16)>,
}

struct Core {
    background_pad: bool,
    background: Vec<ActiveService>,
    recording: Vec<ServiceKey>,
    data_services: bool,
    packet_channels: Vec<PacketChannel>,
    events: Sender<RxEvent>,
    commands: Receiver<RxCommand>,
    fic: FicDecoder,
    figs: FigParser,
    tii: TiiDecoder,
    services: Vec<ActiveService>,
    wanted: Vec<ServiceKey>,
    spectrum: bool,
    spectrum_flag: Arc<AtomicBool>,
    last_revision: u64,
    last_ensemble_sent: Instant,
    last_time_minute: Option<(u32, u8, u8)>,
    signal: SignalInfo,
    fib_window: VecDeque<(u64, u64, u64, u64)>,
    last_signal_sent: Instant,
    last_spectrum_sent: Instant,
    frame_counter: u64,
    /// CIFs received from an ETI / EDI stream.
    eti_cifs: u64,
    service_events: Vec<ServiceEvent>,
    timing: Arc<Mutex<Timing>>,
    /// The ETI recording of the multiplex (etirec.rs) while one is wanted.
    eti_rec: Option<EtiRecorder>,
}

impl Core {
    fn add_time(&mut self, spent: FrameTime) {
        let mut t = self.timing.lock();
        t.fic += spent.fic;
        for (key, channel, decoding) in spent.services {
            let e = t.services.entry(key).or_default();
            e.0 += channel;
            e.1 += decoding;
        }
        t.background += spent.background;
        t.data += spent.data;
        t.tii += spent.tii;
        t.recording += spent.recording;
        if let Some(cpu) = thread_cpu_time() {
            t.thread = cpu;
        }
    }

    fn reset(&mut self) {
        self.fic = FicDecoder::new();
        self.figs.reset();
        self.tii.reset();
        self.services.clear();
        self.background.clear();
        self.packet_channels.clear();
        // (another multiplex: its own frames, from the start)
        self.eti_rec = self.eti_rec.is_some().then(EtiRecorder::new);
        self.last_revision = u64::MAX;
        self.last_time_minute = None;
        self.signal = SignalInfo::default();
        self.fib_window.clear();
        self.frame_counter = 0;
        self.eti_cifs = 0;
        let _ = self.events.send(RxEvent::Ensemble(Arc::new(Ensemble::default()), Arc::new(HashMap::new())));
        let _ = self.events.send(RxEvent::Retuned);
    }

    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.commands.try_recv() {
            match cmd {
                RxCommand::StartService(k) => {
                    if !self.wanted.contains(&k) {
                        self.wanted.push(k);
                    }
                }
                RxCommand::StopService(k) => {
                    self.wanted.retain(|w| *w != k);
                    self.services.retain(|s| s.key != k);
                }
                RxCommand::Spectrum(on) => {
                    self.spectrum = on;
                    self.spectrum_flag.store(on, Ordering::Relaxed);
                }
                RxCommand::TiiMode(m) => self.tii.mode = m,
                RxCommand::TiiIntegration(n) => self.tii.integration = n.clamp(2, 128),
                RxCommand::BackgroundPad(on) => {
                    self.background_pad = on;
                    if !on {
                        self.background.clear();
                    }
                    self.last_revision = self.last_revision.wrapping_add(1);
                }
                RxCommand::RecordEncoded(k, on) => {
                    if on {
                        if !self.recording.contains(&k) {
                            self.recording.push(k);
                        }
                    } else {
                        self.recording.retain(|x| *x != k);
                    }
                    for s in self.services.iter_mut() {
                        if let Some(a) = s.audio.as_mut() {
                            a.record = self.recording.contains(&s.key);
                        }
                    }
                }
                RxCommand::DataServices(on) => {
                    self.data_services = on;
                    if !on {
                        self.packet_channels.clear();
                    }
                    self.last_revision = self.last_revision.wrapping_add(1);
                }
                RxCommand::RecordEti(on) => {
                    if on != self.eti_rec.is_some() {
                        self.eti_rec = on.then(EtiRecorder::new);
                        // (its decoders of the sub-channels nothing else decodes)
                        self.last_revision = self.last_revision.wrapping_add(1);
                    }
                }
            }
        }
    }

    /// Bring the decoders of wanted services in line with the ensemble configuration.
    fn update_services(&mut self) {
        let ens = &self.figs.ensemble;
        // remove services that are no longer wanted
        self.services.retain(|s| self.wanted.contains(&s.key));
        for &key in &self.wanted {
            let Some(svc) = ens.services.get(&key.sid) else { continue };
            let comp = svc.components.iter().find(|c| c.scids == Some(key.scids) || (key.scids == 0 && c.primary)).filter(|c| c.is_audio());
            let Some(comp) = comp else { continue };
            let Some(subch_id) = comp.subch_id else { continue };
            let Some(sc) = ens.subchannels.get(&subch_id) else { continue };
            let dabplus = comp.is_dabplus();
            let existing = self.services.iter_mut().find(|s| s.key == key);
            let needs_new = match &existing {
                Some(s) => s.subch.as_ref().map(|d| d.subch != *sc).unwrap_or(true) || s.subch_id != subch_id,
                None => true,
            };
            if !needs_new {
                continue;
            }
            let mut audio = AudioDecoder::new(dabplus, sc.bitrate);
            audio.record = self.recording.contains(&key);
            for ua in &comp.user_apps {
                if let Some(t) = ua.xpad_app_type {
                    audio.register_xpad_app(t);
                }
            }
            let entry = ActiveService {
                key,
                subch_id,
                subch: SubchannelDecoder::new(sc),
                audio: Some(audio),
                bitrate: sc.bitrate,
                last_stats: Instant::now(),
                xpad_progress: Vec::new(),
            };
            match existing {
                Some(s) => *s = entry,
                None => self.services.push(entry),
            }
        }
        self.update_data_services();
        self.update_background();
        self.update_recorder();
    }

    /// The ETI recording decodes the sub-channels nothing else does.
    fn update_recorder(&mut self) {
        let Some(rec) = self.eti_rec.as_mut() else { return };
        let mut covered: Vec<u8> = Vec::new();
        covered.extend(self.services.iter().filter(|s| s.subch.is_some()).map(|s| s.subch_id));
        covered.extend(self.background.iter().filter(|s| s.subch.is_some()).map(|s| s.subch_id));
        covered.extend(self.packet_channels.iter().filter(|p| p.subch.is_some()).map(|p| p.subch_id));
        rec.update(&self.figs.ensemble, &covered);
    }

    /// PAD-only decoders for all audio services nobody listens to.
    fn update_background(&mut self) {
        if !self.background_pad {
            self.background.clear();
            return;
        }
        let ens = &self.figs.ensemble;
        let wanted = &self.wanted;
        self.background.retain(|b| !wanted.contains(&b.key) && ens.services.contains_key(&b.key.sid));
        for s in ens.services.values() {
            let Some(comp) = s.primary().filter(|c| c.is_audio()) else { continue };
            let key = ServiceKey { sid: s.sid, scids: comp.scids.unwrap_or(0) };
            if wanted.contains(&key) || wanted.iter().any(|w| w.sid == s.sid) {
                continue;
            }
            let Some(subch_id) = comp.subch_id else { continue };
            let Some(sc) = ens.subchannels.get(&subch_id) else { continue };
            let existing = self.background.iter().position(|b| b.key == key);
            if let Some(p) = existing {
                let same = self.background[p].subch.as_ref().map(|d| d.subch == *sc).unwrap_or(false);
                if same {
                    continue;
                }
                self.background.remove(p);
            }
            let mut audio = AudioDecoder::new(comp.is_dabplus(), sc.bitrate);
            audio.pad_only = true;
            for ua in &comp.user_apps {
                if let Some(t) = ua.xpad_app_type {
                    audio.register_xpad_app(t);
                }
            }
            self.background.push(ActiveService {
                key,
                subch_id,
                subch: SubchannelDecoder::new(sc),
                audio: Some(audio),
                bitrate: sc.bitrate,
                last_stats: Instant::now(),
                xpad_progress: Vec::new(),
            });
        }
    }

    /// Packet mode components carrying SPI (UA 0x007) or slideshow (UA 0x002) in a data service.
    fn update_data_services(&mut self) {
        if !self.data_services {
            return;
        }
        let ens = &self.figs.ensemble;
        let mut wanted: Vec<(u8, u16, u32, u16, u16)> = Vec::new();
        for s in ens.services.values() {
            for c in &s.components {
                if c.tmid != 3 {
                    continue;
                }
                let (Some(subch), Some(addr), Some(scid)) = (c.subch_id, c.packet_address, c.scid) else { continue };
                if let Some(ua) = c.user_apps.iter().find(|u| u.ua_type == 0x007 || u.ua_type == 0x002) {
                    wanted.push((subch, addr, s.sid, scid, ua.ua_type));
                }
            }
        }
        self.packet_channels.retain(|pc| wanted.iter().any(|w| w.0 == pc.subch_id));
        for (subch_id, addr, sid, scid, ua) in wanted {
            let Some(sc) = ens.subchannels.get(&subch_id) else { continue };
            let pos = self.packet_channels.iter().position(|p| p.subch_id == subch_id);
            let pc = match pos {
                Some(p) => &mut self.packet_channels[p],
                None => {
                    self.packet_channels.push(PacketChannel {
                        subch_id,
                        subch: SubchannelDecoder::new(sc),
                        packets: PacketDecoder::new(),
                        components: HashMap::new(),
                    });
                    self.packet_channels.last_mut().unwrap()
                }
            };
            if pc.subch.as_ref().map(|d| d.subch != *sc).unwrap_or(true) {
                pc.subch = SubchannelDecoder::new(sc);
            }
            pc.components.entry(addr).or_insert_with(|| (MotDecoder::new(), sid, scid, ua));
        }
    }
}

impl FrameSink for Core {
    fn sync_state(&mut self, synced: bool, signal: bool) {
        self.signal.synced = synced;
        self.signal.signal = signal;
        if !synced {
            self.signal.snr = 0.0;
        }
        if self.last_signal_sent.elapsed() > Duration::from_millis(250) {
            let _ = self.events.send(RxEvent::Signal(self.signal.clone()));
            self.last_signal_sent = Instant::now();
        }
    }

    fn null_depth(&mut self, snr_db: f32) {
        self.signal.null_snr = snr_db;
    }

    fn idle(&mut self) {
        self.handle_commands();
    }

    fn spectrum(&mut self, spectrum: &[f32], freq_offset: f32) {
        self.handle_commands();
        if self.last_spectrum_sent.elapsed() > Duration::from_millis(450) {
            self.last_spectrum_sent = Instant::now();
            let _ = self.events.send(RxEvent::Spectrum {
                spectrum: spectrum.to_vec(),
                null: vec![0.0; spectrum.len()],
                freq_offset,
                constellation: Vec::new(),
                impulse: Vec::new(),
                advance: 0,
                noise: 0.0,
            });
        }
    }

    fn frame(&mut self, frame: &Frame) {
        self.handle_commands();
        self.frame_counter += 1;
        let mut spent = FrameTime::default();
        let started = Instant::now();

        // FIC
        let figs = &mut self.figs;
        let errs = self.fic.decode(&frame.fic, |fib| figs.parse_fib(fib));
        if std::env::var("OFDMDEBUG").is_ok() {
            eprintln!("FRAME {} fib_errors {}", self.frame_counter, errs);
        }
        self.fib_window.push_back((self.fic.fib_total, self.fic.fib_errors, self.fic.coded_bits, self.fic.coded_errors));
        if self.fib_window.len() > 11 {
            self.fib_window.pop_front();
        }

        self.after_fic();
        spent.fic = started.elapsed();

        // MSC
        for svc in self.services.iter_mut() {
            let (Some(dec), Some(audio)) = (svc.subch.as_mut(), svc.audio.as_mut()) else { continue };
            let (mut channel, mut decoding) = (Duration::ZERO, Duration::ZERO);
            for c in 0..CIFS_PER_FRAME {
                let cif = &frame.msc[c * CIF_BITS..(c + 1) * CIF_BITS];
                let t = Instant::now();
                let data = dec.push_cif(cif).map(|d| d.to_vec());
                let t2 = Instant::now();
                channel += t2 - t;
                if let Some(data) = data {
                    if let Some(rec) = self.eti_rec.as_mut() {
                        rec.add(c, svc.subch_id, &data);
                    }
                    self.service_events.clear();
                    audio.push(&data, &mut self.service_events);
                    for ev in self.service_events.drain(..) {
                        let key = svc.key;
                        let e = match ev {
                            ServiceEvent::Audio(pcm) => RxEvent::Audio { key, pcm },
                            ServiceEvent::Encoded { data, aac } => RxEvent::Encoded { key, data, aac },
                            ServiceEvent::Format(format) => RxEvent::AudioFormat { key, format, bitrate: svc.bitrate },
                            ServiceEvent::Label(label) => RxEvent::Label { key, label },
                            ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                        };
                        let _ = self.events.send(e);
                    }
                    decoding += t2.elapsed();
                }
            }
            spent.services.push((svc.key, channel, decoding));
            // statistics every 10 frames (0.96 s of signal time)
            if self.frame_counter % 10 == 0 {
                svc.last_stats = Instant::now();
                let _ = self.events.send(RxEvent::Stats { key: svc.key, stats: audio.stats.clone(), frame: self.frame_counter });
            }
            if self.frame_counter % 5 == 0 {
                svc.report_xpad_progress(&self.events);
            }
        }

        // background PAD decoding (DLS / slideshow of all services)
        let t = Instant::now();
        for svc in self.background.iter_mut() {
            let (Some(dec), Some(audio)) = (svc.subch.as_mut(), svc.audio.as_mut()) else { continue };
            for c in 0..CIFS_PER_FRAME {
                let cif = &frame.msc[c * CIF_BITS..(c + 1) * CIF_BITS];
                if let Some(data) = dec.push_cif(cif) {
                    let data = data.to_vec();
                    if let Some(rec) = self.eti_rec.as_mut() {
                        rec.add(c, svc.subch_id, &data);
                    }
                    self.service_events.clear();
                    audio.push(&data, &mut self.service_events);
                    for ev in self.service_events.drain(..) {
                        let key = svc.key;
                        let e = match ev {
                            ServiceEvent::Label(label) => RxEvent::Label { key, label },
                            ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                            _ => continue,
                        };
                        let _ = self.events.send(e);
                    }
                }
            }
        }

        spent.background = t.elapsed();

        // packet mode data services (SPI, slideshow)
        let t = Instant::now();
        for pc in self.packet_channels.iter_mut() {
            let Some(dec) = pc.subch.as_mut() else { continue };
            for c in 0..CIFS_PER_FRAME {
                let cif = &frame.msc[c * CIF_BITS..(c + 1) * CIF_BITS];
                if let Some(data) = dec.push_cif(cif) {
                    let data = data.to_vec();
                    if let Some(rec) = self.eti_rec.as_mut() {
                        rec.add(c, pc.subch_id, &data);
                    }
                    let comps = &mut pc.components;
                    let events = &self.events;
                    let dbg = std::env::var("PKTDEBUG").is_ok();
                    pc.packets.push(&data, |addr, dg| {
                        if dbg {
                            let p = crate::mot::parse_data_group(&dg);
                            eprintln!(
                                "PKT addr {} dg {} bytes parsed {:?}",
                                addr,
                                dg.len(),
                                p.map(|d| (d.dg_type, d.segment, d.last, d.transport_id))
                            );
                        }
                        if let Some((mot, sid, scid, ua)) = comps.get_mut(&addr) {
                            let mut objs = Vec::new();
                            mot.process(&dg, &mut objs);
                            for object in objs {
                                let _ = events.send(RxEvent::DataObject { sid: *sid, scid: *scid, ua_type: *ua, object });
                            }
                        }
                    });
                    for (addr, &(jumps, dir)) in &pc.packets.ci_jumps {
                        if let Some((mot, ..)) = pc.components.get_mut(addr) {
                            mot.packet_continuity(jumps, dir);
                        }
                    }
                }
            }
        }

        spent.data = t.elapsed();
        if self.frame_counter % 20 == 0 {
            self.send_data_progress();
        }

        // the ETI recording: the sub-channels nothing else decodes, then the
        // frames of the CIFs whose data has come out of the deinterleaver
        let t = Instant::now();
        if let Some(rec) = self.eti_rec.as_mut() {
            rec.fic(&self.fic.raw);
            for c in 0..CIFS_PER_FRAME {
                rec.decode(c, &frame.msc[c * CIF_BITS..(c + 1) * CIF_BITS]);
            }
            let mut out = Vec::with_capacity(CIFS_PER_FRAME);
            rec.frames(&self.figs.ensemble, &mut out);
            for f in out {
                let _ = self.events.send(RxEvent::Eti(f));
            }
            spent.recording = t.elapsed();
        }

        // TII
        let t = Instant::now();
        if let Some(codes) = self.tii.push(&frame.null_fft) {
            let _ = self.events.send(RxEvent::Tii { codes, pairs: self.tii.last_pairs.clone() });
        }
        spent.tii = t.elapsed();
        self.add_time(spent);

        // spectrum, about twice per second
        if let (Some(sp), Some(null)) = (&frame.spectrum, &frame.null_spectrum) {
            if self.last_spectrum_sent.elapsed() > Duration::from_millis(450) {
                self.last_spectrum_sent = Instant::now();
                let (impulse, advance, noise) = frame.impulse.clone().unwrap_or_default();
                let _ = self.events.send(RxEvent::Spectrum {
                    spectrum: sp.clone(),
                    null: null.clone(),
                    freq_offset: frame.freq_offset_hz,
                    constellation: frame.constellation.clone().unwrap_or_default(),
                    impulse,
                    advance,
                    noise,
                });
            }
        }

        // signal info
        self.signal.input = "rf".into();
        self.signal.synced = true;
        self.signal.signal = true;
        self.signal.snr = frame.snr_db;
        self.signal.null_snr = frame.snr_db;
        self.signal.freq_offset = frame.freq_offset_hz;
        self.signal.fib_total = self.fic.fib_total;
        self.signal.fib_errors = self.fic.fib_errors;
        self.signal.frames = self.frame_counter;
        if let (Some(a), Some(b)) = (self.fib_window.front(), self.fib_window.back()) {
            let total = b.0 - a.0;
            self.signal.fib_error_rate = if total > 0 { (b.1 - a.1) as f32 / total as f32 } else { 0.0 };
            let bits = b.2 - a.2;
            self.signal.fic_ber = if bits > 0 { (b.3 - a.3) as f32 / bits as f32 } else { 0.0 };
        }
        if self.last_signal_sent.elapsed() > Duration::from_millis(250) {
            self.last_signal_sent = Instant::now();
            let _ = self.events.send(RxEvent::Signal(self.signal.clone()));
        }
    }
}

impl Core {
    /// Process one frame of an ETI / EDI stream.
    fn eti_frame(&mut self, f: &EtiFrame) {
        self.handle_commands();
        self.eti_cifs += 1;
        let mut spent = FrameTime::default();
        let started = Instant::now();
        let figs = &mut self.figs;
        for fib in f.fic.chunks_exact(32) {
            self.fic.fib_total += 1;
            if crate::crc::check_crc16(fib) {
                figs.parse_fib(&fib[..30]);
            } else {
                self.fic.fib_errors += 1;
            }
        }
        // one transmission frame (4 CIFs) per counter step, like the tuner
        if self.eti_cifs % 4 == 0 {
            self.frame_counter += 1;
            self.fib_window.push_back((self.fic.fib_total, self.fic.fib_errors, self.fic.coded_bits, self.fic.coded_errors));
            if self.fib_window.len() > 11 {
                self.fib_window.pop_front();
            }
        }
        self.after_fic();
        spent.fic = started.elapsed();
        let stream = |id: u8| f.streams.iter().find(|(s, _)| *s == id).map(|(_, d)| d.as_slice());
        for svc in self.services.iter_mut() {
            let Some(audio) = svc.audio.as_mut() else { continue };
            let t = Instant::now();
            if let Some(data) = stream(svc.subch_id) {
                self.service_events.clear();
                audio.push(data, &mut self.service_events);
                for ev in self.service_events.drain(..) {
                    let key = svc.key;
                    let e = match ev {
                        ServiceEvent::Audio(pcm) => RxEvent::Audio { key, pcm },
                        ServiceEvent::Encoded { data, aac } => RxEvent::Encoded { key, data, aac },
                        ServiceEvent::Format(format) => RxEvent::AudioFormat { key, format, bitrate: svc.bitrate },
                        ServiceEvent::Label(label) => RxEvent::Label { key, label },
                        ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                    };
                    let _ = self.events.send(e);
                }
            }
            // (the stream brings the subchannels decoded already)
            spent.services.push((svc.key, Duration::ZERO, t.elapsed()));
            if self.eti_cifs % 40 == 0 {
                svc.last_stats = Instant::now();
                let _ = self.events.send(RxEvent::Stats { key: svc.key, stats: audio.stats.clone(), frame: self.frame_counter });
            }
            if self.eti_cifs % 20 == 0 {
                svc.report_xpad_progress(&self.events);
            }
        }
        let t = Instant::now();
        for svc in self.background.iter_mut() {
            let Some(audio) = svc.audio.as_mut() else { continue };
            if let Some(data) = stream(svc.subch_id) {
                self.service_events.clear();
                audio.push(data, &mut self.service_events);
                for ev in self.service_events.drain(..) {
                    let key = svc.key;
                    let e = match ev {
                        ServiceEvent::Label(label) => RxEvent::Label { key, label },
                        ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                        _ => continue,
                    };
                    let _ = self.events.send(e);
                }
            }
        }
        spent.background = t.elapsed();
        let t = Instant::now();
        for pc in self.packet_channels.iter_mut() {
            let Some(data) = stream(pc.subch_id) else { continue };
            let comps = &mut pc.components;
            let events = &self.events;
            pc.packets.push(data, |addr, dg| {
                if let Some((mot, sid, scid, ua)) = comps.get_mut(&addr) {
                    let mut objs = Vec::new();
                    mot.process(&dg, &mut objs);
                    for object in objs {
                        let _ = events.send(RxEvent::DataObject { sid: *sid, scid: *scid, ua_type: *ua, object });
                    }
                }
            });
            for (addr, &(jumps, dir)) in &pc.packets.ci_jumps {
                if let Some((mot, ..)) = pc.components.get_mut(addr) {
                    mot.packet_continuity(jumps, dir);
                }
            }
        }
        spent.data = t.elapsed();
        // the ETI recording: the stream's frame as it came
        if let Some(rec) = self.eti_rec.as_mut() {
            let t = Instant::now();
            let mut out = Vec::with_capacity(1);
            rec.stream(&self.figs.ensemble, &f.fic, &f.streams, &mut out);
            for frame in out {
                let _ = self.events.send(RxEvent::Eti(frame));
            }
            spent.recording = t.elapsed();
        }
        self.add_time(spent);
        if self.eti_cifs % 80 == 0 {
            self.send_data_progress();
        }
        self.signal.input = "eti".into();
        self.signal.synced = true;
        self.signal.signal = true;
        self.signal.snr = 0.0;
        self.signal.freq_offset = 0.0;
        self.signal.fib_total = self.fic.fib_total;
        self.signal.fib_errors = self.fic.fib_errors;
        self.signal.frames = self.frame_counter;
        if let (Some(a), Some(b)) = (self.fib_window.front(), self.fib_window.back()) {
            let total = b.0 - a.0;
            self.signal.fib_error_rate = if total > 0 { (b.1 - a.1) as f32 / total as f32 } else { 0.0 };
            let bits = b.2 - a.2;
            self.signal.fic_ber = if bits > 0 { (b.3 - a.3) as f32 / bits as f32 } else { 0.0 };
        }
        if self.last_signal_sent.elapsed() > Duration::from_millis(250) {
            self.last_signal_sent = Instant::now();
            let _ = self.events.send(RxEvent::Signal(self.signal.clone()));
        }
    }

    fn send_data_progress(&self) {
        for pc in &self.packet_channels {
            for (mot, sid, scid, ua) in pc.components.values() {
                if let Some(progress) = mot.carousel_progress() {
                    let _ = self.events.send(RxEvent::DataProgress { sid: *sid, scid: *scid, ua_type: *ua, progress });
                }
            }
        }
    }

    /// Ensemble updates and time after new FIC data.
    fn after_fic(&mut self) {
        if self.figs.revision != self.last_revision
            && (self.last_ensemble_sent.elapsed() > Duration::from_millis(300) || self.last_revision == u64::MAX)
        {
            self.last_revision = self.figs.revision;
            self.last_ensemble_sent = Instant::now();
            self.update_services();
            let mut labels = HashMap::new();
            for (k, v) in self.figs_long_labels() {
                labels.insert(k, v);
            }
            let _ = self.events.send(RxEvent::Ensemble(Arc::new(self.figs.ensemble.clone()), Arc::new(labels)));
        } else if self.services.len() != self.wanted.len() {
            self.update_services();
        }
        if let Some(t) = self.figs.ensemble.time {
            let key = (t.mjd, t.hours, t.minutes);
            if self.last_time_minute != Some(key) {
                self.last_time_minute = Some(key);
                let _ = self.events.send(RxEvent::Time(t));
            }
        }
    }

    fn figs_long_labels(&self) -> Vec<((u8, u32, u8), String)> {
        let mut v = Vec::new();
        let e = &self.figs.ensemble;
        if let Some(eid) = e.eid {
            if let Some(l) = self.figs.long_label(0, eid as u32, 0) {
                v.push(((0, eid as u32, 0), l.clone()));
            }
        }
        for s in e.services.values() {
            let kind = if s.is_data { 5 } else { 1 };
            if let Some(l) = self.figs.long_label(kind, s.sid, 0) {
                v.push(((kind, s.sid, 0), l.clone()));
            }
            for c in &s.components {
                if let Some(scids) = c.scids {
                    if let Some(l) = self.figs.long_label(4, s.sid, scids) {
                        v.push(((4, s.sid, scids), l.clone()));
                    }
                }
            }
        }
        v
    }
}

/// Handle of a running receiver thread.
pub struct ReceiverHandle {
    pub queue: Arc<SampleQueue>,
    pub commands: Sender<RxCommand>,
    pub events: Receiver<RxEvent>,
    /// ETI / EDI frames; used instead of samples while `eti_mode` is set.
    pub eti: Sender<EtiFrame>,
    eti_mode: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Where the time of the receiver thread goes (statistics).
    pub timing: Arc<Mutex<Timing>>,
}

impl ReceiverHandle {
    pub fn start() -> Self {
        let queue = SampleQueue::new(2_048_000 * 2);
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let (eti_tx, eti_rx) = crossbeam_channel::bounded::<EtiFrame>(200);
        let eti_mode = Arc::new(AtomicBool::new(false));
        let q = queue.clone();
        let st = stop.clone();
        let em = eti_mode.clone();
        let timing = Arc::new(Mutex::new(Timing::default()));
        let tm = timing.clone();
        let thread = std::thread::Builder::new()
            .name("dab-receiver".into())
            .spawn(move || {
                let mut demod = OfdmDemodulator::new();
                let mut core = Core {
                    background_pad: true,
                    background: Vec::new(),
                    recording: Vec::new(),
                    data_services: true,
                    packet_channels: Vec::new(),
                    events: ev_tx,
                    commands: cmd_rx,
                    fic: FicDecoder::new(),
                    figs: FigParser::new(),
                    tii: TiiDecoder::new(),
                    services: Vec::new(),
                    wanted: Vec::new(),
                    spectrum: false,
                    spectrum_flag: demod.spectrum_requested.clone(),
                    last_revision: u64::MAX,
                    last_ensemble_sent: Instant::now(),
                    last_time_minute: None,
                    signal: SignalInfo::default(),
                    fib_window: VecDeque::new(),
                    last_signal_sent: Instant::now(),
                    last_spectrum_sent: Instant::now(),
                    frame_counter: 0,
                    eti_cifs: 0,
                    service_events: Vec::new(),
                    timing: tm,
                    eti_rec: None,
                };
                while !st.load(Ordering::Relaxed) {
                    let generation = q.generation();
                    demod.reset();
                    core.reset();
                    if em.load(Ordering::Relaxed) {
                        // ETI / EDI input until the next retune (queue reset)
                        // drop frames of an earlier connection
                        while eti_rx.try_recv().is_ok() {}
                        while !st.load(Ordering::Relaxed) && q.generation() == generation {
                            match eti_rx.recv_timeout(Duration::from_millis(200)) {
                                Ok(f) => core.eti_frame(&f),
                                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                    core.handle_commands();
                                    if core.signal.synced {
                                        core.sync_state(false, false);
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    } else {
                        let mut src = QueueSource { queue: q.clone(), generation };
                        demod.run(&mut src, &mut core, &st);
                    }
                    if q.state.lock().closed {
                        break;
                    }
                }
            })
            .expect("spawn receiver thread");
        ReceiverHandle { queue, commands: cmd_tx, events: ev_rx, eti: eti_tx, eti_mode, stop, thread: Some(thread), timing }
    }

    pub fn send(&self, cmd: RxCommand) {
        let _ = self.commands.send(cmd);
    }

    /// Switch between the tuner (samples) and an ETI / EDI stream; takes effect
    /// with the next queue reset (retune).
    pub fn set_eti_mode(&self, on: bool) {
        self.eti_mode.store(on, Ordering::Relaxed);
    }
}

impl Drop for ReceiverHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.queue.close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
