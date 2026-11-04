// librdsparser (third_party/librdsparser, LGPL-2.1-or-later) for RDS decoding.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../third_party/librdsparser");
    let mut build = cc::Build::new();
    build.include(root.join("include")).warnings(false);
    for entry in std::fs::read_dir(root.join("src")).unwrap().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "c") {
            println!("cargo:rerun-if-changed={}", p.display());
            build.file(p);
        }
    }
    build.compile("rdsparser");
}
