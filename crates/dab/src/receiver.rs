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
use crate::journaline::{JmlObject, JournalineDecoder, UaParams};
use crate::mot::MotDecoder;
use crate::mot::MotObject;
use crate::msc::SubchannelDecoder;
use crate::ofdm::{Frame, FrameSink, MscSymbols, OfdmDemodulator, SampleSource};
use crate::packet::PacketDecoder;
use crate::pool::Pool;
use crate::tii::{TiiCode, TiiDecoder, TiiMode};
use crate::tpeg::{EventKey, TpegDecoder, TrafficEvent};
use crossbeam_channel::{Receiver, Sender};
use num_complex::Complex32;
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
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
    /// CPU time of the receiver thread and its decoder threads, all of it
    /// (OFDM demodulation is what is left after the parts below).
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
    /// Journaline objects (journaline.rs) of a component of a service (its
    /// SCIdS; 0 for the X-PAD of its audio): the new or changed ones, and the
    /// IDs gone.
    Journaline {
        sid: u32,
        scids: u8,
        objects: Vec<JmlObject>,
        removed: Vec<u16>,
    },
    /// TPEG traffic messages (tpeg.rs) of a data service: the new or changed
    /// ones, and the keys gone (cancelled or expired).
    Tpeg {
        sid: u32,
        events: Vec<TrafficEvent>,
        removed: Vec<EventKey>,
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

    /// One frame of a station listened to: its sub-channel, the audio, the
    /// texts and slides, now and then its statistics.
    fn decode_listened(&mut self, msc: &[i8], pass: &Pass, took: &mut Took) {
        let (Some(dec), Some(audio)) = (self.subch.as_mut(), self.audio.as_mut()) else { return };
        let mut events = Vec::new();
        for c in 0..CIFS_PER_FRAME {
            let cif = &msc[c * CIF_BITS..(c + 1) * CIF_BITS];
            let t = Instant::now();
            let data = dec.push_cif(cif).map(|d| d.to_vec());
            let t2 = Instant::now();
            took.channel += t2 - t;
            if let Some(data) = data {
                audio.push(&data, &mut events);
                for ev in events.drain(..) {
                    let key = self.key;
                    let e = match ev {
                        ServiceEvent::Audio(pcm) => RxEvent::Audio { key, pcm },
                        ServiceEvent::Encoded { data, aac } => RxEvent::Encoded { key, data, aac },
                        ServiceEvent::Format(format) => RxEvent::AudioFormat { key, format, bitrate: self.bitrate },
                        ServiceEvent::Label(label) => RxEvent::Label { key, label },
                        ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                        ServiceEvent::Journaline { objects, removed } => {
                            RxEvent::Journaline { sid: key.sid, scids: key.scids, objects, removed }
                        }
                    };
                    let _ = pass.events.send(e);
                }
                if pass.record {
                    took.data.push((c, self.subch_id, data));
                }
                took.content += t2.elapsed();
            }
        }
        // statistics every 10 frames (0.96 s of signal time)
        if pass.frame % 10 == 0 {
            self.last_stats = Instant::now();
            let _ = pass.events.send(RxEvent::Stats { key: self.key, stats: audio.stats.clone(), frame: pass.frame });
        }
        if pass.frame % 5 == 0 {
            self.report_xpad_progress(&pass.events);
        }
    }

    /// One frame of a station decoded in the background: only its texts and
    /// slides (DLS, slideshow, Journaline).
    fn decode_background(&mut self, msc: &[i8], pass: &Pass, took: &mut Took) {
        let (Some(dec), Some(audio)) = (self.subch.as_mut(), self.audio.as_mut()) else { return };
        let t = Instant::now();
        let mut events = Vec::new();
        for c in 0..CIFS_PER_FRAME {
            let cif = &msc[c * CIF_BITS..(c + 1) * CIF_BITS];
            let Some(data) = dec.push_cif(cif).map(|d| d.to_vec()) else { continue };
            audio.push(&data, &mut events);
            for ev in events.drain(..) {
                let key = self.key;
                let e = match ev {
                    ServiceEvent::Label(label) => RxEvent::Label { key, label },
                    ServiceEvent::XpadObject { app_type, object } => RxEvent::XpadObject { key, app_type, object },
                    ServiceEvent::Journaline { objects, removed } => {
                        RxEvent::Journaline { sid: key.sid, scids: key.scids, objects, removed }
                    }
                    ServiceEvent::Format(format) => RxEvent::AudioFormat { key, format, bitrate: self.bitrate },
                    _ => continue,
                };
                let _ = pass.events.send(e);
            }
            if pass.record {
                took.data.push((c, self.subch_id, data));
            }
        }
        took.channel += t.elapsed();
    }
}

