//! End-to-end libclang import tests.
//!
//! These verify that `cxx_importer::import_header` parses a small C++
//! header, produces `ClassDef`s in `CxxTypeCtx`, and that those
//! definitions feed correctly into `rustc_abi_cxx`'s layout engine.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::import_header;
use cxx_importer::rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
};
use rustc_abi_cxx::{
    CvQual, CxxType, CxxTypeCtx, FnSig, Ident, IntWidth, MethodName,
    NameSegment, OperatorKind, RecordKind, SpecialMember, Symbol, Target,
    TemplateArg, VTableEntry, Virtuality,
};

/// libclang's initialization is process-exclusive (`Clang::new()` errors
/// out on second call while another instance exists). Serialize import
/// tests with a module-wide mutex so cargo's parallel test runner
/// doesn't race them.
static LIBCLANG: Mutex<()> = Mutex::new(());

fn temp_header(contents: &str, tag: &str) -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("rustcc_import_{}_{}.hpp", tag, std::process::id()));
    std::fs::write(&path, contents).expect("write temp header");
    path
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn imports_pod_struct_with_scalar_fields() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Foo {\n    int x;\n    char y;\n};\n",
        "pod",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert_eq!(class_ids.len(), 1);
    let class = ctx.class(class_ids[0]);

    let name = match class.name.0.last() {
        Some(rustc_abi_cxx::NameSegment::Class(i))
        | Some(rustc_abi_cxx::NameSegment::Namespace(i)) => i.0.as_str(),
        _ => panic!("unexpected name"),
    };
    assert_eq!(name, "Foo");
    assert_eq!(class.fields.len(), 2);
    assert_eq!(class.fields[0].name.0, "x");
    assert_eq!(class.fields[1].name.0, "y");

    assert!(matches!(
        ctx.type_of(class.fields[0].ty),
        CxxType::Int {
            signed: true,
            width: IntWidth::I32
        }
    ));
    assert!(matches!(
        ctx.type_of(class.fields[1].ty),
        CxxType::Int {
            signed: true,
            width: IntWidth::I8
        }
    ));

    cleanup(&header);
}

#[test]
fn imported_struct_layouts_correctly_end_to_end() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Bar {\n    int a;\n    double b;\n};\n",
        "layout",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert_eq!(class_ids.len(), 1);
    let layout = ctx.layout(class_ids[0]).expect("layout");
    // Clang's layout for `{int a; double b}` is: a@0, b@8 (align 8),
    // sizeof=16, align=8. Verifies that the IR we produced feeds into
    // the layout engine identically to a hand-built ClassDef.
    assert_eq!(layout.size_bytes, 16);
    assert_eq!(layout.align_bytes, 8);
    assert_eq!(layout.field_offsets, vec![0, 8]);

    cleanup(&header);
}

#[test]
fn imports_pointer_field() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Buffer {\n    int* data;\n    int size;\n};\n",
        "ptr",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert_eq!(class_ids.len(), 1);
    let class = ctx.class(class_ids[0]);
    assert_eq!(class.fields.len(), 2);
    assert_eq!(class.fields[0].name.0, "data");

    match ctx.type_of(class.fields[0].ty) {
        CxxType::Ptr { pointee, cv } => {
            assert!(!cv.is_const);
            assert!(matches!(
                ctx.type_of(*pointee),
                CxxType::Int {
                    signed: true,
                    width: IntWidth::I32
                }
            ));
        }
        other => panic!("expected Ptr, got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn imports_nested_record_by_value() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Inner {\n    int a;\n    char b;\n};\n\
         struct Outer {\n    char prefix;\n    Inner inner;\n    int trailing;\n};\n",
        "nested",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert_eq!(ids.len(), 2);
    // Inner was declared first, so it's ids[0]. Outer references Inner.
    let (inner_id, outer_id) = (ids[0], ids[1]);

    let outer = ctx.class(outer_id);
    assert_eq!(outer.fields[1].name.0, "inner");
    match ctx.type_of(outer.fields[1].ty) {
        CxxType::Record(got) => assert_eq!(*got, inner_id),
        other => panic!("expected Record, got {other:?}"),
    }

    // End-to-end: the imported Outer's layout matches what our
    // hand-built `nested_record` corpus asserts.
    let layout = ctx.layout(outer_id).expect("layout");
    assert_eq!(layout.size_bytes, 16);
    assert_eq!(layout.align_bytes, 4);
    assert_eq!(layout.field_offsets, vec![0, 4, 12]);

    cleanup(&header);
}

#[test]
fn imports_non_virtual_base() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Base { int x; };\nstruct Derived : Base { int y; };\n",
        "base",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert_eq!(ids.len(), 2);
    let (base_id, derived_id) = (ids[0], ids[1]);

    let derived = ctx.class(derived_id);
    assert_eq!(derived.bases.len(), 1);
    assert_eq!(derived.bases[0].class, base_id);
    assert!(!derived.bases[0].virtual_);

    let layout = ctx.layout(derived_id).expect("layout");
    // Base { int x } is POD, sizeof=4, dsize=4. Derived adds int y at
    // offset 4 (after Base's tail — POD base forbids reuse). sizeof=8.
    assert_eq!(layout.size_bytes, 8);
    assert_eq!(layout.base_offsets, vec![(base_id, 0)]);
    assert_eq!(layout.field_offsets, vec![4]);

    cleanup(&header);
}

