//! v1.12.10: end-to-end shim source rendering via
//! [`render_all_throws_shims_cpp`]. Builds a batch of typed +
//! untyped shim specs, asks the helper to render the complete
//! C++ TU, compiles it with `clang++`, links against a Rust
//! runner that exercises every path, and asserts each call
//! returns the right `CxxRawError` kind tag.
//!
//! This is the highest-fidelity Phase 0 test in the suite — it
//! covers the helper output verbatim, no manual rendering on
//! the test side beyond constructing the spec list.

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::{render_all_throws_shims_cpp, ThrowsShimSpec};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_throws_shim_renderer_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
fn render_all_throws_shims_emits_compilable_self_contained_tu() {
    let clangpp = match find_clangpp() {
        Some(c) => c,
        None => {
            eprintln!("skip: clang++ not available");
            return;
        }
    };

    let dir = tmpdir("e2e");
    let hdr = dir.join("user.hpp");
    let cpp = dir.join("shims.cpp");
    let obj = dir.join("shims.o");
    let main_rs = dir.join("main.rs");
    let bin = dir.join("runner");

    // User's "library" header — declares the wrapped functions
    // + the typed-exception classes the typed shim catches.
    std::fs::write(
        &hdr,
        r#"#pragma once
#include <stdexcept>

class MyErrorA : public std::exception {
public:
    const char* what() const noexcept override { return "MyErrorA hit"; }
};

int do_divide(int a, int b);  // untyped throws
int do_typed(int selector);   // typed throws via MyErrorA
"#,
    )
    .unwrap();

    // The library's implementation lives in the same .cpp as
    // the shims for simplicity — in real use the user has it
    // in a separate TU.
    let user_impl = r#"
int do_divide(int a, int b) {
    if (b == 0) throw std::runtime_error("divide by zero");
    return a / b;
}

int do_typed(int selector) {
    if (selector == 1) throw MyErrorA{};
    if (selector == 2) throw std::runtime_error("typed fallback");
    if (selector == 3) throw 99;  // bare int, hits catch-(...)
    return 7;
}
"#;

    // Build the shim spec list.
    let specs = vec![
        ThrowsShimSpec {
            wrapper_name: "__rustcc_throws_do_divide".into(),
            return_type_cpp: "int".into(),
            param_decls: vec!["int __a".into(), "int __b".into()],
            forward_args: vec!["__a".into(), "__b".into()],
            original_callsite: "do_divide".into(),
            typed_catches: vec![],
        },
        ThrowsShimSpec {
            wrapper_name: "__rustcc_throws_do_typed".into(),
            return_type_cpp: "int".into(),
            param_decls: vec!["int __selector".into()],
            forward_args: vec!["__selector".into()],
            original_callsite: "do_typed".into(),
            typed_catches: vec!["MyErrorA".into()],
        },
    ];

    let mut cpp_src = render_all_throws_shims_cpp(&["user.hpp"], &specs);
    // Append the user implementation so the .o has both the
    // shims AND the wrapped functions.
    cpp_src.push_str(user_impl);
    std::fs::write(&cpp, &cpp_src).unwrap();

    let cxx_compile = Command::new(&clangpp)
        .args(["-std=c++17", "-fexceptions", "-c"])
        .arg(&cpp)
        .arg("-I")
        .arg(&dir)
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

    // Rust runner.
    std::fs::write(
        &main_rs,
        r#"use std::os::raw::c_char;

#[repr(C)]
struct CxxRawError {
    kind: u32,
    message: *const c_char,
}

unsafe extern "C" {
    fn __rustcc_throws_do_divide(a: i32, b: i32, out: *mut i32) -> CxxRawError;
    fn __rustcc_throws_do_typed(selector: i32, out: *mut i32) -> CxxRawError;
}

fn main() {
    // do_divide happy path.
    let mut out: i32 = 0;
    let r = unsafe { __rustcc_throws_do_divide(10, 2, &mut out) };
    if r.kind != 0 || out != 5 {
        eprintln!("FAIL happy: kind={} out={}", r.kind, out);
        std::process::exit(1);
    }

    // do_divide throws std::runtime_error → catch-all path emits
    // kind=1 (CXX_EXC_STD) because we used the plain (untyped)
    // shim for do_divide.
    let r = unsafe { __rustcc_throws_do_divide(10, 0, &mut out) };
    if r.kind != 1 {
        eprintln!("FAIL std-arm: kind={}", r.kind);
        std::process::exit(2);
    }

    // do_typed: happy path (selector=0).
    let r = unsafe { __rustcc_throws_do_typed(0, &mut out) };
    if r.kind != 0 || out != 7 {
        eprintln!("FAIL typed-happy: kind={} out={}", r.kind, out);
        std::process::exit(3);
    }

    // do_typed selector=1 throws MyErrorA → kind=16 (CXX_EXC_TYPED_BASE+0).
    let r = unsafe { __rustcc_throws_do_typed(1, &mut out) };
    if r.kind != 16 {
        eprintln!("FAIL typed-MyErrorA: kind={}", r.kind);
        std::process::exit(4);
    }

    // do_typed selector=2 throws std::runtime_error → falls
    // through to std::exception arm → kind=1.
    let r = unsafe { __rustcc_throws_do_typed(2, &mut out) };
    if r.kind != 1 {
        eprintln!("FAIL typed-std-fallback: kind={}", r.kind);
        std::process::exit(5);
    }

    // do_typed selector=3 throws an int → kind=2 (catch-(...)).
    let r = unsafe { __rustcc_throws_do_typed(3, &mut out) };
    if r.kind != 2 {
        eprintln!("FAIL typed-catch-all: kind={}", r.kind);
        std::process::exit(6);
    }

    println!("ok: render_all_throws_shims_cpp end-to-end");
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

#[test]
fn render_all_throws_shims_emits_expected_preamble() {
    // Static-shape check on the rendered output — header order,
    // CxxRawError struct, generation comment.
    let specs = vec![ThrowsShimSpec {
        wrapper_name: "__rustcc_throws_foo".into(),
        return_type_cpp: "int".into(),
        param_decls: vec![],
        forward_args: vec![],
        original_callsite: "foo".into(),
        typed_catches: vec![],
    }];
    let src = render_all_throws_shims_cpp(&["my_header.hpp"], &specs);

    // User header first.
    let inc_user = src.find("#include \"my_header.hpp\"").expect("user header");
    // Standard headers after.
    let inc_exc = src.find("#include <exception>").expect("exception");
    let inc_se = src.find("#include <stdexcept>").expect("stdexcept");
    let inc_str = src.find("#include <string>").expect("string");
    assert!(inc_user < inc_exc, "user header must come first");
    assert!(inc_exc < inc_se);
    assert!(inc_se < inc_str);

    // CxxRawError struct present, exactly once.
    let raw_err_count = src.matches("struct CxxRawError {").count();
    assert_eq!(raw_err_count, 1, "CxxRawError struct emitted exactly once");

    // The shim body itself.
    assert!(src.contains("extern \"C\" CxxRawError __rustcc_throws_foo"));
    assert!(src.contains("foo()"));
}

#[test]
fn render_all_throws_shims_routes_per_spec_to_typed_or_plain() {
    let specs = vec![
        ThrowsShimSpec {
            wrapper_name: "__rustcc_throws_plain".into(),
            return_type_cpp: "int".into(),
            param_decls: vec!["int x".into()],
            forward_args: vec!["x".into()],
            original_callsite: "plain".into(),
            typed_catches: vec![],
        },
        ThrowsShimSpec {
            wrapper_name: "__rustcc_throws_typed".into(),
            return_type_cpp: "int".into(),
            param_decls: vec!["int x".into()],
            forward_args: vec!["x".into()],
            original_callsite: "typed".into(),
            typed_catches: vec!["MyError".into()],
        },
    ];
    let src = render_all_throws_shims_cpp(&["h.hpp"], &specs);

    // Plain shim does NOT have a typed catch arm.
    let plain_idx = src
        .find("extern \"C\" CxxRawError __rustcc_throws_plain")
        .expect("plain shim");
    let typed_idx = src
        .find("extern \"C\" CxxRawError __rustcc_throws_typed")
        .expect("typed shim");
    let plain_body = &src[plain_idx..typed_idx];
    assert!(
        !plain_body.contains("MyError"),
        "plain shim shouldn't reference typed catch class; body:\n{plain_body}"
    );

    // Typed shim DOES have the typed arm + the right kind tag.
    let typed_body = &src[typed_idx..];
    assert!(typed_body.contains("catch (const MyError& __e)"));
    assert!(typed_body.contains("return { 16, __buf.c_str() };"));
}
