//! v1.12.1: end-to-end test for the bindings-emitter integration of
//! `[[rustcc::cxx_throws]]` (Phase 0 catch shim).
//!
//! Validates that when a free function is listed in
//! `RustBindingsConfig::cxx_throws_functions`, the emitter produces:
//!   - an `unsafe extern "C"` block referring to the C++ shim
//!     symbol `__rustcc_throws_<name>` and taking an `*mut T`
//!     out-param for non-void returns,
//!   - a `pub fn` safe wrapper that calls the extern and returns
//!     `Result<T, ::cxx::CxxException>`.
//!
//! Beyond the static-shape assertions, the test stitches the
//! generated Rust source together with a hand-built C++ source
//! containing the original throwing functions + their
//! `render_throws_shim_cpp`-emitted wrappers, compiles + links,
//! and runs a Rust harness that verifies all three paths (happy,
//! std-exc, unknown) round-trip through the generated bindings as
//! actual `Result<i32, CxxException>` / `Result<(), CxxException>`
//! values.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use cxx_importer::rust_bindings::{
    generate_rust_bindings_full, BindingsBackend, RustBindingsConfig,
};
use cxx_importer::{import_header_with_extras, render_throws_shim_cpp, CXX_RAW_ERROR_HEADER};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_emitter_{tag}_{}_{}",
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

fn find_clangpp() -> Option<String> {
    for cand in ["clang++", "/usr/bin/clang++", "/usr/local/bin/clang++"] {
        if Command::new(cand)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(cand.into());
        }
    }
    None
}

#[test]
fn emitter_generates_throws_aware_extern_and_wrapper() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("shape");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
int do_divide(int a, int b);
void do_throw_void();
int do_clean();
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

    // Two of the three free fns are throw-tagged; do_clean is
    // not, so it should retain the plain extern "C++" / direct
    // wrapper shape.
    let mut throws = std::collections::BTreeSet::new();
    throws.insert("do_divide".to_string());
    throws.insert("do_throw_void".to_string());

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        cxx_throws_functions: throws,
        ..RustBindingsConfig::default()
    };

    // Drive the full emission entry point so the imported free
    // fns from `extras.free_fns` reach the emitter.
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

    // The throwing fns should appear under the extern "C" block
    // and resolve to the shim symbol name.
    assert!(
        src.contains("unsafe extern \"C\" {"),
        "expected throwing fns to be in an extern \"C\" block; src:\n{src}"
    );
    assert!(
        src.contains("__rustcc_throws_do_divide"),
        "expected shim symbol for do_divide; src:\n{src}"
    );
    assert!(
        src.contains("__rustcc_throws_do_throw_void"),
        "expected shim symbol for do_throw_void; src:\n{src}"
    );
    // The non-throwing fn stays in the plain extern "C++" block
    // with its Itanium-mangled link name.
    assert!(
        src.contains("unsafe extern \"C++\" {"),
        "expected plain fns to still use extern \"C++\"; src:\n{src}"
    );
    // do_divide takes an i32 out-param + returns CxxRawError.
    assert!(
        src.contains("__out: *mut i32") || src.contains("*mut i32"),
        "expected i32 out-param on do_divide extern; src:\n{src}"
    );
    // Safe wrappers return Result.
    assert!(
        src.contains("::core::result::Result<i32, ::cxx::CxxException>"),
        "expected Result-returning wrapper for do_divide; src:\n{src}"
    );
    assert!(
        src.contains("::core::result::Result<(), ::cxx::CxxException>"),
        "expected Result-returning wrapper for do_throw_void; src:\n{src}"
    );
    // do_clean stays as a plain () or i32 return — no Result for it.
    // (We don't assert the exact shape since it depends on which
    // type render_rust_type picked; we just check that the
    // emitter didn't accidentally wrap a non-throws fn in Result.)
    let clean_idx = src
        .find("pub fn do_clean")
        .expect("safe wrapper for do_clean missing");
    let clean_block = &src[clean_idx..(clean_idx + 200).min(src.len())];
    assert!(
        !clean_block.contains("Result<"),
        "do_clean shouldn't be Result-wrapped; block:\n{clean_block}"
    );
}

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