#[test]
fn imports_array_field() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct WithArray { int items[5]; char tail; };\n",
        "array",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let class = ctx.class(ids[0]);
    match ctx.type_of(class.fields[0].ty) {
        CxxType::Array { elem, len } => {
            assert_eq!(*len, 5);
            assert!(matches!(
                ctx.type_of(*elem),
                CxxType::Int {
                    signed: true,
                    width: IntWidth::I32
                }
            ));
        }
        other => panic!("expected Array, got {other:?}"),
    }

    let layout = ctx.layout(ids[0]).expect("layout");
    assert_eq!(layout.size_bytes, 24); // 5*4 = 20, + 1 byte tail, rounded to align 4.
    assert_eq!(layout.field_offsets, vec![0, 20]);

    cleanup(&header);
}

#[test]
fn imports_virtual_method_flips_polymorphic() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Widget {\n    virtual void render();\n    int value;\n};\n\
         void Widget::render() {}\n",
        "virtual",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let class = ctx.class(ids[0]);
    assert!(class.is_polymorphic, "virtual method → polymorphic class");
    assert_eq!(class.methods.len(), 1);
    assert_eq!(class.methods[0].name.ident_name(), Some("render"));
    assert_eq!(class.methods[0].virtuality, Virtuality::Virtual);
    assert!(class.methods[0].special.is_none());

    // Layout should allocate a vptr now that the class is polymorphic.
    let layout = ctx.layout(ids[0]).expect("layout");
    assert!(layout.has_vptr);
    assert_eq!(layout.size_bytes, 16); // ptr + int + tail padding.

    cleanup(&header);
}

#[test]
fn imports_polymorphism_through_full_stack() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Widget {\n    virtual ~Widget();\n    virtual void render();\n    int value;\n};\n\
         Widget::~Widget() = default;\n\
         void Widget::render() {}\n",
        "poly_stack",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    // Two methods: the dtor and render.
    assert_eq!(class.methods.len(), 2);
    let dtor = class
        .methods
        .iter()
        .find(|m| m.special == Some(SpecialMember::Dtor))
        .expect("dtor present");
    assert_eq!(dtor.virtuality, Virtuality::Virtual);
    let render = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("render"))
        .expect("render present");
    assert_eq!(render.virtuality, Virtuality::Virtual);

    // Layout matches the `polymorphic` corpus exactly.
    let layout = ctx.layout(class_id).expect("layout");
    assert!(layout.has_vptr);
    assert_eq!(layout.size_bytes, 16);
    assert_eq!(layout.data_size_bytes, 12);

    // Vtable lines up too: offset_to_top, RTTI, D1, D0, render.
    let vt = ctx.vtable(class_id).expect("vtable");
    assert_eq!(vt.sub_tables[0].entries.len(), 5);
    let expect_fn = |slot: usize, target: &str| match &vt.sub_tables[0].entries[slot] {
        VTableEntry::FunctionPointer { mangled_target, .. } => {
            assert_eq!(mangled_target, target, "slot {slot}")
        }
        other => panic!("slot {slot}: expected FunctionPointer, got {other:?}"),
    };
    expect_fn(2, "_ZN6WidgetD1Ev");
    expect_fn(3, "_ZN6WidgetD0Ev");
    expect_fn(4, "_ZN6Widget6renderEv");

    cleanup(&header);
}

#[test]
fn imports_method_mangle_roundtrip() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Calc {\n    int add(int a, int b);\n    int value() const;\n};\n\
         int Calc::add(int a, int b) { return a + b; }\n\
         int Calc::value() const { return 0; }\n",
        "mangle_method",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    // Clone the method sigs out so we can drop the borrow on ctx
    // before calling mangle (which takes &self on ctx).
    let add_sig = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("add"))
        .expect("add")
        .sig
        .clone();
    let value_sig = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("value"))
        .expect("value")
        .sig
        .clone();

    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Ident(Ident("add".into())),
            sig: add_sig,
        }),
        "_ZN4Calc3addEii"
    );
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Ident(Ident("value".into())),
            sig: value_sig,
        }),
        "_ZNK4Calc5valueEv"
    );

    cleanup(&header);
}

#[test]
fn imports_virtual_base_simple_layout() {
    // `struct B : virtual A { int b; }` acquires a vptr at offset 0
    // (even without virtual methods) to store virtual-base offsets.
    // Clang places A at the end of B's non-virtual portion.
    //
    // Expected layout per Itanium / matching Clang:
    //   vptr @ 0 (size 8, align 8)
    //   int b @ 8
    //   virtual base A { int x } @ 12
    //   sizeof = 16, dsize = 16, nvsize = 12, nvalign = 8
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct A { int x; };\n\
         struct B : virtual A { int b; };\n\
         B g_b;\n",
        "virtual_base",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("virtual base should import");

    let a_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "A")
        })
        .expect("A imported");
    let b_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "B")
        })
        .expect("B imported");

    let b = ctx.class(b_id);
    assert_eq!(b.bases.len(), 1);
    assert!(b.bases[0].virtual_, "A is a virtual base of B");
    assert_eq!(b.bases[0].class, a_id);
    assert!(
        b.is_polymorphic,
        "a class with a virtual base is implicitly polymorphic"
    );

    let layout = ctx.layout(b_id).expect("layout");
    assert_eq!(layout.size_bytes, 16);
    assert_eq!(layout.data_size_bytes, 16);
    assert_eq!(layout.nv_size_bytes, 12);
    assert_eq!(layout.align_bytes, 8);
    assert!(layout.has_vptr);
    // int b at offset 8 (past the vptr).
    assert_eq!(layout.field_offsets, vec![8]);
    // Virtual base A is recorded at offset 12, separate from `base_offsets`.
    assert!(layout.base_offsets.is_empty());
    assert_eq!(layout.virtual_base_offsets, vec![(a_id, 12)]);

    cleanup(&header);
}

