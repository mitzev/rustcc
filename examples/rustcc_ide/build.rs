//! Link directives for the `editor` bin. The binding-generation step
//! (`cargo run --bin gen_bindings`) compiles the C++ shim trampolines
//! into `target/m26-out/libfltk_text_editor.a` and prints these same
//! directives informationally — but only a build.rs can hand them to
//! cargo, so they are mirrored here (same source of truth:
//! `fltk-config --ldflags` on macOS arm64 + the shim archive).
//!
//! The `gen_bindings` bin doesn't need any of this; the extra
//! directives are harmless for it.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shim_dir = manifest_dir.join("target").join("m26-out");

    // The generated C++ shim archive (placement-new heap ctors,
    // method trampolines, …). Only exists after `gen_bindings` ran;
    // guard so plain `cargo check` before generation still works.
    if shim_dir.join("libfltk_text_editor.a").exists() {
        println!("cargo:rustc-link-search=native={}", shim_dir.display());
        println!("cargo:rustc-link-lib=static=fltk_text_editor");
    }

    // FLTK itself + its platform deps (mirrors gen_bindings.rs's
    // Build configuration, cribbed from `fltk-config --ldflags`).
    println!("cargo:rustc-link-search=native=/opt/homebrew/Cellar/fltk/1.4.5/lib");
    println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
    // The static archive by ABSOLUTE PATH, not `-lfltk`: FLTK builds
    // its dylib with hidden inline symbols, so header-inline dtors
    // like `~Fl_Text_Editor()` (which the bindings reference by
    // mangled name) only exist in libfltk.a — and with both the .a
    // and .dylib in the same directory, `-l` resolves to the dylib.
    println!("cargo:rustc-link-arg=/opt/homebrew/Cellar/fltk/1.4.5/lib/libfltk.a");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=framework=Cocoa");
    println!("cargo:rustc-link-arg=-weak_framework");
    println!("cargo:rustc-link-arg=UniformTypeIdentifiers");
    println!("cargo:rustc-link-arg=-weak_framework");
    println!("cargo:rustc-link-arg=ScreenCaptureKit");

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        shim_dir.join("libfltk_text_editor.a").display()
    );
}
