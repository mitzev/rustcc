//! v1.12.3: `[[clang::annotate("rustcc::cxx_throws")]]` on a class
//! method drives the same Phase 0 catch-shim emission as free fns,
//! threaded through the class's extern + impl block.
//!
//! Two pieces under test:
//!   1. The class-method emitter routes a throws-tagged method's
//!      extern decl into a separate `unsafe extern "C"` block
//!      (link name: `__rustcc_throws_<Class>_<method>`), takes
//!      `this` + user args + an `*mut RetTy` out-param, and
//!      returns `::cxx::CxxRawError`.
//!   2. The safe wrapper returns
//!      `Result<T, ::cxx::CxxException>` and decodes inline via
//!      `MaybeUninit` + `decode_cxx_raw_error`.
//!
//! v1.12.3 scope: Instance + Static methods only. Ctor / Dtor /
//! Virtual throws emission is rejected at classify-time with a
//! per-method skip diagnostic, tracked for v1.12.4.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::rust_bindings::{
    generate_rust_bindings_full, BindingsBackend, RustBindingsConfig,
};
use cxx_importer::import_header_with_extras;
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_class_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host_target() -> Target {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Target::aarch64_apple_darwin()
        } else {
            Target::x86_64_apple_darwin()
        }
    } else if cfg!(target_arch = "aarch64") {
        Target::aarch64_unknown_linux_gnu()
    } else {
        Target::x86_64_unknown_linux_gnu()
    }
}

#[test]
fn annotated_class_method_gets_throws_emission() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("instance");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class Calc {
public:
    Calc(int seed);

    int read() const;

    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int a, int b);

    [[clang::annotate("rustcc::cxx_throws")]]
    void touch();
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_full(
        &ctx,
        &classes,
        &extras.annotations,
        &extras.aliases,
        &extras.enums,
        &extras.free_fns,
        &extras.static_data,
        &cfg,
    )
    .expect("generate");

    eprintln!("=== generated bindings ===\n{src}\n");

    // Two extern blocks should appear: the existing extern "C++"
    // for the non-throwing parts (ctor, read) and the new
    // extern "C" for the two throws methods.
    assert!(
        src.contains("unsafe extern \"C++\" {"),
        "expected plain extern \"C++\" block; src:\n{src}"
    );
    assert!(
        src.contains("unsafe extern \"C\" {"),
        "expected throws extern \"C\" block; src:\n{src}"
    );

    // Throws extern decls: link names must point at the shim.
    assert!(
        src.contains("__rustcc_throws_Calc_divide"),
        "expected divide shim symbol; src:\n{src}"
    );
    assert!(
        src.contains("__rustcc_throws_Calc_touch"),
        "expected touch shim symbol; src:\n{src}"
    );
    // Out-param appears on divide (non-void), absent on touch (void).
    assert!(
        src.contains("__out: *mut i32"),
        "expected out-param on divide extern; src:\n{src}"
    );
    // Return clause swaps to CxxRawError on the extern decls.
    assert!(
        src.contains("-> ::cxx::CxxRawError"),
        "expected CxxRawError return on throws extern; src:\n{src}"
    );

    // Safe wrappers return Result.
    assert!(
        src.contains("pub fn divide(&mut self, arg0: i32, arg1: i32)"),
        "expected divide safe wrapper signature; src:\n{src}"
    );
    assert!(
        src.contains("::core::result::Result<i32, ::cxx::CxxException>"),
        "expected Result<i32, _> return on divide wrapper; src:\n{src}"
    );
    assert!(
        src.contains("::core::result::Result<(), ::cxx::CxxException>"),
        "expected Result<(), _> return on touch wrapper; src:\n{src}"
    );

    // Non-throwing read() stays plain: it's `&self`, returns i32.
    let read_idx = src.find("pub fn read(&self)").expect("read wrapper missing");
    let line_end = src[read_idx..]
        .find('\n')
        .map(|i| read_idx + i)
        .unwrap_or(src.len());
    let read_sig = &src[read_idx..line_end];
    assert!(
        !read_sig.contains("Result<"),
        "read() shouldn't be Result-wrapped; line:\n{read_sig}"
    );

    // The ctor's link_name should still be the Itanium-mangled C1
    // — NOT a shim symbol — because we never annotated it. Sanity
    // check that the C1 mangling for `Calc::Calc(int)` shows up.
    assert!(
        src.contains("__rustcc_throws_") && src.contains("_ZN4Calc"),
        "expected both throws shim + a Calc-prefixed mangled symbol; src:\n{src}"
    );
}

#[test]
fn throws_on_virtual_method_is_skipped_with_clear_diagnostic() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("virt");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class Base {
public:
    Base();
    virtual ~Base();

    [[clang::annotate("rustcc::cxx_throws")]]
    virtual int fail_me();
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_full(
        &ctx,
        &classes,
        &extras.annotations,
        &extras.aliases,
        &extras.enums,
        &extras.free_fns,
        &extras.static_data,
        &cfg,
    )
    .expect("generate");

    eprintln!("=== generated bindings ===\n{src}\n");

    // The virtual throws method must NOT appear as an emitted
    // safe wrapper — classify rejected it.
    assert!(
        !src.contains("pub fn fail_me("),
        "virtual throws method shouldn't emit a safe wrapper; src:\n{src}"
    );
    // The skipped-method comment block should mention it.
    assert!(
        src.contains("fail_me"),
        "expected skipped-method note to mention fail_me; src:\n{src}"
    );
    assert!(
        src.contains("v1.12.4"),
        "expected the diagnostic to point at the v1.12.4 follow-up; src:\n{src}"
    );
}
