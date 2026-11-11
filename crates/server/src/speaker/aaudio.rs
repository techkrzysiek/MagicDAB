//! Android: AAudio (libaaudio.so, Android 8 and newer), loaded at run time.
//! The stream asks for power saving: the system plays it from its deep
//! buffer, so the processor sleeps between the blocks.

use super::Output;
use libloading::Library;
use std::ffi::{c_char, c_void, CStr};
use std::sync::OnceLock;

type Builder = *mut c_void;
type Stream = *mut c_void;

struct Api {
    _lib: Library,
    create_builder: unsafe extern "C" fn(*mut Builder) -> i32,
    set_format: unsafe extern "C" fn(Builder, i32),
    set_channels: unsafe extern "C" fn(Builder, i32),
    set_rate: unsafe extern "C" fn(Builder, i32),
    set_performance: unsafe extern "C" fn(Builder, i32),
    /// (Android 9 and newer)
    set_usage: Option<unsafe extern "C" fn(Builder, i32)>,
    set_content: Option<unsafe extern "C" fn(Builder, i32)>,
    open: unsafe extern "C" fn(Builder, *mut Stream) -> i32,
    delete_builder: unsafe extern "C" fn(Builder) -> i32,
    start: unsafe extern "C" fn(Stream) -> i32,
    write: unsafe extern "C" fn(Stream, *const c_void, i32, i64) -> i32,
    buffer_size: unsafe extern "C" fn(Stream) -> i32,
    close: unsafe extern "C" fn(Stream) -> i32,
    text: unsafe extern "C" fn(i32) -> *const c_char,
}

const FORMAT_PCM_I16: i32 = 1;
const PERFORMANCE_MODE_POWER_SAVING: i32 = 11;
const USAGE_MEDIA: i32 = 1;
const CONTENT_TYPE_MUSIC: i32 = 2;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| match load() {
        Ok(a) => Some(a),
        Err(e) => {
            log::warn!("sound output: {e}");
            None
        }
    })
    .as_ref()
}

fn load() -> Result<Api, String> {
    let lib = unsafe { Library::new("libaaudio.so") }.map_err(|e| format!("libaaudio: {e}"))?;
    unsafe {
        macro_rules! sym {
            ($n:literal) => {
                *lib.get($n).map_err(|e| format!("libaaudio: {e}"))?
            };
        }
        macro_rules! opt {
            ($n:literal) => {
                lib.get($n).ok().map(|s| *s)
            };
        }
        Ok(Api {
            create_builder: sym!(b"AAudio_createStreamBuilder\0"),
            set_format: sym!(b"AAudioStreamBuilder_setFormat\0"),
            set_channels: sym!(b"AAudioStreamBuilder_setChannelCount\0"),
            set_rate: sym!(b"AAudioStreamBuilder_setSampleRate\0"),
            set_performance: sym!(b"AAudioStreamBuilder_setPerformanceMode\0"),
            set_usage: opt!(b"AAudioStreamBuilder_setUsage\0"),
            set_content: opt!(b"AAudioStreamBuilder_setContentType\0"),
            open: sym!(b"AAudioStreamBuilder_openStream\0"),
            delete_builder: sym!(b"AAudioStreamBuilder_delete\0"),
            start: sym!(b"AAudioStream_requestStart\0"),
            write: sym!(b"AAudioStream_write\0"),
            buffer_size: sym!(b"AAudioStream_getBufferSizeInFrames\0"),
            close: sym!(b"AAudioStream_close\0"),
            text: sym!(b"AAudio_convertResultToText\0"),
            _lib: lib,
        })
    }
}

impl Api {
    fn error(&self, r: i32) -> String {
        let t = unsafe { (self.text)(r) };
        if t.is_null() {
            format!("AAudio error {r}")
        } else {
            unsafe { CStr::from_ptr(t) }.to_string_lossy().into_owned()
        }
    }
}

pub fn available() -> bool {
    api().is_some()
}

struct AAudio {
    api: &'static Api,
    stream: Stream,
}

// (the stream is used by the speaker's thread only)
unsafe impl Send for AAudio {}

pub fn open() -> Result<Box<dyn Output>, String> {
    let api = api().ok_or("AAudio is not available")?;
    unsafe {
        let mut b: Builder = std::ptr::null_mut();
        let r = (api.create_builder)(&mut b);
        if r < 0 {
            return Err(api.error(r));
        }
        (api.set_format)(b, FORMAT_PCM_I16);
        (api.set_channels)(b, 2);
        (api.set_rate)(b, super::RATE as i32);
        (api.set_performance)(b, PERFORMANCE_MODE_POWER_SAVING);
        if let Some(f) = api.set_usage {
            f(b, USAGE_MEDIA);
        }
        if let Some(f) = api.set_content {
            f(b, CONTENT_TYPE_MUSIC);
        }
        let mut s: Stream = std::ptr::null_mut();
        let r = (api.open)(b, &mut s);
        (api.delete_builder)(b);
        if r < 0 {
            return Err(api.error(r));
        }
        let r = (api.start)(s);
        if r < 0 {
            (api.close)(s);
            return Err(api.error(r));
        }
        Ok(Box::new(AAudio { api, stream: s }))
    }
}

impl Output for AAudio {
    fn buffered(&self) -> usize {
        unsafe { (self.api.buffer_size)(self.stream) }.max(0) as usize
    }

    fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        let mut at = 0;
        while at < pcm.len() {
            let frames = ((pcm.len() - at) / 2) as i32;
            let r = unsafe { (self.api.write)(self.stream, pcm[at..].as_ptr() as *const c_void, frames, 1_000_000_000) };
            if r < 0 {
                return Err(self.api.error(r));
            }
            if r == 0 {
                return Err("AAudio plays nothing".into());
            }
            at += r as usize * 2;
        }
        Ok(())
    }
}

impl Drop for AAudio {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.stream) };
    }
}
