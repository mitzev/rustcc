//! v1.12.9: `rustcc::cxx_throws(T1, T2, …)` annotation parser
//! + sidecar `throws_types: [...]` + the `collect_throws_catches`
//! helper that surfaces the typed list to consumers building the
//! matching C++ shim.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{
    collect_throws_catches, import_header_with_extras, load_sidecar, Annotation,
    AnnotationSet,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_typed_anno_{tag}_{}_{}",
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
fn inline_typed_annotation_parses_into_cxx_throws_typed_variant() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("inline");
    let hdr = dir.join("h.hpp");
    // Use a forward-declared exception class so the header
    // compiles without including any STL headers (keeps the
    // test hermetic).
    std::fs::write(
        &hdr,
        r#"#pragma once
class MyErrorA {};
class MyErrorB {};

[[clang::annotate("rustcc::cxx_throws(MyErrorA, MyErrorB)")]]
int do_thing(int x);
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (_classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let anns = extras.annotations.effective("do_thing");
    let typed = anns.iter().find_map(|a| match a {
        Annotation::CxxThrowsTyped(v) => Some(v.clone()),
        _ => None,
    });
    let typed = typed.expect("CxxThrowsTyped should appear in annotations");
    assert_eq!(typed, vec!["MyErrorA".to_string(), "MyErrorB".to_string()]);
}

#[test]
fn inline_typed_annotation_with_nested_generics_keeps_args_intact() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("nested");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class A {};
template <typename K, typename V> class Pair {};

[[clang::annotate("rustcc::cxx_throws(A, Pair<int, double>)")]]
void do_thing();
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (_classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let typed = extras
        .annotations
        .effective("do_thing")
        .into_iter()
        .find_map(|a| match a {
            Annotation::CxxThrowsTyped(v) => Some(v),
            _ => None,
        })
        .expect("CxxThrowsTyped variant");
    // The nested-generics splitter must keep `Pair<int, double>`
    // intact — not slice it into `Pair<int` + `double>`.
    assert_eq!(typed, vec!["A".to_string(), "Pair<int, double>".to_string()]);
}

#[test]
fn empty_parens_collapse_to_untyped_cxx_throws() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("empty");
    let hdr = dir.join("h.hpp");
    // Forgiveness rule: `cxx_throws()` with no types is treated
    // as the bare `cxx_throws` form.
    std::fs::write(
        &hdr,
        r#"#pragma once
[[clang::annotate("rustcc::cxx_throws()")]]
int do_thing();
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (_classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let anns = extras.annotations.effective("do_thing");
    assert!(
        anns.iter().any(|a| matches!(a, Annotation::CxxThrows)),
        "empty `cxx_throws()` should collapse to bare `CxxThrows`; got {anns:?}"
    );
    assert!(
        !anns.iter().any(|a| matches!(a, Annotation::CxxThrowsTyped(_))),
        "empty `cxx_throws()` should NOT produce a Typed variant; got {anns:?}"
    );
}

#[test]
fn sidecar_throws_types_overrides_plain_throws_field() {
    let dir = tmpdir("sidecar");
    let yaml = dir.join("sidecar.yaml");
    std::fs::write(
        &yaml,
        r#"schema: 1
free_functions:
  compute:
    throws: true
    throws_types: [DomainError, RangeError]
"#,
    )
    .unwrap();
    let sidecar = load_sidecar(&yaml).expect("load_sidecar");
    let mut set = AnnotationSet::default();
    set.sidecar = Some(sidecar);

    let anns = set.effective("compute");
    let typed = anns
        .iter()
        .find_map(|a| match a {
            Annotation::CxxThrowsTyped(v) => Some(v.clone()),
            _ => None,
        })
        .expect("CxxThrowsTyped variant");
    assert_eq!(
        typed,
        vec!["DomainError".to_string(), "RangeError".to_string()]
    );
    // The plain CxxThrows should NOT also appear — the typed
    // variant subsumes the boolean field.
    assert!(
        !anns.iter().any(|a| matches!(a, Annotation::CxxThrows)),
        "throws_types should suppress the plain CxxThrows; got {anns:?}"
    );
}

#[test]
fn collect_throws_catches_surfaces_typed_lists_to_shim_consumers() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("collect");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class ErrA {};
class ErrB {};

int plain_fn(int x);

[[clang::annotate("rustcc::cxx_throws")]]
int untyped_throws(int x);

[[clang::annotate("rustcc::cxx_throws(ErrA, ErrB)")]]
int typed_throws(int x);
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (_classes, extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    let typed_map = collect_throws_catches(&extras.annotations, &extras.free_fns);
    eprintln!("=== typed map ===\n{typed_map:?}");

    // Only `typed_throws` should appear in the typed-catches
    // surface — `plain_fn` is non-throws, `untyped_throws` is
    // catch-all (handled by `render_throws_shim_cpp`).
    assert_eq!(typed_map.len(), 1);
    assert_eq!(
        typed_map.get("typed_throws").map(|v| v.as_slice()),
        Some(["ErrA".to_string(), "ErrB".to_string()].as_slice()),
    );
}
