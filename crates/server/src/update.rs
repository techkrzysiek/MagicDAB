//! Updates of the program from a manifest (`latest.json`):
//! `{"version": "0.3.234", "windows-x64": {"url": "MagicDAB-0.3.234-windows-x64.zip", "sha256": "..."}}`
//! (the address may be relative to the manifest). The package is downloaded
//! and checked, files in use (the program, DLLs) are renamed to `*.old` —
//! Windows allows renaming them, not overwriting — the new files written in
//! their place and the program started again. In a container the image is
//! updated instead.
//!
//! The manifest is signed (`latest.json.sig` next to it: the Ed25519
//! signature of its bytes, made by the release workflow with the key of its
//! secret UPDATE_SIGNING_KEY): its checksums are believed only with a
//! signature of one of SIGNING_KEYS, so a changed manifest or package on the
//! way or where it is kept installs nothing.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use tokio::sync::Notify;

static RESTART: LazyLock<Notify> = LazyLock::new(Notify::new);
static RESTART_PENDING: AtomicBool = AtomicBool::new(false);

/// Public keys of the manifest's signature (hex, 32 bytes); a second one
/// while a new key takes over from an old one.
const SIGNING_KEYS: &[&str] = &["feaffbb99bab2f7320bc2d602f19fd60e2eaea244606d3ba407bf7214cb86acc"];

/// `manifest` signed (`sig`, 64 bytes) by one of `keys`.
fn signed(manifest: &[u8], sig: &[u8], keys: &[&str]) -> bool {
    use ring::signature::{UnparsedPublicKey, ED25519};
    let hex = |k: &str| -> Option<Vec<u8>> {
        (k.len() == 64).then(|| (0..32).map(|i| u8::from_str_radix(&k[2 * i..2 * i + 2], 16).ok()).collect::<Option<Vec<u8>>>())?
    };
    keys.iter().filter_map(|k| hex(k)).any(|k| UnparsedPublicKey::new(&ED25519, k).verify(manifest, sig).is_ok())
}

/// Manifest address built into a package (`UPDATE_URL` of the build): used
/// while the settings name none, so a later version can move it.
pub const DEFAULT_URL: &str = match option_env!("MAGICDAB_UPDATE_URL") {
    Some(u) => u,
    None => "",
};

/// The manifest address in use.
pub fn url(configured: &str) -> String {
    if configured.is_empty() {
        DEFAULT_URL.to_string()
    } else {
        configured.to_string()
    }
}

/// Package of this build in the manifest.
pub fn platform() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("linux-arm64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-arm64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("macos-x64")
    } else {
        None
    }
}

/// Inside a container the image is updated, not the program.
pub fn in_container() -> bool {
    // (a Windows program sees the root of its drive there: Wine in Docker)
    cfg!(target_os = "linux") && Path::new("/.dockerenv").exists()
}

/// Something starts the program again when it ends (a container's restart
/// policy, the Android app, a systemd service): it exits instead of starting
/// itself.
pub fn supervised() -> bool {
    in_container() || cfg!(target_os = "android") || systemd_service()
}

/// The main process of a systemd service: a copy started by the program would
/// end with the service (systemd stops everything left of it), its Restart=
/// starts the new version. (Not a program started in a terminal that is
/// itself a service: its parent is the shell.)
fn systemd_service() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("INVOCATION_ID").is_some()
            && std::fs::read_to_string(format!("/proc/{}/comm", std::os::unix::process::parent_id())).is_ok_and(|c| c.trim() == "systemd")
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub notes: Option<String>,
}

/// `a` newer than `b` (dotted numbers).
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    // (a stalled server must not leave the update busy for ever; packages are
    // large, slow links need time)
    let secs = if limit > 1 << 20 { 900 } else { 30 };
    let mut resp = ureq::get(url)
        .header("User-Agent", "MagicDAB-updater")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(secs)))
        .build()
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    resp.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

/// A package is only as trustworthy as the way it comes (its checksum is in
/// the manifest): https, or this computer.
pub(crate) fn secure(url: &str) -> bool {
    url.starts_with("https://")
        || ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|p| url.strip_prefix(p).is_some_and(|r| r.is_empty() || r.starts_with([':', '/'])))
}

