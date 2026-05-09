//! End-to-end driver: run `cxx_importer::build::Build::compile`
//! against the FLTK umbrella header, including a real
//! `cc::Build` C++ compile of the emitted shims into a static
//! library. This is the M26 closer to the "click run, see a
//! window" gap from the original FLTK probe — everything except
//! the final Rust compile (which requires the rustcc fork rustc
//! for `extern "C++"`) lands here.
//!
//! Run:
//!
//! ```sh
//! cd examples/fltk_hello
//! cargo run --release --bin build_demo
//! ```
//!
//! Output lands under `examples/fltk_hello/target/m26-out/`:
//!
//! - `bindings.rs` — generated Rust source the user includes
//! - `cxx_shims.cpp` — generated C++ trampolines
//! - `libfltk_bindings_demo.a` — compiled archive of the shims
//!
//! Plus a list of `cargo:rustc-link-*` directives the
//! downstream crate's `build.rs` would forward to Cargo.

use std::path::PathBuf;

use cxx_importer::build::Build;

fn main() {
    let manifest_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir.join("target").join("m26-out");
    std::fs::create_dir_all(&out_dir)
        .expect("create m26-out");

    let umbrella = manifest_dir.join("cpp").join("fltk_umbrella.hpp");
    let fltk_include = PathBuf::from("/opt/homebrew/include");
    let fltk_lib_dir =
        PathBuf::from("/opt/homebrew/Cellar/fltk/1.4.5/lib");

    println!("=== M26 build pipeline against FLTK 1.4.5 ===\n");
    println!("  umbrella header: {}", umbrella.display());
    println!("  output dir     : {}\n", out_dir.display());

    let outputs = Build::new()
        .header(&umbrella)
        .include_path(&fltk_include)
        .cpp_std("c++17")
        .cstr_ergonomics(true)
        // FLTK link inputs cribbed from `fltk-config --cxxflags
        // --ldflags` on macOS arm64.
        .lib_search_path(&fltk_lib_dir)
        .link("fltk")
        .link_static("pthread")
        .framework("Cocoa")
        .weak_framework("UniformTypeIdentifiers")
        .weak_framework("ScreenCaptureKit")
        // Pin OUT_DIR explicitly so the demo writes to a known
        // location instead of the cargo target dir.
        .out_dir(&out_dir)
        .compile("fltk_bindings_demo")
        .expect("Build::compile against FLTK");

    println!("\n=== Pipeline output ===");
    println!("  bindings.rs        : {}", outputs.bindings_path.display());
    println!("  cxx_shims.cpp      : {}", outputs.shims_path.display());
    println!("  static archive     : {}", outputs.static_lib_path.display());
    println!();

    let bindings_size = std::fs::metadata(&outputs.bindings_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let shims_size = std::fs::metadata(&outputs.shims_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let archive_size = std::fs::metadata(&outputs.static_lib_path)
        .map(|m| m.len())
        .unwrap_or(0);
    println!("=== Sizes ===");
    println!("  bindings.rs    : {bindings_size:>10} bytes");
    println!("  cxx_shims.cpp  : {shims_size:>10} bytes");
    println!("  archive        : {archive_size:>10} bytes");
    println!();

    println!("=== Cargo directives the build.rs would emit ===");
    for d in &outputs.cargo_directives {
        // cc emits its own directives; we only print the ones
        // we collected for the user-supplied link inputs.
        println!("  {d}");
    }

    println!(
        "\n=== Next step ===\n\
         To actually link a Rust binary against this archive, you need\n\
         the rustcc fork rustc (for the `extern \"C++\"` ABI). The\n\
         workflow is:\n\
         \n\
         1. Build the fork: `cd ../.. && ./fork/build.sh`.\n\
         2. `rustup toolchain link rustcc <stage1-path>`.\n\
         3. In a downstream crate, `include!(\"{}\")`\n\
            and call `cargo:rustc-link-search=`/`-link-lib=` for\n\
            the archive at `{}`.\n\
         4. `cargo +rustcc run`.\n\
         \n\
         The full hello-world driver lands in a follow-up release\n\
         once the fork toolchain is available on this machine.\n",
        outputs.bindings_path.display(),
        outputs.static_lib_path.display(),
    );
}