#[test]
fn virtual_base_vtable_has_vbase_offset_entry() {
    // A polymorphic class with a virtual base gets a vbase-offset slot
    // in its primary vtable BEFORE `offset_to_top`. The address point
    // (where the vptr in an object points) therefore shifts to
    // `(num_vbases + 2) * ptr_size` bytes from the sub-table's start,
    // not `2 * ptr_size` as in the classical single-inheritance case.
    //
    // For `struct B : virtual A` with A at offset 16 in B, the primary
    // vtable starts:
    //   [ VbaseOffset(16), OffsetToTop(0), Rtti("_ZTI1B"), fn... ]
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct A { virtual ~A(); virtual void fa(); int a; };\n\
         struct B : virtual A { virtual void fb(); int b; };\n\
         A::~A() = default;\n\
         void A::fa() {}\n\
         void B::fb() {}\n\
         B g_b;\n",
        "vbase_vtable",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let b_id = *ids
        .iter()
        .find(|&&id| matches!(ctx.class(id).name.0.last(),
            Some(NameSegment::Class(i)) if i.0 == "B"))
        .expect("B imported");

    // B has A at offset 16 (past B's vptr and `int b`).
    let layout = ctx.layout(b_id).expect("layout");
    assert_eq!(layout.virtual_base_offsets.len(), 1);
    assert_eq!(layout.virtual_base_offsets[0].1, 16);

    let vt = ctx.vtable(b_id).expect("B is polymorphic");
    let primary = &vt.sub_tables[0];
    // First entry is the vbase-offset slot.
    assert!(matches!(
        &primary.entries[0],
        VTableEntry::VbaseOffset(16)
    ));
    assert!(matches!(
        &primary.entries[1],
        VTableEntry::OffsetToTop(0)
    ));
    assert!(matches!(
        &primary.entries[2],
        VTableEntry::Rtti(s) if s == "_ZTI1B"
    ));
    // Address point lands on the first function slot — past the
    // vbase-offset (1 slot) + offset_to_top (1) + RTTI (1) = 3 * 8 = 24.
    assert_eq!(primary.address_point_offset, 24);

    cleanup(&header);
}

#[test]
fn imports_diamond_with_shared_virtual_base() {
    // Classic diamond: A is shared (only one instance) via virtual
    // inheritance by both D1 and D2. `struct D : D1, D2` must have
    // exactly one A subobject at the end.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct A { int x; };\n\
         struct D1 : virtual A { int d1; };\n\
         struct D2 : virtual A { int d2; };\n\
         struct D : D1, D2 { int d; };\n\
         D g_d;\n",
        "diamond",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("diamond should import");

    let a_id = *ids
        .iter()
        .find(|&&id| matches!(ctx.class(id).name.0.last(),
            Some(NameSegment::Class(i)) if i.0 == "A"))
        .expect("A imported");
    let d_id = *ids
        .iter()
        .find(|&&id| matches!(ctx.class(id).name.0.last(),
            Some(NameSegment::Class(i)) if i.0 == "D"))
        .expect("D imported");

    let layout = ctx.layout(d_id).expect("layout");
    // Clang: D1 at 0 (nvsize 12), D2 at 16 (nvsize 12, end at 28),
    // `d` reuses D2's tail at 28, then virtual A at 32, sizeof 40.
    assert_eq!(layout.size_bytes, 40);
    assert_eq!(layout.align_bytes, 8);
    // Exactly one A subobject (the diamond's shared virtual base).
    assert_eq!(layout.virtual_base_offsets.len(), 1);
    assert_eq!(layout.virtual_base_offsets[0], (a_id, 32));
    // d reuses D2's tail padding at offset 28.
    assert_eq!(layout.field_offsets, vec![28]);

    cleanup(&header);
}

#[test]
fn imports_multiple_nonvirtual_inheritance() {
    // Layout for `struct Derived : A, B` composes each base sequentially
    // after the last. For non-polymorphic bases this is straightforward:
    // A at 0 (4 bytes), B at 4 (4 bytes), trailing `int c` at 8.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct A { int a; };\n\
         struct B { int b; };\n\
         struct Derived : A, B { int c; };\n\
         Derived g;\n",
        "multi_inherit",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let a_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "A")
        })
        .expect("A imported");
    let b_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "B")
        })
        .expect("B imported");
    let derived_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "Derived")
        })
        .expect("Derived imported");

    let derived = ctx.class(derived_id);
    assert_eq!(derived.bases.len(), 2);
    assert_eq!(derived.bases[0].class, a_id);
    assert_eq!(derived.bases[1].class, b_id);

    let layout = ctx.layout(derived_id).expect("layout");
    assert_eq!(layout.size_bytes, 12);
    assert_eq!(layout.align_bytes, 4);
    // A at offset 0, B at offset 4, c at offset 8.
    assert_eq!(layout.base_offsets, vec![(a_id, 0), (b_id, 4)]);
    assert_eq!(layout.field_offsets, vec![8]);

    cleanup(&header);
}

