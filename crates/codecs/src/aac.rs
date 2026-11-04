//! Minimal safe wrapper of libfaad2 for raw AAC access units.

use std::ffi::{c_char, c_ulong, c_void, CStr};

#[repr(C)]
struct NeAACDecConfiguration {
    def_object_type: u8,
    def_sample_rate: c_ulong,
    output_format: u8,
    down_matrix: u8,
    use_old_adts_format: u8,
    dont_upsample_implicit_sbr: u8,
}

#[repr(C)]
struct NeAACDecFrameInfo {
    bytesconsumed: c_ulong,
    samples: c_ulong,
    channels: u8,
    error: u8,
    samplerate: c_ulong,
    sbr: u8,
    object_type: u8,
    header_type: u8,
    num_front_channels: u8,
    num_side_channels: u8,
    num_back_channels: u8,
    num_lfe_channels: u8,
    channel_position: [u8; 64],
    ps: u8,
}

extern "C" {
    fn NeAACDecOpen() -> *mut c_void;
    fn NeAACDecGetCurrentConfiguration(h: *mut c_void) -> *mut NeAACDecConfiguration;
    fn NeAACDecSetConfiguration(h: *mut c_void, cfg: *mut NeAACDecConfiguration) -> u8;
    fn NeAACDecInit2(h: *mut c_void, buf: *mut u8, size: c_ulong, samplerate: *mut c_ulong, channels: *mut u8) -> c_char;
    fn NeAACDecDecode(h: *mut c_void, info: *mut NeAACDecFrameInfo, buf: *mut u8, size: c_ulong) -> *mut c_void;
    fn NeAACDecClose(h: *mut c_void);
    fn NeAACDecGetErrorMessage(code: u8) -> *const c_char;
}

const FAAD_FMT_FLOAT: u8 = 4;

pub struct AacDecoder {
    handle: *mut c_void,
    buf: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u8,
}

// The decoder handle is only used from one thread at a time.
unsafe impl Send for AacDecoder {}

#[derive(Debug, Clone, Copy)]
pub struct AacFrameInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub sbr: bool,
    pub ps: bool,
}

impl AacDecoder {
    /// Create a decoder for raw access units described by an AudioSpecificConfig.
    pub fn new(asc: &[u8]) -> Result<Self, String> {
        unsafe {
            let handle = NeAACDecOpen();
            if handle.is_null() {
                return Err("NeAACDecOpen failed".into());
            }
            let cfg = NeAACDecGetCurrentConfiguration(handle);
            (*cfg).output_format = FAAD_FMT_FLOAT;
            (*cfg).dont_upsample_implicit_sbr = 0;
            (*cfg).down_matrix = 0;
            NeAACDecSetConfiguration(handle, cfg);
            let mut asc_buf = asc.to_vec();
            let mut sr: c_ulong = 0;
            let mut ch: u8 = 0;
            // (a plain char in faad2: its -1 is 255 where char is unsigned, as on ARM)
            let r = NeAACDecInit2(handle, asc_buf.as_mut_ptr(), asc_buf.len() as c_ulong, &mut sr, &mut ch) as i8;
            if r < 0 {
                NeAACDecClose(handle);
                return Err(format!("NeAACDecInit2 failed ({r})"));
            }
            Ok(AacDecoder { handle, buf: Vec::new(), sample_rate: sr as u32, channels: ch })
        }
    }

    /// Decode one access unit into interleaved float samples (appended to `out`).
    pub fn decode(&mut self, au: &[u8], out: &mut Vec<f32>) -> Result<AacFrameInfo, String> {
        self.buf.clear();
        self.buf.extend_from_slice(au);
        unsafe {
            let mut info: NeAACDecFrameInfo = std::mem::zeroed();
            let pcm = NeAACDecDecode(self.handle, &mut info, self.buf.as_mut_ptr(), self.buf.len() as c_ulong);
            if info.error != 0 {
                let msg = CStr::from_ptr(NeAACDecGetErrorMessage(info.error)).to_string_lossy().into_owned();
                return Err(msg);
            }
            if pcm.is_null() || info.samples == 0 {
                return Ok(AacFrameInfo { sample_rate: info.samplerate as u32, channels: info.channels, sbr: false, ps: false });
            }
            let samples = std::slice::from_raw_parts(pcm as *const f32, info.samples as usize);
            out.extend_from_slice(samples);
            Ok(AacFrameInfo {
                sample_rate: info.samplerate as u32,
                channels: info.channels,
                sbr: info.sbr == 1 || info.sbr == 2,
                ps: info.ps != 0,
            })
        }
    }
}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        unsafe { NeAACDecClose(self.handle) }
    }
}
