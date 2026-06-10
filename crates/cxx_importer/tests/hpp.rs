// Unit tests for the `.hpp` emitter (`#[repr(cpp)]` → C++ header).
// No libclang dependency — classes are hand-built.

use cxx_importer::hpp::{generate_hpp, HppOptions};
use rustc_abi_cxx::{
    ClassDef, CvQual, CxxType, CxxTypeCtx, FieldDef, FnSig, Ident, IntWidth,
    MethodDef, MethodName, NameSegment, NestedName, RecordKind, SpecialMember,
    Target, TypeId, Virtuality,
};

fn int32(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    })
}

fn void_type(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Void)
}

fn sig(params: Vec<TypeId>, ret: TypeId, is_const: bool) -> FnSig {
    FnSig {
        params,
        ret,
        cv: CvQual {
            is_const,
            is_volatile: false,
        },
        ref_q: None,
        variadic: false,
        noexcept: false,
    }
}

#[test]
fn emits_class_with_opaque_storage_sized_from_layout() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);

    // A 3×i32 struct → size 12, align 4. Shape check on the generated
    // storage declaration comes directly from `ctx.layout()`.
    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![
            NameSegment::Namespace(Ident("acme".into())),
            NameSegment::Class(Ident("Widget".into())),
        ]),
        bases: Vec::new(),
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: int_, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: int_, explicit_align: None },
            FieldDef { name: Ident("c".into()), ty: int_, explicit_align: None },
        ],
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("tick".into())),
            sig: sig(Vec::new(), void, false),
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        }],
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_hpp(
        &ctx,
        &HppOptions {
            classes: &[widget],
        },
    )
    .expect("generate_hpp");

    // Structural asserts on the generated text.
    assert!(out.contains("#pragma once"), "missing pragma once\n{out}");
    assert!(out.contains("#include <cstdint>"), "missing cstdint\n{out}");
    assert!(out.contains("namespace acme {"), "missing namespace open\n{out}");
    assert!(out.contains("class Widget {"), "missing class line\n{out}");
    assert!(out.contains("public:"), "missing public section\n{out}");
    assert!(out.contains("private:"), "missing private section\n{out}");

    // Special members: deleted copy, noexcept move, dtor.
    assert!(
        out.contains("Widget(const Widget&) = delete;"),
        "missing deleted copy-ctor\n{out}"
    );
    assert!(
        out.contains("Widget(Widget&&) noexcept;"),
        "missing noexcept move-ctor\n{out}"
    );
    assert!(out.contains("~Widget();"), "missing dtor\n{out}");

    // User method is rendered.
    assert!(out.contains("void tick();"), "missing tick method\n{out}");

    // Opaque storage reflects computed layout (3 × i32 = 12 bytes, align 4).
    assert!(
        out.contains("alignas(4) unsigned char __rust_storage[12];"),
        "expected storage[12]/align(4), got:\n{out}"
    );

    assert!(out.contains("}  // namespace acme"), "missing namespace close\n{out}");
}

#[test]
fn const_method_emits_const_qualifier() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident("state".into()),
            ty: int_,
            explicit_align: None,
        }],
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("compute".into())),
            sig: sig(vec![int_], int_, true),
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        }],
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_hpp(
        &ctx,
        &HppOptions {
            classes: &[widget],
        },
    )
    .unwrap();

    // Fixed-width integer name from <cstdint>.
    assert!(
        out.contains("std::int32_t compute(std::int32_t arg0) const;"),
        "expected const method with cstdint types, got:\n{out}"
    );
}

#[test]
fn user_ctor_appears_before_special_members() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("Widget".into())),
            sig: sig(vec![int_], void, false),
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: Some(SpecialMember::OtherCtor),
        }],
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_hpp(
        &ctx,
        &HppOptions {
            classes: &[widget],
        },
    )
    .unwrap();

    let user_ctor = out.find("Widget(std::int32_t arg0);").expect(&format!(
        "expected user ctor signature in:\n{out}"
    ));
    let deleted_copy = out.find("Widget(const Widget&) = delete;").unwrap();
    assert!(
        user_ctor < deleted_copy,
        "user ctor should precede the deleted copy-ctor in the output"
    );
}

#[test]
fn skips_virtual_and_user_copy_move_dtor_in_favor_of_canonical() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident("state".into()),
            ty: int_,
            explicit_align: None,
        }],
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("vcall".into())),
                sig: sig(Vec::new(), void, false),
                virtuality: Virtuality::Virtual,
                vtable_index: Some(0),
                special: None,
            },
            // User-declared copy ctor → swallowed; we always emit `= delete`.
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Widget".into())),
                sig: sig(Vec::new(), void, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::CopyCtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("compute".into())),
                sig: sig(vec![int_], int_, true),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Class,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_hpp(
        &ctx,
        &HppOptions {
            classes: &[widget],
        },
    )
    .unwrap();

    assert!(!out.contains("vcall"), "virtual method leaked in\n{out}");
    // Exactly one copy-ctor line, and it's the deleted one.
    assert_eq!(
        out.matches("Widget(const Widget&)").count(),
        1,
        "expected one copy-ctor line\n{out}"
    );
    assert!(out.contains("= delete;"), "missing delete\n{out}");
    assert!(out.contains("compute"), "user method should still render\n{out}");
}

#[test]
fn nested_namespaces_open_and_close_in_order() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);

    let cls = ctx.define_class(ClassDef {
        name: NestedName(vec![
            NameSegment::Namespace(Ident("outer".into())),
            NameSegment::Namespace(Ident("inner".into())),
            NameSegment::Class(Ident("Bar".into())),
        ]),
        bases: Vec::new(),
        fields: vec![FieldDef {
            name: Ident("x".into()),
            ty: int_,
            explicit_align: None,
        }],
        methods: Vec::new(),
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_hpp(
        &ctx,
        &HppOptions { classes: &[cls] },
    )
    .unwrap();

    let outer_open = out.find("namespace outer {").unwrap();
    let inner_open = out.find("namespace inner {").unwrap();
    let class_line = out.find("class Bar {").unwrap();
    let inner_close = out.find("}  // namespace inner").unwrap();
    let outer_close = out.find("}  // namespace outer").unwrap();
    assert!(outer_open < inner_open);
    assert!(inner_open < class_line);
    assert!(class_line < inner_close);
    assert!(inner_close < outer_close);
}
