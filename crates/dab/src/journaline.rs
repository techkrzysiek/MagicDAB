//! Journaline (ETSI TS 102 979): a text information service of menus and
//! messages, much like teletext. Every JML object and every block of
//! management data travels as one MSC data group, in X-PAD or in a packet
//! mode sub-channel. The decoder keeps the latest version of every object.

use crate::crc::check_crc16;
use crate::ensemble::UserApp;
use crate::mot::parse_data_group;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;

/// User application type of Journaline in FIG 0/13 (TS 101 756).
pub const UA_TYPE: u16 = 0x44A;

/// Largest data group field, and largest object with its content
/// uncompressed (clauses 5.1 and 8.1.1).
const MAX_OBJECT: usize = 4092;
/// Inflated content is cut off here: room above the limit of the standard,
/// but no deflate bomb.
const MAX_INFLATED: usize = 4 * MAX_OBJECT;
/// Objects kept at most, and their size in all (as JSON for the pages);
/// above either the one received longest ago goes.
const MAX_OBJECTS: usize = 4096;
const MAX_BYTES: usize = 4 << 20;
/// General link targets kept per object at most.
const MAX_HOT_LINKS: usize = 16;

/// Data group types (clause 8.1.1).
const DG_OBJECT: u8 = 0;
const DG_MANAGEMENT: u8 = 6;
/// Management data block type of a TOC block ('T').
const TOC_BLOCK: u8 = 0x54;
/// Compression method of a compressed content section: deflate (RFC 1951).
const DEFLATE: u8 = 0x08;
/// 2000-01-01 00:00 UTC as Unix time.
const EPOCH_2000: i64 = 946_684_800;

/// The Journaline part of the user application data (clause 8.1.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UaParams {
    pub version: u8,
    /// Length of the extended header of every object of the service.
    pub extended_header_len: u8,
    /// Object to show first: the root object 0x0000 unless the signalling
    /// names another.
    pub entry_point: u16,
}

impl UaParams {
    /// Parse the Journaline parameters (after the two X-PAD bytes when the
    /// application is carried in X-PAD). Missing fields are 0.
    pub fn parse(data: &[u8]) -> Self {
        UaParams {
            version: data.first().copied().unwrap_or(0),
            extended_header_len: data.get(1).copied().unwrap_or(0),
            entry_point: if data.len() >= 4 { u16::from_be_bytes([data[2], data[3]]) } else { 0 },
        }
    }

    /// The parameters of a user application of FIG 0/13, None if it is not
    /// Journaline.
    pub fn from_user_app(ua: &UserApp) -> Option<Self> {
        if ua.ua_type != UA_TYPE {
            return None;
        }
        let data = if ua.xpad_app_type.is_some() { ua.data.get(2..).unwrap_or(&[]) } else { &ua.data[..] };
        Some(Self::parse(data))
    }
}

/// Object type (clause 5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    /// A title and links to other objects.
    Menu,
    /// A title and body text.
    PlainText,
    /// A title alone, like a news ticker: shown again at once when it changes.
    TitleOnly,
    /// A title and lines of text, optionally in columns.
    List,
}

impl ObjectKind {
    fn from_bits(t: u8) -> Option<Self> {
        match t {
            1 => Some(ObjectKind::Menu),
            2 => Some(ObjectKind::PlainText),
            3 => Some(ObjectKind::TitleOnly),
            4 => Some(ObjectKind::List),
            _ => None,
        }
    }
}

/// A block of the content of an object, in transmission order. Preferred
/// line breaks are `\n`, preferred word breaks soft hyphens (U+00AD).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Item {
    /// A menu entry: the object it opens and its text.
    Link { target: u16, text: String },
    /// Body text of a plain text message.
    Text { text: String },
    /// A line of a list message, by columns (one cell when it has none).
    Row { cells: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HotLinkKind {
    Object,
    DabUri,
    Url,
    Phone,
    Sms,
}

/// A general link target (data section type 0x03): offered on request,
/// like a "hot button" (clause 5.3.2.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotLink {
    pub kind: HotLinkKind,
    /// The object an `object` link opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u16>,
    /// URI, URL or phone number of the other kinds; for an SMS the number
    /// and the text joined by '+'.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub address: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// A decoded JML object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmlObject {
    pub id: u16,
    pub kind: ObjectKind,
    /// Revision index 0..=7, one up (modulo 8) on every change.
    pub revision: u8,
    /// The ID keeps this kind of content: it may be bookmarked.
    pub static_flag: bool,
    pub title: String,
    pub items: Vec<Item>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hot_links: Vec<HotLink>,
    /// Not to be shown after this time (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    /// Not to be shown this many minutes after it was last received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u16>,
}

