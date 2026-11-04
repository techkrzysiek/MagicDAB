//! Decode a raw IQ recording (.uff or plain u8 / s16 .raw) and
//! print what the receiver sees. Used for development and comparison runs.
//!
//! usage: uffplay <file> [seconds] [--sid 0xXXXX] [--wav out.wav] [--all]

use magicdab_dab::receiver::{ReceiverHandle, RxCommand, RxEvent, ServiceKey};
use magicdab_dab::tables;
use magicdab_dab::Complex32;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: uffplay <file> [seconds] [--sid 0x...] [--wav out.wav] [--all]");
        return;
    }
    let path = &args[1];
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1e9);
    let mut sid: Option<u32> = None;
    let mut wav: Option<String> = None;
    let mut all = false;
    let mut window: Option<(f64, f64)> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--sid" => {
                sid = u32::from_str_radix(args[i + 1].trim_start_matches("0x"), 16).ok();
                i += 1;
            }
            "--wav" => {
                wav = Some(args[i + 1].clone());
                i += 1;
            }
            "--all" => all = true,
            "--window" => {
                window = Some((args[i + 1].parse().unwrap(), args[i + 2].parse().unwrap()));
                i += 2;
            }
            _ => {}
        }
        i += 1;
    }

    let mut f = std::fs::File::open(path).expect("open file");
    let mut header = vec![0u8; 2048];
    f.read_exact(&mut header).unwrap();
    let mut s16 = false;
    let offset = if header.starts_with(b"<?xml") {
        let txt = String::from_utf8_lossy(&header);
        s16 = txt.contains("Bits=\"16\"");
        let off = txt.split("Offset=\"").nth(1).and_then(|s| s.split('"').next()).and_then(|s| s.parse().ok()).unwrap_or(2048);
        println!(
            "UFF header: {}",
            txt.lines().filter(|l| l.contains("Frequency") || l.contains("Time")).collect::<Vec<_>>().join(" | ").trim()
        );
        off
    } else {
        0
    };
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(offset)).unwrap();

    let started = Instant::now();
    let rx = ReceiverHandle::start();
    if let Some(s) = sid {
        rx.send(RxCommand::StartService(ServiceKey { sid: s, scids: 0 }));
    }
    let total_samples = (seconds * 2_048_000.0) as u64;
    let queue = rx.queue.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        let mut samples = Vec::with_capacity(32768);
        let mut sent = 0u64;
        while sent < total_samples {
            let n = match f.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            samples.clear();
            if s16 {
                for c in buf[..n].chunks_exact(4) {
                    let i = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
                    let q = i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0;
                    samples.push(Complex32::new(i, q));
                }
            } else {
                for c in buf[..n].chunks_exact(2) {
                    samples.push(Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5));
                }
            }
            while queue.len() > 1_500_000 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            queue.push(&samples);
            sent += samples.len() as u64;
        }
        // let the receiver drain
        while queue.len() > 200_000 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    });

    let mut ensemble = None;
    let mut labels = BTreeMap::new();
    let mut started_services = Vec::new();
    let mut wav_data: Vec<i16> = Vec::new();
    let mut last_signal = None;
    let mut tii_hist: BTreeMap<(u8, u8), (u32, f32)> = BTreeMap::new();
    let mut stats = BTreeMap::new();
    let mut stat_series: Vec<(u64, magicdab_dab::audio::AudioStats)> = Vec::new();
    let mut formats = BTreeMap::new();
    let mut slides = 0;
    let mut audio_samples: BTreeMap<u32, usize> = BTreeMap::new();
    loop {
        match rx.events.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(ev) => match ev {
                RxEvent::Signal(s) => {
                    if std::env::var("FIBLOG").is_ok() {
                        if let Some(prev) = &last_signal {
                            let p: &magicdab_dab::receiver::SignalInfo = prev;
                            if s.fib_errors > p.fib_errors {
                                println!(
                                    "frame {:5} fib errors +{} snr {:.1} off {:.1}",
                                    s.frames,
                                    s.fib_errors - p.fib_errors,
                                    s.snr,
                                    s.freq_offset
                                );
                            }
                        }
                    }
                    last_signal = Some(s)
                }
                RxEvent::Ensemble(e, _) => {
                    if e.eid.is_some() {
                        for (s_id, s) in &e.services {
                            let want = all || sid == Some(*s_id);
                            if want && s.is_audio() && !started_services.contains(s_id) {
                                started_services.push(*s_id);
                                rx.send(RxCommand::StartService(ServiceKey { sid: *s_id, scids: 0 }));
                            }
                        }
                        ensemble = Some(e);
                    }
                }
                RxEvent::Label { key, label } => {
                    labels.insert(key.sid, label);
                }
                RxEvent::Audio { key, pcm } => {
                    *audio_samples.entry(key.sid).or_default() += pcm.len() / 2;
                    if Some(key.sid) == sid && wav.is_some() {
                        wav_data.extend(pcm.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                    }
                }
                RxEvent::AudioFormat { key, format, .. } => {
                    formats.insert(key.sid, format);
                }
                RxEvent::Stats { key, stats: s, frame } => {
                    if Some(key.sid) == sid {
                        stat_series.push((frame, s.clone()));
                    }
                    if std::env::var("AULOG").is_ok() && Some(key.sid) == sid {
                        let prev: magicdab_dab::audio::AudioStats = stats.get(&key.sid).cloned().unwrap_or_default();
                        println!(
                            "t={:.1}s superframes {} au_err +{} rs_uncorr +{} rs_corr +{}",
                            started.elapsed().as_secs_f64(),
                            s.superframes,
                            s.au_errors - prev.au_errors,
                            s.rs_uncorrectable - prev.rs_uncorrectable,
                            s.rs_corrected_bytes - prev.rs_corrected_bytes
                        );
                    }
                    stats.insert(key.sid, s);
                }
                RxEvent::Tii { codes, pairs } => {
                    if std::env::var("TIIDUMP").is_ok() {
                        let mut sorted = pairs.clone();
                        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let med = sorted[96].max(1e-9);
                        let row: Vec<String> = pairs.iter().map(|p| format!("{:.0}", 10.0 * (p / med).log10())).collect();
                        println!("TII pairs dB over median: {}", row.join(" "));
                    }
                    for c in codes {
                        let e = tii_hist.entry((c.main, c.sub)).or_insert((0, 0.0));
                        e.0 += 1;
                        e.1 = c.level_db;
                    }
                }
                RxEvent::XpadObject { key, app_type, object } => {
                    slides += 1;
                    println!(
                        "XPAD object sid {:04X} app {} '{}' {} bytes type {}/{}",
                        key.sid,
                        app_type,
                        object.content_name,
                        object.body.len(),
                        object.content_type,
                        object.content_subtype
                    );
                }
                RxEvent::DataObject { sid, scid, ua_type, object } => {
                    println!(
                        "DATA object sid {:X} scid {} ua {:#x} '{}' {} bytes type {}/{} mime {:?}",
                        sid,
                        scid,
                        ua_type,
                        object.content_name,
                        object.body.len(),
                        object.content_type,
                        object.content_subtype,
                        object.mime
                    );
                    if ua_type == 7 && object.content_type == 7 {
                        if let Some(doc) = magicdab_dab::spi::decode(&object.body, 0) {
                            let sv = magicdab_dab::spi::services(&doc);
                            println!("   SPI doc <{}> services {}", doc.name, sv.len());
                            if std::env::var("SPIXML").is_ok() {
                                let mut x = String::new();
                                doc.to_xml(&mut x, 0);
                                println!("{}", &x[..x.len().min(3000)]);
                                let raw: Vec<String> = object.body.iter().take(120).map(|b| format!("{b:02x}")).collect();
                                println!("RAW {}", raw.join(" "));
                            }
                            for s in sv.iter().take(30) {
                                println!(
                                    "     {:?} {:?} logos {:?}",
                                    s.bearers,
                                    s.medium_name,
                                    s.logos.iter().map(|l| format!("{}x{} {}", l.width, l.height, l.url)).collect::<Vec<_>>()
                                );
                            }
                        }
                    }
                }
                _ => {}
            },
            Err(_) => {
                if reader.is_finished() {
                    break;
                }
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();

    println!("processing time {elapsed:.1}s");
    if let Some(s) = &last_signal {
        println!(
            "signal: synced={} snr={:.1} dB freq_offset={:.1} Hz FIBs={} errors={} ({:.2}%)",
            s.synced,
            s.snr,
            s.freq_offset,
            s.fib_total,
            s.fib_errors,
            100.0 * s.fib_errors as f64 / s.fib_total.max(1) as f64
        );
    }
    if let Some(e) = &ensemble {
        println!(
            "ensemble: 0x{:04X} '{}' [{}] ECC 0x{:02X} LTO {:?} services {} subchannels {} used CU {}",
            e.eid.unwrap_or(0),
            e.label.as_ref().map(|l| l.text.as_str()).unwrap_or(""),
            e.label.as_ref().map(|l| l.short.as_str()).unwrap_or(""),
            e.ecc.unwrap_or(0),
            e.lto,
            e.services.len(),
            e.subchannels.len(),
            e.used_cus()
        );
        if let Some(t) = e.time {
            println!("time: MJD {} {:02}:{:02}:{:02} UTC", t.mjd, t.hours, t.minutes, t.seconds);
        }
        for s in e.services.values() {
            let p = s.primary();
            let sc = p.and_then(|c| c.subch_id).and_then(|id| e.subchannels.get(&id));
            println!(
                "  0x{:04X} {:<16} [{:<8}] pty={:<16} {} subch={:?} {} {}kbps {} UA={:?}",
                s.sid,
                s.label.as_ref().map(|l| l.text.as_str()).unwrap_or("?"),
                s.label.as_ref().map(|l| l.short.as_str()).unwrap_or(""),
                s.pty.map(tables::pty_name).unwrap_or("-"),
                p.map(|c| if c.tmid == 0 {
                    if c.ctype == 63 {
                        "DAB+"
                    } else {
                        "DAB"
                    }
                } else {
                    "data"
                })
                .unwrap_or("?"),
                p.and_then(|c| c.subch_id),
                sc.map(|s| format!("{} start {} size {}", s.protection_label, s.start, s.size)).unwrap_or_default(),
                sc.map(|s| s.bitrate).unwrap_or(0),
                s.language.map(tables::language_name).unwrap_or(""),
                p.map(|c| c.user_apps.iter().map(|u| tables::user_app_name(u.ua_type)).collect::<Vec<_>>()).unwrap_or_default(),
            );
            if let Some(st) = stats.get(&s.sid) {
                let fmt = formats.get(&s.sid);
                println!(
                    "      audio: {} {} Hz ch={} | superframes {} AU ok {} err {} | RS corrected {} ({} bits) uncorrectable {} / {} | out {:.1}s",
                    fmt.map(|f| f.codec.as_str()).unwrap_or("?"),
                    fmt.map(|f| f.sample_rate).unwrap_or(0),
                    fmt.map(|f| f.channels).unwrap_or(0),
                    st.superframes,
                    st.au_ok,
                    st.au_errors,
                    st.rs_corrected_bytes,
                    st.rs_corrected_bits,
                    st.rs_uncorrectable,
                    st.rs_codewords,
                    audio_samples.get(&s.sid).copied().unwrap_or(0) as f64 / 48000.0
                );
            }
            if let Some(l) = labels.get(&s.sid) {
                println!("      DLS: {}", l.text);
                for t in &l.tags {
                    println!("        {}: {}", t.name, t.text);
                }
            }
        }
        for s in e.services.values().filter(|s| !s.is_audio()) {
            println!(
                "  data service {:X} {:?}: {:?}",
                s.sid,
                s.label.as_ref().map(|l| l.text.clone()),
                s.components
                    .iter()
                    .map(|c| format!(
                        "tmid {} type {} subch {:?} scid {:?} addr {:?} UA {:?}",
                        c.tmid,
                        c.ctype,
                        c.subch_id,
                        c.scid,
                        c.packet_address,
                        c.user_apps.iter().map(|u| format!("{:#x}", u.ua_type)).collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        for l in &e.linkage {
            println!(
                "  linkage LSN {:03X} active={} hard={} intl={} oe={} key={:?} ids={:?}",
                l.lsn,
                l.active,
                l.hard,
                l.international,
                l.oe,
                l.key_sid.map(|k| format!("{k:04X}")),
                l.ids.iter().map(|i| format!("{}:{:04X}", i.kind, i.id)).collect::<Vec<_>>()
            );
        }
        for fi in &e.frequency_info {
            println!(
                "  FI id {:04X} R&M {} oe={} cont={} freqs={:?}",
                fi.id,
                fi.rm,
                fi.oe,
                fi.continuity,
                fi.frequencies.iter().map(|f| f.khz).collect::<Vec<_>>()
            );
        }
        for o in &e.oe_services {
            println!("  OE service {:04X} -> {:?}", o.sid, o.eids.iter().map(|e| format!("{e:04X}")).collect::<Vec<_>>());
        }
    }
    println!("TII codes:");
    for ((m, s), (n, lvl)) in &tii_hist {
        println!("  {m}-{s}: seen {n}x level {lvl:.1} dB");
    }
    println!("X-PAD MOT objects: {slides}");
    // statistics in a signal time window (frames of 96 ms)
    if let Some(w) = window {
        let in_win: Vec<_> = stat_series.iter().filter(|(f, _)| (*f as f64 * 0.096) >= w.0 && (*f as f64 * 0.096) <= w.1).collect();
        if let (Some(a), Some(b)) = (in_win.first(), in_win.last()) {
            println!(
                "window {:.0}-{:.0}s: AU ok {} err {} | RS uncorrectable {} corrected bits {} | superframes {}",
                w.0,
                w.1,
                b.1.au_ok - a.1.au_ok,
                b.1.au_errors - a.1.au_errors,
                b.1.rs_uncorrectable - a.1.rs_uncorrectable,
                b.1.rs_corrected_bits - a.1.rs_corrected_bits,
                b.1.superframes - a.1.superframes
            );
        }
    }
    if let Some(path) = wav {
        let mut f = std::fs::File::create(&path).unwrap();
        let data_len = (wav_data.len() * 2) as u32;
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&48000u32.to_le_bytes()).unwrap();
        f.write_all(&(48000u32 * 4).to_le_bytes()).unwrap();
        f.write_all(&4u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for s in wav_data {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
        println!("wrote {path}");
    }
}
