//! End-to-end demo: a real C++ header → libclang import → Rust source
//! emission via [`cxx_importer::rust_bindings`] → compile + link with
//! the C++ implementation → run.
//!
//! Proves the full pipeline works: a user can point `cxx_importer` at
//! a header, get back compilable Rust source, and Rust code that
//! `include!`s that source can call into the C++ class transparently.
//!
//! This test depends on the rustcc fork's `extern "C++"` ABI (the
//! emitted `native_cpp_class!` invocations expand to `extern "C++"`
//! decls). On stock rustc the test skips with a clear message.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use cxx_importer::import_header;
#[allow(unused_imports)]
use cxx_importer::rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

/// Process-exclusive: `Clang::new()` errors out on a second instance.
static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_bindings_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Probe the active rustc for `extern "C++"` support. The rustcc fork
/// accepts the ABI string; stock rustc rejects with E0703. Mirrors
/// the helper in `rust_forwarders_e2e.rs`.
fn rustc_supports_extern_cpp() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let dir = tmpdir("abi_probe");
    let src = dir.join("probe.rs");
    let out = dir.join("probe.rlib");
    if std::fs::write(&src, b"pub unsafe extern \"C++\" fn _x() {}\n").is_err() {
        return false;
    }
    Command::new(&rustc)
        .args(["--edition=2021", "--crate-type", "lib"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Pick a `Target` matching the host running this test. Imports are
/// host-targeted (we'll compile the C++ side with the host clang and
/// link the Rust side with the host rustc).
fn host_target() -> Target {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Target::aarch64_apple_darwin()
        } else {
            Target::x86_64_apple_darwin()
        }
    } else {
        // Default: Linux GNU.
        if cfg!(target_arch = "aarch64") {
            Target::aarch64_unknown_linux_gnu()
        } else {
            Target::x86_64_unknown_linux_gnu()
        }
    }
}

#[test]
fn imports_class_emits_bindings_links_to_cxx_and_calls_through() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    if !rustc_supports_extern_cpp() {
        eprintln!(
            "skipping: this end-to-end test needs the rustcc fork rustc \
             (set RUSTC=<fork>/build/<host>/stage1/bin/rustc)"
        );
        return;
    }

    let dir = tmpdir("calc_roundtrip");
    let header_hpp = dir.join("calc.hpp");
    let calc_cpp = dir.join("calc.cpp");
    let bindings_rs = dir.join("bindings.rs");
    let main_rs = dir.join("main.rs");
    let calc_obj = dir.join("calc.o");
    let bin = dir.join("runner");

    // Tiny C++ class: ctor stores two ints, a const method returns
    // their sum. Private fields make the size known to libclang's
    // record-layout query (8 bytes, 4-byte aligned for two ints).
    std::fs::write(
        &header_hpp,
        r#"#pragma once
class Calc {
public:
    Calc(int a, int b);
    ~Calc();
    int sum() const;
private:
    int a_;
    int b_;
};
"#,
    )
    .unwrap();
    std::fs::write(
        &calc_cpp,
        r#"#include "calc.hpp"
Calc::Calc(int a, int b) : a_(a), b_(b) {}
Calc::~Calc() {}
int Calc::sum() const { return a_ + b_; }
"#,
    )
    .unwrap();

    // Step 1: libclang import.
    let mut ctx = CxxTypeCtx::new(host_target());
    let class_ids = import_header(
        &header_hpp,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    assert_eq!(class_ids.len(), 1, "expected one imported class");

    // Step 2: emit Rust source via the new bindings emitter.
    // `DirectExternCpp` is the import-direction shape: a
    // `#[repr(C)]` opaque struct, an `unsafe extern "C++"` block
    // with `#[link_name = "_ZN…"]`-tagged decls, and safe
    // wrappers + `Drop`.
    let bindings_src = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig {
            backend: BindingsBackend::DirectExternCpp,
            crate_module: None,
            doc_hidden: false,
        },
    )
    .expect("emit_rust_bindings");
    std::fs::write(&bindings_rs, &bindings_src).unwrap();

    // Sanity: the emission contains the right shape pieces.
    assert!(
        bindings_src.contains("#[repr(C)]"),
        "expected repr(C) struct, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("unsafe extern \"C++\""),
        "expected unsafe extern \"C++\" block, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("_ZN4CalcC1Eii"),
        "expected ctor link_name, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("_ZN4CalcD1Ev"),
        "expected dtor link_name, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("_ZNK4Calc3sumEv"),
        "expected sum const-method link_name, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("pub fn new("),
        "expected ctor wrapper, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("pub fn sum("),
        "expected sum wrapper, got:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("impl ::core::ops::Drop for Calc"),
        "expected Drop impl, got:\n{bindings_src}"
    );

    // Step 3: compile the C++ side to an object file using the user's
    // host clang++.
    let cpp_compile = Command::new("clang++")
        .args(["-c", "-std=c++17", "-fPIC"])
        .arg("-o")
        .arg(&calc_obj)
        .arg(&calc_cpp)
        .output()
        .expect("spawn clang++");
    assert!(
        cpp_compile.status.success(),
        "clang++ compile failed:\n{}",
        String::from_utf8_lossy(&cpp_compile.stderr),
    );

    // Step 4: write the Rust consumer that uses the generated bindings.
    // `Calc::new(13, 24)` constructs a stack value via the C++ ctor;
    // `.sum()` returns 13 + 24 = 37 which we surface as the exit
    // code so the parent can verify call-through.
    // DirectExternCpp doesn't need any proc macros or unstable
    // feature gates beyond `extern "C++"` itself, which the fork
    // accepts as a normal ABI string.
    let main_src = format!(
        r#"include!({bindings_path:?});

fn main() {{
    let c = Calc::new(13, 24);
    let s = c.sum();
    std::process::exit(s);
}}
"#,
        bindings_path = bindings_rs.to_str().unwrap(),
    );
    std::fs::write(&main_rs, &main_src).unwrap();

    // Step 5: compile the Rust binary with the fork's stage1 rustc,
    // linking the C++ object directly. DirectExternCpp doesn't need
    // proc macros, so the rustc invocation is just source + linker
    // flags.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let cxx_runtime_link = if cfg!(target_os = "macos") { "-lc++" } else { "-lstdc++" };

    let rust_compile = Command::new(&rustc)
        .args(["--edition=2021"])
        .arg(&main_rs)
        .arg("-o")
        .arg(&bin)
        .arg(format!("-Clink-arg={}", calc_obj.display()))
        .arg(format!("-Clink-arg={cxx_runtime_link}"))
        .output()
        .expect("spawn rustc");
    assert!(
        rust_compile.status.success(),
        "rustc compile failed:\nstderr:\n{}\nbindings.rs:\n{}\nmain.rs:\n{}",
        String::from_utf8_lossy(&rust_compile.stderr),
        bindings_src,
        main_src,
    );

    // Step 6: run, expect exit code 37.
    let run = Command::new(&bin).output().expect("spawn runner");
    assert_eq!(
        run.status.code(),
        Some(37),
        "expected exit code 37 (13 + 24).\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}

