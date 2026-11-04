//! Dynamic Label (EN 300 401 clause 7.4.5.2) and Dynamic Label Plus (TS 102 980).

use crate::charset;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DlPlusTag {
    pub content_type: u8,
    pub name: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicLabel {
    pub text: String,
    pub tags: Vec<DlPlusTag>,
    pub item_toggle: bool,
    pub item_running: bool,
}

pub fn dl_plus_name(ct: u8) -> &'static str {
    const NAMES: [&str; 64] = [
        "DUMMY",
        "ITEM.TITLE",
        "ITEM.ALBUM",
        "ITEM.TRACKNUMBER",
        "ITEM.ARTIST",
        "ITEM.COMPOSITION",
        "ITEM.MOVEMENT",
        "ITEM.CONDUCTOR",
        "ITEM.COMPOSER",
        "ITEM.BAND",
        "ITEM.COMMENT",
        "ITEM.GENRE",
        "INFO.NEWS",
        "INFO.NEWS.LOCAL",
        "INFO.STOCKMARKET",
        "INFO.SPORT",
        "INFO.LOTTERY",
        "INFO.HOROSCOPE",
        "INFO.DAILY_DIVERSION",
        "INFO.HEALTH",
        "INFO.EVENT",
        "INFO.SCENE",
        "INFO.CINEMA",
        "INFO.STUPIDITY_MACHINE",
        "INFO.DATE_TIME",
        "INFO.WEATHER",
        "INFO.TRAFFIC",
        "INFO.ALARM",
        "INFO.ADVERTISEMENT",
        "INFO.URL",
        "INFO.OTHER",
        "STATIONNAME.SHORT",
        "STATIONNAME.LONG",
        "PROGRAMME.NOW",
        "PROGRAMME.NEXT",
        "PROGRAMME.PART",
        "PROGRAMME.HOST",
        "PROGRAMME.EDITORIAL_STAFF",
        "PROGRAMME.FREQUENCY",
        "PROGRAMME.HOMEPAGE",
        "PROGRAMME.SUBCHANNEL",
        "PHONE.HOTLINE",
        "PHONE.STUDIO",
        "PHONE.OTHER",
        "SMS.STUDIO",
        "SMS.OTHER",
        "EMAIL.HOTLINE",
        "EMAIL.STUDIO",
        "EMAIL.OTHER",
        "MMS.OTHER",
        "CHAT",
        "CHAT.CENTER",
        "VOTE.QUESTION",
        "VOTE.CENTRE",
        "RFU",
        "RFU",
        "PRIVATE.1",
        "PRIVATE.2",
        "PRIVATE.3",
        "DESCRIPTOR.PLACE",
        "DESCRIPTOR.APPOINTMENT",
        "DESCRIPTOR.IDENTIFIER",
        "DESCRIPTOR.PURCHASE",
        "DESCRIPTOR.GET_DATA",
    ];
    NAMES[(ct & 0x3F) as usize]
}

#[derive(Default)]
pub struct DlsDecoder {
    toggle: Option<bool>,
    charset: u8,
    segments: [Option<Vec<u8>>; 8],
    last_segment: Option<usize>,
    current: DynamicLabel,
    /// Toggle bit of the label in `current`: DL Plus commands refer to it by
    /// their link bit (`toggle` is already that of a label still coming in).
    shown_toggle: Option<bool>,
    /// Raw DL Plus command waiting for the matching label.
    pending_plus: Option<(bool, Vec<u8>)>,
}

