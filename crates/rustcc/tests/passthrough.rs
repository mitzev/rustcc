//! Integration tests for the `rustcc` binary.
//!
//! Exercise the real executable by pointing `RUSTC` at a tiny shell
//! script that records its argv to a file, then assert the driver
//! forwarded every argument unchanged.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Path to the rustcc binary. Cargo sets `CARGO_BIN_EXE_<name>` for
/// each binary target when running `cargo test`, so we don't have to
/// guess target-dir layouts.
fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

/// Write a POSIX shell script that captures all argv to `out_path`
/// (one arg per line) and exits 0. Used as a stand-in for real rustc.
fn write_stub_rustc(script_path: &Path, out_path: &Path) {
    let body = format!(
        "#!/bin/sh\n\
         : > {out}\n\
         for a in \"$@\"; do\n\
         \tprintf '%s\\n' \"$a\" >> {out}\n\
         done\n",
        out = shell_escape(out_path.to_str().unwrap()),
    );
    std::fs::write(script_path, body).expect("write stub script");
    let mut perms = std::fs::metadata(script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script_path, perms).expect("chmod stub");
}

fn shell_escape(s: &str) -> String {
    // Simple single-quote escape — the paths we use are temp dirs we
    // control, so they shouldn't contain single quotes, but be safe.
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn read_captured_argv(out_path: &Path) -> Vec<String> {
    let body = std::fs::read_to_string(out_path).expect("read captured argv");
    body.lines().map(|s| s.to_string()).collect()
}

#[test]
fn passthrough_forwards_argv_verbatim_to_rustc() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = tmp.path().join("fake-rustc.sh");
    let captured = tmp.path().join("argv.txt");
    write_stub_rustc(&stub, &captured);

    let status = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args([
            "--edition",
            "2021",
            "--crate-name",
            "foo",
            "-C",
            "opt-level=3",
            "src/lib.rs",
        ])
        .status()
        .expect("spawn rustcc");
    assert!(status.success(), "rustcc exited non-zero: {status:?}");

    let argv = read_captured_argv(&captured);
    assert_eq!(
        argv,
        vec![
            "--edition",
            "2021",
            "--crate-name",
            "foo",
            "-C",
            "opt-level=3",
            "src/lib.rs",
        ],
    );
}

#[test]
fn interop_mode_still_forwards_argv_and_prints_banner() {
    // Build a crate-shaped tempdir with a valid [cpp-interop] section
    // so M2's manifest walk succeeds; then verify rustcc prints its
    // banner and forwards argv to the stub rustc.
    let tmp = TempDir::new().expect("tempdir");
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "\
[package]
name = \"mycrate\"
version = \"0.1.0\"

[cpp-interop]
headers = []
",
    )
    .unwrap();
    let source = src_dir.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    let stub = tmp.path().join("fake-rustc.sh");
    let captured = tmp.path().join("argv.txt");
    write_stub_rustc(&stub, &captured);

    let source_str = source.to_str().unwrap().to_string();
    let output = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop"])
        .arg(&source_str)
        .output()
        .expect("spawn rustcc");
    assert!(
        output.status.success(),
        "rustcc exited non-zero. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Banner proves we took the Interop branch; argv capture proves
    // we forwarded unchanged.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interop mode"),
        "expected banner on stderr, got: {stderr:?}"
    );

    let argv = read_captured_argv(&captured);
    // The original argv is preserved at the head; M6 appends link
    // flags (shim object + link-arg) after. We only assert on the
    // preserved prefix here — the argv-augmentation contract is
    // covered in depth by `link_flow.rs`.
    assert!(argv.len() >= 3, "argv should have at least the forwarded prefix: {argv:?}");
    assert_eq!(&argv[..3], &["--cfg".to_string(), "cpp_interop".to_string(), source_str]);
}

#[test]
fn missing_rustc_binary_exits_nonzero_with_message() {
    let status = Command::new(rustcc_bin())
        .env("RUSTC", "/nonexistent/rustc-binary-that-wont-be-found")
        .args(["src/lib.rs"])
        .output()
        .expect("spawn rustcc");
    assert!(!status.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("failed to exec") || stderr.contains("failed to spawn"),
        "expected failure message, got: {stderr:?}"
    );
}