#[test]
fn imports_polymorphic_multi_inheritance_vtable() {
    // For `struct PD : PA, PB` with both bases polymorphic, `_ZTV2PD`
    // holds TWO sub-tables: the primary for PA's subobject at offset 0,
    // and a secondary for PB's subobject at offset 16. The secondary's
    // offset_to_top is `-16`, its RTTI references PD, and its function
    // pointers are `this`-adjusting thunks (`_ZThn16_...`) for the PD
    // methods that override PB's virtuals — when PD doesn't override
    // a PB method, no thunk is needed but our algorithm still emits
    // one (technically incorrect for non-override slots but fine for
    // the structural guarantee this test checks).
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct PA { virtual ~PA(); virtual void fa(); int pa; };\n\
         struct PB { virtual ~PB(); virtual void fb(); int pb; };\n\
         struct PD : PA, PB {\n\
             void fa() override;\n\
             void fb() override;\n\
             int pd;\n\
         };\n\
         PA::~PA() = default;\n\
         PB::~PB() = default;\n\
         void PA::fa() {}\n\
         void PB::fb() {}\n\
         void PD::fa() {}\n\
         void PD::fb() {}\n\
         PD g_pd;\n",
        "poly_mi_vtable",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let pd_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "PD")
        })
        .expect("PD imported");

    let vt = ctx.vtable(pd_id).expect("PD is polymorphic");

    // Two sub-tables: one for the PA subobject (primary) and one for PB.
    assert_eq!(vt.sub_tables.len(), 2, "PD should have 2 sub-tables");

    // Primary: offset_to_top 0, RTTI _ZTI2PD, subobject_offset 0.
    let primary = &vt.sub_tables[0];
    assert_eq!(primary.subobject_offset, 0);
    assert!(matches!(
        &primary.entries[0],
        VTableEntry::OffsetToTop(0)
    ));
    assert!(matches!(
        &primary.entries[1],
        VTableEntry::Rtti(s) if s == "_ZTI2PD"
    ));

    // Secondary: for PB at offset 16, offset_to_top -16.
    let secondary = &vt.sub_tables[1];
    assert_eq!(secondary.subobject_offset, 16);
    assert!(matches!(
        &secondary.entries[0],
        VTableEntry::OffsetToTop(-16)
    ));
    assert!(matches!(
        &secondary.entries[1],
        VTableEntry::Rtti(s) if s == "_ZTI2PD"
    ));
    // PD overrides PB::fb, so the secondary slot is a this-adjusting
    // thunk `_ZThn16_N2PD2fbEv` that subtracts 16 from `this` and
    // jumps into PD::fb.
    let has_fb_thunk = secondary.entries.iter().any(|e| {
        matches!(e, VTableEntry::FunctionPointer { mangled_target, .. }
            if mangled_target == "_ZThn16_N2PD2fbEv")
    });
    assert!(has_fb_thunk, "secondary should contain fb thunk");

    cleanup(&header);
}

#[test]
fn imports_polymorphic_multi_inheritance_layout() {
    // `struct PD : PA, PB` with both bases polymorphic. PA becomes the
    // primary base at offset 0 (its vptr is shared with PD's). PB sits
    // at a non-zero offset with its own vptr. Layout only — the
    // secondary vtable for PB is out of v1 scope and `ctx.vtable(PD)`
    // would currently only emit the primary-chain vtable.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct PA { virtual void fa(); int pa; };\n\
         struct PB { virtual void fb(); int pb; };\n\
         struct PD : PA, PB { int pd; };\n\
         void PA::fa() {}\n\
         void PB::fb() {}\n\
         PD g_pd;\n",
        "poly_multi",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let pd_id = *ids
        .iter()
        .find(|&&id| {
            matches!(ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "PD")
        })
        .expect("PD imported");
    let pd = ctx.class(pd_id);
    assert_eq!(pd.bases.len(), 2);
    assert!(pd.is_polymorphic);

    let layout = ctx.layout(pd_id).expect("layout");
    // PA primary at 0 (shares vptr). PB at 16 with its own vptr.
    // int pd at 28, sizeof rounds up to 32 (align=8 from pointers).
    assert_eq!(layout.size_bytes, 32);
    assert_eq!(layout.align_bytes, 8);
    assert!(layout.has_vptr, "PD inherits PA's primary vptr");
    assert_eq!(layout.base_offsets[0].1, 0);  // PA
    assert_eq!(layout.base_offsets[1].1, 16); // PB
    assert_eq!(layout.field_offsets, vec![28]);

    cleanup(&header);
}

