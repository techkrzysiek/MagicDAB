//! HackRF One input via libhackrf loaded at runtime. Receive only: the
//! transmit functions of the library are not even looked up. The RF
//! amplifier is switched off at every start and nothing here can switch it
//! on (it is meant for faint signals and takes damage from strong ones);
//! the LNA (0–40 dB, steps of 8) and VGA (0–62 dB, steps of 2) gains are
//! fixed by the settings. 2.048 MS/s, a 1.75 MHz baseband filter.

use crate::{DeviceStatus, GainMode, InputDevice, SampleCallback, SAMPLE_RATE};
use libloading::{Library, Symbol};
use num_complex::Complex32;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

type Dev = *mut c_void;

/// libhackrf's `hackrf_transfer` (its first fields, the same in every version).
#[repr(C)]
struct Transfer {
    device: Dev,
    buffer: *mut u8,
    buffer_length: c_int,
    valid_length: c_int,
    rx_ctx: *mut c_void,
    tx_ctx: *mut c_void,
}

type RxCb = unsafe extern "C" fn(*mut Transfer) -> c_int;

struct Api {
    _lib: Library,
    init: unsafe extern "C" fn() -> c_int,
    open_by_serial: unsafe extern "C" fn(*const c_char, *mut Dev) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    set_freq: unsafe extern "C" fn(Dev, u64) -> c_int,
    set_sample_rate: unsafe extern "C" fn(Dev, f64) -> c_int,
    compute_bw: unsafe extern "C" fn(u32) -> u32,
    set_bw: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_lna: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_vga: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_amp: unsafe extern "C" fn(Dev, u8) -> c_int,
    /// Power on the antenna port (bias): kept off.
    set_antenna_power: Option<unsafe extern "C" fn(Dev, u8) -> c_int>,
    start_rx: unsafe extern "C" fn(Dev, RxCb, *mut c_void) -> c_int,
    stop_rx: unsafe extern "C" fn(Dev) -> c_int,
    error_name: unsafe extern "C" fn(c_int) -> *const c_char,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn library_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["hackrf.dll", "libhackrf.dll"]
    } else if cfg!(target_os = "macos") {
        &["libhackrf.0.dylib", "libhackrf.dylib", "/opt/homebrew/lib/libhackrf.dylib", "/usr/local/lib/libhackrf.dylib"]
    } else {
        &["libhackrf.so.0", "libhackrf.so", "/usr/local/lib/libhackrf.so.0"]
    }
}

