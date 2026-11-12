//! MagicDAB - web based DAB/DAB+ receiver.
// the Windows application has a window, not a console
#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

mod access;
mod audio;
mod background;
mod changes;
mod chat;
mod clips;
mod config;
mod desktop;
mod dtr100input;
mod dxrec;
mod dynlabels;
mod etiinput;
mod figcache;
mod firewall;
mod fmradio;
mod gainseek;
mod gallery;
mod hub;
mod icy;
mod linker;
mod linking;
mod localtime;
mod logbuf;
mod public;
mod radio;
mod radiodns;
mod radiovis;
mod rawrec;
mod servermap;
mod servicelist;
mod sichanges;
mod speaker;
mod spi;
mod sysstats;
mod tunnel;
mod txdb;
mod update;
mod view;
mod ws;

use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    public: Arc<parking_lot::RwLock<public::PublicInfo>>,
    web_root: PathBuf,
    data_dir: PathBuf,
    hub: Arc<hub::Hub>,
    radio: crossbeam_channel::Sender<radio::Command>,
    chat: Arc<chat::Chat>,
    streams: Arc<icy::Streams>,
}

fn listen_port_of(listen: &str) -> u16 {
    listen.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(config::DEFAULT_PORT)
}

#[cfg(all(windows, not(feature = "desktop")))]
fn open_browser(url: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Err(e) = std::process::Command::new("cmd").args(["/C", "start", "", url]).creation_flags(CREATE_NO_WINDOW).spawn() {
        log::warn!("cannot open the browser: {e}");
    }
}

/// macOS and Linux desktops: the default browser.
#[cfg(not(windows))]
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let started =
        std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    if let Err(e) = started {
        log::warn!("cannot open the browser: {e}");
    }
}

/// (the Windows application shows its own window)
#[cfg(all(windows, feature = "desktop"))]
#[allow(dead_code)]
fn open_browser(_url: &str) {}

/// A console window opened just for the program closes with it: keep the
/// error readable.
fn pause_before_exit() {
    // (the Windows application has no console: errors show in a message box)
    #[cfg(all(windows, not(feature = "desktop")))]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        }
        let mut list = [0u32; 4];
        if unsafe { GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) } == 1 {
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    }
}

/// The folder exists (or can be made) and a file can be written in it.
fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".magicdab-write-test");
    let ok = std::fs::create_dir_all(dir).is_ok() && std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The user's own folder for the data: %LOCALAPPDATA%\MagicDAB,
/// ~/Library/Application Support/MagicDAB, $XDG_DATA_HOME/magicdab.
fn user_data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("LOCALAPPDATA").map(|d| d.join("MagicDAB"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/MagicDAB"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("magicdab"))
    }
}

fn usage() -> ! {
    eprintln!(
        "MagicDAB {}\n\nusage: magicdab [options]\n\n  --listen ADDR       listen address (default 0.0.0.0:3222; 8080 for an installation from before 0.3.23)\n  --data-dir DIR      settings, service list, logs (default ./data)\n  --web-root DIR      web UI files (default ./web/dist)\n  --rtlsdr [INDEX]    use RTL-SDR device\n  --rtltcp HOST:PORT  use rtl_tcp server\n  --dtr100            use a DTR-100 / C100 USB DAB dongle (16c0:05dc)\n  --hackrf [SERIAL]   use a HackRF One (receive only, amplifier off)\n  --lna N --vga N     its gains after --hackrf: LNA 0-40 (steps of 8, default 16), VGA 0-62 (steps of 2, default 30)\n  --file PATH         play a raw IQ recording (.uff/.raw)\n  --channel CH        stay on one channel (default for rtl_tcp: 5A, recordings: their own)\n  --service SID       stay on one station (hex SId), e.g. for its stream at /stream/SID\n  --no-browser        do not open the web interface on start (Windows)\n  --hidden            start in the tray (Windows application)\n  --verbose           debug logging",
        radio::VERSION
    );
    std::process::exit(1)
}

