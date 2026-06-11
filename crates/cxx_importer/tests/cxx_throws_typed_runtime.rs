//! v1.12.7 (Phase 3): typed-catches runtime e2e.
//!
//! Compiles a C++ object with three distinct exception types:
//!   - `MyErrorA` (deriving from `std::exception`)
//!   - `MyErrorB` (deriving from `std::exception`)
//!   - `std::runtime_error` (deriving from `std::exception`)
//!
//! Wraps a throwing function via `render_throws_shim_cpp_typed`
//! with `typed_catches = ["MyErrorA", "MyErrorB"]`. Calls from
//! Rust with arguments that select each path and asserts:
//!   - `throw MyErrorA{}` → `kind == 16` (CXX_EXC_TYPED_BASE + 0)
//!   - `throw MyErrorB{}` → `kind == 17` (CXX_EXC_TYPED_BASE + 1)
//!   - `throw std::runtime_error("...")` → `kind == 1` (CXX_EXC_STD,
//!     falls through to the std::exception arm because not in
//!     the typed list)
//!   - happy path → `kind == 0` (CXX_EXC_OK)
//!
//! Same pattern as `cxx_throws_phase0.rs` but stresses the typed
//! emission path.

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::{render_throws_shim_cpp_typed, CXX_RAW_ERROR_HEADER};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_typed_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

mod common;

#[test]
fn typed_shim_dispatches_into_per_type_kind_tags() {
    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };
    let clangpp = tc.compiler.clone();

    let dir = tmpdir("e2e");
    let cpp = dir.join("typed.cpp");
    let obj = dir.join("typed.o");
    let main_rs = dir.join("main.rs");
    let bin = dir.join("runner");

    // Build C++ source.
    let shim = render_throws_shim_cpp_typed(
        "__rustcc_throws_do_thing",
        "int",
        &["int __selector".into()],
        &["__selector".into()],
        "do_thing",
        &["MyErrorA".into(), "MyErrorB".into()],
    );

    let mut cpp_src = String::new();
    cpp_src.push_str("#include <stdexcept>\n");
    cpp_src.push_str("#include <string>\n");
    cpp_src.push_str("#include <exception>\n");
    cpp_src.push_str(CXX_RAW_ERROR_HEADER);
    cpp_src.push_str(
        r#"
class MyErrorA : public std::exception {
public:
    const char* what() const noexcept override { return "MyErrorA hit"; }
};
class MyErrorB : public std::exception {
public:
    const char* what() const noexcept override { return "MyErrorB hit"; }
};

int do_thing(int selector) {
    switch (selector) {
        case 0: return 42;
        case 1: throw MyErrorA{};
        case 2: throw MyErrorB{};
        case 3: throw std::runtime_error("plain runtime_error");
        default: throw 123;  // non-std, exercises catch-(...) path
    }
}

"#,
    );
    cpp_src.push_str(&shim);
    std::fs::write(&cpp, &cpp_src).unwrap();

    let cxx_compile = Command::new(&clangpp)
        // Stdlib must match the link below: clang++ pins libc++,
        // g++ keeps its libstdc++ default.
        .args(["-std=c++17", "-fexceptions"])
        .args(tc.stdlib_compile_flags())
        .arg("-c")
        .arg(&cpp)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("spawn C++ compiler");
    assert!(
        cxx_compile.status.success(),
        "clang++ failed:\n  stderr: {}\n  source:\n{}",
        String::from_utf8_lossy(&cxx_compile.stderr),
        cpp_src,
    );

    // Rust runner — exercises all 5 paths.
    std::fs::write(
        &main_rs,
        r#"use std::os::raw::c_char;

#[repr(C)]
struct CxxRawError {
    kind: u32,
    message: *const c_char,
}

unsafe extern "C" {
    fn __rustcc_throws_do_thing(selector: i32, out: *mut i32) -> CxxRawError;
}

fn check(selector: i32, expected_kind: u32, expected_msg_substr: Option<&str>) {
    let mut out: i32 = 0;
    let r = unsafe { __rustcc_throws_do_thing(selector, &mut out) };
    if r.kind != expected_kind {
        eprintln!(
            "FAIL selector={selector}: kind={} (expected {expected_kind})",
            r.kind,
        );
        std::process::exit(selector + 1);
    }
    if let Some(needle) = expected_msg_substr {
        assert!(!r.message.is_null(), "selector={selector}: null message");
        let msg = unsafe { std::ffi::CStr::from_ptr(r.message) }
            .to_string_lossy()
            .into_owned();
        if !msg.contains(needle) {
            eprintln!(
                "FAIL selector={selector}: msg={msg:?} doesn't contain {needle:?}",
            );
            std::process::exit(20 + selector);
        }
    }
}

fn main() {
    // Happy path: kind 0, no message check.
    let mut out: i32 = 0;
    let r = unsafe { __rustcc_throws_do_thing(0, &mut out) };
    if r.kind != 0 || out != 42 {
        eprintln!("FAIL happy: kind={} out={}", r.kind, out);
        std::process::exit(1);
    }

    // Typed catch: MyErrorA -> kind 16 (CXX_EXC_TYPED_BASE + 0).
    check(1, 16, Some("MyErrorA hit"));
    // Typed catch: MyErrorB -> kind 17 (CXX_EXC_TYPED_BASE + 1).
    check(2, 17, Some("MyErrorB hit"));
    // Untyped std::exception fallback -> kind 1.
    check(3, 1, Some("plain runtime_error"));
    // catch-(...) for the bare int -> kind 2.
    check(4, 2, Some("non-std"));

    println!("ok: typed throws all 5 paths round-trip");
}
"#,
    )
    .unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let mut cmd = Command::new(&rustc);
    cmd.args(["--edition=2021", "--crate-type", "bin"])
        .arg(&main_rs)
        .arg("-o")
        .arg(&bin)
        .arg("-C")
        .arg(format!("link-arg={}", obj.display()));
    if let Some(dir) = tc.lib_search_dir() {
        cmd.arg("-L").arg(format!("native={}", dir.display()));
    }
    cmd.args(tc.link_libs());
    let rust_compile = cmd.output().expect("spawn rustc");
    if !rust_compile.status.success() {
        let stderr = String::from_utf8_lossy(&rust_compile.stderr);
        if tc.stdlib_missing(&stderr) {
            eprintln!("skip: C++ stdlib not linkable on this host");
            return;
        }
        panic!("rustc link failed:\n{stderr}");
    }

    let run = Command::new(&bin).output().expect("spawn runner");
    assert!(
        run.status.success(),
        "runner failed (exit={:?}):\n  stdout: {}\n  stderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
