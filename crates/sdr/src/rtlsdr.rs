//! RTL-SDR input via librtlsdr loaded at runtime.

use crate::agc::{default_gain_index, next_index, SoftAgc};
use crate::{convert_u8, DeviceStatus, GainMode, InputDevice, SampleCallback, SAMPLE_RATE};
use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;

type Dev = *mut c_void;
type ReadCb = unsafe extern "C" fn(buf: *mut u8, len: u32, ctx: *mut c_void);

struct Api {
    _lib: Library,
    get_device_count: unsafe extern "C" fn() -> u32,
    get_device_name: unsafe extern "C" fn(u32) -> *const c_char,
    get_device_usb_strings: unsafe extern "C" fn(u32, *mut c_char, *mut c_char, *mut c_char) -> c_int,
    open: unsafe extern "C" fn(*mut Dev, u32) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    set_center_freq: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_freq_correction: unsafe extern "C" fn(Dev, c_int) -> c_int,
    get_tuner_gains: unsafe extern "C" fn(Dev, *mut c_int) -> c_int,
    set_tuner_gain: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_tuner_gain_mode: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_sample_rate: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_agc_mode: unsafe extern "C" fn(Dev, c_int) -> c_int,
    reset_buffer: unsafe extern "C" fn(Dev) -> c_int,
    read_async: unsafe extern "C" fn(Dev, ReadCb, *mut c_void, u32, u32) -> c_int,
    cancel_async: unsafe extern "C" fn(Dev) -> c_int,
    set_bias_tee: Option<unsafe extern "C" fn(Dev, c_int) -> c_int>,
    /// Android: a device the app opened (build/android/rtlsdr-open-fd.patch).
    open_fd: Option<unsafe extern "C" fn(*mut Dev, c_int) -> c_int>,
    get_usb_strings: Option<unsafe extern "C" fn(Dev, *mut c_char, *mut c_char, *mut c_char) -> c_int>,
    get_xtal_freq: Option<unsafe extern "C" fn(Dev, *mut u32, *mut u32) -> c_int>,
    /// The file it was loaded from (for the log).
    name: String,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn library_names() -> Vec<std::ffi::OsString> {
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["rtlsdr.dll", "librtlsdr.dll"]
    } else if cfg!(target_os = "android") {
        // (next to the server in the app's native library folder)
        &["librtlsdr.so"]
    } else if cfg!(target_os = "macos") {
        &["librtlsdr.0.dylib", "librtlsdr.dylib", "/opt/homebrew/lib/librtlsdr.dylib", "/usr/local/lib/librtlsdr.dylib"]
    } else {
        &["librtlsdr.so.0", "librtlsdr.so", "/usr/local/lib/librtlsdr.so.0"]
    };
    // the library a package brings next to the program comes first (Linux and
    // macOS do not look there by themselves; distributions often have an old
    // one that does not know the RTL-SDR Blog V4)
    let own = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(names[0]))).filter(|p| !cfg!(windows) && p.exists());
    own.into_iter().map(|p| p.into_os_string()).chain(names.iter().map(|n| n.into())).collect()
}

/// The error with its cause ("LoadLibraryExW failed" alone does not say
/// whether the file or one of its DLLs is missing).
fn describe(e: &libloading::Error) -> String {
    let mut s = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        s += &format!(": {c}");
        cause = c.source();
    }
    s
}

