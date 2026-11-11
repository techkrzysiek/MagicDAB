//! Window, tray icon and their event loop (a thread of their own).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::windows::{EventLoopBuilderExtWindows, WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Icon, Window, WindowBuilder};
use tokio::sync::Notify;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

enum UserEvent {
    Show,
    Ipc(String),
    Menu(MenuId),
    Tray(TrayIconEvent),
    /// The page did not answer the question about closing.
    CloseTimeout(u64),
    Title(String),
}

static PROXY: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);
static HIDDEN: AtomicBool = AtomicBool::new(false);
static QUIT: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Bring the window to the front (a second start of the program).
pub fn show() {
    if let Some(p) = PROXY.lock().unwrap().as_ref() {
        let _ = p.send_event(UserEvent::Show);
    }
}

/// The window is hidden in the tray (an update starts the new version so).
pub fn hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

/// The user chose to quit.
pub async fn quit_requested() {
    QUIT.notified().await
}

fn polish() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    unsafe { GetUserDefaultUILanguage() & 0x3ff == 0x15 }
}

fn tr(pl: &'static str, en: &'static str) -> &'static str {
    if polish() {
        pl
    } else {
        en
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, utype: u32) -> i32;
    // (in user32 since Windows XP, not documented: a message box closing by itself)
    fn MessageBoxTimeoutW(hwnd: isize, text: *const u16, caption: *const u16, utype: u32, language: u16, ms: u32) -> i32;
    fn MonitorFromPoint(pt: Point, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

/// The desktop (without the taskbar) of the monitor at or nearest to a
/// point, in physical pixels (the event loop has made the process DPI aware).
fn work_area(x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    let monitor = unsafe { MonitorFromPoint(Point { x, y }, MONITOR_DEFAULTTONEAREST) };
    let mut info =
        MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0 };
    if monitor == 0
        || unsafe { GetMonitorInfoW(monitor, &mut info) } == 0
        || info.work.right <= info.work.left
        || info.work.bottom <= info.work.top
    {
        return None;
    }
    Some((info.work.left, info.work.top, info.work.right, info.work.bottom))
}
const MB_OK: u32 = 0x0;
const MB_YESNOCANCEL: u32 = 0x3;
const MB_ICONQUESTION: u32 = 0x20;
const MB_ICONWARNING: u32 = 0x30;
const IDYES: i32 = 6;
const IDNO: i32 = 7;

/// Started as a server: a note for 10 seconds; yes opens the window.
fn server_note(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let text = tr(
            "MagicDAB działa w tle jako serwer (słuchacze w sieci, ikona przy zegarze).\n\nOtworzyć okno aplikacji?",
            "MagicDAB is running in the background as a server (listeners in the network, icon next to the clock).\n\nOpen the application window?",
        );
        let (t, c) = (wide(text), wide("MagicDAB"));
        // MB_YESNO | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST
        let r = unsafe { MessageBoxTimeoutW(0, t.as_ptr(), c.as_ptr(), 0x4 | 0x40 | 0x1_0000 | 0x4_0000, 0, 10_000) };
        if r == IDYES {
            let _ = proxy.send_event(UserEvent::Show);
        }
    });
}

/// Choices of the window kept by the program (data/desktop.json): the page
/// is not there to ask when the program starts as a server.
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Prefs {
    /// Start without the window, only serving (and the tray icon).
    server_mode: bool,
}

