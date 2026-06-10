//! v1.12.4: ctor throws + sidecar `free_functions:` schema.
//!
//! Two pieces under test:
//!   1. A ctor tagged `[[clang::annotate("rustcc::cxx_throws")]]`
//!      gets a `Result<Self, ::cxx::CxxException>`-returning safe
//!      wrapper. The C++ shim placement-constructs into the slot
//!      only if no exception escapes; the wrapper checks the
//!      raw error kind BEFORE `assume_init`, so we never observe
//!      a half-constructed value on the throw path.
//!   2. The sidecar schema's new top-level `free_functions:` map
//!      lets users mark a free fn as throwing (or rename / skip
//!      it) without inline source markup — useful for headers
//!      they can't modify.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::rust_bindings::{
    generate_rust_bindings_full, BindingsBackend, RustBindingsConfig,
};
use cxx_importer::{
    import_header_with_extras, load_sidecar, AnnotationSet,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cxx_throws_v1124_{tag}_{}_{}",
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
fn annotated_ctor_returns_result_self() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("ctor");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
class Resource {
public:
    [[clang::annotate("rustcc::cxx_throws")]]
    Resource(int initial);

    int value() const;
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

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_full(
        &ctx,
        &classes,
        &extras.annotations,
        &extras.aliases,
        &extras.enums,
        &extras.free_fns,
        &extras.static_data,
        &cfg,
    )
    .expect("generate");

    eprintln!("=== generated bindings ===\n{src}\n");

    // Ctor is throws — link name should be the shim, return
    // should be Result<Self, _>.
    assert!(
        src.contains("__rustcc_throws_Resource_new"),
        "expected ctor shim symbol; src:\n{src}"
    );
    assert!(
        src.contains("pub fn new(arg0: i32) -> ::core::result::Result<Self, ::cxx::CxxException>"),
        "expected Result<Self, _>-returning ctor wrapper; src:\n{src}"
    );
    // The wrapper body uses MaybeUninit + Ok(assume_init) only on the
    // success path, Err on the throw path.
    let new_idx = src.find("pub fn new(").expect("ctor wrapper missing");
    let body = &src[new_idx..];
    assert!(
        body.contains("MaybeUninit::<Self>::uninit()"),
        "expected MaybeUninit slot; body excerpt:\n{}",
        &body[..body.len().min(500)]
    );
    assert!(
        body.contains("__raw.kind() == ::cxx::CXX_EXC_OK"),
        "expected kind check before assume_init; body excerpt:\n{}",
        &body[..body.len().min(500)]
    );
    // The non-throws `value()` accessor stays plain.
    assert!(
        src.contains("pub fn value(&self) -> i32"),
        "expected plain value() accessor; src:\n{src}"
    );
}

#[test]
fn sidecar_free_functions_map_marks_throws() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("sidecar");
    let hdr = dir.join("h.hpp");
    let yaml = dir.join("sidecar.yaml");

    std::fs::write(
        &hdr,
        r#"#pragma once
int do_clean();
int compute(int a, int b);
void log_event();
"#,
    )
    .unwrap();
    std::fs::write(
        &yaml,
        r#"schema: 1
free_functions:
  compute:
    throws: true
  log_event:
    throws: true
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let (classes, mut extras) = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import_header_with_extras");

    // Layer the sidecar on top of the (empty) inline annotations.
    let sidecar = load_sidecar(&yaml).expect("load_sidecar");
    extras.annotations.sidecar = Some(sidecar);

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_full(
        &ctx,
        &classes,
        &extras.annotations,
        &extras.aliases,
        &extras.enums,
        &extras.free_fns,
        &extras.static_data,
        &cfg,
    )
    .expect("generate");

    eprintln!("=== generated bindings ===\n{src}\n");

    // The two sidecar-tagged fns should land in the extern "C"
    // throws block with shim link names + Result wrappers.
    assert!(
        src.contains("__rustcc_throws_compute"),
        "expected compute shim symbol from sidecar; src:\n{src}"
    );
    assert!(
        src.contains("__rustcc_throws_log_event"),
        "expected log_event shim symbol from sidecar; src:\n{src}"
    );
    assert!(
        src.contains("pub fn compute(arg0: i32, arg1: i32) -> ::core::result::Result<i32, ::cxx::CxxException>"),
        "expected Result<i32, _> on compute; src:\n{src}"
    );
    assert!(
        src.contains("pub fn log_event() -> ::core::result::Result<(), ::cxx::CxxException>"),
        "expected Result<(), _> on log_event; src:\n{src}"
    );
    // do_clean wasn't tagged — stays plain.
    let clean_idx = src.find("pub fn do_clean").expect("do_clean missing");
    let line_end = src[clean_idx..]
        .find('\n')
        .map(|i| clean_idx + i)
        .unwrap_or(src.len());
    let clean_sig = &src[clean_idx..line_end];
    assert!(
        !clean_sig.contains("Result<"),
        "do_clean shouldn't be Result-wrapped; line:\n{clean_sig}"
    );
}

#[test]
fn sidecar_schema_round_trips_free_functions_block() {
    // Pure unit-level coverage of the new schema parse path.
    let dir = tmpdir("schema");
    let yaml = dir.join("sidecar.yaml");
    std::fs::write(
        &yaml,
        r#"schema: 1
free_functions:
  do_thing:
    throws: true
    rust_name: do_thing_safe
  obsolete:
    skip: true
"#,
    )
    .unwrap();
    let schema = load_sidecar(&yaml).expect("load_sidecar");
    assert_eq!(schema.free_functions.len(), 2);
    assert_eq!(
        schema
            .free_functions
            .get("do_thing")
            .and_then(|e| e.throws),
        Some(true)
    );
    assert_eq!(
        schema
            .free_functions
            .get("do_thing")
            .and_then(|e| e.rust_name.clone()),
        Some("do_thing_safe".into())
    );
    assert_eq!(
        schema.free_functions.get("obsolete").and_then(|e| e.skip),
        Some(true)
    );

    // annotations_for round-trip through AnnotationSet
    use cxx_importer::Annotation;
    let mut set = AnnotationSet::default();
    set.sidecar = Some(schema);
    let do_thing = set.effective("do_thing");
    assert!(
        do_thing.iter().any(|a| matches!(a, Annotation::CxxThrows)),
        "do_thing should carry CxxThrows annotation; got {do_thing:?}"
    );
    assert!(
        do_thing.iter().any(|a| matches!(a, Annotation::Name(n) if n == "do_thing_safe")),
        "do_thing should carry Name override; got {do_thing:?}"
    );
    let obsolete = set.effective("obsolete");
    assert!(
        obsolete.iter().any(|a| matches!(a, Annotation::Skip)),
        "obsolete should carry Skip; got {obsolete:?}"
    );
}