fn load_api() -> Result<Api, String> {
    // Windows: the package brings rtlsdr.dll next to the program; started
    // from inside the zip (Explorer unpacks only the program) it is not there
    if cfg!(windows) {
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
            if !dir.join("rtlsdr.dll").exists() {
                return Err(format!(
                    "rtlsdr.dll is not next to the program in {}: unpack the whole zip and start magicdab.exe from there",
                    dir.display()
                ));
            }
        }
    }
    // (the first error is that of the package's own library)
    let mut first_err = None;
    for name in library_names() {
        match unsafe { Library::new(&name) } {
            Ok(lib) => unsafe {
                macro_rules! sym {
                    ($n:expr) => {{
                        let s: Symbol<_> = lib.get($n).map_err(|e| e.to_string())?;
                        *s
                    }};
                }
                let set_bias_tee = lib.get(b"rtlsdr_set_bias_tee\0").ok().map(|s: Symbol<unsafe extern "C" fn(Dev, c_int) -> c_int>| *s);
                let open_fd = lib.get(b"rtlsdr_open_fd\0").ok().map(|s: Symbol<unsafe extern "C" fn(*mut Dev, c_int) -> c_int>| *s);
                type Strings = unsafe extern "C" fn(Dev, *mut c_char, *mut c_char, *mut c_char) -> c_int;
                let get_usb_strings = lib.get(b"rtlsdr_get_usb_strings\0").ok().map(|s: Symbol<Strings>| *s);
                let get_xtal_freq =
                    lib.get(b"rtlsdr_get_xtal_freq\0").ok().map(|s: Symbol<unsafe extern "C" fn(Dev, *mut u32, *mut u32) -> c_int>| *s);
                let api = Api {
                    get_device_count: sym!(b"rtlsdr_get_device_count\0"),
                    get_device_name: sym!(b"rtlsdr_get_device_name\0"),
                    get_device_usb_strings: sym!(b"rtlsdr_get_device_usb_strings\0"),
                    open: sym!(b"rtlsdr_open\0"),
                    close: sym!(b"rtlsdr_close\0"),
                    set_center_freq: sym!(b"rtlsdr_set_center_freq\0"),
                    set_freq_correction: sym!(b"rtlsdr_set_freq_correction\0"),
                    get_tuner_gains: sym!(b"rtlsdr_get_tuner_gains\0"),
                    set_tuner_gain: sym!(b"rtlsdr_set_tuner_gain\0"),
                    set_tuner_gain_mode: sym!(b"rtlsdr_set_tuner_gain_mode\0"),
                    set_sample_rate: sym!(b"rtlsdr_set_sample_rate\0"),
                    set_agc_mode: sym!(b"rtlsdr_set_agc_mode\0"),
                    reset_buffer: sym!(b"rtlsdr_reset_buffer\0"),
                    read_async: sym!(b"rtlsdr_read_async\0"),
                    cancel_async: sym!(b"rtlsdr_cancel_async\0"),
                    set_bias_tee,
                    open_fd,
                    get_usb_strings,
                    get_xtal_freq,
                    name: name.to_string_lossy().into_owned(),
                    _lib: lib,
                };
                return Ok(api);
            },
            Err(e) => {
                first_err.get_or_insert_with(|| format!("{}: {}", name.to_string_lossy(), describe(&e)));
            }
        }
    }
    Err(format!("librtlsdr not found ({})", first_err.unwrap_or_default()))
}

fn api() -> Result<&'static Api, String> {
    use std::sync::OnceLock;
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

pub fn list_devices() -> Vec<String> {
    if cfg!(target_os = "android") {
        // (the server sees no USB devices there: the app lists them)
        return crate::android_usb::list();
    }
    let Ok(api) = api() else { return Vec::new() };
    let n = unsafe { (api.get_device_count)() };
    (0..n)
        .map(|i| unsafe {
            let mut m = [0 as c_char; 256];
            let mut p = [0 as c_char; 256];
            let mut s = [0 as c_char; 256];
            (api.get_device_usb_strings)(i, m.as_mut_ptr(), p.as_mut_ptr(), s.as_mut_ptr());
            let name = CStr::from_ptr((api.get_device_name)(i)).to_string_lossy();
            format!(
                "{}: {} {} SN:{} ({})",
                i,
                CStr::from_ptr(m.as_ptr()).to_string_lossy(),
                CStr::from_ptr(p.as_ptr()).to_string_lossy(),
                CStr::from_ptr(s.as_ptr()).to_string_lossy(),
                name
            )
        })
        .collect()
}

