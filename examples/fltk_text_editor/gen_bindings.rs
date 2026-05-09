//! Generate the FLTK text-editor bindings + C++ shim archive.
//!
//! This binary runs cxx_importer's M26 `Build::compile` pipeline
//! against the umbrella header in `cpp/umbrella.hpp`. Outputs go
//! to `target/m26-out/`:
//!
//! - `bindings.rs` — the Rust file `src/main.rs` includes via
//!   `include!`. Re-exports every FLTK class, method, and FL_*
//!   constant the importer reached through the umbrella.
//! - `cxx_shims.cpp` — generated C++ trampolines.
//! - `libfltk_text_editor.a` — static archive of the shim
//!   trampolines compiled by cc-rs.
//!
//! Run after any FLTK upgrade or umbrella-header edit:
//!
//! ```sh
//! cd examples/fltk_text_editor
//! cargo run --bin gen_bindings --release
//! ```
//!
//! Does NOT need the rustcc fork toolchain — only libclang +
//! FLTK installed system-wide. The fork is only needed for the
//! `editor` bin, which links against the archive.

use std::path::PathBuf;

use cxx_importer::build::Build;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir.join("target").join("m26-out");
    std::fs::create_dir_all(&out_dir).expect("create m26-out");

    let umbrella = manifest_dir.join("cpp").join("umbrella.hpp");
    let fltk_include = PathBuf::from("/opt/homebrew/include");
    let fltk_lib_dir =
        PathBuf::from("/opt/homebrew/Cellar/fltk/1.4.5/lib");

    println!("=== FLTK text-editor bindings ===\n");
    println!("  umbrella header: {}", umbrella.display());
    println!("  output dir     : {}\n", out_dir.display());

    let outputs = Build::new()
        .header(&umbrella)
        .include_path(&fltk_include)
        .cpp_std("c++17")
        .cstr_ergonomics(true)
        // Same link inputs as fltk_hello's build_demo. Cribbed from
        // `fltk-config --cxxflags --ldflags` on macOS arm64.
        .lib_search_path(&fltk_lib_dir)
        .link("fltk")
        .link_static("pthread")
        .framework("Cocoa")
        .weak_framework("UniformTypeIdentifiers")
        .weak_framework("ScreenCaptureKit")
        .out_dir(&out_dir)
        .compile("fltk_text_editor")
        .expect("Build::compile against FLTK");

    println!("=== Pipeline output ===");
    println!("  bindings.rs    : {}", outputs.bindings_path.display());
    println!("  cxx_shims.cpp  : {}", outputs.shims_path.display());
    println!("  static archive : {}", outputs.static_lib_path.display());

    let bindings_size = std::fs::metadata(&outputs.bindings_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let archive_size = std::fs::metadata(&outputs.static_lib_path)
        .map(|m| m.len())
        .unwrap_or(0);
    println!("\n=== Sizes ===");
    println!("  bindings.rs : {bindings_size:>10} bytes");
    println!("  archive     : {archive_size:>10} bytes");

    println!("\n=== Cargo directives ===");
    for d in &outputs.cargo_directives {
        println!("  {d}");
    }

    println!(
        "\n=== Next step ===\n\
         Run the editor with the rustcc fork toolchain:\n  \
           cargo +rustcc run --bin editor --release\n\
         (Skip the toolchain if you only wanted to test the M26\n\
         binding generation; the build above already exercised\n\
         libclang parsing + Itanium mangling + cc-rs compile.)\n",
    );
}
