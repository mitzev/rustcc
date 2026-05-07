//! End-to-end libclang import tests.
//!
//! These verify that `cxx_importer::import_header` parses a small C++
//! header, produces `ClassDef`s in `CxxTypeCtx`, and that those
//! definitions feed correctly into `rustc_abi_cxx`'s layout engine.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{
    import_header, import_header_with_annotations, import_header_with_extras,
    Driver, HeaderGraph,
};
use cxx_importer::aliases::AliasSet;
use cxx_importer::rust_bindings::{
    generate_rust_bindings, generate_rust_bindings_with_annotations,
    BindingsBackend, RustBindingsConfig,
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
fn populates_vtable_index_on_virtual_methods() {
    // Polymorphic class: vptr is at offset 0; virtual methods get
    // vtable slots ranked by their position in the primary
    // sub-table's function-pointer region. The importer should
    // stamp `vtable_index` onto each virtual method we own.
    //
    // Layout-wise (Itanium AArch64 / SysV):
    //   slot 0 (after offset_to_top + RTTI): area()
    //   slot 1: side()
    //   slot 2 (we own this; new virtual): describe()
    //
    // Non-virtual methods (`tag`) keep `vtable_index = None`.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Shape {\n\
         \x20   virtual int area() const;\n\
         \x20   virtual int side() const;\n\
         \x20   virtual int describe() const;\n\
         \x20   int tag() const;\n\
         };\n",
        "vtable_index",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx)
        .expect("import");
    let class = ctx.class(ids[0]);

    let by_name = |name: &str| {
        class
            .methods
            .iter()
            .find(|m| m.name.ident_name() == Some(name))
            .unwrap_or_else(|| panic!("method `{name}` not found"))
    };

    let area = by_name("area");
    let side = by_name("side");
    let describe = by_name("describe");
    let tag = by_name("tag");

    // Virtuals get a stamped index.
    assert!(
        area.vtable_index.is_some(),
        "area() should have a vtable_index"
    );
    assert!(
        side.vtable_index.is_some(),
        "side() should have a vtable_index"
    );
    assert!(
        describe.vtable_index.is_some(),
        "describe() should have a vtable_index"
    );

    // Non-virtual stays None.
    assert_eq!(
        tag.vtable_index, None,
        "non-virtual `tag()` should NOT have a vtable_index"
    );

    // Indices are distinct.
    let indices: Vec<u32> = vec![area, side, describe]
        .into_iter()
        .map(|m| m.vtable_index.unwrap())
        .collect();
    let mut sorted = indices.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        3,
        "expected three distinct vtable_index values, got {indices:?}"
    );

    cleanup(&header);
}

