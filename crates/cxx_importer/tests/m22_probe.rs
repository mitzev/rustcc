//! M22 exploration probe — investigate what works and what
//! doesn't for multi-inheritance + secondary-vtable cases.
//!
//! These tests pin the *current* behavior: anything that
//! passes here is a gap M22 needs to close. They're meant as
//! reference / scratch-pad rather than asserting correctness;
//! the actual M22 implementation will replace them with
//! correctness tests.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{import_header, rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
}};
use rustc_abi_cxx::{CxxTypeCtx, Target, Virtuality};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn temp(s: &str, tag: &str) -> PathBuf {
    let p = std::env::temp_dir()
        .join(format!("rustcc_m22_{tag}_{}.hpp", std::process::id()));
    std::fs::write(&p, s).unwrap();
    p
}

fn cleanup(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
}

#[test]
fn probe_multi_inheritance_layout_vtable_and_method_indices() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // Two polymorphic bases, derived class overrides one
    // method from each. The classic Itanium "diamond"
    // without virtual inheritance: C inherits non-virtually
    // from both A and B; A and B don't share a base.
    let header = temp(
        r#"
struct A {
    virtual int a_method() { return 1; }
    virtual ~A() {}
};

struct B {
    virtual int b_method() { return 2; }
    virtual ~B() {}
};

struct C : public A, public B {
    int a_method() override { return 10; }
    int b_method() override { return 20; }
    virtual int c_method() { return 30; }
};
"#,
        "multi_inh",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Find C.
    let c_id = *class_ids
        .iter()
        .find(|&&id| {
            ctx.class(id)
                .name
                .0
                .last()
                .map(|s| match s {
                    rustc_abi_cxx::NameSegment::Class(i) => i.0 == "C",
                    _ => false,
                })
                .unwrap_or(false)
        })
        .expect("C imported");

    // Layout: A's subobject is at offset 0 (primary, since A
    // is the first polymorphic base). B's subobject is at
    // offset 8 (sizeof A's vptr). C's own vptr shares with A's
    // primary subobject.
    let layout = ctx.layout(c_id).expect("layout");
    eprintln!("[probe] C layout: size={}, align={}, base_offsets={:?}",
        layout.size_bytes, layout.align_bytes, layout.base_offsets);
    assert!(layout.size_bytes >= 16, "C should have at least 2 vptrs");

    // Vtable: should have at least 2 sub-tables (primary for A,
    // secondary for B).
    let vt = ctx.vtable(c_id).expect("C should have a vtable");
    eprintln!("[probe] C vtable sub_tables count: {}", vt.sub_tables.len());
    for (i, st) in vt.sub_tables.iter().enumerate() {
        eprintln!(
            "[probe]   sub_table[{i}]: for_subobject_offset={}, address_point={}, entries={}",
            st.subobject_offset,
            st.address_point_offset,
            st.entries.len(),
        );
        for (j, e) in st.entries.iter().enumerate() {
            eprintln!("[probe]     entry[{j}]: {e:?}");
        }
    }
    assert!(vt.sub_tables.len() >= 2, "C should have primary + secondary sub-tables");

    // The big M22 gap: walk C's methods and check vtable_index.
    // Today, only methods routed through the PRIMARY sub-table
    // get an index. Methods in the secondary (e.g. C's override
    // of B::b_method) end up with vtable_index = None and skip
    // emission.
    let class = ctx.class(c_id);
    eprintln!("[probe] C methods + vtable_index:");
    let mut secondary_orphans = 0;
    for (i, m) in class.methods.iter().enumerate() {
        let is_virtual = matches!(m.virtuality, Virtuality::Virtual | Virtuality::PureVirtual);
        eprintln!(
            "[probe]   method[{i}]: name={:?} virtual={is_virtual} vtable_index={:?}",
            m.name.ident_name().unwrap_or("<op/conv>"),
            m.vtable_index,
        );
        if is_virtual && m.vtable_index.is_none() {
            secondary_orphans += 1;
        }
    }
    eprintln!("[probe] C has {secondary_orphans} virtual methods missing vtable_index");
    // After the M22 third-pass fix, every virtual method on a
    // multi-inheritance class must have a vtable_index. Without
    // this assertion, regressions could re-introduce the
    // primary-only walker silently.
    assert_eq!(
        secondary_orphans, 0,
        "C should have all virtual methods indexed (M22 multi-inh)",
    );

    // Bindings emission: must not skip any virtual method.
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");
    let skipped = src.matches("skipped: virtual method without populated vtable_index").count();
    eprintln!("[probe] bindings emit skipped {skipped} virtual methods on multi-inh classes");
    assert_eq!(skipped, 0, "Bindings must not skip multi-inh virtual methods");

    cleanup(&header);
}

