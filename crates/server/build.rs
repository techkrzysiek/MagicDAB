//! Windows: the program icon, manifest and version (resource compiled with
//! windres): Explorer's details and the event log show the version of the
//! build (MAGICDAB_VERSION), not 0.0.0.0.

fn main() {
    println!("cargo:rerun-if-changed=assets/magicdab.rc");
    println!("cargo:rerun-if-changed=assets/magicdab.ico");
    println!("cargo:rerun-if-changed=assets/magicdab.manifest");
    println!("cargo:rerun-if-env-changed=MAGICDAB_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let version = std::env::var("MAGICDAB_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let rc = out_dir.join("magicdab-version.rc");
    std::fs::write(&rc, version_rc(&version)).expect("resource file");
    let assets = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let out = out_dir.join("magicdab-res.o");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let windres = if arch == "x86_64" { "x86_64-w64-mingw32-windres" } else { "windres" };
    // (the include folder: magicdab.rc and the files it names)
    let ok = std::process::Command::new(windres)
        .arg("--include-dir")
        .arg(&assets)
        .arg("--input")
        .arg(&rc)
        .args(["--output-format=coff", "--output"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=windres not found: the program has no icon");
    }
}

/// assets/magicdab.rc and a VERSIONINFO of `version` ("0.3.30", "0.3.123-dev").
fn version_rc(version: &str) -> String {
    let mut n = version.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
    let v = version.replace('"', "");
    format!(
        r#"#include "magicdab.rc"

1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "techkrzysiek"
            VALUE "FileDescription", "MagicDAB"
            VALUE "FileVersion", "{v}"
            VALUE "InternalName", "magicdab"
            VALUE "LegalCopyright", "AGPL-3.0-or-later"
            VALUE "OriginalFilename", "magicdab.exe"
            VALUE "ProductName", "MagicDAB"
            VALUE "ProductVersion", "{v}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
}
