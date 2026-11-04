//! Spectrum and null symbol visibility of a raw IQ recording (.uff, u8 I/Q):
//! the average power spectrum in 64 kHz bands, and per transmission frame the
//! lowest null-symbol-long power against the mean, over the whole band and
//! inside the DAB channel (±768 kHz). Development tool.
//!
//! usage: uffspec <file> [seconds]

use magicdab_dab::Complex32;
use rustfft::FftPlanner;
use std::io::{Read, Seek, SeekFrom};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: uffspec <file> [seconds]");
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let mut f = std::fs::File::open(path).expect("open file");
    let mut header = [0u8; 5];
    f.read_exact(&mut header).unwrap();
    f.seek(SeekFrom::Start(if &header == b"<?xml" { 2048 } else { 0 })).unwrap();
    let n = (seconds * 2_048_000.0) as usize;
    let mut raw = vec![0u8; n * 2];
    let got = f.read(&mut raw).unwrap_or(0) / 2;
    let x: Vec<Complex32> =
        raw[..got * 2].chunks_exact(2).map(|c| Complex32::new((c[0] as f32 - 127.5) / 128.0, (c[1] as f32 - 127.5) / 128.0)).collect();
    let mut planner = FftPlanner::<f32>::new();

    // average spectrum
    let fft = planner.plan_fft_forward(2048);
    let hann: Vec<f32> = (0..2048).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 2048.0).cos()).collect();
    let mut acc = vec![0f64; 2048];
    let mut count = 0;
    for blk in x.chunks_exact(2048).step_by(4) {
        let mut b: Vec<Complex32> = blk.iter().zip(&hann).map(|(s, w)| s * w).collect();
        fft.process(&mut b);
        for (a, v) in acc.iter_mut().zip(&b) {
            *a += v.norm_sqr() as f64;
        }
        count += 1;
    }
    let bin = |khz: i32| (khz.rem_euclid(2048)) as usize;
    let level = |lo: i32, hi: i32| {
        let s: f64 = (lo..hi).map(|k| acc[bin(k)]).sum::<f64>() / ((hi - lo) as f64 * count as f64);
        10.0 * s.max(1e-20).log10()
    };
    println!("samples {got}, spectrum (dB, 64 kHz bands, relative):");
    let floor = level(-768, 768);
    for lo in (-1024..1024).step_by(64) {
        let l = level(lo, lo + 64);
        println!("  {:>6} .. {:>6} kHz {:>7.1} {}", lo, lo + 64, l - floor, "#".repeat(((l - floor + 40.0).max(0.0) / 1.5) as usize));
    }

    // power envelopes: wideband (blocks of 64) and in-band (256 point STFT, hop 64)
    const H: usize = 64;
    const N: usize = 256;
    let stft = planner.plan_fft_forward(N);
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let hops = (got - N) / H;
    let mut wide = vec![0f32; hops];
    let mut inband = vec![0f32; hops];
    let mut b = vec![Complex32::new(0.0, 0.0); N];
    for h in 0..hops {
        let s = &x[h * H..h * H + N];
        wide[h] = s.iter().map(|c| c.norm_sqr()).sum();
        for (o, (v, w)) in b.iter_mut().zip(s.iter().zip(&win)) {
            *o = v * w;
        }
        stft.process(&mut b);
        // ±768 kHz: bins of 8 kHz up to 96
        inband[h] = (0..=96).chain(N - 96..N).map(|k| b[k].norm_sqr()).sum();
    }
    let frame = 196_608 / H;
    let wlen = (2656 - N) / H;
    println!("per frame: lowest null-long window / mean (dB), whole band | in-band");
    for (fi, fr) in (0..hops / frame).map(|i| (i, i * frame)) {
        let ratio = |p: &[f32]| {
            let seg = &p[fr..(fr + frame).min(p.len())];
            let mean = seg.iter().sum::<f32>() / seg.len() as f32;
            let mut best = f32::MAX;
            let mut pos = 0;
            for i in 0..seg.len().saturating_sub(wlen) {
                let s = seg[i..i + wlen].iter().sum::<f32>() / wlen as f32;
                if s < best {
                    best = s;
                    pos = i;
                }
            }
            (10.0 * (best / mean).log10(), pos)
        };
        let (w, wp) = ratio(&wide);
        let (ib, ip) = ratio(&inband);
        if fi < 12 {
            println!("  frame {fi:>3}: {w:>6.1} dB at {:>6} | {ib:>6.1} dB at {:>6}", wp * H, ip * H);
        }
    }
}