#[test]
fn annotations_drive_class_and_method_renaming_end_to_end() {
    // Inline `[[clang::annotate("rustcc::name=...")]]` attrs flow
    // through the libclang walker into an `AnnotationSet`, and the
    // bindings emitter consults it for class / method name
    // overrides. Covers the M7 (annotations) deliverable from
    // `docs/cxx_importer.md §14`.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct __attribute__((annotate(\"rustcc::name=Renamed\"))) Original {\n\
         \x20   int __attribute__((annotate(\"rustcc::name=value\"))) compute() const;\n\
         };\n",
        "annotation_renames",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let (ids, anns) = import_header_with_annotations(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import with annotations");
    assert_eq!(ids.len(), 1, "expected one imported class");

    // Check the annotations were collected on the right keys.
    let class_anns = anns.effective("Original");
    assert!(
        class_anns
            .iter()
            .any(|a| matches!(a, cxx_importer::Annotation::Name(n) if n == "Renamed")),
        "expected Name(\"Renamed\") on class, got {class_anns:?}"
    );
    let method_anns = anns.effective("Original::compute");
    assert!(
        method_anns
            .iter()
            .any(|a| matches!(a, cxx_importer::Annotation::Name(n) if n == "value")),
        "expected Name(\"value\") on method, got {method_anns:?}"
    );

    // And the emitter actually applies them.
    let src = generate_rust_bindings_with_annotations(
        &ctx,
        &ids,
        &anns,
        &RustBindingsConfig::default(),
    )
    .expect("emit");
    assert!(
        src.contains("pub struct Renamed"),
        "expected renamed `Renamed` struct:\n{src}"
    );
    assert!(
        src.contains("pub fn value(&self) -> i32"),
        "expected renamed `value()` method:\n{src}"
    );
    cleanup(&header);
}

#[test]
fn driver_force_instantiates_class_template_via_synthetic_root() {
    // M8 (sidecar template instantiation) — exercising the
    // `HeaderGraph::template_instantiations` path. The driver
    // synthesizes a temp `.cpp` that `#include`s the user header
    // and emits `template class Box<int>;`, parses both, and
    // surfaces the instantiated `Box<int>` as a regular concrete
    // class.
    //
    // Limitation note (matches the long-standing import.rs gap):
    // libclang doesn't reliably surface instantiated method
    // bodies via child traversal of the spec cursor — fields come
    // through, methods may not. This test exercises the
    // synthetic-root + class-import side; method coverage on
    // template specs is tracked separately.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "template<class T>\n\
         struct Box {\n\
         \x20   T value;\n\
         };\n",
        "template_instantiate",
    );

    let driver = Driver::new(HeaderGraph {
        roots: vec![header.clone()],
        include_paths: vec![],
        clang_flags: vec!["-std=c++17".into()],
        template_instantiations: vec!["Box<int>".into()],
    });
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = driver.parse_all(&mut ctx).expect("parse_all");

    // The driver's synthetic root forces a specialization. Find
    // the `Box<int>` spec in the imports.
    let box_int = class_ids.iter().find(|&&id| {
        let class = ctx.class(id);
        match class.name.0.last() {
            Some(NameSegment::TemplateSpec { name, .. }) => name.0 == "Box",
            _ => false,
        }
    });
    assert!(
        box_int.is_some(),
        "expected `Box<int>` template specialization in imports; got {:?}",
        class_ids
            .iter()
            .map(|&id| ctx.class(id).name.0.clone())
            .collect::<Vec<_>>()
    );

    // The spec should have its sole field `value: int` resolved
    // through the canonical-type path.
    let class = ctx.class(*box_int.unwrap());
    assert_eq!(
        class.fields.len(),
        1,
        "expected one field (value), got {}",
        class.fields.len()
    );
    match ctx.type_of(class.fields[0].ty) {
        CxxType::Int { signed: true, width: IntWidth::I32 } => {}
        other => panic!("expected i32 (int) field, got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn forward_only_class_referenced_via_pointer_becomes_poison_node() {
    // M9: when a field references a forward-declared class whose
    // definition we never see, the importer should mint a poison
    // node rather than aborting the whole import. Downstream
    // emission renders the poisoned class as an opaque struct.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "class Foo;\n\
         struct Bar { Foo* f; };\n",
        "poison_forward_decl",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    // Bar is the only TU-level definition; `import_header`'s root
    // walk surfaces just it. Foo (forward-only) is minted as a
    // poison node by `import_type` when resolving Bar's `Foo*`
    // field, so it lives in `ctx` but not in the returned id
    // list.
    let _root_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import shouldn't abort just because Foo is forward-only");

    // Walk every class in the ctx (including poison nodes
    // promoted from forward-only references via `import_type`,
    // which `class_ids` doesn't surface because they're not
    // root-walked TU-level definitions). Find Foo and Bar.
    let mut foo: Option<rustc_abi_cxx::ClassId> = None;
    let mut bar: Option<rustc_abi_cxx::ClassId> = None;
    for id in ctx.class_ids() {
        let class = ctx.class(id);
        match class.name.0.last() {
            Some(NameSegment::Class(i)) if i.0 == "Foo" => foo = Some(id),
            Some(NameSegment::Class(i)) if i.0 == "Bar" => bar = Some(id),
            _ => {}
        }
    }
    let foo = foo.expect("Foo (forward-only) should be a poison node in the ctx");
    let bar = bar.expect("Bar should be imported normally");

    assert!(
        ctx.is_poisoned(foo),
        "Foo should be marked as a poison node"
    );
    assert!(
        !ctx.is_poisoned(bar),
        "Bar should not be poisoned (it has a full definition)"
    );
    let reason = ctx.poison_reason(foo).expect("poison reason set");
    assert!(
        reason.contains("forward-declared"),
        "expected `forward-declared` in poison reason, got: {reason:?}"
    );

    // Bindings emit Foo as opaque + doc-comment, Bar as concrete.
    // Pass both ids explicitly since `class_ids` only carries Bar.
    let src = cxx_importer::rust_bindings::generate_rust_bindings(
        &ctx,
        &[foo, bar],
        &cxx_importer::rust_bindings::RustBindingsConfig::default(),
    )
    .expect("emit");
    assert!(
        src.contains("/// (Class poisoned by `cxx_importer`"),
        "expected poison marker doc comment:\n{src}"
    );
    assert!(
        src.contains("pub struct Foo "),
        "expected opaque Foo struct in emission:\n{src}"
    );
    assert!(
        src.contains("pub struct Bar"),
        "expected concrete Bar struct:\n{src}"
    );

    cleanup(&header);
}

#[test]
fn forward_decl_then_full_def_in_same_tu_upgrades_poison_to_concrete() {
    // M13: when a class is forward-declared early in a TU and
    // fully defined later (or in a sibling header that gets
    // included), the importer's USR cache surfaces the previously-
    // minted poison node, but the upgrade path replaces the
    // placeholder ClassDef with the real one and clears the
    // poison marker.
    //
    // Common shape in real headers (FLTK, Qt): class Foo;
    // declared in a fwd-decls header, struct Bar with `Foo*`
    // fields in a second header, then class Foo's full body in
    // a third — all transitively included from a single TU root.
    //
    // We model this by writing a single header that does both:
    // forward-declare Foo on line 1, define struct Bar
    // referencing Foo* on line 2, then provide Foo's full
    // definition on line 3. The importer sees Foo as a forward
    // ref while resolving Bar's field, then sees the full body
    // when walk_top_level reaches it. Both arrive in one
    // import_header_with_cache call.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "class Foo;\n\
         struct Bar { Foo* f; };\n\
         class Foo { public: int compute() const; };\n",
        "fwd_then_def",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let _ = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Find Foo (which should now be a healthy concrete class
    // with its method, NOT a poison node).
    let foo = ctx
        .class_ids()
        .find(|&id| {
            matches!(
                ctx.class(id).name.0.last(),
                Some(NameSegment::Class(i)) if i.0 == "Foo",
            )
        })
        .expect("Foo should be in the ctx");

    assert!(
        !ctx.is_poisoned(foo),
        "Foo should NOT be poisoned after seeing its full definition; \
         poison_reason: {:?}",
        ctx.poison_reason(foo)
    );
    let class = ctx.class(foo);
    assert_eq!(
        class.methods.len(),
        1,
        "expected `compute()` method on the upgraded Foo"
    );
    assert_eq!(
        class.methods[0].name.ident_name(),
        Some("compute"),
    );

    cleanup(&header);
}

#[test]
fn m14_heap_shim_and_new_boxed_wrapper_pair_through_full_pipeline() {
    // M14: import a real C++ class with a ctor + dtor; the shim
    // generator emits `__cxx_<class>_new_heap_<i>` and
    // `__cxx_<class>_delete` thunks; the bindings emitter pairs
    // them with `pub fn new_boxed(...) -> ::cxx::CxxHeap<Self>`
    // and `unsafe impl ::cxx::CxxDeletable for <class>`.
    //
    // The shim source must compile cleanly with clang++ (proves
    // the C++ syntax is right). The bindings source needs to
    // contain both halves of the pairing so consumers can route
    // heap allocation through `CxxHeap`.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Calc {\n\
         \x20   Calc(int a, int b);\n\
         \x20   ~Calc();\n\
         \x20   int sum() const;\n\
         private:\n\
         \x20   int a_;\n\
         \x20   int b_;\n\
         };\n",
        "m14_heap",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Shim source compiles with clang++ (proves C++ syntax).
    let driver = cxx_importer::Driver::new(cxx_importer::HeaderGraph {
        roots: vec![header.clone()],
        clang_flags: vec!["-std=c++17".into()],
        ..cxx_importer::HeaderGraph::default()
    });
    let shim_src = driver
        .emit_shims(&ctx, &class_ids)
        .expect("emit_shims");
    assert!(
        shim_src.contains("__cxx_Calc_new_heap_0("),
        "expected heap-ctor thunk in shim source:\n{shim_src}"
    );
    assert!(
        shim_src.contains("__cxx_Calc_delete("),
        "expected delete thunk in shim source:\n{shim_src}"
    );
    assert!(
        shim_src.contains("return new Calc("),
        "expected `new Calc(...)` body:\n{shim_src}"
    );
    assert!(
        shim_src.contains("delete p"),
        "expected `delete p` body:\n{shim_src}"
    );

    // Compile the shim source through clang++ to verify it's
    // syntactically valid C++ that links against the user's
    // header.
    let dir = std::env::temp_dir().join(format!(
        "rustcc_m14_shim_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shim_cpp = dir.join("shims.cpp");
    let shim_obj = dir.join("shims.o");
    // The shim sources include the user's header by relative
    // path; copy our temp header into the shim dir so the include
    // resolves.
    std::fs::write(&shim_cpp, &shim_src).unwrap();
    let header_in_dir = dir.join(
        header
            .file_name()
            .expect("header has filename"),
    );
    std::fs::copy(&header, &header_in_dir).unwrap();
    // Update the shim source's `#include "<absolute path>"` to
    // resolve against the directory we just created. The driver
    // emits an absolute path, so this just works.
    let cpp_compile = std::process::Command::new("clang++")
        .args(["-c", "-std=c++17", "-fPIC"])
        .arg("-o")
        .arg(&shim_obj)
        .arg(&shim_cpp)
        .output()
        .expect("spawn clang++");
    assert!(
        cpp_compile.status.success(),
        "clang++ shim compile failed:\nshim source:\n{shim_src}\nstderr:\n{}",
        String::from_utf8_lossy(&cpp_compile.stderr),
    );

    // Bindings emission contains both halves (with the M14 opt-in).
    let bindings_src = generate_rust_bindings(
        &ctx,
        &class_ids,
        &RustBindingsConfig {
            emit_heap_alloc: true,
            ..RustBindingsConfig::default()
        },
    )
    .expect("emit_rust_bindings");
    assert!(
        bindings_src.contains("fn __cxx_Calc_new_heap_0(arg0: i32, arg1: i32) -> *mut Calc;"),
        "expected heap-ctor extern decl in bindings:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("fn __cxx_Calc_delete(p: *mut Calc);"),
        "expected delete extern decl in bindings:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("pub fn new_boxed(arg0: i32, arg1: i32) -> ::cxx::CxxHeap<Self>"),
        "expected `new_boxed` wrapper:\n{bindings_src}"
    );
    assert!(
        bindings_src.contains("unsafe impl ::cxx::CxxDeletable for Calc"),
        "expected CxxDeletable impl:\n{bindings_src}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    cleanup(&header);
}

#[test]
fn m12_collect_macros_captures_object_like_define_constants() {
    // M12: enable libclang's `detailed_preprocessing_record`,
    // walk `MacroDefinition` cursors, evaluate object-like
    // macros, and skip function-like ones / system-style names.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "#define FL_RED 88\n\
         #define FL_BOLD 1\n\
         #define FL_PI 3.14\n\
         #define FL_NAME \"widget\"\n\
         #define MIN(a,b) ((a) < (b) ? (a) : (b))\n",
        "m12_macros",
    );
    let macros = cxx_importer::macros::collect_macros(
        &header,
        &["-x", "c++", "-std=c++17"],
    )
    .expect("collect_macros");

    use cxx_importer::macros::MacroValue;
    assert_eq!(
        macros.get("FL_RED").map(|m| m.value.clone()),
        Some(MacroValue::SignedInteger(88))
    );
    assert_eq!(
        macros.get("FL_BOLD").map(|m| m.value.clone()),
        Some(MacroValue::SignedInteger(1))
    );
    let pi = macros.get("FL_PI").map(|m| m.value.clone());
    match pi {
        Some(MacroValue::Float(v)) => assert!((v - 3.14).abs() < 1e-9),
        other => panic!("expected float for FL_PI, got {other:?}"),
    }
    assert_eq!(
        macros.get("FL_NAME").map(|m| m.value.clone()),
        Some(MacroValue::String("widget".into()))
    );
    // Function-like macros are skipped.
    assert!(
        macros.get("MIN").is_none(),
        "function-like MIN should be skipped"
    );

    cleanup(&header);
}

#[test]
fn m11_static_methods_get_marked_and_emit_receiver_less_wrappers() {
    // M11: `class Fl { static int run(); int wait(); };`. The
    // importer should mark `run` as static (no receiver) but
    // leave `wait` as a normal instance method.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "class Fl { public:\n\
         \x20   static int run();\n\
         \x20   int wait() const;\n\
         };\n",
        "m11_static",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let fl = class_ids[0];

    let methods: Vec<_> = ctx.class(fl).methods.clone();
    let run_idx = methods
        .iter()
        .position(|m| m.name.ident_name() == Some("run"))
        .expect("run method");
    let wait_idx = methods
        .iter()
        .position(|m| m.name.ident_name() == Some("wait"))
        .expect("wait method");

    assert!(
        ctx.is_method_static(fl, run_idx),
        "expected `run` to be marked static"
    );
    assert!(
        !ctx.is_method_static(fl, wait_idx),
        "expected `wait` to NOT be marked static"
    );

    // Bindings emit receiver-less wrapper for run, normal for wait.
    let src = cxx_importer::rust_bindings::generate_rust_bindings(
        &ctx,
        &class_ids,
        &cxx_importer::rust_bindings::RustBindingsConfig::default(),
    )
    .expect("emit");
    assert!(
        src.contains("pub fn run() -> i32"),
        "expected static `run()` wrapper:\n{src}"
    );
    assert!(
        src.contains("pub fn wait(&self) -> i32"),
        "expected instance `wait(&self)` wrapper:\n{src}"
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

// ============================================================
// M17: type aliases (typedef / using).
// ============================================================

#[test]
fn m17_captures_typedef_to_primitive_at_tu_scope() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "typedef int MyInt;\n\
         struct Owner { int slot; };\n",
        "m17_typedef_primitive",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "MyInt")
        .expect("MyInt typedef captured");
    assert!(
        alias.parent.is_empty(),
        "TU-scope alias should have empty parent path"
    );
    assert!(matches!(
        ctx.type_of(alias.target),
        CxxType::Int { signed: true, width: IntWidth::I32 },
    ));

    cleanup(&header);
}

#[test]
fn m17_captures_using_alias_at_tu_scope() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "using Real = double;\n\
         struct Foo { Real r; };\n",
        "m17_using_primitive",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "Real")
        .expect("Real using-alias captured");
    assert!(matches!(
        ctx.type_of(alias.target),
        CxxType::Float { kind: rustc_abi_cxx::FloatKind::F64 },
    ));

    cleanup(&header);
}

#[test]
fn m17_captures_alias_to_user_class() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Inner { int v; };\n\
         using InnerAlias = Inner;\n",
        "m17_alias_to_class",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let inner_id = *classes
        .iter()
        .find(|&&id| {
            let c = ctx.class(id);
            matches!(
                c.name.0.last(),
                Some(NameSegment::Class(id) | NameSegment::Namespace(id))
                    if id.0 == "Inner"
            )
        })
        .expect("Inner imported");

    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "InnerAlias")
        .expect("InnerAlias captured");
    match ctx.type_of(alias.target) {
        CxxType::Record(cid) => {
            assert_eq!(*cid, inner_id, "alias target should resolve to Inner");
        }
        other => panic!("expected Record(Inner), got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn m17_captures_namespace_nested_alias_with_parent_path() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "namespace ns {\n\
           namespace inner {\n\
             using Code = unsigned;\n\
           }\n\
         }\n",
        "m17_ns_nested_alias",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "Code")
        .expect("Code captured");

    // Outer-to-inner ordering on `alias.parent`.
    let parent_names: Vec<&str> = alias
        .parent
        .iter()
        .map(|seg| match seg {
            NameSegment::Namespace(id) => id.0.as_str(),
            NameSegment::AnonymousNamespace => "<anon>",
            _ => "<other>",
        })
        .collect();
    assert_eq!(parent_names, vec!["ns", "inner"]);

    cleanup(&header);
}

#[test]
fn m17_alias_chain_resolves_to_canonical_target() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // `Final` -> `Mid` -> `int`. Since import_type strips
    // typedef sugar via canonical(), we expect every alias's
    // target to be the same primitive `int` TypeId.
    let header = temp_header(
        "typedef int Mid;\n\
         typedef Mid Final;\n",
        "m17_alias_chain",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let mid = extras.aliases.iter().find(|a| a.name.0 == "Mid").unwrap();
    let final_ = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "Final")
        .unwrap();
    assert_eq!(
        mid.target, final_.target,
        "both aliases should resolve to the same canonical int TypeId",
    );
    assert!(matches!(
        ctx.type_of(mid.target),
        CxxType::Int { signed: true, width: IntWidth::I32 },
    ));

    cleanup(&header);
}

#[test]
fn m17_aliases_emit_pub_type_lines_in_bindings() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings_with_extras, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "typedef int MyInt;\n\
         namespace ns {\n\
           using Real = double;\n\
         }\n\
         struct Owner { int v; };\n",
        "m17_emit_pub_type",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_with_extras(
        &ctx,
        &classes,
        &cxx_importer::AnnotationSet::default(),
        &extras.aliases,
        &extras.enums,
        &cfg,
    )
    .expect("emit");

    assert!(
        src.contains("pub type MyInt = i32;"),
        "TU-scope MyInt alias should emit at top level; got:\n{src}",
    );
    assert!(
        src.contains("pub mod ns {") && src.contains("pub type Real = f64;"),
        "namespace-scope Real alias should emit inside `pub mod ns`; got:\n{src}",
    );
    cleanup(&header);
}

#[test]
fn m17_alias_to_unsupported_type_is_skipped_not_fatal() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // Member-pointer types aren't yet supported by `import_type`.
    // The alias should silently drop instead of aborting.
    // (Function pointers WERE the canary here pre-M15; M15 lifted
    // that, so we use a shape M15 also doesn't cover yet.)
    let header = temp_header(
        "struct Holder { int field; };\n\
         using MemPtr = int Holder::*;\n",
        "m17_alias_unsupported_target",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import should not fail because of unsupported alias target");

    // The alias must not appear (target type is unsupported in v0).
    assert!(
        extras.aliases.iter().all(|a| a.name.0 != "MemPtr"),
        "alias to member-pointer type should be silently dropped; got: {:?}",
        extras.aliases.iter().map(|a| &a.name.0).collect::<Vec<_>>(),
    );
    // The class still imports.
    assert!(
        !classes.is_empty(),
        "imports should still produce the Holder class",
    );

    cleanup(&header);
}

// ============================================================
// M20: configurable `const char*` → `*const c_char` ergonomics.
// Default-off (preserves prior emission); opt-in via
// `RustBindingsConfig::cstr_ergonomics`.
// ============================================================

#[test]
fn m20_default_emission_keeps_pointer_to_i8_for_const_char() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct W {\n  void label(const char* s);\n};\n",
        "m20_default_off",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("arg0: *const i8") || src.contains("arg0: *const u8"),
        "default-off should preserve the i8/u8 pointer rendering; got:\n{src}",
    );
    assert!(
        !src.contains("c_char"),
        "default-off should not reference c_char; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m20_opt_in_renders_char_ptr_as_c_char() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct W {\n  void label(const char* s);\n  const char* name();\n};\n",
        "m20_opt_in_on",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        cstr_ergonomics: true,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("arg0: *const ::core::ffi::c_char"),
        "param should render as `*const c_char`; got:\n{src}",
    );
    assert!(
        src.contains("-> *const ::core::ffi::c_char"),
        "return should render as `*const c_char`; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m20_opt_in_does_not_touch_non_byte_pointers() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct W {\n  void take_int_ptr(const int* p);\n  void take_widget(W* p);\n};\n",
        "m20_other_pointers_untouched",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        cstr_ergonomics: true,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("*const i32") || src.contains("arg0: *const i32"),
        "int pointers should keep i32 rendering; got:\n{src}",
    );
    assert!(
        src.contains("*mut W"),
        "record pointers should keep their record name; got:\n{src}",
    );

    cleanup(&header);
}