#[tokio::main]
async fn main() {
    // (without --listen or MAGICDAB_LISTEN: config::default_port, once the data folder is known)
    let mut listen: Option<String> = None;
    // Windows and the packages (the interface inside): next to the program,
    // whatever folder it was started from (Explorer, Finder, a shortcut)
    let base = if cfg!(windows) || cfg!(feature = "embed-web") {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(".")
    };
    let mut data_dir = base.join("data");
    let mut data_given = false;
    let mut web_root = base.join("web").join("dist");
    let mut device = None;
    let mut fixed_channel = std::env::var("MAGICDAB_CHANNEL").ok().filter(|c| !c.is_empty());
    let mut service_lock = std::env::var("MAGICDAB_SERVICE").ok().and_then(|s| u32::from_str_radix(s.trim(), 16).ok());
    let mut level = log::LevelFilter::Info;
    // started on a desktop (not on a server, not in a container): the interface
    // opens in the browser, unless switched off in the settings
    let mut browser = cfg!(windows)
        || cfg!(target_os = "macos")
        || ((std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()) && !update::in_container());
    let mut start_hidden = std::env::var_os("MAGICDAB_HIDDEN").is_some();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let next = |i: usize| args.get(i + 1).cloned().unwrap_or_else(|| usage());
        match args[i].as_str() {
            "--listen" => {
                listen = Some(next(i));
                i += 1;
            }
            "--data-dir" => {
                data_dir = PathBuf::from(next(i));
                data_given = true;
                i += 1;
            }
            "--web-root" => {
                web_root = PathBuf::from(next(i));
                i += 1;
            }
            "--rtlsdr" => {
                let index = args.get(i + 1).and_then(|s| s.parse().ok());
                if index.is_some() {
                    i += 1;
                }
                device = Some(magicdab_sdr::DeviceConfig::RtlSdr { index: index.unwrap_or(0), ppm: 0, bias_tee: false });
            }
            "--rtltcp" => {
                let v = next(i);
                i += 1;
                let (h, p) = v.rsplit_once(':').unwrap_or((&v, "1234"));
                device = Some(magicdab_sdr::DeviceConfig::RtlTcp { host: h.to_string(), port: p.parse().unwrap_or(1234) });
            }
            "--channel" => {
                fixed_channel = Some(next(i).to_uppercase());
                i += 1;
            }
            "--service" => {
                service_lock = Some(u32::from_str_radix(next(i).trim(), 16).unwrap_or_else(|_| usage()));
                i += 1;
            }
            "--dtr100" => device = Some(magicdab_sdr::DeviceConfig::Dtr100),
            "--hackrf" => {
                // (a serial number after it, unless the next is an option)
                let serial = args.get(i + 1).filter(|s| !s.starts_with("--")).cloned();
                if serial.is_some() {
                    i += 1;
                }
                device = Some(magicdab_sdr::DeviceConfig::HackRf { serial: serial.unwrap_or_default(), lna: 16, vga: 30 });
            }
            "--lna" | "--vga" => {
                let v: u32 = next(i).parse().unwrap_or_else(|_| usage());
                match device.as_mut() {
                    Some(magicdab_sdr::DeviceConfig::HackRf { lna, vga, .. }) => {
                        if args[i] == "--lna" {
                            *lna = v
                        } else {
                            *vga = v
                        }
                    }
                    _ => usage(),
                }
                i += 1;
            }
            "--file" => {
                device = Some(magicdab_sdr::DeviceConfig::RawFile { path: next(i), looping: true });
                i += 1;
            }
            "--verbose" => level = log::LevelFilter::Debug,
            "--no-browser" => browser = false,
            "--hidden" => start_hidden = true,
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown option {other}");
                usage();
            }
        }
        i += 1;
    }
    if let Ok(v) = std::env::var("MAGICDAB_LISTEN") {
        listen = Some(v);
    }
    if let Ok(v) = std::env::var("MAGICDAB_DATA_DIR") {
        data_dir = PathBuf::from(v);
        data_given = true;
    }
    // a program folder that cannot be written to (Program Files, /opt for
    // another user): the data goes to the user's own folder
    if !data_given && (cfg!(windows) || cfg!(feature = "embed-web")) && !writable(&data_dir) {
        if let Some(dir) = user_data_dir() {
            data_dir = dir;
        }
    }
    if let Ok(v) = std::env::var("MAGICDAB_WEB_ROOT") {
        web_root = PathBuf::from(v);
    }
    // the Windows application started from a terminal: logs there
    #[cfg(all(windows, feature = "desktop"))]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        unsafe { AttachConsole(u32::MAX) };
    }
    // the window of the Windows application has no console (nor has a phone,
    // and a package started from a file manager none to look at): the log goes
    // into a file too, so that what happened can be told afterwards
    if cfg!(any(windows, target_os = "android")) || cfg!(feature = "embed-web") {
        logbuf::to_file(data_dir.join("magicdab.log"));
    }
    logbuf::init(level);
    // after an update or a restart: the program before ends first (it closes
    // the stick, saves its files); nothing in the data folder is read before
    update::wait_for_predecessor();
    // panics go to the log (Log view); one of the radio or the receiver thread
    // restarts the program - a stopped receiver is worse than a short break
    let default_hook = std::panic::take_hook();
    static RESTARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        log::error!("panic in thread {thread}: {info}");
        default_hook(info);
        // (a receiver panic also ends the radio thread: one restart)
        if (thread == "radio" || thread == "dab-receiver") && !RESTARTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            log::error!("the receiver stopped: restarting the program");
            std::thread::sleep(std::time::Duration::from_millis(300));
            logbuf::flush();
            if update::supervised() {
                // (the container is started again by its restart policy, the
                // server of the Android app by the app)
                tunnel::kill_frpc();
                std::process::exit(70);
            }
            update::restart();
        }
    }));
    let listen_given = listen.is_some();
    let listen = listen.unwrap_or_else(|| format!("0.0.0.0:{}", config::default_port(&data_dir)));
    // the Windows application started again from another folder (another
    // data folder, another port): the running one shows its window instead
    // of a second one that would not get the tuner
    #[cfg(all(windows, feature = "desktop"))]
    if !listen_given && !data_given && std::env::var_os("MAGICDAB_RESTARTED").is_none() && show_other() {
        log::info!("MagicDAB is running already (another folder): its window is shown");
        logbuf::flush();
        std::process::exit(0);
    }
    let _ = listen_given;
    let _ = std::fs::create_dir_all(&data_dir);
    log::info!("MagicDAB {} starting, data dir {}, web root {}", radio::VERSION, data_dir.display(), web_root.display());
    access::start(&data_dir);

    // after an update the old program may still hold the port (and the tuners)
    // for a moment: wait for it before opening the devices
    let mut tries = 0;
    let listener = loop {
        match tokio::net::TcpListener::bind(&listen).await {
            Ok(l) => break l,
            // (after an update the old program may need a while to close: the
            // window's WebView2 processes, the tuner)
            Err(e)
                if e.kind() == std::io::ErrorKind::AddrInUse
                    && tries < if std::env::var_os("MAGICDAB_RESTARTED").is_some() { 120 } else { 30 } =>
            {
                // started again while running: show the window of that one
                if tries == 0 && std::env::var_os("MAGICDAB_RESTARTED").is_some() {
                    log::info!("{listen} is still in use (the old program is closing): waiting for it");
                }
                if tries == 0 && std::env::var_os("MAGICDAB_RESTARTED").is_none() && show_running(&data_dir, listen_port_of(&listen)) {
                    log::info!("MagicDAB is running already: its window is shown");
                    logbuf::flush();
                    std::process::exit(0);
                }
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                log::error!("cannot listen on {listen}: {e}");
                #[cfg(all(windows, feature = "desktop"))]
                desktop::message(&format!("MagicDAB: cannot listen on {listen}: {e}"));
                logbuf::flush();
                pause_before_exit();
                std::process::exit(2);
            }
        }
    };
    // the files replaced by an update, now that the old program has ended
    update::cleanup();

    let hub = hub::Hub::new();
    let listen_port = listen_port_of(&listen);
    let public = Arc::new(parking_lot::RwLock::new(public::PublicInfo::default()));
    let streams = Arc::new(icy::Streams::default());
    let radio =
        radio::Radio::new(hub.clone(), data_dir.clone(), device, fixed_channel, listen_port, public.clone(), streams.clone(), service_lock);
    let port = listener.local_addr().map_or(listen_port, |a| a.port());
    // a second start finds this one with the token in the data folder, and
    // (Windows application) from any folder in the user's (show_other)
    write_private(&desktop::instance_file(&data_dir), desktop::token().as_bytes());
    #[cfg(all(windows, feature = "desktop"))]
    if let Some(f) = running_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&data_dir));
        let _ = std::fs::write(f, serde_json::json!({"port": port, "token": desktop::token()}).to_string());
    }
    #[cfg(all(windows, feature = "desktop"))]
    {
        let _ = browser;
        // the window (in the tray when asked or switched off in the settings)
        let hidden = start_hidden || !radio.settings.open_browser;
        desktop::run(port, data_dir.clone(), hidden);
    }
    // started from Explorer: show the interface (unless switched off in the
    // settings, and not again after an update)
    #[cfg(not(all(windows, feature = "desktop")))]
    {
        let _ = start_hidden;
        if browser && radio.settings.open_browser && std::env::var_os("MAGICDAB_RESTARTED").is_none() {
            open_browser(&format!("http://localhost:{port}/"));
        }
    }
    let (radio_tx, radio_thread) = radio::spawn(radio);
    // (Windows: whether other devices may reach the program, for Settings)
    std::thread::spawn(firewall::check);

    let state = AppState {
        hub: hub.clone(),
        radio: radio_tx.clone(),
        public,
        web_root: web_root.clone(),
        data_dir: data_dir.clone(),
        chat: chat::Chat::new(),
        streams,
    };
    let index = web_root.join("index.html");
    let app = Router::new()
        .route("/ws", get(ws_handler))
        // the chat of FM-DX-Webserver, at its path
        .route("/chat", get(chat_handler))
        .route("/", get(index_handler))
        .route("/index.html", get(index_handler))
        .route("/static_data", get(static_data_handler))
        .route("/api", get(api_handler))
        .route("/og-image", get(og_image_handler))
        .route("/ping", get(|| async { "pong" }))
        .route("/robots.txt", get(robots_handler))
        .route("/desktop/show", get(desktop_show_handler))
        .route("/server_time", get(server_time_handler))
        // the background picture of the interface (background.rs)
        .route("/background/upload", post(background_upload).layer(axum::extract::DefaultBodyLimit::max(background::MAX_BYTES)))
        .route("/background/{file}", get(background_handler))
        // DX recordings (dxrec.rs): the admin downloads one, or loads an .eti of its own
        .route("/dxrec/upload", post(dxrec_upload_handler).layer(axum::extract::DefaultBodyLimit::max(dxrec::UPLOAD_MAX as usize + 4096)))
        .route("/dxrec/{name}", get(dxrec_download_handler))
        .route("/raw/{name}", get(raw_download_handler))
        .route("/stream/{name}", get(stream_handler))
        // packages of the program for updates (e.g. the Windows build), if published here
        .nest_service("/updates", ServeDir::new(data_dir.join("updates")))
        .fallback_service(ServeDir::new(&web_root).not_found_service(ServeFile::new(index)));
    // without the files of the web interface: the copy inside the program
    #[cfg(feature = "embed-web")]
    let app = if web_root.join("index.html").exists() { app } else { app.fallback(embedded::serve) };
    let app = app
        .layer(axum::middleware::from_fn(cache_headers))
        .layer(axum::middleware::from_fn_with_state(state.clone(), no_index))
        .with_state(state);
    // (outermost: a refused client gets nothing else)
    let app = app.layer(axum::middleware::from_fn(access_guard));

    log::info!("web interface on http://{listen}/");
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        #[cfg(unix)]
        {
            // also handled by ctrl_c on SIGINT; SIGTERM from docker stop:
        }
    });
    tokio::select! {
        r = server => { if let Err(e) = r { log::error!("server error: {e}"); } }
        _ = terminate() => {}
        _ = update::restart_requested() => {}
        _ = desktop_quit() => {}
    }
    // the new version first: it waits for this one, and a crash or a hang
    // while the stick is closed (its driver) cannot leave the server down
    if update::restart_pending() {
        update::start_successor();
    }
    log::info!("shutting down");
    let _ = radio_tx.send(radio::Command::Shutdown);
    // (closing a stick can hang in its driver: an update must still start the
    // new version, and the end of the process frees the devices anyway)
    let mut waited = 0;
    while !radio_thread.is_finished() && waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    if radio_thread.is_finished() {
        let _ = radio_thread.join();
    } else {
        log::warn!("the receiver did not stop within 10 s: going on without it");
    }
    if update::restart_pending() {
        // 1012: service restart
        hub.broadcast_all(hub::Out::Close(1012, "restart".into()));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        update::restart();
    }
    logbuf::flush();
}