fn load_prefs(data_dir: &std::path::Path) -> Prefs {
    std::fs::read(data_dir.join("desktop.json")).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

/// The web interface in the window (made again after closing to a server).
fn make_webview(
    window: &Window,
    context: &mut WebContext,
    port: u16,
    proxy: &EventLoopProxy<UserEvent>,
    server_mode: bool,
) -> wry::Result<WebView> {
    let (p_ipc, p_title) = (proxy.clone(), proxy.clone());
    WebViewBuilder::new_with_web_context(context)
        .with_url(format!("http://127.0.0.1:{port}/"))
        // (the admin's token for this program's page only, not for a page of
        // another site the window might come to)
        .with_initialization_script(format!(
            "if (location.origin === '{}') window.__MAGICDAB_DESKTOP__ = {{ token: '{}', serverMode: {server_mode} }};",
            if port == 80 { "http://127.0.0.1".to_string() } else { format!("http://127.0.0.1:{port}") },
            super::token()
        ))
        .with_ipc_handler(move |req| {
            let _ = p_ipc.send_event(UserEvent::Ipc(req.body().clone()));
        })
        .with_document_title_changed_handler(move |t| {
            let _ = p_title.send_event(UserEvent::Title(t));
        })
        .with_devtools(std::env::var_os("MAGICDAB_DEVTOOLS").is_some())
        .with_default_context_menus(false)
        .with_additional_browser_args(BROWSER_ARGS)
        .build(window)
}

/// Show the window, making its web interface first if it was closed (the
/// browser when WebView2 does not work).
fn open_window(
    window: &Window,
    webview: &mut Option<WebView>,
    works: &mut bool,
    context: &mut WebContext,
    port: u16,
    proxy: &EventLoopProxy<UserEvent>,
    server_mode: bool,
) {
    if webview.is_none() && *works {
        match make_webview(window, context, port, proxy, server_mode) {
            Ok(w) => *webview = Some(w),
            Err(e) => {
                log::warn!("WebView2 not available ({e}), using the browser");
                *works = false;
            }
        }
    }
    if webview.is_some() {
        show_window(window);
    } else {
        open_browser(port);
    }
}

/// A message box (errors before the window exists, WebView2 missing).
pub fn message(text: &str) {
    let (t, c) = (wide(text), wide("MagicDAB"));
    unsafe { MessageBoxW(0, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONWARNING) };
}

enum Close {
    Hide,
    Quit,
    Cancel,
}

/// The question about closing when the page cannot ask it.
fn ask_native(hwnd: isize) -> Close {
    let text = tr(
        "Zakończyć MagicDAB?\n\nTak: MagicDAB się wyłącza (dźwięk i słuchacze w sieci też)\nNie: okno znika, MagicDAB działa dalej (ikona przy zegarze)",
        "Quit MagicDAB?\n\nYes: MagicDAB ends (the sound and the listeners in the network too)\nNo: the window goes, MagicDAB runs on (icon next to the clock)",
    );
    let (t, c) = (wide(text), wide("MagicDAB"));
    match unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_YESNOCANCEL | MB_ICONQUESTION) } {
        IDYES => Close::Quit,
        IDNO => Close::Hide,
        _ => Close::Cancel,
    }
}

/// Window position and size of the last run (data/window.json).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
struct Geometry {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    maximized: bool,
}

fn open_browser(port: u16) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &format!("http://localhost:{port}/")])
        .creation_flags(0x0800_0000)
        .spawn();
}

/// Chromium flags: autoplay (a radio plays when started) and no throttling of
/// a hidden window (audio keeps playing in the tray); wry's defaults first.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion \
    --autoplay-policy=no-user-gesture-required --disable-background-timer-throttling \
    --disable-renderer-backgrounding --disable-backgrounding-occluded-windows";

/// Start the window (or only the tray icon and the browser when WebView2 is
/// not available) in a thread of its own. Should it fail, the browser: the
/// receiver must never run on unseen.
pub fn run(port: u16, data_dir: PathBuf, start_hidden: bool) {
    let _ = std::thread::Builder::new().name("window".into()).spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| event_loop(port, data_dir, start_hidden))).is_err() {
            log::error!("the window failed, using the browser");
            message(tr(
                "Okno MagicDAB nie mogło się otworzyć – interfejs otworzy się w przeglądarce.",
                "The MagicDAB window could not open – the interface opens in the browser.",
            ));
            open_browser(port);
        }
    });
}

fn show_window(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
    HIDDEN.store(false, Ordering::SeqCst);
}