/// What the last TOC block said of the whole service (clause 6.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toc {
    pub revision: u8,
    /// Objects the service has now.
    pub objects: u16,
    /// The service is valid this many minutes after the last TOC block
    /// (0: no timeout).
    pub timeout_minutes: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalineStats {
    /// Data groups pushed.
    pub data_groups: u64,
    pub crc_errors: u64,
    /// Data groups or objects that are not valid Journaline: dropped.
    pub rejected: u64,
    /// Objects received again unchanged.
    pub repeats: u64,
    /// New or changed objects stored.
    pub updates: u64,
}

struct Entry {
    raw: Vec<u8>,
    object: JmlObject,
    /// Its size as JSON (MAX_BYTES).
    size: usize,
    /// When it was last received (push counter).
    seen: u64,
}

/// Journaline objects of one service component, from its MSC data groups.
#[derive(Default)]
pub struct JournalineDecoder {
    extended_header_len: usize,
    objects: BTreeMap<u16, Entry>,
    /// Sizes of `objects` added up.
    bytes: usize,
    pushes: u64,
    toc: Option<Toc>,
    pub stats: JournalineStats,
}

impl JournalineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Length of the extended header of the service's objects, from the
    /// user application data (`UaParams`). The header is skipped. Objects
    /// decoded with another length are dropped.
    pub fn set_extended_header_len(&mut self, len: u8) {
        if self.extended_header_len != len as usize {
            self.extended_header_len = len as usize;
            self.objects.clear();
            self.bytes = 0;
        }
    }

    /// Forget all objects and the TOC (another service).
    pub fn reset(&mut self) {
        self.objects.clear();
        self.bytes = 0;
        self.toc = None;
        self.stats = JournalineStats::default();
    }

    /// Process one MSC data group as received (header, field, CRC). Returns
    /// the IDs of objects new, changed or removed (`object()` gives None for
    /// those a TOC block said are gone, or dropped for room).
    pub fn push(&mut self, data_group: &[u8]) -> Vec<u16> {
        self.stats.data_groups += 1;
        self.pushes += 1;
        if data_group.len() >= 2 && data_group[0] & 0x40 != 0 && !check_crc16(data_group) {
            self.stats.crc_errors += 1;
            return Vec::new();
        }
        let Some(dg) = parse_data_group(data_group) else {
            self.stats.rejected += 1;
            return Vec::new();
        };
        // Journaline sends no segments, but one whole object in a single
        // segment does no harm; without a CRC the group must be dropped.
        let segmented = dg.segment.is_some_and(|s| s != 0) || (dg.segment.is_some() && !dg.last);
        if !dg.crc || segmented || dg.data.len() > MAX_OBJECT {
            self.stats.rejected += 1;
            return Vec::new();
        }
        match dg.dg_type {
            DG_OBJECT => self.push_object(dg.data),
            DG_MANAGEMENT => self.push_management(dg.data),
            _ => {
                self.stats.rejected += 1;
                Vec::new()
            }
        }
    }

    pub fn object(&self, id: u16) -> Option<&JmlObject> {
        self.objects.get(&id).map(|e| &e.object)
    }

    /// All objects, by ID.
    pub fn objects(&self) -> impl Iterator<Item = &JmlObject> {
        self.objects.values().map(|e| &e.object)
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn toc(&self) -> Option<Toc> {
        self.toc
    }

    fn push_object(&mut self, raw: &[u8]) -> Vec<u16> {
        if raw.len() < 3 {
            self.stats.rejected += 1;
            return Vec::new();
        }
        let id = u16::from_be_bytes([raw[0], raw[1]]);
        if let Some(e) = self.objects.get_mut(&id) {
            // the revision index wraps after 8 changes: compare the whole object
            if e.raw == raw {
                e.seen = self.pushes;
                self.stats.repeats += 1;
                return Vec::new();
            }
        }
        let Some(object) = parse_object(raw, self.extended_header_len) else {
            self.stats.rejected += 1;
            return Vec::new();
        };
        let mut changed = vec![id];
        let size = serde_json::to_vec(&object).map_or(raw.len(), |v| v.len());
        self.remove(id);
        while self.objects.len() >= MAX_OBJECTS || (self.bytes + size > MAX_BYTES && !self.objects.is_empty()) {
            let Some(oldest) = self.objects.iter().min_by_key(|(_, e)| e.seen).map(|(k, _)| *k) else { break };
            self.remove(oldest);
            changed.push(oldest);
        }
        self.bytes += size;
        self.objects.insert(id, Entry { raw: raw.to_vec(), object, size, seen: self.pushes });
        self.stats.updates += 1;
        changed
    }

    fn push_management(&mut self, block: &[u8]) -> Vec<u16> {
        match block.first() {
            Some(&TOC_BLOCK) => self.push_toc(block),
            // other management data is not defined
            _ => Vec::new(),
        }
    }

    /// A TOC block lists the objects of an ID range in ascending order: the
    /// IDs of the range it does not list are not in the service (clause 6.2.3).
    fn push_toc(&mut self, b: &[u8]) -> Vec<u16> {
        if b.len() < 13 {
            self.stats.rejected += 1;
            return Vec::new();
        }
        let revision = b[1];
        let count = b[2];
        let index = b[3];
        let preceding = u16::from_be_bytes([b[4], b[5]]);
        let objects = u16::from_be_bytes([b[6], b[7]]);
        let timeout_minutes = u16::from_be_bytes([b[8], b[9]]);
        let entity_len = b[10] as usize;
        let start = 13 + u16::from_be_bytes([b[11], b[12]]) as usize;
        if entity_len < 3 || index >= count || start > b.len() {
            self.stats.rejected += 1;
            return Vec::new();
        }
        let listed: Vec<u16> = b[start..].chunks_exact(entity_len).map(|e| u16::from_be_bytes([e[0], e[1]])).collect();
        let Some(&last) = listed.last() else {
            self.stats.rejected += 1;
            return Vec::new();
        };
        self.toc = Some(Toc { revision, objects, timeout_minutes });
        let lo = if index == 0 { 0 } else { preceding as u32 + 1 };
        let hi = if index + 1 == count { 0xFFFF } else { last as u32 };
        let listed: HashSet<u16> = listed.into_iter().collect();
        let gone: Vec<u16> = self.objects.keys().copied().filter(|&id| (lo..=hi).contains(&(id as u32)) && !listed.contains(&id)).collect();
        for id in &gone {
            self.remove(*id);
        }
        gone
    }

    fn remove(&mut self, id: u16) {
        if let Some(e) = self.objects.remove(&id) {
            self.bytes -= e.size;
        }
    }
}