/// The newer version of the manifest, if there is one for this platform.
pub fn check(manifest: &str, current: &str) -> Result<Option<Available>, String> {
    if !secure(manifest) {
        return Err(format!("{manifest}: updates only over https"));
    }
    let data = get(manifest, 1 << 20)?;
    let sig = get(&format!("{manifest}.sig"), 1024).map_err(|e| format!("manifest without a signature: {e}"))?;
    if !signed(&data, &sig, SIGNING_KEYS) {
        return Err(format!("{manifest}: wrong signature, nothing installed"));
    }
    let v: serde_json::Value = serde_json::from_slice(&data).map_err(|e| format!("manifest: {e}"))?;
    let version = v["version"].as_str().ok_or("manifest: no version")?.to_string();
    let Some(p) = platform().and_then(|p| v.get(p)) else {
        return Err(format!("manifest: no package for {}", platform().unwrap_or("this platform")));
    };
    let url = p["url"].as_str().ok_or("manifest: no url")?;
    // relative to the manifest
    let url = if url.contains("://") { url.to_string() } else { format!("{}/{url}", manifest.rsplit_once('/').map_or("", |x| x.0)) };
    if !secure(&url) {
        return Err(format!("{url}: updates only over https"));
    }
    let sha256 = p["sha256"].as_str().ok_or("manifest: no sha256")?.to_lowercase();
    let notes = v["notes"].as_str().map(|s| s.to_string());
    Ok(newer(&version, current).then_some(Available { version, url, sha256, notes }))
}

/// Download, check and put the files of the package in place.
pub fn install(a: &Available) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no program folder")?.to_path_buf();
    let data = get(&a.url, 300 << 20)?;
    let sha = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>();
    if sha != a.sha256 {
        return Err(format!("checksum mismatch ({sha})"));
    }
    let files = zip_files(&data).ok_or("damaged package")?;
    if !files.iter().any(|(n, _)| n.ends_with(exe.file_name().and_then(|f| f.to_str()).unwrap_or("magicdab.exe"))) {
        return Err("the package does not contain the program".into());
    }
    put_files(&dir, files)?;
    log::info!("update {} installed in {}", a.version, dir.display());
    Ok(())
}

/// Linux and macOS: the program and its libraries must stay executable (a
/// file written anew is not; zips from Windows carry no modes).
fn executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").trim_end_matches(".new");
        let program = name == "magicdab" || name.contains(".so") || name.ends_with(".dylib");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(if program { 0o755 } else { 0o644 }))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Where a file of a package goes, relative to the program folder:
/// "MagicDAB/magicdab.exe" -> "magicdab.exe"; nothing out of the folder (zips
/// made on Windows may separate with backslashes).
fn package_path(name: &str) -> Option<PathBuf> {
    let mut parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    // (the folder of the package only: the Linux program is called magicdab too)
    if parts.len() > 1 && parts[0].eq_ignore_ascii_case("MagicDAB") {
        parts.remove(0);
    }
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':')) {
        return None;
    }
    Some(parts.iter().collect())
}

/// Put the files of a package into the program folder `dir`.
fn put_files(dir: &std::path::Path, files: Vec<(String, Vec<u8>)>) -> Result<(), String> {
    // first every new file next to its old one: a failure here (disk full, an
    // antivirus taking the new exe) leaves the program as it was
    let mut written: Vec<PathBuf> = Vec::new();
    let undo = |written: &[PathBuf]| written.iter().for_each(|t| drop(std::fs::remove_file(with_suffix(t, ".new"))));
    for (name, content) in files {
        let Some(rel) = package_path(&name) else { continue };
        let target = dir.join(&rel);
        let saved = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(with_suffix(&target, ".new"), content))
            .and_then(|_| executable(&with_suffix(&target, ".new")));
        if let Err(e) = saved {
            undo(&written);
            return Err(format!("{}: {e}", target.display()));
        }
        if !written.contains(&target) {
            written.push(target);
        }
    }
    // then the swap, only renames (a running program and its DLLs can be renamed)
    let mut swapped: Vec<PathBuf> = Vec::new();
    for target in &written {
        let old = with_suffix(target, ".old");
        let _ = std::fs::remove_file(&old);
        let moved = if target.exists() { std::fs::rename(target, &old) } else { Ok(()) };
        if let Err(e) = moved.and_then(|_| std::fs::rename(with_suffix(target, ".new"), target)) {
            // back to the files of the running version
            for t in swapped.iter().chain(std::iter::once(target)) {
                let old = with_suffix(t, ".old");
                if old.exists() {
                    let _ = std::fs::remove_file(t);
                    let _ = std::fs::rename(&old, t);
                }
            }
            undo(&written);
            return Err(format!("{}: {e}", target.display()));
        }
        swapped.push(target.clone());
    }
    Ok(())
}

