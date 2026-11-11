//! wRadio C100 / DTR-100 USB DAB+ dongles (USB 16c0:05dc, "DAB USB Dongle"):
//! a RAONTECH DAB chip behind a small USB controller. The chip demodulates;
//! the host gets the raw FIC and the bytes of one sub-channel and decodes the
//! rest itself (no I/Q, so no spectrum, TII or gain control).
//!
//! A port of the register-level driver of DABplay+ (github.com/jvspier/dabplayplus,
//! MIT licence, `driver-dtr100` and docs/protocol.md there), which replays the
//! transfers of the vendor's own library; register tables are functional data
//! for interoperability. libusb-1.0 is loaded at runtime, like librtlsdr.

use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const VID: u16 = 0x16C0;
pub const PID: u16 = 0x05DC;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x82;
const LIBUSB_ERROR_TIMEOUT: c_int = -7;

/// CER (channel error rate) meaning nothing receivable; the chip's own scale.
pub const SIGNAL_NONE: u32 = 2000;
/// From here errors are likely (one bar on RAON's own meter).
pub const SIGNAL_WEAK: u32 = 700;
/// No audio through the controller this long after a start: read it from the chip.
const MCU_GRACE: Duration = Duration::from_millis(2000);
/// Read size and threshold when audio comes straight from the chip (as DABdream+ does).
const CHIP_READ: usize = 0x800;
const INTERFACE_TRIES: u32 = 100;
const CLOCK_PLAN_FAILED: i32 = -14;

/// A stick of this process needed the chip path: after a reconnect use it straight away.
static CHIP_AUDIO_NEEDED: AtomicBool = AtomicBool::new(false);

// ------------------------------------------------------------------ libusb

type Ctx = *mut c_void;
type Device = *mut c_void;
type Handle = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct DeviceDescriptor {
    b_length: u8,
    b_descriptor_type: u8,
    bcd_usb: u16,
    b_device_class: u8,
    b_device_sub_class: u8,
    b_device_protocol: u8,
    b_max_packet_size0: u8,
    id_vendor: u16,
    id_product: u16,
    bcd_device: u16,
    i_manufacturer: u8,
    i_product: u8,
    i_serial_number: u8,
    b_num_configurations: u8,
}

struct Usb {
    _lib: Library,
    init: unsafe extern "C" fn(*mut Ctx) -> c_int,
    exit: unsafe extern "C" fn(Ctx),
    get_device_list: unsafe extern "C" fn(Ctx, *mut *mut Device) -> isize,
    free_device_list: unsafe extern "C" fn(*mut Device, c_int),
    get_device_descriptor: unsafe extern "C" fn(Device, *mut DeviceDescriptor) -> c_int,
    get_bus_number: unsafe extern "C" fn(Device) -> u8,
    get_device_address: unsafe extern "C" fn(Device) -> u8,
    open: unsafe extern "C" fn(Device, *mut Handle) -> c_int,
    close: unsafe extern "C" fn(Handle),
    get_string_descriptor_ascii: unsafe extern "C" fn(Handle, u8, *mut u8, c_int) -> c_int,
    set_auto_detach_kernel_driver: unsafe extern "C" fn(Handle, c_int) -> c_int,
    claim_interface: unsafe extern "C" fn(Handle, c_int) -> c_int,
    release_interface: unsafe extern "C" fn(Handle, c_int) -> c_int,
    bulk_transfer: unsafe extern "C" fn(Handle, u8, *mut u8, c_int, *mut c_int, c_uint) -> c_int,
    error_name: unsafe extern "C" fn(c_int) -> *const c_char,
}

unsafe impl Send for Usb {}
unsafe impl Sync for Usb {}

fn library_names() -> Vec<std::ffi::OsString> {
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["libusb-1.0.dll"]
    } else if cfg!(target_os = "macos") {
        &["libusb-1.0.0.dylib", "libusb-1.0.dylib", "/opt/homebrew/lib/libusb-1.0.0.dylib", "/usr/local/lib/libusb-1.0.0.dylib"]
    } else {
        &["libusb-1.0.so.0", "libusb-1.0.so"]
    };
    // a library a package brings next to the program comes first
    let own = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(names[0]))).filter(|p| !cfg!(windows) && p.exists());
    own.into_iter().map(|p| p.into_os_string()).chain(names.iter().map(|n| n.into())).collect()
}

fn load_usb() -> Result<Usb, String> {
    let mut last_err = String::new();
    for name in library_names() {
        match unsafe { Library::new(&name) } {
            Ok(lib) => unsafe {
                macro_rules! sym {
                    ($n:expr) => {{
                        let s: Symbol<_> = lib.get($n).map_err(|e| e.to_string())?;
                        *s
                    }};
                }
                return Ok(Usb {
                    init: sym!(b"libusb_init\0"),
                    exit: sym!(b"libusb_exit\0"),
                    get_device_list: sym!(b"libusb_get_device_list\0"),
                    free_device_list: sym!(b"libusb_free_device_list\0"),
                    get_device_descriptor: sym!(b"libusb_get_device_descriptor\0"),
                    get_bus_number: sym!(b"libusb_get_bus_number\0"),
                    get_device_address: sym!(b"libusb_get_device_address\0"),
                    open: sym!(b"libusb_open\0"),
                    close: sym!(b"libusb_close\0"),
                    get_string_descriptor_ascii: sym!(b"libusb_get_string_descriptor_ascii\0"),
                    set_auto_detach_kernel_driver: sym!(b"libusb_set_auto_detach_kernel_driver\0"),
                    claim_interface: sym!(b"libusb_claim_interface\0"),
                    release_interface: sym!(b"libusb_release_interface\0"),
                    bulk_transfer: sym!(b"libusb_bulk_transfer\0"),
                    error_name: sym!(b"libusb_error_name\0"),
                    _lib: lib,
                });
            },
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("libusb-1.0 not found ({last_err})"))
}

fn usb() -> Result<&'static Usb, String> {
    use std::sync::OnceLock;
    static USB: OnceLock<Result<Usb, String>> = OnceLock::new();
    USB.get_or_init(load_usb).as_ref().map_err(|e| e.clone())
}

