//! SoapySDR input (libSoapySDR 0.8 loaded at runtime, like librtlsdr): any
//! receiver with a SoapySDR module that can do 2.048 MS/s, among them the
//! SDRplay RSPs (SoapySDRPlay3 with the SDRplay API, which MagicDAB neither
//! links nor ships).
//!
//! Gains: a table from the driver. SDRplay has no gains but gain reductions
//! (RFGR: the LNA state, IFGR: 20-59 dB), lower = more gain, so its table is
//! a path through both, from the lowest gain to the highest in 2 dB steps;
//! other drivers get their overall gain range in 2 dB steps. The software
//! AGC and the server's per-channel gain optimiser walk the table as with an
//! RTL-SDR.
//!
//! SDRplay also needs: the DAB notch off (SoapySDRPlay3 turns it on, and it
//! takes most of Band III away), the FM notch on, and a lower setpoint of its
//! own IF AGC (the peaks of DAB's OFDM made the default one pump).

use crate::agc::{default_gain_index, next_index, room_db, Level, SoftAgc};
use crate::{DeviceStatus, GainMode, InputDevice, SampleCallback, SAMPLE_RATE};
use libloading::{Library, Symbol};
use num_complex::Complex32;
use std::ffi::{c_char, c_int, c_long, c_longlong, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

const SOAPY_SDR_RX: c_int = 1;
const SOAPY_SDR_TIMEOUT: c_int = -1;
const SOAPY_SDR_OVERFLOW: c_int = -4;

type Dev = *mut c_void;
type Stream = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Range {
    minimum: f64,
    maximum: f64,
    step: f64,
}

#[repr(C)]
struct Kwargs {
    size: usize,
    keys: *mut *mut c_char,
    vals: *mut *mut c_char,
}

struct Api {
    _lib: Library,
    abi_version: unsafe extern "C" fn() -> *const c_char,
    last_error: unsafe extern "C" fn() -> *const c_char,
    free: unsafe extern "C" fn(*mut c_void),
    enumerate: unsafe extern "C" fn(*const c_char, *mut usize) -> *mut Kwargs,
    kwargs_list_clear: unsafe extern "C" fn(*mut Kwargs, usize),
    make: unsafe extern "C" fn(*const c_char) -> Dev,
    unmake: unsafe extern "C" fn(Dev) -> c_int,
    driver_key: unsafe extern "C" fn(Dev) -> *mut c_char,
    hardware_key: unsafe extern "C" fn(Dev) -> *mut c_char,
    set_sample_rate: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    get_sample_rate: unsafe extern "C" fn(Dev, c_int, usize) -> f64,
    set_bandwidth: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    set_frequency: unsafe extern "C" fn(Dev, c_int, usize, f64, *const Kwargs) -> c_int,
    set_frequency_correction: Option<unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int>,
    list_gains: unsafe extern "C" fn(Dev, c_int, usize, *mut usize) -> *mut *mut c_char,
    strings_clear: unsafe extern "C" fn(*mut *mut *mut c_char, usize),
    get_gain_range: unsafe extern "C" fn(Dev, c_int, usize) -> Range,
    get_gain_element_range: unsafe extern "C" fn(Dev, c_int, usize, *const c_char) -> Range,
    set_gain: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    set_gain_element: unsafe extern "C" fn(Dev, c_int, usize, *const c_char, f64) -> c_int,
    set_gain_mode: unsafe extern "C" fn(Dev, c_int, usize, bool) -> c_int,
    set_dc_offset_mode: Option<unsafe extern "C" fn(Dev, c_int, usize, bool) -> c_int>,
    write_setting: unsafe extern "C" fn(Dev, *const c_char, *const c_char) -> c_int,
    setup_stream: unsafe extern "C" fn(Dev, c_int, *const c_char, *const usize, usize, *const Kwargs) -> Stream,
    close_stream: unsafe extern "C" fn(Dev, Stream) -> c_int,
    activate_stream: unsafe extern "C" fn(Dev, Stream, c_int, c_longlong, usize) -> c_int,
    deactivate_stream: unsafe extern "C" fn(Dev, Stream, c_int, c_longlong) -> c_int,
    read_stream: unsafe extern "C" fn(Dev, Stream, *const *mut c_void, usize, *mut c_int, *mut c_longlong, c_long) -> c_int,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn library_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        // (PothosSDR or radioconda put it on the PATH)
        &["SoapySDR.dll", "libSoapySDR.dll"]
    } else if cfg!(target_os = "macos") {
        &["libSoapySDR.0.8.dylib", "libSoapySDR.dylib", "/opt/homebrew/lib/libSoapySDR.0.8.dylib", "/usr/local/lib/libSoapySDR.0.8.dylib"]
    } else {
        &["libSoapySDR.so.0.8", "libSoapySDR.so", "/usr/local/lib/libSoapySDR.so.0.8"]
    }
}