/// `path` with `suffix` appended to its name ("magicdab.exe" -> "magicdab.exe.old").
fn with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Ask for the new version to be started once this one has shut down
/// (tuners closed, settings saved).
pub fn request_restart() {
    RESTART_PENDING.store(true, Ordering::SeqCst);
    RESTART.notify_one();
}

pub async fn restart_requested() {
    RESTART.notified().await
}

pub fn restart_pending() -> bool {
    RESTART_PENDING.load(Ordering::SeqCst)
}

/// Start the (new) program with the same arguments and end this one; under a
/// supervisor only end it (not 0: Restart=on-failure starts it again too).
pub fn restart() -> ! {
    if supervised() {
        crate::logbuf::flush();
        std::process::exit(75);
    }
    if let Ok(exe) = std::env::current_exe() {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args).env("MAGICDAB_RESTARTED", "1");
        // the window stays in the tray if it was there
        #[cfg(all(windows, feature = "desktop"))]
        if crate::desktop::hidden() {
            cmd.env("MAGICDAB_HIDDEN", "1");
        } else {
            // (inherited from a start in the tray before)
            cmd.env_remove("MAGICDAB_HIDDEN");
        }
        if let Err(e) = cmd.spawn() {
            log::error!("restart failed: {e}");
        }
    }
    crate::logbuf::flush();
    std::process::exit(0)
}

