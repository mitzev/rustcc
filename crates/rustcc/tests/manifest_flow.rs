//! End-to-end test: the rustcc binary, in interop mode, walks up from
//! the source-file argv, reads `[cpp-interop]` from `Cargo.toml`, and
//! surfaces a summary on stderr before forwarding to rustc.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

fn write_stub_rustc(script_path: &Path) {
    // Silent successful rustc — we only care that rustcc successfully
    // parsed the manifest and got here.
    std::fs::write(script_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script_path, perms).unwrap();
}

#[test]
fn interop_mode_parses_manifest_and_reports_counts() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    let cpp_dir = crate_dir.join("cpp");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&cpp_dir).unwrap();

    // A manifest with concrete header + include-path + clang-flag so
    // the driver's summary line has non-zero counts.
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "\
[package]
name = \"mycrate\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"cpp/widget.hpp\", \"cpp/stringpool.hpp\"]
header-search-paths = [\"cpp\"]
clang-flags = [\"-std=c++20\"]
",
    )
    .unwrap();
    std::fs::write(cpp_dir.join("widget.hpp"), "// stub\n").unwrap();
    std::fs::write(cpp_dir.join("stringpool.hpp"), "// stub\n").unwrap();

    let source = src_dir.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub);

    let output = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Banner reports the parsed counts.
    assert!(
        stderr.contains("2 header(s)"),
        "expected header count in banner: {stderr:?}"
    );
    assert!(
        stderr.contains("1 include path(s)"),
        "expected include-path count in banner: {stderr:?}"
    );
    assert!(
        stderr.contains("1 clang flag(s)"),
        "expected clang-flag count in banner: {stderr:?}"
    );
}

#[test]
fn interop_mode_fails_when_manifest_lacks_section() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source = src_dir.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub);

    let output = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        !output.status.success(),
        "expected non-zero exit when [cpp-interop] is missing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no [cpp-interop] section"),
        "expected missing-section error on stderr, got: {stderr:?}"
    );
}

#[test]
fn interop_mode_fails_when_no_manifest_is_reachable() {
    let tmp = TempDir::new().unwrap();
    // Source file sitting in a bare tmp dir with no Cargo.toml anywhere
    // up the tree we control (the walk may still find one at /;
    // guard by pointing at a directory we know has no ancestor
    // manifests — use the tmp dir itself).
    let orphan = tmp.path().join("orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    let source = orphan.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    // If a manifest exists at /tmp or higher we'd get a different
    // failure (MissingSection, most likely). Either way, the driver
    // must fail non-zero in interop mode without a valid config.
    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub);

    let output = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop"])
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    // The outcome depends on the ancestor chain; both "no manifest"
    // and "missing [cpp-interop]" are acceptable failure modes here.
    // What we're asserting is the contract: interop mode must not
    // silently forward when the config can't be loaded.
    if output.status.success() {
        panic!(
            "interop mode with unresolvable manifest unexpectedly \
             succeeded. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
