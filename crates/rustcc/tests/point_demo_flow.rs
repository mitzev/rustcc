//! Full-pipeline capstone test on `examples/point_demo/`.
//!
//! - Run the `rustcc` binary in interop mode against the example's
//!   `src/lib.rs` with a stub `RUSTC`.
//! - The driver's rust-scan discovers `Point` and `Segment`; rust-hpp
//!   emits `point_demo-cxx.hpp`; rust-stubs emits the abort-ing
//!   `point_demo-cxx-stubs.cpp`.
//! - Compile `consumer.cpp` + the stubs with `clang++` into a binary.
//! - Run the binary; assert it prints the expected sizeof/alignof
//!   line and exits 0 (the consumer doesn't call any stub, so the
//!   aborts never fire).
//!
//! This is the pre-fork equivalent of "end-to-end, works in anger."

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `crates/rustcc`; walk up to the
    // workspace root.
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn write_stub_rustc(script_path: &Path) {
    std::fs::write(script_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script_path, perms).unwrap();
}

#[test]
fn point_demo_builds_and_runs_end_to_end() {
    let ws = workspace_root();
    let demo = ws.join("examples").join("point_demo");
    assert!(
        demo.join("Cargo.toml").is_file(),
        "example fixture missing at {}",
        demo.display()
    );

    // Use a sandbox target dir to keep the test hermetic.
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = tmp.path().join("target");
    let stub = tmp.path().join("rustc.sh");
    write_stub_rustc(&stub);

    let source = demo.join("src/lib.rs");
    let status = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .env("CARGO_TARGET_DIR", &target_dir)
        .args(["--crate-name", "point_demo"])
        .args(["--cfg", "cpp_interop"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        status.status.success(),
        "rustcc driver failed.\nstderr:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );

    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("rust-hpp emitted"),
        "rust-hpp banner missing: {stderr}"
    );
    assert!(
        stderr.contains("rust-stubs emitted"),
        "rust-stubs banner missing: {stderr}"
    );

    let cache = target_dir.join("rustcc");
    let hpp = cache.join("point_demo-cxx.hpp");
    let stubs_cpp = cache.join("point_demo-cxx-stubs.cpp");
    assert!(hpp.is_file(), "hpp missing at {}", hpp.display());
    assert!(stubs_cpp.is_file(), "stubs missing at {}", stubs_cpp.display());

    // Compile the consumer with the generated hpp + stubs. Include
    // path points at the cache dir so `#include "point_demo-cxx.hpp"`
    // resolves.
    let bin = tmp.path().join("demo");
    let consumer = demo.join("cpp/consumer.cpp");
    let compile = Command::new("clang++")
        .args(["-std=c++17", "-I"])
        .arg(&cache)
        .arg("-o")
        .arg(&bin)
        .arg(&stubs_cpp)
        .arg(&consumer)
        .output()
        .expect("spawn clang++");
    assert!(
        compile.status.success(),
        "clang++ link failed.\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Run it — the consumer doesn't call any stub, so it should exit
    // 0 and print the sizeof banner.
    let run = Command::new(&bin).output().expect("spawn demo");
    assert!(
        run.status.success(),
        "demo exited non-zero.\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("sizeof(Point)=8"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("sizeof(Segment)=16"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("sizeof(Orientation)=4"),
        "expected enum sizeof in stdout: {stdout}"
    );

    // The generated hpp must contain the enum declaration.
    let body = std::fs::read_to_string(&hpp).unwrap();
    assert!(
        body.contains("enum class Orientation : std::int32_t {"),
        "enum class declaration missing from hpp:\n{body}"
    );
    // User-written `impl Drop` surfaces as a dtor declaration in
    // Point. The hpp's canonical set always emits `~Point()` so the
    // assertion holds whether or not the Drop impl is honored, but
    // the stub .cpp should carry a matching body.
    let stubs_body = std::fs::read_to_string(&stubs_cpp).unwrap();
    assert!(
        stubs_body.contains("Point::~Point()"),
        "dtor stub missing for Drop-impl'd Point:\n{stubs_body}"
    );
}