fn load_api() -> Result<Api, String> {
    let mut why = Vec::new();
    for name in library_names() {
        let lib = match unsafe { Library::new(name) } {
            Ok(l) => l,
            Err(e) => {
                why.push(format!("{name}: {e}"));
                continue;
            }
        };
        let api = unsafe {
            macro_rules! sym {
                ($n:expr) => {{
                    let s: Symbol<_> = lib.get($n).map_err(|e| format!("{name}: {e}"))?;
                    *s
                }};
            }
            macro_rules! opt {
                ($n:expr) => {
                    lib.get($n).ok().map(|s: Symbol<_>| *s)
                };
            }
            Api {
                abi_version: sym!(b"SoapySDR_getABIVersion\0"),
                last_error: sym!(b"SoapySDRDevice_lastError\0"),
                free: sym!(b"SoapySDR_free\0"),
                enumerate: sym!(b"SoapySDRDevice_enumerateStrArgs\0"),
                kwargs_list_clear: sym!(b"SoapySDRKwargsList_clear\0"),
                make: sym!(b"SoapySDRDevice_makeStrArgs\0"),
                unmake: sym!(b"SoapySDRDevice_unmake\0"),
                driver_key: sym!(b"SoapySDRDevice_getDriverKey\0"),
                hardware_key: sym!(b"SoapySDRDevice_getHardwareKey\0"),
                set_sample_rate: sym!(b"SoapySDRDevice_setSampleRate\0"),
                get_sample_rate: sym!(b"SoapySDRDevice_getSampleRate\0"),
                set_bandwidth: sym!(b"SoapySDRDevice_setBandwidth\0"),
                set_frequency: sym!(b"SoapySDRDevice_setFrequency\0"),
                set_frequency_correction: opt!(b"SoapySDRDevice_setFrequencyCorrection\0"),
                list_gains: sym!(b"SoapySDRDevice_listGains\0"),
                strings_clear: sym!(b"SoapySDRStrings_clear\0"),
                get_gain_range: sym!(b"SoapySDRDevice_getGainRange\0"),
                get_gain_element_range: sym!(b"SoapySDRDevice_getGainElementRange\0"),
                set_gain: sym!(b"SoapySDRDevice_setGain\0"),
                set_gain_element: sym!(b"SoapySDRDevice_setGainElement\0"),
                set_gain_mode: sym!(b"SoapySDRDevice_setGainMode\0"),
                set_dc_offset_mode: opt!(b"SoapySDRDevice_setDCOffsetMode\0"),
                write_setting: sym!(b"SoapySDRDevice_writeSetting\0"),
                setup_stream: sym!(b"SoapySDRDevice_setupStream\0"),
                close_stream: sym!(b"SoapySDRDevice_closeStream\0"),
                activate_stream: sym!(b"SoapySDRDevice_activateStream\0"),
                deactivate_stream: sym!(b"SoapySDRDevice_deactivateStream\0"),
                read_stream: sym!(b"SoapySDRDevice_readStream\0"),
                _lib: lib,
            }
        };
        // (0.7 has another setupStream: the stream comes back through a pointer)
        let abi = unsafe { CStr::from_ptr((api.abi_version)()) }.to_string_lossy().into_owned();
        if !abi.starts_with("0.8") {
            why.push(format!("{name}: SoapySDR ABI {abi}, 0.8 needed"));
            continue;
        }
        return Ok(api);
    }
    Err(format!("libSoapySDR 0.8 not found ({})", why.join("; ")))
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

fn last_error(api: &Api, what: &str) -> String {
    let p = unsafe { (api.last_error)() };
    let msg = if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().trim().to_string() };
    if msg.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {msg}")
    }
}

