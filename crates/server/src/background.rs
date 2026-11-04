//! Background picture of the interface uploaded by the admin (Settings >
//! Webserver): taken for a JPEG, PNG or WebP by its first bytes, kept in the
//! data folder under the hash of its content and served as
//! `/background/<hash>.<ext>` with a long cache (a new picture has a new name).
//! The upload needs the token of an admin login (main.rs), like the chat.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where the pictures are served.
pub const URL_PREFIX: &str = "/background/";
/// Their folder in the data folder.
pub const DIR: &str = "background";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// Pictures kept: the one in the settings may be an earlier one while the
/// admin tries another.
const KEEP: usize = 3;

/// The file extension of a picture by its first bytes: JPEG, PNG or WebP.
pub fn kind(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// "<32 hex digits>.<jpg|png|webp>": a name made here (nothing else is
/// served or accepted in the settings).
pub fn is_file_name(name: &str) -> bool {
    let Some((hash, ext)) = name.split_once('.') else { return false };
    hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) && matches!(ext, "jpg" | "png" | "webp")
}

/// The media type of a picture by its name.
pub fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

/// Keeps the picture and gives its address (`/background/<hash>.<ext>`).
pub fn store(data_dir: &Path, data: &[u8]) -> Result<String, &'static str> {
    if data.len() > MAX_BYTES {
        return Err("too big");
    }
    let ext = kind(data).ok_or("not a JPEG, PNG or WebP picture")?;
    let hash: String = Sha256::digest(data).iter().take(16).map(|b| format!("{b:02x}")).collect();
    let name = format!("{hash}.{ext}");
    let dir = data_dir.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|_| "cannot write to the data folder")?;
    std::fs::write(dir.join(&name), data).map_err(|_| "cannot write to the data folder")?;
    prune(&dir, &name);
    Ok(format!("{URL_PREFIX}{name}"))
}

/// Older pictures go, but the last few.
fn prune(dir: &Path, newest: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_str() != Some(newest))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(KEEP - 1) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(tail: u8) -> Vec<u8> {
        [vec![0xFF, 0xD8, 0xFF, 0xE0], vec![tail; 64]].concat()
    }

    #[test]
    fn kinds_by_the_first_bytes() {
        assert_eq!(kind(&jpeg(1)), Some("jpg"));
        assert_eq!(kind(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Some("png"));
        assert_eq!(kind(b"RIFF\x24\0\0\0WEBPVP8 "), Some("webp"));
        for bad in [&b"RIFF\x24\0\0\0WAVEfmt "[..], b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", b"GIF89a", b"", b"\xFF\xD8"] {
            assert_eq!(kind(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn names_made_here() {
        assert!(is_file_name("0123456789abcdef0123456789abcdef.jpg"));
        assert!(is_file_name("0123456789abcdef0123456789abcdef.webp"));
        for bad in [
            "0123456789ABCDEF0123456789abcdef.jpg",
            "0123456789abcdef0123456789abcde.jpg",
            "../settings.json",
            "0123456789abcdef0123456789abcdef.svg",
            "0123456789abcdef0123456789abcdef",
            "a.jpg",
        ] {
            assert!(!is_file_name(bad), "{bad}");
        }
        assert_eq!(mime("x.png"), "image/png");
        assert_eq!(mime("x.webp"), "image/webp");
        assert_eq!(mime("x.jpg"), "image/jpeg");
    }

    #[test]
    fn stored_under_its_hash_and_the_old_ones_pruned() {
        let dir = std::env::temp_dir().join(format!("magicdab-background-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = store(&dir, &jpeg(1)).unwrap();
        let name = url.strip_prefix(URL_PREFIX).unwrap();
        assert!(is_file_name(name) && name.ends_with(".jpg"), "{url}");
        assert_eq!(std::fs::read(dir.join(DIR).join(name)).unwrap(), jpeg(1));
        // the same picture again: the same address
        assert_eq!(store(&dir, &jpeg(1)).unwrap(), url);
        assert_eq!(store(&dir, b"GIF89a....").unwrap_err(), "not a JPEG, PNG or WebP picture");
        assert_eq!(store(&dir, &vec![0xFF; MAX_BYTES + 1]).unwrap_err(), "too big");
        // more pictures: the last few stay
        for i in 2..6u8 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            store(&dir, &jpeg(i)).unwrap();
        }
        let mut kept: Vec<Vec<u8>> =
            std::fs::read_dir(dir.join(DIR)).unwrap().flatten().map(|e| std::fs::read(e.path()).unwrap()).collect();
        kept.sort();
        assert_eq!(kept, vec![jpeg(3), jpeg(4), jpeg(5)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
