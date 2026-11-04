//! MPEG-1/2 Layer II decoding via minimp3.

use std::ffi::{c_int, c_void};

extern "C" {
    fn magicdab_mp2_new() -> *mut c_void;
    fn magicdab_mp2_free(d: *mut c_void);
    fn magicdab_mp2_decode(
        d: *mut c_void,
        data: *const u8,
        len: c_int,
        pcm: *mut f32,
        channels: *mut c_int,
        hz: *mut c_int,
        frame_bytes: *mut c_int,
        layer: *mut c_int,
    ) -> c_int;
}

pub struct Mp2Decoder {
    handle: *mut c_void,
    pcm: Vec<f32>,
}

unsafe impl Send for Mp2Decoder {}

#[derive(Debug, Clone, Copy)]
pub struct Mp2FrameInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub samples: usize,
}

impl Default for Mp2Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Mp2Decoder {
    pub fn new() -> Self {
        Mp2Decoder { handle: unsafe { magicdab_mp2_new() }, pcm: vec![0.0; 1152 * 2] }
    }

    /// Decode a complete MP2 frame, appending interleaved samples to `out`.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<f32>) -> Option<Mp2FrameInfo> {
        let (mut ch, mut hz, mut fb, mut layer) = (0, 0, 0, 0);
        let n = unsafe {
            magicdab_mp2_decode(
                self.handle,
                frame.as_ptr(),
                frame.len() as c_int,
                self.pcm.as_mut_ptr(),
                &mut ch,
                &mut hz,
                &mut fb,
                &mut layer,
            )
        };
        if n <= 0 || ch <= 0 {
            return None;
        }
        let total = n as usize * ch as usize;
        out.extend_from_slice(&self.pcm[..total]);
        Some(Mp2FrameInfo { sample_rate: hz as u32, channels: ch as u8, samples: n as usize })
    }
}

impl Drop for Mp2Decoder {
    fn drop(&mut self) {
        unsafe { magicdab_mp2_free(self.handle) }
    }
}