/// What every sub-channel of a frame needs besides its decoder.
#[derive(Clone)]
struct Pass {
    events: Sender<RxEvent>,
    frame: u64,
    /// The ETI recording is on: the sub-channels' data comes back for it.
    record: bool,
}

/// A decoder of a sub-channel, for one frame.
enum Work {
    Listened(ActiveService),
    Background(ActiveService),
    Data(PacketChannel),
}

impl Work {
    fn decode(&mut self, msc: &[i8], pass: &Pass) -> Took {
        let mut took = Took::default();
        match self {
            Work::Listened(s) => s.decode_listened(msc, pass, &mut took),
            Work::Background(s) => s.decode_background(msc, pass, &mut took),
            Work::Data(p) => p.decode(msc, pass, &mut took),
        }
        took
    }

    /// About how much processor a frame of it takes: its size, twice for a
    /// station listened to (the audio).
    fn cost(&self) -> u32 {
        let size = |d: &Option<SubchannelDecoder>| d.as_ref().map(|d| d.subch.size as u32).unwrap_or(0);
        match self {
            Work::Listened(s) => 2 * size(&s.subch),
            Work::Background(s) => size(&s.subch),
            Work::Data(p) => size(&p.subch),
        }
    }

    /// The order of the ETI recording's data (EtiRecorder::add takes the
    /// first of a sub-channel), as on one thread: stations listened to,
    /// in the background, data services.
    fn rank(&self) -> u8 {
        match self {
            Work::Listened(_) => 0,
            Work::Background(_) => 1,
            Work::Data(_) => 2,
        }
    }
}

/// What a frame of a sub-channel took besides the events it sent.
#[derive(Default)]
struct Took {
    /// Time of the sub-channel (deinterleaving, Viterbi; all of it in the
    /// background and for data) and of the audio in it, on the clock.
    channel: Duration,
    content: Duration,
    /// For the ETI recording: (CIF, sub-channel, its data).
    data: Vec<(usize, u8, Vec<u8>)>,
}

/// A decoder of a sub-channel on its way to a decoder thread with the
/// frame's soft bits, and back (Core::decode_msc).
struct Job {
    at: usize,
    work: Work,
    msc: Arc<Vec<i8>>,
    pass: Pass,
}

struct Done {
    at: usize,
    work: Work,
    took: Took,
}

fn decode_job(job: Job) -> Done {
    let Job { at, mut work, msc, pass } = job;
    let took = work.decode(&msc, &pass);
    Done { at, work, took }
}

/// Threads that decode the sub-channels of a frame, the receiver thread among
/// them: one per core, at most 4 (a Raspberry Pi has 4, and the biggest
/// sub-channel takes a part of one core). MAGICDAB_DECODERS sets another
/// number, 1 keeps everything on the receiver thread.
fn decoder_threads() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        std::env::var("MAGICDAB_DECODERS").ok().and_then(|v| v.parse().ok()).unwrap_or(cores.min(4)).max(1)
    })
}

/// Takes the decoders `ready` for this frame out of `v` as work, leaving
/// their places empty (Core::decode_msc puts them back).
fn lend<T>(v: &mut Vec<T>, ready: impl Fn(&T) -> bool, work: fn(T) -> Work, jobs: &mut Vec<(usize, Work)>) -> Vec<Option<T>> {
    let mut slots: Vec<Option<T>> = std::mem::take(v).into_iter().map(Some).collect();
    for (at, slot) in slots.iter_mut().enumerate() {
        if slot.as_ref().is_some_and(&ready) {
            jobs.extend(slot.take().map(|t| (at, work(t))));
        }
    }
    slots
}

