//! v1.10 stretch item 1: M22 multi-inheritance secondary-thunk
//! runtime test.
//!
//! End-to-end exercise of M22's runtime story: a C++ header defines
//! `class C : public A, public B` with overrides on both bases'
//! virtuals. cxx_importer brings it in, generates Rust bindings with
//! `as_a()` / `as_b()` accessors per M22, then we link the C++ side
//! against a Rust binary that dispatches via the upcast pointers and
//! verifies the right vtable slot fires.
//!
//! Static evidence (vtable corpus tests) already shows the secondary
//! subtable structure is correct. This test asserts that the runtime
//! actually picks the right virtual when `&B` points into a `C`
//! subobject — exercising both the secondary subtable entry and the
//! this-adjusting thunk that backs it.
//!
//! See the v1.07.0 README "Remaining stretch items" entry on
//! runtime-dispatch CI validation for the gap this closes.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use cxx_importer::import_header;
use cxx_importer::rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_m22_mi_runtime_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rustc_supports_extern_cpp() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let dir = tmpdir("abi_probe");
    let src = dir.join("probe.rs");
    let out = dir.join("probe.rlib");
    if std::fs::write(&src, b"pub unsafe extern \"C++\" fn _x() {}\n").is_err() {
        return false;
    }
    Command::new(&rustc)
        .args(["--edition=2021", "--crate-type", "lib"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
fn mi_secondary_subtable_dispatches_correctly_at_runtime() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    if !rustc_supports_extern_cpp() {
        eprintln!("skip: rustc doesn't support extern \"C++\" (need fork)");
        return;
    }

    let dir = tmpdir("mi");
    let header = dir.join("mi.hpp");
    let cpp = dir.join("mi.cpp");
    let bindings_rs = dir.join("bindings.rs");
    let main_rs = dir.join("main.rs");
    let cpp_obj = dir.join("mi.o");
    let bin = dir.join("runner");

    // C++ side: A and B each declare a virtual; C inherits both and
    // overrides both. Each override returns a value derived from
    // `c_tag` so we can tell which slot fired.
    std::fs::write(
        &header,
        r#"#pragma once
class A {
public:
    A(unsigned int t);
    virtual ~A();
    virtual unsigned int a_id() const;
private:
    unsigned int a_tag_;
};

class B {
public:
    B(unsigned int t);
    virtual ~B();
    virtual unsigned int b_id() const;
private:
    unsigned int b_tag_;
};

class C : public A, public B {
public:
    C(unsigned int a_tag, unsigned int b_tag, unsigned int c_tag);
    ~C() override;
    unsigned int a_id() const override;
    unsigned int b_id() const override;
private:
    unsigned int c_tag_;
};
"#,
    )
    .unwrap();
    std::fs::write(
        &cpp,
        r#"#include "mi.hpp"
A::A(unsigned int t) : a_tag_(t) {}
A::~A() {}
unsigned int A::a_id() const { return a_tag_ * 10; }

B::B(unsigned int t) : b_tag_(t) {}
B::~B() {}
unsigned int B::b_id() const { return b_tag_ * 100; }

C::C(unsigned int a_tag, unsigned int b_tag, unsigned int c_tag)
    : A(a_tag), B(b_tag), c_tag_(c_tag) {}
C::~C() {}
unsigned int C::a_id() const { return c_tag_ + 1000; }
unsigned int C::b_id() const { return c_tag_ + 2000; }
"#,
    )
    .unwrap();

    // Step 1: libclang import. cxx_importer brings in A, B, C with
    // full MI structure — C's ClassDef carries two BaseSpec entries.
    let mut ctx = CxxTypeCtx::new(host_target());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    assert!(class_ids.len() >= 3, "expected at least 3 classes (A, B, C); got {}", class_ids.len());

    // Step 2: emit Rust bindings. M22 cross-base accessors expose
    // `as_a()` and `as_b()` on `C`, plus inherited virtuals are
    // reachable through them.
    let bindings_src = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig {
            backend: BindingsBackend::DirectExternCpp,
            ..RustBindingsConfig::default()
        },
    )
    .expect("emit_rust_bindings");
    std::fs::write(&bindings_rs, &bindings_src).unwrap();

    // Sanity: the M22 cross-base accessor emission fired. We don't
    // pin a specific name format here (the emitter chooses
    // `as_<base_lowercase>`); a substring match is sufficient.
    assert!(
        bindings_src.contains("fn as_a(") || bindings_src.contains("as_A("),
        "expected `as_a` accessor on C — got:\n{}", &bindings_src[..bindings_src.len().min(8000)]
    );
    assert!(
        bindings_src.contains("fn as_b(") || bindings_src.contains("as_B("),
        "expected `as_b` accessor on C — got:\n{}", &bindings_src[..bindings_src.len().min(8000)]
    );

    // Step 3: compile the C++ side.
    let cpp_compile = Command::new("clang++")
        // -stdlib=libc++ so the C++ object's symbols match the `-lc++`
        // link below (no-op on macOS; required on Linux, where clang++
        // defaults to libstdc++).
        .args(["-c", "-std=c++17", "-stdlib=libc++", "-fPIC"])
        .arg("-o")
        .arg(&cpp_obj)
        .arg(&cpp)
        .output()
        .expect("spawn clang++");
    assert!(
        cpp_compile.status.success(),
        "clang++ failed: {}",
        String::from_utf8_lossy(&cpp_compile.stderr)
    );

    // Step 4: write the Rust runner. It uses the generated bindings
    // through `include!`. Dispatch path under test:
    // - `c.a_id()` → C::a_id via primary subtable (slot 0)
    // - `c.b_id()` → C::b_id via SECONDARY subtable + this-adjust
    //   thunk (the B-vptr in the C subobject points at the secondary
    //   subtable, whose slot for b_id is a `_ZThn<offset>_C::b_id`
    //   thunk that decrements `this` back to C's origin)
    // - Both upcast accessors `c.as_a()` and `c.as_b()` return
    //   references typed as A/B respectively; calling `.a_id()` /
    //   `.b_id()` through them exercises virtual dispatch through
    //   the upcast pointer — which on the B side is the case the
    //   M22 README bullet specifically called out as missing
    //   runtime validation.
    std::fs::write(
        &main_rs,
        r#"// Stub the `::cxx` crate via `extern crate self as cxx` so the
// bindings' `impl ::cxx::CxxBase<…>` lines resolve into a local
// module. M22's cross-base accessors reference this trait; the
// test doesn't need a real cxx runtime — just a typecheck-passing
// trait definition.
extern crate self as cxx;

pub trait CxxBase<B> {
    fn upcast(&self) -> &B;
    fn upcast_mut(&mut self) -> &mut B;
}

include!("bindings.rs");

fn main() {
    let c = C::new(1, 2, 5);
    let via_c_a = c.a_id();
    let via_c_b = c.b_id();
    let via_upcast_a = c.as_a().a_id();
    let via_upcast_b = c.as_b().b_id();

    // Expected:
    //  via_c_a       = C::a_id  = 5 + 1000 = 1005
    //  via_c_b       = C::b_id  = 5 + 2000 = 2005
    //  via_upcast_a  = C::a_id  = 1005   (A* points at primary subobject)
    //  via_upcast_b  = C::b_id  = 2005   (B* points at SECONDARY subobject; vtable slot is a this-adjusting thunk)
    if via_c_a != 1005 || via_c_b != 2005 || via_upcast_a != 1005 || via_upcast_b != 2005 {
        eprintln!(
            "FAIL via_c_a={via_c_a} (expect 1005)\n  \
              via_c_b={via_c_b} (expect 2005)\n  \
              via_upcast_a={via_upcast_a} (expect 1005)\n  \
              via_upcast_b={via_upcast_b} (expect 2005)"
        );
        std::process::exit(1);
    }
    println!("ok: MI runtime dispatch passes");
}
"#,
    )
    .unwrap();

    // Step 5: compile + link the Rust binary with the C++ object.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rust_compile = Command::new(&rustc)
        .args(["--edition=2021", "--crate-type", "bin"])
        .arg(&main_rs)
        .arg("-o")
        .arg(&bin)
        .arg("-C")
        .arg(format!("link-arg={}", cpp_obj.display()))
        .arg("-lc++")
        .output()
        .expect("spawn rustc");
    if !rust_compile.status.success() {
        // Skip if the host rustc can't link to libc++ — this is a
        // known issue on Linux runners that default to libstdc++.
        // The test's value is on macOS (libc++ default); we soft-fail
        // on Linux until the runner-side libc++ install lands.
        let stderr = String::from_utf8_lossy(&rust_compile.stderr);
        if stderr.contains("library 'c++' not found")
            || stderr.contains("cannot find -lc++")
        {
            eprintln!("skip: libc++ not available on this host");
            return;
        }
        panic!("rustc link failed:\n{}", stderr);
    }

    // Step 6: run it. Exit code 0 = pass; non-zero = the dispatch
    // returned wrong values.
    let run = Command::new(&bin).output().expect("spawn runner");
    assert!(
        run.status.success(),
        "runner failed (exit={:?}):\n  stdout: {}\n  stderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