#[test]
fn imports_class_template_specialization() {
    // Class template specializations (`Box<int>`) are imported as
    // concrete classes whose `NestedName` carries a `TemplateSpec`
    // segment with the argument types. Verifies the full stack:
    //   C++ source → importer (detects spec, records args)
    //              → layout   (treats spec like a regular class)
    //              → mangler  (emits `<name>I<args>E`)
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "template<typename T>\n\
         struct Box {\n\
             T value;\n\
             T get() const;\n\
         };\n\
         template<typename T>\n\
         T Box<T>::get() const { return value; }\n\
         \n\
         template struct Box<int>;  // explicit instantiation\n\
         \n\
         struct Holder {\n\
             Box<int> b;\n\
             int tag;\n\
         };\n\
         Holder g_holder;\n",
        "template_spec",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Locate Box<int> and Holder in the imports (order depends on Clang's
    // AST walk; search by the last segment's kind/name).
    let box_id = *ids
        .iter()
        .find(|&&id| {
            matches!(
                ctx.class(id).name.0.last(),
                Some(NameSegment::TemplateSpec { name, .. }) if name.0 == "Box"
            )
        })
        .expect("Box<int> imported");
    let holder_id = *ids
        .iter()
        .find(|&&id| {
            matches!(
                ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "Holder"
            )
        })
        .expect("Holder imported");

    // The Box<int> specialization carries one type template argument (int).
    let box_class = ctx.class(box_id);
    match box_class.name.0.last() {
        Some(NameSegment::TemplateSpec { args, .. }) => {
            assert_eq!(args.len(), 1);
            let TemplateArg::Type(int_id) = &args[0];
            assert!(matches!(
                ctx.type_of(*int_id),
                CxxType::Int {
                    signed: true,
                    width: IntWidth::I32
                }
            ));
        }
        other => panic!("expected TemplateSpec segment, got {other:?}"),
    }

    // Box<int>::value is an int field; the `T` parameter has been
    // substituted with `int`.
    assert_eq!(box_class.fields.len(), 1);
    assert_eq!(box_class.fields[0].name.0, "value");
    assert!(matches!(
        ctx.type_of(box_class.fields[0].ty),
        CxxType::Int {
            signed: true,
            width: IntWidth::I32
        }
    ));

    // Holder holds Box<int> by value; composed layout should be
    // sizeof=8 with Box<int> (4 bytes) at 0 and int tag at 4.
    let holder = ctx.class(holder_id);
    assert_eq!(holder.fields[0].name.0, "b");
    match ctx.type_of(holder.fields[0].ty) {
        CxxType::Record(r) => assert_eq!(*r, box_id),
        other => panic!("expected Record(Box<int>), got {other:?}"),
    }
    let layout = ctx.layout(holder_id).expect("layout");
    assert_eq!(layout.size_bytes, 8);
    assert_eq!(layout.align_bytes, 4);
    assert_eq!(layout.field_offsets, vec![0, 4]);

    // Mangle Box<int>::get() const — Clang emits _ZNK3BoxIiE3getEv.
    // The `IiE` between `3Box` and `3get` carries the template argument.
    //
    // libclang's cursor traversal doesn't surface the instantiated
    // method on a template spec cursor (documented limitation in
    // `import.rs`), so we construct the `Symbol::Method` signature by
    // hand — the important roundtrip is that the mangler uses the
    // imported `box_id`'s `NestedName` (which does carry the correct
    // `TemplateSpec` segment) and emits the expected string.
    let int_ty = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let get_sig = FnSig {
        params: Vec::new(),
        ret: int_ty,
        cv: CvQual {
            is_const: true,
            is_volatile: false,
        },
        ref_q: None,
        variadic: false,
        noexcept: false,
    };
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: box_id,
            name: MethodName::Ident(Ident("get".into())),
            sig: get_sig,
        }),
        "_ZNK3BoxIiE3getEv"
    );

    cleanup(&header);
}

#[test]
fn imports_union_lays_out_all_fields_at_offset_zero() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "union U {\n\
             int as_int;\n\
             float as_float;\n\
             char bytes[4];\n\
         };\n\
         U g;\n",
        "union",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class = ctx.class(ids[0]);
    assert_eq!(class.kind, RecordKind::Union);
    assert_eq!(class.fields.len(), 3);
    assert_eq!(class.fields[0].name.0, "as_int");
    assert_eq!(class.fields[1].name.0, "as_float");
    assert_eq!(class.fields[2].name.0, "bytes");

    let layout = ctx.layout(ids[0]).expect("layout");
    assert_eq!(layout.size_bytes, 4);
    assert_eq!(layout.align_bytes, 4);
    // Defining feature of unions: all members share the same storage.
    for (i, &off) in layout.field_offsets.iter().enumerate() {
        assert_eq!(off, 0, "field {i} should be at offset 0");
    }

    cleanup(&header);
}