#[test]
fn probe_fltk_fl_image_secondary_vtable_methods() {
    // Skip if we don't have FLTK installed (CI without FLTK).
    let fltk_inc = std::path::Path::new("/opt/homebrew/include/FL/Fl_Image.H");
    if !fltk_inc.exists() {
        eprintln!("[probe] skipping: FLTK not installed at /opt/homebrew");
        return;
    }
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        fltk_inc,
        &["-x", "c++", "-std=c++17", "-I/opt/homebrew/include"],
        &mut ctx,
    )
    .expect("import");
    let img_id = *class_ids
        .iter()
        .find(|&&id| {
            ctx.class(id).name.0.last().map(|s| match s {
                rustc_abi_cxx::NameSegment::Class(i) => i.0 == "Fl_Image",
                _ => false,
            }).unwrap_or(false)
        })
        .expect("Fl_Image imported");

    let class = ctx.class(img_id);
    let layout = ctx.layout(img_id).expect("layout");
    let vt = ctx.vtable(img_id);
    eprintln!(
        "[probe] Fl_Image: bases={}, polymorphic={}, vtable_subtables={}, methods={}",
        class.bases.len(),
        class.is_polymorphic,
        vt.as_ref().map(|v| v.sub_tables.len()).unwrap_or(0),
        class.methods.len(),
    );
    eprintln!("[probe] Fl_Image base_offsets: {:?}", layout.base_offsets);

    // Count virtual methods that did vs. didn't get vtable_index.
    let mut with_idx = 0;
    let mut without_idx = 0;
    let mut samples_without: Vec<String> = Vec::new();
    for m in &class.methods {
        let is_v = matches!(m.virtuality, Virtuality::Virtual | Virtuality::PureVirtual);
        if !is_v {
            continue;
        }
        if m.vtable_index.is_some() {
            with_idx += 1;
        } else {
            without_idx += 1;
            if samples_without.len() < 5 {
                samples_without.push(
                    m.name.ident_name().unwrap_or("<op/conv>").to_string(),
                );
            }
        }
    }
    eprintln!(
        "[probe] Fl_Image virtual methods: {with_idx} with vtable_index, {without_idx} WITHOUT",
    );
    eprintln!("[probe] sample methods missing vtable_index: {samples_without:?}");

    // Show how many FP entries the primary actually has so we
    // can compare against the method count.
    if let Some(vt) = &vt {
        for (i, st) in vt.sub_tables.iter().enumerate() {
            let fps = st.entries.iter().filter(|e| matches!(
                e, rustc_abi_cxx::VTableEntry::FunctionPointer { .. }
            )).count();
            eprintln!("[probe]   sub[{i}] for_offset={} entries={} fps={}",
                st.subobject_offset, st.entries.len(), fps);
        }
    }
}

