//! The program as a Windows application (feature `desktop`): the web interface
//! in a window of its own (WebView2, part of Windows 10 and 11) and an icon
//! next to the clock. Closing the window asks whether to hide it (the receiver
//! keeps serving the network, optionally playing here too) or to quit.
//!
//! The window logs in as admin with a token only it knows, and a second start
//! of the program shows the window of the running one. The Android app's
//! WebView logs in the same way, with the token the app gives the server.

use std::path::Path;
use std::sync::OnceLock;

/// Token of the window (admin) and of this run of the program (show requests).
static TOKEN: OnceLock<String> = OnceLock::new();

fn random() -> String {
    (0..32).map(|_| char::from(b'a' + rand::random::<u8>() % 26)).collect()
}

pub fn token() -> &'static str {
    TOKEN.get_or_init(|| {
        // Android: the token of the app's WebView, which outlives a server
        // started again after a crash (no other app sees the environment)
        std::env::var("MAGICDAB_TOKEN").ok().filter(|t| cfg!(target_os = "android") && t.len() >= 16).unwrap_or_else(random)
    })
}

/// A login with the token of the window.
pub fn is_token(t: &str) -> bool {
    TOKEN.get().is_some_and(|x| !t.is_empty() && x == t)
}

/// Where a second start finds the token to reach this one.
pub fn instance_file(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("instance.token")
}

#[cfg(all(windows, feature = "desktop"))]
mod window;
#[cfg(all(windows, feature = "desktop"))]
pub use window::{hidden, message, quit_requested, run, show};

/// Position and inner size of the window (physical pixels) that fit the
/// desktop `area` of a monitor (left, top, right, bottom, without the
/// taskbar), with `frame` for the borders and the title bar: no bigger than
/// the area and inside it; without a position, centred.
#[cfg_attr(not(all(windows, feature = "desktop")), allow(dead_code))]
pub fn fit_window(pos: Option<(i32, i32)>, size: (u32, u32), frame: (u32, u32), area: (i32, i32, i32, i32)) -> ((i32, i32), (u32, u32)) {
    let (l, t, r, b) = area;
    let (aw, ah) = ((r - l).max(0) as u32, (b - t).max(0) as u32);
    let w = size.0.min(aw.saturating_sub(frame.0)).max(400.min(aw));
    let h = size.1.min(ah.saturating_sub(frame.1)).max(300.min(ah));
    let (ow, oh) = ((w + frame.0) as i32, (h + frame.1) as i32);
    let pos = match pos {
        Some((x, y)) => (x.clamp(l, (r - ow).max(l)), y.clamp(t, (b - oh).max(t))),
        None => (l + ((r - l) - ow).max(0) / 2, t + ((b - t) - oh).max(0) / 2),
    };
    (pos, (w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_fits_the_screen() {
        // first start on a laptop, 1366 x 768 with the taskbar: 1280 x 820 does not fit
        assert_eq!(fit_window(None, (1280, 820), (16, 48), (0, 0, 1366, 728)), ((35, 0), (1280, 680)));
        // the same at 150 %: the wanted size in pixels, shrunk to the desktop
        assert_eq!(fit_window(None, (1920, 1230), (24, 72), (0, 0, 1920, 1032)), ((0, 0), (1896, 960)));
        // a saved window bigger than the screen, partly outside it
        assert_eq!(fit_window(Some((100, 50)), (1900, 1000), (16, 48), (0, 0, 1920, 1040)), ((4, 0), (1900, 992)));
        // on the second monitor it stays there
        assert_eq!(fit_window(Some((2000, 100)), (1200, 800), (16, 48), (1920, 0, 3840, 1040)), ((2000, 100), (1200, 800)));
        // moved off the bottom right corner: back inside
        assert_eq!(fit_window(Some((1800, 900)), (1200, 800), (16, 48), (0, 0, 1920, 1040)), ((704, 192), (1200, 800)));
    }
}