// ============================================================
// M18: default-argument detection (count-only v0). Per-arity
// convenience wrappers are tracked as M18.b.
// ============================================================

#[test]
fn m18_records_trailing_default_arg_count_per_method() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // - `redraw(int)` has zero defaults.
    // - `set(int, int = 1)` has one trailing default.
    // - `paint(int = 0, int = 0)` has two trailing defaults.
    // - `mid(int, int = 5, int)` is illegal C++ — defaults
    //   must occupy a contiguous tail. The compiler rejects
    //   it, so we don't try to test that path.
    let header = temp_header(
        "struct W {\n  void redraw(int delay);\n  void set(int a, int b = 1);\n  void paint(int x = 0, int y = 0);\n};\n",
        "m18_default_arg_counts",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let class_id = class_ids[0];

    // Look up methods by source name.
    let class = ctx.class(class_id);
    let idx_of = |name: &str| {
        class
            .methods
            .iter()
            .position(|m| m.name.ident_name() == Some(name))
            .unwrap_or_else(|| panic!("method `{name}` not found"))
    };

    assert_eq!(ctx.default_arg_count(class_id, idx_of("redraw")), 0);
    assert_eq!(ctx.default_arg_count(class_id, idx_of("set")), 1);
    assert_eq!(ctx.default_arg_count(class_id, idx_of("paint")), 2);

    cleanup(&header);
}