/// A string SoapySDR made for us (freed here).
fn take_string(api: &Api, p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (api.free)(p as *mut c_void) };
    s
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// The receivers SoapySDR finds: (arguments that open one, its name). Asked
/// at most every half minute (the settings ask often; a search may take a
/// while and an open receiver does not change).
pub fn list_devices() -> Vec<(String, String)> {
    static LAST: Mutex<Option<(std::time::Instant, Vec<(String, String)>)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((_, list)) = last.as_ref().filter(|(at, _)| at.elapsed().as_secs() < 30) {
        return list.clone();
    }
    let list = enumerate();
    *last = Some((std::time::Instant::now(), list.clone()));
    list
}

fn enumerate() -> Vec<(String, String)> {
    let Ok(api) = api() else { return Vec::new() };
    let mut n = 0usize;
    let list = unsafe { (api.enumerate)(c"".as_ptr(), &mut n) };
    if list.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for k in unsafe { std::slice::from_raw_parts(list, n) } {
        let mut pairs = Vec::new();
        for j in 0..k.size {
            let (key, val) = unsafe { (*k.keys.add(j), *k.vals.add(j)) };
            if key.is_null() || val.is_null() {
                continue;
            }
            let key = unsafe { CStr::from_ptr(key) }.to_string_lossy().into_owned();
            let val = unsafe { CStr::from_ptr(val) }.to_string_lossy().into_owned();
            pairs.push((key, val));
        }
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        // driver and serial open this one again (labels may have spaces and commas)
        let mut args = get("driver").map(|d| format!("driver={d}")).unwrap_or_default();
        if let Some(s) = get("serial") {
            args += &format!("{}serial={s}", if args.is_empty() { "" } else { "," });
        }
        let label = get("label").unwrap_or_else(|| args.clone());
        out.push((args, label));
    }
    unsafe { (api.kwargs_list_clear)(list, n) };
    out
}

