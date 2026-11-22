//! TPEG binary primitives (ISO 21219-4, UML to binary conversion rules):
//! fixed size integers, multi-byte integers, selectors (BitArray), strings
//! and generic components (id, component length, attribute block length).

/// Reads TPEG binary data types from a byte slice; every read returns `None`
/// when the data ends too early.
#[derive(Clone)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(b)
    }

    /// IntUnTi.
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    /// IntUnLi.
    pub fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    /// IntSiLi.
    pub fn i16(&mut self) -> Option<i16> {
        Some(self.u16()? as i16)
    }

    /// IntSi24.
    pub fn i24(&mut self) -> Option<i32> {
        let b = self.bytes(3)?;
        Some(i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8)
    }

    /// IntUnLo, also DateTime (seconds since 1970-01-01 UTC).
    pub fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// IntUnLoMB: 7 bits per byte, most significant first, bit 7 set on
    /// every byte but the last.
    pub fn mb(&mut self) -> Option<u32> {
        let mut v: u64 = 0;
        for _ in 0..5 {
            let b = self.u8()?;
            v = (v << 7) | (b & 0x7F) as u64;
            if b & 0x80 == 0 {
                return u32::try_from(v).ok();
            }
        }
        None
    }

    /// IntSiLoMB: as IntUnLoMB, bit 6 of the first byte is the sign.
    pub fn mb_signed(&mut self) -> Option<i32> {
        let first = *self.data.get(self.pos)?;
        let mut v: i64 = if first & 0x40 != 0 { -1 } else { 0 };
        for _ in 0..5 {
            let b = self.u8()?;
            v = (v << 7) | (b & 0x7F) as i64;
            if b & 0x80 == 0 {
                return i32::try_from(v).ok();
            }
        }
        None
    }

    /// BitArray (the selector of optional attributes): 7 flags per byte,
    /// bit 7 set on every byte but the last; flag 0 is bit 6 of the first byte.
    pub fn selector(&mut self) -> Option<Selector> {
        let mut bits: u64 = 0;
        let mut n = 0;
        loop {
            let b = self.u8()?;
            if n < 63 {
                bits |= ((b & 0x7F) as u64) << (56 - n);
            }
            n += 7;
            if b & 0x80 == 0 {
                return Some(Selector(bits));
            }
            if n > 63 {
                return None;
            }
        }
    }

    /// ShortString: length byte and UTF-8 text.
    pub fn short_string(&mut self) -> Option<String> {
        let n = self.u8()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    /// LocalisedShortString: language code, length byte and UTF-8 text.
    pub fn localised_short_string(&mut self) -> Option<String> {
        self.u8()?;
        self.short_string()
    }

    /// LocalisedLongString: language code, 16 bit length and UTF-8 text.
    pub fn localised_long_string(&mut self) -> Option<String> {
        self.u8()?;
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    /// A component: id, component length, attribute block length, attributes
    /// and sub-components.
    pub fn component(&mut self) -> Option<Component<'a>> {
        let id = self.u8()?;
        let len = self.mb()? as usize;
        let body = self.bytes(len)?;
        let mut r = Reader::new(body);
        let attr_len = r.mb()? as usize;
        let attrs = r.bytes(attr_len)?;
        Some(Component { id, attrs, subs: &body[r.pos..] })
    }
}

/// Flags of a BitArray, flag 0 first.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Selector(u64);

impl Selector {
    pub fn has(&self, flag: u32) -> bool {
        flag < 63 && self.0 & (1 << (62 - flag)) != 0
    }
}

/// A TPEG2 component with its attribute block and sub-components.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Component<'a> {
    pub id: u8,
    pub attrs: &'a [u8],
    pub subs: &'a [u8],
}