#[test]
fn m18_emits_doc_comment_when_method_has_default_args() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct W {\n  void paint(int x = 0, int y = 0);\n  void plain(int z);\n};\n",
        "m18_emits_doc",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    // The two-default `paint` method gets a doc comment hint.
    assert!(
        src.contains("trailing 2 parameters") && src.contains("M18"),
        "paint should carry M18 doc comment; got:\n{src}",
    );
    // The plain method (no defaults) does not — only one method
    // should carry the M18 hint in this header.
    let m18_count = src.matches("(M18 v0:").count();
    assert_eq!(
        m18_count, 1,
        "exactly one method should carry the M18 hint; got {m18_count} in:\n{src}",
    );

    cleanup(&header);
}

// ============================================================
// M15: function pointer types — `void (*)(int)` lowering and
// emission. Closure-as-callback `CxxCallback<F>` runtime helper
// lives in `crates/cxx/src/callback.rs` and is exercised by its
// own unit tests.
// ============================================================

#[test]
fn m15_lowers_function_pointer_alias_to_fn_type() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "using SignalHandler = void(*)(int);\n\
         struct Owner { int v; };\n",
        "m15_fnptr_alias",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "SignalHandler")
        .expect("SignalHandler alias captured");
    match ctx.type_of(alias.target) {
        CxxType::Fn(sig) => {
            assert_eq!(sig.params.len(), 1);
            assert!(matches!(
                ctx.type_of(sig.params[0]),
                CxxType::Int { signed: true, width: IntWidth::I32 },
            ));
            assert!(matches!(ctx.type_of(sig.ret), CxxType::Void));
            assert!(!sig.variadic);
        }
        other => panic!("expected Fn, got {other:?}"),
    }

    cleanup(&header);
}

