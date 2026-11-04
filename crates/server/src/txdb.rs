//! Transmitter database for TII decoding, imported from a CSV file.
//!
//! The format: columns separated by semicolons (id; country; channel; label; EId; TII; location;
//! latitude; longitude; altitude; height; polarization; frequency MHz; power kW).

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transmitter {
    pub country: String,
    pub channel: String,
    pub label: String,
    pub eid: u16,
    pub ecc: u8,
    pub main: u8,
    pub sub: u8,
    pub location: String,
    pub lat: f64,
    pub lon: f64,
    pub altitude: f64,
    pub height: f64,
    pub polarization: String,
    pub frequency: u32,
    pub power_kw: f32,
}

#[derive(Default)]
pub struct TxDb {
    by_key: HashMap<(u16, u8, u8), Vec<Transmitter>>,
    pub count: usize,
    pub updated: Option<i64>,
}

/// ECC per ISO country code.
fn ecc_for_country(c: &str) -> u8 {
    match c {
        "AT" | "BE" | "DE" | "IT" | "MT" | "AL" | "HU" | "RU" | "AD" | "IL" | "DZ" | "EG" => 0xE0,
        "CH" | "DK" | "FR" | "GB" | "GI" | "GR" | "LU" | "FI" | "BG" | "RO" | "CY" | "SM" => 0xE1,
        "CZ" | "ES" | "MC" | "NO" | "PL" | "RS" | "SK" | "TN" | "VA" | "LT" | "IS" | "LI" | "MA" => 0xE2,
        "AZ" | "HR" | "IE" | "NL" | "SE" | "TR" | "ME" | "LV" | "BY" | "KZ" => 0xE3,
        "MK" | "SI" | "TM" | "UA" | "PT" | "EE" | "MD" | "BA" | "GE" | "AM" | "XK" => 0xE4,
        "AU" | "MY" => 0xF0,
        "IR" | "KR" | "OM" => 0xF1,
        "ID" | "KW" | "QA" => 0xF2,
        "TH" => 0xF3,
        "ZA" => 0xD0,
        _ => 0,
    }
}

impl TxDb {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("transmitters.csv")
    }

    pub fn load(dir: &Path) -> Self {
        let path = Self::file(dir);
        let mut db = TxDb::default();
        if let Ok(txt) = std::fs::read_to_string(&path) {
            db.parse(&txt);
            db.updated = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
        }
        db
    }

    /// Parse CSV content; returns the number of transmitters.
    pub fn parse(&mut self, txt: &str) -> usize {
        self.by_key.clear();
        self.count = 0;
        let txt = txt.trim_start_matches('\u{feff}');
        for (n, line) in txt.lines().enumerate() {
            if n == 0 && !line.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                // header line
                continue;
            }
            let cols: Vec<&str> = line.split(';').map(|c| c.trim()).collect();
            if cols.len() < 14 || cols[5].is_empty() {
                continue;
            }
            let lat: f64 = cols[7].replace(',', ".").parse().unwrap_or(f64::NAN);
            let lon: f64 = cols[8].replace(',', ".").parse().unwrap_or(f64::NAN);
            if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
                continue;
            }
            let Ok(eid) = u16::from_str_radix(cols[4].trim_start_matches("0x"), 16) else { continue };
            let Ok(tii) = cols[5].parse::<u16>() else { continue };
            let country = cols[1].to_uppercase();
            let tx = Transmitter {
                ecc: ecc_for_country(&country),
                country,
                channel: cols[2].to_string(),
                label: cols[3].to_string(),
                eid,
                main: (tii / 100) as u8,
                sub: (tii % 100) as u8,
                location: cols[6].to_string(),
                lat,
                lon,
                altitude: cols[9].replace(',', ".").parse().unwrap_or(0.0),
                height: cols[10].replace(',', ".").parse().unwrap_or(0.0),
                polarization: cols[11].to_string(),
                frequency: (cols[12].replace(',', ".").parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u32,
                power_kw: cols[13].replace(',', ".").parse().unwrap_or(0.0),
            };
            self.by_key.entry((tx.eid, tx.main, tx.sub)).or_default().push(tx);
            self.count += 1;
        }
        self.count
    }

    /// Look up a transmitter by ensemble id, ECC, TII code and frequency (kHz).
    pub fn find(&self, eid: u16, ecc: Option<u8>, main: u8, sub: u8, freq_khz: u32) -> Option<&Transmitter> {
        let list = self.by_key.get(&(eid, main, sub))?;
        // prefer a matching ECC, then the closest frequency
        list.iter().min_by_key(|t| {
            let ecc_penalty = match ecc {
                Some(e) if t.ecc != 0 && t.ecc != e => 1_000_000i64,
                _ => 0,
            };
            ecc_penalty + (t.frequency as i64 - freq_khz as i64).abs()
        })
    }

    /// All transmitters of an ensemble (for showing undetected ones on the map).
    pub fn ensemble(&self, eid: u16) -> Vec<&Transmitter> {
        self.by_key.iter().filter(|((e, _, _), _)| *e == eid).flat_map(|(_, v)| v.iter()).collect()
    }
}

/// Great circle distance (km) and initial bearing (degrees) between two points.
pub fn distance_azimuth(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let dp = p2 - p1;
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    let d = 2.0 * 6371.0 * a.sqrt().atan2((1.0 - a).sqrt());
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    let az = (y.atan2(x).to_degrees() + 360.0) % 360.0;
    (d, az)
}