/// Banned addresses, Tor exits and refused networks get nothing: 403 on every
/// path, so neither the page nor a WebSocket (/ws, /chat).
async fn access_guard(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ip = client_addr(req.headers(), addr);
    let Some(refusal) = access::admit(ip) else { return next.run(req).await };
    access::get().log_refusal(ip, refusal);
    let text = match refusal {
        access::Refusal::Banned => "Access from your address is not allowed. / Dostęp z Twojego adresu jest zablokowany.",
        access::Refusal::Tor => "Connections through Tor are not accepted. / Połączenia przez sieć Tor nie są przyjmowane.",
        access::Refusal::Network => "Connections from your network are not accepted. / Połączenia z Twojej sieci nie są przyjmowane.",
    };
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>403</title><p>{text}</p>"
    );
    (axum::http::StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// Search engines kept out (Settings > Internet): crawlers are refused and
/// every answer says noindex, nofollow.
async fn no_index(State(state): State<AppState>, req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if !state.public.read().no_index {
        return next.run(req).await;
    }
    let ua = req.headers().get(axum::http::header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    if public::is_crawler(ua) && req.uri().path() != "/robots.txt" {
        return (axum::http::StatusCode::FORBIDDEN, "no crawlers").into_response();
    }
    let mut res = next.run(req).await;
    res.headers_mut().insert("x-robots-tag", axum::http::HeaderValue::from_static("noindex, nofollow"));
    res
}

async fn robots_handler(State(state): State<AppState>) -> impl IntoResponse {
    let body = if state.public.read().no_index { "User-agent: *\nDisallow: /\n" } else { "User-agent: *\nAllow: /\n" };
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

/// The user quit through the window or the tray icon.
async fn desktop_quit() {
    #[cfg(all(windows, feature = "desktop"))]
    desktop::quit_requested().await;
    #[cfg(not(all(windows, feature = "desktop")))]
    std::future::pending::<()>().await
}

/// A second start of the program asks this one to show its window.
async fn desktop_show_handler(headers: axum::http::HeaderMap) -> axum::http::StatusCode {
    let token = headers.get("x-magicdab-instance").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !desktop::is_token(token) {
        return axum::http::StatusCode::FORBIDDEN;
    }
    #[cfg(all(windows, feature = "desktop"))]
    desktop::show();
    axum::http::StatusCode::OK
}

/// Windows application: where the running MagicDAB leaves its port and
/// token, whatever folder it was started from.
#[cfg(all(windows, feature = "desktop"))]
fn running_file() -> Option<PathBuf> {
    user_data_dir().map(|d| d.join("running.json"))
}

/// Ask a MagicDAB started from another folder to show its window.
#[cfg(all(windows, feature = "desktop"))]
fn show_other() -> bool {
    let Some(v) =
        running_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return false;
    };
    let (Some(port), Some(token)) = (v["port"].as_u64(), v["token"].as_str()) else { return false };
    ureq::get(&format!("http://127.0.0.1:{port}/desktop/show"))
        .header("x-magicdab-instance", token)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .call()
        .is_ok()
}

/// Ask a running MagicDAB (same data folder) to show its window.
fn show_running(data_dir: &std::path::Path, port: u16) -> bool {
    let Ok(token) = std::fs::read_to_string(desktop::instance_file(data_dir)) else { return false };
    ureq::get(&format!("http://127.0.0.1:{port}/desktop/show"))
        .header("x-magicdab-instance", token.trim())
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .call()
        .is_ok()
}

/// Hashed build assets and the background pictures (named by their content)
/// never change; everything else (index.html) must be revalidated, otherwise
/// mobile browsers keep running an outdated UI.
async fn cache_headers(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let get = req.method() == axum::http::Method::GET;
    let mut res = next.run(req).await;
    let immutable = get && (path.starts_with("/assets/") || (path.starts_with(background::URL_PREFIX) && res.status().is_success()));
    let v = if immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
    res.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static(v));
    // (an uploaded picture is served as what its bytes say, never sniffed into a page)
    res.headers_mut().insert(axum::http::header::X_CONTENT_TYPE_OPTIONS, axum::http::HeaderValue::from_static("nosniff"));
    res
}

#[cfg(unix)]
async fn terminate() {
    if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        s.recv().await;
    }
}

