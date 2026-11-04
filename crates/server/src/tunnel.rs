//! Public tunnel through an frp server, like the tunnel of fm-dx-webserver:
//! frpc runs with a generated configuration and its state is reported.
//!
//! frpc is looked up in `MAGICDAB_FRPC`, next to the executable, in the data
//! directory and in `PATH`; if it is missing, the official frp release for the
//! platform is downloaded from GitHub (Linux, Windows, macOS).

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

const FRP_VERSION: &str = "0.61.1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TunnelConfig {
    pub enabled: bool,
    /// frp server / tunnel domain, e.g. dxserver.eu
    pub host: String,
    pub port: u16,
    pub username: String,
    pub subdomain: String,
    pub token: String,
    /// KCP transport (lower latency on lossy links).
    pub low_latency: bool,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        TunnelConfig {
            enabled: false,
            host: "dxserver.eu".into(),
            port: 7000,
            username: String::new(),
            subdomain: String::new(),
            token: String::new(),
            low_latency: false,
        }
    }
}

impl TunnelConfig {
    pub fn url(&self) -> Option<String> {
        (!self.subdomain.is_empty() && !self.host.is_empty()).then(|| format!("https://{}.{}", self.subdomain, self.host))
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    /// off, starting, downloading, connecting, online, error
    pub state: String,
    pub message: String,
    pub url: Option<String>,
}

pub struct Tunnel {
    data_dir: PathBuf,
    config: Option<(TunnelConfig, u16)>,
    child: Arc<parking_lot::Mutex<Option<Child>>>,
    status: Arc<parking_lot::Mutex<TunnelStatus>>,
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Tunnel {
    pub fn new(data_dir: &Path) -> Self {
        Tunnel {
            data_dir: data_dir.to_path_buf(),
            config: None,
            child: Arc::new(parking_lot::Mutex::new(None)),
            status: Arc::new(parking_lot::Mutex::new(TunnelStatus { state: "off".into(), ..Default::default() })),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn status(&self) -> TunnelStatus {
        self.status.lock().clone()
    }

    /// Start, restart or stop frpc according to the configuration.
    pub fn apply(&mut self, cfg: &TunnelConfig, local_port: u16) {
        let wanted = (cfg.enabled && !cfg.host.is_empty() && !cfg.subdomain.is_empty()).then(|| (cfg.clone(), local_port));
        if wanted == self.config {
            return;
        }
        self.stop();
        self.config = wanted.clone();
        let Some((cfg, port)) = wanted else { return };
        let gen = self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let (data_dir, child, status, generation) =
            (self.data_dir.clone(), self.child.clone(), self.status.clone(), self.generation.clone());
        *status.lock() = TunnelStatus { state: "starting".into(), message: String::new(), url: cfg.url() };
        let _ = std::thread::Builder::new().name("tunnel".into()).spawn(move || {
            let current = || generation.load(std::sync::atomic::Ordering::SeqCst) == gen;
            let set = |state: &str, message: String| {
                if current() {
                    *status.lock() = TunnelStatus { state: state.into(), message, url: cfg.url() };
                }
            };
            let frpc = match find_frpc(&data_dir) {
                Some(p) => p,
                None => {
                    set("downloading", format!("frp {FRP_VERSION}"));
                    match download_frpc(&data_dir) {
                        Ok(p) => p,
                        Err(e) => {
                            log::error!("tunnel: cannot get frpc: {e}");
                            set("error", format!("frpc: {e}"));
                            return;
                        }
                    }
                }
            };
            let conf = data_dir.join("frpc.toml");
            if let Err(e) = write_config(&conf, &cfg, port) {
                set("error", format!("config: {e}"));
                return;
            }
            while current() {
                set("connecting", String::new());
                log::info!("tunnel: starting frpc for {}", cfg.url().unwrap_or_default());
                // (stderr straight to ours: a pipe nobody reads would stop frpc once full)
                let mut cmd = Command::new(&frpc);
                cmd.arg("-c").arg(&conf).stdout(Stdio::piped()).stderr(Stdio::inherit());
                // no console window of its own on top of the program's (Windows)
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000);
                }
                let spawned = cmd.spawn();
                let mut c = match spawned {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("tunnel: cannot start {}: {e}", frpc.display());
                        set("error", format!("frpc: {e}"));
                        return;
                    }
                };
                let stdout = c.stdout.take();
                *child.lock() = Some(c);
                if let Some(out) = stdout {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if line.contains("start proxy success") {
                            log::info!("tunnel: online at {}", cfg.url().unwrap_or_default());
                            set("online", String::new());
                        } else if line.contains("login to server success") {
                            log::info!("tunnel: connected to {}", cfg.host);
                            set("connecting", "login ok".into());
                        } else if line.contains("invalid user or token") || line.contains("token in login doesn't match") {
                            log::warn!("tunnel: invalid user or token");
                            set("error", "invalid user or token".into());
                        } else if let Some(i) = line.find("connect to server error") {
                            let reason = line[i..].trim().to_string();
                            log::warn!("tunnel: {reason}");
                            set("error", reason);
                        } else if line.contains("start error") || line.contains("[E]") {
                            let msg = line.split("] ").last().unwrap_or(&line).trim().to_string();
                            log::warn!("tunnel: {msg}");
                            set("error", msg);
                        } else {
                            log::debug!("frpc: {line}");
                        }
                    }
                }
                if let Some(mut c) = child.lock().take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                if !current() {
                    break;
                }
                log::warn!("tunnel: frpc exited, restarting in 10 s");
                set("error", "frpc exited".into());
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    pub fn stop(&mut self) {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if self.config.take().is_some() {
            log::info!("tunnel: stopped");
        }
        *self.status.lock() = TunnelStatus { state: "off".into(), ..Default::default() };
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_config(path: &Path, c: &TunnelConfig, port: u16) -> std::io::Result<()> {
    let mut s =
        format!("serverAddr = {}\nserverPort = {}\nloginFailExit = false\nlog.disablePrintColor = true\n", toml_str(&c.host), c.port);
    if !c.username.is_empty() {
        s += &format!("user = {}\n", toml_str(&c.username));
    }
    if !c.token.is_empty() {
        s += &format!("metadatas.token = {}\n", toml_str(&c.token));
    }
    if c.low_latency {
        s += "transport.protocol = \"kcp\"\n";
    }
    s += &format!(
        "\n[[proxies]]\nname = \"web\"\ntype = \"http\"\nlocalIP = \"127.0.0.1\"\nlocalPort = {port}\nsubdomain = {}\n",
        toml_str(&c.subdomain)
    );
    std::fs::write(path, s)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "frpc.exe"
    } else {
        "frpc"
    }
}

fn find_frpc(data_dir: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MAGICDAB_FRPC") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
        candidates.push(dir.join(exe_name()));
    }
    candidates.push(data_dir.join("bin").join(exe_name()));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(exe_name())));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Download the official frp release and extract frpc into `<data>/bin`.
fn download_frpc(data_dir: &Path) -> Result<PathBuf, String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "windows" => "windows",
        "macos" => "darwin",
        "freebsd" => "freebsd",
        o => return Err(format!("unsupported OS {o}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "x86" => "386",
        a => return Err(format!("unsupported architecture {a}")),
    };
    let ext = if os == "windows" { "zip" } else { "tar.gz" };
    let url = format!("https://github.com/fatedier/frp/releases/download/v{FRP_VERSION}/frp_{FRP_VERSION}_{os}_{arch}.{ext}");
    log::info!("tunnel: downloading {url}");
    // (no request may hang forever: a server that accepts but never answers)
    let mut resp = ureq::get(&url)
        .header("User-Agent", "MagicDAB")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let data = resp.body_mut().with_config().limit(64 << 20).read_to_vec().map_err(|e| e.to_string())?;
    let bin = if ext == "zip" { extract_zip(&data, exe_name()) } else { extract_tar_gz(&data, exe_name()) }
        .ok_or("frpc not found in the archive")?;
    let dir = data_dir.join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(exe_name());
    std::fs::write(&path, bin).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(path)
}

/// File `name` (in any directory) from a gzip compressed tar archive.
fn extract_tar_gz(data: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(data).read_to_end(&mut tar).ok()?;
    let mut pos = 0;
    while pos + 512 <= tar.len() {
        let h = &tar[pos..pos + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let field = |a: usize, b: usize| String::from_utf8_lossy(&h[a..b]).trim_end_matches('\0').to_string();
        let mut entry = field(0, 100);
        let prefix = field(345, 500);
        if !prefix.is_empty() {
            entry = format!("{prefix}/{entry}");
        }
        let size = usize::from_str_radix(field(124, 136).trim(), 8).ok()?;
        let start = pos + 512;
        if (h[156] == b'0' || h[156] == 0) && (entry == name || entry.ends_with(&format!("/{name}"))) {
            return tar.get(start..start + size).map(|d| d.to_vec());
        }
        pos = start + size.div_ceil(512) * 512;
    }
    None
}

/// File `name` (in any directory) from a zip archive (stored or deflated).
fn extract_zip(data: &[u8], name: &str) -> Option<Vec<u8>> {
    let u16le = |p: usize| u16::from_le_bytes([data[p], data[p + 1]]) as usize;
    let u32le = |p: usize| u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
    // end of central directory record
    let eocd = (0..data.len().saturating_sub(21)).rev().find(|&i| data[i..i + 4] == [0x50, 0x4B, 0x05, 0x06])?;
    let count = u16le(eocd + 10);
    let mut p = u32le(eocd + 16);
    for _ in 0..count {
        if p + 46 > data.len() || data[p..p + 4] != [0x50, 0x4B, 0x01, 0x02] {
            return None;
        }
        let method = u16le(p + 10);
        let csize = u32le(p + 20);
        let nlen = u16le(p + 28);
        let xlen = u16le(p + 30);
        let clen = u16le(p + 32);
        let local = u32le(p + 42);
        let entry = String::from_utf8_lossy(data.get(p + 46..p + 46 + nlen)?).to_string();
        p += 46 + nlen + xlen + clen;
        if entry != name && !entry.ends_with(&format!("/{name}")) {
            continue;
        }
        if local + 30 > data.len() || data[local..local + 4] != [0x50, 0x4B, 0x03, 0x04] {
            return None;
        }
        let start = local + 30 + u16le(local + 26) + u16le(local + 28);
        let raw = data.get(start..start + csize)?;
        return match method {
            0 => Some(raw.to_vec()),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut out).ok()?;
                Some(out)
            }
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads the real archives: cargo test -p magicdab download_archives -- --ignored
    #[test]
    #[ignore]
    fn download_archives() {
        for (os, ext, name) in [("linux", "tar.gz", "frpc"), ("windows", "zip", "frpc.exe")] {
            let url = format!("https://github.com/fatedier/frp/releases/download/v{FRP_VERSION}/frp_{FRP_VERSION}_{os}_amd64.{ext}");
            let mut resp = ureq::get(&url).call().unwrap();
            let data = resp.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap();
            let bin = if ext == "zip" { extract_zip(&data, name) } else { extract_tar_gz(&data, name) }.unwrap();
            assert!(bin.len() > 1_000_000, "{os}: {}", bin.len());
            // ELF / PE header
            assert!(bin.starts_with(b"\x7fELF") || bin.starts_with(b"MZ"));
            eprintln!("{os}: {} bytes", bin.len());
        }
    }

    #[test]
    fn config_file() {
        let dir = std::env::temp_dir().join(format!("magicdab-tunnel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = TunnelConfig {
            enabled: true,
            username: "example-dab".into(),
            subdomain: "example-dab".into(),
            token: "abc\"d".into(),
            ..Default::default()
        };
        write_config(&dir.join("f.toml"), &c, 8080).unwrap();
        let s = std::fs::read_to_string(dir.join("f.toml")).unwrap();
        assert!(s.contains("serverAddr = \"dxserver.eu\""));
        assert!(s.contains("metadatas.token = \"abc\\\"d\""));
        assert!(s.contains("subdomain = \"example-dab\""));
        assert_eq!(c.url().as_deref(), Some("https://example-dab.dxserver.eu"));
        // (a private server asks for its passwords itself: radio/gate.rs)
        assert!(!s.contains("httpUser") && !s.contains("httpPassword"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
