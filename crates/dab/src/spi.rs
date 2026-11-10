//! Service and Programme Information: decoder of the binary encoding of the
//! SPI XML documents (ETSI TS 102 371) into a small element tree.

use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: Option<String>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// All descendants with the given element name (depth first).
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            }
            c.find_all(name, out);
        }
    }

    /// Text of the first CDATA child (names, descriptions).
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref().or_else(|| self.children.iter().find_map(|c| if c.name == "#text" { c.text.as_deref() } else { None }))
    }

    /// Render as XML (for debugging and the data storage).
    pub fn to_xml(&self, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        if self.name == "#text" {
            out.push_str(&pad);
            out.push_str(&xml_escape(self.text.as_deref().unwrap_or("")));
            out.push('\n');
            return;
        }
        out.push_str(&format!("{pad}<{}", self.name));
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", xml_escape(v)));
        }
        if self.children.is_empty() && self.text.is_none() {
            out.push_str("/>\n");
            return;
        }
        out.push_str(">\n");
        if let Some(t) = &self.text {
            out.push_str(&format!("{pad}  {}\n", xml_escape(t)));
        }
        for c in &self.children {
            c.to_xml(out, indent + 2);
        }
        out.push_str(&format!("{pad}</{}>\n", self.name));
    }
}

/// Text for XML; control characters (not allowed in XML 1.0) are left out, so
/// that a document with an odd string stays well-formed.
fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A genre as TS 102 371 encodes it: the classification scheme (low 4 bits of
/// the first byte), then a byte a level; written as the TV-Anytime URN of TS
/// 102 818 (the year of the scheme is not sent: 2005). Some encoders send
/// the URN itself as text ("urn:tva:..."): taken as it is.
fn genre(d: &[u8]) -> Option<String> {
    if d.starts_with(b"urn:") {
        return std::str::from_utf8(d).ok().filter(|s| s.chars().all(|c| c.is_ascii_graphic())).map(str::to_string);
    }
    let cs = match d.first()? & 0x0F {
        1 => "IntentionCS",
        2 => "FormatCS",
        3 => "ContentCS",
        4 => "IntendedAudienceCS",
        5 => "OriginationCS",
        6 => "ContentAlertCS",
        7 => "MediaTypeCS",
        8 => "AtmosphereCS",
        _ => return None,
    };
    let levels: Vec<String> = d[1..].iter().map(|l| l.to_string()).collect();
    (!levels.is_empty()).then(|| format!("urn:tva:metadata:cs:{cs}:2005:{}", levels.join(".")))
}

fn element_name(tag: u8) -> Option<&'static str> {
    Some(match tag {
        0x02 => "epg",
        0x03 => "serviceInformation",
        0x10 => "shortName",
        0x11 => "mediumName",
        0x12 => "longName",
        0x13 => "mediaDescription",
        0x14 => "genre",
        0x16 => "keywords",
        0x17 => "memberOf",
        0x18 => "link",
        0x19 => "location",
        0x1A => "shortDescription",
        0x1B => "longDescription",
        0x1C => "programme",
        0x20 => "programmeGroups",
        0x21 => "schedule",
        0x23 => "programmeGroup",
        0x24 => "scope",
        0x25 => "serviceScope",
        0x26 => "ensemble",
        0x28 => "service",
        0x29 => "serviceID",
        0x2B => "multimedia",
        0x2C => "time",
        0x2D => "bearer",
        0x2E => "programmeEvent",
        0x2F => "relativeTime",
        0x31 => "radiodns",
        0x32 => "geolocation",
        0x33 => "country",
        0x34 => "point",
        0x35 => "polygon",
        0x36 => "onDemand",
        0x37 => "presentationTime",
        0x38 => "acquisitionTime",
        _ => return None,
    })
}

enum Kind {
    Str,
    Genre,
    StrTokens,
    Time,
    Duration,
    U16,
    U24,
    Bearer,
    EnsembleId,
    MultimediaType,
    Enum(&'static [(u8, &'static str)]),
}

