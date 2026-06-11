//! End-to-end shim-generation test via the `Driver` high-level API.
//!
//! Flow: build a `HeaderGraph` → `Driver::parse_all` imports classes →
//! `Driver::emit_shims` produces compile-ready `.cpp` → invoke
//! `clang++ -c` → verify the resulting `.o` contains the expected
//! `__rustcc_shim_*` symbols.
//!
//! The substring-based unit tests in `shims.rs` cover the generator's
//! structural output without a toolchain. This test catches regressions
//! that only surface when the source is fed to a real compiler: invalid
//! C++ syntax, missing includes, return-type / parameter-type mismatches
//! against the real header, Itanium-mangled symbols that don't match
//! what the compiler emits.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use cxx_importer::{Driver, HeaderGraph};
use rustc_abi_cxx::{CxxTypeCtx, Target};

mod common;

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rustcc_shim_e2e_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir tmp");
    dir
}

fn write(path: &PathBuf, body: &str) {
    std::fs::write(path, body).unwrap_or_else(|e| {
        panic!("failed writing {}: {e}", path.display())
    });
}

#[test]
fn driver_emits_shims_that_compile_and_expose_expected_symbols() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };

    let dir = tmpdir("widget");
    let header_path = dir.join("widget.h");
    let shim_path = dir.join("widget_shims.cpp");
    let obj_path = dir.join("widget_shims.o");

    let header_src = "\
struct Widget {
    int state;
    int compute(int x) const;
    void tick();
    bool equals(const Widget& other) const;
};
";
    write(&header_path, header_src);

    // Absolute path so the shim's #include resolves regardless of the
    // compiler's cwd.
    let header_abs = header_path.canonicalize().expect("canonicalize header");

    let graph = HeaderGraph {
        roots: vec![header_abs.clone()],
        clang_flags: vec!["-std=c++17".into()],
        ..HeaderGraph::default()
    };
    let driver = Driver::new(graph);

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let classes = driver.parse_all(&mut ctx).expect("parse_all");
    assert_eq!(classes.len(), 1, "expected exactly one class imported");

    let shim_src = driver.emit_shims(&ctx, &classes).expect("emit_shims");
    assert!(shim_src.contains("__rustcc_shim_"));
    assert!(shim_src.contains("#include <exception>"));
    // Driver plumbed the header path into the output.
    assert!(
        shim_src.contains(header_abs.to_str().unwrap()),
        "expected driver to thread header path into #include:\n{shim_src}"
    );

    write(&shim_path, &shim_src);

    let status = Command::new(&tc.compiler)
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj_path)
        .arg(&shim_path)
        .status()
        .expect("spawn clang++ (is it on PATH?)");
    assert!(
        status.success(),
        "clang++ failed to compile driver-emitted shim at {}",
        shim_path.display()
    );

    // Mach-O `nm` prefixes external C symbols with `_`, so the shim
    // symbol `__rustcc_shim_X` appears as `___rustcc_shim_X`. A plain
    // substring check matches both Mach-O and ELF output.
    let nm = Command::new("nm")
        .arg(&obj_path)
        .output()
        .expect("spawn nm");
    assert!(nm.status.success(), "nm failed");
    let nm_out = String::from_utf8_lossy(&nm.stdout);

    for expected in [
        "__rustcc_shim__ZNK6Widget7computeEi",
        "__rustcc_shim__ZN6Widget4tickEv",
        "__rustcc_shim__ZNK6Widget6equalsERKS_",
    ] {
        assert!(
            nm_out.contains(expected),
            "nm output missing {expected}\nfull output:\n{nm_out}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn driver_handles_multiple_roots_with_shared_include_dir() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };

    let dir = tmpdir("multi");
    let include_dir = dir.join("include");
    std::fs::create_dir_all(&include_dir).unwrap();

    // Shared common header lives under include_dir; two roots #include
    // it using the `-I` path. This verifies the driver's include-path
    // flags are actually threaded through to libclang.
    let common_path = include_dir.join("common.h");
    write(
        &common_path,
        "\
#pragma once
struct Point { int x; int y; };
",
    );

    let a_path = dir.join("a.h");
    write(
        &a_path,
        "\
#include \"common.h\"
struct Alpha {
    int alpha_value() const;
};
",
    );

    let b_path = dir.join("b.h");
    write(
        &b_path,
        "\
#include \"common.h\"
struct Beta {
    void beta_tick();
};
",
    );

    let a_abs = a_path.canonicalize().unwrap();
    let b_abs = b_path.canonicalize().unwrap();

    let graph = HeaderGraph {
        roots: vec![a_abs.clone(), b_abs.clone()],
        include_paths: vec![include_dir.canonicalize().unwrap()],
        clang_flags: vec!["-std=c++17".into()],
        ..HeaderGraph::default()
    };
    let driver = Driver::new(graph);

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let classes = driver.parse_all(&mut ctx).expect("parse_all");

    // Alpha and Beta must both show up; Point shows up through `common.h`
    // in each TU but the driver dedups `ClassId`s so it appears once.
    let names: Vec<&str> = classes
        .iter()
        .map(|id| match ctx.class(*id).name.0.last().unwrap() {
            rustc_abi_cxx::NameSegment::Class(i)
            | rustc_abi_cxx::NameSegment::Namespace(i) => i.0.as_str(),
            _ => "<other>",
        })
        .collect();
    assert!(names.contains(&"Alpha"), "missing Alpha in {names:?}");
    assert!(names.contains(&"Beta"), "missing Beta in {names:?}");
    assert_eq!(
        names.iter().filter(|n| **n == "Point").count(),
        1,
        "Point should be deduped across roots, got: {names:?}"
    );

    // Shim source should compile cleanly with the same -I flag.
    let shim_src = driver.emit_shims(&ctx, &classes).expect("emit_shims");
    let shim_path = dir.join("multi_shims.cpp");
    let obj_path = dir.join("multi_shims.o");
    write(&shim_path, &shim_src);

    let status = Command::new(&tc.compiler)
        .args(["-std=c++17", "-I"])
        .arg(include_dir.canonicalize().unwrap())
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&shim_path)
        .status()
        .expect("spawn clang++");
    assert!(
        status.success(),
        "multi-header shim failed to compile: {}",
        shim_path.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