#[cfg(not(unix))]
async fn terminate() {
    std::future::pending::<()>().await
}

/// A file only its user may read (a token that logs in as admin).
fn write_private(path: &std::path::Path, data: &[u8]) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(path);
        let _ = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).and_then(|mut f| f.write_all(data));
    }
    #[cfg(not(unix))]
    let _ = std::fs::write(path, data);
}

/// Public origin of a request (behind tunnels / reverse proxies: forwarded headers).
fn origin(headers: &axum::http::HeaderMap) -> String {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim().to_string());
    let host = get("x-forwarded-host").or_else(|| get("host")).unwrap_or_else(|| "localhost".into());
    let bare = host.split(':').next().unwrap_or("");
    let is_ip_or_local = bare == "localhost" || bare.parse::<std::net::IpAddr>().is_ok();
    // tunnels terminate TLS before frp and forward plain http: a domain is served over https
    let proto = if get("x-forwarded-proto").as_deref() == Some("https") || !is_ip_or_local { "https" } else { "http" };
    format!("{proto}://{host}")
}

/// The web interface built into the program (feature `embed-web`).
#[cfg(feature = "embed-web")]
mod embedded {
    use axum::http::{header, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};

    #[derive(rust_embed::RustEmbed)]
    #[folder = "../../web/dist"]
    pub struct Web;

