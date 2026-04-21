// Unit tests for the C++ shim generator.
//
// These hand-build a small `CxxTypeCtx` and assert the emitted `.cpp`
// source contains the expected `extern "C" noexcept` trampolines and
// `try/catch -> std::terminate` bodies, per docs/exception_boundary.md §2.
// No libclang dependency — the tests run without the `libclang` feature.

use cxx_importer::shims::{generate_shims, ShimOptions};
use rustc_abi_cxx::{
    ClassDef, CxxType, CxxTypeCtx, CvQual, FnSig, Ident, IntWidth, MethodDef,
    MethodName, NameSegment, NestedName, OperatorKind, RecordKind, RefKind,
    SpecialMember, Target, TypeId, Virtuality,
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

fn bool_type(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Bool)
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
fn emits_header_and_exception_include() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef {
            name: MethodName::Ident(Ident("tick".into())),
            sig: sig(vec![int_], void, false),
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        }],
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["widget.h"],
            classes: &[widget],
        },
    )
    .expect("generator should succeed");

    assert!(out.contains("#include \"widget.h\""), "missing user header\n{out}");
    assert!(out.contains("#include <exception>"), "missing <exception>\n{out}");
}

#[test]
fn emits_extern_c_noexcept_trampoline_for_nonvirtual_method() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let int_ = int32(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef {
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

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["w.h"],
            classes: &[widget],
        },
    )
    .unwrap();

    // Signature shape: `extern "C" int __rustcc_shim_<mangled>(const Widget* self, int arg0) noexcept`.
    assert!(out.contains("extern \"C\""), "missing extern \"C\"\n{out}");
    assert!(out.contains("noexcept"), "missing noexcept\n{out}");
    assert!(out.contains("__rustcc_shim_"), "missing shim prefix\n{out}");
    assert!(out.contains("const Widget* self"), "missing const self param\n{out}");
    assert!(out.contains("int arg0"), "missing positional arg\n{out}");

    // Body: try { return self->compute(arg0); } catch (...) { std::terminate(); }
    assert!(out.contains("self->compute(arg0)"), "missing forwarding call\n{out}");
    assert!(out.contains("catch (...)"), "missing catch-all\n{out}");
    assert!(out.contains("std::terminate()"), "missing terminate()\n{out}");
    assert!(out.contains("return"), "missing return for non-void ret\n{out}");
}

#[test]
fn void_return_omits_return_keyword_in_body() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let void = void_type(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef {
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

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["w.h"],
            classes: &[widget],
        },
    )
    .unwrap();

    // Non-const method → self is `Widget*` (not `const Widget*`).
    assert!(out.contains("Widget* self"), "missing mutable self\n{out}");
    assert!(!out.contains("const Widget* self"), "self should be mutable\n{out}");
    // Forwarding call followed by `;` (no `return` before it).
    assert!(out.contains("self->tick();"), "missing forwarding call\n{out}");
}

#[test]
fn uses_mangled_symbol_in_shim_name() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let int_ = int32(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef {
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

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["w.h"],
            classes: &[widget],
        },
    )
    .unwrap();

    // `int Widget::compute(int) const` → Itanium `_ZNK6Widget7computeEi`.
    assert!(
        out.contains("__rustcc_shim__ZNK6Widget7computeEi"),
        "expected exact Itanium-mangled shim name, got:\n{out}"
    );
}

#[test]
fn skips_virtual_and_special_methods() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);

    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            MethodDef {
                name: MethodName::Ident(Ident("vcall".into())),
                sig: sig(Vec::new(), void, false),
                virtuality: Virtuality::Virtual,
                vtable_index: Some(0),
                special: None,
            },
            MethodDef {
                name: MethodName::Ident(Ident("~Widget".into())),
                sig: sig(Vec::new(), void, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            MethodDef {
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

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["w.h"],
            classes: &[widget],
        },
    )
    .unwrap();

    assert!(!out.contains("vcall"), "virtual method leaked in\n{out}");
    assert!(!out.contains("~Widget"), "destructor leaked in\n{out}");
    assert!(out.contains("compute"), "non-virtual method should be emitted\n{out}");
}

#[test]
fn renders_reference_and_record_params() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let bool_ = bool_type(&mut ctx);

    // Two-phase: register the class, intern a `const Widget&` that points
    // back at it, then attach the method that takes that reference.
    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let widget_record = ctx.intern_type(CxxType::Record(widget));
    let widget_const_ref = ctx.intern_type(CxxType::Ref {
        pointee: widget_record,
        kind: RefKind::Lvalue,
        cv: CvQual {
            is_const: true,
            is_volatile: false,
        },
    });

    ctx.class_mut(widget).methods.push(MethodDef {
        name: MethodName::Operator(OperatorKind::Eq),
        sig: sig(vec![widget_const_ref], bool_, true),
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: None,
    });

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["w.h"],
            classes: &[widget],
        },
    )
    .unwrap();

    assert!(out.contains("const Widget& arg0"), "expected const ref param\n{out}");
    assert!(
        out.contains("self->operator==(arg0)"),
        "expected operator== forwarding call\n{out}"
    );
    assert!(out.contains("bool __rustcc_shim_"), "expected bool return\n{out}");
}

#[test]
fn renders_namespaced_class_in_self_param() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let void = void_type(&mut ctx);

    let inner = ctx.define_class(ClassDef {
        name: NestedName(vec![
            NameSegment::Namespace(Ident("outer".into())),
            NameSegment::Namespace(Ident("inner".into())),
            NameSegment::Class(Ident("Bar".into())),
        ]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![MethodDef {
            name: MethodName::Ident(Ident("ping".into())),
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

    let out = generate_shims(
        &ctx,
        &ShimOptions {
            headers: &["bar.h"],
            classes: &[inner],
        },
    )
    .unwrap();

    assert!(
        out.contains("outer::inner::Bar* self"),
        "expected qualified self type, got:\n{out}"
    );
}