/// An RTL-SDR Blog V4 the library does not know (librtlsdr before 2.0.1): it
/// takes the stick for a plain R828D with a 16 MHz crystal (the V4 has 28.8
/// MHz), so it tunes 1.8 times too high on the wrong input and hears nothing.
fn v4_unknown_to_library(manufacturer: &str, product: &str, tuner_xtal: u32) -> bool {
    manufacturer == "RTLSDRBlog" && product == "Blog V4" && tuner_xtal < 20_000_000
}

/// The tuner crystal of an RTL-SDR Blog V4 the library does not know.
fn v4_wrong_crystal(api: &Api, dev: Dev) -> Option<u32> {
    let (strings, xtal) = (api.get_usb_strings?, api.get_xtal_freq?);
    let mut m = [0 as c_char; 256];
    let mut p = [0 as c_char; 256];
    let mut s = [0 as c_char; 256];
    let (mut rtl, mut tuner) = (0u32, 0u32);
    unsafe {
        if strings(dev, m.as_mut_ptr(), p.as_mut_ptr(), s.as_mut_ptr()) != 0 || xtal(dev, &mut rtl, &mut tuner) != 0 {
            return None;
        }
        let (m, p) = (CStr::from_ptr(m.as_ptr()).to_string_lossy(), CStr::from_ptr(p.as_ptr()).to_string_lossy());
        v4_unknown_to_library(m.trim(), p.trim(), tuner).then_some(tuner)
    }
}

struct DevPtr(Dev);
unsafe impl Send for DevPtr {}
unsafe impl Sync for DevPtr {}

struct Shared {
    dev: DevPtr,
    /// librtlsdr is not thread safe: all control transfers are serialised.
    ctrl: Mutex<()>,
    callback: SampleCallback,
    agc: Mutex<SoftAgc>,
    gains: Vec<i32>,
    gain_index: AtomicUsize,
    /// Highest gain index the software AGC may use.
    gain_ceiling: AtomicUsize,
    /// Gain index requested by the AGC (usize::MAX = none). Control transfers
    /// are not possible from the USB callback (libusb returns BUSY for nested
    /// event handling), the gain thread applies the request.
    gain_request: AtomicUsize,
    stop: AtomicBool,
    /// The USB read loop runs (it ends when the stick is unplugged or reset).
    running: AtomicBool,
    software_agc: AtomicBool,
    samples: Mutex<Vec<num_complex::Complex32>>,
    /// Copy of the USB buffer: librtlsdr's zero-copy buffers can be uncached
    /// memory (Raspberry Pi), where every single byte read is slow.
    raw: Mutex<Vec<u8>>,
    mean_power_bits: AtomicU32,
}

impl Shared {
    /// Set the tuner gain (caller must not be the USB callback).
    fn set_gain_index(&self, api: &Api, i: usize) {
        let Some(&g) = self.gains.get(i) else { return };
        let _g = self.ctrl.lock().unwrap();
        let r = unsafe { (api.set_tuner_gain)(self.dev.0, g) };
        if r == 0 {
            self.gain_index.store(i, Ordering::Relaxed);
        } else {
            log::warn!("rtlsdr_set_tuner_gain({g}) failed ({r})");
        }
    }
}

pub struct RtlSdr {
    index: u32,
    ppm: i32,
    bias_tee: bool,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
    gain_thread: Option<JoinHandle<()>>,
    /// Gain ceiling set by the server (kept across restarts of the device).
    ceiling: Option<usize>,
    frequency: u32,
    mode: GainMode,
    error: Option<String>,
    /// The stick will hear nothing (DeviceStatus::warning).
    warning: Option<String>,
    name: String,
    /// Android: the device opened by the app (open while librtlsdr uses it).
    usb_fd: Option<crate::android_usb::UsbFd>,
}

impl RtlSdr {
    pub fn new(index: u32, ppm: i32, bias_tee: bool) -> Self {
        RtlSdr {
            index,
            ppm,
            bias_tee,
            shared: None,
            thread: None,
            gain_thread: None,
            ceiling: None,
            frequency: 0,
            mode: GainMode::Software,
            error: None,
            warning: None,
            name: String::new(),
            usb_fd: None,
        }
    }