    pub async fn serve(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        match Web::get(path).or_else(|| Web::get("index.html")) {
            Some(f) => ([(header::CONTENT_TYPE, f.metadata.mimetype().to_string())], f.data).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    pub fn index() -> Option<String> {
        Web::get("index.html").map(|f| String::from_utf8_lossy(&f.data).to_string())
    }
}

/// Start page with Open Graph tags (link previews do not run JavaScript).
async fn index_handler(State(state): State<AppState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let html = std::fs::read_to_string(state.web_root.join("index.html")).ok();
    #[cfg(feature = "embed-web")]
    let html = html.or_else(embedded::index);
    let html = html.unwrap_or_default();
    let (tags, title) = {
        let p = state.public.read();
        (public::og_tags(&p, &origin(&headers)), public::title(&p))
    };
    let robots = if state.public.read().no_index { "<meta name=\"robots\" content=\"noindex, nofollow\">" } else { "" };
    let html = html.replacen("<title>MagicDAB</title>", &title, 1).replacen("</head>", &format!("{tags}{robots}</head>"), 1);
    ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"), (axum::http::header::CACHE_CONTROL, "no-cache")], html)
}

async fn static_data_handler(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(public::static_data(&state.public.read()))
}

async fn api_handler(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(public::api(&state.public.read()))
}

async fn server_time_handler() -> impl IntoResponse {
    axum::Json(serde_json::json!({ "serverTime": chrono::Utc::now().to_rfc3339() }))
}

/// A background picture from the admin (Settings > Webserver), who proves
/// to be one by the token of their login (as in the chat); the answer is the
/// address of the picture, for the settings.
async fn background_upload(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let token = headers.get("x-magicdab-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !state.hub.is_admin_token(token) {
        return (axum::http::StatusCode::FORBIDDEN, "admin login required").into_response();
    }
    match background::store(&state.data_dir, &body) {
        Ok(url) => {
            log::info!("background picture uploaded: {url} ({} bytes)", body.len());
            axum::Json(serde_json::json!({"url": url})).into_response()
        }
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// An uploaded background picture (only names made by background.rs).
async fn background_handler(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> axum::response::Response {
    if !background::is_file_name(&file) {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(state.data_dir.join(background::DIR).join(&file)) {
        Ok(data) => ([(axum::http::header::CONTENT_TYPE, background::mime(&file))], data).into_response(),
        Err(_) => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// Logo of the playing station for link previews (or the MagicDAB icon; a
/// private server shows no station).
async fn og_image_handler(State(state): State<AppState>) -> impl IntoResponse {
    let logo = {
        let p = state.public.read();
        if p.private {
            None
        } else {
            p.logo.clone()
        }
    };
    let (data, mime) = match logo.and_then(|(f, m)| std::fs::read(state.data_dir.join("logos").join(f)).ok().map(|d| (d, m))) {
        Some(x) => x,
        None => (std::fs::read(state.web_root.join("og.png")).unwrap_or_default(), "image/png".to_string()),
    };
    ([(axum::http::header::CONTENT_TYPE, mime), (axum::http::header::CACHE_CONTROL, "max-age=300".to_string())], data)
}

/// The admin's token of the query (`?token=`, as the chat knows admins).
fn admin_query(state: &AppState, query: &std::collections::HashMap<String, String>) -> bool {
    query.get("token").is_some_and(|t| state.hub.is_admin_token(t))
}

/// A DX recording for the admin (dxrec.rs), paced: the listeners' audio
/// goes through the same uplink.
async fn dxrec_download_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if !admin_query(&state, &query) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = dxrec::safe_name(&name) else { return StatusCode::NOT_FOUND.into_response() };
    paced_download(&client_ip(&headers, addr), state.data_dir.join(dxrec::DIR).join(&name), name).await
}

/// A raw IQ recording (.uff, rawrec.rs) to the admin, paced like the DX recordings.
async fn raw_download_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    if !admin_query(&state, &query) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = rawrec::safe_name(&name) else { return StatusCode::NOT_FOUND.into_response() };
    paced_download(&client_ip(&headers, addr), state.data_dir.join(rawrec::DIR).join(&name), name).await
}

/// A big file at the pace the receiver's uplink allows (the listeners' audio
/// goes the same way).
async fn paced_download(ip: &str, path: PathBuf, name: String) -> axum::response::Response {
    use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
    use axum::http::StatusCode;
    // one at a time per address, two in all, ten an hour per address
    let slot = match dxrec::download_slot(ip) {
        Ok(s) => s,
        Err(wait) => return (StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, wait.as_secs().to_string())]).into_response(),
    };
    let Ok(file) = tokio::fs::File::open(&path).await else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(len) = file.metadata().await.map(|m| m.len()) else { return StatusCode::NOT_FOUND.into_response() };
    const CHUNK: usize = 64 * 1024;
    let started = std::time::Instant::now();
    // (the slot goes with the stream: free again when it ends or is dropped)
    let stream = futures_util::stream::unfold((file, 0u64, false, slot), move |(mut file, sent, done, slot)| async move {
        use tokio::io::AsyncReadExt;
        if done {
            return None;
        }
        let mut buf = vec![0u8; CHUNK];
        let n = match file.read(&mut buf).await {
            Ok(0) => return None,
            Ok(n) => n,
            Err(e) => return Some((Err(e), (file, sent, true, slot))),
        };
        buf.truncate(n);
        let sent = sent + n as u64;
        // the bytes so far take this long at the pace allowed
        let due = std::time::Duration::from_secs_f64(sent as f64 / dxrec::DOWNLOAD_BYTES_PER_SECOND as f64);
        let el = started.elapsed();
        if due > el {
            tokio::time::sleep(due - el).await;
        }
        Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), (file, sent, false, slot)))
    });
    (
        [
            (CONTENT_TYPE, "application/octet-stream".to_string()),
            (CONTENT_LENGTH, len.to_string()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

/// A station as an Icecast style stream (icy.rs): `/stream/4C7E` (also with
/// `.opus` or `.ogg`), Ogg Opus, the DLS as the ICY title for players asking
/// for it (`Icy-MetaData: 1`).
async fn stream_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use axum::http::{HeaderValue, StatusCode};
    // a private server: for those let in only (`?token=` of their login, as the chat)
    if state.public.read().private && !query.get("token").is_some_and(|t| state.hub.is_token_in(t)) {
        return (StatusCode::FORBIDDEN, "This receiver is private.\n").into_response();
    }
    let hex = name.trim_end_matches(".opus").trim_end_matches(".ogg");
    let Ok(sid) = u32::from_str_radix(hex, 16) else { return StatusCode::NOT_FOUND.into_response() };
    let sub = match state.streams.subscribe(sid) {
        Ok(s) => s,
        Err(icy::Refusal::Busy) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "All the streams this server allows are in use.\n").into_response()
        }
        Err(_) => return (StatusCode::NOT_FOUND, "No such stream.\n").into_response(),
    };
    let meta = headers.get("icy-metadata").and_then(|v| v.to_str().ok()).is_some_and(|v| v.trim() == "1");
    log::info!("stream: {} ({sid:04X}) to {}{}", sub.name, client_ip(&headers, addr), if meta { ", with titles" } else { "" });
    // (names in UTF-8, as Icecast sends them)
    let value = |s: &str| HeaderValue::from_bytes(s.replace(['\r', '\n'], " ").as_bytes()).unwrap_or(HeaderValue::from_static(""));
    let mut r = axum::response::Response::builder()
        .header(CONTENT_TYPE, "audio/ogg")
        .header(CACHE_CONTROL, "no-cache, no-store")
        .header("icy-name", value(&sub.name))
        .header("icy-description", value(&format!("{}, {}, MagicDAB", sub.name, sub.channel)))
        .header("icy-br", sub.bitrate.to_string())
        .header("icy-pub", "0")
        .header("access-control-allow-origin", "*");
    if meta {
        r = r.header("icy-metaint", icy::META_INTERVAL.to_string());
    }
    let listener = icy::Listener::new(sub, meta);
    let body = futures_util::stream::unfold(listener, |mut l| async move {
        let chunk = l.next_chunk().await?;
        Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(chunk)), l))
    });
    r.body(axum::body::Body::from_stream(body)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// An .eti or .edi file of the admin's to play back for a day (dxrec.rs):
/// the file as the body, `?token=&name=`; checked to hold ETI frames or EDI
/// packets (the name gets the extension of what it holds), capped.
async fn dxrec_upload_handler(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Body,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    if !admin_query(&state, &query) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let original = query.get("name").cloned().unwrap_or_default();
    // (the extension of the name until the first bytes say what it is)
    let mut edi = original.to_ascii_lowercase().ends_with(".edi");
    let name = dxrec::upload_name(&original, edi);
    let dir = state.data_dir.join(dxrec::DIR);
    let mut path = dir.join(&name);
    let file = match tokio::fs::create_dir_all(&dir).await {
        Ok(()) => tokio::fs::File::create(&path).await,
        Err(e) => Err(e),
    };
    let Ok(mut file) = file else { return (StatusCode::INTERNAL_SERVER_ERROR, "cannot write the file").into_response() };
    let fail = |path: PathBuf, status: StatusCode, why: &'static str| async move {
        let _ = tokio::fs::remove_file(&path).await;
        (status, why).into_response()
    };
    let mut stream = body.into_data_stream();
    // the first frames, checked once there are enough bytes for a few; the
    // packets of an .edi are counted (its size does not tell its length)
    let mut head: Vec<u8> = Vec::new();
    let mut checked = false;
    let mut edi_packets: Option<magicdab_dab::eti::StreamParser> = None;
    let mut packets = 0u64;
    let mut size = 0u64;
    let format = |head: &[u8]| {
        if dxrec::looks_like_eti(head) {
            Some(false)
        } else {
            dxrec::looks_like_edi(head).then_some(true)
        }
    };
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { return fail(path, StatusCode::BAD_REQUEST, "the upload broke off").await };
        size += chunk.len() as u64;
        if size > dxrec::UPLOAD_MAX {
            return fail(path, StatusCode::PAYLOAD_TOO_LARGE, "the file is too big").await;
        }
        if let Some(parser) = edi_packets.as_mut() {
            parser.push(&chunk, |_| packets += 1);
        } else if !checked {
            head.extend_from_slice(&chunk);
            if head.len() >= 3 * magicdab_dab::eti::ETI_FRAME {
                checked = true;
                let Some(f) = format(&head) else { return fail(path, StatusCode::BAD_REQUEST, "not an ETI or EDI file").await };
                edi = f;
                if edi {
                    let mut parser = magicdab_dab::eti::StreamParser::new(true);
                    parser.push(&head, |_| packets += 1);
                    edi_packets = Some(parser);
                }
            }
        }
        if file.write_all(&chunk).await.is_err() {
            return fail(path, StatusCode::INTERNAL_SERVER_ERROR, "cannot write the file").await;
        }
    }
    if !checked {
        let Some(f) = format(&head) else { return fail(path, StatusCode::BAD_REQUEST, "not an ETI or EDI file").await };
        edi = f;
        if edi {
            magicdab_dab::eti::StreamParser::new(true).push(&head, |_| packets += 1);
        }
    }
    if file.flush().await.is_err() {
        return fail(path, StatusCode::INTERNAL_SERVER_ERROR, "cannot write the file").await;
    }
    drop(file);
    let name = if dxrec::is_edi(&path) == edi {
        name
    } else {
        let to = path.with_extension(if edi { "edi" } else { "eti" });
        if tokio::fs::rename(&path, &to).await.is_err() {
            return fail(path, StatusCode::INTERNAL_SERVER_ERROR, "cannot write the file").await;
        }
        path = to;
        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    };
    let frames = if edi { packets } else { size / magicdab_dab::eti::ETI_FRAME as u64 };
    let meta = dxrec::Meta {
        time: servicelist::now(),
        started: localtime::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        label: original.rsplit(['/', '\\']).next().unwrap_or("").chars().take(60).collect(),
        reason: "upload".into(),
        ended: "complete".into(),
        size,
        frames,
        duration: frames as f32 / dxrec::FRAMES_PER_SECOND as f32,
        version: radio::VERSION.to_string(),
        ..Default::default()
    };
    dxrec::write_meta(&path, &meta);
    log::info!("an admin uploaded the recording {name} ({:.1} MB)", size as f64 / 1e6);
    axum::Json(serde_json::json!({"name": name, "size": size})).into_response()
}

