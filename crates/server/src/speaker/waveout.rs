//! Windows: waveOut on the default device (part of every Windows; it follows
//! the default output when another one is chosen).

use super::Output;
use std::ffi::c_void;

type Handle = *mut c_void;
/// A kernel object (as sysstats.rs declares the same functions).
type Event = isize;

#[repr(C, packed(1))]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
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

const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_EVENT: u32 = 0x0005_0000;
const WHDR_DONE: u32 = 0x0000_0001;
const WAIT_TIMEOUT: u32 = 0x0000_0102;

#[link(name = "winmm")]
extern "system" {
    fn waveOutOpen(out: *mut Handle, device: u32, format: *const WaveFormatEx, callback: usize, instance: usize, flags: u32) -> u32;
    fn waveOutPrepareHeader(out: Handle, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveOutUnprepareHeader(out: Handle, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveOutWrite(out: Handle, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveOutReset(out: Handle) -> u32;
    fn waveOutClose(out: Handle) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(attributes: *mut c_void, manual_reset: i32, initial: i32, name: *const u16) -> Event;
    fn WaitForSingleObject(handle: Event, ms: u32) -> u32;
    fn CloseHandle(handle: Event) -> i32;
}

/// Blocks queued at the device (of `super::BLOCK` frames, 20 ms each).
const BLOCKS: usize = 5;

struct WaveOut {
    out: Handle,
    event: Event,
    /// (boxed: the device keeps their addresses from the first push on)
    #[allow(clippy::vec_box)]
    headers: Vec<Box<WaveHdr>>,
    buffers: Vec<Vec<i16>>,
    /// Queued at the device (not done yet).
    queued: Vec<bool>,
}

// (the device is used by the speaker's thread only)
unsafe impl Send for WaveOut {}

const HDR: u32 = std::mem::size_of::<WaveHdr>() as u32;

pub fn open() -> Result<Box<dyn Output>, String> {
    let format = WaveFormatEx {
        format_tag: WAVE_FORMAT_PCM,
        channels: 2,
        samples_per_sec: super::RATE as u32,
        avg_bytes_per_sec: super::RATE as u32 * 4,
        block_align: 4,
        bits_per_sample: 16,
        cb_size: 0,
    };
    unsafe {
        let event = CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null());
        if event == 0 {
            return Err("no event for waveOut".into());
        }
        let mut out: Handle = std::ptr::null_mut();
        let r = waveOutOpen(&mut out, WAVE_MAPPER, &format, event as usize, 0, CALLBACK_EVENT);
        if r != 0 {
            CloseHandle(event);
            return Err(format!("waveOut cannot open the output (error {r})"));
        }
        let mut w = WaveOut { out, event, headers: Vec::new(), buffers: Vec::new(), queued: vec![false; BLOCKS] };
        for _ in 0..BLOCKS {
            let mut buf = vec![0i16; super::BLOCK * 2];
            let mut h = Box::new(WaveHdr {
                data: buf.as_mut_ptr() as *mut u8,
                buffer_length: (buf.len() * 2) as u32,
                bytes_recorded: 0,
                user: 0,
                flags: 0,
                loops: 0,
                next: std::ptr::null_mut(),
                reserved: 0,
            });
            let r = waveOutPrepareHeader(out, &mut *h, HDR);
            if r != 0 {
                return Err(format!("waveOut cannot prepare a block (error {r})"));
            }
            w.headers.push(h);
            w.buffers.push(buf);
        }
        Ok(Box::new(w))
    }
}

impl Output for WaveOut {
    fn buffered(&self) -> usize {
        BLOCKS * super::BLOCK
    }

    fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        for chunk in pcm.chunks(super::BLOCK * 2) {
            // a free block: one never queued or one the device is done with
            let i = loop {
                // (the device sets the flag from a thread of its own)
                let done = |h: &WaveHdr| unsafe { std::ptr::read_volatile(&raw const h.flags) } & WHDR_DONE != 0;
                let free = (0..BLOCKS).find(|&i| !self.queued[i] || done(&self.headers[i]));
                if let Some(i) = free {
                    break i;
                }
                if unsafe { WaitForSingleObject(self.event, 1000) } == WAIT_TIMEOUT {
                    return Err("waveOut plays nothing".into());
                }
            };
            let buf = &mut self.buffers[i];
            buf[..chunk.len()].copy_from_slice(chunk);
            let h = &mut self.headers[i];
            h.buffer_length = (chunk.len() * 2) as u32;
            h.flags &= !WHDR_DONE;
            let r = unsafe { waveOutWrite(self.out, &mut **h, HDR) };
            if r != 0 {
                return Err(format!("waveOut error {r}"));
            }
            self.queued[i] = true;
        }
        Ok(())
    }
}

impl Drop for WaveOut {
    fn drop(&mut self) {
        unsafe {
            waveOutReset(self.out);
            for h in &mut self.headers {
                waveOutUnprepareHeader(self.out, &mut **h, HDR);
            }
            waveOutClose(self.out);
            CloseHandle(self.event);
        }
    }
}
