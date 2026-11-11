//! Android: the stick without a driver app. Apps cannot open USB devices
//! themselves (no access to /dev/bus/usb): the app asks Android (UsbManager,
//! with the user's permission) and hands the open device's file descriptor
//! to the server over a local socket (SCM_RIGHTS; a child process inherits
//! none from the app). librtlsdr then works on it (rtlsdr_open_fd,
//! build/android/rtlsdr-open-fd.patch).

/// The device opened by the app, open while librtlsdr uses it.
#[cfg(unix)]
pub type UsbFd = std::os::fd::OwnedFd;
#[cfg(not(unix))]
pub struct UsbFd;

/// Its number for librtlsdr.
#[cfg(unix)]
pub fn raw(fd: &UsbFd) -> i32 {
    std::os::fd::AsRawFd::as_raw_fd(fd)
}
#[cfg(not(unix))]
pub fn raw(_fd: &UsbFd) -> i32 {
    -1
}

#[cfg(not(unix))]
pub fn request_fd() -> Result<UsbFd, String> {
    Err("only on Android".into())
}

#[cfg(not(unix))]
pub fn list() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
pub use imp::{list, request_fd};

#[cfg(unix)]
mod imp {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// The open stick from the app (without the user's permission yet: an error
    /// at once while Android asks; the engine tries again).
    pub fn request_fd() -> Result<OwnedFd, String> {
        let name = std::env::var("MAGICDAB_USB_SOCKET").map_err(|_| "no USB bridge of the app".to_string())?;
        let s = connect(&name).map_err(|e| format!("USB bridge: {e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
        (&s).write_all(b"open\n").map_err(|e| format!("USB bridge: {e}"))?;
        let (text, fd) = receive(&s).map_err(|e| format!("USB bridge: {e}"))?;
        match fd {
            Some(fd) if text.starts_with("ok") => Ok(fd),
            _ => Err(text.trim().trim_start_matches("error:").trim().to_string()),
        }
    }

    #[cfg(target_os = "android")]
    fn connect(name: &str) -> std::io::Result<UnixStream> {
        use std::os::android::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
        UnixStream::connect_addr(&addr)
    }

    #[cfg(not(target_os = "android"))]
    fn connect(_name: &str) -> std::io::Result<UnixStream> {
        Err(std::io::Error::other("only on Android"))
    }

    /// The sticks plugged in, as the app sees them (no permission needed):
    /// "0: <manufacturer> <product>" each.
    pub fn list() -> Vec<String> {
        let ask = || -> Result<String, String> {
            let name = std::env::var("MAGICDAB_USB_SOCKET").map_err(|_| "no USB bridge".to_string())?;
            let s = connect(&name).map_err(|e| e.to_string())?;
            s.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
            (&s).write_all(b"list\n").map_err(|e| e.to_string())?;
            let mut text = String::new();
            std::io::Read::read_to_string(&mut (&s), &mut text).map_err(|e| e.to_string())?;
            Ok(text)
        };
        let Ok(text) = ask() else { return Vec::new() };
        text.lines().filter(|l| !l.trim().is_empty()).enumerate().map(|(i, l)| format!("{i}: {}", l.trim())).collect()
    }

    /// A message and the descriptor sent with it.
    fn receive(s: &UnixStream) -> std::io::Result<(String, Option<OwnedFd>)> {
        let mut data = [0u8; 512];
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut libc::c_void, iov_len: data.len() };
        // room for one descriptor
        let mut cmsg = [0u64; 8];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        let n = unsafe { libc::recvmsg(s.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fd = None;
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let raw = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const libc::c_int);
                    fd = Some(OwnedFd::from_raw_fd(raw));
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        Ok((String::from_utf8_lossy(&data[..n as usize]).to_string(), fd))
    }
}
