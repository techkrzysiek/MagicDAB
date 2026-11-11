//! A DTR-100 / C100 USB DAB dongle as the tuner (magicdab_sdr::dtr100): the
//! dongle demodulates and hands out the FIC and one sub-channel, which reach
//! the receiver as frames like those of an ETI / EDI stream (the same path as
//! etiinput.rs). One sub-channel at a time: one station for everybody.

use crossbeam_channel::{Receiver, Sender, TrySendError};
use magicdab_dab::crc::{check_crc16, check_firecode};
use magicdab_dab::receiver::EtiFrame;
use magicdab_sdr::dtr100::Dtr100;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The sub-channel to stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stream {
    pub subch: u8,
    pub bitrate: u16,
    pub dabplus: bool,
}

enum Cmd {
    /// Tune (kHz); None: another source has the channel, the dongle idles.
    Tune(Option<u32>),
    Stream(Option<Stream>),
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    pub name: String,
    pub firmware: u8,
    pub error: Option<String>,
    /// The tuned frequency (kHz) once the dongle locked.
    pub locked: Option<u32>,
    /// 0..4, as the dongle reports reception.
    pub bars: u8,
    /// The chip's channel error rate: 0 = perfect, 2000 = nothing.
    pub cer: Option<u32>,
    /// Audio read from the chip (this stick's controller passes none on).
    pub chip_audio: bool,
    pub subchannel: Option<u8>,
    /// Frames the receiver could not take (a computer too slow for it).
    pub dropped: u64,
}

pub struct Dtr100Input {
    cmd: Sender<Cmd>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<Status>>,
}

impl Dtr100Input {
    pub fn start(tx: Sender<EtiFrame>) -> Self {
        let (cmd, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let (st, sts) = (stop.clone(), status.clone());
        let thread = std::thread::Builder::new()
            .name("dtr100".into())
            .spawn(move || {
                let now = Instant::now();
                let mut w = Worker {
                    rx,
                    tx,
                    status: sts,
                    dev: None,
                    want: None,
                    tuned: None,
                    need_tune: false,
                    want_stream: None,
                    playing: None,
                    framer: Framer::default(),
                    fic_aligner: FicAligner::default(),
                    fic: Vec::new(),
                    fic_note_at: None,
                    open_at: now,
                    retune_at: now,
                    fic_at: now,
                    last_fic: now,
                    full: false,
                    overruns_seen: 0,
                    overruns_at: now,
                    overruns_known: 0,
                    overrun_at: None,
                    replies_at: now,
                };
                w.run(&st);
            })
            .expect("dtr100 thread");
        Dtr100Input { cmd, stop, thread: Some(thread), status }
    }

    /// Tune to a band III frequency (kHz); None lets go of the channel.
    pub fn tune(&self, khz: Option<u32>) {
        let _ = self.cmd.send(Cmd::Tune(khz));
    }

    pub fn set_stream(&self, stream: Option<Stream>) {
        let _ = self.cmd.send(Cmd::Stream(stream));
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }
}

impl Drop for Dtr100Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    rx: Receiver<Cmd>,
    tx: Sender<EtiFrame>,
    status: Arc<Mutex<Status>>,
    dev: Option<Dtr100>,
    want: Option<u32>,
    tuned: Option<u32>,
    need_tune: bool,
    want_stream: Option<Stream>,
    playing: Option<Stream>,
    framer: Framer,
    fic_aligner: FicAligner,
    /// FIC read while a sub-channel streams, carried by its next frame.
    fic: Vec<u8>,
    fic_note_at: Option<Instant>,
    open_at: Instant,
    retune_at: Instant,
    fic_at: Instant,
    last_fic: Instant,
    /// The receiver's queue was full (reported once).
    full: bool,
    /// The chip's buffer overruns counted at the last report, and when.
    overruns_seen: u64,
    overruns_at: Instant,
    /// The overruns the framer knows of; when the last one was (until its
    /// frames come again).
    overruns_known: u64,
    overrun_at: Option<Instant>,
    /// The longest replies were last logged (Dtr100::replies).
    replies_at: Instant,
}