/// Remove the files replaced by the last update.
pub fn cleanup() {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else { return };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            // (".new": an install that was interrupted)
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".old") || name.ends_with(".new") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// All files of a zip archive (stored or deflated).
fn zip_files(data: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let u16le = |p: usize| -> Option<usize> { Some(u16::from_le_bytes([*data.get(p)?, *data.get(p + 1)?]) as usize) };
    let u32le = |p: usize| -> Option<usize> {
        Some(u32::from_le_bytes([*data.get(p)?, *data.get(p + 1)?, *data.get(p + 2)?, *data.get(p + 3)?]) as usize)
    };
    let eocd = (0..data.len().saturating_sub(21)).rev().find(|&i| data[i..i + 4] == [0x50, 0x4B, 0x05, 0x06])?;
    let count = u16le(eocd + 10)?;
    let mut p = u32le(eocd + 16)?;
    let mut out = Vec::new();
    for _ in 0..count {
        if data.get(p..p + 4)? != [0x50, 0x4B, 0x01, 0x02] {
            return None;
        }
        let method = u16le(p + 10)?;
        let csize = u32le(p + 20)?;
        let nlen = u16le(p + 28)?;
        let xlen = u16le(p + 30)?;
        let clen = u16le(p + 32)?;
        let local = u32le(p + 42)?;
        let name = String::from_utf8_lossy(data.get(p + 46..p + 46 + nlen)?).to_string();
        p += 46 + nlen + xlen + clen;
        if name.ends_with('/') {
            continue;
        }
        if data.get(local..local + 4)? != [0x50, 0x4B, 0x03, 0x04] {
            return None;
        }
        let start = local + 30 + u16le(local + 26)? + u16le(local + 28)?;
        let raw = data.get(start..start + csize)?;
        let content = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::new();
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut v).ok()?;
                v
            }
            _ => return None,
        };
        out.push((name, content));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_manifest_signed_with_our_key_counts() {
        // (a key made for this test with openssl, signed as the release workflow does)
        let key = "7a83fbe8ae39ec92d8cd31068bad1d3d180dadd7ef39a561dc2d6e80d15703ae";
        let manifest = br#"{"version": "0.3.99", "windows-x64": {"url": "MagicDAB-0.3.99-windows-x64.zip", "sha256": "00"}}"#;
        let sig: Vec<u8> = (0..64)
            .map(|i| {
                let h = "1f6f5dc5efd25112a78199e9fac6f867723262c8673b20e9aa10385e28d0e202\
                         675cb25bf4ad99f3ca2f0f02a4a41ba1d7d178d8b1ba09b4e7fe2c46fbd7f20e";
                u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()
            })
            .collect();
        assert!(signed(manifest, &sig, &[key]));
        assert!(signed(manifest, &sig, &["not a key", key]));
        // another manifest, another key, a damaged signature, none
        let mut changed = manifest.to_vec();
        changed[14] = b'8';
        assert!(!signed(&changed, &sig, &[key]));
        assert!(!signed(manifest, &sig, SIGNING_KEYS));
        let mut bad = sig.clone();
        bad[0] ^= 1;
        assert!(!signed(manifest, &bad, &[key]));
        assert!(!signed(manifest, b"", &[key]));
        assert!(!signed(manifest, &sig, &[]));
        // the built-in keys are keys
        assert!(SIGNING_KEYS.iter().all(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit())));
    }

    #[test]
    fn files_replaced_or_left_as_they_were() {
        let dir = std::env::temp_dir().join(format!("magicdab-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("magicdab.exe"), "v1").unwrap();
        std::fs::write(dir.join("readme.txt"), "v1").unwrap();
        // a new file that cannot be written (a directory in its place): nothing changes
        std::fs::create_dir_all(dir.join("blocked.dll.new")).unwrap();
        let pkg = |extra: bool| {
            let mut f = vec![("MagicDAB/magicdab.exe".to_string(), b"v2".to_vec()), ("MagicDAB/readme.txt".to_string(), b"v2".to_vec())];
            if extra {
                f.push(("MagicDAB/blocked.dll".to_string(), b"v2".to_vec()));
            }
            f
        };
        assert!(put_files(&dir, pkg(true)).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("magicdab.exe")).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(dir.join("readme.txt")).unwrap(), "v1");
        assert!(!dir.join("magicdab.exe.new").exists());
        put_files(&dir, pkg(false)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("magicdab.exe")).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(dir.join("magicdab.exe.old")).unwrap(), "v1");
        assert!(!dir.join("readme.txt.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_new_program_can_be_started() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("magicdab-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let files = ["magicdab", "librtlsdr.so.0", "librtlsdr.0.dylib", "README.txt"].map(|n| (format!("MagicDAB/{n}"), b"v2".to_vec()));
        put_files(&dir, files.to_vec()).unwrap();
        let mode = |n: &str| std::fs::metadata(dir.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("magicdab"), 0o755);
        assert_eq!(mode("librtlsdr.so.0"), 0o755);
        assert_eq!(mode("librtlsdr.0.dylib"), 0o755);
        assert_eq!(mode("README.txt"), 0o644);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_https_or_this_computer() {
        assert!(secure("https://github.com/o/r/releases/latest/download/latest.json"));
        assert!(secure("http://127.0.0.1:8082/updates/latest.json"));
        assert!(secure("http://localhost/updates/latest.json"));
        assert!(!secure("http://example.org/latest.json"));
        assert!(!secure("http://localhost.example.org/latest.json"));
        assert!(!secure("http://127.0.0.1.example.org/latest.json"));
        assert!(!secure("ftp://example.org/latest.json"));
    }

    #[test]
    fn package_paths_stay_in_the_folder() {
        let safe = |name: &str| package_path(name).is_some();
        assert_eq!(package_path("MagicDAB/magicdab"), Some(PathBuf::from("magicdab")));
        assert_eq!(package_path("MagicDAB/magicdab.exe"), Some(PathBuf::from("magicdab.exe")));
        assert!(safe("MagicDAB/magicdab.exe"));
        assert!(safe("MagicDAB\\README.txt"));
        assert!(!safe("MagicDAB/../evil.exe"));
        assert!(!safe("..\\..\\evil.exe"));
        assert!(!safe("C:\\Windows\\evil.dll"));
    }

    #[test]
    fn versions() {
        assert!(newer("0.3.250", "0.3.249"));
        assert!(newer("0.3.1", "0.2.101"));
        assert!(!newer("0.3.249", "0.3.249"));
        assert!(!newer("0.3.9", "0.3.10"));
    }

    #[test]
    fn zip() {
        // stored entry "MagicDAB/a.txt" = "hi", made with Python's zipfile
        let mut z = Vec::new();
        let name = b"MagicDAB/a.txt";
        let body = b"hi";
        let crc = 0x8f5ab8e5u32.to_le_bytes(); // not checked by the reader
        z.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&crc);
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(name);
        z.extend_from_slice(body);
        let cd = z.len();
        z.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&crc);
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0; 12]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(name);
        let cd_len = z.len() - cd;
        z.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_len as u32).to_le_bytes());
        z.extend_from_slice(&(cd as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        let files = zip_files(&z).unwrap();
        assert_eq!(files, vec![("MagicDAB/a.txt".to_string(), b"hi".to_vec())]);
    }
}