    fn apply_gain(&self) {
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return };
        let _g = sh.ctrl.lock().unwrap();
        unsafe {
            match &self.mode {
                GainMode::Hardware => {
                    sh.software_agc.store(false, Ordering::Relaxed);
                    (api.set_tuner_gain_mode)(sh.dev.0, 0);
                    (api.set_agc_mode)(sh.dev.0, 1);
                }
                GainMode::Software => {
                    sh.software_agc.store(true, Ordering::Relaxed);
                    (api.set_agc_mode)(sh.dev.0, 0);
                    (api.set_tuner_gain_mode)(sh.dev.0, 1);
                    let i = sh
                        .gain_index
                        .load(Ordering::Relaxed)
                        .min(sh.gain_ceiling.load(Ordering::Relaxed))
                        .min(sh.gains.len().saturating_sub(1));
                    if let Some(&g) = sh.gains.get(i) {
                        if (api.set_tuner_gain)(sh.dev.0, g) == 0 {
                            sh.gain_index.store(i, Ordering::Relaxed);
                        }
                    }
                }
                GainMode::Manual { index } => {
                    sh.software_agc.store(false, Ordering::Relaxed);
                    (api.set_agc_mode)(sh.dev.0, 0);
                    (api.set_tuner_gain_mode)(sh.dev.0, 1);
                    let i = (*index).min(sh.gains.len().saturating_sub(1));
                    sh.gain_index.store(i, Ordering::Relaxed);
                    if let Some(&g) = sh.gains.get(i) {
                        (api.set_tuner_gain)(sh.dev.0, g);
                    }
                }
            }
        }
    }
}

unsafe extern "C" fn read_cb(buf: *mut u8, len: u32, ctx: *mut c_void) {
    let sh = &*(ctx as *const Shared);
    // one bulk copy, then the conversion and the AGC read the copy (on a
    // Raspberry Pi 3 reading the zero-copy buffer byte by byte took most of a core)
    let mut raw = sh.raw.lock().unwrap();
    raw.clear();
    raw.extend_from_slice(std::slice::from_raw_parts(buf, len as usize));
    let data: &[u8] = &raw;
    {
        let mut samples = sh.samples.lock().unwrap();
        convert_u8(data, &mut samples);
        (sh.callback)(&samples);
    }
    let i = sh.gain_index.load(Ordering::Relaxed);
    let up_ratio = match (sh.gains.get(i), sh.gains.get(i + 1)) {
        (Some(&a), Some(&b)) => 10f32.powf((b - a) as f32 / 100.0),
        _ => 1.0,
    };
    let mut agc = sh.agc.lock().unwrap();
    agc.feed(data);
    let action = agc.evaluate(up_ratio);
    sh.mean_power_bits.store(agc.mean_power.to_bits(), Ordering::Relaxed);
    drop(agc);
    if sh.software_agc.load(Ordering::Relaxed) {
        let ceiling = sh.gain_ceiling.load(Ordering::Relaxed);
        if let Some(n) = next_index(action, i, ceiling, sh.gains.len()) {
            sh.gain_request.store(n, Ordering::Relaxed);
        }
    }
}

/// Applies gain changes requested by the software AGC.
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

impl InputDevice for RtlSdr {
    fn status(&self) -> DeviceStatus {
        let (gains, gi, _mean) = match &self.shared {
            Some(sh) => (
                sh.gains.iter().map(|&g| g as f32 / 10.0).collect::<Vec<_>>(),
                Some(sh.gain_index.load(Ordering::Relaxed)),
                f32::from_bits(sh.mean_power_bits.load(Ordering::Relaxed)),
            ),
            None => (Vec::new(), None, 0.0),
        };
        let gain_db = gi.and_then(|i| gains.get(i).copied());
        DeviceStatus {
            kind: "rtlsdr".into(),
            name: self.name.clone(),
            connected: self.shared.as_ref().is_some_and(|sh| sh.running.load(Ordering::Relaxed)),
            frequency: self.frequency,
            gain_db: if matches!(self.mode, GainMode::Hardware) { None } else { gain_db },
            gain_index: gi,
            gains,
            // no calibrated RF level estimate for this driver
            rf_level: None,
            error: self.error.clone(),
            warning: self.warning.clone(),
            position: None,
            duration: None,
            ..Default::default()
        }
    }