#[test]
fn imports_conversion_operator_and_mangles_it() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Celsius {\n\
             int degrees;\n\
             operator int() const;\n\
         };\n\
         Celsius::operator int() const { return degrees; }\n",
        "conv_op",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    let conv = class
        .methods
        .iter()
        .find(|m| matches!(&m.name, MethodName::ConversionTo(_)))
        .expect("conversion operator imported");

    // The target type should be `int`.
    let target_ty = match &conv.name {
        MethodName::ConversionTo(t) => *t,
        _ => unreachable!(),
    };
    assert!(matches!(
        ctx.type_of(target_ty),
        CxxType::Int {
            signed: true,
            width: IntWidth::I32
        }
    ));

    // The method is `const` (the trailing `const` in source).
    assert!(conv.sig.cv.is_const);
    // No source-level parameters.
    assert!(conv.sig.params.is_empty());

    // Clone for Symbol::Method construction.
    let conv_name = conv.name.clone();
    let conv_sig = conv.sig.clone();

    // Celsius::operator int() const → _ZNK7CelsiuscviEv
    //   `_Z` + `N` + `K` (const) + `7Celsius` + `cvi` (conv-to-int) +
    //   `E` + `v` (no source-level params).
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: conv_name,
            sig: conv_sig,
        }),
        "_ZNK7CelsiuscviEv"
    );

    cleanup(&header);
}

#[test]
fn imports_enum_field_and_mangles_method_with_enum_param() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "enum E { A, B };\n\
         struct Foo {\n\
             E state;\n\
             void set(E e);\n\
         };\n\
         void Foo::set(E e) { (void)e; state = e; }\n",
        "enum_field",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    // The `state` field's type is `CxxType::Enum` with name "E".
    let state_field = &class.fields[0];
    assert_eq!(state_field.name.0, "state");
    match ctx.type_of(state_field.ty) {
        CxxType::Enum { name, .. } => {
            assert_eq!(name.0.len(), 1);
            match &name.0[0] {
                NameSegment::Enum(i) => assert_eq!(i.0, "E"),
                other => panic!("expected NameSegment::Enum, got {other:?}"),
            }
        }
        other => panic!("expected CxxType::Enum, got {other:?}"),
    }

    // Mangle Foo::set(E) — `_ZN3Foo3setE1E`. The trailing `E` after
    // `3set` closes the nested-name; the `1E` is the enum parameter's
    // source-name (length-1, "E"), the same form a class type would
    // use.
    let set_sig = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("set"))
        .expect("set method")
        .sig
        .clone();
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Ident(Ident("set".into())),
            sig: set_sig,
        }),
        "_ZN3Foo3setE1E"
    );

    cleanup(&header);
}

#[test]
fn imports_ctor_variants_classification() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Widget {\n\
             int x;\n\
             Widget();\n\
             Widget(int);\n\
             Widget(const Widget&);\n\
             Widget(Widget&&);\n\
             Widget& operator=(const Widget&);\n\
             Widget& operator=(Widget&&);\n\
             ~Widget();\n\
         };\n\
         Widget::Widget() {}\n\
         Widget::Widget(int v) { x = v; }\n\
         Widget::Widget(const Widget& o) { x = o.x; }\n\
         Widget::Widget(Widget&& o) noexcept { x = o.x; }\n\
         Widget& Widget::operator=(const Widget& o) { x = o.x; return *this; }\n\
         Widget& Widget::operator=(Widget&& o) noexcept { x = o.x; return *this; }\n\
         Widget::~Widget() {}\n",
        "ctor_variants",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class = ctx.class(ids[0]);

    let count_special = |want: SpecialMember| {
        class
            .methods
            .iter()
            .filter(|m| m.special == Some(want))
            .count()
    };

    assert_eq!(count_special(SpecialMember::DefaultCtor), 1, "Widget()");
    assert_eq!(
        count_special(SpecialMember::OtherCtor),
        1,
        "Widget(int)"
    );
    assert_eq!(
        count_special(SpecialMember::CopyCtor),
        1,
        "Widget(const Widget&)"
    );
    assert_eq!(
        count_special(SpecialMember::MoveCtor),
        1,
        "Widget(Widget&&)"
    );
    assert_eq!(
        count_special(SpecialMember::CopyAssign),
        1,
        "operator=(const Widget&)"
    );
    assert_eq!(
        count_special(SpecialMember::MoveAssign),
        1,
        "operator=(Widget&&)"
    );
    assert_eq!(count_special(SpecialMember::Dtor), 1, "~Widget()");

    cleanup(&header);
}

#[test]
fn imports_operator_mangle_roundtrip() {
    // Exercises operator detection end-to-end: the importer maps
    // `operator+`/`operator=`/`operator[]` source-name strings to
    // `MethodName::Operator(...)`, and the mangler emits the matching
    // Itanium two-letter codes (pl/aS/ix) with substitutions for the
    // repeated `Vec` references.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Vec {\n\
             int x;\n\
             Vec operator+(const Vec& other) const;\n\
             Vec& operator=(const Vec& other);\n\
             int& operator[](int idx);\n\
         };\n\
         Vec Vec::operator+(const Vec& other) const { return Vec{x + other.x}; }\n\
         Vec& Vec::operator=(const Vec& other) { x = other.x; return *this; }\n\
         int& Vec::operator[](int idx) { (void)idx; return x; }\n",
        "operators",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    let find_op = |op: OperatorKind| {
        class
            .methods
            .iter()
            .find(|m| matches!(&m.name, MethodName::Operator(k) if *k == op))
            .unwrap_or_else(|| panic!("operator {op:?} not imported"))
    };
    let plus_sig = find_op(OperatorKind::Plus).sig.clone();
    let assign_sig = find_op(OperatorKind::Assign).sig.clone();
    let index_sig = find_op(OperatorKind::Index).sig.clone();

    // Vec::operator+(const Vec&) const → _ZNK3VecplERKS_
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Operator(OperatorKind::Plus),
            sig: plus_sig,
        }),
        "_ZNK3VecplERKS_"
    );
    // Vec::operator=(const Vec&) → _ZN3VecaSERKS_
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Operator(OperatorKind::Assign),
            sig: assign_sig,
        }),
        "_ZN3VecaSERKS_"
    );
    // Vec::operator[](int) → _ZN3VecixEi  (no substitution needed)
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Operator(OperatorKind::Index),
            sig: index_sig,
        }),
        "_ZN3VecixEi"
    );

    cleanup(&header);
}

