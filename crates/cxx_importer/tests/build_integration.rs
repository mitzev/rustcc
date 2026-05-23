//! M26 end-to-end integration: drive the full `Build::compile`
//! pipeline against a synthetic mini C++ library. Validates that
//! the orchestrator parses → emits Rust + C++ → invokes cc → and
//! produces an archive that links cleanly.
//!
//! These tests exercise the same path a downstream `build.rs`
//! takes, but use a tempfile-backed `OUT_DIR` instead of the
//! Cargo-set one (so `cargo test` doesn't need to be inside a
//! build.rs context).

#![cfg(feature = "build")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::build::{Build, BuildOutputs};

/// libclang's `Clang::new()` is a process-wide singleton; cargo
/// runs tests in the same binary in parallel so we serialize
/// every test that calls into the importer. Same pattern as
/// `tests/import.rs::LIBCLANG`.
static LIBCLANG: Mutex<()> = Mutex::new(());

/// Best-effort host-triple lookup for the cc-invoking
/// integration test. `cargo test` runs without setting `TARGET`
/// or `HOST` (those are build-script-specific), so we
/// reconstruct from `cfg!(...)` macros.
fn host_triple() -> &'static str {
    // Match every triple the cc crate's runner table knows
    // about. The list isn't exhaustive — the
    // MINI_CC_INTEGRATION test is opt-in and only run on
    // hosts the rustcc workspace targets.
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// Write a minimal but realistic C++ header to a tempdir so the
/// orchestrator has something to chew on. The library is just
/// big enough to exercise every Phase A–C feature in one TU:
/// a class with an enum, an alias, a static data member, an
/// inline + virtual method, plus a free function.
fn write_mini_lib(dir: &std::path::Path) -> PathBuf {
    let header = dir.join("mini.hpp");
    let body = r#"
#pragma once

namespace mini {

enum class Color { Red = 1, Green = 2, Blue = 3 };

using ColorInt = int;

struct Point {
    int x;
    int y;
    static int instances;
    Point() : x(0), y(0) {}
    int sum_xy() const { return x + y; }
};

int free_double(int v);

} // namespace mini
"#;
    std::fs::write(&header, body).expect("write mini.hpp");
    header
}

#[test]
fn compile_emits_bindings_shims_and_static_lib_for_a_synthetic_library() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let header = write_mini_lib(tmp.path());
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs: BuildOutputs = Build::new()
        .header(&header)
        .cpp_std("c++17")
        .out_dir(&out_dir)
        // Don't run cc — this test just verifies the
        // orchestrator wires every step and writes the
        // expected files. A separate test gates real cc
        // invocation behind `MINI_CC_INTEGRATION=1` so CI
        // without a C++ toolchain still passes.
        .invoke_cc(false)
        .compile("mini_bindings")
        .expect("compile orchestrator");

    // 1. Bindings file exists, is non-trivial, and references
    //    every Phase A–C side-table the synthetic header
    //    exercises.
    let bindings = std::fs::read_to_string(&outputs.bindings_path)
        .expect("read bindings");
    assert!(bindings.contains("pub struct Point"), "missing class: {bindings}");
    assert!(bindings.contains("pub enum Color"), "missing enum: {bindings}");
    assert!(
        bindings.contains("pub type ColorInt = i32;"),
        "missing alias: {bindings}",
    );
    assert!(
        bindings.contains("pub fn instances_ptr()"),
        "missing static-data accessor: {bindings}",
    );
    assert!(
        bindings.contains("pub fn free_double"),
        "missing free fn: {bindings}",
    );

    // 2. Shim source file exists. The exact contents depend on
    //    the shim emitter; we just sanity-check it's a non-
    //    trivial C++ source file.
    let shims =
        std::fs::read_to_string(&outputs.shims_path).expect("read shims");
    assert!(shims.contains("#include"), "shim should #include user headers: {shims}");

    // 3. Cargo directives include rerun-if-changed for the
    //    header we listed, and any link directives we
    //    configured. With the empty config they're empty;
    //    test the rerun directive against the header path.
    let header_str = header.display().to_string();
    let rerun = format!("cargo:rerun-if-changed={header_str}");
    assert!(
        outputs.cargo_directives.iter().any(|d| d == &rerun),
        "expected rerun directive {rerun:?} in {:?}",
        outputs.cargo_directives,
    );
}

#[test]
fn link_directives_include_user_specified_libs_and_search_paths() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let header = write_mini_lib(tmp.path());
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .invoke_cc(false)
        .lib_search_path("/opt/homebrew/Cellar/fltk/1.4.5/lib")
        .link("fltk")
        .framework("Cocoa")
        .weak_framework("ScreenCaptureKit")
        .compile("mini_bindings")
        .expect("compile");

    let directives = &outputs.cargo_directives;
    assert!(
        directives.iter().any(|d| d
            == "cargo:rustc-link-search=native=/opt/homebrew/Cellar/fltk/1.4.5/lib"),
        "missing search path directive in {directives:?}",
    );
    assert!(
        directives.iter().any(|d| d == "cargo:rustc-link-lib=fltk"),
        "missing fltk link directive in {directives:?}",
    );
    assert!(
        directives.iter().any(|d| d == "cargo:rustc-link-lib=framework=Cocoa"),
        "missing Cocoa framework directive in {directives:?}",
    );
    let weak = directives
        .iter()
        .find(|d| d.contains("ScreenCaptureKit"))
        .expect("ScreenCaptureKit directive missing");
    assert!(weak.contains("rustc-link-arg"));
}

