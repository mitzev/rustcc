//! M4 end-to-end: the rustcc binary emits the shim `.cpp` and
//! compiles it via `clang++ -c`, producing a `.o` in the cache dir.
//! Second run with unchanged inputs reuses the cached object.

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
fn interop_mode_emits_and_compiles_shims_then_caches() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    let cpp_dir = crate_dir.join("cpp");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&cpp_dir).unwrap();

    std::fs::write(
        cpp_dir.join("widget.hpp"),
        "struct Widget { int compute(int x) const; void tick(); };\n",
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

    // First run: shims compiled.
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
        stderr1.contains("shims compiled"),
        "expected 'shims compiled' banner: {stderr1:?}"
    );

    let cpp = crate_dir.join("target/rustcc/mycrate.shims.cpp");
    let obj = crate_dir.join("target/rustcc/mycrate.shims.o");
    assert!(cpp.is_file(), "expected shim source at {}", cpp.display());
    assert!(obj.is_file(), "expected shim object at {}", obj.display());
    let cpp_body = std::fs::read_to_string(&cpp).unwrap();
    assert!(
        cpp_body.contains("__rustcc_shim__ZNK6Widget7computeEi"),
        "shim source missing expected symbol\n{cpp_body}"
    );
    assert!(
        cpp_body.contains("__rustcc_shim__ZN6Widget4tickEv"),
        "shim source missing tick symbol\n{cpp_body}"
    );

    // Verify the compiled object carries both mangled shim symbols.
    let nm = Command::new("nm").arg(&obj).output().expect("spawn nm");
    assert!(nm.status.success());
    let nm_out = String::from_utf8_lossy(&nm.stdout);
    for expected in [
        "__rustcc_shim__ZNK6Widget7computeEi",
        "__rustcc_shim__ZN6Widget4tickEv",
    ] {
        assert!(
            nm_out.contains(expected),
            "nm output missing {expected}\n{nm_out}"
        );
    }

    // Second run: inputs unchanged → banner says 'cached', both files
    // stay in place (same mtime).
    let obj_mtime_before = std::fs::metadata(&obj).unwrap().modified().unwrap();

    let out2 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("shims cached"),
        "expected 'shims cached' banner: {stderr2:?}"
    );
    let obj_mtime_after = std::fs::metadata(&obj).unwrap().modified().unwrap();
    assert_eq!(
        obj_mtime_before, obj_mtime_after,
        "cached object shouldn't be rewritten"
    );

    // Third run: edit the header → banner flips back to 'compiled' and
    // the object is regenerated (new mtime).
    std::fs::write(
        cpp_dir.join("widget.hpp"),
        "struct Widget { int compute(int x, int y) const; void tick(); };\n",
    )
    .unwrap();
    // Sleep briefly to guarantee mtime resolution on all filesystems.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let out3 = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        out3.status.success(),
        "third invocation failed. stderr:\n{}",
        String::from_utf8_lossy(&out3.stderr)
    );
    let stderr3 = String::from_utf8_lossy(&out3.stderr);
    assert!(
        stderr3.contains("shims compiled"),
        "header edit should trigger recompile: {stderr3:?}"
    );
}
