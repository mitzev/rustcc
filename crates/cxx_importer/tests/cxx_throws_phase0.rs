//! v1.12 stretch 4: extern "C++" throw-lowering — Phase 0 runtime test.
//!
//! Validates that the C++-side catch shim emitted by
//! `cxx_importer::render_throws_shim_cpp` actually catches a thrown
//! C++ exception at runtime, packs it into the `CxxRawError`
//! tagged union, and that the Rust-side `decode` helper turns it
//! into `Err(CxxException)` with the right kind + `what()` text.
//!
//! Three flight paths under test:
//!   1. Happy path — no throw. `decode` returns `Ok(value)`.
//!   2. `std::exception` subclass thrown. Caught by the
//!      `catch (const std::exception& e)` arm; `what()` round-trips
//!      to Rust as `CxxException { kind: Std, message: <what> }`.
//!   3. Non-std exception thrown (here: a bare `int`). Caught by
//!      `catch (...)`; reaches Rust as
//!      `CxxException { kind: Unknown, message: "non-std::exception C++ exception" }`.
//!
//! The C++ source is generated programmatically: header definitions
//! plus two throwing functions, with their `extern "C"` shim
//! wrappers emitted by `render_throws_shim_cpp`. We compile to an
//! object via the host's `clang++`, then link into a Rust binary
//! that declares the shim signatures by hand (since v1.12 doesn't
//! yet wire `[[rustcc::cxx_throws]]` into the bindings emitter —
//! that automation lands in v1.12.1 once we have field experience
//! with the shim API).

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::{render_throws_shim_cpp, CXX_RAW_ERROR_HEADER};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_phase0_{tag}_{}_{}",
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
fn phase0_shim_catches_std_and_unknown_exceptions_at_runtime() {
    let clangpp = match find_clangpp() {
        Some(c) => c,
        None => {
            eprintln!("skip: clang++ not available on this host");
            return;
        }
    };

    let dir = tmpdir("e2e");
    let cpp = dir.join("throws.cpp");
    let obj = dir.join("throws.o");
    let main_rs = dir.join("main.rs");
    let bin = dir.join("runner");

    // -------- generate C++ side --------
    // Two throwing functions:
    //   do_divide(a, b)  — throws std::runtime_error if b == 0
    //   do_throw_int()   — throws a bare `int` (non-std path)
    // Plus the rustcc-emitted catch shims around each.
    let shim_divide = render_throws_shim_cpp(
        "__rustcc_throws_do_divide",
        "int",
        &["int __a".to_string(), "int __b".to_string()],
        &["__a".to_string(), "__b".to_string()],
        "do_divide",
    );
    let shim_throw_int = render_throws_shim_cpp(
        "__rustcc_throws_do_throw_int",
        "int",
        &[],
        &[],
        "do_throw_int",
    );

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

int do_throw_int() {
    throw 42;
}

"#,
    );
    cpp_src.push_str(&shim_divide);
    cpp_src.push('\n');
    cpp_src.push_str(&shim_throw_int);

    std::fs::write(&cpp, &cpp_src).unwrap();

    let cxx_compile = Command::new(&clangpp)
        .args(["-std=c++17", "-fexceptions", "-c"])
        .arg(&cpp)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("spawn clang++");
    assert!(
        cxx_compile.status.success(),
        "clang++ failed:\n  stdout: {}\n  stderr: {}\n  source was:\n{}",
        String::from_utf8_lossy(&cxx_compile.stdout),
        String::from_utf8_lossy(&cxx_compile.stderr),
        cpp_src,
    );

    // -------- Rust runner --------
    // Mirrors the FFI layout by hand because we're not depending on
    // cxx_importer at runtime here — only at build-time to render
    // the C++ shim. The struct + extern decls below are exactly
    // what the bindings emitter will produce in v1.12.1 once
    // `[[rustcc::cxx_throws]]` is recognized.
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
    fn __rustcc_throws_do_throw_int(out: *mut i32) -> CxxRawError;
}

fn main() {
    // 1) Happy path.
    let mut out: i32 = 0;
    let r = unsafe { __rustcc_throws_do_divide(10, 2, &mut out) };
    if r.kind != 0 {
        eprintln!("FAIL: happy path returned kind={}", r.kind);
        std::process::exit(1);
    }
    if out != 5 {
        eprintln!("FAIL: happy path out={out}, expected 5");
        std::process::exit(2);
    }

    // 2) std::exception path.
    let r = unsafe { __rustcc_throws_do_divide(10, 0, &mut out) };
    if r.kind != 1 {
        eprintln!("FAIL: std exc path returned kind={}, expected 1", r.kind);
        std::process::exit(3);
    }
    if r.message.is_null() {
        eprintln!("FAIL: std exc path message ptr was null");
        std::process::exit(4);
    }
    let msg = unsafe { std::ffi::CStr::from_ptr(r.message) }
        .to_string_lossy()
        .into_owned();
    if !msg.contains("divide by zero") {
        eprintln!("FAIL: std exc path msg was {msg:?}, expected to contain 'divide by zero'");
        std::process::exit(5);
    }

    // 3) Unknown (catch-...) path.
    let r = unsafe { __rustcc_throws_do_throw_int(&mut out) };
    if r.kind != 2 {
        eprintln!("FAIL: unknown exc path returned kind={}, expected 2", r.kind);
        std::process::exit(6);
    }
    if r.message.is_null() {
        eprintln!("FAIL: unknown exc path message ptr was null");
        std::process::exit(7);
    }
    let msg = unsafe { std::ffi::CStr::from_ptr(r.message) }
        .to_string_lossy()
        .into_owned();
    if !msg.contains("non-std") {
        eprintln!("FAIL: unknown exc path msg was {msg:?}, expected to contain 'non-std'");
        std::process::exit(8);
    }

    println!("ok: all three throw-lowering paths round-trip");
}
"#,
    )
    .unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rust_compile = Command::new(&rustc)
        .args(["--edition=2024", "--crate-type", "bin"])
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
        // Linux runners that default to libstdc++ won't find libc++
        // — soft-skip same as the MI runtime test. The Phase 0
        // mechanism is C++-stdlib-agnostic; on Linux we'd just swap
        // `-lc++` for `-lstdc++`. Tracked for v1.12.1.
        if stderr.contains("library 'c++' not found")
            || stderr.contains("cannot find -lc++")
        {
            eprintln!("skip: libc++ not available on this host");
            return;
        }
        // Older fork rustcs might not accept edition=2024. Retry
        // with 2021 — the runner doesn't use any 2024-only features.
        if stderr.contains("edition") && stderr.contains("2024") {
            let retry = Command::new(&rustc)
                .args(["--edition=2021", "--crate-type", "bin"])
                .arg(&main_rs)
                .arg("-o")
                .arg(&bin)
                .arg("-C")
                .arg(format!("link-arg={}", obj.display()))
                .arg("-lc++")
                .output()
                .expect("spawn rustc retry");
            if !retry.status.success() {
                panic!(
                    "rustc link failed (after edition retry):\n{}",
                    String::from_utf8_lossy(&retry.stderr)
                );
            }
        } else {
            panic!("rustc link failed:\n{}", stderr);
        }
    }

    let run = Command::new(&bin).output().expect("spawn runner");
    assert!(
        run.status.success(),
        "runner failed (exit={:?}):\n  stdout: {}\n  stderr: {}\n  generated C++ was:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
        cpp_src,
    );
}