struct PacketChannel {
    subch_id: u8,
    subch: Option<SubchannelDecoder>,
    packets: PacketDecoder,
    /// packet address -> (MOT decoder, SId, SCId, UA type)
    components: HashMap<u16, (MotDecoder, u32, u16, u16)>,
    /// packet address -> (Journaline decoder, SId, SCIdS)
    journaline: HashMap<u16, (JournalineDecoder, u32, u8)>,
    /// packet address -> (TPEG decoder, SId, MSC data groups used)
    tpeg: HashMap<u16, (TpegDecoder, u32, bool)>,
    last_expire: Instant,
}

/// The new or changed Journaline objects of `ids` (the decoder's answer), and the ones gone.
fn journaline_event(dec: &JournalineDecoder, sid: u32, scids: u8, ids: &[u16]) -> RxEvent {
    let objects = ids.iter().filter_map(|id| dec.object(*id).cloned()).collect();
    let removed = ids.iter().copied().filter(|id| dec.object(*id).is_none()).collect();
    RxEvent::Journaline { sid, scids, objects, removed }
}

impl PacketChannel {
    fn new(subch_id: u8, sc: &crate::ensemble::Subchannel) -> Self {
        PacketChannel {
            subch_id,
            subch: SubchannelDecoder::new(sc),
            packets: PacketDecoder::new(),
            components: HashMap::new(),
            journaline: HashMap::new(),
            tpeg: HashMap::new(),
            last_expire: Instant::now(),
        }
    }

    /// One frame of the sub-channel: its packets.
    fn decode(&mut self, msc: &[i8], pass: &Pass, took: &mut Took) {
        let t = Instant::now();
        for c in 0..CIFS_PER_FRAME {
            let cif = &msc[c * CIF_BITS..(c + 1) * CIF_BITS];
            let Some(data) = self.subch.as_mut().and_then(|d| d.push_cif(cif)).map(|d| d.to_vec()) else { continue };
            self.feed(&data, &pass.events);
            if pass.record {
                took.data.push((c, self.subch_id, data));
            }
        }
        took.channel += t.elapsed();
    }

    /// One logical frame of the sub-channel: its packets to the decoders of
    /// their addresses (MOT, Journaline, TPEG), what they give out as events.
    fn feed(&mut self, data: &[u8], events: &Sender<RxEvent>) {
        let PacketChannel { packets, components, journaline, tpeg, .. } = self;
        let dbg = std::env::var("PKTDEBUG").is_ok();
        packets.push(data, |addr, dg| {
            if dbg {
                let p = crate::mot::parse_data_group(&dg);
                eprintln!("PKT addr {} dg {} bytes parsed {:?}", addr, dg.len(), p.map(|d| (d.dg_type, d.segment, d.last, d.transport_id)));
            }
            if let Some((mot, sid, scid, ua)) = components.get_mut(&addr) {
                let mut objs = Vec::new();
                mot.process(&dg, &mut objs);
                for object in objs {
                    let _ = events.send(RxEvent::DataObject { sid: *sid, scid: *scid, ua_type: *ua, object });
                }
            } else if let Some((dec, sid, scids)) = journaline.get_mut(&addr) {
                let ids = dec.push(&dg);
                if !ids.is_empty() {
                    let _ = events.send(journaline_event(dec, *sid, *scids, &ids));
                }
            } else if let Some((dec, _, groups)) = tpeg.get_mut(&addr) {
                if *groups {
                    dec.push_data_group(&dg);
                } else {
                    dec.push(&dg);
                }
            }
        });
        for (addr, &(jumps, dir)) in &self.packets.ci_jumps {
            if let Some((mot, ..)) = self.components.get_mut(addr) {
                mot.packet_continuity(jumps, dir);
            }
        }
        // the messages that changed, and now and then the expired ones
        let expire = self.last_expire.elapsed() > Duration::from_secs(10);
        if expire {
            self.last_expire = Instant::now();
        }
        for (dec, sid, _) in self.tpeg.values_mut() {
            if expire {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                dec.expire(now);
            }
            let keys = dec.take_changed();
            if !keys.is_empty() {
                let events_now: Vec<TrafficEvent> = keys.iter().filter_map(|k| dec.get(k).cloned()).collect();
                let removed: Vec<EventKey> = keys.iter().copied().filter(|k| dec.get(k).is_none()).collect();
                let _ = events.send(RxEvent::Tpeg { sid: *sid, events: events_now, removed });
            }
        }
    }
}

