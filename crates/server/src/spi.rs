//! Service logos and service information from SPI (received over the air)
//! and RadioDNS (internet), stored on disk and offered to the web clients.

use magicdab_dab::ensemble::Ensemble;
use magicdab_dab::mot::MotObject;
use magicdab_dab::spi::{self, LogoRef};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoVariant {
    pub id: String,
    pub sid: u32,
    pub ecc: u8,
    pub width: u16,
    pub height: u16,
    pub mime: String,
    /// "SPI" or "RadioDNS"
    pub source: String,
    pub size: usize,
    /// Unix time of reception.
    pub received: i64,
    /// MOT content name or URL.
    pub url: String,
    pub file: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceText {
    pub short_name: Option<String>,
    pub medium_name: Option<String>,
    pub long_name: Option<String>,
    pub description: Option<String>,
    pub links: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Programme {
    pub start: i64,
    pub duration: i64,
    pub name: String,
    pub description: Option<String>,
    pub genres: Vec<String>,
}

/// Parse an ISO 8601 date/time with offset into Unix time.
pub fn parse_iso(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, dd): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let (time, tz) = match rest.find(['+', '-', 'Z']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "Z"),
    };
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next().unwrap_or("0").parse().ok()?;
    let ss: i64 = t.next().unwrap_or("0").split('.').next()?.parse().ok()?;
    let offset = if tz == "Z" || tz.is_empty() {
        0
    } else {
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        let mut p = tz[1..].split(':');
        let oh: i64 = p.next()?.parse().ok()?;
        let om: i64 = p.next().unwrap_or("0").parse().ok()?;
        sign * (oh * 3600 + om * 60)
    };
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mi * 60 + ss - offset)
}

/// "PT1H30M", "PT3600S" -> seconds
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.strip_prefix("PT")?;
    let mut total = 0i64;
    let mut num = String::new();
    for c in s.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'H' => {
                total += num.parse::<f64>().ok()? as i64 * 3600;
                num.clear();
            }
            'M' => {
                total += num.parse::<f64>().ok()? as i64 * 60;
                num.clear();
            }
            'S' => {
                total += num.parse::<f64>().ok()? as i64;
                num.clear();
            }
            _ => return None,
        }
    }
    Some(total)
}

/// A service information document (SI, PI) as received, kept as XML.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpiDoc {
    pub id: String,
    /// "SPI" (over the air) or "RadioDNS"
    pub source: String,
    /// Root element: serviceInformation, epg, ...
    pub kind: String,
    /// MOT content name or URL.
    pub url: String,
    /// Ensemble (over the air) or host (RadioDNS).
    pub scope: String,
    pub received: i64,
    /// Size of the document as transmitted (binary or XML).
    pub size: usize,
    /// Number of services (SI) or programmes (PI).
    pub items: usize,
    /// Hash of the XML (unchanged repetitions of a carousel are not stored again).
    #[serde(default)]
    pub hash: String,
}

const MAX_DOCS: usize = 120;

fn doc_id(key: &str) -> String {
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    docs: Vec<SpiDoc>,
    logos: Vec<LogoVariant>,
    #[serde(default)]
    texts: HashMap<String, ServiceText>,
    /// SId (hex) -> programmes sorted by start time
    #[serde(default)]
    epg: HashMap<String, Vec<Programme>>,
}

pub struct SpiStore {
    dir: PathBuf,
    index: Index,
    /// Objects of the SPI carousels received in this session (content name -> data).
    objects: HashMap<String, (String, Arc<Vec<u8>>)>,
    /// Logo references from SI documents waiting for their MOT object.
    pending: Vec<(u32, u8, LogoRef)>,
    pub documents: u64,
    /// The document list changed since it was last sent.
    pub docs_dirty: bool,
    /// Since when the index on disk is older than the one here.
    unsaved: Option<std::time::Instant>,
}

fn ext_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

pub fn sniff_mime(data: &[u8]) -> &'static str {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if data.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.starts_with(b"BM") && data.len() > 54 {
        "image/bmp"
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else if data.starts_with(b"<svg") || data.starts_with(b"<?xml") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

/// Width and height of PNG / JPEG / GIF images.
pub fn image_size(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() > 24 && data.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((w as u16, h as u16));
    }
    if data.len() > 10 && data.starts_with(b"GIF8") {
        return Some((u16::from_le_bytes([data[6], data[7]]), u16::from_le_bytes([data[8], data[9]])));
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]);
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]);
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

