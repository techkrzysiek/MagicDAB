//! Linux: the default ALSA device (libasound.so.2, loaded at run time: the
//! program runs without it, on a server with no sound). On a desktop the
//! default device leads to PipeWire or PulseAudio.

use super::Output;
use libloading::Library;
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr};
use std::sync::OnceLock;

type Pcm = *mut c_void;

struct Api {
    _lib: Library,
    open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
    set_params: unsafe extern "C" fn(Pcm, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
    writei: unsafe extern "C" fn(Pcm, *const c_void, c_ulong) -> c_long,
    recover: unsafe extern "C" fn(Pcm, c_int, c_int) -> c_int,
    drop: unsafe extern "C" fn(Pcm) -> c_int,
    close: unsafe extern "C" fn(Pcm) -> c_int,
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

const STREAM_PLAYBACK: c_int = 0;
const FORMAT_S16_LE: c_int = 2;
const ACCESS_RW_INTERLEAVED: c_int = 3;
/// The device's own buffer (µs).
const LATENCY_US: c_uint = 100_000;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| load().map_err(|e| log::debug!("sound output: {e}")).ok()).as_ref()
}

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
            writei: sym!(b"snd_pcm_writei\0"),
            recover: sym!(b"snd_pcm_recover\0"),
            drop: sym!(b"snd_pcm_drop\0"),
            close: sym!(b"snd_pcm_close\0"),
            strerror: sym!(b"snd_strerror\0"),
            _lib: lib,
        })
    }
}

impl Api {
    fn error(&self, e: c_int) -> String {
        unsafe { CStr::from_ptr((self.strerror)(e)) }.to_string_lossy().into_owned()
    }
}

/// Only on a computer with a desktop session: a screen, or the sound server
/// of a logged-in user (PipeWire, PulseAudio). A receiver without one (a
/// Raspberry Pi as a server, a system service) plays nothing itself, also for
/// a page that reaches it as "this computer" through an SSH tunnel.
pub fn available() -> bool {
    desktop() && api().is_some()
}

fn desktop() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if set("DISPLAY") || set("WAYLAND_DISPLAY") || set("PULSE_SERVER") {
        return true;
    }
    // (a user's sound server without a screen variable: the program started by systemd --user)
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|d| d.join("pipewire-0").exists() || d.join("pulse/native").exists())
}

struct Alsa {
    api: &'static Api,
    pcm: Pcm,
}

// (the device is used by the speaker's thread only)
unsafe impl Send for Alsa {}

pub fn open() -> Result<Box<dyn Output>, String> {
    let api = api().ok_or("no ALSA library (libasound.so.2)")?;
    let mut pcm: Pcm = std::ptr::null_mut();
    let r = unsafe { (api.open)(&mut pcm, c"default".as_ptr(), STREAM_PLAYBACK, 0) };
    if r < 0 {
        return Err(format!("ALSA default device: {}", api.error(r)));
    }
    let r = unsafe { (api.set_params)(pcm, FORMAT_S16_LE, ACCESS_RW_INTERLEAVED, 2, super::RATE as c_uint, 1, LATENCY_US) };
    if r < 0 {
        unsafe { (api.close)(pcm) };
        return Err(format!("ALSA default device: {}", api.error(r)));
    }
    Ok(Box::new(Alsa { api, pcm }))
}

impl Output for Alsa {
    fn buffered(&self) -> usize {
        super::RATE * LATENCY_US as usize / 1_000_000
    }

    fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        let mut at = 0;
        while at < pcm.len() {
            let frames = ((pcm.len() - at) / 2) as c_ulong;
            let n = unsafe { (self.api.writei)(self.pcm, pcm[at..].as_ptr() as *const c_void, frames) };
            if n < 0 {
                // an underrun (or a suspended device): prepared again and on
                if unsafe { (self.api.recover)(self.pcm, n as c_int, 1) } < 0 {
                    return Err(self.api.error(n as c_int));
                }
                continue;
            }
            at += n as usize * 2;
        }
        Ok(())
    }
}

impl Drop for Alsa {
    fn drop(&mut self) {
        unsafe {
            (self.api.drop)(self.pcm);
            (self.api.close)(self.pcm);
        }
    }
}
