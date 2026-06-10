//! MSVC-target bindings emission smoke test.
//!
//! Builds a minimal `CxxTypeCtx` against a Windows-MSVC `Target`, runs
//! the binding generator, and asserts the emitted `#[link_name = "..."]`
//! attributes carry MSVC-mangled symbols (start with `?` / `??`),
//! not Itanium symbols (`_Z` prefix).
//!
//! This verifies the dispatcher routing end-to-end without needing
//! libclang or a Windows machine — the test only builds string-level
//! Rust source.

use cxx_importer::rust_bindings::{
    generate_rust_bindings, BindingsBackend, RustBindingsConfig,
};
use rustc_abi_cxx::{
    AbiFlavor, ClassDef, CvQual, CxxType, CxxTypeCtx, FnSig, Ident, IntWidth,
    MethodDef, MethodName, NameSegment, NestedName, RecordKind, SpecialMember,
    Target, Virtuality,
};

fn intern_int(ctx: &mut CxxTypeCtx) -> rustc_abi_cxx::TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    })
}

fn intern_void(ctx: &mut CxxTypeCtx) -> rustc_abi_cxx::TypeId {
    ctx.intern_type(CxxType::Void)
}

fn build_simple_class(ctx: &mut CxxTypeCtx) -> rustc_abi_cxx::ClassId {
    let i = intern_int(ctx);
    let v = intern_void(ctx);
    // struct Foo { Foo(int x); ~Foo(); int get() const; }
    ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Foo".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![
            // Default ctor (Foo(int x))
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Foo".into())),
                sig: FnSig {
                    params: vec![i],
                    ret: v,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::OtherCtor),
            },
            // Destructor
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("~Foo".into())),
                sig: FnSig {
                    params: vec![],
                    ret: v,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            // int get() const
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("get".into())),
                sig: FnSig {
                    params: vec![],
                    ret: i,
                    cv: CvQual { is_const: true, is_volatile: false },
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

#[test]
fn msvc_bindings_use_msvc_mangling() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_pc_windows_msvc());
    assert_eq!(ctx.target().abi_flavor, AbiFlavor::Msvc);
    let foo = build_simple_class(&mut ctx);

    let config = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..Default::default()
    };
    let src = generate_rust_bindings(&ctx, &[foo], &config)
        .expect("generate bindings");

    // The emitted source should reference MSVC-mangled link names
    // (start with `?` / `??`). We don't pin exact symbols because
    // they're regression-tested in `rustc_abi_cxx::mangle_corpus_msvc`;
    // here we just confirm that the dispatcher routes correctly.
    let has_msvc_mangled = src.contains("link_name = \"?")
        || src.contains("link_name = \"??");
    let has_itanium_mangled = src.contains("link_name = \"_Z");
    assert!(
        has_msvc_mangled,
        "expected MSVC-mangled link names (?...) in output. Source:\n{src}"
    );
    assert!(
        !has_itanium_mangled,
        "found Itanium-mangled link names (_Z...) in MSVC-target output — \
         dispatcher routing is broken. Source:\n{src}"
    );

    // Spot-check: the ctor link name should match what
    // `mangle_corpus_msvc` validates against clang.
    assert!(
        src.contains("??0Foo@@QEAA@H@Z"),
        "expected MSVC ctor symbol ??0Foo@@QEAA@H@Z in output. Source:\n{src}"
    );
    assert!(
        src.contains("??1Foo@@QEAA@XZ"),
        "expected MSVC dtor symbol ??1Foo@@QEAA@XZ in output. Source:\n{src}"
    );
    assert!(
        src.contains("?get@Foo@@QEBAHXZ"),
        "expected MSVC method symbol ?get@Foo@@QEBAHXZ in output. Source:\n{src}"
    );
}

#[test]
fn itanium_bindings_still_use_itanium_mangling() {
    // Negative control — same class graph but with a Linux target.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    assert_eq!(ctx.target().abi_flavor, AbiFlavor::Itanium);
    let foo = build_simple_class(&mut ctx);

    let config = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..Default::default()
    };
    let src = generate_rust_bindings(&ctx, &[foo], &config)
        .expect("generate bindings");
    assert!(
        src.contains("link_name = \"_Z"),
        "expected Itanium-mangled link names in output. Source:\n{src}"
    );
    assert!(
        !src.contains("link_name = \"??"),
        "found MSVC-mangled symbols in Itanium-target output. Source:\n{src}"
    );
}
