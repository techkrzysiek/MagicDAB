//! Windows: the program icon and manifest (resource compiled with windres).

fn main() {
    println!("cargo:rerun-if-changed=assets/magicdab.rc");
    println!("cargo:rerun-if-changed=assets/magicdab.ico");
    println!("cargo:rerun-if-changed=assets/magicdab.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("magicdab-res.o");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let windres = if arch == "x86_64" { "x86_64-w64-mingw32-windres" } else { "windres" };
    let ok = std::process::Command::new(windres)
        .args(["--input", "assets/magicdab.rc", "--output-format=coff", "--output"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=windres not found: the program has no icon");
    }
}
