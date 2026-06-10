//! Conformance tests for `rustc_abi_cxx::layout` against Clang.
//!
//! For each corpus entry under `tests/corpus/`, this file has two tests:
//!
//! - `<name>_golden_is_valid`: loads the golden and checks the shape
//!   our harness depends on. Runs today.
//! - `<name>_layout_matches_clang`: builds a `ClassDef` matching the
//!   corpus `.cpp`, calls `ctx.layout(class_id)`, and diffs the result
//!   against the golden. `#[ignore]`-d until `layout()` is implemented
//!   (docs/rustc_abi_cxx.md §5). Removing `#[ignore]` is the ship bar
//!   for each milestone that grows the algorithm's coverage.

use std::path::PathBuf;

use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, CxxTypeCtx, FieldDef,
    FnSig, Ident, IntWidth, MethodDef, MethodName, NameSegment, NestedName,
    RecordKind, RecordLayout, RefKind, SpecialMember, Target, TypeId,
    Virtuality,
};
use test_support::golden::{self, BaseDump, Dump, FieldDump};

// -------- Harness helpers -----------------------------------------------

fn corpus_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus")
        .join(filename)
}

fn load_golden(basename: &str) -> Dump {
    let path = corpus_path(&format!("{basename}.layout.golden"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read {} ({e}). Run `cargo xtask refresh-goldens` to regenerate.",
            path.display()
        )
    });
    golden::parse(&text).unwrap_or_else(|e| {
        panic!(
            "failed to parse {}: {e}. Run `cargo xtask refresh-goldens`.",
            path.display()
        )
    })
}

fn target_from_golden(triple: &str) -> Option<Target> {
    if triple.starts_with("x86_64-apple-darwin") {
        Some(Target::x86_64_apple_darwin())
    } else if triple.starts_with("x86_64-unknown-linux-gnu") {
        Some(Target::x86_64_unknown_linux_gnu())
    } else if triple.starts_with("aarch64-apple-darwin") {
        Some(Target::aarch64_apple_darwin())
    } else if triple.starts_with("aarch64-unknown-linux-gnu") {
        Some(Target::aarch64_unknown_linux_gnu())
    } else {
        None
    }
}

fn last_segment_name(n: &NestedName) -> String {
    match n.0.last() {
        Some(NameSegment::Class(i))
        | Some(NameSegment::Namespace(i))
        | Some(NameSegment::Enum(i)) => i.0.clone(),
        Some(NameSegment::TemplateSpec { name, .. }) => name.0.clone(),
        Some(NameSegment::AnonymousNamespace) => String::from("(anon)"),
        None => String::from("(unnamed)"),
    }
}

fn layout_to_dump(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
    layout: &RecordLayout,
    target: &str,
) -> Dump {
    let class = ctx.class(class_id);
    let fields = class
        .fields
        .iter()
        .enumerate()
        .zip(layout.field_offsets.iter())
        .map(|((idx, fd), off)| {
            // v1.13.1: emit bit-field info when the layout marked
            // this field as a bit-field (non-zero width). Bit
            // offset is within the byte at `field_offsets[idx]`.
            let bits = if layout.field_bit_widths.get(idx).copied().unwrap_or(0) != 0 {
                Some((
                    layout.field_bit_offsets[idx],
                    layout.field_bit_widths[idx],
                ))
            } else {
                None
            };
            FieldDump {
                name: fd.name.0.clone(),
                offset: *off,
                bits,
            }
        })
        .collect();
    let bases = layout
        .base_offsets
        .iter()
        .map(|(bid, off)| BaseDump {
            class: last_segment_name(&ctx.class(*bid).name),
            offset: *off,
        })
        .collect();
    Dump {
        target: target.to_string(),
        class: last_segment_name(&class.name),
        sizeof: layout.size_bytes,
        dsize: layout.data_size_bytes,
        align: layout.align_bytes,
        nvsize: layout.nv_size_bytes,
        nvalign: layout.nv_align_bytes,
        has_vptr: layout.has_vptr,
        bases,
        fields,
    }
}

// -------- Corpus builders -----------------------------------------------
//
// Each builder constructs the same classes the matching `.cpp` declares
// and returns the ClassId of the target class named in the file's
// `// @target` directive.

fn int_ty(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    })
}

fn char_ty(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I8,
    })
}

fn uint_ty(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: false,
        width: IntWidth::I32,
    })
}

