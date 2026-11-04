//! Audio of the tuner (a USB audio device): ALSA capture with libasound loaded
//! at run time, 48 kHz interleaved stereo.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
#[cfg(any(target_os = "linux", windows))]
use std::time::Duration;

/// Name of the TEF668x headless tuner's audio (ALSA; Windows: part of the device name).
#[cfg(not(windows))]
pub const DEFAULT_DEVICE: &str = "plughw:CARD=Tuner,DEV=0";
#[cfg(windows)]
pub const DEFAULT_DEVICE: &str = "TEF668X";
pub const RATE: u32 = 48_000;

/// Capture devices: (device name, description).
#[cfg(windows)]
pub fn list_devices() -> Vec<(String, String)> {
    backend::devices().into_iter().map(|n| (n.clone(), n)).collect()
}

/// ALSA capture devices: (device name, description), from /proc/asound.
#[cfg(not(windows))]
pub fn list_devices() -> Vec<(String, String)> {
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    let pcm = std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default();
    // " 1 [Tuner          ]: USB-Audio - TEF668X Headless USB Tuner"
    let mut names = std::collections::HashMap::new();
    for l in cards.lines() {
        let Some((head, rest)) = l.split_once("]:") else { continue };
        let Some((num, id)) = head.trim().split_once('[') else { continue };
        let desc = rest.split_once(" - ").map_or(rest, |x| x.1).trim().to_string();
        if let Ok(n) = num.trim().parse::<u32>() {
            names.insert(n, (id.trim().to_string(), desc));
        }
    }
    // "01-00: USB Audio : USB Audio : capture 1"
    let mut out = Vec::new();
    for l in pcm.lines().filter(|l| l.contains("capture")) {
        let Some((cd, _)) = l.split_once(':') else { continue };
        let Some((c, d)) = cd.split_once('-') else { continue };
        let (Ok(c), Ok(d)) = (c.parse::<u32>(), d.parse::<u32>()) else { continue };
        if let Some((id, desc)) = names.get(&c) {
            out.push((format!("plughw:CARD={id},DEV={d}"), desc.clone()));
        }
    }
    if out.is_empty() {
        // containers hide /proc/asound: capture devices from /dev/snd, names from sysfs
        let mut nodes: Vec<(u32, u32)> = std::fs::read_dir("/dev/snd")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                let (c, d) = n.strip_prefix("pcmC")?.strip_suffix('c')?.split_once('D')?;
                Some((c.parse().ok()?, d.parse().ok()?))
            })
            .collect();
        nodes.sort();
        for (c, d) in nodes {
            let sys = format!("/sys/class/sound/card{c}");
            let read = |p: &str| std::fs::read_to_string(format!("{sys}/{p}")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            let Some(id) = read("id") else { continue };
            let desc = read("device/../product").unwrap_or_else(|| id.clone());
            out.push((format!("plughw:CARD={id},DEV={d}"), desc));
        }
    }
    out
}

pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Capture from `device` ("" = the tuner) and hand interleaved stereo
    /// samples to `sink` in blocks of 10 ms; reopens the device after errors.
    pub fn start(device: String, sink: impl FnMut(&[f32]) + Send + 'static) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let device = if device.is_empty() { DEFAULT_DEVICE.to_string() } else { device };
        let thread =
            std::thread::Builder::new().name("fm-audio".into()).spawn(move || backend::run(&device, &st, sink)).expect("fm audio thread");
        Capture { stop, thread: Some(thread) }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // the thread ends by itself (it sleeps up to 2 s between attempts);
        // waiting for it would hold up the radio thread
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take();
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;
    use libloading::Library;
    use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};

    type Pcm = *mut c_void;

    struct Api {
        _lib: Library,
        open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
        set_params: unsafe extern "C" fn(Pcm, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
        readi: unsafe extern "C" fn(Pcm, *mut c_void, c_ulong) -> c_long,
        recover: unsafe extern "C" fn(Pcm, c_int, c_int) -> c_int,
        start: unsafe extern "C" fn(Pcm) -> c_int,
        close: unsafe extern "C" fn(Pcm) -> c_int,
        strerror: unsafe extern "C" fn(c_int) -> *const c_char,
    }

    const STREAM_CAPTURE: c_int = 1;
    const NONBLOCK: c_int = 1;
    const EAGAIN: c_long = 11;
    const FORMAT_S16_LE: c_int = 2;
    const ACCESS_RW_INTERLEAVED: c_int = 3;

    fn load() -> Result<Api, String> {
        let lib = unsafe { Library::new("libasound.so.2") }.map_err(|e| format!("libasound: {e}"))?;
        unsafe {
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get($n).map_err(|e| format!("libasound: {e}"))?
                };
            }
            Ok(Api {
                open: sym!(b"snd_pcm_open\0"),
                set_params: sym!(b"snd_pcm_set_params\0"),
                readi: sym!(b"snd_pcm_readi\0"),
                recover: sym!(b"snd_pcm_recover\0"),
                start: sym!(b"snd_pcm_start\0"),
                close: sym!(b"snd_pcm_close\0"),
                strerror: sym!(b"snd_strerror\0"),
                _lib: lib,
            })
        }
    }

    impl Api {
        fn error(&self, e: c_int) -> String {
            unsafe { CStr::from_ptr((self.strerror)(e)) }.to_string_lossy().to_string()
        }
    }

    pub fn run(device: &str, stop: &AtomicBool, mut sink: impl FnMut(&[f32])) {
        let api = match load() {
            Ok(a) => a,
            Err(e) => {
                log::warn!("FM audio: {e}");
                return;
            }
        };
        let name = CString::new(device).unwrap_or_default();
        const FRAMES: usize = RATE as usize / 100;
        let mut raw = vec![0i16; FRAMES * 2];
        let mut out = vec![0f32; FRAMES * 2];
        let mut warned = false;
        while !stop.load(Ordering::Relaxed) {
            let mut pcm: Pcm = std::ptr::null_mut();
            // non-blocking: the tuner sends nothing while its serial port is closed
            let r = unsafe { (api.open)(&mut pcm, name.as_ptr(), STREAM_CAPTURE, NONBLOCK) };
            let r = if r == 0 { unsafe { (api.set_params)(pcm, FORMAT_S16_LE, ACCESS_RW_INTERLEAVED, 2, RATE, 1, 100_000) } } else { r };
            // USB audio of the tuner: the first read does not start the stream (EIO)
            let r = if r == 0 { unsafe { (api.start)(pcm) } } else { r };
            if r < 0 {
                if !pcm.is_null() {
                    unsafe { (api.close)(pcm) };
                }
                if !warned {
                    log::debug!("FM audio {device}: {}", api.error(r));
                    warned = true;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            warned = false;
            log::info!("FM audio from {device}");
            let mut last_data = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let n = unsafe { (api.readi)(pcm, raw.as_mut_ptr() as *mut c_void, FRAMES as c_ulong) };
                if n == -EAGAIN {
                    if last_data.elapsed() > Duration::from_secs(2) {
                        log::debug!("FM audio {device}: no data");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                if n < 0 {
                    if unsafe { (api.recover)(pcm, n as c_int, 1) } < 0 || unsafe { (api.start)(pcm) } < 0 {
                        log::warn!("FM audio {device}: {}", api.error(n as c_int));
                        break;
                    }
                    continue;
                }
                last_data = std::time::Instant::now();
                let n = n as usize * 2;
                for (o, &s) in out[..n].iter_mut().zip(&raw[..n]) {
                    *o = s as f32 / 32768.0;
                }
                sink(&out[..n]);
            }
            unsafe { (api.close)(pcm) };
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Windows: waveIn (winmm), buffers polled.
#[cfg(windows)]
mod backend {
    use super::*;
    use std::ffi::c_void;

    #[repr(C, packed)]
    struct WaveFormatEx {
        tag: u16,
        channels: u16,
        rate: u32,
        bytes_per_sec: u32,
        block_align: u16,
        bits: u16,
        cb_size: u16,
    }

    #[repr(C)]
    struct WaveHdr {
        data: *mut u8,
        buffer_length: u32,
        bytes_recorded: u32,
        user: usize,
        flags: u32,
        loops: u32,
        next: *mut WaveHdr,
        reserved: usize,
    }

    #[repr(C)]
    struct WaveInCaps {
        mid: u16,
        pid: u16,
        driver_version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
    }

    type HWaveIn = *mut c_void;

    #[link(name = "winmm")]
    extern "system" {
        fn waveInGetNumDevs() -> u32;
        fn waveInGetDevCapsW(id: usize, caps: *mut WaveInCaps, size: u32) -> u32;
        fn waveInOpen(h: *mut HWaveIn, id: u32, fmt: *const WaveFormatEx, cb: usize, inst: usize, flags: u32) -> u32;
        fn waveInPrepareHeader(h: HWaveIn, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveInUnprepareHeader(h: HWaveIn, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveInAddBuffer(h: HWaveIn, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveInStart(h: HWaveIn) -> u32;
        fn waveInReset(h: HWaveIn) -> u32;
        fn waveInClose(h: HWaveIn) -> u32;
    }

    const WHDR_DONE: u32 = 1;
    const BUFFERS: usize = 8;

    pub fn devices() -> Vec<String> {
        (0..unsafe { waveInGetNumDevs() })
            .filter_map(|i| {
                let mut c: WaveInCaps = unsafe { std::mem::zeroed() };
                if unsafe { waveInGetDevCapsW(i as usize, &mut c, std::mem::size_of::<WaveInCaps>() as u32) } != 0 {
                    return None;
                }
                let len = c.name.iter().position(|&x| x == 0).unwrap_or(c.name.len());
                Some(String::from_utf16_lossy(&c.name[..len]))
            })
            .collect()
    }

    pub fn run(device: &str, stop: &AtomicBool, mut sink: impl FnMut(&[f32])) {
        const FRAMES: usize = RATE as usize / 100;
        let mut warned = false;
        while !stop.load(Ordering::Relaxed) {
            let names = devices();
            let want = device.to_lowercase();
            let id = names.iter().position(|n| n.to_lowercase().contains(&want)).or_else(|| names.iter().position(|n| n.contains("Tuner")));
            let Some(id) = id else {
                if !warned {
                    log::debug!("FM audio: no capture device like {device} ({})", names.join(", "));
                    warned = true;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            warned = false;
            let fmt = WaveFormatEx { tag: 1, channels: 2, rate: RATE, bytes_per_sec: RATE * 4, block_align: 4, bits: 16, cb_size: 0 };
            let mut h: HWaveIn = std::ptr::null_mut();
            if unsafe { waveInOpen(&mut h, id as u32, &fmt, 0, 0, 0) } != 0 {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            log::info!("FM audio from {}", names[id]);
            let mut data = vec![vec![0u8; FRAMES * 4]; BUFFERS];
            let mut hdrs: Vec<WaveHdr> = data
                .iter_mut()
                .map(|d| WaveHdr {
                    data: d.as_mut_ptr(),
                    buffer_length: d.len() as u32,
                    bytes_recorded: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: std::ptr::null_mut(),
                    reserved: 0,
                })
                .collect();
            let size = std::mem::size_of::<WaveHdr>() as u32;
            for hd in hdrs.iter_mut() {
                unsafe {
                    waveInPrepareHeader(h, hd, size);
                    waveInAddBuffer(h, hd, size);
                }
            }
            unsafe { waveInStart(h) };
            let mut out = vec![0f32; FRAMES * 2];
            let mut next = 0;
            let mut last_data = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let hd = &mut hdrs[next];
                if unsafe { std::ptr::read_volatile(&hd.flags) } & WHDR_DONE == 0 {
                    // the tuner sends nothing while its serial port is closed
                    if last_data.elapsed() > Duration::from_secs(2) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                last_data = std::time::Instant::now();
                let n = (hd.bytes_recorded as usize / 2).min(out.len());
                let raw = unsafe { std::slice::from_raw_parts(hd.data as *const i16, n) };
                for (o, &s) in out[..n].iter_mut().zip(raw) {
                    *o = s as f32 / 32768.0;
                }
                sink(&out[..n]);
                hd.flags &= !WHDR_DONE;
                unsafe { waveInAddBuffer(h, hd, size) };
                next = (next + 1) % BUFFERS;
            }
            unsafe {
                waveInReset(h);
                for hd in hdrs.iter_mut() {
                    waveInUnprepareHeader(h, hd, size);
                }
                waveInClose(h);
            }
            drop(data);
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod backend {
    use super::*;

    pub fn run(device: &str, _stop: &AtomicBool, _sink: impl FnMut(&[f32])) {
        log::warn!("FM audio {device}: capturing is implemented for Linux (ALSA) and Windows");
    }
}