#[test]
fn m15_lowers_bare_function_type_alias_to_fn_type() {
    // `using F = void(int);` — the alias target is a bare
    // `FunctionPrototype`, not a pointer-to-function. We
    // collapse to `CxxType::Fn` regardless so the renderer
    // can produce `extern "C" fn(...)`.
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "using F = void(int);\n",
        "m15_bare_fn_alias",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let alias = extras
        .aliases
        .iter()
        .find(|a| a.name.0 == "F")
        .expect("F alias captured");
    assert!(matches!(ctx.type_of(alias.target), CxxType::Fn(_)));
    cleanup(&header);
}

#[test]
fn m15_renders_function_pointer_as_extern_c_fn_in_alias() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings_with_extras, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "using IntCallback = int(*)(int, int);\n\
         using VoidCallback = void(*)();\n",
        "m15_fnptr_emit",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_with_extras(
        &ctx,
        &classes,
        &cxx_importer::AnnotationSet::default(),
        &extras.aliases,
        &extras.enums,
        &cfg,
    )
    .expect("emit");

    assert!(
        src.contains(
            "pub type IntCallback = Option<unsafe extern \"C\" fn(i32, i32) -> i32>"
        ),
        "non-void return should render with `-> ret`; got:\n{src}",
    );
    assert!(
        src.contains("pub type VoidCallback = Option<unsafe extern \"C\" fn()>"),
        "void return should render without `-> ()`; got:\n{src}",
    );

    cleanup(&header);
}

