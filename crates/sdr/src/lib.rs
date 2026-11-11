//! Input devices for MagicDAB.
//!
//! All devices deliver complex baseband samples at 2.048 MS/s through a
//! callback. RTL-SDR support loads `librtlsdr` at runtime, so the same
//! binary runs on systems without the library (the device is then reported
//! as unavailable); so does the HackRF One (`libhackrf`, receive only). The
//! exception is the DTR-100 (`dtr100`): a USB dongle
//! with a DAB chip that demodulates by itself and hands out the FIC and one
//! sub-channel; the server drives it directly.

pub mod agc;
pub mod android_usb;
pub mod dtr100;
pub mod hackrf;
pub mod rawfile;
pub mod rtlsdr;
pub mod rtltcp;

use num_complex::Complex32;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type SampleCallback = Arc<dyn Fn(&[Complex32]) + Send + Sync>;

pub const SAMPLE_RATE: u32 = 2_048_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "mode")]
#[derive(Default)]
pub enum GainMode {
    /// Gain controlled by MagicDAB (software AGC).
    #[default]
    Software,
    /// Tuner/hardware AGC.
    Hardware,
    /// Fixed gain index.
    Manual { index: usize },
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub kind: String,
    pub name: String,
    pub connected: bool,
    pub frequency: u32,
    /// Current gain in dB (if known).
    pub gain_db: Option<f32>,
    pub gain_index: Option<usize>,
    pub gains: Vec<f32>,
    /// Estimated RF level in dBm (rough).
    pub rf_level: Option<f32>,
    pub error: Option<String>,
    /// Raw file playback position in seconds.
    pub position: Option<f64>,
    pub duration: Option<f64>,
    /// Network inputs: interruptions of the sample stream (> 300 ms) and the
    /// length of the last one.
    pub stalls: u32,
    pub last_stall_ms: Option<u32>,
    /// Network inputs: sample rate actually received over the last seconds (kS/s).
    pub stream_rate: Option<f32>,
}

pub trait InputDevice: Send {
    fn status(&self) -> DeviceStatus;
    /// Start streaming at the given frequency (Hz).
    fn start(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String>;
    fn tune(&mut self, frequency: u32) -> Result<(), String>;
    fn stop(&mut self);
    fn set_gain_mode(&mut self, mode: GainMode);
    fn set_ppm(&mut self, _ppm: i32) {}
    /// Upper limit (gain table index) of the software AGC; `None` restores the
    /// default. A lower limit applies at once; up to a higher one the AGC climbs
    /// as far as the signal level allows, or with `jump` the gain goes straight there.
    fn set_gain_ceiling(&mut self, _index: Option<usize>, _jump: bool) {}
}

/// Convert unsigned 8 bit IQ samples (RTL-SDR format) into complex floats.
pub fn convert_u8(buf: &[u8], out: &mut Vec<Complex32>) {
    // (a table: cheap on small ARM cores too)
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| std::array::from_fn(|i| (i as f32 - 127.4) * (1.0 / 128.0)));
    out.clear();
    out.extend(buf.chunks_exact(2).map(|c| Complex32::new(t[c[0] as usize], t[c[1] as usize])));
}