fn attribute(element: &str, tag: u8) -> Option<(&'static str, Kind)> {
    use Kind::*;
    Some(match (element, tag) {
        ("serviceInformation", 0x80) | ("service", 0x80) | ("programmeGroups", 0x80) | ("schedule", 0x80) => ("version", U16),
        ("serviceInformation", 0x81) | ("programmeGroups", 0x81) | ("schedule", 0x81) => ("creationTime", Time),
        ("serviceInformation", 0x82) | ("programmeGroups", 0x82) | ("schedule", 0x82) => ("originator", Str),
        ("serviceInformation", 0x83) => ("serviceProvider", Str),
        ("ensemble", 0x80) => ("id", EnsembleId),
        ("multimedia", 0x80) => ("mimeValue", Str),
        ("multimedia", 0x81) => ("xml:lang", Str),
        ("multimedia", 0x82) => ("url", StrTokens),
        ("multimedia", 0x83) => ("type", MultimediaType),
        ("multimedia", 0x84) => ("width", U16),
        ("multimedia", 0x85) => ("height", U16),
        ("shortName" | "mediumName" | "longName" | "shortDescription" | "longDescription" | "keywords", 0x80) => ("xml:lang", Str),
        ("genre", 0x80) => ("href", Genre),
        ("genre", 0x81) => ("type", Enum(&[(1, "main"), (2, "secondary"), (3, "other")])),
        ("link", 0x80) => ("uri", StrTokens),
        ("link", 0x81) => ("mimeValue", Str),
        ("link", 0x82) => ("xml:lang", Str),
        ("link", 0x83) => ("description", StrTokens),
        ("link", 0x84) => ("expiryTime", Time),
        ("memberOf", 0x80) => ("id", StrTokens),
        ("memberOf", 0x81) => ("shortId", U24),
        ("memberOf", 0x82) => ("index", U16),
        ("programme" | "programmeEvent", 0x80) => ("id", StrTokens),
        ("programme" | "programmeEvent", 0x81) => ("shortId", U24),
        ("programme" | "programmeEvent", 0x82) => ("version", U16),
        ("programme" | "programmeEvent", 0x83) => ("recommendation", Enum(&[(1, "no"), (2, "yes")])),
        ("programme" | "programmeEvent", 0x84) => ("broadcast", Enum(&[(1, "on-air"), (2, "off-air")])),
        ("programme" | "programmeEvent", 0x86) => ("xml:lang", Str),
        ("programmeGroup", 0x80) => ("id", StrTokens),
        ("programmeGroup", 0x81) => ("shortId", U24),
        ("programmeGroup", 0x82) => ("version", U16),
        ("programmeGroup", 0x83) => (
            "type",
            Enum(&[
                (2, "series"),
                (3, "show"),
                (4, "programConcept"),
                (5, "magazine"),
                (6, "programCompilation"),
                (7, "otherCollection"),
                (8, "otherChoice"),
                (9, "topic"),
            ]),
        ),
        ("programmeGroup", 0x84) => ("numOfItems", U16),
        ("scope", 0x80) => ("startTime", Time),
        ("scope", 0x81) => ("stopTime", Time),
        ("serviceScope", 0x80) => ("id", Bearer),
        ("bearer" | "serviceID", 0x80) => ("id", Bearer),
        ("bearer" | "serviceID", 0x82) => ("url", Str),
        ("time" | "relativeTime", 0x80) => ("time", Time),
        ("time" | "relativeTime", 0x81) => ("duration", Duration),
        ("time" | "relativeTime", 0x82) => ("actualTime", Time),
        ("time" | "relativeTime", 0x83) => ("actualDuration", Duration),
        ("radiodns", 0x80) => ("fqdn", StrTokens),
        ("radiodns", 0x81) => ("serviceIdentifier", StrTokens),
        ("geolocation", 0x80) => ("xml:id", StrTokens),
        ("geolocation", 0x81) => ("ref", StrTokens),
        ("presentationTime" | "acquisitionTime", 0x80) => ("start", Time),
        ("presentationTime" | "acquisitionTime", 0x81) => ("end", Time),
        ("presentationTime", 0x82) => ("duration", Duration),
        _ => return None,
    })
}