    fn start(&mut self, frequency: u32, callback: SampleCallback) -> Result<(), String> {
        self.stop();
        self.warning = None;
        let api = api().inspect_err(|e| {
            self.error = Some(e.clone());
        })?;
        let mut dev: Dev = std::ptr::null_mut();
        let r = if cfg!(target_os = "android") {
            // the app opens the stick (android_usb.rs)
            let fd = crate::android_usb::request_fd().inspect_err(|e| self.error = Some(e.clone()))?;
            let open_fd = api.open_fd.ok_or("librtlsdr without rtlsdr_open_fd")?;
            let r = unsafe { open_fd(&mut dev, crate::android_usb::raw(&fd)) };
            self.usb_fd = Some(fd);
            r
        } else {
            unsafe { (api.open)(&mut dev, self.index) }
        };
        if r < 0 || dev.is_null() {
            self.usb_fd = None;
            // (libusb's codes: what the admin can do about them)
            let why = match r {
                -3 if cfg!(windows) => {
                    ": no access, another program uses the stick (a second MagicDAB, an SDR program) or it has no WinUSB driver (Zadig)"
                }
                -3 => ": no access, another program uses the stick or the user may not open it (udev rule, plugdev group)",
                -6 => ": busy, another program uses the stick",
                -1 => ": no stick found (plugged in?)",
                -4 | -5 => ": the stick is gone (unplugged?)",
                -12 => ": no driver for the stick (Windows: WinUSB with Zadig)",
                _ => "",
            };
            let e = format!("cannot open RTL-SDR device #{} (error {r}){why}", self.index);
            self.error = Some(e.clone());
            return Err(e);
        }
        self.name = list_devices().get(self.index as usize).cloned().unwrap_or_else(|| "RTL-SDR".into());
        self.warning = v4_wrong_crystal(api, dev).map(|xtal| {
            log::warn!(
                "RTL-SDR Blog V4 with a librtlsdr that does not know it ({}: tuner crystal {} MHz instead of 28.8): it tunes 1.8 times too high \
                 and hears nothing; it needs librtlsdr 2.0.1 or newer, put next to the program or from the MagicDAB packages",
                api.name,
                xtal as f64 / 1e6
            );
            "v4_library".to_string()
        });
        let gains = unsafe {
            let n = (api.get_tuner_gains)(dev, std::ptr::null_mut());
            let mut v = vec![0 as c_int; n.max(0) as usize];
            if n > 0 {
                (api.get_tuner_gains)(dev, v.as_mut_ptr());
            }
            v
        };
        unsafe {
            (api.set_sample_rate)(dev, SAMPLE_RATE);
            if self.ppm != 0 {
                (api.set_freq_correction)(dev, self.ppm);
            }
            if let Some(bt) = api.set_bias_tee {
                bt(dev, self.bias_tee as c_int);
            }
            (api.set_center_freq)(dev, frequency);
            (api.reset_buffer)(dev);
        }
        let start_index = default_gain_index(&gains);
        let ceiling = self.ceiling.unwrap_or(start_index);
        let shared = Arc::new(Shared {
            dev: DevPtr(dev),
            ctrl: Mutex::new(()),
            callback,
            agc: Mutex::new(SoftAgc::new()),
            gains,
            gain_index: AtomicUsize::new(start_index.min(ceiling)),
            gain_ceiling: AtomicUsize::new(ceiling),
            gain_request: AtomicUsize::new(usize::MAX),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(true),
            software_agc: AtomicBool::new(true),
            samples: Mutex::new(Vec::new()),
            raw: Mutex::new(Vec::new()),
            mean_power_bits: AtomicU32::new(0),
        });
        self.shared = Some(shared.clone());
        self.frequency = frequency;
        self.apply_gain();
        {
            let sh = shared.clone();
            self.gain_thread = std::thread::Builder::new().name("rtlsdr-gain".into()).spawn(move || gain_thread(sh)).ok();
        }
        let ctx = Arc::into_raw(shared) as usize;
        self.thread = Some(
            std::thread::Builder::new()
                .name("rtlsdr".into())
                .spawn(move || unsafe {
                    let sh = Arc::from_raw(ctx as *const Shared);
                    let r = (api.read_async)(sh.dev.0, read_cb, ctx as *mut c_void, 15, 65536);
                    sh.running.store(false, Ordering::Relaxed);
                    if !sh.stop.load(Ordering::Relaxed) {
                        log::warn!("RTL-SDR: the USB stream ended (rtlsdr_read_async returned {r}), stick unplugged or reset?");
                    }
                    drop(sh);
                })
                .map_err(|e| e.to_string())?,
        );
        self.error = None;
        Ok(())
    }

