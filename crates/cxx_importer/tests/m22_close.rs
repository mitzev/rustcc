//! M22 close-out battery — covers the Qt/LLVM/Chromium-style cases
//! that were out of scope for the FLTK-driven m22_probe tests.
//!
//! Each test asserts both *structural* properties (vtable shape,
//! `vtable_index` populated) and *bindings emission* properties
//! (no skips). When a test fails, the failure mode tells you what
//! M22 sub-feature is missing.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{
    import_header,
    rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    },
};
use rustc_abi_cxx::{CxxTypeCtx, NameSegment, Target, VTableEntry, Virtuality};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn temp(s: &str, tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rustcc_m22_close_{tag}_{}.hpp",
        std::process::id()
    ));
    std::fs::write(&p, s).unwrap();
    p
}

fn cleanup(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
}

fn class_id_by_name(
    ctx: &CxxTypeCtx,
    class_ids: &[rustc_abi_cxx::ClassId],
    name: &str,
) -> rustc_abi_cxx::ClassId {
    *class_ids
        .iter()
        .find(|&&id| {
            ctx.class(id)
                .name
                .0
                .last()
                .map(|s| match s {
                    NameSegment::Class(i) => i.0 == name,
                    _ => false,
                })
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("{name} not imported"))
}

fn count_skips(src: &str) -> usize {
    src.matches("skipped: virtual method without populated vtable_index")
        .count()
}

/// Multi-inheritance with a *pure-virtual* base on one side and a
/// concrete polymorphic base on the other.
///
/// The pure-virtual slot resolves to `__cxa_pure_virtual` in the
/// originating class's vtable; the most-derived class implements it
/// and replaces the slot in its own primary table.
#[test]
fn pure_virtual_in_multi_inh() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct Iface {
    virtual int do_thing() = 0;
    virtual ~Iface() = default;
};
struct Concrete {
    virtual int weight() { return 7; }
    virtual ~Concrete() {}
};
struct Impl : public Iface, public Concrete {
    int do_thing() override { return 42; }
    int weight() override { return 99; }
};
"#,
        "pure_virt_mi",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let impl_id = class_id_by_name(&ctx, &class_ids, "Impl");
    let class = ctx.class(impl_id);
    assert!(class.is_polymorphic);

    let vt = ctx.vtable(impl_id).expect("Impl vtable");
    assert!(
        vt.sub_tables.len() >= 2,
        "Impl needs primary + secondary sub-tables (pure-virt + concrete bases)",
    );

    // Both overrides on Impl must be indexed.
    let mut do_thing_idx = None;
    let mut weight_idx = None;
    for m in &class.methods {
        let nm = m.name.ident_name().unwrap_or_default();
        if nm == "do_thing" {
            do_thing_idx = m.vtable_index;
        }
        if nm == "weight" {
            weight_idx = m.vtable_index;
        }
    }
    assert!(do_thing_idx.is_some(), "Impl::do_thing override must be indexed");
    assert!(weight_idx.is_some(), "Impl::weight override must be indexed");

    // No `__cxa_pure_virtual` should appear in Impl's PRIMARY
    // sub-table after override resolution: do_thing is overridden.
    let primary = &vt.sub_tables[0];
    for e in &primary.entries {
        if let VTableEntry::FunctionPointer { mangled_target, .. } = e {
            assert!(
                !mangled_target.contains("__cxa_pure_virtual"),
                "Impl primary slot still references __cxa_pure_virtual: {mangled_target}",
            );
        }
    }

    // Bindings emit zero virtual-method skips.
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");
    assert_eq!(count_skips(&src), 0);

    cleanup(&header);
}