fn load_api() -> Result<Api, String> {
    let mut last_err = String::new();
    for name in library_names() {
        match unsafe { Library::new(name) } {
            Ok(lib) => unsafe {
                macro_rules! sym {
                    ($n:expr) => {{
                        let s: Symbol<_> = lib.get($n).map_err(|e| e.to_string())?;
                        *s
                    }};
                }
                let set_antenna_power =
                    lib.get(b"hackrf_set_antenna_enable\0").ok().map(|s: Symbol<unsafe extern "C" fn(Dev, u8) -> c_int>| *s);
                let api = Api {
                    init: sym!(b"hackrf_init\0"),
                    open_by_serial: sym!(b"hackrf_open_by_serial\0"),
                    close: sym!(b"hackrf_close\0"),
                    set_freq: sym!(b"hackrf_set_freq\0"),
                    set_sample_rate: sym!(b"hackrf_set_sample_rate\0"),
                    compute_bw: sym!(b"hackrf_compute_baseband_filter_bw\0"),
                    set_bw: sym!(b"hackrf_set_baseband_filter_bandwidth\0"),
                    set_lna: sym!(b"hackrf_set_lna_gain\0"),
                    set_vga: sym!(b"hackrf_set_vga_gain\0"),
                    set_amp: sym!(b"hackrf_set_amp_enable\0"),
                    set_antenna_power,
                    start_rx: sym!(b"hackrf_start_rx\0"),
                    stop_rx: sym!(b"hackrf_stop_rx\0"),
                    error_name: sym!(b"hackrf_error_name\0"),
                    _lib: lib,
                };
                let r = (api.init)();
                if r != 0 {
                    return Err(format!("hackrf_init failed ({r})"));
                }
                return Ok(api);
            },
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("libhackrf not found ({last_err})"))
}

fn api() -> Result<&'static Api, String> {
    use std::sync::OnceLock;
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

fn error(api: &Api, what: &str, r: c_int) -> String {
    let name = unsafe { CStr::from_ptr((api.error_name)(r)) }.to_string_lossy().to_string();
    format!("{what}: {name} ({r})")
}

/// The gains the HackRF can set: LNA 0–40 dB in steps of 8, VGA 0–62 dB in
/// steps of 2 (rounded down).
pub fn gains(lna: u32, vga: u32) -> (u32, u32) {
    (lna.min(40) / 8 * 8, vga.min(62) / 2 * 2)
}

/// Signed 8 bit I/Q (HackRF) to complex floats, without the DC offset of the
/// receiver (its local oscillator leaks into the middle of the band; the DC
/// carrier of DAB is unused anyway).
struct Converter {
    dc: Complex32,
}

impl Converter {
    fn convert(&mut self, buf: &[u8], out: &mut Vec<Complex32>) {
        out.clear();
        out.reserve(buf.len() / 2);
        let mut dc = self.dc;
        for c in buf.chunks_exact(2) {
            let s = Complex32::new(c[0] as i8 as f32 / 128.0, c[1] as i8 as f32 / 128.0);
            // (a slow mean: a few tens of milliseconds)
            dc += (s - dc) * 1.0e-4;
            out.push(s - dc);
        }
        self.dc = dc;
    }
}

struct Shared {
    callback: SampleCallback,
    converter: Mutex<(Converter, Vec<Complex32>)>,
    stop: AtomicBool,
}

unsafe extern "C" fn rx_cb(t: *mut Transfer) -> c_int {
    let t = &*t;
    let sh = &*(t.rx_ctx as *const Shared);
    if sh.stop.load(Ordering::Relaxed) {
        return -1;
    }
    let data = std::slice::from_raw_parts(t.buffer, t.valid_length.max(0) as usize);
    let mut g = sh.converter.lock().unwrap();
    let (conv, samples) = &mut *g;
    conv.convert(data, samples);
    (sh.callback)(samples);
    0
}

struct DevPtr(Dev);
unsafe impl Send for DevPtr {}

pub struct HackRf {
    serial: String,
    lna: u32,
    vga: u32,
    dev: Option<DevPtr>,
    shared: Option<*const Shared>,
    frequency: u32,
    error: Option<String>,
    name: String,
}

unsafe impl Send for HackRf {}

impl HackRf {
    /// `serial`: of the HackRF to open (the end of it is enough), empty for the first one.
    pub fn new(serial: String, lna: u32, vga: u32) -> Self {
        let (lna, vga) = gains(lna, vga);
        HackRf { serial, lna, vga, dev: None, shared: None, frequency: 0, error: None, name: String::new() }
    }

    fn open(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        let api = api()?;
        let serial = CString::new(self.serial.trim()).map_err(|_| "bad serial number".to_string())?;
        let mut dev: Dev = std::ptr::null_mut();
        let r = unsafe { (api.open_by_serial)(if self.serial.trim().is_empty() { std::ptr::null() } else { serial.as_ptr() }, &mut dev) };
        if r != 0 || dev.is_null() {
            return Err(error(api, "cannot open the HackRF", r));
        }
        self.dev = Some(DevPtr(dev));
        unsafe {
            // receive only, the amplifier off whatever was set before, no
            // power on the antenna port
            let r = (api.set_amp)(dev, 0);
            if r != 0 {
                return Err(error(api, "hackrf_set_amp_enable(0)", r));
            }
            if let Some(f) = api.set_antenna_power {
                f(dev, 0);
            }
            let r = (api.set_sample_rate)(dev, SAMPLE_RATE as f64);
            if r != 0 {
                return Err(error(api, "hackrf_set_sample_rate", r));
            }
            (api.set_bw)(dev, (api.compute_bw)(1_750_000));
            (api.set_lna)(dev, self.lna);
            (api.set_vga)(dev, self.vga);
            let r = (api.set_freq)(dev, frequency as u64);
            if r != 0 {
                return Err(error(api, "hackrf_set_freq", r));
            }
        }
        let shared = Box::into_raw(Box::new(Shared {
            callback,
            converter: Mutex::new((Converter { dc: Complex32::new(0.0, 0.0) }, Vec::new())),
            stop: AtomicBool::new(false),
        }));
        self.shared = Some(shared);
        let r = unsafe { (api.start_rx)(dev, rx_cb, shared as *mut c_void) };
        if r != 0 {
            return Err(error(api, "hackrf_start_rx", r));
        }
        self.frequency = frequency;
        self.name = if self.serial.trim().is_empty() { "HackRF One".into() } else { format!("HackRF One {}", self.serial.trim()) };
        log::info!("HackRF: receiving at {} kHz, LNA {} dB, VGA {} dB, amplifier off", frequency / 1000, self.lna, self.vga);
        Ok(())
    }
}

impl InputDevice for HackRf {
    fn status(&self) -> DeviceStatus {
        DeviceStatus {
            kind: "hackrf".into(),
            name: self.name.clone(),
            connected: self.dev.is_some(),
            frequency: self.frequency,
            gain_db: self.dev.as_ref().map(|_| (self.lna + self.vga) as f32),
            error: self.error.clone(),
            ..Default::default()
        }
    }

    fn start(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        self.stop();
        match self.open(frequency, callback) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(e) => {
                self.stop();
                self.error = Some(e.clone());
                Err(e)
            }
        }
    }

    fn tune(&mut self, frequency: u32) -> Result<(), String> {
        let (Some(dev), Ok(api)) = (&self.dev, api()) else { return Err("device not open".into()) };
        let r = unsafe { (api.set_freq)(dev.0, frequency as u64) };
        if r != 0 {
            return Err(error(api, "hackrf_set_freq", r));
        }
        self.frequency = frequency;
        Ok(())
    }

    fn stop(&mut self) {
        let Ok(api) = api() else { return };
        if let Some(sh) = self.shared {
            unsafe { (*sh).stop.store(true, Ordering::Relaxed) };
        }
        if let Some(dev) = self.dev.take() {
            unsafe {
                (api.stop_rx)(dev.0);
                (api.close)(dev.0);
            }
        }
        // (the callback runs no more once the device is closed)
        if let Some(sh) = self.shared.take() {
            drop(unsafe { Box::from_raw(sh as *mut Shared) });
        }
    }

    /// The gains are fixed (Settings > Receiver): no AGC.
    fn set_gain_mode(&mut self, _mode: GainMode) {}
}

impl Drop for HackRf {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_the_hackrf_can_set() {
        assert_eq!(gains(16, 30), (16, 30));
        assert_eq!(gains(17, 31), (16, 30));
        assert_eq!(gains(99, 99), (40, 62));
    }

    #[test]
    fn signed_samples_without_their_dc_offset() {
        let mut c = Converter { dc: Complex32::new(0.0, 0.0) };
        let mut out = Vec::new();
        // a constant offset fades out, the signal stays
        let buf: Vec<u8> = (0..200_000).flat_map(|i| [((i % 2) as i8 * 64 + 10) as u8, (-20i8) as u8]).collect();
        c.convert(&buf, &mut out);
        let tail = &out[out.len() - 1000..];
        let mean = tail.iter().sum::<Complex32>() / tail.len() as f32;
        assert!(mean.norm() < 0.01, "{mean}");
        assert!((tail[1].re - tail[0].re).abs() > 0.45);
    }
}
