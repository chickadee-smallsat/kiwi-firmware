//! Copies `memory.x` from the workspace `config/` directory into a
//! directory the linker can find, and points the linker at it. Unlike the
//! main firmware, this crate doesn't embed a Wi-Fi firmware blob at the top
//! of flash, so the template's flash layout is used as-is — no address
//! rewriting needed (see `firmware/build.rs` for the version that does).

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let memx_path = manifest_dir.join("../config/memory.x.template");
    let memx = fs::read_to_string(&memx_path).expect("Failed to read memory.x.template");

    let outdir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(outdir.join("memory.x"), memx).expect("Failed to write memory.x");

    println!("cargo:rustc-link-search={}", outdir.display());
    println!("cargo:rerun-if-changed=../config/memory.x.template");
    println!("cargo:rerun-if-changed=build.rs");
}