/// Decode an object (standard header, extended header, content section).
/// None for unknown object types and unknown compression.
fn parse_object(raw: &[u8], extended_header_len: usize) -> Option<JmlObject> {
    if raw.len() < 3 {
        return None;
    }
    let desc = raw[2];
    let kind = ObjectKind::from_bits(desc >> 5)?;
    let start = 3 + extended_header_len;
    if start > raw.len() {
        return None;
    }
    let inflated;
    let content = if desc & 0x08 != 0 {
        // a compression method byte, then the compressed content section
        if raw.get(start) != Some(&DEFLATE) {
            return None;
        }
        inflated = inflate(&raw[start + 1..])?;
        &inflated[..]
    } else {
        &raw[start..]
    };
    let mut b = Builder {
        object: JmlObject {
            id: u16::from_be_bytes([raw[0], raw[1]]),
            kind,
            revision: desc & 0x07,
            static_flag: desc & 0x10 != 0,
            title: String::new(),
            items: Vec::new(),
            hot_links: Vec::new(),
            expires: None,
            timeout_minutes: None,
        },
        block: Block::None,
        text: Vec::new(),
        cells: Vec::new(),
        has_title: false,
    };
    tokens(content, |t| b.token(t));
    b.end_block();
    Some(b.object)
}

/// Deflate as RFC 1951 says (a raw stream, 4 KiB window); a zlib stream is
/// taken too.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    fn read_limited(r: impl Read) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        r.take(MAX_INFLATED as u64 + 1).read_to_end(&mut out).ok()?;
        (out.len() <= MAX_INFLATED).then_some(out)
    }
    read_limited(flate2::read::DeflateDecoder::new(data)).or_else(|| read_limited(flate2::read::ZlibDecoder::new(data)))
}

/// Pieces of a content section (clause 5.3).
enum Token<'a> {
    /// A JML code that starts a block, or ends one (0x00).
    Block(u8),
    /// The JML code of a link item with its target object.
    Link(u16),
    /// List column code.
    Column,
    /// Characters (UTF-8, no control codes).
    Text(&'a [u8]),
    LineBreak,
    WordBreak,
    /// The payload of a data section (start plus its continuations).
    DataSection(Vec<u8>),
}