#[test]
fn missing_out_dir_returns_clear_env_error() {
    // Don't override OUT_DIR and don't run inside a Cargo
    // build.rs — `compile` should surface a helpful error.
    // We can't reliably test this when `OUT_DIR` *is* set
    // (e.g. when run under `cargo test` itself in a workspace
    // member that has a build.rs). Skip the test in that
    // case rather than incorrectly assert.
    if std::env::var_os("OUT_DIR").is_some() {
        return;
    }
    let result = Build::new()
        .header("nonexistent.hpp")
        .invoke_cc(false)
        .compile("noop");
    match result {
        Err(cxx_importer::build::BuildError::EnvMissing(msg)) => {
            assert!(msg.contains("OUT_DIR"), "msg should mention OUT_DIR; got {msg}");
        }
        other => panic!("expected EnvMissing error; got {other:?}"),
    }
}

/// Real cc-invoking test, gated behind an env var so CI can
/// opt in. The Rust workspace's CI runner has clang installed
/// so we *could* run this unconditionally, but defaulting to
/// "off" keeps `cargo test --features build` quick when run
/// locally on machines that don't want to compile C++ for a
/// test.
#[test]
fn real_cc_invocation_produces_a_static_archive_when_opted_in() {
    if std::env::var_os("MINI_CC_INTEGRATION").is_none() {
        eprintln!(
            "skipping: set MINI_CC_INTEGRATION=1 to run the cc-invoking \
             integration test"
        );
        return;
    }
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let header = write_mini_lib(tmp.path());
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    // cc::Build reads `OUT_DIR` from the env. The standard
    // override path is to set it before calling compile().
    std::env::set_var("OUT_DIR", &out_dir);
    // cc also wants `TARGET` and `OPT_LEVEL` set — Cargo
    // populates these in a real build.rs invocation. Use the
    // host triple as a reasonable default for the test.
    if std::env::var_os("TARGET").is_none() {
        std::env::set_var("TARGET", host_triple());
    }
    if std::env::var_os("OPT_LEVEL").is_none() {
        std::env::set_var("OPT_LEVEL", "0");
    }
    if std::env::var_os("HOST").is_none() {
        std::env::set_var("HOST", host_triple());
    }

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .compile("mini_bindings")
        .expect("compile must succeed when MINI_CC_INTEGRATION=1");
    assert!(
        outputs.static_lib_path.exists()
            || outputs
                .static_lib_path
                .with_extension("lib")
                .exists(),
        "expected archive to exist after cc invocation",
    );
}

/// v1.12.14: annotations harvested from the header (notably
/// `[[clang::annotate("rustcc::cxx_throws")]]` and
/// `rustcc::skip`) must reach the bindings emitter through the
/// `Build` orchestrator. Without this wiring, the orchestrator
/// passed `AnnotationSet::default()` to
/// `generate_rust_bindings_full`, dropping every inline
/// annotation on the floor.
#[test]
fn compile_picks_up_inline_annotations_from_headers() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().expect("tempdir");
    let header = tmp.path().join("anno.hpp");
    std::fs::write(
        &header,
        r#"#pragma once
[[clang::annotate("rustcc::cxx_throws")]]
int risky_op(int x);

[[clang::annotate("rustcc::cxx_throws(MyErrorA, MyErrorB)")]]
int risky_typed_op(int x);

[[clang::annotate("rustcc::skip")]]
int internal_only();
"#,
    )
    .unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .invoke_cc(false)
        .compile("anno_bindings")
        .expect("compile orchestrator");

    let bindings = std::fs::read_to_string(&outputs.bindings_path)
        .expect("read bindings");

    // The two throwing fns should land with the shim shape —
    // extern "C" block + __rustcc_throws_<name> link symbol +
    // Result<T, ::cxx::CxxException> wrapper return.
    assert!(
        bindings.contains("__rustcc_throws_risky_op"),
        "expected risky_op shim symbol; bindings:\n{bindings}"
    );
    assert!(
        bindings.contains("__rustcc_throws_risky_typed_op"),
        "expected risky_typed_op shim symbol; bindings:\n{bindings}"
    );
    assert!(
        bindings.contains("Result<i32, ::cxx::CxxException>"),
        "expected Result-returning wrapper; bindings:\n{bindings}"
    );

    // skip annotation should be honored — internal_only must
    // NOT appear in the generated bindings.
    assert!(
        !bindings.contains("internal_only"),
        "skip-annotated fn shouldn't appear; bindings:\n{bindings}"
    );
}