fn void_ty(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Void)
}

fn nested(parts: &[&str]) -> NestedName {
    NestedName(
        parts
            .iter()
            .map(|s| NameSegment::Class(Ident((*s).to_string())))
            .collect(),
    )
}

// v1.13.1-A: Itanium bit-field packing. Mirrors corpus/bitfield.cpp —
// `unsigned int a:4, b:20, c:8` + `char tail`. After defining the
// class we record each bit-field's declared width via
// `record_bitfield_width` (the importer does this from libclang in
// the real pipeline).
fn build_bitfield(ctx: &mut CxxTypeCtx) -> ClassId {
    let uint_ = uint_ty(ctx);
    let char_ = char_ty(ctx);
    let id = ctx.define_class(ClassDef {
        name: nested(&["BF"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef { name: Ident(String::from("a")), ty: uint_, explicit_align: None },
            FieldDef { name: Ident(String::from("b")), ty: uint_, explicit_align: None },
            FieldDef { name: Ident(String::from("c")), ty: uint_, explicit_align: None },
            FieldDef { name: Ident(String::from("tail")), ty: char_, explicit_align: None },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    ctx.record_bitfield_width(id, 0, 4); // a:4
    ctx.record_bitfield_width(id, 1, 20); // b:20
    ctx.record_bitfield_width(id, 2, 8); // c:8
    // `tail` (idx 3) is a regular field — no bitfield width recorded.
    id
}

// v1.13.1-B: Itanium `__attribute__((packed))`. Mirrors
// corpus/packed.cpp — `char a; int b; char c;` with the packed
// attribute, which forces 1-byte field alignment (no padding
// before `int b`). We mark the class packed via `set_packed`.
fn build_packed(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    let id = ctx.define_class(ClassDef {
        name: nested(&["P"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef { name: Ident(String::from("a")), ty: char_, explicit_align: None },
            FieldDef { name: Ident(String::from("b")), ty: int_, explicit_align: None },
            FieldDef { name: Ident(String::from("c")), ty: char_, explicit_align: None },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    ctx.set_packed(id);
    id
}

fn build_pod_scalar(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Foo"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("x")),
                ty: int_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("y")),
                ty: char_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_single_inherit(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    let void_ = void_ty(ctx);
    let base = ctx.define_class(ClassDef {
        name: nested(&["Base"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("x")),
                ty: int_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("y")),
                ty: char_,
                explicit_align: None,
            },
        ],
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident(String::from("~Base"))),
            sig: FnSig {
                params: Vec::new(),
                ret: void_,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: Some(SpecialMember::Dtor),
        }],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    ctx.define_class(ClassDef {
        name: nested(&["Derived"]),
        bases: vec![BaseSpec {
            class: base,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![
            FieldDef {
                name: Ident(String::from("z")),
                ty: char_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("w")),
                ty: int_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_empty_base(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let empty = ctx.define_class(ClassDef {
        name: nested(&["Empty"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    ctx.define_class(ClassDef {
        name: nested(&["EmptyDerived"]),
        bases: vec![BaseSpec {
            class: empty,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![FieldDef {
            name: Ident(String::from("x")),
            ty: int_,
            explicit_align: None,
        }],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_aligned(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Aligned"]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident(String::from("x")),
            ty: int_,
            explicit_align: None,
        }],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: Some(16),
    })
}

fn build_nested_record(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    let inner = ctx.define_class(ClassDef {
        name: nested(&["Inner"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("a")),
                ty: int_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("b")),
                ty: char_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let inner_ty = ctx.intern_type(CxxType::Record(inner));
    ctx.define_class(ClassDef {
        name: nested(&["Outer"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("prefix")),
                ty: char_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("inner")),
                ty: inner_ty,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("trailing")),
                ty: int_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_reference_member(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let int_ref = ctx.intern_type(CxxType::Ref {
        pointee: int_,
        kind: RefKind::Lvalue,
        cv: CvQual::default(),
    });
    ctx.define_class(ClassDef {
        name: nested(&["HoldsRef"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("ref")),
                ty: int_ref,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("tag")),
                ty: int_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_array_members(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    let int_array_5 = ctx.intern_type(CxxType::Array {
        elem: int_,
        len: 5,
    });
    let char_array_16 = ctx.intern_type(CxxType::Array {
        elem: char_,
        len: 16,
    });
    ctx.define_class(ClassDef {
        name: nested(&["WithArrays"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("items")),
                ty: int_array_5,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("name")),
                ty: char_array_16,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("trailer")),
                ty: int_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_field_alignas(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let char_ = char_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["WithAlign"]),
        bases: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident(String::from("small")),
                ty: char_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident(String::from("aligned")),
                ty: int_,
                explicit_align: Some(16),
            },
            FieldDef {
                name: Ident(String::from("tail")),
                ty: char_,
                explicit_align: None,
            },
        ],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_inherit_with_fields(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let void_ = void_ty(ctx);
    let animal = ctx.define_class(ClassDef {
        name: nested(&["Animal"]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident(String::from("id")),
            ty: int_,
            explicit_align: None,
        }],
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident(String::from("~Animal"))),
            sig: FnSig {
                params: Vec::new(),
                ret: void_,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::Virtual,
            vtable_index: None,
            special: Some(SpecialMember::Dtor),
        }],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    ctx.define_class(ClassDef {
        name: nested(&["Bird"]),
        bases: vec![BaseSpec {
            class: animal,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![FieldDef {
            name: Ident(String::from("wingspan")),
            ty: int_,
            explicit_align: None,
        }],
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn build_polymorphic(ctx: &mut CxxTypeCtx) -> ClassId {
    let int_ = int_ty(ctx);
    let void_ = void_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Widget"]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident(String::from("value")),
            ty: int_,
            explicit_align: None,
        }],
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("~Widget"))),
                sig: FnSig {
                    params: Vec::new(),
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::Virtual,
                vtable_index: Some(0),
                special: Some(SpecialMember::Dtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("render"))),
                sig: FnSig {
                    params: Vec::new(),
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::Virtual,
                vtable_index: Some(1),
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

// -------- Golden-integrity tests (run today) ----------------------------

#[test]
fn pod_scalar_golden_is_valid() {
    let d = load_golden("pod_scalar");
    assert_eq!(d.class, "Foo");
    assert_eq!(d.sizeof, 8);
    assert_eq!(d.align, 4);
    assert_eq!(d.fields.len(), 2);
    assert!(d.bases.is_empty());
    assert!(!d.has_vptr);
}

#[test]
fn single_inherit_golden_is_valid() {
    let d = load_golden("single_inherit");
    assert_eq!(d.class, "Derived");
    assert_eq!(d.sizeof, 12);
    assert_eq!(d.bases.len(), 1);
    assert_eq!(d.bases[0].class, "Base");
    // Tail-padding reuse: z at 5, not 8.
    assert_eq!(d.fields[0].name, "z");
    assert_eq!(d.fields[0].offset, 5);
}

#[test]
fn empty_base_golden_is_valid() {
    let d = load_golden("empty_base");
    assert_eq!(d.class, "EmptyDerived");
    assert_eq!(d.sizeof, 4);
    assert_eq!(d.bases.len(), 1);
    assert_eq!(d.bases[0].class, "Empty");
    // EBO: int x at 0, not 1.
    assert_eq!(d.fields[0].offset, 0);
}

#[test]
fn polymorphic_golden_is_valid() {
    let d = load_golden("polymorphic");
    assert_eq!(d.class, "Widget");
    assert_eq!(d.sizeof, 16);
    assert_eq!(d.align, 8);
    assert!(d.has_vptr);
    // Field after vptr.
    assert_eq!(d.fields[0].name, "value");
    assert_eq!(d.fields[0].offset, 8);
}

#[test]
fn alignas_golden_is_valid() {
    let d = load_golden("alignas");
    assert_eq!(d.class, "Aligned");
    assert_eq!(d.sizeof, 16);
    assert_eq!(d.align, 16);
    assert_eq!(d.nvalign, 16);
    assert_eq!(d.fields.len(), 1);
    assert_eq!(d.fields[0].offset, 0);
}

#[test]
fn nested_record_golden_is_valid() {
    let d = load_golden("nested_record");
    assert_eq!(d.class, "Outer");
    assert_eq!(d.sizeof, 16);
    assert_eq!(d.align, 4);
    // prefix@0, inner@4 (aligned to 4), trailing@12 (after Inner's 8 bytes)
    assert_eq!(d.fields[0].offset, 0);
    assert_eq!(d.fields[1].offset, 4);
    assert_eq!(d.fields[2].offset, 12);
}

#[test]
fn reference_member_golden_is_valid() {
    let d = load_golden("reference_member");
    assert_eq!(d.class, "HoldsRef");
    assert_eq!(d.sizeof, 16);
    assert_eq!(d.dsize, 12); // Reference members make the class non-POD.
    assert_eq!(d.fields[0].name, "ref");
    assert_eq!(d.fields[0].offset, 0);
    assert_eq!(d.fields[1].name, "tag");
    assert_eq!(d.fields[1].offset, 8);
}

#[test]
fn array_members_golden_is_valid() {
    let d = load_golden("array_members");
    assert_eq!(d.class, "WithArrays");
    assert_eq!(d.sizeof, 40);
    assert_eq!(d.align, 4);
    assert_eq!(d.fields.len(), 3);
    assert_eq!(d.fields[0].offset, 0);  // items[5]: 20 bytes at align 4.
    assert_eq!(d.fields[1].offset, 20); // name[16]: 16 bytes at align 1.
    assert_eq!(d.fields[2].offset, 36); // trailer: int@36 after name ends at 36.
}

#[test]
fn field_alignas_golden_is_valid() {
    let d = load_golden("field_alignas");
    assert_eq!(d.class, "WithAlign");
    assert_eq!(d.sizeof, 32);
    assert_eq!(d.align, 16);
    // alignas(16) on `aligned` pushes it to offset 16 (not 4).
    assert_eq!(d.fields[1].name, "aligned");
    assert_eq!(d.fields[1].offset, 16);
    assert_eq!(d.fields[2].offset, 20);
}

#[test]
fn inherit_with_fields_golden_is_valid() {
    let d = load_golden("inherit_with_fields");
    assert_eq!(d.class, "Bird");
    assert_eq!(d.sizeof, 16);
    assert!(d.has_vptr); // Inherited from Animal primary base.
    assert_eq!(d.bases.len(), 1);
    assert_eq!(d.bases[0].class, "Animal");
    assert_eq!(d.bases[0].offset, 0);
    // wingspan lands in Animal's tail padding at 12, not after sizeof(Animal).
    assert_eq!(d.fields[0].name, "wingspan");
    assert_eq!(d.fields[0].offset, 12);
}

#[test]
fn corpus_builders_compile() {
    // Exercising the builders on each target to catch IR-shape breakage
    // without relying on layout() being implemented.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let _ = build_pod_scalar(&mut ctx);
    let _ = build_single_inherit(&mut ctx);
    let _ = build_empty_base(&mut ctx);
    let _ = build_polymorphic(&mut ctx);
    let _ = build_aligned(&mut ctx);
    let _ = build_nested_record(&mut ctx);
    let _ = build_reference_member(&mut ctx);
    let _ = build_array_members(&mut ctx);
    let _ = build_field_alignas(&mut ctx);
    let _ = build_inherit_with_fields(&mut ctx);
}

// -------- Layout-diff tests (unignore when M2 lands) --------------------

#[test]
fn packed_layout_matches_clang() {
    let expected = load_golden("packed");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_packed(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn bitfield_layout_matches_clang() {
    let expected = load_golden("bitfield");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_bitfield(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn pod_scalar_layout_matches_clang() {
    let expected = load_golden("pod_scalar");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_pod_scalar(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn single_inherit_layout_matches_clang() {
    let expected = load_golden("single_inherit");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_single_inherit(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn empty_base_layout_matches_clang() {
    let expected = load_golden("empty_base");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_empty_base(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn polymorphic_layout_matches_clang() {
    let expected = load_golden("polymorphic");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_polymorphic(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn alignas_layout_matches_clang() {
    let expected = load_golden("alignas");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_aligned(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn nested_record_layout_matches_clang() {
    let expected = load_golden("nested_record");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_nested_record(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn reference_member_layout_matches_clang() {
    let expected = load_golden("reference_member");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_reference_member(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn array_members_layout_matches_clang() {
    let expected = load_golden("array_members");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_array_members(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn field_alignas_layout_matches_clang() {
    let expected = load_golden("field_alignas");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_field_alignas(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}

#[test]
fn inherit_with_fields_layout_matches_clang() {
    let expected = load_golden("inherit_with_fields");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_inherit_with_fields(&mut ctx);
    let layout = ctx.layout(class_id).expect("layout should succeed");
    let actual = layout_to_dump(&ctx, class_id, &layout, &expected.target);
    assert_eq!(actual, expected);
}
