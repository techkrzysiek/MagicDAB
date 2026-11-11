//! Windows: an exception in the Windows Defender Firewall, so that other
//! devices of the network (phones, the other computers) reach the receiver.
//!
//! Windows asks itself the first time the program listens; whoever closed
//! that question without allowing got rules that block the program. The admin
//! can allow it from Settings: the rules of the program are replaced by one
//! named "MagicDAB" that lets it in on every network type, through netsh run
//! as administrator (Windows asks for the permission, UAC).
//!
//! Elsewhere there is no such firewall to handle: `status` is None.

use std::sync::atomic::{AtomicU8, Ordering};

/// The rule of the program: 0 not known yet, 1 there, 2 missing.
static STATE: AtomicU8 = AtomicU8::new(0);

/// Whether the rule is there (None: not Windows, or not checked yet).
pub fn status() -> Option<bool> {
    match STATE.load(Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

#[cfg(windows)]
const RULE: &str = "MagicDAB";

#[cfg(windows)]
fn netsh(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn present() -> bool {
    let there = netsh(&["advfirewall", "firewall", "show", "rule", &format!("name={RULE}")]);
    STATE.store(if there { 1 } else { 2 }, Ordering::Relaxed);
    there
}

/// Looks for the rule (netsh, a fraction of a second): at the start, in a
/// thread of its own.
pub fn check() {
    #[cfg(windows)]
    if !present() {
        log::info!("Windows firewall: no rule lets other devices reach this program (Settings > Setup)");
    }
}

/// Replaces the program's rules by one that allows it (Windows asks for the
/// administrator's permission), then waits for the rule; `done` gets whether
/// it is there. Runs in a thread of its own.
pub fn allow(done: impl FnOnce(Result<(), String>) + Send + 'static) {
    std::thread::spawn(move || done(allow_now()));
}

#[cfg(windows)]
fn allow_now() -> Result<(), String> {
    use std::ffi::c_void;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(hwnd: *mut c_void, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
    }
    const SW_HIDE: i32 = 0;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy();
    // (the program's rules from the question of Windows block it: they go)
    let params = format!(
        "/c netsh advfirewall firewall delete rule name=all program=\"{exe}\" & \
         netsh advfirewall firewall add rule name=\"{RULE}\" dir=in action=allow program=\"{exe}\" enable=yes profile=any"
    );
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("runas").as_ptr(),
            wide("cmd.exe").as_ptr(),
            wide(&params).as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };
    // (32 and less: not started; 5 is the permission refused)
    if r <= 32 {
        return Err(if r == 5 { "refused".into() } else { format!("error {r}") });
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if present() {
            log::info!("Windows firewall: other devices may reach this program now");
            return Ok(());
        }
    }
    Err("no rule after 30 s".into())
}

#[cfg(not(windows))]
fn allow_now() -> Result<(), String> {
    Err("not Windows".into())
}