/// Triple inheritance: D inherits from three polymorphic bases A,
/// B, C — each at increasing offsets. D's vtable has 4 sub-tables
/// (primary + one secondary per non-primary polymorphic base).
#[test]
fn triple_polymorphic_inheritance() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct A {
    virtual int a() { return 1; }
    virtual ~A() {}
};
struct B {
    virtual int b() { return 2; }
    virtual ~B() {}
};
struct C {
    virtual int c() { return 3; }
    virtual ~C() {}
};
struct D : public A, public B, public C {
    int a() override { return 10; }
    int b() override { return 20; }
    int c() override { return 30; }
    virtual int d() { return 40; }
};
"#,
        "triple_inh",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let d_id = class_id_by_name(&ctx, &class_ids, "D");
    let class = ctx.class(d_id);
    let vt = ctx.vtable(d_id).expect("D vtable");

    // Triple inh: A is primary (offset 0), B at +8, C at +16.
    // D's vtable should have primary + 2 secondaries.
    assert_eq!(
        vt.sub_tables.len(),
        3,
        "D needs primary + 2 secondaries (one per non-primary base): {:?}",
        vt.sub_tables.iter().map(|st| st.subobject_offset).collect::<Vec<_>>(),
    );

    // Every override on D must have a populated vtable_index.
    let mut missing: Vec<String> = Vec::new();
    for m in &class.methods {
        if matches!(m.virtuality, Virtuality::Virtual | Virtuality::PureVirtual)
            && m.vtable_index.is_none()
        {
            missing.push(m.name.ident_name().unwrap_or("<op>").to_string());
        }
    }
    assert!(missing.is_empty(), "Unindexed virtual methods on D: {missing:?}");

    // Each secondary must include this-adjustment thunks for the
    // class's own override of the corresponding base method.
    let mut found_b_thunk = false;
    let mut found_c_thunk = false;
    for st in vt.sub_tables.iter().skip(1) {
        for e in &st.entries {
            if let VTableEntry::FunctionPointer { mangled_target, .. } = e {
                if mangled_target.starts_with("_ZThn") {
                    if mangled_target.contains("1D1bEv") {
                        found_b_thunk = true;
                    }
                    if mangled_target.contains("1D1cEv") {
                        found_c_thunk = true;
                    }
                }
            }
        }
    }
    assert!(found_b_thunk, "D::b override must have a Thn-thunk in B's secondary");
    assert!(found_c_thunk, "D::c override must have a Thn-thunk in C's secondary");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");
    assert_eq!(count_skips(&src), 0);

    cleanup(&header);
}

/// Compiler-generated dtor across multi-inheritance: D doesn't
/// declare a destructor, but all of its bases have virtual ones.
/// The vtable must still surface D1/D0 dtor slots, and the
/// generated bindings must not skip (`drop`) on D.
///
/// Failure mode if `build_virtual_slots`'s `has_virtual_dtor`
/// only checks the chain root's *own* `class.methods`: no D1/D0
/// slots get pushed because D's compiler-generated dtor isn't
/// in `class.methods`. This is a real gap for any C++ class
/// that follows the common "rule of zero" pattern in the
/// most-derived class.
#[test]
fn compiler_generated_dtor_in_multi_inh() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct A {
    virtual int a() { return 1; }
    virtual ~A() {}
};
struct B {
    virtual int b() { return 2; }
    virtual ~B() {}
};
// D doesn't declare its own destructor. The compiler synthesizes
// `~D() = default` whose vtable entry points at A's destructor
// (chain root) for the D1/D0 slots.
struct D : public A, public B {
    int a() override { return 10; }
    int b() override { return 20; }
};
"#,
        "synth_dtor",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let d_id = class_id_by_name(&ctx, &class_ids, "D");
    let vt = ctx.vtable(d_id).expect("D vtable");

    // Primary sub-table must include D1/D0 dtor slots even though
    // D doesn't declare a destructor.
    let primary = &vt.sub_tables[0];
    let mut d1_seen = false;
    let mut d0_seen = false;
    for e in &primary.entries {
        if let VTableEntry::FunctionPointer { mangled_target, .. } = e {
            if mangled_target.ends_with("D1Ev") {
                d1_seen = true;
            }
            if mangled_target.ends_with("D0Ev") {
                d0_seen = true;
            }
        }
    }
    assert!(d1_seen, "D primary vtable should include D1 dtor slot (rule of zero)");
    assert!(d0_seen, "D primary vtable should include D0 dtor slot (rule of zero)");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");
    assert_eq!(count_skips(&src), 0);

    cleanup(&header);
}