impl SpiStore {
    pub fn load(data_dir: &Path) -> Self {
        let dir = data_dir.join("logos");
        let _ = std::fs::create_dir_all(&dir);
        let index = std::fs::read_to_string(dir.join("index.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        SpiStore { dir, index, objects: HashMap::new(), pending: Vec::new(), documents: 0, docs_dirty: false, unsaved: None }
    }

    /// The index (with the whole programme guide) is written at most every 10 s:
    /// while a carousel comes in it changes with every object.
    fn index_changed(&mut self) {
        self.unsaved.get_or_insert_with(std::time::Instant::now);
    }

    pub fn save_if_due(&mut self) {
        if self.unsaved.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(10)) {
            self.save();
        }
    }

    /// Write the index now if it changed (before the program ends).
    pub fn flush(&mut self) {
        if self.unsaved.is_some() {
            self.save();
        }
    }

    fn save(&mut self) {
        self.unsaved = None;
        // (atomically: a power cut while writing must not lose the index)
        if let Ok(s) = serde_json::to_string(&self.index) {
            let tmp = self.dir.join("index.json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, self.dir.join("index.json"));
            }
        }
    }

    pub fn clear_session(&mut self) {
        self.objects.clear();
        self.pending.clear();
    }

    /// Store a logo image; returns true when something changed.
    pub fn add_logo(
        &mut self,
        sid: u32,
        ecc: u8,
        width: u16,
        height: u16,
        data: &[u8],
        source: &str,
        url: &str,
        mime: Option<&str>,
    ) -> bool {
        if data.is_empty() || data.len() > 2_000_000 {
            return false;
        }
        let mime = mime.filter(|m| m.starts_with("image/")).unwrap_or_else(|| sniff_mime(data)).to_string();
        let (w, h) = image_size(data).unwrap_or((width, height));
        let id = format!("{ecc:02x}{sid:04x}-{w}x{h}-{}", source.to_lowercase());
        let file = format!("{id}.{}", ext_for(&mime));
        if let Some(v) = self.index.logos.iter().find(|v| v.id == id) {
            // the same picture again (a new one under the same name and size
            // replaces it)
            if v.size == data.len() && v.url == url && std::fs::read(self.dir.join(&v.file)).is_ok_and(|d| d == data) {
                return false;
            }
        }
        if std::fs::write(self.dir.join(&file), data).is_err() {
            return false;
        }
        // (a replaced picture of another type leaves no file behind)
        for v in self.index.logos.iter().filter(|v| v.id == id && v.file != file) {
            let _ = std::fs::remove_file(self.dir.join(&v.file));
        }
        self.index.logos.retain(|v| v.id != id);
        self.index.logos.push(LogoVariant {
            id,
            sid,
            ecc,
            width: w,
            height: h,
            mime,
            source: source.to_string(),
            size: data.len(),
            received: crate::servicelist::now(),
            url: url.to_string(),
            file,
        });
        self.index_changed();
        if source == "SPI" {
            log::info!("SPI logo {ecc:02X}{sid:04X} {w}x{h} ({} bytes, {url})", data.len());
        }
        true
    }

    /// The logos of a station its SPI no longer lists (`listed`: the ones it
    /// has now) are gone, also those still waiting for their picture.
    fn drop_unlisted_logos(&mut self, ecc: u8, sid: u32, listed: &[LogoRef]) -> bool {
        let has = |url: &str| listed.iter().any(|l| l.url == url);
        self.pending.retain(|(ps, pe, pl)| *ps != sid || *pe != ecc || has(&pl.url));
        let (gone, keep): (Vec<LogoVariant>, Vec<LogoVariant>) = std::mem::take(&mut self.index.logos)
            .into_iter()
            .partition(|v| v.source == "SPI" && v.sid == sid && v.ecc == ecc && !has(&v.url));
        self.index.logos = keep;
        for v in &gone {
            let _ = std::fs::remove_file(self.dir.join(&v.file));
            log::info!("SPI logo {ecc:02X}{sid:04X} {}x{} is no longer sent ({})", v.width, v.height, v.url);
        }
        if !gone.is_empty() {
            self.index_changed();
        }
        !gone.is_empty()
    }

    /// The name of a service from SPI / RadioDNS (medium, long or short name).
    pub fn service_name(&self, ecc: u8, sid: u32) -> Option<String> {
        let t = self.index.texts.get(&format!("{ecc:02x}{sid:04x}"))?;
        t.medium_name.clone().or_else(|| t.long_name.clone()).or_else(|| t.short_name.clone()).filter(|n| !n.trim().is_empty())
    }

    pub fn set_text(&mut self, ecc: u8, sid: u32, text: ServiceText) -> bool {
        let key = format!("{ecc:02x}{sid:04x}");
        let changed =
            self.index.texts.get(&key).map(|t| serde_json::to_string(t).ok() != serde_json::to_string(&text).ok()).unwrap_or(true);
        if changed {
            self.index.texts.insert(key, text);
            self.index_changed();
        }
        changed
    }

    /// Handle a MOT object of an SPI channel. Returns true when logos changed.
    pub fn on_object(&mut self, ens: &Ensemble, object: &MotObject) -> bool {
        let mut changed = false;
        if object.content_type == 7 {
            // binary encoded SPI document
            let ueid = ens.ueid().unwrap_or(0);
            if let Some(doc) = spi::decode(&object.body, ueid) {
                self.documents += 1;
                let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
                doc.to_xml(&mut xml, 0);
                let scope = format!("{} ({:06X})", ens.label.as_ref().map(|l| l.text.as_str()).unwrap_or(""), ueid);
                let name =
                    if object.content_name.is_empty() { format!("transport {}", object.transport_id) } else { object.content_name.clone() };
                self.add_doc("SPI", &format!("spi:{ueid:06X}:{name}"), &doc.name, &name, &scope, object.body.len(), &xml);
                if doc.name == "epg" {
                    let scope = object.param(0x27).and_then(|d| String::from_utf8(d.to_vec()).ok());
                    changed |= self.add_epg(&doc, scope.as_deref());
                }
                if doc.name == "serviceInformation" {
                    // the multiplex's own logos, kept by its EId (the interface shows them by the ensemble)
                    for (ecc, eid, l) in spi::ensemble_logos(&doc) {
                        let id = eid as u32;
                        if !self.pending.iter().any(|(ps, _, pl)| *ps == id && *pl == l) {
                            self.pending.push((id, ecc, l));
                        }
                    }
                    for s in spi::services(&doc) {
                        for b in &s.bearers {
                            let Some((ecc, _eid, sid, scids)) = spi::parse_bearer(b) else { continue };
                            // (a secondary component of the service, e.g. .1: not the station itself)
                            if scids != 0 {
                                continue;
                            }
                            let text = ServiceText {
                                short_name: s.short_name.clone(),
                                medium_name: s.medium_name.clone(),
                                long_name: s.long_name.clone(),
                                description: s.description.clone(),
                                links: s.links.clone(),
                            };
                            changed |= self.set_text(ecc, sid, text);
                            changed |= self.drop_unlisted_logos(ecc, sid, &s.logos);
                            for l in &s.logos {
                                if !self.pending.iter().any(|(ps, _, pl)| *ps == sid && pl == l) {
                                    self.pending.push((sid, ecc, l.clone()));
                                }
                            }
                        }
                    }
                }
            }
        } else if !object.content_name.is_empty() {
            self.objects.insert(object.content_name.clone(), (object.mime_type(), Arc::new(object.body.clone())));
            if self.objects.len() > 400 {
                // keep memory bounded
                let k = self.objects.keys().next().cloned();
                if let Some(k) = k {
                    self.objects.remove(&k);
                }
            }
        }
        // resolve logo references with received objects
        let mut resolved = Vec::new();
        for (i, (sid, ecc, l)) in self.pending.iter().enumerate() {
            let name = l.url.trim_start_matches('/');
            let obj = self.objects.get(&l.url).or_else(|| self.objects.get(name));
            if let Some((mime, data)) = obj {
                resolved.push((i, *sid, *ecc, l.clone(), mime.clone(), data.clone()));
            }
        }
        for (_, sid, ecc, l, mime, data) in resolved.iter() {
            changed |= self.add_logo(*sid, *ecc, l.width, l.height, data, "SPI", &l.url, l.mime.as_deref().or(Some(mime.as_str())));
        }
        let done: Vec<usize> = resolved.iter().map(|r| r.0).collect();
        let mut i = 0;
        self.pending.retain(|_| {
            let keep = !done.contains(&i);
            i += 1;
            keep
        });
        changed
    }

    fn add_epg(&mut self, doc: &spi::Element, scope_hint: Option<&str>) -> bool {
        let mut scopes = Vec::new();
        doc.find_all("serviceScope", &mut scopes);
        let default_bearer = scopes.first().and_then(|s| s.attr("id")).map(|s| s.to_string()).or(scope_hint.map(|s| s.to_string()));
        let mut programmes = Vec::new();
        doc.find_all("programme", &mut programmes);
        let now = crate::servicelist::now();
        let before = self.index.epg.clone();
        for p in programmes {
            // (the long name first: the medium one is cut at 16 characters)
            let name = p
                .child("longName")
                .or_else(|| p.child("mediumName"))
                .or_else(|| p.child("shortName"))
                .and_then(|e| e.text())
                .unwrap_or("")
                .trim()
                .to_string();
            let mut descs = Vec::new();
            p.find_all("shortDescription", &mut descs);
            p.find_all("longDescription", &mut descs);
            let description = descs.first().and_then(|e| e.text()).map(|s| s.trim().to_string());
            let mut genres = Vec::new();
            p.find_all("genre", &mut genres);
            let genres: Vec<String> = genres.iter().filter_map(|g| g.text().map(|s| s.to_string())).collect();
            let mut locations = Vec::new();
            p.find_all("location", &mut locations);
            for loc in locations {
                let Some(time) = loc.child("time") else { continue };
                let Some(start) = time.attr("time").and_then(parse_iso) else { continue };
                let duration = time.attr("duration").and_then(parse_duration).unwrap_or(0);
                let bearer = loc.child("bearer").and_then(|b| b.attr("id")).map(|s| s.to_string()).or_else(|| default_bearer.clone());
                let Some((_ecc, _eid, sid, scids)) = bearer.as_deref().and_then(spi::parse_bearer) else { continue };
                if scids != 0 {
                    continue;
                }
                let list = self.index.epg.entry(format!("{sid:X}")).or_default();
                if list.iter().any(|x| x.start == start && x.name == name) {
                    continue;
                }
                list.retain(|x| x.start != start);
                list.push(Programme { start, duration, name: name.clone(), description: description.clone(), genres: genres.clone() });
                list.sort_by_key(|x| x.start);
            }
        }
        // forget old programmes; a schedule that is all old (some broadcasters
        // send one of long ago) keeps its last day, shown as outdated
        for list in self.index.epg.values_mut() {
            if list.iter().any(|x| x.start + x.duration > now - 2 * 86400) {
                list.retain(|x| x.start + x.duration > now - 2 * 86400);
            } else if let Some(last) = list.iter().map(|x| x.start + x.duration).max() {
                list.retain(|x| x.start + x.duration > last - 86400);
            }
        }
        let changed = self.index.epg != before;
        if changed {
            self.index_changed();
        }
        changed
    }

    /// Remember a document (replacing an older version from the same place).
    pub fn add_doc(&mut self, source: &str, key: &str, kind: &str, url: &str, scope: &str, size: usize, xml: &str) {
        let id = doc_id(key);
        let hash = doc_id(xml);
        if let Some(d) = self.index.docs.iter_mut().find(|d| d.id == id && d.hash == hash) {
            // the same document again (carousel repetition)
            d.received = crate::servicelist::now();
            return;
        }
        let items = if kind == "epg" {
            xml.matches("<programme").count()
        } else {
            xml.matches("<service>").count() + xml.matches("<service ").count()
        };
        let dir = self.dir.parent().unwrap_or(&self.dir).join("spi");
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(dir.join(format!("{id}.xml")), xml).is_err() {
            return;
        }
        self.index.docs.retain(|d| d.id != id);
        self.index.docs.push(SpiDoc {
            id,
            source: source.to_string(),
            kind: kind.to_string(),
            url: url.to_string(),
            scope: scope.to_string(),
            received: crate::servicelist::now(),
            size,
            items,
            hash,
        });
        if self.index.docs.len() > MAX_DOCS {
            // programme information is the most numerous: drop the oldest first
            self.index.docs.sort_by_key(|d| (d.kind != "epg", d.received));
            let old: Vec<SpiDoc> = self.index.docs.drain(..self.index.docs.len() - MAX_DOCS).collect();
            for d in old {
                let _ = std::fs::remove_file(dir.join(format!("{}.xml", d.id)));
            }
        }
        self.index.docs.sort_by_key(|d| std::cmp::Reverse(d.received));
        self.docs_dirty = true;
        self.index_changed();
        log::info!(
            "SPI document from {source}: {kind} ({items} {}), {} kB, {scope}",
            if kind == "epg" { "programmes" } else { "services" },
            size / 1024
        );
    }

    pub fn docs_json(&self) -> Value {
        json!({"type": "spiDocs", "docs": self.index.docs})
    }

    pub fn doc_json(&self, id: &str) -> Value {
        let dir = self.dir.parent().unwrap_or(&self.dir).join("spi");
        let ok = self.index.docs.iter().any(|d| d.id == id) && id.chars().all(|c| c.is_ascii_hexdigit());
        let xml = if ok { std::fs::read_to_string(dir.join(format!("{id}.xml"))).ok() } else { None };
        // (documents stored before control characters were left out of the XML)
        let xml = xml.map(|x| x.chars().filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r')).collect::<String>());
        json!({"type": "spiDoc", "id": id, "xml": xml})
    }

    pub fn epg_index_json(&self) -> Value {
        let idx: HashMap<&String, usize> = self.index.epg.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k, v.len())).collect();
        json!({"type": "epgIndex", "services": idx})
    }

