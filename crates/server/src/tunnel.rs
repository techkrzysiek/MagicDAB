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
/// SHA-256 of the archives of that release (frp_sha256_checksums.txt): a
/// program downloaded and run must be the one checked here.
const FRP_SHA256: &[(&str, &str)] = &[
    ("darwin_amd64", "403a0ee5e92f083a863d984b7af1e9d70ba2aaa28e87f42f1fe085adf76b8491"),
    ("darwin_arm64", "3e65f13a17a284bd6013e6bb6856bc2720074cea6094cc446c1f4c3932154c2d"),
    ("freebsd_amd64", "cb5edf41be38bd47bd7f68210c34500f5dac74423d8084ed8d7e0f3bce050cf5"),
    ("linux_amd64", "bff260b68ca7b1461182a46c4f34e9709ba32764eed30a15dd94ac97f50a2c40"),
    ("linux_arm", "8c7ebb07916e69d959d2a90f80900850c41b6dc1decdb87ccba4bc2f3574f855"),
    ("linux_arm64", "af6366f2b43920ebfe6235dba6060770399ed1fb18601e5818552bd46a7621f8"),
    ("windows_amd64", "e0094cd0baf03d5ff9ce9739199406871ad8788cf51e766f00ad3a9e7a836f3a"),
    ("windows_arm64", "099914b6f66b1d983a9402e3a9a116bccc683c877f6dbfbbb5c0f171b584675f"),
];

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

/// The frpc running now, with the tunnel generation that started it: one per
/// program. A second one with the same user is refused by the server ("proxy
/// already exists") and keeps trying, so none may outlive its generation or
/// the program (`kill_frpc` before a restart, `leftovers` after a crash).
static CHILD: std::sync::Mutex<Option<(u64, Child)>> = std::sync::Mutex::new(None);

fn child_slot() -> std::sync::MutexGuard<'static, Option<(u64, Child)>> {
    CHILD.lock().unwrap_or_else(|e| e.into_inner())
}

/// End the frpc of this program (a stop, a restart of the program).
pub fn kill_frpc() {
    let taken = child_slot().take();
    if let Some((_, mut c)) = taken {
        let _ = c.kill();
        let _ = c.wait();
    }
}

