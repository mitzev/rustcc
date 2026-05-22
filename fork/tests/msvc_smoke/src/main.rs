//! v1.09.0 MSVC ABI smoke test.
//!
//! Parses `headers/widget.h` via `cxx_importer` configured for an
//! MSVC target, runs the binding generator, and asserts the
//! emitted Rust source contains the expected MSVC-mangled
//! `#[link_name = "..."]` attributes. Exits with code 0 on
//! success; prints a clear failure banner otherwise.
//!
//! When `libclang` isn't available (e.g. on a runner without
//! `libclang-dev` / Apple's CLT headers), the test compiles but
//! immediately reports `skip: libclang not available` and exits 0.
//! That keeps the smoke test usable in environments where the
//! full pipeline isn't reachable.

#[cfg(feature = "libclang")]
fn main() {
    use cxx_importer::build::Build;
    use rustc_abi_cxx::Target;
    use std::path::PathBuf;

    let header = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("headers/widget.h");
    let out_dir =
        std::env::temp_dir().join(format!("msvc_smoke_out_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&out_dir);

    let mut b = Build::new();
    b.header(&header)
        .target(Target::x86_64_pc_windows_msvc())
        .invoke_cc(false)
        .out_dir(&out_dir);

    let outputs = match b.compile("msvc_smoke") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("fail: cxx_importer build failed: {e:?}");
            std::process::exit(1);
        }
    };

    let bindings_src = std::fs::read_to_string(&outputs.bindings_path)
        .expect("read bindings");

    // Sanity checks. Each MSVC-mangled symbol we expect to see in
    // the emitted bindings as a `#[link_name]` attribute. Virtual
    // methods go through the vtable so they don't carry a
    // `link_name` on the Rust side — those are validated via the
    // shim file separately.
    let expected_substrings = [
        // Ctor with int arg (the `H@Z` tail is the int param + no
        // exc spec). Real MSVC mangles `Widget(int)` as
        // `??0Widget@@QEAA@H@Z`.
        "??0Widget@@QEAA@H@Z",
        // Virtual dtor (`U` access letter — virtuality is recovered
        // via the implicit-virtual-dtor rule even though the class
        // declares `virtual ~Widget()` explicitly).
        "??1Widget@@UEAA@XZ",
    ];

    let mut all_present = true;
    for sym in &expected_substrings {
        if !bindings_src.contains(sym) {
            eprintln!("fail: expected MSVC symbol `{sym}` missing");
            all_present = false;
        }
    }

    // Bindings should generate safe Rust wrappers for the virtual
    // methods — both `next` and `current` should be present as
    // `pub fn` items (the actual MSVC mangling of the virtual
    // dispatch happens in the shim source, not in the Rust
    // bindings).
    let expected_wrapper_fns = ["pub fn next", "pub fn current"];
    for fname in &expected_wrapper_fns {
        if !bindings_src.contains(fname) {
            eprintln!("fail: expected wrapper `{fname}` missing");
            all_present = false;
        }
    }

    // Negative control: no Itanium symbols should leak through.
    if bindings_src.contains("link_name = \"_Z") {
        eprintln!("fail: bindings contain Itanium `_Z`-prefixed symbols");
        all_present = false;
    }

    if !all_present {
        eprintln!(
            "fail: bindings dump (first 4000 chars):\n{}",
            &bindings_src[..bindings_src.len().min(4000)]
        );
        std::process::exit(1);
    }

    println!("ok: msvc_smoke — 2 mangled symbols + 2 wrappers match");
}

#[cfg(not(feature = "libclang"))]
fn main() {
    println!("skip: libclang not available");
}
