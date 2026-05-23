//! v1.12.21 demo build.rs — drives `cxx_importer::build::Build`
//! against the example library header. The orchestrator:
//!
//! 1. Parses `cpp/library.hpp` via libclang.
//! 2. Harvests every `[[clang::annotate("rustcc::…")]]` annotation.
//! 3. Emits `bindings.rs` (Result-returning wrappers for the
//!    annotated functions) under `OUT_DIR`.
//! 4. Emits `cxx_shims.cpp` containing the matching
//!    `__rustcc_throws_*` shim bodies.
//! 5. Compiles `cxx_shims.cpp` AND `cpp/library.cpp` together
//!    via cc::Build into one static archive.
//! 6. Prints the `cargo:rustc-link-*` directives so the main.rs
//!    binary links against the archive.
//!
//! The library.cpp file holds the actual function bodies (the
//! ones the shims trampoline into); cxx_shims.cpp is the catch
//! wrapper layer. Both compile into the same static lib so the
//! final link has every symbol the bindings.rs references.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cpp_dir = manifest_dir.join("cpp");
    let header = cpp_dir.join("library.hpp");
    let lib_cpp = cpp_dir.join("library.cpp");

    // Rerun if either the header or the implementation
    // changes. cxx_importer also emits its own rerun directives
    // for the header it parses.
    println!("cargo:rerun-if-changed={}", header.display());
    println!("cargo:rerun-if-changed={}", lib_cpp.display());

    // Drive the orchestrator. Emits bindings.rs + cxx_shims.cpp
    // (with throws-shim bodies) under OUT_DIR and links them
    // into a static archive.
    cxx_importer::build::Build::new()
        .header(&header)
        .include_path(&cpp_dir)
        .cpp_std("c++17")
        .compile("cxx_throws_demo_shims")
        .expect("Build::compile");

    // The user library's own implementation TU. Build into a
    // SECOND static archive so cargo links both. We use a
    // fresh cc::Build call so the shim-emitting orchestrator
    // above stays single-purpose.
    cc::Build::new()
        .cpp(true)
        .file(&lib_cpp)
        .include(&cpp_dir)
        .flag_if_supported("-std=c++17")
        .compile("cxx_throws_demo_library");
}