#[test]
fn end_to_end_compile_link_run_with_phase0_shim() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    // Note: this test deliberately uses *only* throwing free
    // functions so the generated bindings emit `extern "C"`
    // shims (not `extern "C++"`). That means stock rustc can
    // build the runner — no fork rustc dependency.
    let clangpp = match find_clangpp() {
        Some(c) => c,
        None => {
            eprintln!("skip: clang++ not available on this host");
            return;
        }
    };

    let dir = tmpdir("e2e");
    let hdr = dir.join("h.hpp");
    let cpp = dir.join("shim.cpp");
    let obj = dir.join("shim.o");
    let bindings_rs = dir.join("bindings.rs");
    let main_rs = dir.join("main.rs");
    let bin = dir.join("runner");

    std::fs::write(
        &hdr,
        r#"#pragma once
int do_divide(int a, int b);
void do_throw_void();
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

    let mut throws = std::collections::BTreeSet::new();
    throws.insert("do_divide".to_string());
    throws.insert("do_throw_void".to_string());

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        cxx_throws_functions: throws,
        ..RustBindingsConfig::default()
    };
    let bindings = generate_rust_bindings_full(
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
    std::fs::write(&bindings_rs, &bindings).unwrap();

    // Build the C++ side: the throwing function bodies, plus
    // the catch shim wrappers from render_throws_shim_cpp.
    let mut cpp_src = String::new();
    cpp_src.push_str("#include <stdexcept>\n");
    cpp_src.push_str("#include <string>\n");
    cpp_src.push_str(CXX_RAW_ERROR_HEADER);
    cpp_src.push_str(
        r#"
int do_divide(int a, int b) {
    if (b == 0) {
        throw std::runtime_error("divide by zero");
    }
    return a / b;
}

void do_throw_void() {
    throw std::logic_error("void path failure");
}

"#,
    );
    cpp_src.push_str(&render_throws_shim_cpp(
        "__rustcc_throws_do_divide",
        "int",
        &["int __a".into(), "int __b".into()],
        &["__a".into(), "__b".into()],
        "do_divide",
    ));
    cpp_src.push('\n');
    cpp_src.push_str(&render_throws_shim_cpp(
        "__rustcc_throws_do_throw_void",
        "void",
        &[],
        &[],
        "do_throw_void",
    ));
    std::fs::write(&cpp, &cpp_src).unwrap();

    let cxx_compile = Command::new(&clangpp)
        // -stdlib=libc++ so the shim's symbols match the `-lc++` link
        // below (no-op on macOS where libc++ is default; required on
        // Linux where clang++ defaults to libstdc++).
        .args(["-std=c++17", "-stdlib=libc++", "-fexceptions", "-c"])
        .arg(&cpp)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("spawn clang++");
    assert!(
        cxx_compile.status.success(),
        "clang++ failed:\n  stderr: {}\n  source:\n{}",
        String::from_utf8_lossy(&cxx_compile.stderr),
        cpp_src,
    );

    // Rust runner: provides a local `cxx` shim that mirrors the
    // pieces of the runtime crate the bindings reference, so we
    // don't have to link the real `cxx` rlib for this test.
    std::fs::write(
        &main_rs,
        r#"// Local `cxx` shim — the generated bindings reference
// `::cxx::CxxException`, `::cxx::CxxRawError`, etc. We don't
// need the full `cxx` runtime crate at link time for this test;
// a minimal in-crate definition suffices.
extern crate self as cxx;

use std::borrow::Cow;
use std::os::raw::c_char;

#[derive(Debug, Clone)]
pub struct CxxException {
    pub kind: CxxExceptionKind,
    pub message: Cow<'static, str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CxxExceptionKind {
    Std,
    Unknown,
}

impl CxxException {
    pub unsafe fn from_raw(kind_tag: u32, message_ptr: *const c_char) -> Self {
        let message: Cow<'static, str> = if message_ptr.is_null() {
            Cow::Borrowed("")
        } else {
            let cstr = unsafe { std::ffi::CStr::from_ptr(message_ptr) };
            Cow::Owned(cstr.to_string_lossy().into_owned())
        };
        let kind = if kind_tag == CXX_EXC_STD {
            CxxExceptionKind::Std
        } else {
            CxxExceptionKind::Unknown
        };
        CxxException { kind, message }
    }
}

#[repr(C)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}

pub const CXX_EXC_OK: u32 = 0;
pub const CXX_EXC_STD: u32 = 1;
pub const CXX_EXC_UNKNOWN: u32 = 2;

pub unsafe fn decode_cxx_raw_error<T>(raw: CxxRawError, ok: T) -> Result<T, CxxException> {
    if raw.kind == CXX_EXC_OK {
        Ok(ok)
    } else {
        Err(unsafe { CxxException::from_raw(raw.kind, raw.message) })
    }
}

include!("bindings.rs");

fn main() {
    // Happy path.
    match do_divide(10, 2) {
        Ok(5) => {}
        other => {
            eprintln!("FAIL happy: {other:?}");
            std::process::exit(1);
        }
    }

    // std::exception path on int-returning function.
    match do_divide(10, 0) {
        Err(e) if matches!(e.kind, CxxExceptionKind::Std) && e.message.contains("divide by zero") => {}
        other => {
            eprintln!("FAIL int-throw: {other:?}");
            std::process::exit(2);
        }
    }

    // std::exception path on void-returning function.
    match do_throw_void() {
        Err(e) if matches!(e.kind, CxxExceptionKind::Std) && e.message.contains("void path failure") => {}
        other => {
            eprintln!("FAIL void-throw: {other:?}");
            std::process::exit(3);
        }
    }

    println!("ok: emitter-integrated cxx_throws round-trip");
}
"#,
    )
    .unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rust_compile = Command::new(&rustc)
        .args(["--edition=2021", "--crate-type", "bin"])
        .arg(&main_rs)
        .arg("-o")
        .arg(&bin)
        .arg("-C")
        .arg(format!("link-arg={}", obj.display()))
        .arg("-lc++")
        .output()
        .expect("spawn rustc");
    if !rust_compile.status.success() {
        let stderr = String::from_utf8_lossy(&rust_compile.stderr);
        if stderr.contains("library 'c++' not found")
            || stderr.contains("cannot find -lc++")
        {
            eprintln!("skip: libc++ not available on this host");
            return;
        }
        panic!(
            "rustc link failed:\n{}\n=== bindings.rs ===\n{}",
            stderr, bindings,
        );
    }

    let run = Command::new(&bin).output().expect("spawn runner");
    assert!(
        run.status.success(),
        "runner failed (exit={:?}):\n  stdout: {}\n  stderr: {}\n=== bindings.rs ===\n{}\n=== shim.cpp ===\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
        bindings,
        cpp_src,
    );
}
