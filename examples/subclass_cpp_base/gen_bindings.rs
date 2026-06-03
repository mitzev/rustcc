//! Generate the `CppBase` Rust bindings from `cpp/cppbase.hpp` using
//! cxx_importer. Output goes to `target/gen-out/bindings.rs`, which
//! `src/lib.rs` pulls in via `include!`.
//!
//! For a polymorphic class the importer now emits a
//! `#[rustc_cxx_imported_vtable = "…"]` attribute (P09.x / 1.13.7) so
//! the Rust `class MyWidget : CppBase` in `src/lib.rs` can subclass it
//! with cross-boundary virtual dispatch.
//!
//! ```sh
//! cd examples/subclass_cpp_base
//! cargo run --bin gen_bindings --release      # needs libclang only
//! ```

use std::path::PathBuf;

use cxx_importer::build::Build;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir.join("target").join("gen-out");
    std::fs::create_dir_all(&out_dir).expect("create gen-out");

    let header = manifest_dir.join("cpp").join("cppbase.hpp");

    let outputs = Build::new()
        .header(&header)
        .cpp_std("c++17")
        .out_dir(&out_dir)
        .compile("subclass_cpp_base_shims")
        .expect("Build::compile against cppbase.hpp");

    println!("bindings.rs : {}", outputs.bindings_path.display());
    let bindings = std::fs::read_to_string(&outputs.bindings_path).unwrap_or_default();
    if let Some(line) = bindings.lines().find(|l| l.contains("rustc_cxx_imported_vtable")) {
        println!("imported-vtable attribute emitted:\n  {}", line.trim());
    } else {
        eprintln!("WARNING: no #[rustc_cxx_imported_vtable] attribute in generated bindings!");
    }
}
