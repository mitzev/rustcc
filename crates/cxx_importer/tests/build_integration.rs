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

    // v1.12.15: the C++ shim source must include the matching
    // `__rustcc_throws_*` shim bodies so the bindings actually
    // link. Without this, the generated bindings reference
    // unresolved symbols.
    let shims = std::fs::read_to_string(&outputs.shims_path)
        .expect("read shims");
    assert!(
        shims.contains("extern \"C\" CxxRawError __rustcc_throws_risky_op"),
        "expected risky_op shim body in cxx_shims.cpp; shims:\n{shims}"
    );
    assert!(
        shims.contains("extern \"C\" CxxRawError __rustcc_throws_risky_typed_op"),
        "expected risky_typed_op shim body; shims:\n{shims}"
    );
    // Typed shim should have the per-type catch arms.
    assert!(
        shims.contains("catch (const MyErrorA& __e)"),
        "expected MyErrorA catch arm; shims:\n{shims}"
    );
    assert!(
        shims.contains("catch (const MyErrorB& __e)"),
        "expected MyErrorB catch arm; shims:\n{shims}"
    );
    // The CxxRawError struct should appear exactly once even
    // though both throws-shim emission and the legacy shim
    // emission run.
    assert_eq!(
        shims.matches("struct CxxRawError {").count(),
        1,
        "CxxRawError struct should appear exactly once; shims:\n{shims}"
    );
}

/// v1.12.16: class-method throws annotations should also reach
/// `cxx_shims.cpp` — without this, the generated bindings'
/// per-method Result wrappers reference unresolved
/// `__rustcc_throws_<Class>_<method>` symbols.
#[test]
fn compile_emits_throws_shim_bodies_for_class_methods() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().expect("tempdir");
    let header = tmp.path().join("calc.hpp");
    std::fs::write(
        &header,
        r#"#pragma once
class Calc {
public:
    Calc();
    int read() const;

    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int a, int b);

    [[clang::annotate("rustcc::cxx_throws(MyErrorA)")]]
    int risky(int x);
};

class MyErrorA {};
"#,
    )
    .unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .invoke_cc(false)
        .compile("calc_bindings")
        .expect("compile");

    let shims = std::fs::read_to_string(&outputs.shims_path)
        .expect("read shims");

    // Both annotated methods should have shim bodies pointing
    // at __this->method(args).
    assert!(
        shims.contains("extern \"C\" CxxRawError __rustcc_throws_Calc_divide"),
        "expected Calc::divide shim body; shims:\n{shims}"
    );
    assert!(
        shims.contains("extern \"C\" CxxRawError __rustcc_throws_Calc_risky"),
        "expected Calc::risky shim body; shims:\n{shims}"
    );
    // Const-method this-pointer is const-qualified so the
    // call into a non-const `divide` from inside the shim
    // type-checks (we used non-const `divide` here, so the
    // this-pointer is plain).
    assert!(
        shims.contains("Calc* __this"),
        "expected Calc* __this param for non-const method; shims:\n{shims}"
    );
    // Typed catch arm present for risky.
    assert!(
        shims.contains("catch (const MyErrorA& __e)"),
        "expected MyErrorA catch arm; shims:\n{shims}"
    );
    // Callsite goes through __this->.
    assert!(
        shims.contains("__this->divide(__a0, __a1)"),
        "expected __this->divide callsite; shims:\n{shims}"
    );
    // Non-throwing read() should NOT have a throws shim.
    assert!(
        !shims.contains("__rustcc_throws_Calc_read"),
        "non-throws method shouldn't emit a throws shim; shims:\n{shims}"
    );
}

/// v1.12.18: throws annotations on overloaded class methods
/// produce per-overload-unique shim symbols via the workspace's
/// existing `disambiguate_overloads` machinery. Before this PR
/// `int divide(int)` + `int divide(double)` collapsed to a
/// single `__rustcc_throws_Calc_divide` shim.
#[test]
fn compile_disambiguates_overloaded_class_method_throws_shims() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().expect("tempdir");
    let header = tmp.path().join("ovl.hpp");
    std::fs::write(
        &header,
        r#"#pragma once
class Calc {
public:
    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int a, int b);

    [[clang::annotate("rustcc::cxx_throws")]]
    double divide(double a, double b);
};
"#,
    )
    .unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .invoke_cc(false)
        .compile("ovl_bindings")
        .expect("compile");

    let shims = std::fs::read_to_string(&outputs.shims_path)
        .expect("read shims");
    eprintln!("=== shims ===\n{shims}");

    // Two distinct shim wrapper symbols should appear — one per
    // overload. The exact disambiguator the workspace's
    // `disambiguate_overloads` picks is implementation-defined,
    // but we know `divide` is the base name + each overload
    // adds a suffix derived from its C++ param types.
    let int_int = shims
        .matches("__rustcc_throws_Calc_divide_int_int")
        .count();
    let dbl_dbl = shims
        .matches("__rustcc_throws_Calc_divide_double_double")
        .count();
    // At least one of each disambiguated symbol must appear in
    // the extern fn declaration. (The exact match count depends
    // on whether disambiguate_overloads suffixes both overloads
    // or only the colliding ones — we accept either, just want
    // both overloads to land as distinct symbols.)
    assert!(
        int_int >= 1 || shims.contains("__rustcc_throws_Calc_divide(") && dbl_dbl >= 1,
        "expected int-int overload's shim symbol; shims:\n{shims}"
    );
    assert!(
        dbl_dbl >= 1,
        "expected double-double overload's shim symbol; shims:\n{shims}"
    );
}