/// Address of the client: behind tunnels and reverse proxies the socket
/// address is the proxy (see access::client_addr: what a client forwards
/// itself is not believed).
fn client_addr(headers: &axum::http::HeaderMap, addr: SocketAddr) -> std::net::IpAddr {
    let forwarded = headers.get_all("x-forwarded-for").into_iter().filter_map(|v| v.to_str().ok());
    let real_ip = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
    access::client_addr(forwarded, real_ip, addr.ip(), |ip| access::get().is_proxy(ip))
}

fn client_ip(headers: &axum::http::HeaderMap, addr: SocketAddr) -> String {
    client_addr(headers, addr).to_string()
}

/// The chat (when it is on in Settings > Internet).
async fn chat_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if !state.public.read().chat_enabled {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    let ip = client_ip(&headers, addr);
    let token = query.get("token").cloned();
    // a private server: for those let in (the token of their login, radio/gate.rs)
    if state.public.read().private && !token.as_deref().is_some_and(|t| state.hub.is_token_in(t)) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(chat::PAYLOAD_MAX * 2)
        .on_upgrade(move |socket| chat::handle(socket, state.chat, state.hub, ip, token, state.public))
}

/// A Host header naming this computer (localhost, 127.0.0.1, [::1], with a port).
fn loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// An Origin header (`http://127.0.0.1:3222`) naming the host of the request.
fn same_origin_host(origin: &str, host: &str) -> bool {
    origin.split_once("://").is_some_and(|(scheme, rest)| matches!(scheme, "http" | "https") && rest.eq_ignore_ascii_case(host))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let ip = client_ip(&headers, addr);
    let host = get("x-forwarded-host").or_else(|| get("host")).unwrap_or_default();
    let local = host.split(':').next().map(|h| h == "localhost" || h.parse::<std::net::IpAddr>().is_ok()).unwrap_or(true);
    // on this computer: a loopback connection that no proxy or tunnel forwarded
    let forwarded = ["x-forwarded-for", "x-real-ip", "forwarded", "x-forwarded-host"].iter().any(|k| headers.contains_key(*k));
    // (a page of another site in a browser here may open a WebSocket to
    // 127.0.0.1 too: its Origin names that site)
    let origin_here = get("origin").is_none_or(|o| same_origin_host(&o, &host));
    let same_computer = addr.ip().to_canonical().is_loopback() && !forwarded && loopback_host(&host) && origin_here;
    let meta = hub::ClientMeta {
        ip,
        agent: ws::short_agent(&get("user-agent").unwrap_or_default()),
        via: if local { String::new() } else { host },
        local: same_computer,
    };
    // listeners send small messages (the transmitter list comes in parts)
    ws.max_message_size(1 << 20).max_frame_size(1 << 20).on_upgrade(move |socket| ws::handle(socket, state.hub, state.radio, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_page_of_this_computer_is_local() {
        assert!(loopback_host("127.0.0.1:3222") && loopback_host("localhost") && loopback_host("[::1]:3222"));
        assert!(!loopback_host("evil.example:3222"));
        assert!(same_origin_host("http://127.0.0.1:3222", "127.0.0.1:3222"));
        assert!(same_origin_host("http://LOCALHOST:3222", "localhost:3222"));
        // another site in a browser here, a page from a file, another port
        assert!(!same_origin_host("https://evil.example", "127.0.0.1:3222"));
        assert!(!same_origin_host("null", "127.0.0.1:3222"));
        assert!(!same_origin_host("http://127.0.0.1:8080", "127.0.0.1:3222"));
    }
}