    pub fn epg_json(&self, sid: u32) -> Value {
        json!({"type": "epg", "sid": sid, "programmes": self.index.epg.get(&format!("{sid:X}")).cloned().unwrap_or_default()})
    }

    /// Largest square logo of a service (file name, mime) for link previews.
    pub fn best_logo(&self, sid: u32, ecc: u8) -> Option<(String, String)> {
        self.index
            .logos
            .iter()
            .filter(|v| v.sid == sid && (v.ecc == ecc || v.ecc == 0 || ecc == 0) && v.width == v.height && v.mime != "image/svg+xml")
            .max_by_key(|v| (v.width.min(600), v.source == "SPI"))
            .map(|v| (v.file.clone(), v.mime.clone()))
    }

    /// The logos of a service received over the air (SPI), for the server map.
    pub fn spi_logos(&self, ecc: u8, sid: u32) -> Vec<&LogoVariant> {
        self.index.logos.iter().filter(|v| v.sid == sid && v.source == "SPI" && (v.ecc == ecc || v.ecc == 0)).collect()
    }

    pub fn logo_path(&self, v: &LogoVariant) -> PathBuf {
        self.dir.join(&v.file)
    }

    pub fn text(&self, ecc: u8, sid: u32) -> Option<&ServiceText> {
        self.index.texts.get(&format!("{ecc:02x}{sid:04x}"))
    }

