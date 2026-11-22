//! Journaline objects of a sub-channel recording: its logical frames one
//! after the other (bitrate * 3 bytes each), as an EDI or ETI capture gives
//! them. Journaline comes from the X-PAD of a DAB+ service or from a packet
//! mode sub-channel. Prints the menu tree from the entry object, then every
//! object. Development tool.
//!
//! usage: journaline_dump <file> <bitrate kbps> xpad [X-PAD AppTy, default 4] [--full] [--hex]
//!        journaline_dump <file> <bitrate kbps> packet <packet address> [--full] [--hex]
//!
//! --full prints whole texts, --hex every data group that changed an object.

use magicdab_dab::dabplus::SuperframeDecoder;
use magicdab_dab::journaline::{Item, JmlObject, JournalineDecoder};
use magicdab_dab::packet::PacketDecoder;
use magicdab_dab::pad::{dabplus_pad, PadDecoder, PadEvent};
use std::collections::HashSet;

const USAGE: &str = "usage: journaline_dump <file> <bitrate kbps> xpad [AppTy] | packet <address> [--full] [--hex]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let full = std::env::args().any(|a| a == "--full");
    let hex = std::env::args().any(|a| a == "--hex");
    let (Some(path), Some(bitrate), Some(mode)) = (args.first(), args.get(1).and_then(|b| b.parse::<u16>().ok()), args.get(2)) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let data = std::fs::read(path).expect("read file");
    let frame_len = bitrate as usize * 3;
    let mut jml = JournalineDecoder::new();
    let mut changes = 0usize;
    let mut on_dg = |dg: &[u8]| {
        let changed = jml.push(dg);
        if hex && !changed.is_empty() {
            println!("{changed:04X?}: {}", dg.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        changes += changed.len();
    };
    match mode.as_str() {
        "xpad" => {
            let app_type: u8 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(4);
            let mut sf = SuperframeDecoder::new(bitrate);
            let mut pad = PadDecoder::new();
            pad.register_app(app_type);
            let mut aus: Vec<Vec<u8>> = Vec::new();
            let mut events = Vec::new();
            let mut data_groups = 0usize;
            for frame in data.chunks_exact(frame_len) {
                sf.push(frame, |_, au| {
                    if au.crc_ok {
                        aus.push(au.data.to_vec());
                    }
                });
                for au in aus.drain(..) {
                    if let Some((xpad, fpad)) = dabplus_pad(&au) {
                        pad.process(xpad, true, fpad, &mut events);
                    }
                }
                for ev in events.drain(..) {
                    if let PadEvent::DataGroup { data, .. } = ev {
                        data_groups += 1;
                        on_dg(&data);
                    }
                }
            }
            let st = sf.stats;
            println!(
                "{} frames, {} superframes, AUs ok {} bad {}, X-PAD AppTy {app_type}: {data_groups} data groups, PAD errors {}",
                data.len() / frame_len,
                st.superframes,
                st.au_ok,
                st.au_errors,
                pad.errors
            );
        }
        "packet" => {
            let address: u16 = args.get(3).and_then(|a| a.parse().ok()).expect(USAGE);
            let mut packets = PacketDecoder::new();
            let mut data_groups = 0usize;
            for frame in data.chunks_exact(frame_len) {
                packets.push(frame, |a, dg| {
                    if a == address {
                        data_groups += 1;
                        on_dg(&dg);
                    }
                });
            }
            println!(
                "{} frames, {} packets ({} CRC errors), address {address}: {data_groups} data groups",
                data.len() / frame_len,
                packets.packets,
                packets.crc_errors
            );
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    println!("{:?}, {changes} changes reported, {} objects, TOC {:?}", jml.stats, jml.len(), jml.toc());

    println!("\nmenu tree:");
    let mut shown = HashSet::new();
    tree(&jml, 0, 0, &mut shown);
    let unreachable: Vec<String> = jml.objects().filter(|o| !shown.contains(&o.id)).map(|o| format!("{:04X}", o.id)).collect();
    if !unreachable.is_empty() {
        println!("not reached from 0000: {}", unreachable.join(" "));
    }

    println!("\nobjects:");
    for o in jml.objects() {
        print_object(o, full);
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.replace('\n', " / ");
    if s.chars().count() > n {
        format!("{}...", s.chars().take(n).collect::<String>())
    } else {
        s
    }
}

fn tree(jml: &JournalineDecoder, id: u16, depth: usize, shown: &mut HashSet<u16>) {
    let indent = "  ".repeat(depth);
    let Some(o) = jml.object(id) else {
        println!("{indent}[{id:04X}] (not received)");
        return;
    };
    if !shown.insert(id) {
        println!("{indent}{id:04X} (above)");
        return;
    }
    println!("{indent}{id:04X} {:?} r{} \"{}\"", o.kind, o.revision, short(&o.title, 60));
    for item in &o.items {
        if let Item::Link { target, .. } = item {
            tree(jml, *target, depth + 1, shown);
        }
    }
}

fn print_object(o: &JmlObject, full: bool) {
    let n = if full { usize::MAX } else { 100 };
    println!(
        "{:04X} {:?} rev {} static {} title \"{}\"{}{}",
        o.id,
        o.kind,
        o.revision,
        o.static_flag,
        short(&o.title, n),
        o.expires.map(|t| format!(" expires {t}")).unwrap_or_default(),
        o.timeout_minutes.map(|t| format!(" timeout {t} min")).unwrap_or_default()
    );
    for item in &o.items {
        match item {
            Item::Link { target, text } => println!("    -> {target:04X} {}", short(text, n)),
            Item::Text { text } => println!("    {}", short(text, n)),
            Item::Row { cells } => println!("    | {}", cells.iter().map(|c| short(c, n)).collect::<Vec<_>>().join(" | ")),
        }
    }
    for l in &o.hot_links {
        println!("    hot link {l:?}");
    }
}