impl Worker {
    fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            while let Ok(c) = self.rx.try_recv() {
                match c {
                    Cmd::Tune(k) => {
                        if k != self.want {
                            self.want = k;
                            self.need_tune = true;
                        }
                    }
                    Cmd::Stream(s) => self.want_stream = s,
                }
            }
            if self.want.is_none() {
                // another source has the channel: nothing may reach the receiver
                if let Some(mut d) = self.dev.take() {
                    match self.idle(&mut d) {
                        Ok(()) => self.dev = Some(d),
                        Err(e) => self.lost(e),
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if self.dev.is_none() && !self.open() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let Some(mut d) = self.dev.take() else { continue };
            match self.step(&mut d) {
                Ok(()) => self.dev = Some(d),
                Err(e) => {
                    drop(d);
                    self.lost(e);
                }
            }
        }
        if let Some(d) = self.dev.as_mut() {
            if self.playing.is_some() {
                let _ = d.stop_audio();
            }
        }
    }

    fn open(&mut self) -> bool {
        if Instant::now() < self.open_at {
            return false;
        }
        match Dtr100::open() {
            Ok(d) => {
                let mut st = self.status.lock();
                st.connected = true;
                st.name = d.name().to_string();
                st.firmware = d.firmware_level();
                st.chip_audio = d.chip_audio();
                st.error = None;
                drop(st);
                self.dev = Some(d);
                self.need_tune = true;
                true
            }
            Err(e) => {
                let mut st = self.status.lock();
                if st.error.as_deref() != Some(e.as_str()) {
                    log::warn!("DTR-100: {e} (trying again every 5 s)");
                }
                st.connected = false;
                st.error = Some(e);
                self.open_at = Instant::now() + Duration::from_secs(5);
                false
            }
        }
    }

    /// A USB error: the dongle is opened (and initialised) again.
    fn lost(&mut self, e: String) {
        log::warn!("DTR-100: {e}; opening it again");
        self.dev = None;
        self.playing = None;
        self.tuned = None;
        self.need_tune = true;
        self.framer.reset();
        self.fic_aligner.reset();
        self.fic.clear();
        let mut st = self.status.lock();
        st.connected = false;
        st.error = Some(e.clone());
        st.locked = None;
        st.subchannel = None;
        st.cer = None;
        // (a dongle still there that did not answer is opened again at once)
        let wait = if e.contains("(timeout)") { Duration::from_millis(100) } else { Duration::from_secs(2) };
        self.open_at = Instant::now() + wait;
    }

    fn idle(&mut self, d: &mut Dtr100) -> Result<(), String> {
        if self.playing.take().is_some() {
            d.stop_audio()?;
        }
        if self.tuned.take().is_some() {
            let mut st = self.status.lock();
            st.locked = None;
            st.subchannel = None;
        }
        Ok(())
    }

    fn step(&mut self, d: &mut Dtr100) -> Result<(), String> {
        let Some(khz) = self.want else { return Ok(()) };
        // no FIC for a while: the chip lost the ensemble for good, start over
        if self.tuned.is_some() && self.last_fic.elapsed() > Duration::from_secs(15) {
            log::info!("DTR-100: no FIC for 15 s, tuning again");
            self.need_tune = true;
        }
        if self.need_tune || (self.tuned.is_none() && Instant::now() >= self.retune_at) {
            self.need_tune = false;
            if self.playing.take().is_some() {
                d.stop_audio()?;
            }
            self.framer.reset();
            self.fic_aligner.reset();
            self.fic.clear();
            {
                let mut st = self.status.lock();
                st.locked = None;
                st.subchannel = None;
                st.cer = None;
            }
            let t = d.tune(khz, true)?;
            self.tuned = t.locked.then_some(khz);
            self.last_fic = Instant::now();
            if t.locked {
                log::info!("DTR-100: locked on {khz} kHz ({})", t.detail);
            } else {
                log::debug!("DTR-100: no ensemble on {khz} kHz ({})", t.detail);
                self.retune_at = Instant::now() + Duration::from_secs(3);
            }
            let mut st = self.status.lock();
            st.locked = self.tuned;
            st.bars = t.bars;
            return Ok(());
        }
        if self.tuned.is_none() {
            std::thread::sleep(Duration::from_millis(20));
            return Ok(());
        }
        if self.want_stream != self.playing {
            if self.playing.take().is_some() {
                d.stop_audio()?;
            }
            if let Some(s) = self.want_stream {
                d.start_audio(s.subch, s.bitrate)?;
                self.playing = Some(s);
            }
            self.framer.reset();
            self.status.lock().subchannel = self.playing.map(|s| s.subch);
        }
        let mut busy = false;
        // the sub-channel first: its buffer in the chip holds a fraction of a
        // second of a 192 kbit/s station, read too late it starts again (audio
        // lost); what is there is read until it is empty
        if let Some(s) = self.playing {
            for _ in 0..4 {
                let data = d.msc_read()?;
                if d.overruns != self.overruns_known {
                    // the buffer started again: the stream jumped, its
                    // superframes are looked for at once
                    self.overruns_known = d.overruns;
                    self.overrun_at = Some(Instant::now());
                    self.framer.lost();
                }
                if data.is_empty() {
                    break;
                }
                busy = true;
                let mut frames = Vec::new();
                self.framer.push(&data, &s, |f| frames.push(f));
                if let Some(at) = self.overrun_at.filter(|_| !frames.is_empty()) {
                    self.overrun_at = None;
                    log::info!("DTR-100: audio frames again {} ms after the overrun", at.elapsed().as_millis());
                }
                for f in frames {
                    let fic = std::mem::take(&mut self.fic);
                    self.send(EtiFrame { fic, streams: vec![(s.subch, f)] });
                }
            }
        }
        // the FIC as it comes; next to a sub-channel every 150 ms (the vendor's
        // library serialises the two reads too; the stations' names come from
        // it, not their audio)
        if self.playing.is_none() || self.fic_at.elapsed() >= Duration::from_millis(150) {
            if let Some(raw) = d.fic_read()? {
                // (not ready yet: asked again in the next round, not 150 ms
                // later, or the ready moments are missed time after time and
                // the stick is tuned again for want of FIC)
                self.fic_at = Instant::now();
                busy = true;
                self.last_fic = Instant::now();
                self.note_fic(&raw, khz);
                let mut f = Vec::with_capacity(raw.len());
                self.fic_aligner.push(&raw, &mut f);
                if self.playing.is_none() {
                    // 12 FIBs: 3 per CIF, like a multiplexer's frames
                    for c in f.chunks(96) {
                        self.send(EtiFrame { fic: c.to_vec(), streams: Vec::new() });
                    }
                } else {
                    self.fic.extend_from_slice(&f);
                    if self.fic.len() >= 3 * 384 {
                        // no audio to carry it (yet): on its own
                        let fic = std::mem::take(&mut self.fic);
                        for c in fic.chunks(96) {
                            self.send(EtiFrame { fic: c.to_vec(), streams: Vec::new() });
                        }
                    }
                }
            }
        }
        // how long the replies take while streaming: in the log every 10 minutes
        if self.playing.is_some() && self.replies_at.elapsed() >= Duration::from_secs(600) {
            self.replies_at = Instant::now();
            let [small, big] = d.replies();
            log::info!("DTR-100: longest replies in 10 minutes: {small} ms (registers), {big} ms (2 kB of the sub-channel)");
        }
        // audio lost to a full buffer: in the log once a minute
        if self.overruns_at.elapsed() >= Duration::from_secs(60) {
            self.overruns_at = Instant::now();
            let n = d.overruns - self.overruns_seen;
            self.overruns_seen = d.overruns;
            if n > 0 {
                let kbps = self.playing.map_or(0, |s| s.bitrate);
                log::info!("DTR-100: the chip's sub-channel buffer ran over {n} time(s) in the last minute ({kbps} kbit/s): audio lost");
            }
        }
        if let Some(sig) = d.signal() {
            let mut st = self.status.lock();
            st.bars = sig.bars;
            st.cer = Some(sig.cer);
            st.chip_audio = d.chip_audio();
        }
        if !busy {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// A FIC block whose FIBs do not start at its first byte (seen while a
    /// sub-channel streams on some multiplexes): logged once a minute, with
    /// the offset that has the most valid FIBs.
    fn note_fic(&mut self, raw: &[u8], khz: u32) {
        let valid = |o: usize| raw.get(o..).map_or(0, |r| r.chunks_exact(32).filter(|f| check_crc16(f)).count());
        let at0 = valid(0);
        if at0 * 32 * 2 >= raw.len() || self.fic_note_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        self.fic_note_at = Some(Instant::now());
        let (best, n) = (1..32).map(|o| (o, valid(o))).max_by_key(|(_, n)| *n).unwrap_or((0, 0));
        log::info!(
            "DTR-100 {khz} kHz: FIC block of {} bytes with {at0} valid FIBs at offset 0, {n} at offset {best} (sub-channel {:?}); first bytes {:02x?}",
            raw.len(),
            self.playing.map(|s| s.subch),
            &raw[..raw.len().min(8)]
        );
    }

    fn send(&mut self, f: EtiFrame) {
        match self.tx.try_send(f) {
            Ok(()) => self.full = false,
            Err(TrySendError::Full(_)) => {
                if !self.full {
                    log::warn!("DTR-100: the receiver is behind, frames dropped");
                }
                self.full = true;
                self.status.lock().dropped += 1;
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// FIBs out of the FIC reads, aligned by their CRC: while a sub-channel
/// streams, the chip's FIC blocks did not start with a FIB on some
/// multiplexes (every CRC failed at offset 0, the audio was fine).
#[derive(Default)]
struct FicAligner {
    buf: Vec<u8>,
    synced: bool,
    /// FIBs in a row whose CRC failed.
    bad: u32,
}

impl FicAligner {
    fn reset(&mut self) {
        self.buf.clear();
        self.synced = false;
        self.bad = 0;
    }

    fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.buf.extend_from_slice(data);
        loop {
            if !self.synced {
                if self.buf.len() < 64 {
                    return;
                }
                let limit = self.buf.len() - 64;
                let buf = &self.buf;
                match (0..=limit).find(|&o| check_crc16(&buf[o..o + 32]) && check_crc16(&buf[o + 32..o + 64])) {
                    Some(o) => {
                        self.buf.drain(..o);
                        self.synced = true;
                        self.bad = 0;
                    }
                    None => {
                        // (nothing valid: the receiver counts these FIBs as errors)
                        let n = (self.buf.len() - 63) / 32 * 32;
                        out.extend(self.buf.drain(..n));
                        return;
                    }
                }
            }
            if self.buf.len() < 32 {
                return;
            }
            let fib: Vec<u8> = self.buf.drain(..32).collect();
            if check_crc16(&fib) {
                self.bad = 0;
            } else {
                self.bad += 1;
                if self.bad >= 12 {
                    self.synced = false;
                }
            }
            out.extend_from_slice(&fib);
        }
    }
}

/// Cuts the sub-channel byte stream into logical frames (one CIF each). The
/// dongle's stream has no frame boundaries, so DAB+ frames are aligned to a
/// superframe found by its firecode (here and one superframe later); the MP2
/// decoder finds its frames by itself.
#[derive(Default)]
struct Framer {
    buf: Vec<u8>,
    synced: bool,
    /// Logical frames since the last superframe start.
    phase: usize,
    /// Superframe starts in a row whose firecode failed.
    fails: u32,
    /// Frames given out, and where among them (modulo 5) the superframes
    /// started before the sync was lost: the receiver's decoder keeps that
    /// grid, so the next superframe found is put on it (empty frames before
    /// it) and the decoder stays in sync (the frames between are lost audio,
    /// not a new search of 4 superframes).
    emitted: usize,
    grid: Option<usize>,
}

impl Framer {
    fn reset(&mut self) {
        *self = Framer::default();
    }

    /// The stream jumped (bytes lost): the superframes are looked for again.
    fn lost(&mut self) {
        if self.synced {
            self.grid = Some((self.emitted + 5 - self.phase % 5) % 5);
        }
        self.buf.clear();
        self.synced = false;
        self.phase = 0;
        self.fails = 0;
    }

    fn push(&mut self, data: &[u8], s: &Stream, mut out: impl FnMut(Vec<u8>)) {
        let n = s.bitrate as usize * 3;
        if n < 11 {
            return;
        }
        self.buf.extend_from_slice(data);
        if !s.dabplus {
            while self.buf.len() >= n {
                out(self.buf.drain(..n).collect());
            }
            return;
        }
        let sf = 5 * n;
        loop {
            if !self.synced {
                if self.buf.len() < sf + 11 {
                    return;
                }
                let limit = self.buf.len() - sf - 11;
                let buf = &self.buf;
                match (0..=limit).find(|&o| check_firecode(&buf[o..o + 11]) && check_firecode(&buf[o + sf..o + sf + 11])) {
                    Some(o) => {
                        self.buf.drain(..o);
                        self.synced = true;
                        self.phase = 0;
                        self.fails = 0;
                        if let Some(g) = self.grid.take() {
                            for _ in 0..(g + 5 - self.emitted % 5) % 5 {
                                out(vec![0; n]);
                                self.emitted += 1;
                            }
                        }
                    }
                    None => {
                        self.buf.drain(..=limit);
                        return;
                    }
                }
            }
            if self.buf.len() < n {
                return;
            }
            if self.phase == 0 {
                if check_firecode(&self.buf[..11]) {
                    self.fails = 0;
                } else {
                    // a few bad ones are reception errors (the decoder corrects
                    // what it can); more in a row: the stream moved
                    self.fails += 1;
                    if self.fails > 3 {
                        self.grid = Some(self.emitted % 5);
                        self.synced = false;
                        self.buf.drain(..1);
                        continue;
                    }
                }
            }
            out(self.buf.drain(..n).collect());
            self.emitted += 1;
            self.phase = (self.phase + 1) % 5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicdab_dab::crc::firecode;

    /// A DAB+ sub-channel stream: superframes with a valid firecode.
    fn superframes(bitrate: u16, count: usize) -> Vec<u8> {
        let sf = bitrate as usize * 15;
        let mut out = Vec::new();
        for k in 0..count {
            let mut s: Vec<u8> = (0..sf).map(|i| (i * 7 + k * 13) as u8 | 1).collect();
            let crc = firecode(&s[2..11]);
            s[0] = (crc >> 8) as u8;
            s[1] = crc as u8;
            out.extend(s);
        }
        out
    }

    #[test]
    fn dabplus_frames_start_at_a_superframe() {
        let s = Stream { subch: 3, bitrate: 48, dabplus: true };
        let stream = superframes(48, 6);
        let n = 48 * 3;
        let mut f = Framer::default();
        let mut frames = Vec::new();
        // joined in the middle of a superframe, in odd pieces
        for piece in stream[1000..].chunks(333) {
            f.push(piece, &s, |x| frames.push(x));
        }
        assert!(!frames.is_empty());
        assert!(frames.iter().all(|x| x.len() == n));
        assert!(check_firecode(&frames[0][..11]));
        assert!(check_firecode(&frames[5][..11]));
        assert!(!check_firecode(&frames[1][..11]));
    }

    #[test]
    fn after_a_jump_the_superframes_stay_on_the_decoders_grid() {
        let s = Stream { subch: 3, bitrate: 48, dabplus: true };
        let stream = superframes(48, 12);
        let n = 48 * 3;
        let mut f = Framer::default();
        let mut frames = Vec::new();
        f.push(&stream[1000..20 * n + 1000], &s, |x| frames.push(x));
        let first = frames.iter().position(|x| check_firecode(&x[..11])).unwrap();
        let before = frames.len();
        // the buffer started again: 777 bytes lost, picked up mid-frame
        f.lost();
        f.push(&stream[20 * n + 1777..], &s, |x| frames.push(x));
        assert!(frames.len() > before);
        let starts: Vec<usize> = (before..frames.len()).filter(|&i| check_firecode(&frames[i][..11])).collect();
        assert!(!starts.is_empty());
        assert!(starts.iter().all(|&i| i % 5 == first % 5), "{first} {starts:?}");
        // (the empty frames that put it there)
        assert!(frames[before..starts[0]].iter().all(|x| x.iter().all(|&b| b == 0)));
    }

    fn fib(k: u8) -> Vec<u8> {
        let mut f: Vec<u8> = (0..30).map(|i| i as u8 ^ k).collect();
        let crc = magicdab_dab::crc::crc16_ccitt(&f);
        f.extend_from_slice(&crc.to_be_bytes());
        f
    }

    #[test]
    fn fibs_are_found_in_shifted_fic_blocks() {
        let stream: Vec<u8> = (0..40).flat_map(fib).collect();
        assert!(check_crc16(&stream[..32]));
        let mut a = FicAligner::default();
        let mut out = Vec::new();
        // blocks of 384 bytes that start 7 bytes into a FIB
        for b in stream[7..].chunks(384) {
            a.push(b, &mut out);
        }
        let good = out.chunks_exact(32).filter(|f| check_crc16(f)).count();
        assert!(good >= 37, "{good}");
    }

    #[test]
    fn mp2_is_cut_into_cif_sized_pieces() {
        let s = Stream { subch: 1, bitrate: 128, dabplus: false };
        let mut f = Framer::default();
        let mut frames = Vec::new();
        f.push(&vec![0u8; 1000], &s, |x| frames.push(x));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 384);
    }
}