#[test]
fn probe_fltk_umbrella_fl_image_state() {
    // When parsed via the FLTK umbrella header (the same one
    // examples/fltk_hello uses), does Fl_Image still get
    // vtable_index for all virtual methods, or do some go
    // missing? The standalone-Fl_Image probe shows all 11
    // get indexed; the umbrella demo shows them ALL missing.
    let umbrella = std::path::Path::new(
        "/Users/ogi/rustcc/examples/fltk_hello/cpp/fltk_umbrella.hpp",
    );
    if !umbrella.exists() {
        eprintln!("[probe] skipping: umbrella not found");
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
    let img_id = match class_ids.iter().find(|&&id| {
        ctx.class(id).name.0.last().map(|s| match s {
            rustc_abi_cxx::NameSegment::Class(i) => i.0 == "Fl_Image",
            _ => false,
        }).unwrap_or(false)
    }) {
        Some(&i) => i,
        None => {
            eprintln!("[probe] Fl_Image not in returned class_ids; checking ctx");
            ctx.class_ids().find(|&id| {
                ctx.class(id).name.0.last().map(|s| match s {
                    rustc_abi_cxx::NameSegment::Class(i) => i.0 == "Fl_Image",
                    _ => false,
                }).unwrap_or(false)
            }).expect("Fl_Image in ctx")
        }
    };

    let class = ctx.class(img_id);
    eprintln!(
        "[probe] umbrella Fl_Image: poisoned={}, methods={}, polymorphic={}",
        ctx.is_poisoned(img_id),
        class.methods.len(),
        class.is_polymorphic,
    );
    let mut with_idx = 0;
    let mut without_idx = 0;
    let mut samples: Vec<String> = Vec::new();
    for m in &class.methods {
        let is_v = matches!(m.virtuality, Virtuality::Virtual | Virtuality::PureVirtual);
        if !is_v { continue; }
        if m.vtable_index.is_some() { with_idx += 1; }
        else {
            without_idx += 1;
            if samples.len() < 5 {
                samples.push(m.name.ident_name().unwrap_or("?").to_string());
            }
        }
    }
    eprintln!("[probe] umbrella Fl_Image virtual methods: {with_idx} with vtable_index, {without_idx} WITHOUT");
    eprintln!("[probe] sample methods missing: {samples:?}");
    // After the M22 cache-pollution fix + third-pass fix, every
    // virtual method on Fl_Image must have a vtable_index even
    // when imported via the umbrella header.
    assert_eq!(
        without_idx, 0,
        "Fl_Image virtual methods missing vtable_index (umbrella context) \u{2014} samples: {samples:?}",
    );
    assert!(class.is_polymorphic, "Fl_Image must be polymorphic in umbrella context");

    // Detailed dump.
    eprintln!("[probe] Fl_Image method virtualities:");
    let mut nv = 0;
    let mut v = 0;
    let mut pv = 0;
    for m in &class.methods {
        match m.virtuality {
            Virtuality::NonVirtual => nv += 1,
            Virtuality::Virtual => v += 1,
            Virtuality::PureVirtual => pv += 1,
        }
    }
    eprintln!("[probe]   NonVirtual: {nv}, Virtual: {v}, PureVirtual: {pv}");
    // Check vtable presence.
    let vt = ctx.vtable(img_id);
    eprintln!("[probe]   ctx.vtable(): is_some={}", vt.is_some());
    if let Some(vt) = vt {
        eprintln!("[probe]   subtables: {}, primary_fp_count: {}",
            vt.sub_tables.len(),
            vt.sub_tables[0].entries.iter().filter(|e| matches!(e, rustc_abi_cxx::VTableEntry::FunctionPointer { .. })).count(),
        );
    }
}

/// Regression test for the M22 third-pass fix.
///
/// Before the fix: deriving classes (Fl_Group, Fl_Window,
/// Fl_RGB_Image) imported via the FLTK umbrella header would end up
/// with `dtor.vtable_index = None`, even though they're
/// single-inheritance and declare their own virtual destructors.
/// Root cause: `populate_vtable_indices` ran at the end of each
/// `import_class` body walk — but a base class's body walk could
/// recursively trigger `import_class` on a derived class via a
/// method-parameter type lookup (e.g. Fl_Widget's `Fl_Group*
/// parent()` triggers `import_class(Fl_Group)`). The derived class
/// then computed its vtable against the base class in placeholder
/// state (methods=0, polymorphic=false) → no inherited dtor slots
/// → no vtable_index for the dtor.
///
/// The fix moves polymorphism convergence + populate_vtable_indices
/// into a third pass that runs after every class body has been
/// walked. This test pins the FLTK umbrella case so the regression
/// cannot return.
#[test]
fn regression_dtor_vtable_index_in_umbrella_context() {
    let umbrella = std::path::Path::new(
        "/Users/ogi/rustcc/examples/fltk_hello/cpp/fltk_umbrella.hpp",
    );
    if !umbrella.exists() {
        eprintln!("[probe] skipping: umbrella not found");
        return;
    }
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let _class_ids = import_header(
        umbrella,
        &["-x", "c++", "-std=c++17", "-I/opt/homebrew/include"],
        &mut ctx,
    )
    .expect("import");

    // Each of these single-inheritance FLTK classes declares its
    // own virtual destructor. Post-fix, every dtor surfacing in
    // `class.methods` must have a populated `vtable_index`.
    let mut failures: Vec<String> = Vec::new();
    for target in &["Fl_Widget", "Fl_Group", "Fl_Window", "Fl_Image", "Fl_RGB_Image"] {
        let id = match ctx.class_ids().find(|&id| {
            ctx.class(id).name.0.last().map(|s| match s {
                rustc_abi_cxx::NameSegment::Class(i) => i.0 == *target,
                _ => false,
            }).unwrap_or(false)
        }) {
            Some(i) => i,
            None => {
                failures.push(format!("{target}: not imported"));
                continue;
            }
        };
        let class = ctx.class(id);
        if !class.is_polymorphic {
            failures.push(format!("{target}: is_polymorphic = false"));
        }
        let dtors: Vec<&rustc_abi_cxx::MethodDef> = class
            .methods
            .iter()
            .filter(|m| matches!(m.special, Some(rustc_abi_cxx::SpecialMember::Dtor)))
            .collect();
        if dtors.is_empty() {
            failures.push(format!("{target}: no dtor in methods"));
            continue;
        }
        for d in dtors {
            if d.virtuality == rustc_abi_cxx::Virtuality::NonVirtual {
                failures.push(format!("{target}: dtor reports NonVirtual"));
            }
            if d.vtable_index.is_none() {
                failures.push(format!(
                    "{target}: dtor.vtable_index = None (the bug this test pins)"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Dtor vtable-index regression — failures:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn probe_virtual_base_layout_and_vtable() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // Diamond with virtual inheritance: B and C both inherit
    // virtually from A. D inherits from both. There's only
    // one A subobject in D; B and C share it via the virtual
    // inheritance.
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
        "virt_diamond",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let d_id = *class_ids
        .iter()
        .find(|&&id| {
            ctx.class(id).name.0.last().map(|s| match s {
                rustc_abi_cxx::NameSegment::Class(i) => i.0 == "D",
                _ => false,
            }).unwrap_or(false)
        })
        .expect("D imported");

    let layout = ctx.layout(d_id).expect("layout");
    eprintln!(
        "[probe] D layout: size={}, virtual_base_offsets={:?}, base_offsets={:?}",
        layout.size_bytes, layout.virtual_base_offsets, layout.base_offsets,
    );

    let vt = ctx.vtable(d_id).expect("D vtable");
    eprintln!("[probe] D vtable sub_tables: {}", vt.sub_tables.len());
    for (i, st) in vt.sub_tables.iter().enumerate() {
        eprintln!(
            "[probe]   sub[{i}]: for_offset={} entries={}",
            st.subobject_offset,
            st.entries.len(),
        );
    }

    cleanup(&header);
}