fn tokens<'a>(c: &'a [u8], mut f: impl FnMut(Token<'a>)) {
    let mut i = 0;
    while i < c.len() {
        let b = c[i];
        if b >= 0x20 {
            let end = c[i..].iter().position(|&x| x < 0x20).map_or(c.len(), |p| i + p);
            f(Token::Text(&c[i..end]));
            i = end;
            continue;
        }
        i += 1;
        match b {
            0x02 => {
                if i + 2 > c.len() {
                    return;
                }
                f(Token::Link(u16::from_be_bytes([c[i], c[i + 1]])));
                i += 2;
            }
            0x05 => f(Token::Column),
            0x00..=0x0F => f(Token::Block(b)),
            0x10 => f(Token::LineBreak),
            0x11 => f(Token::WordBreak),
            0x1A | 0x1B => {
                // a length byte (n - 1) and n bytes; a full section (256 bytes)
                // may go on in a continuation right after it
                let mut payload = Vec::new();
                while i < c.len() {
                    let n = c[i] as usize + 1;
                    let end = (i + 1 + n).min(c.len());
                    payload.extend_from_slice(&c[i + 1..end]);
                    i = end;
                    if n < 256 || c.get(i) != Some(&0x1B) {
                        break;
                    }
                    i += 1;
                }
                // (a continuation without its start is skipped)
                if b == 0x1A {
                    f(Token::DataSection(payload));
                }
            }
            // extended code begin/end: one more byte, no extended codes are defined
            0x1C | 0x1D => i += 1,
            // highlight start/stop, end of introductory section, reserved
            // escape codes: one byte, ignored
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Title,
    Link(u16),
    Body,
    ListItem,
    /// A reserved JML code: its content is not shown.
    Other,
}

struct Builder {
    object: JmlObject,
    block: Block,
    text: Vec<u8>,
    cells: Vec<String>,
    has_title: bool,
}

impl Builder {
    fn token(&mut self, t: Token) {
        match t {
            Token::Block(code) => {
                self.end_block();
                self.block = match code {
                    0x01 => Block::Title,
                    0x03 => Block::Body,
                    0x04 => Block::ListItem,
                    0x00 => Block::None,
                    _ => Block::Other,
                };
            }
            Token::Link(target) => {
                self.end_block();
                self.block = Block::Link(target);
            }
            Token::Column => {
                if self.block == Block::ListItem {
                    let cell = take_text(&mut self.text);
                    self.cells.push(cell);
                } else {
                    self.text.push(b' ');
                }
            }
            Token::Text(s) => self.text.extend_from_slice(s),
            Token::LineBreak => self.text.push(b'\n'),
            Token::WordBreak => self.text.extend_from_slice("\u{AD}".as_bytes()),
            Token::DataSection(p) => self.data_section(&p),
        }
    }

    fn end_block(&mut self) {
        let text = take_text(&mut self.text);
        let o = &mut self.object;
        match self.block {
            // exactly one title, the first block
            Block::Title if !self.has_title => {
                o.title = text;
                self.has_title = true;
            }
            Block::Link(target) => o.items.push(Item::Link { target, text }),
            Block::Body if !text.is_empty() => o.items.push(Item::Text { text }),
            Block::ListItem => {
                let mut cells = std::mem::take(&mut self.cells);
                cells.push(text);
                if cells.iter().any(|c| !c.is_empty()) {
                    o.items.push(Item::Row { cells });
                }
            }
            _ => {}
        }
        self.cells.clear();
        self.block = Block::None;
    }

    /// Data sections the decoder uses (clause 5.3.2.2): timeouts and general
    /// link targets. Keywords, macros, speech and location data are skipped.
    fn data_section(&mut self, p: &[u8]) {
        let Some((&ds_type, d)) = p.split_first() else {
            return;
        };
        let o = &mut self.object;
        match ds_type {
            0x01 if d.len() >= 3 => {
                let quarters = ((d[0] as i64) << 16) | ((d[1] as i64) << 8) | d[2] as i64;
                o.expires = Some(EPOCH_2000 + quarters * 900);
            }
            0x02 if d.len() >= 2 => o.timeout_minutes = Some(u16::from_be_bytes([d[0], d[1]])),
            0x03 => {
                if let Some(link) = hot_link(d) {
                    if o.hot_links.len() < MAX_HOT_LINKS {
                        o.hot_links.push(link);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A general link target: link type, address, then optionally an address
/// end marker (0x00) and a label. None for unknown link types.
fn hot_link(d: &[u8]) -> Option<HotLink> {
    let (&link_type, rest) = d.split_first()?;
    let kind = match link_type {
        0 => HotLinkKind::Object,
        1 => HotLinkKind::DabUri,
        2 => HotLinkKind::Url,
        3 => HotLinkKind::Phone,
        4 => HotLinkKind::Sms,
        _ => return None,
    };
    let (target, address, label) = if kind == HotLinkKind::Object {
        // the two byte object ID may hold 0x00 itself
        if rest.len() < 2 {
            return None;
        }
        (Some(u16::from_be_bytes([rest[0], rest[1]])), String::new(), rest.get(3..).unwrap_or(&[]))
    } else {
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let address = String::from_utf8_lossy(&rest[..end]).trim().to_string();
        if address.is_empty() {
            return None;
        }
        (None, address, rest.get(end + 1..).unwrap_or(&[]))
    };
    Some(HotLink { kind, target, address, label: visible_text(label) })
}

/// Text with escape sequences, on one line (a link label).
fn visible_text(c: &[u8]) -> String {
    let mut text = Vec::new();
    tokens(c, |t| match t {
        Token::Text(s) => text.extend_from_slice(s),
        Token::LineBreak | Token::Column => text.push(b' '),
        _ => {}
    });
    take_text(&mut text)
}

fn take_text(text: &mut Vec<u8>) -> String {
    let s = String::from_utf8_lossy(text).trim().to_string();
    text.clear();
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::crc16_ccitt;
    use std::io::Write;

    /// An MSC data group as Journaline sends it: CRC flag, no segment or
    /// user access field, the field, the CRC.
    fn dg(dg_type: u8, field: &[u8]) -> Vec<u8> {
        let mut d = vec![0x40 | dg_type, 0x30];
        d.extend_from_slice(field);
        let crc = crc16_ccitt(&d);
        d.extend_from_slice(&crc.to_be_bytes());
        d
    }

    /// Standard header: ID, type, static flag, compress flag, revision.
    fn header(id: u16, kind: u8, static_flag: bool, revision: u8) -> Vec<u8> {
        let mut h = id.to_be_bytes().to_vec();
        h.push((kind << 5) | ((static_flag as u8) << 4) | revision);
        h
    }

    fn menu() -> Vec<u8> {
        let mut o = header(0x0000, 1, true, 2);
        o.push(0x01);
        o.extend_from_slice("Main menu".as_bytes());
        // a target ID whose bytes look like codes (0x00, 0x10)
        o.extend_from_slice(&[0x02, 0x00, 0x10]);
        o.extend_from_slice("News".as_bytes());
        o.extend_from_slice(&[0x02, 0x10, 0x00]);
        o.extend_from_slice("Weather and roads".as_bytes());
        o.push(0x00);
        o
    }

    #[test]
    fn menu_with_links() {
        let mut d = JournalineDecoder::new();
        assert_eq!(d.push(&dg(0, &menu())), vec![0]);
        let m = d.object(0).unwrap();
        assert_eq!(m.kind, ObjectKind::Menu);
        assert_eq!(m.revision, 2);
        assert!(m.static_flag);
        assert_eq!(m.title, "Main menu");
        assert_eq!(
            m.items,
            vec![Item::Link { target: 0x0010, text: "News".into() }, Item::Link { target: 0x1000, text: "Weather and roads".into() },]
        );
        assert_eq!(d.stats.updates, 1);
    }

    #[test]
    fn plain_text_with_escape_sequences() {
        let mut o = header(0x1000, 2, false, 0);
        o.push(0x01);
        o.extend_from_slice("Über die Brücke".as_bytes());
        o.push(0x03);
        o.extend_from_slice("First paragraph.".as_bytes());
        o.push(0x10);
        o.extend_from_slice("Second ".as_bytes());
        // highlight start/stop, extended code begin/end with their byte
        o.extend_from_slice(&[0x12]);
        o.extend_from_slice("one".as_bytes());
        o.extend_from_slice(&[0x13, 0x1C, 0x05, 0x1D, 0x05]);
        o.extend_from_slice(" with a long".as_bytes());
        o.push(0x11);
        o.extend_from_slice("word.".as_bytes());
        // a data section with padding, not shown
        o.extend_from_slice(&[0x1A, 0x02, 0x00, 0x41, 0x42]);
        o.push(0x14);
        o.extend_from_slice(" Details.".as_bytes());
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &o));
        let m = d.object(0x1000).unwrap();
        assert_eq!(m.kind, ObjectKind::PlainText);
        assert!(!m.static_flag);
        assert_eq!(m.title, "Über die Brücke");
        assert_eq!(m.items, vec![Item::Text { text: "First paragraph.\nSecond one with a long\u{AD}word. Details.".into() }]);
    }

    #[test]
    fn title_only_and_list_with_columns() {
        let mut t = header(0x0020, 3, false, 5);
        t.push(0x01);
        t.extend_from_slice("Ticker line".as_bytes());
        let mut l = header(0x0021, 4, true, 1);
        l.push(0x01);
        l.extend_from_slice("Results".as_bytes());
        for row in [&b"Team A - Team B\x053:0"[..], b"Team C - Team D\x05\x05x", b"No columns"] {
            l.push(0x04);
            l.extend_from_slice(row);
        }
        l.push(0x00);
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &t));
        d.push(&dg(0, &l));
        let t = d.object(0x0020).unwrap();
        assert_eq!((t.kind, t.title.as_str(), t.items.len()), (ObjectKind::TitleOnly, "Ticker line", 0));
        let l = d.object(0x0021).unwrap();
        assert_eq!(l.kind, ObjectKind::List);
        let rows: Vec<Vec<&str>> = l
            .items
            .iter()
            .map(|i| match i {
                Item::Row { cells } => cells.iter().map(|c| c.as_str()).collect(),
                _ => panic!("not a row"),
            })
            .collect();
        assert_eq!(rows, vec![vec!["Team A - Team B", "3:0"], vec!["Team C - Team D", "", "x"], vec!["No columns"]]);
    }

    #[test]
    fn compressed_content() {
        let mut content = vec![0x01];
        content.extend_from_slice("Packed".as_bytes());
        content.push(0x03);
        content.extend_from_slice("Some text ".repeat(40).as_bytes());
        for zlib in [false, true] {
            let packed = if zlib {
                let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(&content).unwrap();
                e.finish().unwrap()
            } else {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(&content).unwrap();
                e.finish().unwrap()
            };
            let mut o = header(0x0030, 2, false, 0);
            o[2] |= 0x08;
            o.push(DEFLATE);
            o.extend_from_slice(&packed);
            let mut d = JournalineDecoder::new();
            assert_eq!(d.push(&dg(0, &o)), vec![0x30], "zlib {zlib}");
            let m = d.object(0x30).unwrap();
            assert_eq!(m.title, "Packed");
            assert_eq!(m.items, vec![Item::Text { text: "Some text ".repeat(40).trim().to_string() }]);
        }
        // another compression method is not known
        let mut o = header(0x0031, 2, false, 0);
        o[2] |= 0x08;
        o.extend_from_slice(&[0x09, 0x01, 0x41]);
        let mut d = JournalineDecoder::new();
        assert!(d.push(&dg(0, &o)).is_empty());
        assert_eq!(d.stats.rejected, 1);
    }

    #[test]
    fn deflate_bomb_is_cut_off() {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&[0x01]).unwrap();
        e.write_all(&vec![b'a'; 1 << 20]).unwrap();
        let packed = e.finish().unwrap();
        assert!(packed.len() < MAX_OBJECT);
        let mut o = header(0x0032, 3, false, 0);
        o[2] |= 0x08;
        o.push(DEFLATE);
        o.extend_from_slice(&packed);
        let mut d = JournalineDecoder::new();
        assert!(d.push(&dg(0, &o)).is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn changes_are_reported_once() {
        let mut d = JournalineDecoder::new();
        let m = menu();
        assert_eq!(d.push(&dg(0, &m)), vec![0]);
        assert!(d.push(&dg(0, &m)).is_empty());
        assert_eq!(d.stats.repeats, 1);
        // a new revision, also after the index wrapped (7 -> 0)
        let mut m7 = m.clone();
        m7[2] = (m7[2] & !0x07) | 7;
        assert_eq!(d.push(&dg(0, &m7)), vec![0]);
        let mut m0 = m.clone();
        m0[2] &= !0x07;
        assert_eq!(d.push(&dg(0, &m0)), vec![0]);
        assert_eq!(d.object(0).unwrap().revision, 0);
        // the same revision with other content still is a change
        let mut other = m0.clone();
        *other.last_mut().unwrap() = 0x02;
        other.extend_from_slice(&[0x00, 0x05]);
        other.extend_from_slice("More".as_bytes());
        assert_eq!(d.push(&dg(0, &other)), vec![0]);
        assert_eq!(d.object(0).unwrap().items.len(), 3);
        assert_eq!(d.stats.updates, 4);
    }

    #[test]
    fn invalid_data_groups_are_dropped() {
        let mut d = JournalineDecoder::new();
        let mut bad = dg(0, &menu());
        bad[5] ^= 0x01;
        assert!(d.push(&bad).is_empty());
        assert_eq!(d.stats.crc_errors, 1);
        // no CRC
        let mut no_crc = vec![0x00, 0x00];
        no_crc.extend_from_slice(&menu());
        assert!(d.push(&no_crc).is_empty());
        // a reserved data group type
        assert!(d.push(&dg(1, &menu())).is_empty());
        // an unknown object type (5)
        let mut o = menu();
        o[2] = (5 << 5) | (o[2] & 0x1F);
        assert!(d.push(&dg(0, &o)).is_empty());
        // a later segment of a segmented object
        let mut seg = vec![0x60, 0x00, 0x00, 0x01];
        seg.extend_from_slice(&menu());
        let crc = crc16_ccitt(&seg);
        seg.extend_from_slice(&crc.to_be_bytes());
        assert!(d.push(&seg).is_empty());
        assert_eq!(d.stats.rejected, 4);
        assert!(d.is_empty());
        // a single last segment 0 holds the whole object
        let mut whole = vec![0x60, 0x00, 0x80, 0x00];
        whole.extend_from_slice(&menu());
        let crc = crc16_ccitt(&whole);
        whole.extend_from_slice(&crc.to_be_bytes());
        assert_eq!(d.push(&whole), vec![0]);
    }

    #[test]
    fn extended_header_is_skipped() {
        let mut o = header(0x0040, 3, false, 0);
        o.extend_from_slice(&[0x01, 0x02]);
        o.push(0x01);
        o.extend_from_slice("After the extension".as_bytes());
        let mut d = JournalineDecoder::new();
        d.set_extended_header_len(2);
        d.push(&dg(0, &o));
        assert_eq!(d.object(0x40).unwrap().title, "After the extension");
    }

    #[test]
    fn data_sections() {
        let mut o = header(0x0050, 2, false, 0);
        o.push(0x01);
        // absolute timeout: 2 days after 2000-01-01 in quarter hours
        o.extend_from_slice(&[0x1A, 0x03, 0x01, 0x00, 0x00, 0xC0]);
        // relative timeout: 90 minutes
        o.extend_from_slice(&[0x1A, 0x02, 0x02, 0x00, 0x5A]);
        // links to an object (ID with a 0x00 byte), with and without a label
        o.extend_from_slice(&[0x1A, 0x08, 0x03, 0x00, 0x00, 0x07, 0x00]);
        o.extend_from_slice("Back".as_bytes());
        o.extend_from_slice(&[0x1A, 0x03, 0x03, 0x00, 0x00, 0x12]);
        // a URL with a label holding a line break
        let mut url = vec![0x03, 0x02];
        url.extend_from_slice("https://example.org/a".as_bytes());
        url.push(0x00);
        url.extend_from_slice("More".as_bytes());
        url.push(0x10);
        url.extend_from_slice("online".as_bytes());
        o.push(0x1A);
        o.push(url.len() as u8 - 1);
        o.extend_from_slice(&url);
        // an unknown link type is ignored
        o.extend_from_slice(&[0x1A, 0x02, 0x03, 0x09, 0x41]);
        o.extend_from_slice("Title".as_bytes());
        o.push(0x03);
        o.extend_from_slice("Body".as_bytes());
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &o));
        let m = d.object(0x50).unwrap();
        assert_eq!(m.title, "Title");
        assert_eq!(m.expires, Some(EPOCH_2000 + 2 * 86400));
        assert_eq!(m.timeout_minutes, Some(90));
        assert_eq!(
            m.hot_links,
            vec![
                HotLink { kind: HotLinkKind::Object, target: Some(0x0007), address: String::new(), label: "Back".into() },
                HotLink { kind: HotLinkKind::Object, target: Some(0x0012), address: String::new(), label: String::new() },
                HotLink { kind: HotLinkKind::Url, target: None, address: "https://example.org/a".into(), label: "More online".into() },
            ]
        );
    }

    #[test]
    fn long_data_section_goes_on_in_a_continuation() {
        let mut o = header(0x0060, 3, false, 0);
        o.push(0x01);
        o.extend_from_slice("Before ".as_bytes());
        // 262 bytes of payload: padding type, then filler that looks like text and codes
        let payload: Vec<u8> = std::iter::once(0x00).chain((0..261).map(|i| [b'x', 0x01, 0x02][i % 3])).collect();
        o.extend_from_slice(&[0x1A, 0xFF]);
        o.extend_from_slice(&payload[..256]);
        o.extend_from_slice(&[0x1B, 0x05]);
        o.extend_from_slice(&payload[256..]);
        o.extend_from_slice("after".as_bytes());
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &o));
        assert_eq!(d.object(0x60).unwrap().title, "Before after");
    }

    #[test]
    fn reserved_jml_codes_hide_their_block() {
        let mut o = header(0x0070, 2, false, 0);
        o.push(0x01);
        o.extend_from_slice("Title".as_bytes());
        o.push(0x03);
        o.extend_from_slice("Shown".as_bytes());
        o.push(0x09);
        o.extend_from_slice("Hidden".as_bytes());
        o.push(0x01);
        o.extend_from_slice("Second title".as_bytes());
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &o));
        let m = d.object(0x70).unwrap();
        assert_eq!(m.title, "Title");
        assert_eq!(m.items, vec![Item::Text { text: "Shown".into() }]);
    }

    fn toc(count: u8, index: u8, preceding: u16, ids: &[u16]) -> Vec<u8> {
        let mut b = vec![TOC_BLOCK, 7, count, index];
        b.extend_from_slice(&preceding.to_be_bytes());
        b.extend_from_slice(&(ids.len() as u16).to_be_bytes());
        b.extend_from_slice(&30u16.to_be_bytes());
        b.push(3);
        b.extend_from_slice(&0u16.to_be_bytes());
        for id in ids {
            b.extend_from_slice(&id.to_be_bytes());
            b.push(0x20);
        }
        b
    }

    fn title_only(id: u16) -> Vec<u8> {
        let mut o = header(id, 3, false, 0);
        o.push(0x01);
        o.extend_from_slice("Ticker".as_bytes());
        o
    }

    #[test]
    fn toc_removes_objects_no_longer_in_the_service() {
        let mut d = JournalineDecoder::new();
        for id in [0x0000, 0x0005, 0x0010, 0x0020, 0x0030] {
            d.push(&dg(0, &title_only(id)));
        }
        // first of two fragments: IDs 0x0000..=0x0010, 0x0005 is gone
        assert_eq!(d.push(&dg(6, &toc(2, 0, 0, &[0x0000, 0x0010]))), vec![0x0005]);
        assert_eq!(d.toc(), Some(Toc { revision: 7, objects: 2, timeout_minutes: 30 }));
        // the last fragment covers everything above the preceding ID: 0x0030 is gone
        assert_eq!(d.push(&dg(6, &toc(2, 1, 0x0010, &[0x0020]))), vec![0x0030]);
        let ids: Vec<u16> = d.objects().map(|o| o.id).collect();
        assert_eq!(ids, vec![0x0000, 0x0010, 0x0020]);
        // other management data is ignored
        assert!(d.push(&dg(6, &[0x41, 0x00])).is_empty());
    }

    #[test]
    fn oldest_object_goes_when_full() {
        let mut d = JournalineDecoder::new();
        for id in 0..MAX_OBJECTS as u16 {
            d.push(&dg(0, &title_only(id)));
        }
        // received again: no longer the oldest
        d.push(&dg(0, &title_only(0)));
        assert_eq!(d.push(&dg(0, &title_only(0x2000))), vec![0x2000, 1]);
        assert_eq!(d.len(), MAX_OBJECTS);
        assert!(d.object(0).is_some());
        assert!(d.object(1).is_none());
        assert!(d.object(0x2000).is_some());
    }

    #[test]
    fn user_application_data() {
        assert_eq!(UaParams::parse(&[0x00, 0x00]), UaParams::default());
        assert_eq!(UaParams::parse(&[0x00, 0x02, 0x12, 0x34]), UaParams { version: 0, extended_header_len: 2, entry_point: 0x1234 });
        let mut ua = UserApp {
            ua_type: UA_TYPE,
            xpad_app_type: Some(4),
            dscty: Some(5),
            dg_flag: true,
            data: vec![0x04, 0x05, 0x00, 0x00, 0x00, 0x10],
        };
        assert_eq!(UaParams::from_user_app(&ua).map(|p| p.entry_point), Some(0x0010));
        ua.xpad_app_type = None;
        ua.data = vec![0x00, 0x00];
        assert_eq!(UaParams::from_user_app(&ua), Some(UaParams::default()));
        ua.ua_type = 0x002;
        assert_eq!(UaParams::from_user_app(&ua), None);
    }

    #[test]
    fn json_of_an_object() {
        let mut d = JournalineDecoder::new();
        d.push(&dg(0, &menu()));
        let v = serde_json::to_value(d.object(0).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "id": 0,
                "kind": "menu",
                "revision": 2,
                "staticFlag": true,
                "title": "Main menu",
                "items": [
                    {"type": "link", "target": 16, "text": "News"},
                    {"type": "link", "target": 4096, "text": "Weather and roads"}
                ]
            })
        );
        let mut l = header(0x0021, 4, false, 1);
        l.extend_from_slice(&[0x01, b'L', 0x04, b'a', 0x05, b'b']);
        d.push(&dg(0, &l));
        let v = serde_json::to_value(d.object(0x21).unwrap()).unwrap();
        assert_eq!(v["kind"], "list");
        assert_eq!(v["items"][0], serde_json::json!({"type": "row", "cells": ["a", "b"]}));
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut d = JournalineDecoder::new();
        let mut x: u32 = 1;
        for len in 0..600 {
            let mut field = Vec::with_capacity(len);
            for _ in 0..len {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                field.push(x as u8);
            }
            if len >= 3 {
                // a known object type, sometimes compressed
                field[2] = (field[2] & 0x1F) | 0x20;
            }
            d.push(&dg(0, &field));
            d.push(&dg(6, &field));
        }
    }
}