/// v1.12.17: ctor throws annotations emit a placement-new shim.
/// The Rust ctor wrapper returns `Result<Self, CxxException>`
/// and calls `__rustcc_throws_<Class>_new(__slot.as_mut_ptr(), args)`;
/// the C++ shim does `new (__out) Class(args)` inside the try.
#[test]
fn compile_emits_ctor_throws_shim_with_placement_new() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().expect("tempdir");
    let header = tmp.path().join("resource.hpp");
    std::fs::write(
        &header,
        r#"#pragma once
class Resource {
public:
    [[clang::annotate("rustcc::cxx_throws")]]
    Resource(int initial);

    int value() const;
};
"#,
    )
    .unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let outputs = Build::new()
        .header(&header)
        .out_dir(&out_dir)
        .invoke_cc(false)
        .compile("resource_bindings")
        .expect("compile");

    let shims = std::fs::read_to_string(&outputs.shims_path)
        .expect("read shims");

    // Ctor shim is named `__rustcc_throws_Resource_new`.
    assert!(
        shims.contains("extern \"C\" CxxRawError __rustcc_throws_Resource_new"),
        "expected Resource ctor shim; shims:\n{shims}"
    );
    // First param is the out-slot pointer (no extra trailing
    // out-param since the slot IS the result).
    assert!(
        shims.contains("Resource* __out"),
        "expected Resource* __out first param; shims:\n{shims}"
    );
    // Body uses placement-new into the out-slot.
    assert!(
        shims.contains("new (__out) Resource(__a0)"),
        "expected placement-new in ctor shim body; shims:\n{shims}"
    );
    // Bindings should also have the Result<Self, _> wrapper
    // pointing at this shim.
    let bindings = std::fs::read_to_string(&outputs.bindings_path)
        .expect("read bindings");
    assert!(
        bindings.contains("pub fn new(arg0: i32) -> ::core::result::Result<Self, ::cxx::CxxException>"),
        "expected Result<Self, _> ctor wrapper; bindings:\n{bindings}"
    );
}

#[test]
fn compile_auto_instantiates_referenced_template_specializations() {
    // v1.13.3 task F: a template specialization referenced only by
    // pointer (so it isn't implicitly instantiated) is discovered and
    // force-instantiated by `Build`'s auto-instantiate pass, then
    // emitted as a concrete binding. Turning auto-instantiate off (with
    // no explicit list) leaves it out; an explicit `.instantiate(...)`
    // brings it back even with auto off.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let header = tmp.path().join("wrapper.hpp");
    std::fs::write(
        &header,
        "template<class T> struct Wrapper { T value; T unwrap() const; };\n\
         template<class T> T Wrapper<T>::unwrap() const { return value; }\n\
         // Referenced by pointer only: not implicitly instantiated, so\n\
         // it surfaces as a full class solely via the discovery pass.\n\
         struct Holder { Wrapper<int>* w; int tag; };\n\
         Holder make_holder();\n",
    )
    .unwrap();

    let compile_in = |sub: &str, auto: bool, explicit: Option<&str>| {
        let out_dir = tmp.path().join(sub);
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut b = Build::new();
        b.header(&header)
            .cpp_std("c++17")
            .out_dir(&out_dir)
            .auto_instantiate(auto)
            .invoke_cc(false);
        if let Some(e) = explicit {
            b.instantiate(e);
        }
        let outputs = b.compile("wrap_bindings").expect("compile");
        std::fs::read_to_string(&outputs.bindings_path).unwrap()
    };

    // Auto ON (default): Wrapper<int> is discovered, force-instantiated,
    // and emitted as the concrete `Wrapper_i32` with its `unwrap()`
    // method bound (`_ZNK7WrapperIiE6unwrapEv`).
    let auto = compile_in("auto_on", true, None);
    assert!(
        auto.contains("struct Wrapper_i32"),
        "auto-instantiate should emit the concrete Wrapper<int>:\n{auto}"
    );
    assert!(
        auto.contains("_ZNK7WrapperIiE6unwrapEv"),
        "the instantiated method should be bound:\n{auto}"
    );

    // Auto OFF, no explicit list: the pointer-only spec is left as an
    // opaque forward-decl — the concrete `Wrapper_i32` is not emitted.
    let none = compile_in("auto_off", false, None);
    assert!(
        !none.contains("Wrapper_i32"),
        "without auto-instantiate the spec should stay un-materialized:\n{none}"
    );

    // Auto OFF but explicitly requested: brought back.
    let explicit = compile_in("explicit", false, Some("Wrapper<int>"));
    assert!(
        explicit.contains("struct Wrapper_i32"),
        "explicit .instantiate(\"Wrapper<int>\") should emit it:\n{explicit}"
    );
}
