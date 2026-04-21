//! M3 end-to-end: the rustcc binary, in interop mode, runs libclang
//! via cxx_importer::Driver on the manifest-declared headers and
//! writes a fingerprint that the next run recognizes.

#![cfg(all(unix, feature = "libclang"))]

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
fn interop_mode_parses_headers_and_writes_fingerprint() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    let cpp_dir = crate_dir.join("cpp");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&cpp_dir).unwrap();

    std::fs::write(
        cpp_dir.join("widget.hpp"),
        "struct Widget { int compute(int x) const; };\n",
    )
    .unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "\
[package]
name = \"mycrate\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"cpp/widget.hpp\"]
clang-flags = [\"-std=c++17\"]
",
    )
    .unwrap();

    let source = src_dir.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub);

    // First run: expect a cache miss, class_count == 1, fingerprint file
    // written under target/rustcc/.
    let out1 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        out1.status.success(),
        "first invocation failed. stderr:\n{}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    assert!(
        stderr1.contains("parsed 1 class(es)"),
        "expected class count in banner: {stderr1:?}"
    );
    assert!(
        stderr1.contains("changed"),
        "first run should report inputs 'changed': {stderr1:?}"
    );

    let fp_path = crate_dir.join("target/rustcc/mycrate.fingerprint");
    assert!(fp_path.is_file(), "expected fingerprint at {}", fp_path.display());
    let fp_body = std::fs::read_to_string(&fp_path).unwrap();
    assert_eq!(fp_body.len(), 64, "fingerprint should be a hex SHA256");

    // Second run: unchanged inputs → banner reports 'unchanged'.
    let out2 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("unchanged"),
        "second run should report 'unchanged': {stderr2:?}"
    );

    // Edit the header. Third run: banner flips back to 'changed'.
    std::fs::write(
        cpp_dir.join("widget.hpp"),
        "struct Widget { int compute(int x, int y) const; };\n",
    )
    .unwrap();

    let out3 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(out3.status.success());
    let stderr3 = String::from_utf8_lossy(&out3.stderr);
    assert!(
        stderr3.contains("changed"),
        "header edit should bust fingerprint: {stderr3:?}"
    );
}
