//! Generate the FLTK bindings + C++ shim archive for the ADVANCED
//! editor. Outputs to `target/gen-out/`:
//!
//! - `bindings.rs` — included by `src/main.rs`; carries the
//!   `#[rustc_cxx_imported_vtable]` attrs that let the Rust `class`es
//!   subclass Fl_Text_Editor / Fl_Box.
//! - `cxx_shims.cpp` + static archive — generated C++ trampolines.
//!
//! Needs libclang + FLTK only (NOT the fork toolchain):
//!
//! ```sh
//! cd examples/fltk_editor_advanced
//! cargo run --release --bin gen_bindings
//! ```

use std::path::PathBuf;

use cxx_importer::build::Build;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir.join("target").join("gen-out");
    std::fs::create_dir_all(&out_dir).expect("create gen-out");

    let umbrella = manifest_dir.join("cpp").join("umbrella.hpp");
    let fltk_include = PathBuf::from("/opt/homebrew/include");
    let fltk_lib_dir = PathBuf::from("/opt/homebrew/Cellar/fltk/1.4.5/lib");

    let outputs = Build::new()
        .header(&umbrella)
        .include_path(&fltk_include)
        .cpp_std("c++17")
        .cstr_ergonomics(true)
        .lib_search_path(&fltk_lib_dir)
        .link("fltk")
        .link_static("pthread")
        .framework("Cocoa")
        .weak_framework("UniformTypeIdentifiers")
        .weak_framework("ScreenCaptureKit")
        .out_dir(&out_dir)
        .compile("fltk_editor_advanced")
        .expect("Build::compile against FLTK");

    println!("bindings.rs : {}", outputs.bindings_path.display());

    // Show the imported-vtable attrs for the two subclassed chains —
    // the contract that the fork's `class RustEditor : Fl_Text_Editor`
    // and `class StatusBox : Fl_Box` consume.
    let bindings = std::fs::read_to_string(&outputs.bindings_path).unwrap_or_default();
    for class in ["_ZTV14Fl_Text_Editor", "_ZTV6Fl_Box"] {
        match bindings
            .lines()
            .find(|l| l.contains("rustc_cxx_imported_vtable") && l.contains(class))
        {
            Some(line) => {
                let slots = line.matches("slot=").count();
                println!("attr for {class}: {slots} slots");
            }
            None => eprintln!("WARNING: no imported-vtable attr for {class}!"),
        }
    }
}