impl<'a> Component<'a> {
    pub fn attributes(&self) -> Reader<'a> {
        Reader::new(self.attrs)
    }

    /// The sub-components; iteration stops at the first malformed one.
    pub fn children(&self) -> impl Iterator<Item = Component<'a>> {
        let mut r = Reader::new(self.subs);
        std::iter::from_fn(move || if r.is_empty() { None } else { r.component() })
    }

    /// The sub-components fill the component exactly.
    pub fn well_formed(&self) -> bool {
        let mut r = Reader::new(self.subs);
        while !r.is_empty() {
            if r.component().is_none() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
pub(crate) mod build {
    //! Encoders for the unit tests.

    pub fn mb(v: u32) -> Vec<u8> {
        let mut out = vec![(v & 0x7F) as u8];
        let mut v = v >> 7;
        while v > 0 {
            out.insert(0, 0x80 | (v & 0x7F) as u8);
            v >>= 7;
        }
        out
    }

    /// A selector with the given flags set.
    pub fn selector(flags: &[u32]) -> Vec<u8> {
        let n = flags.iter().map(|f| f / 7 + 1).max().unwrap_or(1) as usize;
        let mut out = vec![0u8; n];
        for &f in flags {
            out[(f / 7) as usize] |= 0x40 >> (f % 7);
        }
        for b in out.iter_mut().take(n - 1) {
            *b |= 0x80;
        }
        out
    }

    pub fn component(id: u8, attrs: &[u8], subs: &[&[u8]]) -> Vec<u8> {
        let mut body = mb(attrs.len() as u32);
        body.extend_from_slice(attrs);
        for s in subs {
            body.extend_from_slice(s);
        }
        let mut out = vec![id];
        out.extend(mb(body.len() as u32));
        out.extend(body);
        out
    }

    /// A WGS84 coordinate as IntSi24 (360 / 2^24 degree units).
    pub fn coord24(deg: f64) -> [u8; 3] {
        let v = (deg * (1 << 24) as f64 / 360.0 + 0.5 * deg.signum()) as i32;
        let b = v.to_be_bytes();
        [b[1], b[2], b[3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_byte_integers() {
        assert_eq!(Reader::new(&[0x7F]).mb(), Some(127));
        assert_eq!(Reader::new(&[0x81, 0x00]).mb(), Some(128));
        assert_eq!(Reader::new(&[0x81, 0xBF, 0x5F]).mb(), Some((1 << 14) | (0x3F << 7) | 0x5F));
        assert_eq!(Reader::new(&[0x81]).mb(), None);
        assert_eq!(Reader::new(&[0xFF; 6]).mb(), None);
        for v in [0, 1, 127, 128, 16383, 16384, 1 << 28, u32::MAX] {
            assert_eq!(Reader::new(&build::mb(v)).mb(), Some(v));
        }
        assert_eq!(Reader::new(&[0x7F]).mb_signed(), Some(-1));
        assert_eq!(Reader::new(&[0x3F]).mb_signed(), Some(63));
        assert_eq!(Reader::new(&[0xFF, 0x00]).mb_signed(), Some(-128));
    }

    #[test]
    fn signed_24_bit() {
        assert_eq!(Reader::new(&[0xFF, 0xFF, 0xFF]).i24(), Some(-1));
        assert_eq!(Reader::new(&[0x08, 0xD5, 0x38]).i24(), Some(0x08D538));
        assert_eq!(Reader::new(&[0x80, 0x00, 0x00]).i24(), Some(-(1 << 23)));
    }

    #[test]
    fn selectors_count_flags_from_bit_6() {
        let s = Reader::new(&[0x40]).selector().unwrap();
        assert!(s.has(0) && !s.has(1));
        let s = Reader::new(&[0x30]).selector().unwrap();
        assert!(!s.has(0) && s.has(1) && s.has(2) && !s.has(3));
        // two bytes: flag 8 is bit 5 of the second byte
        let mut r = Reader::new(&[0x81, 0x20, 0x55]);
        let s = r.selector().unwrap();
        assert!(s.has(6) && s.has(8) && !s.has(7));
        assert_eq!(r.u8(), Some(0x55));
        for flags in [&[0u32][..], &[3, 9], &[13, 14]] {
            let s = Reader::new(&build::selector(flags)).selector().unwrap();
            for f in 0..21 {
                assert_eq!(s.has(f), flags.contains(&f), "{flags:?} {f}");
            }
        }
    }

    #[test]
    fn components_and_children() {
        let child = build::component(4, &[3, 2, 0], &[]);
        let root = build::component(3, &[7, 0], &[&child, &child]);
        let c = Reader::new(&root).component().unwrap();
        assert_eq!((c.id, c.attrs), (3, &[7u8, 0][..]));
        assert!(c.well_formed());
        let kids: Vec<_> = c.children().collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[1].attrs, &[3, 2, 0]);
        // a length beyond the data
        assert!(Reader::new(&root[..root.len() - 1]).component().is_none());
    }
}