pub struct Tunnel {
    data_dir: PathBuf,
    config: Option<(TunnelConfig, u16)>,
    status: Arc<parking_lot::Mutex<TunnelStatus>>,
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Tunnel {
    pub fn new(data_dir: &Path) -> Self {
        Tunnel {
            data_dir: data_dir.to_path_buf(),
            config: None,
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
        let (data_dir, status, generation) = (self.data_dir.clone(), self.status.clone(), self.generation.clone());
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
            // (absolute: leftovers::end recognises its frpc by it)
            let conf = std::path::absolute(&data_dir).unwrap_or_else(|_| data_dir.clone()).join("frpc.toml");
            if let Err(e) = write_config(&conf, &cfg, port) {
                set("error", format!("config: {e}"));
                return;
            }
            // an frpc of an earlier run (a crash, an older version that left it
            // when it restarted) would keep the name on the server
            leftovers::end(&frpc, &conf, &data_dir);
            while current() {
                set("connecting", String::new());
                log::info!("tunnel: starting frpc for {}", cfg.url().unwrap_or_default());
                // (stderr straight to ours: a pipe nobody reads would stop frpc once full)
                let mut cmd = Command::new(&frpc);
                cmd.arg("-c").arg(&conf).stdout(Stdio::piped()).stderr(Stdio::inherit());
                leftovers::die_with_us(&mut cmd);
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
                {
                    let mut slot = child_slot();
                    // stopped meanwhile (stop() found nothing to end): this one
                    // ends here, not next to the one of the new settings
                    if !current() {
                        drop(slot);
                        let _ = c.kill();
                        let _ = c.wait();
                        break;
                    }
                    leftovers::adopt(&c);
                    *slot = Some((gen, c));
                }
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
                // (only its own: the slot may hold the frpc of newer settings)
                let own = {
                    let mut slot = child_slot();
                    if slot.as_ref().is_some_and(|(g, _)| *g == gen) {
                        slot.take()
                    } else {
                        None
                    }
                };
                if let Some((_, mut c)) = own {
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
        // (first the generation: a thread storing its frpc after this sees it)
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        kill_frpc();
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
    let platform = format!("{os}_{arch}");
    let Some(&(_, sha256)) = FRP_SHA256.iter().find(|(p, _)| *p == platform) else {
        return Err(format!("no frpc for {platform}"));
    };
    let url = format!("https://github.com/fatedier/frp/releases/download/v{FRP_VERSION}/frp_{FRP_VERSION}_{platform}.{ext}");
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
    let got: String = {
        use sha2::Digest;
        sha2::Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect()
    };
    if got != sha256 {
        return Err(format!("{url}: wrong checksum {got}"));
    }
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

/// frpc must not outlive the program: tied to it where the system allows
/// (Linux: a signal when the program ends; Windows: a job that ends with the
/// program), and the ones of an earlier run ended before a new one starts.
mod leftovers {
    use std::path::Path;
    use std::process::{Child, Command};

    #[cfg(target_os = "linux")]
    pub fn die_with_us(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        let parent = unsafe { libc::getpid() };
        // (the signal comes when the starting thread ends: the tunnel thread
        // waits for this frpc until it ends)
        unsafe {
            cmd.pre_exec(move || {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
                if libc::getppid() != parent {
                    libc::_exit(0);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    pub fn die_with_us(_cmd: &mut Command) {}

    #[cfg(windows)]
    pub fn adopt(child: &Child) {
        use std::os::windows::io::AsRawHandle;
        if let Some(job) = win::job() {
            unsafe { win::AssignProcessToJobObject(job, child.as_raw_handle() as win::Handle) };
        }
    }
    #[cfg(not(windows))]
    pub fn adopt(_child: &Child) {}

    /// End the frpc processes of an earlier run of this installation (its
    /// configuration file, no MagicDAB as their parent any more).
    pub fn end(frpc: &Path, conf: &Path, data_dir: &Path) {
        for pid in orphans(frpc, conf, data_dir) {
            log::warn!("tunnel: ending frpc {pid} left by an earlier run (it held the tunnel's name on the server)");
            kill(pid);
        }
    }

    /// The file name of this program (a parent with it is a running MagicDAB).
    #[cfg(any(target_os = "linux", windows))]
    fn program_name() -> Option<String> {
        std::env::current_exe().ok().and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn orphans(_frpc: &Path, conf: &Path, _data_dir: &Path) -> Vec<u32> {
        use std::os::unix::ffi::OsStrExt;
        let (Some(ours), Ok(conf)) = (program_name(), std::fs::canonicalize(conf)) else { return Vec::new() };
        let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
        let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().trim_end_matches(" (deleted)").to_string());
        let uses_conf = |pid: u32| -> bool {
            let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
            let args: Vec<&[u8]> = cmd.split(|&b| b == 0).collect();
            let frpc =
                args.first().is_some_and(|a| name_of(Path::new(std::ffi::OsStr::from_bytes(a))).is_some_and(|n| n.starts_with("frpc")));
            let Some(c) = args.iter().position(|a| *a == b"-c").and_then(|i| args.get(i + 1)) else { return false };
            let c = Path::new(std::ffi::OsStr::from_bytes(c));
            let c = if c.is_absolute() {
                c.to_path_buf()
            } else {
                match std::fs::read_link(format!("/proc/{pid}/cwd")) {
                    Ok(d) => d.join(c),
                    Err(_) => return false,
                }
            };
            frpc && std::fs::canonicalize(c).is_ok_and(|c| c == conf)
        };
        let parent_is_magicdab = |pid: u32| -> bool {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return true };
            // (after the name in parentheses: state, then the parent)
            let ppid = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)).and_then(|p| p.parse::<u32>().ok());
            ppid.and_then(|p| std::fs::read_link(format!("/proc/{p}/exe")).ok())
                .is_some_and(|e| name_of(&e).as_deref() == Some(ours.as_str()))
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| uses_conf(pid) && !parent_is_magicdab(pid))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn orphans(_frpc: &Path, conf: &Path, _data_dir: &Path) -> Vec<u32> {
        let Ok(out) = Command::new("ps").args(["-axww", "-o", "pid=,ppid=,args="]).output() else { return Vec::new() };
        let wanted = format!(" -c {}", conf.display());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (pid, rest) = l.trim_start().split_once(' ')?;
                let (ppid, args) = rest.trim_start().split_once(' ')?;
                let program = args.split(" -c ").next().unwrap_or("");
                // (an orphan's parent is launchd)
                let frpc = Path::new(program.trim()).file_name().is_some_and(|n| n.to_string_lossy().starts_with("frpc"));
                (frpc && ppid == "1" && args.ends_with(&wanted)).then(|| pid.parse().ok()).flatten()
            })
            .collect()
    }

    #[cfg(windows)]
    fn orphans(frpc: &Path, _conf: &Path, data_dir: &Path) -> Vec<u32> {
        // (no command line to look at: only an frpc in this installation's own
        // folders, not one from PATH that other programs may run too)
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
        let (Ok(frpc), Ok(data_dir)) = (std::path::absolute(frpc), std::path::absolute(data_dir)) else { return Vec::new() };
        let Some(ours) = program_name() else { return Vec::new() };
        if !(frpc.starts_with(&data_dir) || frpc.parent() == exe_dir.as_deref()) {
            return Vec::new();
        }
        let frpc = frpc.to_string_lossy().to_lowercase();
        let all = win::processes();
        all.iter()
            .filter(|(_, _, name)| name.eq_ignore_ascii_case("frpc.exe"))
            .filter(|(_, parent, _)| !all.iter().any(|(p, _, n)| p == parent && n.eq_ignore_ascii_case(&ours)))
            .filter(|(pid, _, _)| win::image(*pid).is_some_and(|i| i.to_lowercase() == frpc))
            .map(|(pid, _, _)| *pid)
            .collect()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    fn orphans(_frpc: &Path, _conf: &Path, _data_dir: &Path) -> Vec<u32> {
        Vec::new()
    }

    #[cfg(unix)]
    fn kill(pid: u32) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }

    #[cfg(windows)]
    fn kill(pid: u32) {
        unsafe {
            let h = win::OpenProcess(win::PROCESS_TERMINATE, 0, pid);
            if h != 0 {
                win::TerminateProcess(h, 1);
                win::CloseHandle(h);
            }
        }
    }

    #[cfg(windows)]
    mod win {
        pub type Handle = isize;
        pub const PROCESS_TERMINATE: u32 = 0x0001;
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const TH32CS_SNAPPROCESS: u32 = 0x2;
        const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
        const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

        #[repr(C)]
        #[derive(Default)]
        struct BasicLimit {
            per_process_user_time: i64,
            per_job_user_time: i64,
            limit_flags: u32,
            minimum_working_set: usize,
            maximum_working_set: usize,
            active_process_limit: u32,
            affinity: usize,
            priority_class: u32,
            scheduling_class: u32,
        }

        #[repr(C)]
        #[derive(Default)]
        struct ExtendedLimit {
            basic: BasicLimit,
            io_counters: [u64; 6],
            process_memory_limit: usize,
            job_memory_limit: usize,
            peak_process_memory_used: usize,
            peak_job_memory_used: usize,
        }

        #[repr(C)]
        struct ProcessEntry {
            size: u32,
            usage: u32,
            pid: u32,
            default_heap_id: usize,
            module_id: u32,
            threads: u32,
            parent: u32,
            priority_base: i32,
            flags: u32,
            exe: [u16; 260],
        }

        extern "system" {
            fn CreateJobObjectW(attributes: *const core::ffi::c_void, name: *const u16) -> Handle;
            fn SetInformationJobObject(job: Handle, class: i32, info: *const core::ffi::c_void, len: u32) -> i32;
            pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
            fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
            fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
            pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn QueryFullProcessImageNameW(process: Handle, flags: u32, name: *mut u16, len: *mut u32) -> i32;
            pub fn TerminateProcess(process: Handle, code: u32) -> i32;
            pub fn CloseHandle(handle: Handle) -> i32;
        }

        /// A job that ends its processes when its last handle closes: the
        /// program's own, at its end whatever the way.
        pub fn job() -> Option<Handle> {
            static JOB: std::sync::OnceLock<Option<Handle>> = std::sync::OnceLock::new();
            *JOB.get_or_init(|| unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job == 0 {
                    return None;
                }
                let mut info = ExtendedLimit::default();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let len = std::mem::size_of::<ExtendedLimit>() as u32;
                (SetInformationJobObject(job, JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, &info as *const _ as *const _, len) != 0)
                    .then_some(job)
            })
        }

        /// (pid, parent, file name) of every process.
        pub fn processes() -> Vec<(u32, u32, String)> {
            let mut all = Vec::new();
            unsafe {
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                if snapshot == -1 || snapshot == 0 {
                    return all;
                }
                let mut e: ProcessEntry = std::mem::zeroed();
                e.size = std::mem::size_of::<ProcessEntry>() as u32;
                let mut more = Process32FirstW(snapshot, &mut e) != 0;
                while more {
                    let len = e.exe.iter().position(|&c| c == 0).unwrap_or(e.exe.len());
                    all.push((e.pid, e.parent, String::from_utf16_lossy(&e.exe[..len])));
                    more = Process32NextW(snapshot, &mut e) != 0;
                }
                CloseHandle(snapshot);
            }
            all
        }

        /// The full path of a process's program.
        pub fn image(pid: u32) -> Option<String> {
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if h == 0 {
                    return None;
                }
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
                CloseHandle(h);
                ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With a real frpc: FRPC=/path/to/frpc cargo test -p magicdab leftover -- --ignored
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn a_leftover_frpc_is_ended_the_running_one_kept() {
        let frpc = PathBuf::from(std::env::var("FRPC").expect("FRPC=/path/to/frpc"));
        let dir = std::env::temp_dir().join(format!("magicdab-frpc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("frpc.toml");
        // (a server that is not there: frpc keeps trying)
        std::fs::write(&conf, "serverAddr = \"127.0.0.1\"\nserverPort = 9\nloginFailExit = false\n").unwrap();
        // an earlier run's: its parent (sh) gone at once
        let sh = format!("{} -c {} >/dev/null 2>&1 &", frpc.display(), conf.display());
        Command::new("sh").arg("-c").arg(sh).status().unwrap();
        // this program's own
        let mut own = Command::new(&frpc).arg("-c").arg(&conf).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let found = leftovers::orphans(&frpc, &conf, &dir);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_ne!(found[0], own.id());
        leftovers::end(&frpc, &conf, &dir);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(leftovers::orphans(&frpc, &conf, &dir).is_empty());
        assert!(own.try_wait().unwrap().is_none(), "the running one was ended");
        let _ = own.kill();
        let _ = own.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

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