fn event_loop(port: u16, data_dir: PathBuf, start_hidden: bool) {
    // COM and OLE in this thread (single-threaded apartment): WebView2 needs it
    #[link(name = "ole32")]
    extern "system" {
        fn OleInitialize(reserved: *mut std::ffi::c_void) -> i32;
    }
    unsafe { OleInitialize(std::ptr::null_mut()) };
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().with_any_thread(true).build();
    let proxy = event_loop.create_proxy();
    *PROXY.lock().unwrap() = Some(proxy.clone());

    let geometry_file = data_dir.join("window.json");
    let saved: Option<Geometry> = std::fs::read(&geometry_file).ok().and_then(|d| serde_json::from_slice(&d).ok());
    let icon64 = Icon::from_rgba(include_bytes!("../../assets/icon-64.rgba").to_vec(), 64, 64).ok();
    // (no files are dropped on the window)
    let mut builder = WindowBuilder::new().with_title("MagicDAB").with_window_icon(icon64).with_visible(false).with_drag_and_drop(false);
    // the window fits the desktop of its monitor (a laptop screen, display
    // scaling, a saved size from a bigger screen); `frame`: borders and title bar
    let scale = event_loop.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0);
    let frame = ((16.0 * scale) as u32, (48.0 * scale) as u32);
    builder = match saved {
        Some(g) => {
            let size = (g.w.max(400), g.h.max(300));
            let ((x, y), (w, h)) = match work_area(g.x + 50, g.y + 50) {
                Some(area) => super::fit_window(Some((g.x, g.y)), size, frame, area),
                None => ((g.x, g.y), size),
            };
            builder.with_inner_size(PhysicalSize::new(w, h)).with_position(PhysicalPosition::new(x, y)).with_maximized(g.maximized)
        }
        None => match work_area(0, 0) {
            Some(area) => {
                let want = ((1280.0 * scale) as u32, (820.0 * scale) as u32);
                let ((x, y), (w, h)) = super::fit_window(None, want, frame, area);
                builder.with_inner_size(PhysicalSize::new(w, h)).with_position(PhysicalPosition::new(x, y))
            }
            None => builder.with_inner_size(LogicalSize::new(1280.0, 820.0)),
        },
    };
    let window = match builder.build(&event_loop) {
        Ok(w) => w,
        Err(e) => {
            log::error!("window: {e}");
            open_browser(port);
            return;
        }
    };

    let mut context = WebContext::new(Some(data_dir.join("webview")));
    let prefs_file = data_dir.join("desktop.json");
    let mut prefs = load_prefs(&data_dir);
    // As a server the program starts without the web interface (it is made
    // when the window is opened); `webview_works` turns false when WebView2 fails.
    let mut webview_works = true;
    let mut webview: Option<WebView> = None;
    if !prefs.server_mode {
        match make_webview(&window, &mut context, port, &proxy, prefs.server_mode) {
            Ok(w) => webview = Some(w),
            Err(e) => {
                // no WebView2 runtime: the browser, and the tray icon to quit
                log::warn!("WebView2 not available ({e}), using the browser");
                webview_works = false;
                message(tr(
                    "Brak środowiska WebView2 (Microsoft Edge WebView2 Runtime) – MagicDAB otworzy się w przeglądarce. Program zamkniesz ikoną przy zegarze.",
                    "The WebView2 runtime (Microsoft Edge WebView2 Runtime) is missing – MagicDAB opens in the browser. Quit it with the icon next to the clock.",
                ));
                open_browser(port);
            }
        }
    }
    if webview.is_some() && !start_hidden {
        show_window(&window);
    } else {
        HIDDEN.store(true, Ordering::SeqCst);
        if prefs.server_mode && !start_hidden {
            server_note(proxy.clone());
        }
    }

    let menu = Menu::new();
    let show_item = MenuItem::new(tr("Pokaż MagicDAB", "Show MagicDAB"), webview_works, None);
    let browser_item = MenuItem::new(tr("Otwórz w przeglądarce", "Open in the browser"), true, None);
    let play_item = MenuItem::new(tr("Graj", "Play"), webview_works, None);
    let quit_item = MenuItem::new(tr("Zamknij MagicDAB", "Quit MagicDAB"), true, None);
    let _ = menu.append_items(&[&show_item, &browser_item, &play_item, &PredefinedMenuItem::separator(), &quit_item]);
    let tray_icon = tray_icon::Icon::from_rgba(include_bytes!("../../assets/icon-32.rgba").to_vec(), 32, 32).ok();
    let mut tray_builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("MagicDAB").with_menu_on_left_click(false);
    if let Some(i) = tray_icon {
        tray_builder = tray_builder.with_icon(i);
    }
    let tray = tray_builder.build().map_err(|e| log::warn!("tray icon: {e}")).ok();
    let p_menu = Mutex::new(proxy.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p_menu.lock().unwrap().send_event(UserEvent::Menu(e.id));
    }));
    let p_tray = Mutex::new(proxy.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        let _ = p_tray.lock().unwrap().send_event(UserEvent::Tray(e));
    }));

    let mut close_seq = 0u64;
    let mut answered = true;
    let mut geometry_saved = Instant::now();
    let save_geometry = move |window: &Window| {
        if window.is_minimized() {
            return;
        }
        let (Ok(pos), size) = (window.outer_position(), window.inner_size()) else { return };
        let g = Geometry { x: pos.x, y: pos.y, w: size.width, h: size.height, maximized: window.is_maximized() };
        let _ = std::fs::write(&geometry_file, serde_json::to_vec(&g).unwrap_or_default());
    };
    let quit = move |window: &Window| {
        window.set_visible(false);
        QUIT.notify_one();
    };
    let hide = |window: &Window| {
        window.set_visible(false);
        HIDDEN.store(true, Ordering::SeqCst);
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // owned by the loop for as long as it runs
        let _ = (&context, &tray);
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                save_geometry(&window);
                match &webview {
                    Some(w) => {
                        // the page asks (styled, with choices); without an answer, Windows does
                        close_seq += 1;
                        answered = false;
                        let _ = w.evaluate_script("window.dispatchEvent(new Event('magicdab-close'))");
                        let (p, seq) = (proxy.clone(), close_seq);
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(1500));
                            let _ = p.send_event(UserEvent::CloseTimeout(seq));
                        });
                    }
                    None => hide(&window),
                }
            }
            Event::WindowEvent { event: WindowEvent::Moved(_) | WindowEvent::Resized(_), .. } => {
                if geometry_saved.elapsed() > Duration::from_secs(2) {
                    geometry_saved = Instant::now();
                    save_geometry(&window);
                }
            }
            Event::UserEvent(UserEvent::Ipc(body)) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                match v["type"].as_str() {
                    Some("close") => {
                        answered = true;
                        match v["action"].as_str() {
                            Some("hide") => hide(&window),
                            Some("quit") => quit(&window),
                            // the application closes, the server stays (and the tray icon):
                            // no web interface, no audio here, until the window is opened
                            Some("server") => {
                                webview = None;
                                play_item.set_text(tr("Graj", "Play"));
                                hide(&window);
                            }
                            _ => {}
                        }
                    }
                    Some("serverMode") => {
                        prefs.server_mode = v["value"].as_bool() == Some(true);
                        let _ = std::fs::write(&prefs_file, serde_json::to_vec(&prefs).unwrap_or_default());
                    }
                    Some("playing") => {
                        let playing = v["value"].as_bool() == Some(true);
                        play_item.set_text(if playing { tr("Zatrzymaj", "Stop") } else { tr("Graj", "Play") });
                    }
                    Some("openBrowser") => open_browser(port),
                    _ => {}
                }
            }
            Event::UserEvent(UserEvent::CloseTimeout(seq)) if seq == close_seq && !answered => {
                answered = true;
                match ask_native(window.hwnd()) {
                    Close::Hide => hide(&window),
                    Close::Quit => quit(&window),
                    Close::Cancel => {}
                }
            }
            Event::UserEvent(UserEvent::Show) => {
                open_window(&window, &mut webview, &mut webview_works, &mut context, port, &proxy, prefs.server_mode);
            }
            Event::UserEvent(UserEvent::Menu(id)) => {
                if id == *show_item.id() {
                    open_window(&window, &mut webview, &mut webview_works, &mut context, port, &proxy, prefs.server_mode);
                } else if id == *browser_item.id() {
                    open_browser(port);
                } else if id == *play_item.id() {
                    match &webview {
                        Some(w) => {
                            let _ = w.evaluate_script("window.dispatchEvent(new Event('magicdab-toggle-play'))");
                        }
                        // closed to a server: the window first
                        None => open_window(&window, &mut webview, &mut webview_works, &mut context, port, &proxy, prefs.server_mode),
                    }
                } else if id == *quit_item.id() {
                    quit(&window);
                }
            }
            Event::UserEvent(UserEvent::Tray(
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { .. },
            )) => {
                open_window(&window, &mut webview, &mut webview_works, &mut context, port, &proxy, prefs.server_mode);
            }
            Event::UserEvent(UserEvent::Title(t)) => {
                window.set_title(&t);
                if let Some(tray) = &tray {
                    // tooltips are limited to 127 characters
                    let _ = tray.set_tooltip(Some(t.chars().take(120).collect::<String>()));
                }
            }
            _ => {}
        }
    });
}
