//! Run the OFDM demodulator over a raw IQ recording as fast as it goes and
//! print a checksum of all soft bits and the time taken: a changed
//! demodulator must give the same checksum (or explain why not), and the
//! time shows what an optimisation brought.
//!
//! usage: ofdmcheck <file.uff|.raw> [seconds]

use magicdab_dab::ofdm::{Frame, FrameSink, OfdmDemodulator, SampleSource};
use magicdab_dab::Complex32;
use std::io::{Read, Seek};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

struct File {
    f: std::fs::File,
    s16: bool,
    left: u64,
    buf: Vec<u8>,
}

impl SampleSource for File {
    fn read(&mut self, out: &mut [Complex32]) -> bool {
        if self.left < out.len() as u64 {
            return false;
        }
        let width = if self.s16 { 4 } else { 2 };
        self.buf.resize(out.len() * width, 0);
        if self.f.read_exact(&mut self.buf).is_err() {
            return false;
        }
        for (o, c) in out.iter_mut().zip(self.buf.chunks_exact(width)) {
            *o = if self.s16 {
                Complex32::new(i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0, i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0)
            } else {
                Complex32::new((c[0] as f32 - 127.4) / 128.0, (c[1] as f32 - 127.4) / 128.0)
            };
        }
        self.left -= out.len() as u64;
        true
    }
}

#[derive(Default)]
struct Check {
    frames: u64,
    hash: u64,
    synced: u32,
}

impl Check {
    fn mix(&mut self, bits: &[i8]) {
        // FNV-1a
        for &b in bits {
            self.hash ^= b as u8 as u64;
            self.hash = self.hash.wrapping_mul(0x100000001b3);
        }
    }
}

impl FrameSink for Check {
    fn frame(&mut self, frame: &Frame) {
        self.frames += 1;
        self.mix(&frame.fic);
        self.mix(&frame.msc);
    }
    fn sync_state(&mut self, synced: bool, _signal: bool) {
        if synced {
            self.synced += 1;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: ofdmcheck <file> [seconds]");
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let mut f = std::fs::File::open(path).expect("open");
    let mut head = vec![0u8; 2048];
    f.read_exact(&mut head).expect("header");
    let (offset, s16) = if head.starts_with(b"<?xml") {
        let txt = String::from_utf8_lossy(&head);
        let off = txt.split("Offset=\"").nth(1).and_then(|s| s.split('"').next()).and_then(|s| s.parse().ok()).unwrap_or(2048);
        (off, txt.contains("Bits=\"16\""))
    } else {
        (0, false)
    };
    f.seek(std::io::SeekFrom::Start(offset)).expect("seek");
    let mut src = File { f, s16, left: (seconds * 2_048_000.0) as u64, buf: Vec::new() };
    let mut sink = Check { hash: 0xcbf29ce484222325, ..Default::default() };
    let mut demod = OfdmDemodulator::new();
    let t = Instant::now();
    demod.run(&mut src, &mut sink, &AtomicBool::new(false));
    let took = t.elapsed().as_secs_f64();
    println!(
        "{} frames, synced {} times, soft bits {:016x}, {:.2} s for {:.0} s of signal ({:.1}% of real time)",
        sink.frames,
        sink.synced,
        sink.hash,
        took,
        seconds,
        100.0 * took / seconds
    );
}
