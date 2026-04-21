//! End-to-end capstone: `examples/cpp_class_demo` drives the rustcc
//! driver with `emit-forwarders = true`, builds a Rust staticlib
//! with Itanium-mangled exported bodies, links it with the C++
//! consumer, runs the binary, and verifies the return value equals
//! the result of actual Rust code execution.
//!
//! This is the "Rust fork equivalent for the happy path" — real
//! method bodies in Rust, callable from C++ via Itanium mangling,
//! without touching rustc internals.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn cpp_calls_real_rust_bodies_via_forwarders() {
    let ws = workspace_root();
    let demo = ws.join("examples/cpp_class_demo");
    assert!(demo.join("Cargo.toml").is_file());

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");

    // The real rustc — inherited from cargo's RUSTC env when this
    // test runs under `cargo test`. We NEED the real thing because
    // the rustcc driver forwards to rustc to actually compile the
    // Rust staticlib; a stub rustc would produce an empty archive.
    let real_rustc = std::env::var("RUSTC")
        .unwrap_or_else(|_| "rustc".into());

    let source = demo.join("src/lib.rs");
    let out_lib = tmp.path().join("libcpp_class_demo.a");

    // rustc needs access to the compiled rustcc_macros proc-macro.
    // cargo puts it under target/debug/deps/. We look up the
    // workspace's target/debug/deps directory the test binary lives
    // in and pass it via -L dependency=.
    let deps_dir = PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
        .parent()
        .expect("rustcc lives in target/debug")
        .join("deps");

    // Drive rustcc. It:
    //   (a) reads [package.metadata.cpp-interop] from demo/Cargo.toml
    //   (b) scans src/lib.rs for #[cpp_class] / #[repr(cpp)]
    //   (c) emits forwarders into <cache>/cpp_class_demo-cxx-forwarders.rs
    //   (d) sets RUSTCC_FORWARDERS_PATH + --cfg rustcc_forwarders
    //   (e) execs rustc to compile lib.rs → libcpp_class_demo.a
    let out = Command::new(rustcc_bin())
        .env("RUSTC", &real_rustc)
        .env("CARGO_TARGET_DIR", &cache)
        .args(["--crate-name", "cpp_class_demo"])
        .args(["--crate-type", "staticlib"])
        .args(["--edition", "2021"])
        .args(["--cfg", "cpp_interop"])
        .args([
            "-L",
            &format!("dependency={}", deps_dir.display()),
        ])
        // Proc-macros live under deps/; rustc discovers them via
        // --extern for each dep the crate uses.
        .args(["--extern", &resolve_extern_for("rustcc_macros", &deps_dir)])
        .arg("-o")
        .arg(&out_lib)
        .arg(&source)
        .output()
        .expect("spawn rustcc");
    assert!(
        out.status.success(),
        "rustcc driver failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rust-forwarders emitted"),
        "forwarders banner missing.\nstderr:\n{stderr}"
    );
    assert!(out_lib.is_file(), "staticlib not produced: {out_lib:?}");

    // Compile consumer.cpp and link against the Rust staticlib.
    let rustcc_cache_dir = cache.join("rustcc");
    let consumer = demo.join("cpp/consumer.cpp");
    let bin = tmp.path().join("demo");
    let compile = Command::new("clang++")
        .args(["-std=c++17", "-I"])
        .arg(&rustcc_cache_dir)
        .arg("-o")
        .arg(&bin)
        .arg(&consumer)
        .arg(&out_lib)
        .output()
        .expect("spawn clang++");
    assert!(
        compile.status.success(),
        "clang++ link failed.\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Run the binary. consumer.cpp returns `c.get()` which should
    // equal 10 + 32 = 42 after the two bumps. Any abort/wrong-value
    // proves the forwarder didn't route to the real Rust body.
    let run = Command::new(&bin).output().expect("spawn demo");
    assert_eq!(
        run.status.code(),
        Some(42),
        "expected exit code 42 from `c.get()` after two bumps.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("counter=42"),
        "unexpected stdout: {stdout}"
    );
}

/// Resolve the `--extern name=path` form for a rustc dep given a
/// cargo deps directory. Picks the most recently modified hashed
/// rlib matching the crate name.
fn resolve_extern_for(crate_name: &str, deps_dir: &Path) -> String {
    let prefix = format!("lib{crate_name}-");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in
        std::fs::read_dir(deps_dir).expect("read deps dir").flatten()
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Proc-macro crates are dylib on macOS, so on Apple
        // platforms we want .dylib; on Linux it's .so. Just accept
        // any compiled artifact matching the prefix.
        if !name.starts_with(&prefix) {
            continue;
        }
        if !(name.ends_with(".dylib")
            || name.ends_with(".so")
            || name.ends_with(".rlib"))
        {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
            best = Some((mtime, path));
        }
    }
    let (_, path) = best.expect("rustcc_macros artifact not found in deps/");
    format!("{crate_name}={}", path.display())
}