struct Core {
    background_pad: bool,
    background: Vec<ActiveService>,
    /// Services just no longer wanted, until update_background takes them.
    released: Vec<ActiveService>,
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
    /// The MSC symbols the demodulator works out (the ones the decoders below use).
    msc_symbols: Arc<MscSymbols>,
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
    /// Threads decoding sub-channels next to this one, from the first frame
    /// with more than one (see decoder_threads).
    decoders: Option<Pool<Job, Done>>,
    /// The frame's MSC soft bits for them.
    msc: Arc<Vec<i8>>,
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
            t.thread = cpu + self.decoders.as_ref().map(|d| d.cpu()).unwrap_or_default();
        }
    }

    fn reset(&mut self) {
        self.fic = FicDecoder::new();
        self.figs.reset();
        self.tii.restart();
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
                    // (on in the background, warm for the next time it is chosen:
                    // update_services takes it after a station chosen instead,
                    // whose decoder of the background it takes first)
                    let (gone, keep): (Vec<ActiveService>, Vec<ActiveService>) =
                        std::mem::take(&mut self.services).into_iter().partition(|s| s.key == k);
                    self.services = keep;
                    self.released.extend(gone);
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
                        // (Journaline and TPEG stay: they come back at once)
                        self.packet_channels.clear();
                        self.update_data_services();
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
        // services no longer wanted: to the background as they are (a station
        // chosen again starts without waiting for its sub-channel)
        let (keep, released): (Vec<ActiveService>, Vec<ActiveService>) =
            std::mem::take(&mut self.services).into_iter().partition(|s| self.wanted.contains(&s.key));
        self.services = keep;
        self.released.extend(released);
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
            // the decoder of the background for it: its sub-channel is
            // deinterleaved and its superframes found, the sound comes at once
            // (a new decoder waits for 16 frames of time interleaving and the
            // start of a superframe, about half a second)
            if existing.is_none() {
                let warm = self
                    .background
                    .iter()
                    .position(|b| b.key == key && b.subch_id == subch_id && b.subch.as_ref().is_some_and(|d| d.subch == *sc));
                if let Some(p) = warm {
                    let mut s = self.background.remove(p);
                    if let Some(a) = s.audio.as_mut() {
                        a.set_pad_only(false);
                        a.record = self.recording.contains(&key);
                    }
                    s.last_stats = Instant::now();
                    self.services.push(s);
                    continue;
                }
            }
            let mut audio = AudioDecoder::new(dabplus, sc.bitrate);
            audio.record = self.recording.contains(&key);
            for ua in &comp.user_apps {
                if let Some(t) = ua.xpad_app_type {
                    // (Journaline in X-PAD has its own decoder, not MOT)
                    match UaParams::from_user_app(ua) {
                        Some(p) => audio.register_journaline_app(t, p.extended_header_len),
                        None => audio.register_xpad_app(t),
                    }
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
        self.released.clear();
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
            // a station just left: its decoder goes on in the background
            let released = self
                .released
                .iter()
                .position(|r| r.key == key && r.subch_id == subch_id && r.subch.as_ref().is_some_and(|d| d.subch == *sc));
            if let Some(p) = released {
                let mut s = self.released.remove(p);
                if let Some(a) = s.audio.as_mut() {
                    a.set_pad_only(true);
                    a.record = false;
                }
                self.background.push(s);
                continue;
            }
            let mut audio = AudioDecoder::new(comp.is_dabplus(), sc.bitrate);
            audio.pad_only = true;
            for ua in &comp.user_apps {
                if let Some(t) = ua.xpad_app_type {
                    // (Journaline in X-PAD has its own decoder, not MOT)
                    match UaParams::from_user_app(ua) {
                        Some(p) => audio.register_journaline_app(t, p.extended_header_len),
                        None => audio.register_xpad_app(t),
                    }
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

    /// Packet mode components: SPI (UA 0x007) and slideshow (UA 0x002) of data
    /// services while those are wanted; Journaline (UA 0x44A) and TPEG (UA 0x004)
    /// always (a few kbps each).
    fn update_data_services(&mut self) {
        enum Want {
            Mot(u16, u16),
            Journaline(u8, u8),
            Tpeg(bool),
        }
        let ens = &self.figs.ensemble;
        let mut wanted: Vec<(u8, u16, u32, Want)> = Vec::new();
        for s in ens.services.values() {
            for c in &s.components {
                if c.tmid != 3 {
                    continue;
                }
                let (Some(subch), Some(addr)) = (c.subch_id, c.packet_address) else { continue };
                if let Some(p) = c.user_apps.iter().find_map(UaParams::from_user_app) {
                    wanted.push((subch, addr, s.sid, Want::Journaline(c.scids.unwrap_or(0), p.extended_header_len)));
                } else if c.user_apps.iter().any(|u| u.ua_type == 0x004) {
                    // (DG flag 0: MSC data groups)
                    wanted.push((subch, addr, s.sid, Want::Tpeg(!c.dg_flag)));
                } else if let (true, Some(scid), Some(ua)) =
                    (self.data_services, c.scid, c.user_apps.iter().find(|u| u.ua_type == 0x007 || u.ua_type == 0x002))
                {
                    wanted.push((subch, addr, s.sid, Want::Mot(scid, ua.ua_type)));
                }
            }
        }
        self.packet_channels.retain(|pc| wanted.iter().any(|w| w.0 == pc.subch_id));
        for pc in self.packet_channels.iter_mut() {
            let here = |a: &u16| wanted.iter().any(|w| w.0 == pc.subch_id && w.1 == *a);
            pc.components.retain(|a, _| here(a));
            pc.journaline.retain(|a, _| here(a));
            pc.tpeg.retain(|a, _| here(a));
        }
        for (subch_id, addr, sid, want) in wanted {
            let Some(sc) = ens.subchannels.get(&subch_id) else { continue };
            let pos = self.packet_channels.iter().position(|p| p.subch_id == subch_id);
            let pc = match pos {
                Some(p) => &mut self.packet_channels[p],
                None => {
                    self.packet_channels.push(PacketChannel::new(subch_id, sc));
                    self.packet_channels.last_mut().unwrap()
                }
            };
            if pc.subch.as_ref().map(|d| d.subch != *sc).unwrap_or(true) {
                pc.subch = SubchannelDecoder::new(sc);
            }
            match want {
                Want::Mot(scid, ua) => {
                    pc.components.entry(addr).or_insert_with(|| (MotDecoder::new(), sid, scid, ua));
                }
                Want::Journaline(scids, ext) => {
                    pc.journaline.entry(addr).or_insert_with(|| {
                        let mut d = JournalineDecoder::new();
                        d.set_extended_header_len(ext);
                        (d, sid, scids)
                    });
                }
                Want::Tpeg(groups) => {
                    pc.tpeg.entry(addr).or_insert_with(|| (TpegDecoder::new(), sid, groups));
                }
            }
        }
    }
}

impl FrameSink for Core {
    fn sync_state(&mut self, synced: bool, signal: bool) {
        self.signal.synced = synced;
        self.signal.signal = signal;
        if !synced {
            self.signal.snr = 0.0;
            // (what was summed before is of another timing and frequency)
            self.tii.restart();
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
        self.decode_msc(frame, &mut spent);
        if self.frame_counter % 20 == 0 {
            self.send_data_progress();
        }

        // what the next frame needs demodulated: the sub-channels decoded
        // here, everything for an ETI recording
        let geometry = |d: &Option<SubchannelDecoder>| d.as_ref().map(|d| (d.subch.start as usize, d.subch.size as usize));
        self.msc_symbols.set(
            self.services
                .iter()
                .chain(self.background.iter())
                .filter_map(|s| geometry(&s.subch))
                .chain(self.packet_channels.iter().filter_map(|p| geometry(&p.subch))),
            self.eti_rec.is_some(),
        );

        // the ETI recording: the sub-channels nothing else decodes, then the
        // frames of the CIFs whose data has come out of the deinterleaver
        let t = Instant::now();
        if let Some(rec) = self.eti_rec.as_mut().filter(|_| MscSymbols::is_all(&frame.msc_mask)) {
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
    /// The sub-channels of a frame: the stations listened to, the texts and
    /// slides of the others, the data services. Each is decoded on its own,
    /// so they are spread over the decoder threads.
    fn decode_msc(&mut self, frame: &Frame, spent: &mut FrameTime) {
        // (a decoder made for this frame, demodulated before it was there, starts with the next one)
        let covered = |d: &Option<SubchannelDecoder>| {
            d.as_ref().is_some_and(|d| MscSymbols::covers(&frame.msc_mask, d.subch.start as usize, d.subch.size as usize))
        };
        let mut jobs = Vec::new();
        let mut services = lend(&mut self.services, |s| s.audio.is_some() && covered(&s.subch), Work::Listened, &mut jobs);
        let mut background = lend(&mut self.background, |s| s.audio.is_some() && covered(&s.subch), Work::Background, &mut jobs);
        let mut data = lend(&mut self.packet_channels, |p| covered(&p.subch), Work::Data, &mut jobs);
        let pass = Pass { events: self.events.clone(), frame: self.frame_counter, record: self.eti_rec.is_some() };
        if jobs.len() > 1 && self.decoders.is_none() && decoder_threads() > 1 {
            self.decoders = Some(Pool::new(decoder_threads() - 1, "dab-decoder", decode_job));
        }
        let mut done = Vec::with_capacity(jobs.len());
        match self.decoders.as_ref().filter(|_| jobs.len() > 1) {
            Some(pool) => {
                // the soft bits for the other threads (the last frame's
                // copy is free again: its jobs are back)
                match Arc::get_mut(&mut self.msc) {
                    Some(m) => {
                        m.clear();
                        m.extend_from_slice(&frame.msc);
                    }
                    None => self.msc = Arc::new(frame.msc.clone()),
                }
                // the biggest first, each to the thread with the least so far:
                // the same sub-channels to the same threads every frame
                jobs.sort_by_key(|(_, w)| std::cmp::Reverse(w.cost()));
                let mut shares: Vec<(u32, Vec<Job>)> = (0..pool.threads().min(jobs.len())).map(|_| (0, Vec::new())).collect();
                for (at, work) in jobs {
                    let Some(share) = shares.iter_mut().min_by_key(|s| s.0) else { break };
                    share.0 += work.cost().max(1);
                    share.1.push(Job { at, work, msc: self.msc.clone(), pass: pass.clone() });
                }
                pool.run(shares.into_iter().map(|s| s.1).collect(), &mut done);
            }
            None => {
                for (at, mut work) in jobs {
                    let took = work.decode(&frame.msc, &pass);
                    done.push(Done { at, work, took });
                }
            }
        }
        done.sort_by_key(|d| (d.work.rank(), d.at));
        for Done { at, work, took } in done {
            if let Some(rec) = self.eti_rec.as_mut() {
                for (c, id, data) in &took.data {
                    rec.add(*c, *id, data);
                }
            }
            match work {
                Work::Listened(s) => {
                    spent.services.push((s.key, took.channel, took.content));
                    services[at] = Some(s);
                }
                Work::Background(s) => {
                    spent.background += took.channel;
                    background[at] = Some(s);
                }
                Work::Data(p) => {
                    spent.data += took.channel;
                    data[at] = Some(p);
                }
            }
        }
        self.services = services.into_iter().flatten().collect();
        self.background = background.into_iter().flatten().collect();
        self.packet_channels = data.into_iter().flatten().collect();
    }

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
                        ServiceEvent::Journaline { objects, removed } => {
                            RxEvent::Journaline { sid: key.sid, scids: key.scids, objects, removed }
                        }
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
                        ServiceEvent::Journaline { objects, removed } => {
                            RxEvent::Journaline { sid: key.sid, scids: key.scids, objects, removed }
                        }
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
            pc.feed(data, &self.events);
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
        } else if self.services.len() != self.wanted.len() || !self.released.is_empty() {
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
                    released: Vec::new(),
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
                    msc_symbols: demod.msc_symbols.clone(),
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
                    decoders: None,
                    msc: Arc::default(),
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
