//! v1.12.2: validates that `[[clang::annotate("rustcc::cxx_throws")]]`
//! on a free-function declaration causes the bindings emitter to
//! produce the Phase 0 catch-shim shape — same surface as the
//! v1.12.1 `RustBindingsConfig::cxx_throws_functions` knob, but
//! driven by inline source markup instead of caller-side config.
//!
//! Two pieces under test:
//!   1. The libclang import walker reads the `AnnotateAttr` child
//!      of a `FunctionDecl` and surfaces `Annotation::CxxThrows`
//!      keyed by the function's FQN.
//!   2. The bindings emitter consults the annotation set (in
//!      addition to the existing config knob) when deciding
//!      whether to emit a throws-aware extern + Result-returning
//!      safe wrapper.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::rust_bindings::{
    generate_rust_bindings_full, BindingsBackend, RustBindingsConfig,
};
use cxx_importer::{import_header_with_extras, Annotation};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_anno_{tag}_{}_{}",
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
fn annotation_drives_throws_emission_for_free_function() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("freefn");
    let hdr = dir.join("h.hpp");

    // `do_clean` is unannotated → plain extern "C++" + i32 return.
    // `do_divide` carries `[[clang::annotate("rustcc::cxx_throws")]]`
    //   → emitter should produce the shim shape: extern "C" decl
    //     targeting __rustcc_throws_do_divide + Result wrapper.
    std::fs::write(
        &hdr,
        r#"#pragma once
int do_clean();

[[clang::annotate("rustcc::cxx_throws")]]
int do_divide(int a, int b);
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

    // The annotation set must record CxxThrows on do_divide's FQN.
    let do_divide_anns = extras.annotations.effective("do_divide");
    assert!(
        do_divide_anns.iter().any(|a| matches!(a, Annotation::CxxThrows)),
        "expected CxxThrows annotation on do_divide; got {do_divide_anns:?}"
    );
    let do_clean_anns = extras.annotations.effective("do_clean");
    assert!(
        !do_clean_anns.iter().any(|a| matches!(a, Annotation::CxxThrows)),
        "do_clean should NOT be annotated; got {do_clean_anns:?}"
    );

    // Drive the emitter with the empty config knob — the
    // throws decision must come purely from the annotation.
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

    // do_divide: shim shape.
    assert!(
        src.contains("__rustcc_throws_do_divide"),
        "expected shim symbol for do_divide; src:\n{src}"
    );
    assert!(
        src.contains("::core::result::Result<i32, ::cxx::CxxException>"),
        "expected Result-returning wrapper for do_divide; src:\n{src}"
    );
    // do_clean: still plain — its signature line should NOT
    // contain Result. We scope the check to just the signature
    // line because the following function (do_divide) IS
    // Result-wrapped and lives a few lines below in the
    // emitted text.
    let clean_idx = src.find("pub fn do_clean").expect("do_clean wrapper missing");
    let line_end = src[clean_idx..]
        .find('\n')
        .map(|i| clean_idx + i)
        .unwrap_or(src.len());
    let clean_sig = &src[clean_idx..line_end];
    assert!(
        !clean_sig.contains("Result<"),
        "do_clean signature shouldn't be Result-wrapped; line:\n{clean_sig}"
    );
}

#[test]
fn sidecar_throws_drives_emission_for_free_function() {
    // Sidecar lookup keys methods on type entries — free fns
    // aren't yet in the sidecar schema. This is a placeholder
    // for the v1.12.3 follow-on that extends the schema with
    // a top-level `free_functions:` map. Today, sidecar-driven
    // throws annotation on free fns is unsupported; tracking it
    // here so the gap stays visible.
    eprintln!("skip: sidecar `free_functions:` schema is v1.12.3 work");
}