struct Decoder {
    tokens: HashMap<u8, String>,
    ueid: u32,
    default_lang: Option<String>,
    /// Nesting of the element being parsed.
    depth: usize,
}

/// Real documents nest a few levels; deeper ones are skipped (a hostile
/// object of a few kilobytes would otherwise overflow the stack).
const MAX_DEPTH: usize = 32;

/// Format a binary time point as ISO 8601.
pub fn time_point(d: &[u8]) -> Option<String> {
    if d.len() < 4 {
        return None;
    }
    let v = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
    let mjd = (v >> 14) & 0x1FFFF;
    let lto_flag = v & (1 << 12) != 0;
    let utc_long = v & (1 << 11) != 0;
    let hours = (v >> 6) & 0x1F;
    let minutes = v & 0x3F;
    let mut i = 4;
    let (mut sec, mut _ms) = (0, 0);
    if utc_long && d.len() >= 6 {
        let w = u16::from_be_bytes([d[4], d[5]]);
        sec = w >> 10;
        _ms = w & 0x3FF;
        i = 6;
    }
    let mut tz = "Z".to_string();
    if lto_flag && d.len() > i {
        let lto = d[i];
        let sign = if lto & 0x20 != 0 { '-' } else { '+' };
        let half = (lto & 0x1F) as u32;
        tz = format!("{sign}{:02}:{:02}", half / 2, (half % 2) * 30);
    }
    // MJD to calendar date
    let jd = mjd as i64 + 2400001;
    let l = jd + 68569;
    let n = 4 * l / 146097;
    let l = l - (146097 * n + 3) / 4;
    let i4 = 4000 * (l + 1) / 1461001;
    let l = l - 1461 * i4 / 4 + 31;
    let j = 80 * l / 2447;
    let day = l - 2447 * j / 80;
    let l = j / 11;
    let month = j + 2 - 12 * l;
    let year = 100 * (n - 49) + i4 + l;
    Some(format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{sec:02}{tz}"))
}

impl Decoder {
    fn string(&self, d: &[u8], tokens: bool) -> String {
        let s = String::from_utf8_lossy(d).to_string();
        if !tokens || self.tokens.is_empty() {
            return s;
        }
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            let code = c as u32;
            if (1..=0x13).contains(&code) {
                if let Some(t) = self.tokens.get(&(code as u8)) {
                    out.push_str(t);
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    /// contentId / bearer URI: flags (b6 ensemble present, b4 long SId,
    /// b3..b0 SCIdS), [ECC, EId], SId (16 or 32 bit).
    fn bearer(&self, d: &[u8]) -> Option<String> {
        if d.len() < 3 {
            return None;
        }
        let flags = d[0];
        let long_sid = flags & 0x10 != 0;
        let scids = flags & 0x0F;
        let mut i = 1;
        let mut ecc = (self.ueid >> 16) as u8;
        let mut eid = self.ueid as u16;
        if flags & 0x40 != 0 {
            if d.len() < 4 {
                return None;
            }
            ecc = d[1];
            eid = u16::from_be_bytes([d[2], d[3]]);
            i = 4;
        }
        if long_sid {
            if d.len() < i + 4 {
                return None;
            }
            let sid = u32::from_be_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
            Some(format!("dab:{:x}{:02x}.{:04x}.{:08x}.{}", (sid >> 20) & 0xF, ecc, eid, sid, scids))
        } else {
            if d.len() < i + 2 {
                return None;
            }
            let sid = u16::from_be_bytes([d[i], d[i + 1]]);
            Some(format!("dab:{:x}{:02x}.{:04x}.{:04x}.{}", (sid >> 12) & 0xF, ecc, eid, sid, scids))
        }
    }

    fn parse(&mut self, d: &[u8], parent: &mut Element) -> usize {
        if d.len() < 2 {
            return d.len();
        }
        let tag = d[0];
        let mut len = d[1] as usize;
        let mut hdr = 2;
        if len == 0xFE {
            if d.len() < 4 {
                return d.len();
            }
            len = u16::from_be_bytes([d[2], d[3]]) as usize;
            hdr = 4;
        } else if len == 0xFF {
            if d.len() < 5 {
                return d.len();
            }
            len = ((d[2] as usize) << 16) | ((d[3] as usize) << 8) | d[4] as usize;
            hdr = 5;
        }
        if d.len() < hdr + len {
            return d.len();
        }
        let body = &d[hdr..hdr + len];
        if tag >= 0x80 {
            // attribute of the parent element
            if let Some((name, kind)) = attribute(&parent.name, tag) {
                let value = match kind {
                    Kind::Str => Some(self.string(body, false)),
                    Kind::StrTokens => Some(self.string(body, true)),
                    Kind::Genre => genre(body),
                    Kind::Time => time_point(body),
                    Kind::Duration => (body.len() >= 2).then(|| format!("PT{}S", u16::from_be_bytes([body[0], body[1]]))),
                    Kind::U16 => (body.len() >= 2).then(|| u16::from_be_bytes([body[0], body[1]]).to_string()),
                    Kind::U24 => {
                        (body.len() >= 3).then(|| (((body[0] as u32) << 16) | ((body[1] as u32) << 8) | body[2] as u32).to_string())
                    }
                    Kind::Bearer => self.bearer(body),
                    Kind::EnsembleId => {
                        (body.len() >= 3).then(|| format!("{:02x}.{:04x}", body[0], u16::from_be_bytes([body[1], body[2]])))
                    }
                    Kind::MultimediaType => body.first().map(|t| {
                        match t {
                            0x02 => "logo_unrestricted",
                            0x03 => "logo_mono_square",
                            0x04 => "logo_colour_square",
                            0x05 => "logo_mono_rectangle",
                            0x06 => "logo_colour_rectangle",
                            _ => "unknown",
                        }
                        .to_string()
                    }),
                    Kind::Enum(values) => body.first().and_then(|v| values.iter().find(|(k, _)| k == v)).map(|(_, s)| s.to_string()),
                };
                if let Some(v) = value {
                    parent.attrs.push((name.to_string(), v));
                }
            }
            return hdr + len;
        }
        match tag {
            0x01 => {
                parent.children.push(Element { name: "#text".into(), text: Some(self.string(body, true)), ..Default::default() });
            }
            0x04 => {
                // token table
                let mut i = 0;
                while i + 2 <= body.len() {
                    let id = body[i];
                    let l = body[i + 1] as usize;
                    if i + 2 + l > body.len() {
                        break;
                    }
                    self.tokens.insert(id, String::from_utf8_lossy(&body[i + 2..i + 2 + l]).to_string());
                    i += 2 + l;
                }
            }
            0x05 => {
                if let Some(b) = self.bearer(body) {
                    parent.attrs.push(("defaultContentId".into(), b));
                }
            }
            0x06 => {
                self.default_lang = Some(self.string(body, false));
            }
            _ => {
                let Some(name) = element_name(tag) else { return hdr + len };
                if self.depth >= MAX_DEPTH {
                    return hdr + len;
                }
                self.depth += 1;
                let mut el = Element { name: name.to_string(), ..Default::default() };
                let mut i = 0;
                while i < body.len() {
                    let used = self.parse(&body[i..], &mut el);
                    if used == 0 {
                        break;
                    }
                    i += used;
                }
                self.depth -= 1;
                parent.children.push(el);
            }
        }
        hdr + len
    }
}

/// Decode a binary encoded SPI object. `ueid` (ECC << 16 | EId) is used for
/// bearer URIs that do not carry an ensemble id.
pub fn decode(data: &[u8], ueid: u32) -> Option<Element> {
    let mut root = Element { name: "#root".into(), ..Default::default() };
    let mut dec = Decoder { tokens: HashMap::new(), ueid, default_lang: None, depth: 0 };
    let mut i = 0;
    while i < data.len() {
        let used = dec.parse(&data[i..], &mut root);
        if used == 0 {
            break;
        }
        i += used;
    }
    let doc = root.children.into_iter().next()?;
    Some(doc)
}

/// A logo reference of a service found in an SPI service information document.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogoRef {
    pub url: String,
    pub mime: Option<String>,
    pub width: u16,
    pub height: u16,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    /// Bearer URIs ("dab:e2c.4a5c.5e1d.0").
    pub bearers: Vec<String>,
    pub short_name: Option<String>,
    pub medium_name: Option<String>,
    pub long_name: Option<String>,
    pub description: Option<String>,
    pub logos: Vec<LogoRef>,
    pub links: Vec<String>,
    pub genres: Vec<String>,
    pub radiodns: Option<(String, String)>,
}

/// A logo a `multimedia` element names (the legacy types carry their size in the type).
fn logo_ref(m: &Element) -> Option<LogoRef> {
    let url = m.attr("url")?;
    let (mut w, mut h) =
        (m.attr("width").and_then(|v| v.parse().ok()).unwrap_or(0), m.attr("height").and_then(|v| v.parse().ok()).unwrap_or(0));
    match m.attr("type") {
        Some("logo_colour_square") | Some("logo_mono_square") if w == 0 => (w, h) = (32, 32),
        Some("logo_colour_rectangle") | Some("logo_mono_rectangle") if w == 0 => (w, h) = (112, 32),
        _ => {}
    }
    Some(LogoRef {
        url: url.to_string(),
        mime: m.attr("mimeValue").map(|s| s.to_string()),
        width: w,
        height: h,
        kind: m.attr("type").map(|s| s.to_string()),
    })
}

/// The logos of the ensembles a service information document describes: the
/// `mediaDescription` of an `ensemble` itself (not of its services), with its
/// ECC and EId (`id="e3.2e03"`).
pub fn ensemble_logos(doc: &Element) -> Vec<(u8, u16, LogoRef)> {
    let mut ensembles = Vec::new();
    doc.find_all("ensemble", &mut ensembles);
    let mut out = Vec::new();
    for e in ensembles {
        let Some((ecc, eid)) = e
            .attr("id")
            .and_then(|id| id.split_once('.'))
            .and_then(|(c, i)| Some((u8::from_str_radix(c, 16).ok()?, u16::from_str_radix(i, 16).ok()?)))
        else {
            continue;
        };
        for md in e.children.iter().filter(|c| c.name == "mediaDescription") {
            let mut mm = Vec::new();
            md.find_all("multimedia", &mut mm);
            out.extend(mm.into_iter().filter_map(logo_ref).map(|l| (ecc, eid, l)));
        }
    }
    out
}

/// Extract services (names, logos, bearers) from a service information document.
pub fn services(doc: &Element) -> Vec<ServiceInfo> {
    let mut list = Vec::new();
    doc.find_all("service", &mut list);
    list.into_iter()
        .map(|s| {
            let mut info = ServiceInfo::default();
            let mut bearers = Vec::new();
            s.find_all("bearer", &mut bearers);
            s.find_all("serviceID", &mut bearers);
            info.bearers = bearers.iter().filter_map(|b| b.attr("id")).filter(|id| id.starts_with("dab:")).map(|s| s.to_string()).collect();
            info.short_name = s.child("shortName").and_then(|e| e.text()).map(|s| s.to_string());
            info.medium_name = s.child("mediumName").and_then(|e| e.text()).map(|s| s.to_string());
            info.long_name = s.child("longName").and_then(|e| e.text()).map(|s| s.to_string());
            let mut mm = Vec::new();
            s.find_all("multimedia", &mut mm);
            info.logos = mm.into_iter().filter_map(logo_ref).collect();
            let mut desc = Vec::new();
            s.find_all("shortDescription", &mut desc);
            info.description = desc.first().and_then(|e| e.text()).map(|s| s.to_string());
            let mut links = Vec::new();
            s.find_all("link", &mut links);
            info.links = links.iter().filter_map(|l| l.attr("uri")).map(|s| s.to_string()).collect();
            let mut genres = Vec::new();
            s.find_all("genre", &mut genres);
            info.genres = genres.iter().filter_map(|g| g.text().or(g.attr("href"))).map(|s| s.to_string()).collect();
            if let Some(r) = s.child("radiodns") {
                if let (Some(f), Some(i)) = (r.attr("fqdn"), r.attr("serviceIdentifier")) {
                    info.radiodns = Some((f.to_string(), i.to_string()));
                }
            }
            info
        })
        .collect()
}

/// Parse "dab:<gcc>.<eid>.<sid>.<scids>" into (ecc, eid, sid, scids).
pub fn parse_bearer(uri: &str) -> Option<(u8, u16, u32, u8)> {
    let rest = uri.strip_prefix("dab:")?;
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let gcc = parts[0];
    let ecc = u8::from_str_radix(gcc.get(1..3)?, 16).ok()?;
    let eid = u16::from_str_radix(parts[1], 16).ok()?;
    let sid = u32::from_str_radix(parts[2], 16).ok()?;
    let scids = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((ecc, eid, sid, scids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensemble_logos_of_service_information() {
        let el = |name: &str, attrs: &[(&str, &str)], children: Vec<Element>| Element {
            name: name.into(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            children,
            text: None,
        };
        let md = |attrs: &[(&str, &str)]| el("mediaDescription", &[], vec![el("multimedia", attrs, vec![])]);
        let doc = el(
            "serviceInformation",
            &[],
            vec![el(
                "ensemble",
                &[("id", "e3.4c1d")],
                vec![
                    md(&[("url", "a1"), ("type", "logo_colour_square")]),
                    md(&[("url", "a2"), ("mimeValue", "image/png"), ("width", "128"), ("height", "128")]),
                    // (a service of the ensemble: its logo is its own)
                    el("service", &[], vec![md(&[("url", "s1"), ("width", "128"), ("height", "128")])]),
                ],
            )],
        );
        let l = ensemble_logos(&doc);
        assert_eq!(
            l.iter().map(|(ecc, eid, r)| (*ecc, *eid, r.url.as_str(), r.width)).collect::<Vec<_>>(),
            [(0xE3, 0x4C1D, "a1", 32), (0xE3, 0x4C1D, "a2", 128)]
        );
    }

    #[test]
    fn genres_and_well_formed_text() {
        // ContentCS 3.6.8 (a 2005 scheme of TV-Anytime), as bytes
        assert_eq!(genre(&[0x03, 3, 6, 8]).as_deref(), Some("urn:tva:metadata:cs:ContentCS:2005:3.6.8"));
        assert_eq!(genre(&[0x02, 1, 2]).as_deref(), Some("urn:tva:metadata:cs:FormatCS:2005:1.2"));
        assert_eq!(genre(&[0x0F, 1]), None);
        assert_eq!(genre(&[0x03]), None);
        // (the URN as text, as some encoders send it, is no list of levels)
        assert_eq!(genre(b"urn:tva:metadata:cs:ContentCS:2004:3.6.4.14").as_deref(), Some("urn:tva:metadata:cs:ContentCS:2004:3.6.4.14"));
        assert_eq!(genre(b"urn:tva\x01"), None);
        // control characters would make the document not well-formed
        assert_eq!(xml_escape("a\u{3}\u{6}b<c>\n"), "ab&lt;c&gt;\n");
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        // 100000 nested <service> elements in 500 kB
        let n = 100_000;
        let mut data = Vec::with_capacity(5 * n);
        for k in 0..n {
            let len = 5 * (n - 1 - k);
            data.extend_from_slice(&[0x28, 0xFF, (len >> 16) as u8, (len >> 8) as u8, len as u8]);
        }
        let doc = decode(&data, 0).unwrap();
        let mut depth = 1;
        let mut el = &doc;
        while let Some(c) = el.children.first() {
            el = c;
            depth += 1;
        }
        assert_eq!(depth, MAX_DEPTH);
        let mut xml = String::new();
        doc.to_xml(&mut xml, 0);
    }

    #[test]
    fn mjd_to_date() {
        // MJD 61273 = 2026-08-21
        let mjd: u32 = 61273;
        let v: u32 = (mjd << 14) | (15 << 6) | 51;
        let s = time_point(&v.to_be_bytes()).unwrap();
        assert_eq!(s, "2026-08-21T15:51:00Z");
    }

    #[test]
    fn bearer_parse() {
        assert_eq!(parse_bearer("dab:2e2.4a5c.5e1d.0"), Some((0xE2, 0x4A5C, 0x5E1D, 0)));
    }
}