// ============================================================
// M19: CxxBase<T> upcast emission for non-virtual inheritance.
// ============================================================
// Each derived class gets one `impl ::cxx::CxxBase<Base> for
// Derived` per non-virtual base, with the offset baked in
// using the layout engine's `base_offsets` table. Single
// inheritance with offset 0 elides the `add(0)` for clarity;
// non-zero offsets (multi-inheritance) keep the explicit
// pointer arithmetic.

#[test]
fn m19_emits_upcast_impl_for_single_non_virtual_base() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Base { int a; };\n\
         struct Derived : public Base { int b; };\n",
        "m19_single_inheritance",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("impl ::cxx::CxxBase<Base> for Derived"),
        "Derived should impl CxxBase<Base>; got:\n{src}",
    );
    assert!(
        src.contains("fn upcast(&self) -> &Base"),
        "upcast signature missing; got:\n{src}",
    );
    assert!(
        src.contains("fn upcast_mut(&mut self) -> &mut Base"),
        "upcast_mut signature missing; got:\n{src}",
    );
    // Offset-0 path elides .add(0).
    assert!(
        !src.contains(".add(0)"),
        "offset-0 upcast should elide `.add(0)` for readability; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m19_emits_upcast_impl_per_non_virtual_base_in_multi_inheritance() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct A { int x; };\n\
         struct B { int y; };\n\
         struct C : public A, public B { int z; };\n",
        "m19_multi_inheritance",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("impl ::cxx::CxxBase<A> for C"),
        "C should impl CxxBase<A>; got:\n{src}",
    );
    assert!(
        src.contains("impl ::cxx::CxxBase<B> for C"),
        "C should impl CxxBase<B>; got:\n{src}",
    );
    // The B base sits past A in the layout — non-zero offset.
    assert!(
        src.contains(".add(4)") || src.contains(".add(8)"),
        "second base should use a pointer-add for its non-zero offset; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m19_skips_upcast_for_virtual_base() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // Virtual inheritance — base offset is dynamic via vtable.
    // M19 v0 skips these; M22 picks them up.
    let header = temp_header(
        "struct Base { int a; };\n\
         struct Derived : public virtual Base { int b; };\n",
        "m19_virtual_base_skipped",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        !src.contains("impl ::cxx::CxxBase<Base> for Derived"),
        "virtual-base upcast should be skipped in M19 v0; got:\n{src}",
    );
}