/// Diamond + virtual base + override on the shared method. D
/// overrides `a_method` defined in the shared virtual A. Both
/// B's and C's a_method overrides should resolve to D's in the
/// final vtable, and D's binding must expose a working a_method
/// through its vtable.
#[test]
fn diamond_virtual_base_with_shared_override() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct A {
    virtual int a_method() { return 1; }
    virtual ~A() {}
};
struct B : virtual public A {
    int a_method() override { return 2; }
};
struct C : virtual public A {
    int a_method() override { return 3; }
};
struct D : public B, public C {
    int a_method() override { return 4; }
};
"#,
        "diamond_override",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let d_id = class_id_by_name(&ctx, &class_ids, "D");
    let class = ctx.class(d_id);
    assert!(class.is_polymorphic);

    // D::a_method override must be indexed.
    let mut a_idx = None;
    for m in &class.methods {
        if m.name.ident_name() == Some("a_method") {
            a_idx = m.vtable_index;
        }
    }
    assert!(a_idx.is_some(), "D::a_method must have vtable_index in diamond+vbase");

    // Bindings emit must not skip a_method.
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");
    assert_eq!(count_skips(&src), 0);

    // The C++ shims (cxx_shims.cpp) must reference D::a_method's
    // mangled symbol. If the binding emission silently skipped
    // the override, the shim wouldn't link.
    assert!(
        src.contains("a_method"),
        "Bindings should emit a_method for D",
    );

    cleanup(&header);
}

/// Cross-base method exposure: when C : A, B, calling
/// `c.foo()` (defined only on A) should work through the
/// generated binding. Today this requires the binding to
/// either flatten inherited methods into C's struct OR to
/// expose `c.as_a()` / `c.as_b()` upcast accessors.
///
/// This test pins the *current* behavior so future improvements
/// (M22 follow-up: cross-base ergonomics) can be detected. If
/// neither path is available, the test fails with a clear
/// diagnostic listing the affected method count.
#[test]
fn cross_base_method_exposure() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct A {
    virtual int a_only() { return 1; }
    virtual ~A() {}
};
struct B {
    virtual int b_only() { return 2; }
    virtual ~B() {}
};
struct C : public A, public B {
    virtual int c_new() { return 3; }
};
"#,
        "cross_base",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let c_id = class_id_by_name(&ctx, &class_ids, "C");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    // Locate C's impl block.
    let c_block_start = src.find("impl C {").expect("C impl block must exist");
    let c_block_rest = &src[c_block_start..];
    let c_block_end = c_block_rest
        .find("\n}\n")
        .map(|e| e + 2)
        .unwrap_or(c_block_rest.len());
    let c_block = &c_block_rest[..c_block_end];

    // Either of these has to be true for cross-base methods to
    // be reachable from a C-typed receiver:
    //   (a) flattening: c_block exposes a_only / b_only directly;
    //   (b) upcast: c_block exposes as_a() / as_b() (or similar)
    //       returning a base reference the user can call methods
    //       on.
    let has_as_a = c_block.contains("pub fn as_a");
    let has_as_b = c_block.contains("pub fn as_b");
    let has_as_a_mut = c_block.contains("pub fn as_a_mut");
    let has_as_b_mut = c_block.contains("pub fn as_b_mut");

    assert!(
        has_as_a && has_as_b,
        "M22 cross-base accessors missing: as_a={has_as_a} as_b={has_as_b}",
    );
    assert!(
        has_as_a_mut && has_as_b_mut,
        "M22 cross-base mutable accessors missing: as_a_mut={has_as_a_mut} as_b_mut={has_as_b_mut}",
    );

    // The non-primary base accessor must use a non-zero offset
    // (B is at offset 8 in C). The primary (A) is at offset 0.
    let as_b_block_start = c_block.find("pub fn as_b").expect("as_b emitted");
    let as_b_block_rest = &c_block[as_b_block_start..];
    // Look at the first `add(...)` after `pub fn as_b` — must
    // not be `add(0)`. (The primary's accessor uses no `.add()`.)
    if let Some(add_pos) = as_b_block_rest.find(".add(") {
        let after_add = &as_b_block_rest[add_pos + 5..];
        let arg_end = after_add.find(')').expect("add(...) closes");
        let arg = &after_add[..arg_end];
        assert!(
            arg != "0",
            "as_b must use a non-zero offset (B is at offset 8 in C)",
        );
    }

    // Sanity: the binding should NOT have skipped C's own new
    // virtual or A/B's destructors via the multi-inh-skip path.
    assert_eq!(count_skips(&src), 0);

    let _ = c_id;
    cleanup(&header);
}

