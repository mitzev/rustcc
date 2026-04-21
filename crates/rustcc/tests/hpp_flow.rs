//! M5 end-to-end: rustcc binary writes `<crate>-cxx.hpp` alongside the
//! shim artifacts and caches it when inputs are unchanged.

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
fn interop_mode_emits_hpp_alongside_shims_and_caches() {
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

    // First run: hpp emitted.
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
        stderr1.contains("hpp emitted"),
        "expected 'hpp emitted' banner: {stderr1:?}"
    );

    let hpp = crate_dir.join("target/rustcc/mycrate-cxx.hpp");
    assert!(hpp.is_file(), "expected {}", hpp.display());
    let body = std::fs::read_to_string(&hpp).unwrap();
    assert!(body.contains("#pragma once"), "missing pragma once\n{body}");
    assert!(body.contains("class Widget {"), "missing class decl\n{body}");
    assert!(
        body.contains("__rust_storage"),
        "missing opaque storage field\n{body}"
    );

    // The generated header should compile cleanly by itself — that's
    // the contract consumers rely on.
    let obj = tmp.path().join("hpp_compile_check.o");
    let consumer = tmp.path().join("hpp_consumer.cpp");
    std::fs::write(&consumer, format!("#include \"{}\"\n", hpp.display()))
        .unwrap();
    let status = Command::new("clang++")
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj)
        .arg(&consumer)
        .status()
        .expect("spawn clang++");
    assert!(status.success(), "generated hpp failed to compile on its own");

    // Second run: cached.
    let mtime_before = std::fs::metadata(&hpp).unwrap().modified().unwrap();
    let out2 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("hpp cached"),
        "expected 'hpp cached' banner: {stderr2:?}"
    );
    let mtime_after = std::fs::metadata(&hpp).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "cached hpp shouldn't be rewritten");
}