fn usb_error(u: &Usb, code: c_int) -> String {
    let p = unsafe { (u.error_name)(code) };
    if p.is_null() {
        format!("libusb error {code}")
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// The open USB device (interface 0 claimed).
struct Link {
    usb: &'static Usb,
    ctx: Ctx,
    handle: Handle,
}

unsafe impl Send for Link {}

impl Link {
    /// The first 16c0:05dc device that names itself a DAB dongle (the ID is
    /// shared by many libusb gadgets, e.g. USBasp programmers).
    fn open() -> Result<(Link, String), String> {
        let u = usb()?;
        let mut ctx: Ctx = std::ptr::null_mut();
        let rc = unsafe { (u.init)(&mut ctx) };
        if rc != 0 {
            return Err(format!("libusb_init: {}", usb_error(u, rc)));
        }
        let mut list: *mut Device = std::ptr::null_mut();
        let n = unsafe { (u.get_device_list)(ctx, &mut list) };
        let mut found: Option<(Handle, String)> = None;
        let mut seen = Vec::new();
        for i in 0..n.max(0) as usize {
            let dev = unsafe { *list.add(i) };
            let mut d = DeviceDescriptor::default();
            if unsafe { (u.get_device_descriptor)(dev, &mut d) } != 0 || d.id_vendor != VID || d.id_product != PID {
                continue;
            }
            let (bus, addr) = unsafe { ((u.get_bus_number)(dev), (u.get_device_address)(dev)) };
            let mut h: Handle = std::ptr::null_mut();
            let rc = unsafe { (u.open)(dev, &mut h) };
            if rc != 0 {
                seen.push(format!("{bus:03}/{addr:03}: {}", usb_error(u, rc)));
                continue;
            }
            let text = |idx: u8| {
                let mut buf = [0u8; 128];
                let r = unsafe { (u.get_string_descriptor_ascii)(h, idx, buf.as_mut_ptr(), buf.len() as c_int) };
                if idx == 0 || r <= 0 {
                    String::new()
                } else {
                    String::from_utf8_lossy(&buf[..r as usize]).into_owned()
                }
            };
            let (mfr, product) = (text(d.i_manufacturer), text(d.i_product));
            if mfr.to_ascii_lowercase().contains("dab") || product.to_ascii_lowercase().contains("dab") {
                found = Some((h, format!("{product} ({mfr}, USB {bus:03}/{addr:03})")));
                break;
            }
            seen.push(format!("{bus:03}/{addr:03}: {mfr} {product}"));
            unsafe { (u.close)(h) };
        }
        if !list.is_null() {
            unsafe { (u.free_device_list)(list, 1) };
        }
        let Some((handle, name)) = found else {
            unsafe { (u.exit)(ctx) };
            return Err(if seen.is_empty() {
                "no DTR-100 / C100 DAB dongle (16c0:05dc) found".into()
            } else {
                format!("no usable DAB dongle ({})", seen.join("; "))
            });
        };
        unsafe { (u.set_auto_detach_kernel_driver)(handle, 1) };
        let rc = unsafe { (u.claim_interface)(handle, 0) };
        if rc != 0 {
            unsafe {
                (u.close)(handle);
                (u.exit)(ctx);
            }
            return Err(format!("cannot claim the dongle: {}", usb_error(u, rc)));
        }
        Ok((Link { usb: u, ctx, handle }, name))
    }

    fn write(&self, data: &[u8], timeout_ms: u32) -> Result<(), String> {
        let mut buf = data.to_vec();
        let mut done: c_int = 0;
        let rc = unsafe { (self.usb.bulk_transfer)(self.handle, EP_OUT, buf.as_mut_ptr(), buf.len() as c_int, &mut done, timeout_ms) };
        if rc != 0 {
            return Err(format!("USB write: {}", usb_error(self.usb, rc)));
        }
        Ok(())
    }

    /// Ok(None) on a timeout.
    fn read(&self, buf: &mut [u8], timeout_ms: u32) -> Result<Option<usize>, String> {
        let mut done: c_int = 0;
        let rc = unsafe { (self.usb.bulk_transfer)(self.handle, EP_IN, buf.as_mut_ptr(), buf.len() as c_int, &mut done, timeout_ms) };
        match rc {
            0 => Ok(Some(done.max(0) as usize)),
            LIBUSB_ERROR_TIMEOUT => Ok(None),
            _ => Err(format!("USB read: {}", usb_error(self.usb, rc))),
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe {
            (self.usb.release_interface)(self.handle, 0);
            (self.usb.close)(self.handle);
            (self.usb.exit)(self.ctx);
        }
    }
}

// ------------------------------------------------------------------ driver

/// A tune attempt.
#[derive(Clone, Debug, Default)]
pub struct Tuned {
    pub locked: bool,
    /// Bars 0..4 right after the lock (from the tune's level value).
    pub bars: u8,
    pub detail: String,
}

/// Reception as the dongle last reported it.
#[derive(Clone, Copy, Debug)]
pub struct Signal {
    /// 0..4.
    pub bars: u8,
    /// Nothing receivable.
    pub none: bool,
    /// Errors likely.
    pub weak: bool,
    /// The chip's CER: 0 = perfect, 2000 = nothing.
    pub cer: u32,
}

#[derive(Clone, Debug, Default)]
struct TuneInfo {
    why: Option<String>,
    sync: Option<u8>,
    mode_bits: Option<u8>,
    polls: u32,
    u1: Option<u32>,
    u21: Option<u32>,
    loop_b_rounds: Option<u32>,
    u15: Option<u32>,
    q: Option<u32>,
}

pub struct Dtr100 {
    link: Link,
    name: String,
    /// MCU firmware level (selects the sub-channel data path).
    fw: u8,
    plan: Option<u8>,
    band_mode: u8,
    irq_mask: u8,
    fic_open: bool,
    mcu_signal: Option<u32>,
    msc_seq: u8,
    msc_size: usize,
    subch: Option<u8>,
    chip_audio: bool,
    last_mcu_status: Vec<u8>,
    last_chip_status: u8,
    // the tuner layer (DABplay+ Dtr100Tuner)
    fic_enabled: bool,
    mcu_delivered: bool,
    audio_at: Option<Instant>,
    chip_sig: Option<u32>,
    chip_sig_at: Option<Instant>,
    /// A sub-channel streams: the register reads of every few milliseconds
    /// wait for a reply as long as normal ones take, not as long as the
    /// chip's start (STEADY_REPLY_MS).
    streaming: bool,
    /// Times the chip's sub-channel buffer ran over (read too late) and
    /// started again: audio lost.
    pub overruns: u64,
}

/// Time a reply of the controller gets while a sub-channel streams (normally
/// a few milliseconds); then the request is made again. A request it lets go
/// unanswered (now and then on a Raspberry Pi) is answered at once the next time.
const STEADY_REPLY_MS: u32 = 250;
/// Requests made again before the dongle counts as lost.
const ASK_AGAIN: usize = 2;

impl Dtr100 {
    /// Finds and initialises the dongle (power on, chip init: about a second).
    pub fn open() -> Result<Dtr100, String> {
        let (link, name) = Link::open()?;
        let mut d = Dtr100 {
            link,
            name,
            fw: 0,
            plan: None,
            band_mode: 0,
            irq_mask: 0,
            fic_open: false,
            mcu_signal: None,
            msc_seq: 0,
            msc_size: 0,
            subch: None,
            chip_audio: false,
            last_mcu_status: Vec::new(),
            last_chip_status: 0,
            fic_enabled: false,
            mcu_delivered: false,
            audio_at: None,
            chip_sig: None,
            chip_sig_at: None,
            streaming: false,
            overruns: 0,
        };
        let stale = d.drain_stale(64);
        if stale > 0 {
            log::info!("DTR-100: dropped {stale} stale bytes of an earlier session");
        }
        d.fw = d.version()?;
        d.power_on()?;
        let checks = d.system_init()?;
        if checks > 1 {
            log::info!("DTR-100: the chip answered after {checks} interface checks");
        }
        if CHIP_AUDIO_NEEDED.load(Ordering::Relaxed) && d.fw > 1 {
            d.use_chip_audio()?;
            log::info!("DTR-100: reading audio from the DAB chip, as before the reconnect");
        }
        log::info!("DTR-100: {} ready, controller firmware level {}", d.name, d.fw);
        Ok(d)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn firmware_level(&self) -> u8 {
        self.fw
    }

    /// Audio is read from the DAB chip (the controller of this stick passes none on).
    pub fn chip_audio(&self) -> bool {
        self.chip_audio
    }

    // --- transport

    /// A request and its reply. A reply that does not come (the controller
    /// held up for a moment, a dip of the supply) is asked for again, after
    /// what is left of the exchange is read away: reopening the dongle costs
    /// about 8 s of audio.
    fn xfer(&mut self, req: &[u8], reply_len: usize, timeout_out: u32, timeout_in: u32) -> Result<Vec<u8>, String> {
        let timeout_in = if self.streaming { timeout_in.min(STEADY_REPLY_MS) } else { timeout_in };
        let mut result = self.xfer_once(req, reply_len, timeout_out, timeout_in);
        let mut tries = 0;
        while tries < ASK_AGAIN && result.as_ref().is_err_and(|e| e.ends_with("(timeout)")) {
            tries += 1;
            let stale = self.drain_stale(8);
            result = self.xfer_once(req, reply_len, timeout_out, timeout_in);
            if result.is_ok() {
                log::info!(
                    "DTR-100: no reply to {:02x?}; asked again ({tries}), answered ({stale} bytes left over)",
                    &req[..req.len().min(5)]
                );
            }
        }
        result
    }

    fn xfer_once(&mut self, req: &[u8], reply_len: usize, timeout_out: u32, timeout_in: u32) -> Result<Vec<u8>, String> {
        self.link.write(req, timeout_out)?;
        let mut buf = vec![0u8; reply_len.max(64)];
        match self.link.read(&mut buf, timeout_in)? {
            Some(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            None => Err(format!("no reply to {:02x?} (timeout)", &req[..req.len().min(5)])),
        }
    }

    fn cmd(&mut self, req: [u8; 4], reply_len: usize) -> Result<Vec<u8>, String> {
        self.xfer(&req, reply_len, 1000, 1000)
    }

    fn expect(rep: &[u8], code: u8, what: &str) -> Result<(), String> {
        if rep.first() != Some(&code) {
            return Err(format!("{what}: expected reply {code:#04x}, got {}", hex(rep)));
        }
        Ok(())
    }

    /// Replies a previous session left queued; only reads.
    fn drain_stale(&mut self, max_reads: usize) -> usize {
        let mut buf = vec![0u8; 16 * 1024];
        let mut total = 0;
        for _ in 0..max_reads {
            match self.link.read(&mut buf, 30) {
                Ok(Some(n)) if n > 0 => total += n,
                _ => break,
            }
        }
        total
    }

    // --- controller commands

    fn version(&mut self) -> Result<u8, String> {
        let r = self.cmd([0x01, 0, 0, 0], 4)?;
        Self::expect(&r, 0x81, "version")?;
        if r.len() < 4 {
            return Err(format!("version: short reply {}", hex(&r)));
        }
        log::info!("DTR-100: version reply {}", hex(&r));
        Ok(r[3])
    }

    fn power_on(&mut self) -> Result<(), String> {
        let r = self.xfer(&[0x10, 0, 0, 1], 4, 2000, 4000)?;
        Self::expect(&r, 0x90, "power on")
    }

    // --- chip registers over SPI

    fn wreg(&mut self, reg: u8, value: u8) -> Result<(), String> {
        let r = self.xfer(&[0x21, 0, 0, 2, reg, value], 1, 1000, 4000)?;
        Self::expect(&r, 0xA1, &format!("write reg {reg:#04x}"))
    }

    fn rreg(&mut self, reg: u8, n: usize) -> Result<Vec<u8>, String> {
        let r = self.xfer(&[0x22, 0, (n & 0xFF) as u8, (n >> 8) as u8, reg], n + 4, 1000, 4000)?;
        Self::expect(&r, 0xA2, &format!("read reg {reg:#04x}"))?;
        let got = r.get(2).copied().unwrap_or(0) as usize | (r.get(3).copied().unwrap_or(0) as usize) << 8;
        if got != n || r.len() < 4 + n {
            return Err(format!("read reg {reg:#04x}: size mismatch, asked {n}, got {got}"));
        }
        Ok(r[4..4 + n].to_vec())
    }

    fn r1(&mut self, reg: u8) -> Result<u8, String> {
        Ok(self.rreg(reg, 1)?[0])
    }

    fn page(&mut self, p: u8) -> Result<(), String> {
        self.wreg(0x03, p)
    }

    fn wlist(&mut self, pairs: &[u8]) -> Result<(), String> {
        for p in pairs.chunks_exact(2) {
            self.wreg(p[0], p[1])?;
        }
        Ok(())
    }

    fn check_interface(&mut self) -> Result<u8, String> {
        self.page(7)?;
        self.wreg(0x7D, 0x06)?;
        self.r1(0x7D)
    }

    /// Until the chip answers, up to 100 times 5 ms apart (some sticks need time).
    fn await_interface(&mut self) -> Result<u32, String> {
        let mut seen = Vec::new();
        for k in 1..=INTERFACE_TRIES {
            let v = self.check_interface().map_err(|e| format!("check interface: {e} (not a register-level C100 stick?)"))?;
            if v == 0x06 {
                return Ok(k);
            }
            if !seen.contains(&v) {
                seen.push(v);
            }
            sleep(5);
        }
        Err(format!("check interface failed {INTERFACE_TRIES} times (read back {seen:02x?}): not a register-level C100 stick"))
    }

    // --- DAB chip (band III)

    fn clock_plan_regs(&mut self, p: u8) -> Result<(), String> {
        let c = CLOCK_PLANS[p as usize];
        if c[0] == 0xFF {
            return Err(format!("clock plan {p} invalid"));
        }
        self.page(0x0F)?;
        self.wlist(&[0xE8, c[0] | 0x40, 0xE9, c[1] | 0x50, 0xEA, c[2], 0xEB, c[3], 0xEC, c[4], 0xED, c[5], 0xEE, c[6]])
    }

    /// 0, or -14 when the plan is not applied (a failed tune; the next tune writes it again).
    fn set_clock_plan(&mut self, p: u8) -> Result<i32, String> {
        if self.plan == Some(p) {
            return Ok(0);
        }
        self.clock_plan_regs(p)?;
        let mut ok = false;
        for _ in 0..10 {
            if self.r1(0x15)? & 1 != 0 {
                ok = true;
                break;
            }
            sleep(10);
        }
        if !ok {
            return Ok(CLOCK_PLAN_FAILED);
        }
        let (r6a, p6, (r40, r41), r42) = PLAN_REGS[p as usize];
        self.page(4)?;
        self.wreg(0x6A, r6a)?;
        self.page(6)?;
        self.wlist(&p6)?;
        self.wlist(&[0x54, 0x58, 0x40, r40, 0x41, r41, 0x42, r42, 0x43, 0x00])?;
        self.plan = Some(p);
        Ok(0)
    }

    /// The vendor's system init; returns how many interface checks it took.
    fn system_init(&mut self) -> Result<u32, String> {
        let checks = self.await_interface()?;
        self.page(0x0F)?;
        self.wlist(&[0x54, 0x1C, 0x52, 0x07, 0x30, 0xF4, 0x2F, 0x71])?;
        self.clock_plan_regs(0)?;
        let mut ok = false;
        for _ in 0..10 {
            if self.r1(0x15)? & 1 != 0 {
                ok = true;
                break;
            }
            sleep(1);
        }
        if !ok {
            return Err("clock plan 0 not applied".into());
        }
        self.plan = Some(0);
        self.page(6)?;
        self.wlist(&[0x07, 0x08, 0x05, 0x17, 0x06, 0x10, 0x0A, 0x00])?;
        self.page(4)?;
        self.wlist(&[
            0x10, 0x91, 0xE1, 0x00, 0x35, 0x7B, 0x3B, 0x3C, 0x36, 0x68, 0x3A, 0x44, 0x3C, 0x20, 0x3D, 0x0B, 0x3D, 0x09, 0xA6, 0x30, 0xAA,
            0x01, 0xAF, 0x07,
        ])?;
        self.page(7)?;
        self.wlist(&[
            0x10, 0x00, 0x13, 0x16, 0x14, 0x00, 0x19, 0x0A, 0xF0, 0, 0xF1, 0, 0xF2, 0, 0xF3, 0, 0xF4, 0, 0xF5, 0, 0xF6, 0, 0xF7, 0, 0xF8,
            0, 0xFB, 0xFF,
        ])?;
        self.page(6)?;
        self.wlist(&[
            0x12, 0x04, 0x13, 0x72, 0x14, 0x63, 0x15, 0x64, 0x16, 0x6C, 0x38, 0x01, 0x1A, 0xB4, 0x20, 0x5B, 0x25, 0x09, 0x44, 0x09, 0x46,
            0xA0, 0x47, 0x0F, 0x48, 0xB8, 0x49, 0x0B, 0x54, 0x58, 0x55, 0x06, 0x56, 0x10, 0x59, 0x51, 0x5A, 0x1C, 0x6D, 0x00, 0x8B, 0x34,
            0x6A, 0x1C, 0x6B, 0x2D, 0x85, 0x32, 0x8D, 0x0C, 0x8E, 0x01, 0x33, 0x02, 0x53, 0x06, 0x6F, 0x03, 0xBA, 0x08,
        ])?;
        // clock plan 0 branch
        self.page(4)?;
        self.wreg(0x6A, 0x01)?;
        self.page(6)?;
        self.wlist(&[0x3C, 0x4B, 0x3D, 0x37, 0x3E, 0x89, 0x3F, 0x41, 0x40, 0x8F, 0x41, 0xC2, 0x42, 0xF5, 0x43, 0x00])?;
        self.wlist(&[
            0x94, 0x08, 0x98, 0x05, 0x99, 0x03, 0x9B, 0xCF, 0x9C, 0x10, 0x9D, 0x1C, 0x9F, 0x32, 0xA0, 0x90, 0xA2, 0xA0, 0xA3, 0x08, 0xA4,
            0x01, 0xA8, 0xF6, 0xA9, 0x89, 0xAA, 0x0C, 0xAB, 0x32, 0xAC, 0x14, 0xAD, 0x09, 0xAE, 0xFF, 0xEB, 0x6B, 0x93, 0x10, 0x94, 0x29,
            0xA2, 0x50, 0xA4, 0x02, 0xAF, 0x01,
        ])?;
        self.page(9)?;
        self.wlist(&[0x80, 0x80, 0x81, 0xFF, 0x87, 0x07, 0x45, 0xA1, 0xDD, 0xD0, 0x39, 0x07, 0xE6, 0x10, 0xA5, 0xA0])?;
        self.page(9)?;
        self.wreg(0x46, 0x00)?;
        self.wlist(&[0x09, 0x00, 0x0B, 0x00])?;
        self.page(7)?;
        self.wlist(&[0x28, 0x01, 0x2A, 0x13])?;
        self.page(7)?;
        self.wreg(0x29, 0x08)?;
        self.page(9)?;
        let v = self.r1(0x31)?;
        self.wreg(0x31, v & 0xFC)?;
        self.page(7)?;
        self.wlist(&[0x77, 0x14, 0x04, 0x28, 0x0C, 0xF5])?;
        self.rf_init_dab()?;
        sleep(100);
        self.page(0x0F)?;
        self.wreg(0x6B, 0xC5)?;
        Ok(checks)
    }

    fn rf_init_dab(&mut self) -> Result<(), String> {
        self.band_mode = 1;
        self.page(0x0F)?;
        self.wlist(&RF_INIT_DAB)?;
        self.wlist(&RF_INIT_DAB_TAIL)?;
        let hi = self.r1(0x10)? as u16;
        let lo = self.r1(0x11)? as u16;
        if (hi << 8 | lo) == 0xFFFF {
            self.wlist(&[0x2C, 200, 0x47, 0xB0, 0x35, 0x04, 0x2A, 0x07, 0x2D, 0xEC, 0x61, 0x2A])
        } else {
            self.wlist(&[0x2C, 0x48, 0x47, 0xE0, 0x35, 0x04, 0x2A, 0x05, 0x2D, 0x8C, 0x61, 0x25])
        }
    }

    fn demod_restart(&mut self) -> Result<(), String> {
        self.page(6)?;
        self.wreg(0x10, 0x48)?;
        sleep(1);
        self.wreg(0x10, 0xC9)
    }

    fn demod_restart_no_wait(&mut self) -> Result<(), String> {
        self.page(6)?;
        self.wreg(0x10, 0x48)?;
        self.wreg(0x10, 0xC9)
    }

    /// 0, -13 (no PLL lock) or -14 (clock plan).
    fn pll_band3(&mut self, f: u32) -> Result<i32, String> {
        if self.band_mode != 1 {
            self.page(0x0F)?;
            self.wlist(&RF_BAND3)?;
            self.band_mode = 1;
        }
        let i = band3_index(f).ok_or_else(|| format!("{f} kHz is not a band III channel"))?;
        let w = BAND3_PLL[i] as u64;
        let p = BAND3_PLAN[i];
        let ratio = w as f64 / f as f64;
        if !(341.0..=341.8).contains(&ratio) {
            return Err(format!("PLL word {w:#x} implausible for {f} kHz (index {i})"));
        }
        let rc = self.set_clock_plan(p)?;
        if rc != 0 {
            return Ok(rc);
        }
        self.page(0x0F)?;
        self.wlist(&[0x23, (w >> 22 & 0xFF) as u8, 0x24, (w >> 14 & 0xFF) as u8, 0x25, (w >> 6 & 0xFF) as u8])?;
        for r in [0x23, 0x24, 0x25] {
            self.rreg(r, 1)?;
        }
        self.wreg(0x26, ((w * 4) | 1) as u8)?;
        sleep(20);
        self.wreg(0x20, 0x00)?;
        sleep(20);
        let mut locked = false;
        for _ in 0..10 {
            let r15 = self.r1(0x15)? as u64;
            let r12 = self.r1(0x12)? as u64;
            let r13 = self.r1(0x13)? as u64;
            let r14 = self.r1(0x14)? as u64;
            let rb = r12 * 0x400000 + r13 * 0x4000 + r14 * 0x40 + (r15 >> 2 & 0x3F);
            if (w ^ rb) & !1u64 == 0 || w >> 1 == rb {
                if r15 & 2 != 0 {
                    locked = true;
                    break;
                }
                self.wreg(0x20, 0x00)?;
                sleep(20);
            } else {
                self.wreg(0x20, 0x00)?;
                sleep(10);
            }
        }
        self.demod_restart()?;
        if !locked {
            return Ok(-13);
        }
        sleep(1);
        Ok(0)
    }

    /// The vendor's tune (band III): rc 0 = locked and accepted, -4 = rejected,
    /// -13 = PLL, -14 = clock plan. The sync poll is capped by time.
    fn tune_once(&mut self, f: u32, sync_timeout: Duration) -> Result<(i32, TuneInfo), String> {
        let mut info = TuneInfo::default();
        macro_rules! fail {
            ($($why:tt)*) => {{
                info.why = Some(format!($($why)*));
                return Ok((-4, info));
            }};
        }
        let rc = self.pll_band3(f)?;
        if rc != 0 {
            info.why = Some(if rc == CLOCK_PLAN_FAILED { "clock plan not applied" } else { "pll" }.into());
            return Ok((rc, info));
        }
        self.page(4)?;
        self.wreg(0x55, 0x0D)?;
        self.page(6)?;
        let r54 = match self.plan {
            Some(0) => 0x7B,
            Some(1) => 0x78,
            Some(3) => 0x7A,
            _ => 0x78,
        };
        self.wreg(0x54, r54)?;
        self.wreg(0x5A, 0x1B)?;
        self.page(0x0F)?;
        self.wreg(0x4B, 0x8F)?;
        self.demod_restart()?;

        let deadline = Instant::now() + sync_timeout;
        let (th, r38, r39, r66, r67);
        loop {
            info.polls += 1;
            self.page(6)?;
            let sync = self.r1(0xCF)?;
            if sync == 0xFF {
                fail!("sync reg 0xff");
            }
            self.page(4)?;
            let a = self.r1(0x38)? as u32;
            let b = self.r1(0x39)? as u32;
            self.page(6)?;
            let v = self.r1(0x53)?;
            self.wreg(0x53, v | 0x80)?;
            let c = self.r1(0x66)? as u32;
            let d = self.r1(0x67)? as u32;
            let mode = ((self.r1(0x27)? & 0x30) >> 4) as usize;
            self.wreg(0x1A, MODE_1A[mode])?;
            let Some(t) = thresholds(self.plan, mode, f) else { fail!("clock plan") };
            info.sync = Some(sync);
            info.mode_bits = Some(mode as u8);
            if sync == 3 {
                (th, r38, r39, r66, r67) = (t, a, b, c, d);
                break;
            }
            if sync == 1 || Instant::now() > deadline {
                self.page(6)?;
                self.wreg(0x5A, 0x1C)?;
                if sync == 1 {
                    fail!("no sync");
                }
                fail!("sync timeout");
            }
        }
        let [u15, u17, lo, hi, mut u20, u22] = th;
        let u1 = (r39 << 8) | r38;
        let u21 = (r67 * 4) | (r66 & 3);
        info.u1 = Some(u1);
        info.u21 = Some(u21);
        self.page(6)?;
        self.wreg(0x5A, 0x1C)?;
        self.rreg(0x18, 1)?;
        self.page(0x0F)?;
        if self.r1(0x00)? >> 6 == 2 {
            u20 = u22;
        }
        if u21 * 10 < u20 && u21 != 0 {
            fail!("u21 {u21} * 10 < {u20}");
        }
        let b23 = u1 != 0;

        // loop A: signal level gate
        for i in 0..16 {
            self.page(0x0F)?;
            let rf0 = self.r1(0x00)?;
            let u16v = if rf0 >> 6 == 2 && i > 10 { u15 } else { u17 };
            if u1 < u16v && b23 {
                fail!("A: u1 {u1} < {u16v}");
            }
            let weak = u1 < u17;
            self.page(0x0F)?;
            self.rreg(0x00, 1)?;
            self.page(6)?;
            self.rreg(0xB1, 1)?;
            self.rreg(0xB5, 1)?;
            self.rreg(0xB6, 1)?;
            self.page(0x0F)?;
            let rf0 = self.r1(0x00)?;
            if rf0 >> 6 != 2 && weak && i > 0x0D && b23 {
                fail!("A: weak");
            }
            if i == 15 {
                break;
            }
            sleep(10);
        }

        // loop B: settle and latch bits
        let (mut l1d, mut cnt37, mut cnt30, mut l1f) = (0u32, 0u32, 0u32, 0u32);
        let mut i = 0u32;
        loop {
            sleep(10);
            self.page(6)?;
            let r18 = self.r1(0x18)? as u32;
            self.page(0x0F)?;
            let rf0 = self.r1(0x00)?;
            if u1 < u17 && b23 && rf0 >> 6 != 2 && i > 0x19 && (r18 < lo || hi < r18) {
                fail!("B: reg18 {r18} outside {lo}..{hi}");
            }
            if u1 < u17 * 2 && b23 && rf0 >> 6 == 2 && i > 0x19 && (r18 < lo || hi < r18) {
                fail!("B: reg18 {r18} outside {lo}..{hi} (rf0 mode 2)");
            }
            self.page(6)?;
            let r30 = self.r1(0x30)? & 0x0F;
            if (r30 > 9 && cnt30 > 4) || (r30 > 8 && cnt30 > 6) {
                fail!("B: reg30 {r30}");
            }
            if r30 > 7 {
                if cnt30 > 8 && i > 0x0F {
                    fail!("B: reg30 {r30} persistent");
                }
                cnt30 += 1;
            }
            self.page(6)?;
            let r37 = self.r1(0x37)? & 7;
            let mut check = true;
            if u1 < 3000 && u21 < 0x226 {
                check = r37 == 1;
                if check {
                    cnt37 += 1;
                }
            }
            if check && r37 == 1 && cnt37 > 0x14 && l1f < 2 && i > 0x1E {
                fail!("B: reg37 == 1 persistent");
            }
            l1f = (self.r1(0x17)? & 0x1F) as u32;
            let r18b = self.r1(0x18)? as u32;
            let v = self.r1(0x82)?;
            self.wreg(0x82, v | 1)?;
            let r7e = self.r1(0x7E)? as u32;
            let r7f = self.r1(0x7F)? as u32;
            let v = self.r1(0x1C)?;
            self.wreg(0x1C, v | 0x10)?;
            let r26 = self.r1(0x26)? as u32;
            let r27 = self.r1(0x27)? as u32;
            let v = self.r1(0x44)?;
            self.wreg(0x44, v | 0x80)?;
            let r4c = self.r1(0x4C)? as i32;
            let r4d = self.r1(0x4D)? as i32;
            if l1f == 1 && r37 == 6 {
                if i > 0x0F && l1d > 1 {
                    fail!("B: l1d");
                }
                if ((r4c - 0x41) + (r4d & 1) * 0x100) & 0xFFFF > 0x69 || (((r27 & 0x0F) << 8) | r26) == 0 {
                    l1d += 1;
                }
            } else if l1f > 1 && (r18b < lo || hi < r18b) && (((r7f & 0x1F) << 8) | r7e) <= 0x8B {
                break;
            }
            i += 1;
            if i == 0x4B {
                fail!("B: no settle in 75 rounds");
            }
        }
        info.loop_b_rounds = Some(i + 1);

        let mut u15b = 0u32;
        for left in (1..=40).rev() {
            sleep(10);
            self.page(6)?;
            let r12 = self.r1(0x12)?;
            let v = self.r1(0x82)?;
            self.wreg(0x82, v | 1)?;
            let a = self.r1(0x7E)? as u32;
            let b = self.r1(0x7F)? as u32;
            u15b = a + (b & 0x1F) * 0x100;
            if u15b != 0 && r12 & 0x80 != 0 {
                self.page(6)?;
                self.wreg(0x54, 0x58)?;
                self.page(0x0F)?;
                self.wreg(0x4B, 0x80)?;
                self.page(4)?;
                self.wreg(0x55, 0x15)?;
                break;
            }
            if left == 1 {
                if r12 & 0x80 == 0 {
                    self.page(6)?;
                    self.rreg(0x18, 1)?;
                    fail!("final: reg12 bit7 never set");
                }
                break;
            }
        }
        info.u15 = Some(u15b);

        // quality verdict
        self.page(9)?;
        let fb = self.r1(0xFB)?;
        let mut q4 = [self.r1(0x83)? as u32, self.r1(0x84)? as u32, self.r1(0x85)? as u32, self.r1(0x86)? as u32];
        if fb & 3 != 3 {
            self.page(6)?;
            self.rreg(0x18, 1)?;
            self.page(9)?;
            q4 = [self.r1(0x83)? as u32, self.r1(0x84)? as u32, self.r1(0x85)? as u32, self.r1(0x86)? as u32];
        }
        let den = (q4[0] << 8) | q4[1];
        if den == 0 {
            fail!("quality: zero denominator");
        }
        let q = ((q4[2] << 8) | q4[3]) * 1000 / den;
        info.q = Some(q);
        if fb & 3 == 3 {
            if q < 0xC4 || u15b > 0xB3 {
                return Ok((0, info));
            }
        } else if q < 0xC3 && u15b < 0xB4 {
            return Ok((0, info));
        }
        fail!("quality q={q} u15={u15b}");
    }

    fn fic_enable(&mut self) -> Result<(), String> {
        if self.fic_open {
            return Ok(());
        }
        self.fic_open = true;
        self.page(9)?;
        self.wreg(0x35, 0x01)?;
        self.page(7)?;
        self.irq_mask &= 0xFE;
        let m = self.irq_mask;
        self.wreg(0x62, m)?;
        self.page(9)?;
        self.wreg(0x46, 0x14)?;
        self.wreg(0x46, 0x16)
    }

    /// Tunes to a band III centre frequency (kHz). `retries`: try a marginal
    /// lock again (slower; for a scan).
    pub fn tune(&mut self, khz: u32, retries: bool) -> Result<Tuned, String> {
        self.streaming = false;
        let (mut rc, mut info) = self.tune_once(khz, Duration::from_millis(1500))?;
        let mut tries = 1;
        loop {
            let max = if info.u1.unwrap_or(0) >= 1000 {
                3
            } else if info.sync == Some(3) {
                2
            } else {
                1
            };
            if rc == 0 || !retries || tries >= max {
                break;
            }
            (rc, info) = self.tune_once(khz, Duration::from_millis(1500))?;
            tries += 1;
        }
        let locked = rc == 0;
        if locked && !self.fic_enabled {
            self.fic_enable()?;
            self.fic_enabled = true;
        }
        let u1 = info.u1.unwrap_or(0);
        let bars = if !locked {
            0
        } else if u1 < 4000 {
            1
        } else if u1 < 9000 {
            2
        } else if u1 < 15000 {
            3
        } else {
            4
        };
        self.chip_sig = None;
        self.mcu_signal = None;
        Ok(Tuned { locked, bars, detail: format!("{info:?}") })
    }

    /// FIC bytes (12 FIBs, 384 bytes) or None when no new FIC is ready.
    pub fn fic_read(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.page(9)?;
        let st = self.rreg(0x33, 3)?;
        self.last_chip_status = st[0];
        if st[0] & 1 == 0 {
            return Ok(None);
        }
        self.page(0x0A)?;
        let data = self.rreg(0x10, 0x180)?;
        self.page(9)?;
        self.wreg(0x35, 0x01)?;
        Ok(Some(data))
    }

    /// Streams one sub-channel of the tuned ensemble.
    pub fn start_audio(&mut self, subchannel: u8, bitrate_kbps: u16) -> Result<(), String> {
        let n = bitrate_kbps as usize * 3; // bytes per 24 ms CIF
        if subchannel > 0x3F || !(1..=0xD38).contains(&n) {
            return Err(format!("bad sub-channel {subchannel} / size {n}"));
        }
        self.demod_restart_no_wait()?;
        self.page(9)?;
        if self.chip_audio {
            self.wlist(&[0x56, 0x08, 0x57, 0x00])?;
        } else {
            self.wlist(&[0x56, (n >> 8 & 0x0F) as u8, 0x57, (n & 0xFF) as u8])?;
        }
        self.page(9)?;
        self.wlist(&[0x48, 0x00, 0x48, 0x05])?;
        self.page(9)?;
        self.wreg(0x35, 0x70)?;
        self.page(7)?;
        self.irq_mask &= 0x8F;
        let m = self.irq_mask;
        self.wreg(0x62, m)?;
        self.page(9)?;
        self.wreg(0x3A, 0x80 | subchannel)?;
        self.page(9)?;
        self.wreg(0x35, 0x7E)?;
        self.page(7)?;
        self.wreg(0x62, m)?;
        self.msc_size = n;
        self.subch = Some(subchannel);
        self.audio_at = None;
        if self.chip_audio {
            return Ok(());
        }
        if self.fw > 1 {
            let r = self.cmd([0x41, 0, (n & 0xFF) as u8, (n >> 8) as u8], 4)?;
            Self::expect(&r, 0xC1, "set msc size")?;
        }
        if self.fw > 4 {
            let r = self.cmd([0x43, 0, 0, 1], 4)?;
            Self::expect(&r, 0xC3, "msc flag on")?;
        }
        if self.fw > 1 && !self.mcu_delivered {
            self.audio_at = Some(Instant::now());
        }
        self.streaming = true;
        Ok(())
    }

    /// From now on audio comes from the chip's registers, not the controller's buffer.
    fn use_chip_audio(&mut self) -> Result<(), String> {
        if self.chip_audio {
            return Ok(());
        }
        self.chip_audio = true;
        if self.subch.is_none() {
            return Ok(());
        }
        if self.fw > 4 {
            let r = self.cmd([0x43, 0, 0, 0], 4)?;
            Self::expect(&r, 0xC3, "msc flag off")?;
        } else if self.fw > 1 {
            let r = self.cmd([0x41, 0, 0, 0], 4)?;
            Self::expect(&r, 0xC1, "stop msc")?;
        }
        self.page(9)?;
        self.wlist(&[0x56, 0x08, 0x57, 0x00, 0x48, 0x00, 0x48, 0x05, 0x35, 0x04])
    }

    fn next_seq(&mut self) -> u8 {
        let s = self.msc_seq;
        self.msc_seq = s.wrapping_add(1);
        s
    }

    pub fn stop_audio(&mut self) -> Result<(), String> {
        self.audio_at = None;
        self.streaming = false;
        if let Some(s) = self.subch.take() {
            self.page(9)?;
            self.wreg(0x3A, s)?;
            self.page(7)?;
            self.irq_mask |= 0x70;
            let m = self.irq_mask;
            self.wreg(0x62, m)?;
            self.page(7)?;
            self.irq_mask |= 0x0E;
            let m = self.irq_mask;
            self.wreg(0x62, m)?;
        }
        if self.fw > 1 {
            for _ in 0..64 {
                let seq = self.next_seq();
                let st = self.cmd([0x42, 0, 0, seq], 6)?;
                let avail = if st.len() >= 4 { st[2] as usize | (st[3] as usize) << 8 } else { 0 };
                if avail == 0 {
                    break;
                }
                let m = avail.min(0x480);
                let r = self.cmd([0x41, 0, (m & 0xFF) as u8, (m >> 8) as u8], 4)?;
                Self::expect(&r, 0xC1, "flush size")?;
                self.xfer(&[0x40, 0, (m & 0xFF) as u8, (m >> 8) as u8], m + 4, 1000, 1000)?;
            }
        }
        self.msc_size = 0;
        if self.fw > 4 {
            let r = self.cmd([0x43, 0, 0, 0], 4)?;
            Self::expect(&r, 0xC3, "msc flag off")?;
        } else if self.fw > 1 {
            let r = self.cmd([0x41, 0, 0, 0], 4)?;
            Self::expect(&r, 0xC1, "stop msc")?;
        }
        Ok(())
    }

    fn msc_read_raw(&mut self, max_len: usize) -> Result<Vec<u8>, String> {
        if self.fw <= 1 || self.chip_audio {
            self.page(9)?;
            let st = self.rreg(0x33, 3)?[0];
            self.last_chip_status = st;
            if st & 0x70 == 0 {
                return Ok(Vec::new());
            }
            if st & 0x60 != 0 {
                // under/overrun: the buffer starts again
                self.overruns += 1;
                self.wlist(&[0x48, 0x00, 0x48, 0x05])?;
                self.page(9)?;
                self.wreg(0x35, 0x04)?;
                return Ok(Vec::new());
            }
            self.page(0x0C)?;
            let n = if self.chip_audio { CHIP_READ } else { self.msc_size };
            let data = self.rreg(0x10, n)?;
            self.page(9)?;
            self.wreg(0x35, 0x04)?;
            return Ok(data);
        }
        let seq = self.next_seq();
        let st = self.cmd([0x42, 0, 0, seq], 6)?;
        if st.len() >= 6 {
            self.mcu_signal = Some(st[4] as u32 | (st[5] as u32) << 8);
        }
        let avail = if st.len() >= 4 { st[2] as usize | (st[3] as usize) << 8 } else { 0 };
        self.last_mcu_status = st;
        if avail == 0 {
            return Ok(Vec::new());
        }
        let m = avail.min(max_len);
        if m < self.msc_size {
            return Ok(Vec::new());
        }
        let r = self.xfer(&[0x40, 0, (m & 0xFF) as u8, (m >> 8) as u8], m + 4, 1000, 1000)?;
        Self::expect(&r, 0xC0, "msc read")?;
        if r.len() < 4 {
            return Err(format!("msc read: short reply {}", hex(&r)));
        }
        let got = (r[2] as usize | (r[3] as usize) << 8).min(r.len() - 4);
        Ok(r[4..4 + got].to_vec())
    }

    /// Sub-channel bytes received since the last call (possibly none). A stick
    /// whose controller passes no audio on within 2 s of the first start is
    /// read from the chip instead, as DABdream+ reads every C100.
    pub fn msc_read(&mut self) -> Result<Vec<u8>, String> {
        let data = self.msc_read_raw(4096)?;
        if self.chip_audio {
            // (a dozen register reads: not too often next to the audio)
            if self.chip_sig_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(1000)) {
                self.chip_sig_at = Some(Instant::now());
                self.chip_sig = Some(self.chip_signal()?);
            }
        } else if !data.is_empty() {
            self.mcu_delivered = true;
            self.audio_at = None;
        } else if self.audio_at.is_some_and(|t| t.elapsed() >= MCU_GRACE) {
            self.audio_at = None;
            log::warn!(
                "DTR-100: no audio from the stick's controller {} s after the start (status {}, chip status {:#04x}): reading it from the DAB chip instead",
                MCU_GRACE.as_secs(),
                hex(&self.last_mcu_status),
                self.last_chip_status
            );
            CHIP_AUDIO_NEEDED.store(true, Ordering::Relaxed);
            self.use_chip_audio()?;
        }
        Ok(data)
    }

    /// The chip's CER from its bit counters (0 = perfect, 2000 = nothing);
    /// resets the counters when DABdream+ does.
    fn chip_signal(&mut self) -> Result<u32, String> {
        self.page(9)?;
        if self.r1(0x37)? & 1 == 0 {
            return Ok(SIGNAL_NONE);
        }
        let mut tot = 0u64;
        for r in 0x88..=0x8B {
            tot = tot << 8 | self.r1(r)? as u64;
        }
        let mut err = 0u64;
        for r in 0x8C..=0x8F {
            err = err << 8 | self.r1(r)? as u64;
        }
        let d7 = self.r1(0xD7)?;
        let cer = if tot == 0 {
            SIGNAL_NONE
        } else if err <= 4000 {
            0
        } else {
            let c = err * 1000 / tot * 10;
            if c < 1201 {
                c as u32
            } else {
                SIGNAL_NONE
            }
        };
        if tot == 0 || d7 & 0x10 == 0 || cer == SIGNAL_NONE {
            self.page(9)?;
            self.wlist(&[0x46, 0x1E, 0x35, 0x01, 0x46, 0x16])?;
        }
        Ok(cer)
    }

    /// Reception from the last status poll (free to read); None before the first.
    pub fn signal(&self) -> Option<Signal> {
        let cer = if self.chip_audio { self.chip_sig } else { self.mcu_signal }?;
        Some(Signal { bars: signal_bars(cer), none: cer >= SIGNAL_NONE, weak: cer >= SIGNAL_WEAK, cer })
    }
}

/// Bars from the CER, after RAON's own antenna-level table.
pub fn signal_bars(cer: u32) -> u8 {
    match cer {
        c if c >= SIGNAL_NONE => 0,
        c if c >= 700 => 1,
        c if c >= 400 => 2,
        c if c >= 180 => 3,
        _ => 4,
    }
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// (u15, u17, lo, hi, u20, u22) for the tune's quality gates.
fn thresholds(plan: Option<u8>, mode: usize, f: u32) -> Option<[u32; 6]> {
    let low = f < 0x3D091;
    let (u22, u20, s2, s3) = match plan? {
        0 | 1 => {
            (0x9B5 + if low { 0xF5 } else { 0 }, 0x163 + if low { 0x23 } else { 0 }, [0x50, 0x140, 0x08, 0xF8], [1000, 4000, 0x1A, 0xE6])
        }
        2 | 3 => {
            (0x9B5 + if low { 0xAF } else { 0 }, 0x163 + if low { 0x19 } else { 0 }, [100, 400, 0x08, 0xF8], [0x4E2, 5000, 0x1A, 0xE6])
        }
        _ => return None,
    };
    let s = match mode {
        2 => s2,
        3 => s3,
        1 => [0xED, 0x3B6, 0x0E, 0xF2],
        _ => [0x659, 0x1964, 0x37, 0xCE],
    };
    Some([s[0], s[1], s[2], s[3], u20, u22])
}

/// Index in the band III tables (5A..13F with the N channels).
pub fn band3_index(f: u32) -> Option<usize> {
    if !(174_000..=240_000).contains(&f) {
        return None;
    }
    let u = (f as i64 - 174_928).div_euclid(1712);
    let i = if f < 224_096 {
        if f < 217_008 {
            u + 1 - if f < 210_096 { 1 } else { 0 }
        } else {
            u + 2
        }
    } else {
        u + 3
    };
    usize::try_from(i).ok().filter(|i| *i < BAND3_PLL.len())
}

const MODE_1A: [u8; 4] = [0xB4, 0x44, 0x04, 0x84];

/// Per clock plan: page 4 reg 0x6a, page 6 regs, (0x40, 0x41), 0x42.
const PLAN_REGS: [(u8, [u8; 8], (u8, u8), u8); 4] = [
    (0x01, [0x3C, 0x4B, 0x3D, 0x37, 0x3E, 0x89, 0x3F, 0x41], (0x8F, 0xC2), 0xF5),
    (0x01, [0x3C, 0x00, 0x3D, 0x00, 0x3E, 0x00, 0x3F, 0x40], (0x00, 0x00), 0xF0),
    (0x21, [0x3C, 0xB5, 0x3D, 0x14, 0x3E, 0x41, 0x3F, 0x3A], (0x0D, 0x74), 0xDA),
    (0x31, [0x3C, 0x69, 0x3D, 0x03, 0x3E, 0x9D, 0x3F, 0x36], (0xCC, 0xCC), 0xCC),
];

// register tables (from DABplay+ Dtr100Tables.kt, generated from the vendor library)

/// RF init: (register, value) pairs.
const RF_INIT_DAB: [u8; 198] = [
    0x27, 0x96, 0x2B, 0x88, 0x2D, 0xEC, 0x2E, 0xB0, 0x31, 0x04, 0x34, 0xF8, 0x35, 0x14, 0x3A, 0x62, 0x3B, 0x74, 0x3C, 0xA8, 0x43, 0x36,
    0x44, 0x70, 0x46, 0x07, 0x49, 0x1F, 0x4A, 0x50, 0x4B, 0x80, 0x53, 0x20, 0x55, 0xD6, 0x5A, 0x83, 0x60, 0x13, 0x6B, 0x85, 0x6E, 0x88,
    0x6F, 0x78, 0x72, 0xF0, 0x73, 0xCA, 0x77, 0x80, 0x78, 0x47, 0x84, 0x80, 0x85, 0x90, 0x86, 0x80, 0x87, 0x98, 0x8A, 0xF6, 0x8B, 0x80,
    0x8C, 0x75, 0x8F, 0xF9, 0x90, 0x91, 0x93, 0x2D, 0x94, 0x23, 0x99, 0x77, 0x9A, 0x2D, 0x9B, 0x1E, 0x9C, 0x47, 0x9D, 0x3A, 0x9E, 0x03,
    0x9F, 0x1E, 0xA0, 0x22, 0xA1, 0x33, 0xA2, 0x51, 0xA3, 0x36, 0xA4, 0x0C, 0xAE, 0x37, 0xB5, 0x9B, 0xBD, 0x45, 0xBE, 0x68, 0xBF, 0x5C,
    0xC0, 0x33, 0xC1, 0xCA, 0xC2, 0x43, 0xC3, 0x90, 0xC4, 0xEC, 0xC5, 0x17, 0xC6, 0xDA, 0xC7, 0x41, 0xC8, 0x49, 0xC9, 0xBC, 0xCA, 0x3A,
    0xCB, 0x20, 0xCC, 0x43, 0xCD, 0xA6, 0xCE, 0x4C, 0xCF, 0x1F, 0xD0, 0x8D, 0xD1, 0x3D, 0xD2, 0xDF, 0xD3, 0xA4, 0xD4, 0x30, 0xD5, 0x00,
    0xD6, 0x41, 0xD7, 0x81, 0xD8, 0xD0, 0xD9, 0x00, 0xDA, 0x00, 0xDB, 0x41, 0xDC, 0x6D, 0xDD, 0xAC, 0xDE, 0x39, 0xDF, 0x4E, 0xE0, 0x43,
    0xE1, 0xA0, 0xE2, 0x4C, 0xE3, 0x1D, 0xE4, 0x99, 0xE5, 0x3B, 0xE6, 0x00, 0xE7, 0x0D, 0xA5, 0x00, 0xE9, 0x41, 0xAE, 0x77, 0xE9, 0x51,
];

const RF_INIT_DAB_TAIL: [u8; 4] = [0x37, 0x3B, 0x39, 0x2C];

/// Band III RF settings: (register, value) pairs.
const RF_BAND3: [u8; 44] = [
    0x27, 0x96, 0x34, 0xF8, 0x3B, 0x74, 0x43, 0x36, 0x44, 0x70, 0x46, 0x07, 0x49, 0x1F, 0x4A, 0x50, 0x55, 0xD6, 0x60, 0x13, 0x6E, 0x88,
    0x6F, 0x78, 0x84, 0x80, 0x85, 0x90, 0x86, 0x80, 0x87, 0x98, 0x8B, 0x80, 0x8C, 0x75, 0x8F, 0xF9, 0x90, 0x91, 0x93, 0x2D, 0x94, 0x23,
];

/// 7 bytes each, page 0x0F regs 0xE8..0xEE; 0xFF in byte 0 = invalid plan.
const CLOCK_PLANS: [[u8; 7]; 4] = [
    [0x08, 0x01, 0x13, 0x25, 0x06, 0x7D, 0xB4],
    [0x06, 0x01, 0x0F, 0x27, 0x07, 0x60, 0xB8],
    [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    [0x0C, 0x01, 0x1F, 0x27, 0x06, 0xE1, 0xB8],
];

/// PLL word per band III channel (41 entries, 5A..13F with 10N, 11N, 12N).
const BAND3_PLL: [u32; 41] = [
    0x38F3D55, 0x3982800, 0x3A112AA, 0x3A9FD55, 0x3B3BD55, 0x3BCA800, 0x3C592AA, 0x3CE7D55, 0x3D82800, 0x3E112AA, 0x3E9FD55, 0x3F2E800,
    0x3FCA800, 0x40592AA, 0x40E7D55, 0x4176800, 0x42112AA, 0x429FD55, 0x432E800, 0x43BD2AA, 0x44592AA, 0x4466800, 0x44E7D55, 0x4576800,
    0x46052AA, 0x469FD55, 0x46AD2AA, 0x472E800, 0x47BD2AA, 0x484BD55, 0x48E7D55, 0x48F52AA, 0x4976800, 0x4A052AA, 0x4A93D55, 0x4B22800,
    0x4BB12AA, 0x4C3FD55, 0x4CC2800, 0x4D512AA, 0x4DDFD55,
];

/// Clock plan per band III channel.
const BAND3_PLAN: [u8; 41] =
    [3, 1, 1, 3, 1, 3, 1, 1, 0, 1, 1, 1, 0, 1, 1, 0, 1, 0, 1, 3, 1, 1, 1, 0, 1, 3, 1, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 1];

#[cfg(test)]
mod tests {
    use super::*;

    /// Band III channels in table order (frequencies in kHz).
    const CHANNELS: [(&str, u32); 41] = [
        ("5A", 174928),
        ("5B", 176640),
        ("5C", 178352),
        ("5D", 180064),
        ("6A", 181936),
        ("6B", 183648),
        ("6C", 185360),
        ("6D", 187072),
        ("7A", 188928),
        ("7B", 190640),
        ("7C", 192352),
        ("7D", 194064),
        ("8A", 195936),
        ("8B", 197648),
        ("8C", 199360),
        ("8D", 201072),
        ("9A", 202928),
        ("9B", 204640),
        ("9C", 206352),
        ("9D", 208064),
        ("10A", 209936),
        ("10N", 210096),
        ("10B", 211648),
        ("10C", 213360),
        ("10D", 215072),
        ("11A", 216928),
        ("11N", 217088),
        ("11B", 218640),
        ("11C", 220352),
        ("11D", 222064),
        ("12A", 223936),
        ("12N", 224096),
        ("12B", 225648),
        ("12C", 227360),
        ("12D", 229072),
        ("13A", 230784),
        ("13B", 232496),
        ("13C", 234208),
        ("13D", 235776),
        ("13E", 237488),
        ("13F", 239200),
    ];

    #[test]
    fn every_channel_maps_to_its_own_pll_word() {
        for (i, (name, f)) in CHANNELS.iter().enumerate() {
            assert_eq!(band3_index(*f), Some(i), "{name}");
            let ratio = BAND3_PLL[i] as f64 / *f as f64;
            assert!((341.0..=341.8).contains(&ratio), "{name}: {ratio}");
            assert_ne!(CLOCK_PLANS[BAND3_PLAN[i] as usize][0], 0xFF, "{name}");
        }
        assert_eq!(band3_index(100_000), None);
    }

    #[test]
    fn bars_follow_the_cer() {
        assert_eq!(signal_bars(0), 4);
        assert_eq!(signal_bars(250), 3);
        assert_eq!(signal_bars(500), 2);
        assert_eq!(signal_bars(900), 1);
        assert_eq!(signal_bars(SIGNAL_NONE), 0);
    }

    #[test]
    fn tables_are_register_pairs() {
        assert_eq!(RF_INIT_DAB.len() % 2, 0);
        assert_eq!(RF_BAND3.len() % 2, 0);
        assert!(thresholds(Some(2), 0, 200_000).is_some());
        assert!(thresholds(None, 0, 200_000).is_none());
    }
}
