//! The FM tuner next to the DAB receiver: state, RDS (librdsparser) and the
//! messages for the web interface. The audio reaches the listeners like a DAB
//! service under the key `FM_KEY`.

use crate::radio::Command;
use crossbeam_channel::Sender;
use magicdab_dab::receiver::ServiceKey;
use magicdab_fm::capture::Capture;
use magicdab_fm::rds::{RdsInfo, RdsParser};
use magicdab_fm::tuner::{FmTuner, Signal, TunerEvent};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// The tuner is reached through a serial port and ALSA, which Android apps do
/// not have: the Android app is DAB only (no DAB/FM linking either).
pub const AVAILABLE: bool = !cfg!(target_os = "android");

/// Service id under which the FM audio is selected and sent.
pub const FM_SID: u32 = 0xFFFF_FFFF;
pub const FM_KEY: ServiceKey = ServiceKey { sid: FM_SID, scids: 0 };

pub struct FmRadio {
    pub tuner: FmTuner,
    /// Audio of the tuner, only while it is connected: it sends none
    /// otherwise, and a missing sound card would be tried again and again.
    capture: Option<Capture>,
    audio: String,
    commands: Sender<Command>,
    rds: RdsParser,
    pub connected: bool,
    pub port: String,
    pub khz: u32,
    pub antenna: u8,
    /// Antennas on the antenna switch (settings).
    pub antennas: u8,
    pub signal: Signal,
    pi: Option<(u16, u8)>,
    pub info: RdsInfo,
    pub scan: Vec<(u32, f32)>,
    pub scan_id: u64,
    dirty: bool,
    last_sent: Instant,
}

impl FmRadio {
    pub fn start(port: String, audio: String, commands: Sender<Command>) -> FmRadio {
        let (tx, rx) = crossbeam_channel::unbounded();
        let tuner = FmTuner::start(port, tx);
        let c = commands.clone();
        std::thread::Builder::new()
            .name("fm-events".into())
            .spawn(move || {
                for e in rx {
                    if c.send(Command::Fm(e)).is_err() {
                        break;
                    }
                }
            })
            .expect("fm events thread");
        FmRadio {
            tuner,
            capture: None,
            audio,
            commands,
            rds: RdsParser::new(),
            connected: false,
            port: String::new(),
            khz: 0,
            antenna: 0,
            antennas: 1,
            signal: Signal::default(),
            pi: None,
            info: RdsInfo::default(),
            scan: Vec::new(),
            scan_id: 0,
            dirty: true,
            last_sent: Instant::now(),
        }
    }

    /// Handle a tuner event; returns the frequency when the tuner was retuned.
    pub fn on_event(&mut self, e: TunerEvent) -> Option<u32> {
        match e {
            TunerEvent::Connected(port) => {
                self.connected = true;
                self.port = port;
                if self.capture.is_none() {
                    let c = self.commands.clone();
                    self.capture = Some(Capture::start(self.audio.clone(), move |pcm| {
                        let _ = c.send(Command::FmAudio(pcm.to_vec()));
                    }));
                }
            }
            TunerEvent::Disconnected => {
                self.connected = false;
                self.capture = None;
            }
            TunerEvent::Tuned(khz) => {
                self.khz = khz;
                self.rds.clear();
                self.pi = None;
                self.info = RdsInfo::default();
                self.dirty = true;
                return Some(khz);
            }
            TunerEvent::Antenna(a) => self.antenna = a,
            TunerEvent::Signal(s) => self.signal = s,
            TunerEvent::Pi { pi, errors } => self.pi = Some((pi, errors)),
            TunerEvent::Group { blocks, errors } => {
                let (pi, pe) = self.pi.unwrap_or((0, 3));
                self.rds.parse([pi, blocks[0], blocks[1], blocks[2]], [pe, errors[0], errors[1], errors[2]]);
                let info = self.rds.info();
                if info != self.info {
                    self.info = info;
                }
            }
            TunerEvent::Scan(points) => {
                self.scan = points;
                self.scan_id += 1;
            }
        }
        self.dirty = true;
        None
    }

    /// The state for the web interface, at most four times a second.
    pub fn due(&mut self) -> Option<Value> {
        if !self.dirty || self.last_sent.elapsed() < Duration::from_millis(250) {
            return None;
        }
        self.dirty = false;
        self.last_sent = Instant::now();
        Some(self.state_json())
    }

    pub fn state_json(&self) -> Value {
        json!({
            "type": "fm",
            "connected": self.connected,
            "port": self.port,
            "khz": self.khz,
            "antenna": self.antenna,
            "antennas": self.antennas,
            "signal": self.signal,
            "piErrors": self.pi.map(|p| p.1),
            "rds": self.info,
        })
    }

    pub fn scan_json(&self) -> Value {
        json!({"type": "fmScan", "points": self.scan})
    }

    /// What the service following needs to know.
    pub fn view(&self) -> crate::linking::FmView {
        crate::linking::FmView {
            connected: self.connected,
            khz: self.khz,
            antenna: self.antenna,
            antennas: self.antennas,
            level: self.signal.level,
            cci: self.signal.cci,
            aci: self.signal.aci,
            pi: self.pi,
            ps: self.info.ps.clone(),
            af: self.info.af.clone(),
            scan: self.scan.clone(),
            scan_id: self.scan_id,
        }
    }

    /// "FM 105.0 MHz ! ALFA !" for logs.
    pub fn name(&self) -> String {
        let ps = if self.info.ps.is_empty() { String::new() } else { format!(" {}", self.info.ps) };
        format!("FM {:.1} MHz{ps}", self.khz as f64 / 1000.0)
    }
}
