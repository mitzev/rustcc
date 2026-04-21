//! End-to-end: a fixture crate with a `#[repr(cpp)]` struct under
//! `src/` drives the `rustcc` binary in interop mode; the Rust-scan
//! phase discovers the type and the rust-hpp phase writes a
//! `<crate>-cxx.hpp` to the cache dir whose contents the test then
//! reads and asserts against. Does NOT require libclang — only the
//! Rust-side pipeline.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

fn write_stub_rustc(script_path: &Path) {
    std::fs::write(script_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script_path, perms).unwrap();
}

#[test]
fn rust_scan_phase_emits_hpp_for_repr_cpp_types() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    let cpp_dir = crate_dir.join("cpp");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&cpp_dir).unwrap();

    // Minimal manifest with a [cpp-interop] section — the rest of the
    // pipeline reaches the rust-scan stage regardless of how many
    // headers there are.
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "\
[package]
name = \"mycrate\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"cpp/stub.hpp\"]
header-search-paths = []
clang-flags = []
",
    )
    .unwrap();
    std::fs::write(cpp_dir.join("stub.hpp"), "// stub\n").unwrap();

    // Crate source with a #[repr(cpp)] struct + impl.
    let lib_rs = "\
#[repr(cpp)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }
    pub fn magnitude_sq(&self) -> i32 { self.x * self.x + self.y * self.y }
}

#[repr(cpp)]
#[cpp_name = \"Counter64\"]
pub struct Counter { pub n: i64 }
";
    std::fs::write(src_dir.join("lib.rs"), lib_rs).unwrap();

    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub);

    let output = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .env("CARGO_TARGET_DIR", crate_dir.join("target"))
        .args(["--crate-name", "mycrate"])
        .args(["--cfg", "cpp_interop"])
        .arg(src_dir.join("lib.rs"))
        .output()
        .expect("spawn rustcc");
    assert!(
        output.status.success(),
        "exit: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rust-scan"),
        "missing rust-scan banner: {stderr:?}"
    );
    assert!(
        stderr.contains("2 #[repr(cpp)] type(s)"),
        "expected 2 rust types reported: {stderr:?}"
    );
    assert!(
        stderr.contains("rust-hpp emitted"),
        "missing rust-hpp banner: {stderr:?}"
    );

    // Verify the file was written and contains both types.
    let hpp = crate_dir
        .join("target")
        .join("rustcc")
        .join("mycrate-cxx.hpp");
    assert!(hpp.is_file(), "hpp not created at {}", hpp.display());
    let body = std::fs::read_to_string(&hpp).unwrap();
    assert!(body.contains("class Point {"), "body:\n{body}");
    assert!(
        body.contains("class Counter64 {"),
        "cpp_name override missing:\n{body}"
    );
    assert!(
        body.contains("alignas(4) unsigned char __rust_storage[8];"),
        "Point layout wrong:\n{body}"
    );
    assert!(
        body.contains("alignas(8) unsigned char __rust_storage[8];"),
        "Counter64 layout wrong (i64 → size 8 align 8):\n{body}"
    );
}

#[test]
fn second_run_caches_hpp_when_sources_unchanged() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("c");
    let src = crate_dir.join("src");
    let cpp = crate_dir.join("cpp");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&cpp).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname=\"c\"\nversion=\"0.1.0\"\n[cpp-interop]\n\
         headers=[\"cpp/s.hpp\"]\nheader-search-paths=[]\nclang-flags=[]\n",
    )
    .unwrap();
    std::fs::write(cpp.join("s.hpp"), "// stub\n").unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "#[repr(cpp)] pub struct X { pub a: i32 }\n",
    )
    .unwrap();

    let stub = tmp.path().join("rustc.sh");
    write_stub_rustc(&stub);

    let run = || {
        Command::new(rustcc_bin())
            .env("RUSTC", &stub)
            .env("CARGO_TARGET_DIR", crate_dir.join("target"))
            .args(["--crate-name", "c"])
            .args(["--cfg", "cpp_interop"])
            .arg(src.join("lib.rs"))
            .output()
            .unwrap()
    };
    let a = run();
    assert!(a.status.success());
    let a_err = String::from_utf8_lossy(&a.stderr).to_string();
    assert!(
        a_err.contains("rust-hpp emitted"),
        "first run should emit: {a_err:?}"
    );

    let b = run();
    assert!(b.status.success());
    let b_err = String::from_utf8_lossy(&b.stderr).to_string();
    assert!(
        b_err.contains("rust-hpp cached"),
        "second run should hit cache: {b_err:?}"
    );

    // Edit the Rust source — the fingerprint should flip and rust-hpp
    // should re-emit.
    std::fs::write(
        src.join("lib.rs"),
        "#[repr(cpp)] pub struct X { pub a: i32, pub b: i32 }\n",
    )
    .unwrap();
    let c = run();
    assert!(c.status.success());
    let c_err = String::from_utf8_lossy(&c.stderr).to_string();
    assert!(
        c_err.contains("rust-hpp emitted"),
        "edit-then-run should re-emit: {c_err:?}"
    );
}