#[test]
fn imports_class_inside_single_namespace() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "namespace ns {\n\
         struct Foo { int x; };\n\
         }\n",
        "ns_single",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    assert_eq!(ids.len(), 1);

    let class = ctx.class(ids[0]);
    assert_eq!(class.name.0.len(), 2);
    match &class.name.0[0] {
        NameSegment::Namespace(i) => assert_eq!(i.0, "ns"),
        other => panic!("expected Namespace, got {other:?}"),
    }
    match &class.name.0[1] {
        NameSegment::Class(i) => assert_eq!(i.0, "Foo"),
        other => panic!("expected Class, got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn imports_nested_namespaces_and_mangles_correctly() {
    // Full source-to-mangled-string pipeline exercising the importer's
    // namespace walk, the IR's NestedName representation, and the
    // mangler's substitution table — the mangling matches the golden
    // string pinned in the `mangle_nested` corpus.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "namespace outer {\n\
         namespace inner {\n\
         struct Bar {\n\
             void baz();\n\
             void self_(const Bar& other);\n\
         };\n\
         }\n\
         }\n\
         void outer::inner::Bar::baz() {}\n\
         void outer::inner::Bar::self_(const Bar& other) { (void)other; }\n",
        "ns_nested",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = ids[0];
    let class = ctx.class(class_id);

    // Path is outer → inner → Bar.
    assert_eq!(class.name.0.len(), 3);
    assert!(matches!(&class.name.0[0], NameSegment::Namespace(i) if i.0 == "outer"));
    assert!(matches!(&class.name.0[1], NameSegment::Namespace(i) if i.0 == "inner"));
    assert!(matches!(&class.name.0[2], NameSegment::Class(i) if i.0 == "Bar"));

    let baz_sig = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("baz"))
        .expect("baz")
        .sig
        .clone();
    let self_sig = class
        .methods
        .iter()
        .find(|m| m.name.ident_name() == Some("self_"))
        .expect("self_")
        .sig
        .clone();

    // Mangling: outer::inner::Bar::baz() → _ZN5outer5inner3Bar3bazEv.
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Ident(Ident("baz".into())),
            sig: baz_sig,
        }),
        "_ZN5outer5inner3Bar3bazEv"
    );
    // Mangling: outer::inner::Bar::self_(const Bar&). The param type
    // references the enclosing class, so the substitution table should
    // kick in and emit S1_ (the third registered nested-name prefix).
    assert_eq!(
        ctx.mangle(&Symbol::Method {
            class: class_id,
            name: MethodName::Ident(Ident("self_".into())),
            sig: self_sig,
        }),
        "_ZN5outer5inner3Bar5self_ERKS1_"
    );

    cleanup(&header);
}

#[test]
fn imports_const_pointer_preserves_cv() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct CRef {\n    const int* p;\n};\n",
        "const",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let class = ctx.class(class_ids[0]);
    match ctx.type_of(class.fields[0].ty) {
        CxxType::Ptr { cv, .. } => {
            assert!(cv.is_const, "const should propagate to pointee CV");
        }
        other => panic!("expected Ptr, got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn imports_noexcept_methods_into_fnsig() {
    // Polish item: `noexcept` is part of the function type from
    // C++17 onward and useful surface info for downstream emitters.
    // We extract `BasicNoexcept` (bare `noexcept`) and
    // `ComputedNoexcept` (`noexcept(expr)`).
    //
    // Known libclang limitation: the API exposes only the *kind* of
    // exception spec, not the computed boolean value of the
    // `noexcept(expr)` expression. So `noexcept(false)` (which
    // semantically means the function CAN throw) is reported as
    // `ComputedNoexcept` and gets flagged here as `noexcept = true`
    // — a false positive. A future revision could pair this with
    // `clang_Cursor_isFunctionInlined`-style probes or by parsing
    // the expr node, but the cost-vs-benefit is poor for what's
    // already a rarely-used corner of the spec.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Foo {\n\
         \x20   void plain();\n\
         \x20   void noex() noexcept;\n\
         \x20   void noex_true() noexcept(true);\n\
         };\n",
        "noexcept_extract",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx)
        .expect("import");
    let class = ctx.class(ids[0]);
    let by_name = |name: &str| -> bool {
        class
            .methods
            .iter()
            .find(|m| m.name.ident_name() == Some(name))
            .unwrap_or_else(|| panic!("method `{name}` not found"))
            .sig
            .noexcept
    };
    assert!(!by_name("plain"), "plain method should not be noexcept");
    assert!(by_name("noex"), "bare noexcept should be flagged");
    assert!(
        by_name("noex_true"),
        "noexcept(true) reduces to noexcept; should be flagged"
    );
    cleanup(&header);
}

