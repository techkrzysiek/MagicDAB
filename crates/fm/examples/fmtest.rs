//! Live check of a tuner: cargo run --release -p magicdab-fm --example fmtest -- <kHz>
use magicdab_fm::{
    capture::Capture,
    rds::RdsParser,
    tuner::{FmTuner, TunerEvent},
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let khz: u32 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(105_000);
    let (tx, rx) = crossbeam_channel::unbounded();
    let tuner = FmTuner::start(String::new(), tx);
    tuner.tune(khz);
    let level = Arc::new(Mutex::new((0f64, 0usize)));
    let l = level.clone();
    let _cap = Capture::start(String::new(), move |s| {
        let mut g = l.lock().unwrap();
        for v in s {
            g.0 += (*v as f64) * (*v as f64);
        }
        g.1 += s.len();
    });
    let t = Instant::now();
    let (mut signals, mut groups, mut last_signal, mut pi) = (0, 0, None, None);
    let mut rds = RdsParser::new();
    while t.elapsed() < Duration::from_secs(8) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(TunerEvent::Signal(s)) => {
                signals += 1;
                last_signal = Some(s);
            }
            Ok(TunerEvent::Group { blocks, errors }) => {
                groups += 1;
                let (p, pe) = pi.unwrap_or((0, 3));
                rds.parse([p, blocks[0], blocks[1], blocks[2]], [pe, errors[0], errors[1], errors[2]]);
            }
            Ok(TunerEvent::Pi { pi: p, errors }) => pi = Some((p, errors)),
            Ok(e) => println!("{e:?}"),
            Err(_) => {}
        }
    }
    let (sum, n) = *level.lock().unwrap();
    let rms = if n > 0 { 20.0 * ((sum / n as f64).sqrt()).log10() } else { -999.0 };
    println!("RDS: {:?}", rds.info());
    println!("{signals} signal reports, last {last_signal:?}, PI {pi:x?}, {groups} RDS groups, audio {n} samples at {rms:.1} dBFS");
}
