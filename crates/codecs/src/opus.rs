//! Opus encoder (48 kHz stereo, 20 ms frames) for streaming to browsers.

use std::ffi::{c_int, c_void};

extern "C" {
    fn opus_encoder_create(fs: i32, channels: c_int, application: c_int, error: *mut c_int) -> *mut c_void;
    fn opus_encoder_destroy(st: *mut c_void);
    fn opus_encoder_ctl(st: *mut c_void, request: c_int, ...) -> c_int;
    fn opus_encode_float(st: *mut c_void, pcm: *const f32, frame_size: c_int, data: *mut u8, max_bytes: i32) -> i32;
}

const OPUS_APPLICATION_AUDIO: c_int = 2049;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
const OPUS_SET_SIGNAL_REQUEST: c_int = 4024;
const OPUS_SIGNAL_MUSIC: c_int = 3002;

/// Samples per channel of one 20 ms frame at 48 kHz.
pub const FRAME: usize = 960;

pub struct OpusEncoder {
    st: *mut c_void,
    out: Vec<u8>,
}

unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(bitrate: i32) -> Result<Self, String> {
        let mut err: c_int = 0;
        let st = unsafe { opus_encoder_create(48_000, 2, OPUS_APPLICATION_AUDIO, &mut err) };
        if st.is_null() || err != 0 {
            return Err(format!("opus_encoder_create failed ({err})"));
        }
        unsafe {
            opus_encoder_ctl(st, OPUS_SET_BITRATE_REQUEST, bitrate as c_int);
            opus_encoder_ctl(st, OPUS_SET_COMPLEXITY_REQUEST, 6 as c_int);
            opus_encoder_ctl(st, OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_MUSIC);
        }
        Ok(OpusEncoder { st, out: vec![0; 4000] })
    }

    /// Encode one frame of 960 interleaved stereo samples.
    pub fn encode(&mut self, pcm: &[f32]) -> Option<&[u8]> {
        debug_assert_eq!(pcm.len(), FRAME * 2);
        let n = unsafe { opus_encode_float(self.st, pcm.as_ptr(), FRAME as c_int, self.out.as_mut_ptr(), self.out.len() as i32) };
        if n <= 0 {
            return None;
        }
        Some(&self.out[..n as usize])
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.st) }
    }
}