// ============================================================
// M21: bitfield-aware layout — probe-first behavior.
// ============================================================
//
// `rustc_abi_cxx::layout` doesn't model Itanium bitfield packing,
// so a class with bit-packed members would compute a wrong size
// and silently mismatch the C++ side at runtime. Until proper
// support lands, the importer poisons any class with bitfields
// so emission produces an opaque `pub struct` + clear doc-
// comment reason instead of a layout that looks fine but
// corrupts data.

#[test]
fn m21_bitfield_class_is_poisoned_with_clear_reason() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct PackedFlags {\n  unsigned a : 4;\n  unsigned b : 4;\n  unsigned c : 8;\n};\n",
        "m21_bitfield_poisoned",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");
    assert_eq!(class_ids.len(), 1);
    let id = class_ids[0];

    assert!(
        ctx.is_poisoned(id),
        "bitfield-bearing class should be poisoned (M21 v0)",
    );
    let reason = ctx.poison_reason(id).expect("reason recorded");
    assert!(
        reason.contains("bitfield"),
        "poison reason should mention bitfield; got: {reason}",
    );
    assert!(
        reason.contains("M21"),
        "poison reason should reference the milestone; got: {reason}",
    );
    cleanup(&header);
}

#[test]
fn m21_bitfield_class_emits_opaque_struct_with_doc_comment() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct PackedFlags {\n  unsigned a : 4;\n  unsigned b : 4;\n};\n",
        "m21_bitfield_emits_opaque",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    assert!(
        src.contains("bitfield member") && src.contains("M21"),
        "opaque struct should include the bitfield-poison reason; got:\n{src}",
    );
    assert!(
        src.contains("pub struct PackedFlags"),
        "opaque PackedFlags struct should still emit; got:\n{src}",
    );
    cleanup(&header);
}

// ============================================================
// M16: enum class + plain enum body lowering.
// ============================================================

#[test]
fn m16_captures_scoped_enum_with_unique_discriminants() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "enum class Color : int { Red = 1, Green = 2, Blue = 3 };\n",
        "m16_scoped_enum_unique",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let e = extras
        .enums
        .iter()
        .find(|e| e.name.0 == "Color")
        .expect("Color enum captured");
    assert!(e.scoped, "enum class should be scoped");
    assert_eq!(e.variants.len(), 3);
    assert_eq!(e.variants[0].name, "Red");
    assert_eq!(e.variants[0].value, 1);
    assert_eq!(e.variants[1].name, "Green");
    assert_eq!(e.variants[1].value, 2);
    assert_eq!(e.variants[2].name, "Blue");
    assert_eq!(e.variants[2].value, 3);
    assert!(matches!(
        ctx.type_of(e.underlying),
        CxxType::Int { signed: true, width: IntWidth::I32 },
    ));

    cleanup(&header);
}