/// One gain of the table: what to set and how many dB it is (roughly, for
/// SDRplay: 98 dB less the reductions, the LNA state counted as 6 dB).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Gain {
    Overall(f64),
    Reduction { rfgr: u32, ifgr: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Step {
    gain: Gain,
    db: f32,
}

/// SDRplay: from the lowest gain to the highest, 2 dB at a time. The IF
/// reduction stays in a band that moves down with the LNA state, so the LNA
/// gives its gain first and the IF stage is not driven hard (for an RSP1A in
/// Band III: RFGR 7 / IFGR 56 up to RFGR 0 / IFGR 20).
fn reduction_table(rf: Range, ifr: Range) -> Vec<Step> {
    let rf_top = (rf.maximum.max(0.0) as u32).min(7);
    let rf_low = rf.minimum.max(0.0) as u32;
    let (if_min, if_max) = (ifr.minimum.max(0.0) as u32, ifr.maximum.max(0.0) as u32);
    let mut out = Vec::new();
    for rfgr in (rf_low..=rf_top).rev() {
        let hi = if rfgr == rf_top { if_max.min(56) } else { 30 + 2 * rfgr };
        let lo = if rfgr == rf_low { if_min } else { 24 + 2 * rfgr };
        let (lo, hi) = (lo.clamp(if_min, if_max), hi.clamp(if_min, if_max));
        let mut ifgr = hi;
        while ifgr >= lo {
            let db = 98.0 - (6 * rfgr + ifgr) as f32;
            if out.last().is_none_or(|s: &Step| db > s.db) {
                out.push(Step { gain: Gain::Reduction { rfgr, ifgr }, db });
            }
            if ifgr < lo + 2 {
                break;
            }
            ifgr -= 2;
        }
    }
    out
}

/// Other drivers: their overall gain range, 2 dB at a time (or their step).
fn overall_table(r: Range) -> Vec<Step> {
    if r.maximum.is_nan() || r.maximum <= r.minimum {
        return vec![Step { gain: Gain::Overall(r.minimum), db: r.minimum as f32 }];
    }
    let step = r.step.max(2.0);
    let mut out = Vec::new();
    let mut g = r.minimum;
    while g < r.maximum - 0.01 {
        out.push(Step { gain: Gain::Overall(g), db: g as f32 });
        g += step;
    }
    out.push(Step { gain: Gain::Overall(r.maximum), db: r.maximum as f32 });
    out
}

struct DevPtr(Dev);
unsafe impl Send for DevPtr {}
unsafe impl Sync for DevPtr {}

struct StreamPtr(Stream);
unsafe impl Send for StreamPtr {}
unsafe impl Sync for StreamPtr {}

struct Shared {
    dev: DevPtr,
    stream: StreamPtr,
    /// Control calls one at a time (the read thread runs beside them).
    ctrl: Mutex<()>,
    callback: SampleCallback,
    agc: Mutex<SoftAgc>,
    table: Vec<Step>,
    gain_index: AtomicUsize,
    gain_ceiling: AtomicUsize,
    /// Asked for by the AGC in the read thread, set by the gain thread.
    gain_request: AtomicUsize,
    software_agc: AtomicBool,
    stop: AtomicBool,
    running: AtomicBool,
    level: Level,
    /// The RFGR set now (SDRplay: only set when it changes, it is slow).
    rfgr: Mutex<Option<u32>>,
}

impl Shared {
    fn set_gain_index(&self, api: &Api, i: usize) {
        let Some(step) = self.table.get(i) else { return };
        let _g = self.ctrl.lock().unwrap();
        let dev = self.dev.0;
        let ok = unsafe {
            match step.gain {
                Gain::Overall(db) => (api.set_gain)(dev, SOAPY_SDR_RX, 0, db) == 0,
                Gain::Reduction { rfgr, ifgr } => {
                    let mut current = self.rfgr.lock().unwrap();
                    let rf_ok = *current == Some(rfgr) || (api.set_gain_element)(dev, SOAPY_SDR_RX, 0, c"RFGR".as_ptr(), rfgr as f64) == 0;
                    if rf_ok {
                        *current = Some(rfgr);
                    }
                    rf_ok && (api.set_gain_element)(dev, SOAPY_SDR_RX, 0, c"IFGR".as_ptr(), ifgr as f64) == 0
                }
            }
        };
        if ok {
            self.gain_index.store(i, Ordering::Relaxed);
        } else {
            log::warn!("{}", last_error(api, &format!("SoapySDR: cannot set the gain {:?}", step.gain)));
        }
    }
}

fn read_thread(sh: Arc<Shared>) {
    let Ok(api) = api() else { return };
    let mut buf = vec![Complex32::new(0.0, 0.0); 32768];
    let mut failures = 0u32;
    let mut overflows = 0u64;
    while !sh.stop.load(Ordering::Relaxed) {
        let mut flags: c_int = 0;
        let mut time: c_longlong = 0;
        let ptrs = [buf.as_mut_ptr() as *mut c_void];
        let n = unsafe { (api.read_stream)(sh.dev.0, sh.stream.0, ptrs.as_ptr(), buf.len(), &mut flags, &mut time, 200_000) };
        if n > 0 {
            failures = 0;
            let samples = &buf[..n as usize];
            (sh.callback)(samples);
            agc_step(&sh, samples);
            continue;
        }
        match n {
            SOAPY_SDR_TIMEOUT => continue,
            SOAPY_SDR_OVERFLOW => {
                overflows += 1;
                if overflows.is_power_of_two() {
                    log::debug!("SoapySDR: {overflows} overflows (samples lost)");
                }
            }
            _ => {
                failures += 1;
                if failures >= 20 {
                    log::warn!("SoapySDR: the stream ended (readStream {n}), receiver unplugged?");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    sh.running.store(false, Ordering::Relaxed);
}

fn agc_step(sh: &Shared, samples: &[Complex32]) {
    let i = sh.gain_index.load(Ordering::Relaxed);
    let up_ratio = match (sh.table.get(i), sh.table.get(i + 1)) {
        (Some(a), Some(b)) => 10f32.powf((b.db - a.db) / 10.0),
        _ => 1.0,
    };
    let mut agc = sh.agc.lock().unwrap();
    agc.feed_cf32(samples);
    let windows = agc.windows;
    let action = agc.evaluate(up_ratio);
    if agc.windows != windows {
        sh.level.evaluated(i, agc.mean_power);
    }
    drop(agc);
    if sh.software_agc.load(Ordering::Relaxed) {
        let ceiling = sh.gain_ceiling.load(Ordering::Relaxed);
        if let Some(n) = next_index(action, i, ceiling, sh.table.len()) {
            sh.gain_request.store(n, Ordering::Relaxed);
        }
    }
}

fn gain_thread(sh: Arc<Shared>) {
    let Ok(api) = api() else { return };
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(40));
        let n = sh.gain_request.swap(usize::MAX, Ordering::Relaxed);
        if n != usize::MAX && sh.software_agc.load(Ordering::Relaxed) && n != sh.gain_index.load(Ordering::Relaxed) {
            sh.set_gain_index(api, n);
        }
    }
}

pub struct SoapySdr {
    args: String,
    bias_tee: bool,
    ppm: i32,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
    gain_thread: Option<JoinHandle<()>>,
    ceiling: Option<usize>,
    frequency: u32,
    mode: GainMode,
    error: Option<String>,
    name: String,
    /// SDRplay: gain reductions and settings of its own.
    sdrplay: bool,
}

unsafe impl Send for SoapySdr {}

impl SoapySdr {
    /// `args`: SoapySDR device arguments ("driver=sdrplay", "driver=airspy,serial=…"), empty for the first receiver found.
    pub fn new(args: String, bias_tee: bool) -> Self {
        SoapySdr {
            args,
            bias_tee,
            ppm: 0,
            shared: None,
            thread: None,
            gain_thread: None,
            ceiling: None,
            frequency: 0,
            mode: GainMode::Software,
            error: None,
            name: String::new(),
            sdrplay: false,
        }
    }

    fn write(api: &Api, dev: Dev, key: &str, value: &str) {
        let (k, v) = (cstr(key), cstr(value));
        if unsafe { (api.write_setting)(dev, k.as_ptr(), v.as_ptr()) } != 0 {
            log::debug!("{}", last_error(api, &format!("SoapySDR: setting {key}={value}")));
        }
    }

    /// SDRplay's settings: the DAB notch off, the FM notch on, bias-T as set.
    fn sdrplay_settings(&self, api: &Api, dev: Dev) {
        Self::write(api, dev, "dabnotch_ctrl", "false");
        Self::write(api, dev, "rfnotch_ctrl", "true");
        Self::write(api, dev, "biasT_ctrl", if self.bias_tee { "true" } else { "false" });
    }

    fn gain_table(&mut self, api: &Api, dev: Dev) -> Vec<Step> {
        let mut n = 0usize;
        let names = unsafe { (api.list_gains)(dev, SOAPY_SDR_RX, 0, &mut n) };
        let mut gains = Vec::new();
        if !names.is_null() {
            for i in 0..n {
                let p = unsafe { *names.add(i) };
                if !p.is_null() {
                    gains.push(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned());
                }
            }
            let mut names = names;
            unsafe { (api.strings_clear)(&mut names, n) };
        }
        self.sdrplay = gains.iter().any(|g| g == "RFGR") && gains.iter().any(|g| g == "IFGR");
        let table = if self.sdrplay {
            let rf = unsafe { (api.get_gain_element_range)(dev, SOAPY_SDR_RX, 0, c"RFGR".as_ptr()) };
            let ifr = unsafe { (api.get_gain_element_range)(dev, SOAPY_SDR_RX, 0, c"IFGR".as_ptr()) };
            reduction_table(rf, ifr)
        } else {
            overall_table(unsafe { (api.get_gain_range)(dev, SOAPY_SDR_RX, 0) })
        };
        log::info!(
            "SoapySDR: gains {} ({} steps, {:.0} to {:.0} dB)",
            gains.join(", "),
            table.len(),
            table.first().map_or(0.0, |s| s.db),
            table.last().map_or(0.0, |s| s.db)
        );
        table
    }

    fn open(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        let api = api()?;
        let args = cstr(self.args.trim());
        let dev = unsafe { (api.make)(args.as_ptr()) };
        if dev.is_null() {
            return Err(last_error(api, &format!("no SoapySDR receiver for \"{}\"", self.args.trim())));
        }
        // (closed by stop() from here on, also when something below fails)
        let driver = take_string(api, unsafe { (api.driver_key)(dev) });
        let hardware = take_string(api, unsafe { (api.hardware_key)(dev) });
        self.name = format!("{driver} {hardware}").trim().to_string();
        let table = self.gain_table(api, dev);
        let fail = |what: &str| {
            let e = last_error(api, what);
            unsafe { (api.unmake)(dev) };
            Err(e)
        };
        unsafe {
            if (api.set_sample_rate)(dev, SOAPY_SDR_RX, 0, SAMPLE_RATE as f64) != 0 {
                return fail("SoapySDR: cannot set 2.048 MS/s");
            }
            let rate = (api.get_sample_rate)(dev, SOAPY_SDR_RX, 0);
            if (rate - SAMPLE_RATE as f64).abs() > 100.0 {
                let e = format!("{}: no 2.048 MS/s (it gives {:.3} MS/s), DAB needs exactly that", self.name, rate / 1e6);
                (api.unmake)(dev);
                return Err(e);
            }
            // (the width of a DAB block; a driver without it takes the nearest)
            (api.set_bandwidth)(dev, SOAPY_SDR_RX, 0, 1_536_000.0);
            if let Some(dc) = api.set_dc_offset_mode {
                dc(dev, SOAPY_SDR_RX, 0, true);
            }
            if self.ppm != 0 {
                if let Some(f) = api.set_frequency_correction {
                    f(dev, SOAPY_SDR_RX, 0, self.ppm as f64);
                }
            }
            if self.sdrplay {
                self.sdrplay_settings(api, dev);
            }
            if (api.set_frequency)(dev, SOAPY_SDR_RX, 0, frequency as f64, std::ptr::null()) != 0 {
                return fail("SoapySDR: cannot tune");
            }
        }
        let channels = [0usize];
        let stream = unsafe { (api.setup_stream)(dev, SOAPY_SDR_RX, c"CF32".as_ptr(), channels.as_ptr(), 1, std::ptr::null()) };
        if stream.is_null() {
            return fail("SoapySDR: cannot set up the stream");
        }
        if unsafe { (api.activate_stream)(dev, stream, 0, 0, 0) } != 0 {
            unsafe { (api.close_stream)(dev, stream) };
            return fail("SoapySDR: cannot start the stream");
        }
        // (some SoapySDRPlay3 versions apply these only once streaming)
        if self.sdrplay {
            self.sdrplay_settings(api, dev);
        }
        let tenths: Vec<i32> = table.iter().map(|s| (s.db * 10.0).round() as i32).collect();
        let start = default_gain_index(&tenths);
        let ceiling = self.ceiling.unwrap_or(start).min(table.len().saturating_sub(1));
        let shared = Arc::new(Shared {
            dev: DevPtr(dev),
            stream: StreamPtr(stream),
            ctrl: Mutex::new(()),
            callback,
            agc: Mutex::new(SoftAgc::new()),
            table,
            gain_index: AtomicUsize::new(start.min(ceiling)),
            gain_ceiling: AtomicUsize::new(ceiling),
            gain_request: AtomicUsize::new(usize::MAX),
            software_agc: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(true),
            level: Level::default(),
            rfgr: Mutex::new(None),
        });
        self.shared = Some(shared.clone());
        self.frequency = frequency;
        self.apply_gain();
        let sh = shared.clone();
        self.gain_thread = std::thread::Builder::new().name("soapy-gain".into()).spawn(move || gain_thread(sh)).ok();
        self.thread = Some(std::thread::Builder::new().name("soapy".into()).spawn(move || read_thread(shared)).map_err(|e| e.to_string())?);
        log::info!("SoapySDR: {} receiving at {} kHz", self.name, frequency / 1000);
        Ok(())
    }

    fn apply_gain(&self) {
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return };
        let dev = sh.dev.0;
        match &self.mode {
            GainMode::Hardware => {
                sh.software_agc.store(false, Ordering::Relaxed);
                let _g = sh.ctrl.lock().unwrap();
                if self.sdrplay {
                    // (its default setpoint pumps on DAB's peaks)
                    Self::write(api, dev, "agc_setpoint", "-34");
                }
                unsafe { (api.set_gain_mode)(dev, SOAPY_SDR_RX, 0, true) };
            }
            GainMode::Software => {
                {
                    let _g = sh.ctrl.lock().unwrap();
                    unsafe { (api.set_gain_mode)(dev, SOAPY_SDR_RX, 0, false) };
                }
                sh.software_agc.store(true, Ordering::Relaxed);
                let i = sh.gain_index.load(Ordering::Relaxed).min(sh.gain_ceiling.load(Ordering::Relaxed));
                sh.set_gain_index(api, i.min(sh.table.len().saturating_sub(1)));
            }
            GainMode::Manual { index } => {
                sh.software_agc.store(false, Ordering::Relaxed);
                {
                    let _g = sh.ctrl.lock().unwrap();
                    unsafe { (api.set_gain_mode)(dev, SOAPY_SDR_RX, 0, false) };
                }
                sh.set_gain_index(api, (*index).min(sh.table.len().saturating_sub(1)));
            }
        }
    }
}

impl InputDevice for SoapySdr {
    fn status(&self) -> DeviceStatus {
        let (gains, gi, level) = match &self.shared {
            Some(sh) => (sh.table.iter().map(|s| s.db).collect::<Vec<_>>(), Some(sh.gain_index.load(Ordering::Relaxed)), sh.level.load()),
            None => (Vec::new(), None, None),
        };
        let hardware = matches!(self.mode, GainMode::Hardware);
        let gain_db = gi.and_then(|i| gains.get(i).copied()).filter(|_| !hardware);
        let level = level.filter(|_| !hardware);
        let rf_level = level.and_then(|(i, power)| gains.get(i).map(|&g| crate::estimate_rf_level(g, power)));
        let gain_room = level.filter(|&(i, _)| Some(i) == gi).map(|(_, power)| room_db(power));
        DeviceStatus {
            kind: "soapy".into(),
            name: if self.name.is_empty() { "SoapySDR".into() } else { self.name.clone() },
            connected: self.shared.as_ref().is_some_and(|sh| sh.running.load(Ordering::Relaxed)),
            frequency: self.frequency,
            gain_db,
            gain_index: gi,
            gains,
            rf_level,
            gain_room,
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
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return Err("device not open".into()) };
        let r = {
            let _g = sh.ctrl.lock().unwrap();
            unsafe { (api.set_frequency)(sh.dev.0, SOAPY_SDR_RX, 0, frequency as f64, std::ptr::null()) }
        };
        if r != 0 {
            return Err(last_error(api, "SoapySDR: cannot tune"));
        }
        sh.level.restart();
        self.frequency = frequency;
        Ok(())
    }

    fn stop(&mut self) {
        let Some(sh) = self.shared.take() else { return };
        sh.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.gain_thread.take() {
            let _ = t.join();
        }
        // (the read thread returns within its timeout)
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Ok(api) = api() {
            unsafe {
                (api.deactivate_stream)(sh.dev.0, sh.stream.0, 0, 0);
                (api.close_stream)(sh.dev.0, sh.stream.0);
                (api.unmake)(sh.dev.0);
            }
        }
    }

    fn set_gain_mode(&mut self, mode: GainMode) {
        self.mode = mode;
        self.apply_gain();
    }

    fn set_gain_ceiling(&mut self, index: Option<usize>, jump: bool) {
        self.ceiling = index;
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return };
        let tenths: Vec<i32> = sh.table.iter().map(|s| (s.db * 10.0).round() as i32).collect();
        let c = index.unwrap_or_else(|| default_gain_index(&tenths)).min(sh.table.len().saturating_sub(1));
        sh.gain_ceiling.store(c, Ordering::Relaxed);
        let i = sh.gain_index.load(Ordering::Relaxed);
        if sh.software_agc.load(Ordering::Relaxed) && (i > c || (jump && i != c)) {
            sh.gain_request.store(usize::MAX, Ordering::Relaxed);
            sh.set_gain_index(api, c);
        }
    }

    fn set_ppm(&mut self, ppm: i32) {
        self.ppm = ppm;
        if let (Some(sh), Ok(api)) = (&self.shared, api()) {
            if let Some(f) = api.set_frequency_correction {
                let _g = sh.ctrl.lock().unwrap();
                unsafe { f(sh.dev.0, SOAPY_SDR_RX, 0, ppm as f64) };
            }
        }
    }
}

impl Drop for SoapySdr {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rsp1a_table_climbs_two_db_at_a_time() {
        // RSP1A in Band III: RFGR 0..9, IFGR 20..59
        let t = reduction_table(Range { minimum: 0.0, maximum: 9.0, step: 1.0 }, Range { minimum: 20.0, maximum: 59.0, step: 1.0 });
        assert_eq!(t.first().unwrap().gain, Gain::Reduction { rfgr: 7, ifgr: 56 });
        assert_eq!(t.last().unwrap().gain, Gain::Reduction { rfgr: 0, ifgr: 20 });
        assert!(t.windows(2).all(|w| w[1].db > w[0].db), "gains rise");
        assert!(t.windows(2).all(|w| w[1].db - w[0].db <= 2.0 + 1e-3), "2 dB steps");
        assert_eq!((t.first().unwrap().db, t.last().unwrap().db), (0.0, 78.0));
        // all in the driver's ranges
        assert!(t.iter().all(|s| matches!(s.gain, Gain::Reduction { rfgr, ifgr } if rfgr <= 7 && (20..=59).contains(&ifgr))));
    }

    #[test]
    fn a_model_with_fewer_lna_states_still_gets_a_rising_table() {
        let t = reduction_table(Range { minimum: 0.0, maximum: 4.0, step: 1.0 }, Range { minimum: 20.0, maximum: 59.0, step: 1.0 });
        assert!(t.len() > 10);
        assert!(t.windows(2).all(|w| w[1].db > w[0].db));
        assert!(t.iter().all(|s| matches!(s.gain, Gain::Reduction { rfgr, .. } if rfgr <= 4)));
    }

    #[test]
    fn other_drivers_step_through_their_overall_range() {
        let t = overall_table(Range { minimum: 0.0, maximum: 49.6, step: 0.0 });
        assert_eq!(t.first().unwrap().db, 0.0);
        assert_eq!(t.last().unwrap().db, 49.6);
        assert!(t.windows(2).all(|w| w[1].db > w[0].db));
        assert_eq!(overall_table(Range { minimum: 10.0, maximum: 10.0, step: 1.0 }).len(), 1);
    }
}
