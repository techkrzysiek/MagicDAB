//! RDS decoding with librdsparser by Konrad Kosmatka (third_party/librdsparser,
//! LGPL-2.1-or-later): a thin binding, configured like FM-DX Webserver.

use serde::Serialize;
use std::ffi::{c_char, c_void, CStr};

#[repr(C)]
struct Raw {
    _p: [u8; 0],
}

#[repr(C)]
struct RawString {
    _p: [u8; 0],
}

#[repr(C)]
struct RawCt {
    _p: [u8; 0],
}

/// wchar_t of the platform (librdsparser converts the RDS characters).
#[cfg(windows)]
type WChar = u16;
#[cfg(not(windows))]
type WChar = u32;

extern "C" {
    fn rdsparser_new() -> *mut Raw;
    fn rdsparser_free(rds: *mut Raw);
    fn rdsparser_clear(rds: *mut Raw);
    fn rdsparser_parse(rds: *mut Raw, data: *mut u16, errors: *mut u8);
    fn rdsparser_set_text_correction(rds: *mut Raw, text: u8, block_type: u8, error: u8);
    fn rdsparser_set_text_progressive(rds: *mut Raw, text: u8, mode: u8);
    fn rdsparser_get_pi(rds: *const Raw) -> i32;
    fn rdsparser_get_pty(rds: *const Raw) -> i8;
    fn rdsparser_get_tp(rds: *const Raw) -> i8;
    fn rdsparser_get_ta(rds: *const Raw) -> i8;
    fn rdsparser_get_ms(rds: *const Raw) -> i8;
    fn rdsparser_get_ecc(rds: *const Raw) -> i16;
    fn rdsparser_get_country(rds: *const Raw) -> u8;
    fn rdsparser_get_ps(rds: *const Raw) -> *const RawString;
    fn rdsparser_get_rt(rds: *const Raw, flag: u8) -> *const RawString;
    fn rdsparser_get_ptyn(rds: *const Raw) -> *const RawString;
    fn rdsparser_get_lps(rds: *const Raw) -> *const c_char;
    fn rdsparser_set_user_data(rds: *mut Raw, user_data: *mut c_void);
    fn rdsparser_register_af(rds: *mut Raw, cb: extern "C" fn(*mut Raw, u32, *mut c_void));
    fn rdsparser_register_rt(rds: *mut Raw, cb: extern "C" fn(*mut Raw, u8, *mut c_void));
    fn rdsparser_register_ct(rds: *mut Raw, cb: extern "C" fn(*mut Raw, *const RawCt, *mut c_void));
    fn rdsparser_string_get_length(s: *const RawString) -> u8;
    fn rdsparser_string_get_available(s: *const RawString) -> bool;
    fn rdsparser_string_get_content(s: *const RawString) -> *const WChar;
    fn rdsparser_ct_get_year(ct: *const RawCt) -> u16;
    fn rdsparser_ct_get_month(ct: *const RawCt) -> u8;
    fn rdsparser_ct_get_day(ct: *const RawCt) -> u8;
    fn rdsparser_ct_get_hour(ct: *const RawCt) -> u8;
    fn rdsparser_ct_get_minute(ct: *const RawCt) -> u8;
    fn rdsparser_ct_get_offset(ct: *const RawCt) -> i16;
    fn rdsparser_pty_lookup_name(pty: i8, rbds: bool) -> *const c_char;
    fn rdsparser_country_lookup_name(country: u8) -> *const c_char;
    fn rdsparser_country_lookup_iso(country: u8) -> *const c_char;
}

const TEXT_PS: u8 = 0;
const TEXT_RT: u8 = 1;
const BLOCK_TYPE_INFO: u8 = 0;
const BLOCK_TYPE_DATA: u8 = 1;
const PROGRESSIVE_AUTO: u8 = 1;

/// Clock time (RDS group 4A).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    /// Local time offset in minutes.
    pub offset: i16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RdsInfo {
    pub pi: Option<u16>,
    pub ps: String,
    /// Current radiotext (the A/B flag last received).
    pub rt: String,
    pub pty: Option<u8>,
    pub pty_name: String,
    pub tp: Option<bool>,
    pub ta: Option<bool>,
    pub music: Option<bool>,
    pub ecc: Option<u8>,
    pub country: String,
    pub country_iso: String,
    /// Alternative frequencies (kHz).
    pub af: Vec<u32>,
    pub ptyn: String,
    pub lps: String,
    pub ct: Option<ClockTime>,
}

/// Collected by the callbacks.
#[derive(Default)]
struct Shared {
    af: Vec<u32>,
    rt_flag: u8,
    ct: Option<ClockTime>,
}

pub struct RdsParser {
    raw: *mut Raw,
    shared: Box<Shared>,
}

// the parser is used from one thread at a time
unsafe impl Send for RdsParser {}

extern "C" fn on_af(_: *mut Raw, khz: u32, user: *mut c_void) {
    let s = unsafe { &mut *(user as *mut Shared) };
    if !s.af.contains(&khz) {
        s.af.push(khz);
        s.af.sort_unstable();
    }
}

