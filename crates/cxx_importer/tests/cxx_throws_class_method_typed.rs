//! v1.12.11: `collect_class_method_throws_catches` end-to-end.
//!
//! Imports a header with three class methods carrying mixed
//! annotations:
//!   - plain (no throws)
//!   - `cxx_throws` (untyped)
//!   - `cxx_throws(MyErrorA, MyErrorB)` (typed)
//! plus a typed ctor.
//!
//! Asserts the helper surfaces ONLY the typed entries, keyed by
//! `Class::method` FQN, with the correct type lists.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{
    collect_class_method_throws_catches, import_header_with_extras,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_throws_class_typed_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host_target() -> Target {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Target::aarch64_apple_darwin()
        } else {
            Target::x86_64_apple_darwin()
        }
    } else if cfg!(target_arch = "aarch64") {
        Target::aarch64_unknown_linux_gnu()
    } else {
        Target::x86_64_unknown_linux_gnu()
    }
}

#[test]
fn collect_class_method_throws_catches_surfaces_typed_methods_only() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("class");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class MyErrorA {};
class MyErrorB {};
class DomainError {};

class Calc {
public:
    [[clang::annotate("rustcc::cxx_throws(DomainError)")]]
    Calc(int seed);                          // typed ctor

    int read() const;                        // plain

    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int a, int b);                // untyped throws

    [[clang::annotate("rustcc::cxx_throws(MyErrorA, MyErrorB)")]]
    int risky(int x);                        // typed throws
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let typed_map =
        collect_class_method_throws_catches(&ctx, &extras.annotations, &classes);
    eprintln!("=== typed map ===");
    for (k, v) in &typed_map {
        eprintln!("  {k} -> {v:?}");
    }

    // Typed ctor surfaces as Class::Class.
    assert_eq!(
        typed_map.get("Calc::Calc").map(|v| v.as_slice()),
        Some(["DomainError".to_string()].as_slice()),
        "expected typed ctor entry; got map={typed_map:?}"
    );

    // Typed method surfaces as Class::method.
    assert_eq!(
        typed_map.get("Calc::risky").map(|v| v.as_slice()),
        Some(["MyErrorA".to_string(), "MyErrorB".to_string()].as_slice()),
        "expected typed risky entry; got map={typed_map:?}"
    );

    // Untyped throws + plain read should NOT appear.
    assert!(
        !typed_map.contains_key("Calc::divide"),
        "untyped throws shouldn't appear in typed-catches map; got {typed_map:?}"
    );
    assert!(
        !typed_map.contains_key("Calc::read"),
        "plain method shouldn't appear; got {typed_map:?}"
    );

    // Total entries: exactly the 2 typed ones.
    assert_eq!(
        typed_map.len(),
        2,
        "expected exactly 2 typed entries; got {typed_map:?}"
    );
}

#[test]
fn collect_class_method_throws_catches_handles_namespaced_classes() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("nested");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
namespace ns {
namespace sub {

class ErrA {};
class ErrB {};

class Worker {
public:
    [[clang::annotate("rustcc::cxx_throws(ErrA, ErrB)")]]
    int do_work(int input);
};

}  // namespace sub
}  // namespace ns
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let typed_map =
        collect_class_method_throws_catches(&ctx, &extras.annotations, &classes);
    eprintln!("=== typed map (namespaced) ===");
    for (k, v) in &typed_map {
        eprintln!("  {k} -> {v:?}");
    }

    // The key must reflect the full FQN — the namespace path
    // is part of the lookup key for annotations.
    let entry = typed_map.get("ns::sub::Worker::do_work");
    assert!(
        entry.is_some(),
        "namespaced class FQN missing — got map={typed_map:?}"
    );
    assert_eq!(
        entry.unwrap().as_slice(),
        ["ErrA".to_string(), "ErrB".to_string()].as_slice(),
    );
}

#[test]
fn collect_class_method_throws_catches_empty_when_no_typed() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("empty");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class Plain {
public:
    Plain();
    int read() const;
    [[clang::annotate("rustcc::cxx_throws")]]
    int catch_all();  // untyped — should NOT appear
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let typed_map =
        collect_class_method_throws_catches(&ctx, &extras.annotations, &classes);
    assert!(
        typed_map.is_empty(),
        "expected empty typed-catches map; got {typed_map:?}"
    );
}