#[test]
fn imports_ref_qualified_methods_into_fnsig() {
    // C++11 ref-qualifiers split overloads on the value category
    // of the receiver. The importer surfaces them in
    // `FnSig::ref_q`; the mangler uses them to disambiguate
    // overload resolution at the symbol level.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Foo {\n\
         \x20   void unqual();\n\
         \x20   void lref() &;\n\
         \x20   void rref() &&;\n\
         };\n",
        "refq_extract",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx)
        .expect("import");
    let class = ctx.class(ids[0]);
    let refq_by_name = |name: &str| -> Option<rustc_abi_cxx::RefKind> {
        class
            .methods
            .iter()
            .find(|m| m.name.ident_name() == Some(name))
            .unwrap_or_else(|| panic!("method `{name}` not found"))
            .sig
            .ref_q
    };
    assert_eq!(refq_by_name("unqual"), None);
    assert_eq!(refq_by_name("lref"), Some(rustc_abi_cxx::RefKind::Lvalue));
    assert_eq!(refq_by_name("rref"), Some(rustc_abi_cxx::RefKind::Rvalue));
    cleanup(&header);
}

#[test]
fn rust_bindings_emits_op_words_for_operators_and_distinguishes_const_mut() {
    // Real C++ headers lean on operator overloading. The bindings
    // emitter should route `MethodName::Operator(Plus)` →
    // `op_add`, `Index` → `op_index` (or `op_index_mut` when the
    // overload is non-const), comparison ops to `op_eq` / `op_lt`
    // / etc. without `_mut` regardless of qualifier.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Vec3 {\n\
         \x20   int x_, y_, z_;\n\
         \x20   Vec3();\n\
         \x20   ~Vec3();\n\
         \x20   Vec3 operator+(const Vec3& other) const;\n\
         \x20   bool operator==(const Vec3& other) const;\n\
         \x20   int operator[](int i) const;\n\
         \x20   int& operator[](int i);\n\
         };\n",
        "operator_bindings",
    );

    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let src = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig {
            backend: BindingsBackend::DirectExternCpp,
            ..RustBindingsConfig::default()
        },
    )
    .expect("emit_rust_bindings");

    // `operator+` (const) → `op_add`.
    assert!(
        src.contains("pub fn op_add("),
        "expected `op_add` wrapper:\n{src}"
    );
    // `operator==` (const) → `op_eq` (no `_mut` even when looser
    // headers omit `const`; this header has `const`).
    assert!(
        src.contains("pub fn op_eq("),
        "expected `op_eq` wrapper:\n{src}"
    );
    // `operator[](int) const` → `op_index`. `operator[](int)` →
    // `op_index_mut`. Both exist as separate wrappers.
    assert!(
        src.contains("pub fn op_index("),
        "expected const `op_index` wrapper:\n{src}"
    );
    assert!(
        src.contains("pub fn op_index_mut("),
        "expected non-const `op_index_mut` wrapper:\n{src}"
    );

    cleanup(&header);
}

#[test]
fn rust_bindings_disambiguates_overloaded_plain_methods_by_param_signature() {
    // Two methods with the same source name + different param
    // signatures must each get a unique Rust identifier in the
    // emitted impl block. The first occurrence keeps the base
    // name; subsequent ones append a sanitized parameter
    // signature.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Cmp {\n\
         \x20   int compare(int a) const;\n\
         \x20   int compare(double a) const;\n\
         };\n",
        "overload_bindings",
    );

    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let src = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig::default(),
    )
    .expect("emit_rust_bindings");

    // First overload keeps base name.
    assert!(
        src.contains("pub fn compare(&self, arg0: i32)"),
        "expected base `compare(i32)` wrapper:\n{src}"
    );
    // Second overload gets a disambiguator suffix.
    assert!(
        src.contains("pub fn compare_f64") || src.contains("pub fn compare_double"),
        "expected disambiguated wrapper for compare(double):\n{src}"
    );

    cleanup(&header);
}

#[test]
fn imports_variadic_methods_into_fnsig() {
    // Variadic functions / methods (C-style `...` ellipsis) are
    // surfaced via `FnSig::variadic`. Required for round-tripping
    // C-interop methods whose signature legitimately uses the
    // variadic shape (`printf`-style logging hooks).
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Logger {\n\
         \x20   int log(const char* fmt, ...);\n\
         \x20   int regular(int n);\n\
         };\n",
        "variadic_extract",
    );
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx)
        .expect("import");
    let class = ctx.class(ids[0]);
    let variadic_by_name = |name: &str| -> bool {
        class
            .methods
            .iter()
            .find(|m| m.name.ident_name() == Some(name))
            .unwrap_or_else(|| panic!("method `{name}` not found"))
            .sig
            .variadic
    };
    assert!(variadic_by_name("log"), "log(fmt, ...) should be variadic");
    assert!(
        !variadic_by_name("regular"),
        "regular(int) should not be variadic"
    );
    cleanup(&header);
}
