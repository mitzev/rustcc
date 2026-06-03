//! P09.x (1.13.7): the importer emits `#[rustc_cxx_imported_vtable]` on
//! a polymorphic C++ class so a Rust `class D : CppBase` can subclass it
//! with cross-boundary virtual dispatch (and, with a virtual destructor,
//! cross-boundary `delete`). This test checks the *generated Rust
//! source* shape; it needs libclang but not the rustcc fork rustc.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::import_header;
use cxx_importer::rust_bindings::{generate_rust_bindings, BindingsBackend, RustBindingsConfig};
use rustc_abi_cxx::{CxxTypeCtx, Target};

/// `Clang::new()` errors out on a second concurrent instance.
static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_imported_vtable_{tag}_{}",
        std::process::id()
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

fn emit(header_src: &str, tag: &str) -> String {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir(tag);
    let header = dir.join("h.hpp");
    std::fs::write(&header, header_src).unwrap();
    let mut ctx = CxxTypeCtx::new(host_target());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx).expect("import");
    generate_rust_bindings(
        &ctx,
        &ids,
        &RustBindingsConfig { backend: BindingsBackend::DirectExternCpp, ..Default::default() },
    )
    .expect("emit")
}

/// Concrete + pure virtual, non-virtual destructor: the attribute lists
/// both method slots in C++ vtable order; no `vdtor` record.
#[test]
fn polymorphic_class_emits_imported_vtable_attribute() {
    // Plain `int` keeps the header parseable without a C++ sysroot.
    let src = "struct CppBase {\n  int x;\n  explicit CppBase(int x_);\n  virtual int foo();\n  virtual int describe() = 0;\n};\n";
    let out = emit(src, "concrete");

    assert!(
        out.contains("#[rustc_cxx_imported_vtable = \""),
        "expected imported-vtable attribute, got:\n{out}"
    );
    assert!(out.contains("slot=foo,_ZN7CppBase3fooEv"), "missing concrete foo slot:\n{out}");
    assert!(
        out.contains("slot=describe,__cxa_pure_virtual"),
        "pure virtual should slot __cxa_pure_virtual:\n{out}"
    );
    assert!(!out.contains("vdtor=1"), "non-virtual dtor must NOT mark vdtor:\n{out}");
    assert!(out.contains("zti=_ZTI7CppBase"), "missing base typeinfo symbol:\n{out}");
}

/// Virtual destructor: the attribute carries `vdtor=1`, and the Rust
/// `Drop` targets the base-object destructor `D2` (subobject-safe), not
/// the complete-object `D1`.
#[test]
fn virtual_destructor_marks_vdtor_and_uses_base_object_dtor() {
    let src = "struct CppBase {\n  int x;\n  explicit CppBase(int x_);\n  virtual ~CppBase();\n  virtual int foo();\n  virtual int describe() = 0;\n};\n";
    let out = emit(src, "vdtor");

    assert!(out.contains("vdtor=1"), "virtual dtor must mark vdtor=1:\n{out}");
    assert!(out.contains("slot=foo,_ZN7CppBase3fooEv"), "missing foo slot:\n{out}");
    // The dtor slots are encoded by `vdtor=1`, not listed individually.
    assert!(
        !out.contains("slot=drop,") && !out.contains("slot=~"),
        "dtor must not be emitted as a named slot:\n{out}"
    );
    // Base-object destructor D2 (not complete-object D1) for the subobject.
    assert!(
        out.contains("_ZN7CppBaseD2Ev"),
        "polymorphic base Drop must call the base-object dtor D2:\n{out}"
    );
    assert!(
        !out.contains("_ZN7CppBaseD1Ev"),
        "polymorphic base Drop must NOT call the complete-object dtor D1:\n{out}"
    );
}