    /// The programme guide of a service (kept by SId: see add_epg).
    pub fn programmes(&self, sid: u32) -> &[Programme] {
        self.index.epg.get(&format!("{sid:X}")).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn has_radiodns_logos(&self, sid: u32, ecc: u8) -> bool {
        self.index.logos.iter().any(|v| v.sid == sid && v.ecc == ecc && v.source == "RadioDNS")
    }

    /// The logos of the stations `wanted` (those a listener can see: the
    /// ones received here; a RadioDNS document names a thousand others).
    pub fn list_json(&self, wanted: &std::collections::HashSet<u32>) -> Value {
        let mut by: HashMap<String, Vec<&LogoVariant>> = HashMap::new();
        for v in self.index.logos.iter().filter(|v| wanted.contains(&v.sid)) {
            by.entry(format!("{:X}", v.sid)).or_default().push(v);
        }
        for v in by.values_mut() {
            v.sort_by_key(|x| (x.width as u32) * (x.height as u32));
        }
        json!({"type": "logos", "logos": by, "texts": self.index.texts})
    }

    pub fn get(&self, id: &str) -> Option<(Value, Vec<u8>)> {
        let v = self.index.logos.iter().find(|v| v.id == id)?;
        let data = std::fs::read(self.dir.join(&v.file)).ok()?;
        Some((json!({"kind": "logo", "id": v.id, "sid": v.sid, "mime": v.mime}), data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logo_ref(url: &str) -> LogoRef {
        LogoRef { url: url.into(), mime: Some("image/png".into()), width: 128, height: 128, kind: None }
    }

    #[test]
    fn a_new_logo_replaces_the_old_and_one_no_longer_sent_is_gone() {
        let dir = std::env::temp_dir().join(format!("magicdab-spi-{}", std::process::id()));
        let mut s = SpiStore::load(&dir);
        let add = |s: &mut SpiStore, data: &[u8], url: &str| s.add_logo(0x1234, 0xE1, 128, 128, data, "SPI", url, Some("image/png"));
        assert!(add(&mut s, b"old picture", "/logo128.png"));
        assert!(!add(&mut s, b"old picture", "/logo128.png"));
        // (the same name and size, another picture)
        assert!(add(&mut s, b"new picture", "/logo128.png"));
        let v = s.index.logos.iter().find(|v| v.sid == 0x1234).unwrap();
        assert_eq!(std::fs::read(s.logo_path(v)).unwrap(), b"new picture");
        let file = s.logo_path(v);
        assert!(!s.drop_unlisted_logos(0xE1, 0x1234, &[logo_ref("/logo128.png")]));
        assert!(s.drop_unlisted_logos(0xE1, 0x1234, &[logo_ref("/logo600.png")]));
        assert!(s.index.logos.is_empty() && !file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn iso_times() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-08-21T15:51:00+02:00"), Some(1787320260));
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("PT3600S"), Some(3600));
    }
}