/// Approximate RF level from the gain and the average sample power of 8 bit samples.
pub fn estimate_rf_level(gain_db: f32, mean_power: f32) -> f32 {
    // full scale sine of an RTL2832 ADC corresponds to roughly -10 dBm at 0 dB gain
    let dbfs = 10.0 * mean_power.max(1e-12).log10();
    dbfs - gain_db - 12.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum DeviceConfig {
    RtlSdr {
        #[serde(default)]
        index: u32,
        #[serde(default)]
        ppm: i32,
        #[serde(default)]
        bias_tee: bool,
    },
    RtlTcp {
        host: String,
        port: u16,
    },
    RawFile {
        path: String,
        #[serde(default = "default_true")]
        looping: bool,
    },
    /// No tuner: only channels mapped to ETI / EDI streams.
    None,
    /// HackRF One: receive only, its amplifier always off, fixed LNA and VGA
    /// gains (hackrf.rs); `serial` empty for the first one.
    #[serde(rename = "hackrf")]
    HackRf {
        #[serde(default)]
        serial: String,
        #[serde(default = "default_lna")]
        lna: u32,
        #[serde(default = "default_vga")]
        vga: u32,
    },
    /// DTR-100 / C100 USB DAB dongle (16c0:05dc): demodulates by itself, one
    /// sub-channel at a time; no samples (the server drives it, see `dtr100`).
    Dtr100,
}

fn default_true() -> bool {
    true
}

fn default_lna() -> u32 {
    16
}

fn default_vga() -> u32 {
    30
}

impl Default for DeviceConfig {
    fn default() -> Self {
        // (Android too: the app opens the stick and hands it over, android_usb.rs)
        DeviceConfig::RtlSdr { index: 0, ppm: 0, bias_tee: false }
    }
}

impl DeviceConfig {
    /// rtl_tcp on this machine: a stick of its own (rtl_tcp next to MagicDAB,
    /// the driver app on Android), unlike a server far away, which usually
    /// streams one multiplex (e.g. from a modulator).
    pub fn local_rtltcp(&self) -> bool {
        matches!(self, DeviceConfig::RtlTcp { host, .. } if local_host(host))
    }
}

/// This machine by name or loopback address.
pub fn local_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub fn open(config: &DeviceConfig) -> Box<dyn InputDevice> {
    match config {
        DeviceConfig::RtlSdr { index, ppm, bias_tee } => Box::new(rtlsdr::RtlSdr::new(*index, *ppm, *bias_tee)),
        DeviceConfig::RtlTcp { host, port } => Box::new(rtltcp::RtlTcp::new(host.clone(), *port)),
        DeviceConfig::RawFile { path, looping } => Box::new(rawfile::RawFile::new(path.clone(), *looping)),
        DeviceConfig::None => Box::new(NoDevice),
        DeviceConfig::Dtr100 => Box::new(HardwareReceiver),
        DeviceConfig::HackRf { serial, lna, vga } => Box::new(hackrf::HackRf::new(serial.clone(), *lna, *vga)),
    }
}

/// Stands in for the DTR-100, which delivers no samples: the server runs the
/// dongle itself and only asks this for a name.
struct HardwareReceiver;

impl InputDevice for HardwareReceiver {
    fn status(&self) -> DeviceStatus {
        DeviceStatus { kind: "dtr100".into(), name: "DTR-100".into(), connected: true, ..Default::default() }
    }
    fn start(&mut self, _frequency: u32, _callback: SampleCallback) -> Result<(), String> {
        Err("the DTR-100 delivers no samples".into())
    }
    fn tune(&mut self, _frequency: u32) -> Result<(), String> {
        Err("the DTR-100 delivers no samples".into())
    }
    fn stop(&mut self) {}
    fn set_gain_mode(&mut self, _mode: GainMode) {}
}

/// Input without a tuner (all channels come from ETI / EDI streams).
struct NoDevice;

impl InputDevice for NoDevice {
    fn status(&self) -> DeviceStatus {
        DeviceStatus { kind: "none".into(), name: "-".into(), connected: true, ..Default::default() }
    }
    fn start(&mut self, _frequency: u32, _callback: SampleCallback) -> Result<(), String> {
        Ok(())
    }
    fn tune(&mut self, _frequency: u32) -> Result<(), String> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn set_gain_mode(&mut self, _mode: GainMode) {}
}

/// Enumerate RTL-SDR devices (empty when librtlsdr is not available).
pub fn list_rtlsdr() -> Vec<String> {
    rtlsdr::list_devices()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_tcp_on_this_machine() {
        let tcp = |h: &str| DeviceConfig::RtlTcp { host: h.into(), port: 1234 };
        for h in ["127.0.0.1", "127.0.1.1", "localhost", "LOCALHOST", "::1", "[::1]"] {
            assert!(tcp(h).local_rtltcp(), "{h}");
        }
        for h in ["rtl-tcp.example.org", "192.168.1.10", "localhost.example.org", "127.0.0.1.example.org", ""] {
            assert!(!tcp(h).local_rtltcp(), "{h}");
        }
        assert!(!DeviceConfig::RtlSdr { index: 0, ppm: 0, bias_tee: false }.local_rtltcp());
    }
}