extern "C" fn on_rt(_: *mut Raw, flag: u8, user: *mut c_void) {
    unsafe { (*(user as *mut Shared)).rt_flag = flag };
}

extern "C" fn on_ct(_: *mut Raw, ct: *const RawCt, user: *mut c_void) {
    let c = unsafe {
        ClockTime {
            year: rdsparser_ct_get_year(ct),
            month: rdsparser_ct_get_month(ct),
            day: rdsparser_ct_get_day(ct),
            hour: rdsparser_ct_get_hour(ct),
            minute: rdsparser_ct_get_minute(ct),
            offset: rdsparser_ct_get_offset(ct),
        }
    };
    unsafe { (*(user as *mut Shared)).ct = Some(c) };
}

fn text(s: *const RawString) -> String {
    if s.is_null() || !unsafe { rdsparser_string_get_available(s) } {
        return String::new();
    }
    let len = unsafe { rdsparser_string_get_length(s) } as usize;
    let content = unsafe { std::slice::from_raw_parts(rdsparser_string_get_content(s), len) };
    #[cfg(windows)]
    let t = String::from_utf16_lossy(content);
    #[cfg(not(windows))]
    let t: String = content.iter().map(|&c| char::from_u32(c).unwrap_or(' ')).collect();
    t.trim_end().to_string()
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().to_string()
    }
}

impl RdsParser {
    pub fn new() -> Self {
        let raw = unsafe { rdsparser_new() };
        assert!(!raw.is_null(), "rdsparser_new");
        let mut shared = Box::<Shared>::default();
        unsafe {
            rdsparser_set_user_data(raw, &mut *shared as *mut Shared as *mut c_void);
            rdsparser_register_af(raw, on_af);
            rdsparser_register_rt(raw, on_rt);
            rdsparser_register_ct(raw, on_ct);
            // FM-DX Webserver's correction settings
            for t in [TEXT_PS, TEXT_RT] {
                rdsparser_set_text_correction(raw, t, BLOCK_TYPE_INFO, 2);
                rdsparser_set_text_correction(raw, t, BLOCK_TYPE_DATA, 2);
                rdsparser_set_text_progressive(raw, t, PROGRESSIVE_AUTO);
            }
        }
        RdsParser { raw, shared }
    }

    /// New station.
    pub fn clear(&mut self) {
        unsafe { rdsparser_clear(self.raw) };
        *self.shared = Shared::default();
    }

    /// One group: blocks A-D and their error levels (0-3).
    pub fn parse(&mut self, mut blocks: [u16; 4], mut errors: [u8; 4]) {
        unsafe { rdsparser_parse(self.raw, blocks.as_mut_ptr(), errors.as_mut_ptr()) };
    }

    pub fn info(&self) -> RdsInfo {
        let r = self.raw;
        unsafe {
            let pty = rdsparser_get_pty(r);
            let ecc = rdsparser_get_ecc(r);
            let country = rdsparser_get_country(r);
            let flag = |v: i8| (v >= 0).then_some(v > 0);
            RdsInfo {
                pi: u16::try_from(rdsparser_get_pi(r)).ok(),
                ps: text(rdsparser_get_ps(r)),
                rt: text(rdsparser_get_rt(r, self.shared.rt_flag)),
                pty: u8::try_from(pty).ok(),
                pty_name: if pty >= 0 { cstr(rdsparser_pty_lookup_name(pty, false)) } else { String::new() },
                tp: flag(rdsparser_get_tp(r)),
                ta: flag(rdsparser_get_ta(r)),
                music: flag(rdsparser_get_ms(r)),
                ecc: u8::try_from(ecc).ok(),
                country: if country > 0 { cstr(rdsparser_country_lookup_name(country)) } else { String::new() },
                country_iso: if country > 0 { cstr(rdsparser_country_lookup_iso(country)) } else { String::new() },
                af: self.shared.af.clone(),
                ptyn: text(rdsparser_get_ptyn(r)),
                lps: cstr(rdsparser_get_lps(r)).trim_end().to_string(),
                ct: self.shared.ct.clone(),
            }
        }
    }
}

impl Default for RdsParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RdsParser {
    fn drop(&mut self) {
        unsafe { rdsparser_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_and_af() {
        let mut p = RdsParser::new();
        let text = b"RADIO X ";
        // 0A groups, TP set, PTY 10; AF codes 175 (105.0 MHz) and 101 (97.6 MHz)
        for _ in 0..3 {
            for s in 0..4u16 {
                let d = ((text[s as usize * 2] as u16) << 8) | text[s as usize * 2 + 1] as u16;
                p.parse([0x5A2C, 0x0540 | s, (175 << 8) | 101, d], [0; 4]);
            }
        }
        let i = p.info();
        assert_eq!(i.pi, Some(0x5A2C));
        assert_eq!(i.ps, "RADIO X");
        assert_eq!(i.pty, Some(10));
        assert_eq!(i.tp, Some(true));
        assert_eq!(i.af, vec![97_600, 105_000]);
        p.clear();
        assert_eq!(p.info().ps, "");
    }
}
