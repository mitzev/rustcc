//! v1.10 stretch item 2: method-flattening multi-level walk.
//!
//! Validates that `flatten_inherited_methods` now reaches
//! grandparent methods (and deeper). v1.07.0 only walked one
//! level — methods declared on `Fl_Widget` weren't surfaced on
//! `Fl_Window` directly, requiring `window.as_fl_group().handle()`.
//!
//! Now the recursive walker (in `render_flattened_inherited_methods`)
//! emits a `window.handle()` forwarder that internally chains
//! `self.as_fl_group().as_fl_widget().handle()`.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::import_header;
use cxx_importer::rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_m22_multi_flatten_{tag}_{}_{}",
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
fn multi_level_flatten_emits_grandparent_method() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir("threelev");
    let header = dir.join("threelev.hpp");

    // Three-level chain: A → B → C. A declares `a_method`,
    // B declares `b_method`, C declares `c_method`. With multi-
    // level flattening enabled on C, all three methods should
    // appear directly on the Rust binding for C — including
    // `a_method` which lives on the grandparent A.
    std::fs::write(
        &header,
        r#"#pragma once
class A {
public:
    A();
    int a_method() const;
};
class B : public A {
public:
    B();
    int b_method() const;
};
class C : public B {
public:
    C();
    int c_method() const;
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(host_target());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Enable the flatten flag.
    let bindings = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig {
            backend: BindingsBackend::DirectExternCpp,
            flatten_inherited_methods: true,
            ..RustBindingsConfig::default()
        },
    )
    .expect("emit bindings");

    // Single-level baseline expectation: c_method (own), b_method
    // (direct base flatten). v1.10 stretch 2 adds: a_method
    // (grandparent flatten via recursive walker).
    let lower = bindings.to_lowercase();

    // C should have all three methods declared.
    assert!(
        bindings.contains("pub fn c_method"),
        "expected c_method on C — got:\n{}",
        &bindings[..bindings.len().min(8000)]
    );
    assert!(
        bindings.contains("pub fn b_method"),
        "expected b_method flattened onto C — single-level walk should produce this"
    );
    assert!(
        bindings.contains("pub fn a_method"),
        "expected a_method flattened onto C from grandparent A — \
         this is the v1.10 stretch 2 case (multi-level walk). \
         If missing, the recursive walker isn't reaching the \
         grandparent. Bindings (lowercased preview):\n{}",
        &lower[..lower.len().min(4000)]
    );

    // The grandparent's flatten must chain through the full
    // accessor path. Sanity-check the emitted body references
    // both `as_b` (direct base) AND `as_a` (grandparent base).
    // The exact format varies (`.as_b().as_a().a_method()` vs
    // multiple lines), so just check for both accessor calls.
    assert!(
        bindings.contains("as_b()") && bindings.contains("as_a()"),
        "expected accessor chain `as_b().as_a()` in flattened \
         method body — found `as_b`: {}, `as_a`: {}",
        bindings.contains("as_b()"),
        bindings.contains("as_a()"),
    );
}