/// Abstract classes (any pure-virtual slot in their primary
/// vtable) must have ctor-shim emission skipped. Otherwise the
/// generated `cxx_shims.cpp` would contain
/// `new AbstractClass(...)` which C++ rejects with
/// `error: allocating an object of abstract class type`.
///
/// This surfaces when an umbrella header transitively pulls in
/// abstract intermediates (FLTK's Fl_Menu_, Fl_Input_, etc.).
#[test]
fn abstract_class_ctor_shim_skipped() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
struct Iface {
    virtual int do_thing() = 0;
    virtual ~Iface() = default;
};
// Abstract through inheritance: Mid doesn't override do_thing.
struct Mid : public Iface {
    int helper() const { return 1; }
};
struct Concrete : public Mid {
    Concrete() {}
    int do_thing() override { return 42; }
};
"#,
        "abstract",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // The shim emitter should produce *no* `new Iface(` or
    // `new Mid(` lines. Concrete should still get `new Concrete(`.
    let opts = cxx_importer::shims::ShimOptions {
        headers: &["dummy.hpp"],
        classes: &class_ids,
    };
    let shims_src = cxx_importer::shims::generate_shims(&ctx, &opts).expect("shims");

    assert!(
        !shims_src.contains("new Iface("),
        "Iface (pure-virt root) ctor shim must not be emitted",
    );
    assert!(
        !shims_src.contains("new Mid("),
        "Mid (pure-virt inherited) ctor shim must not be emitted",
    );
    // Concrete IS instantiable, so its shim should still emit.
    assert!(
        shims_src.contains("new Concrete("),
        "Concrete (overrides do_thing) ctor shim must be emitted",
    );

    cleanup(&header);
}

/// Verify the M22 cross-base accessors work for FLTK's umbrella
/// classes. Fl_Window inherits from Fl_Group; Fl_Group inherits
/// from Fl_Widget. After this fix Rust users should be able to:
///   `window.as_fl_group().handle(...)` → calls Fl_Group::handle
/// without needing fully-qualified `<W as CxxBase<Fl_Group>>::upcast`.
#[test]
fn cross_base_accessors_in_fltk_umbrella() {
    let umbrella = std::path::Path::new(
        "/Users/ogi/rustcc/examples/fltk_hello/cpp/fltk_umbrella.hpp",
    );
    if !umbrella.exists() {
        eprintln!("skipping: umbrella not found");
        return;
    }
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        umbrella,
        &["-x", "c++", "-std=c++17", "-I/opt/homebrew/include"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    // Find the impl Fl_Window block.
    let win_start = src.find("impl Fl_Window {").expect("Fl_Window impl");
    let win_block = &src[win_start..];
    let win_end = win_block.find("\n}\n").map(|e| e + 2).unwrap_or(win_block.len());
    let win_block = &win_block[..win_end];

    assert!(
        win_block.contains("pub fn as_fl_group"),
        "Fl_Window must have as_fl_group accessor (M22 cross-base)",
    );
    assert!(
        win_block.contains("pub fn as_fl_group_mut"),
        "Fl_Window must have as_fl_group_mut accessor (M22 cross-base)",
    );

    // Same check on Fl_Group → Fl_Widget.
    let grp_start = src.find("impl Fl_Group {").expect("Fl_Group impl");
    let grp_block = &src[grp_start..];
    let grp_end = grp_block.find("\n}\n").map(|e| e + 2).unwrap_or(grp_block.len());
    let grp_block = &grp_block[..grp_end];
    assert!(
        grp_block.contains("pub fn as_fl_widget"),
        "Fl_Group must have as_fl_widget accessor (M22 cross-base)",
    );
}