#[test]
fn m16_captures_unscoped_enum() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "enum Mode { Off, On, Auto };\n",
        "m16_unscoped_enum",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let e = extras
        .enums
        .iter()
        .find(|e| e.name.0 == "Mode")
        .expect("Mode enum captured");
    assert!(!e.scoped, "plain enum should be unscoped");
    let names: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["Off", "On", "Auto"]);
    let values: Vec<i64> = e.variants.iter().map(|v| v.value).collect();
    assert_eq!(values, vec![0, 1, 2]);

    cleanup(&header);
}

#[test]
fn m16_captures_namespace_nested_enum() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "namespace gfx {\n\
           enum class Boxtype : unsigned char { None = 0, Up = 1, Down = 2 };\n\
         }\n",
        "m16_ns_nested_enum",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let e = extras
        .enums
        .iter()
        .find(|e| e.name.0 == "Boxtype")
        .expect("Boxtype enum captured");
    let parent_names: Vec<&str> = e
        .parent
        .iter()
        .map(|seg| match seg {
            NameSegment::Namespace(id) => id.0.as_str(),
            _ => "<other>",
        })
        .collect();
    assert_eq!(parent_names, vec!["gfx"]);
    assert!(e.scoped);
    assert_eq!(e.variants.len(), 3);
    // Underlying type should be unsigned 8-bit.
    assert!(matches!(
        ctx.type_of(e.underlying),
        CxxType::Int { signed: false, width: IntWidth::I8 },
    ));

    cleanup(&header);
}

#[test]
fn m16_emits_pub_enum_for_scoped_unique() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings_with_extras, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "enum class Color : int { Red = 1, Green = 2, Blue = 3 };\n",
        "m16_emit_pub_enum",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_with_extras(
        &ctx,
        &classes,
        &cxx_importer::AnnotationSet::default(),
        &extras.aliases,
        &extras.enums,
        &cfg,
    )
    .expect("emit");

    assert!(
        src.contains("#[repr(i32)]") && src.contains("pub enum Color"),
        "scoped+unique enum should emit as `#[repr(i32)] pub enum Color`; got:\n{src}",
    );
    assert!(
        src.contains("Red = 1") && src.contains("Green = 2") && src.contains("Blue = 3"),
        "all three variants should appear; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m16_emits_struct_with_consts_for_unscoped() {
    use cxx_importer::rust_bindings::{
        generate_rust_bindings_with_extras, BindingsBackend, RustBindingsConfig,
    };
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        // unscoped + non-unique forces the struct shape.
        "enum Flags : unsigned int { F_NONE = 0, F_A = 1, F_ALSO_A = 1, F_B = 2 };\n",
        "m16_emit_struct_consts",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings_with_extras(
        &ctx,
        &classes,
        &cxx_importer::AnnotationSet::default(),
        &extras.aliases,
        &extras.enums,
        &cfg,
    )
    .expect("emit");

    assert!(
        src.contains("#[repr(transparent)]")
            && src.contains("pub struct Flags(pub u32)"),
        "unscoped/aliasing enum should emit as transparent struct; got:\n{src}",
    );
    assert!(
        src.contains("pub const F_A: Self = Self(1)")
            && src.contains("pub const F_ALSO_A: Self = Self(1)")
            && src.contains("pub const F_B: Self = Self(2)"),
        "associated consts should be present including the aliasing pair; got:\n{src}",
    );

    cleanup(&header);
}

#[test]
fn m16_class_scope_enum_is_skipped_in_v0() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "struct Outer {\n  enum class Mode { A, B };\n  int slot;\n};\n",
        "m16_class_scope_enum_skipped",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert!(
        extras.enums.iter().all(|e| e.name.0 != "Mode"),
        "class-scope enum should not appear at TU scope; got: {:?}",
        extras.enums.iter().map(|e| &e.name.0).collect::<Vec<_>>(),
    );
}

#[test]
fn m16_anonymous_enum_is_skipped_in_v0() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp_header(
        "enum { GLOBAL_X = 7 };\n\
         struct Owner { int slot; };\n",
        "m16_anon_enum_skipped",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Anonymous enums have no name to dedup on; v0 drops them.
    assert!(
        extras.enums.iter().all(|e| !e.name.0.is_empty()),
        "anonymous enum should not appear in EnumSet",
    );
    cleanup(&header);
}

#[test]
fn m17_class_scope_typedef_is_skipped_in_v0() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    // In-class aliases require associated-type emission we
    // don't have yet (deferred per docs/cxx_importer.md §16).
    // The walker filters them out — verify they're absent
    // from the AliasSet.
    let header = temp_header(
        "struct Foo {\n  using It = int;\n  int slot;\n};\n",
        "m17_class_scope_typedef",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let (_classes, extras) = import_header_with_extras(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    assert!(
        extras.aliases.iter().all(|a| a.name.0 != "It"),
        "class-scope `using It = int;` should not appear at TU scope; got: {:?}",
        extras.aliases.iter().map(|a| &a.name.0).collect::<Vec<_>>(),
    );
    // Sanity: AliasSet may be empty entirely.
    let _ = AliasSet::default();
}