impl DlsDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn current(&self) -> &DynamicLabel {
        &self.current
    }

    /// Process one DLS data group (CRC already verified). Returns the label
    /// when it changed.
    pub fn process(&mut self, dg: &[u8]) -> Option<DynamicLabel> {
        if dg.len() < 4 {
            return None;
        }
        let p0 = dg[0];
        let toggle = p0 & 0x80 != 0;
        let first = p0 & 0x40 != 0;
        let last = p0 & 0x20 != 0;
        let c_flag = p0 & 0x10 != 0;
        let body = &dg[2..dg.len() - 2];
        if c_flag {
            match p0 & 0x0F {
                0x01 => {
                    // clear display
                    if !self.current.text.is_empty() {
                        self.current = DynamicLabel::default();
                        self.segments = Default::default();
                        return Some(self.current.clone());
                    }
                }
                0x02 => {
                    // DL Plus command
                    let link = dg[1] & 0x80 != 0;
                    return self.dl_plus(link, body.to_vec());
                }
                _ => {}
            }
            return None;
        }
        if self.toggle != Some(toggle) {
            self.toggle = Some(toggle);
            self.segments = Default::default();
            self.last_segment = None;
        }
        let seg = if first {
            self.charset = dg[1] >> 4;
            0
        } else {
            ((dg[1] >> 4) & 0x07) as usize
        };
        self.segments[seg] = Some(body.to_vec());
        if last {
            self.last_segment = Some(seg);
        }
        let last = self.last_segment?;
        if (0..=last).any(|i| self.segments[i].is_none()) {
            return None;
        }
        let mut bytes = Vec::new();
        for i in 0..=last {
            bytes.extend_from_slice(self.segments[i].as_ref().unwrap());
        }
        let text = charset::decode(&bytes, self.charset);
        self.shown_toggle = self.toggle;
        if text == self.current.text && self.pending_plus.is_none() {
            return None;
        }
        let changed = text != self.current.text;
        self.current.text = text;
        if changed {
            self.current.tags.clear();
        }
        if let Some((link, cmd)) = self.pending_plus.take().filter(|(link, _)| self.shown_toggle == Some(*link)) {
            self.apply_plus(link, &cmd);
        }
        Some(self.current.clone())
    }

    fn dl_plus(&mut self, link: bool, cmd: Vec<u8>) -> Option<DynamicLabel> {
        // the tags refer to the label with the same toggle bit as the link bit
        if self.shown_toggle == Some(link) && !self.current.text.is_empty() {
            let before = self.current.clone();
            self.apply_plus(link, &cmd);
            if self.current != before {
                return Some(self.current.clone());
            }
            None
        } else {
            self.pending_plus = Some((link, cmd));
            None
        }
    }

    fn apply_plus(&mut self, _link: bool, cmd: &[u8]) {
        if cmd.is_empty() || cmd[0] >> 4 != 0 {
            return;
        }
        let it = cmd[0] & 0x08 != 0;
        let ir = cmd[0] & 0x04 != 0;
        let nt = (cmd[0] & 0x03) as usize + 1;
        let chars: Vec<char> = self.current.text.chars().collect();
        let mut tags = Vec::new();
        for t in 0..nt {
            let o = 1 + t * 3;
            if o + 3 > cmd.len() {
                break;
            }
            let ct = cmd[o] & 0x7F;
            let start = (cmd[o + 1] & 0x7F) as usize;
            let len = (cmd[o + 2] & 0x7F) as usize + 1;
            if ct == 0 || start >= chars.len() {
                continue;
            }
            let end = (start + len).min(chars.len());
            let text: String = chars[start..end].iter().collect::<String>().trim().to_string();
            tags.push(DlPlusTag { content_type: ct, name: dl_plus_name(ct), text });
        }
        self.current.tags = tags;
        self.current.item_toggle = it;
        self.current.item_running = ir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-segment label (EBU Latin) or a DL Plus command, without the CRC
    /// (`process` takes data groups with a verified CRC and skips the last 2 bytes).
    fn label(toggle: bool, text: &str) -> Vec<u8> {
        let mut dg = vec![(toggle as u8) << 7 | 0x60 | (text.len() as u8 - 1), 0x00];
        dg.extend_from_slice(text.as_bytes());
        dg.extend_from_slice(&[0, 0]);
        dg
    }

    fn first_segment(toggle: bool, text: &str) -> Vec<u8> {
        let mut dg = label(toggle, text);
        dg[0] &= !0x20;
        dg
    }

    /// DL Plus: one tag (content type 1 = ITEM.TITLE) from `start`, `len` characters.
    fn plus(link: bool, start: u8, len: u8) -> Vec<u8> {
        vec![0x12, (link as u8) << 7 | 0x03, 0x00, 0x01, start, len - 1, 0, 0]
    }

    #[test]
    fn tags_go_with_the_label_of_their_link_bit() {
        let mut d = DlsDecoder::new();
        d.process(&label(false, "Song A by X"));
        // the next label begins (a segment lost), its tags come first
        d.process(&first_segment(true, "Other "));
        assert!(d.process(&plus(true, 0, 5)).is_none());
        assert!(d.current().tags.is_empty(), "applied to the old label");
        // the next label complete: the waiting tags are applied to it
        let l = d.process(&label(true, "Title Artist")).unwrap();
        assert_eq!(l.tags.first().map(|t| t.text.as_str()), Some("Title"));
    }
}
