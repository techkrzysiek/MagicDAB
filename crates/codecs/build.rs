use std::env;
use std::path::PathBuf;

fn link_system(pkg: &str, fallback_lib: &str) {
    match pkg_config::Config::new().probe(pkg) {
        Ok(_) => {}
        Err(_) => {
            // Windows (vcpkg / MSYS2) or systems without pkg-config files
            if let Ok(dir) = env::var(format!("{}_LIB_DIR", fallback_lib.to_uppercase())) {
                println!("cargo:rustc-link-search=native={dir}");
            }
            println!("cargo:rustc-link-lib={fallback_lib}");
        }
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let third_party = root.join("../../third_party");

    cc::Build::new()
        .file(root.join("csrc/mp2_shim.c"))
        .include(third_party.join("minimp3"))
        .opt_level(3)
        .warnings(false)
        .compile("magicdab_mp2");

    link_system("faad2", "faad");
    link_system("opus", "opus");
    // Android: static faad2 and opus need libm
    // (the NDK's clang adds it only for C++)
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-lib=m");
    }

    println!("cargo:rerun-if-changed=csrc/mp2_shim.c");
    println!("cargo:rerun-if-changed=build.rs");
}
