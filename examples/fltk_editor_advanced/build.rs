//! Build-script for the `editor` bin:
//!  - compiles `cpp/helpers.cpp` (C++ dispatch probes + construction
//!    self-checks) into a static lib,
//!  - emits the FLTK link directives (mirrors gen_bindings.rs's Build
//!    config / `fltk-config --ldflags` on macOS arm64),
//!  - links the generated shim archive when present.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // 1. The demo's own C++ helpers.
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include("/opt/homebrew/include")
        .file(manifest_dir.join("cpp").join("helpers.cpp"))
        .compile("rde_helpers");

    // 2. The generated C++ shim archive (only exists after
    //    `gen_bindings` ran; guard so `cargo check` still works).
    let shim_dir = manifest_dir.join("target").join("gen-out");
    if shim_dir.join("libfltk_editor_advanced.a").exists() {
        println!("cargo:rustc-link-search=native={}", shim_dir.display());
        println!("cargo:rustc-link-lib=static=fltk_editor_advanced");
    }

    // 3. FLTK + platform deps. The static archive by ABSOLUTE PATH —
    //    FLTK's dylib hides inline symbols (e.g. the header-inline
    //    `~Fl_Text_Editor()` the bindings reference by mangled name),
    //    and `-lfltk` would resolve to the dylib since both sit in the
    //    same directory.
    println!("cargo:rustc-link-arg=/opt/homebrew/Cellar/fltk/1.4.5/lib/libfltk.a");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=framework=Cocoa");
    println!("cargo:rustc-link-arg=-weak_framework");
    println!("cargo:rustc-link-arg=UniformTypeIdentifiers");
    println!("cargo:rustc-link-arg=-weak_framework");
    println!("cargo:rustc-link-arg=ScreenCaptureKit");

    println!("cargo:rerun-if-changed=cpp/helpers.cpp");
    println!("cargo:rerun-if-changed=build.rs");
}