    fn tune(&mut self, frequency: u32) -> Result<(), String> {
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return Err("device not open".into()) };
        let r = {
            let _g = sh.ctrl.lock().unwrap();
            unsafe { (api.set_center_freq)(sh.dev.0, frequency) }
        };
        if r < 0 {
            return Err(format!("set_center_freq failed ({r})"));
        }
        self.frequency = frequency;
        Ok(())
    }

    fn stop(&mut self) {
        let Some(sh) = self.shared.take() else { return };
        sh.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.gain_thread.take() {
            let _ = t.join();
        }
        if let Ok(api) = api() {
            unsafe { (api.cancel_async)(sh.dev.0) };
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            unsafe { (api.close)(sh.dev.0) };
        }
        self.usb_fd = None;
    }

    fn set_gain_mode(&mut self, mode: GainMode) {
        self.mode = mode;
        self.apply_gain();
    }

    fn set_gain_ceiling(&mut self, index: Option<usize>, jump: bool) {
        self.ceiling = index;
        let (Some(sh), Ok(api)) = (&self.shared, api()) else { return };
        let c = index.unwrap_or_else(|| default_gain_index(&sh.gains)).min(sh.gains.len().saturating_sub(1));
        sh.gain_ceiling.store(c, Ordering::Relaxed);
        let i = sh.gain_index.load(Ordering::Relaxed);
        // going straight up to a ceiling the signal level does not allow would
        // overload the tuner until the AGC walks the gain back down
        if sh.software_agc.load(Ordering::Relaxed) && (i > c || (jump && i != c)) {
            sh.gain_request.store(usize::MAX, Ordering::Relaxed);
            sh.set_gain_index(api, c);
        }
    }

    fn set_ppm(&mut self, ppm: i32) {
        self.ppm = ppm;
        if let (Some(sh), Ok(api)) = (&self.shared, api()) {
            let _g = sh.ctrl.lock().unwrap();
            unsafe { (api.set_freq_correction)(sh.dev.0, ppm) };
        }
    }
}

impl Drop for RtlSdr {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::v4_unknown_to_library;

    #[test]
    fn a_v4_tuned_with_a_16_mhz_crystal_is_reported() {
        assert!(v4_unknown_to_library("RTLSDRBlog", "Blog V4", 16_000_000));
        // (with the frequency correction applied)
        assert!(v4_unknown_to_library("RTLSDRBlog", "Blog V4", 16_000_480));
        assert!(!v4_unknown_to_library("RTLSDRBlog", "Blog V4", 28_800_000));
        // other R828D sticks have 16 MHz, the V4 Lite is another product
        assert!(!v4_unknown_to_library("Realtek", "RTL2838UHIDIR", 16_000_000));
        assert!(!v4_unknown_to_library("RTLSDRBlog", "Blog V4L", 16_000_000));
    }
}
