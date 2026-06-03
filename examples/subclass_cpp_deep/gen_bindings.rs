//! Generate the `Shape`/`Drawable`/`Widget` Rust bindings from
//! `cpp/cppbase.hpp` using cxx_importer. Output goes to
//! `target/gen-out/bindings.rs`, which `src/mywidget.rs` pulls in via
//! `include!`.
//!
//! For a deep polymorphic chain the importer flattens the full primary
//! vtable into a single `#[rustc_cxx_imported_vtable = "…"]` attribute on
//! the deepest class (`Widget`) — every inherited slot from `Shape` and
//! `Drawable` included — so the Rust `class MyWidget : Widget` in
//! `src/mywidget.rs` can override virtuals introduced at any level.
//!
//! ```sh
//! cd examples/subclass_cpp_deep
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
        .compile("subclass_cpp_deep_shims")
        .expect("Build::compile against cppbase.hpp");

    println!("bindings.rs : {}", outputs.bindings_path.display());
    let bindings = std::fs::read_to_string(&outputs.bindings_path).unwrap_or_default();
    let attrs: Vec<&str> =
        bindings.lines().filter(|l| l.contains("rustc_cxx_imported_vtable")).collect();
    if attrs.is_empty() {
        eprintln!("WARNING: no #[rustc_cxx_imported_vtable] attribute in generated bindings!");
    } else {
        println!("imported-vtable attributes emitted ({}):", attrs.len());
        for line in attrs {
            println!("  {}", line.trim());
        }
    }
}
