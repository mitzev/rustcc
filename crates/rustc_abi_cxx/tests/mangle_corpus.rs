//! Mangling corpus — "hand-authored expected symbols must appear in the
//! Clang-generated golden" sanity tests.
//!
//! Full `ctx.mangle()` diff tests are not wired here yet: they depend on
//! two things that post-date this corpus, (1) `rustc_abi_cxx::mangle()`
//! being implemented (docs §6), and (2) the IR gaining a way to express
//! free-function and operator symbols as `Symbol` inputs (the current
//! `NestedName` shape only speaks namespace/class segments). When both
//! land, add per-corpus diff tests alongside these integrity checks.

use std::collections::HashSet;
use std::path::PathBuf;

use rustc_abi_cxx::{
    ClassDef, ClassId, CtorVariant, CvQual, CxxType, CxxTypeCtx, DtorVariant,
    FloatKind, FnSig, Ident, IntWidth, MethodName, NameSegment, NestedName,
    OperatorKind, RecordKind, RefKind, Symbol, Target, TemplateArg, TypeId,
};
use test_support::golden::{self, MangleDump};

fn corpus_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus")
        .join(filename)
}

fn load(basename: &str) -> MangleDump {
    let path = corpus_path(&format!("{basename}.mangle.golden"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read {} ({e}). Run `cargo xtask refresh-goldens`.",
            path.display()
        )
    });
    golden::parse_mangle(&text).unwrap_or_else(|e| {
        panic!(
            "failed to parse {}: {e}. Run `cargo xtask refresh-goldens`.",
            path.display()
        )
    })
}

fn assert_all_present(dump: &MangleDump, expected: &[(&str, &str)]) {
    let set: HashSet<&str> =
        dump.symbols.iter().map(String::as_str).collect();
    let missing: Vec<_> = expected
        .iter()
        .filter(|(_, mangled)| !set.contains(*mangled))
        .collect();
    assert!(
        missing.is_empty(),
        "these expected symbols are not in the golden (corpus drift — \
         did Clang's mangling change, or does the .cpp no longer define \
         these?): {missing:?}"
    );
}

// -------- mangle_basic --------------------------------------------------

const BASIC_EXPECTED: &[(&str, &str)] = &[
    ("free_fn(int)", "_Z7free_fni"),
    ("Foo::Foo() [C1]", "_ZN3FooC1Ev"),
    ("Foo::Foo() [C2]", "_ZN3FooC2Ev"),
    ("Foo::Foo(int) [C1]", "_ZN3FooC1Ei"),
    ("Foo::Foo(int) [C2]", "_ZN3FooC2Ei"),
    ("Foo::~Foo() [D1]", "_ZN3FooD1Ev"),
    ("Foo::~Foo() [D2]", "_ZN3FooD2Ev"),
    ("Foo::bar(int)", "_ZN3Foo3barEi"),
    ("Foo::bar(double) const", "_ZNK3Foo3barEd"),
];

#[test]
fn mangle_basic_golden_has_expected_symbols() {
    let d = load("mangle_basic");
    assert_all_present(&d, BASIC_EXPECTED);
    // Non-methodological sanity: nothing is empty.
    assert!(!d.symbols.is_empty());
    // All symbols must start with `_Z` (C++ mangled) — proves the
    // extractor isn't picking up stray non-C++ globals.
    for s in &d.symbols {
        assert!(s.starts_with("_Z"), "non-C++ symbol leaked: {s}");
    }
}

// -------- mangle_nested -------------------------------------------------

const NESTED_EXPECTED: &[(&str, &str)] = &[
    (
        "outer::inner::Bar::baz()",
        "_ZN5outer5inner3Bar3bazEv",
    ),
    // Substitution S1_ references `outer::inner::Bar` itself in the
    // parameter type.
    (
        "outer::inner::Bar::self(const Bar&)",
        "_ZN5outer5inner3Bar4selfERKS1_",
    ),
    (
        "outer::inner::Bar::Bar() [C1]",
        "_ZN5outer5inner3BarC1Ev",
    ),
    (
        "outer::inner::Bar::~Bar() [D1]",
        "_ZN5outer5inner3BarD1Ev",
    ),
];

#[test]
fn mangle_nested_golden_has_expected_symbols() {
    let d = load("mangle_nested");
    assert_all_present(&d, NESTED_EXPECTED);
}

// -------- mangle_operators ---------------------------------------------

const OPERATORS_EXPECTED: &[(&str, &str)] = &[
    // `pl` = operator+, `K` = const method, `S_` = first sub (= Vec).
    (
        "Vec::operator+(const Vec&) const",
        "_ZNK3VecplERKS_",
    ),
    // `aS` = operator=.
    (
        "Vec::operator=(const Vec&)",
        "_ZN3VecaSERKS_",
    ),
    // `ix` = operator[].
    ("Vec::operator[](int)", "_ZN3VecixEi"),
];

#[test]
fn mangle_operators_golden_has_expected_symbols() {
    let d = load("mangle_operators");
    assert_all_present(&d, OPERATORS_EXPECTED);
}

// -------- mangle_types --------------------------------------------------

const TYPES_EXPECTED: &[(&str, &str)] = &[
    // `P` = pointer, `PK` = pointer-to-const, `R` = lvalue reference,
    // `RK` = reference-to-const, `O` = rvalue reference.
    ("f_ptr(S*)", "_Z5f_ptrP1S"),
    ("f_cptr(const S*)", "_Z6f_cptrPK1S"),
    ("f_ref(S&)", "_Z5f_refR1S"),
    ("f_cref(const S&)", "_Z6f_crefRK1S"),
    ("f_rref(S&&)", "_Z6f_rrefO1S"),
    // Unscoped and scoped enums mangle as source-names (length-prefixed)
    // just like classes.
    ("f_enum(E)", "_Z6f_enum1E"),
    ("f_scoped(Scoped)", "_Z8f_scoped6Scoped"),
];

#[test]
fn mangle_types_golden_has_expected_symbols() {
    let d = load("mangle_types");
    assert_all_present(&d, TYPES_EXPECTED);
}

// -------- mangle_substitutions ------------------------------------------

const SUBSTITUTIONS_EXPECTED: &[(&str, &str)] = &[
    // mix(A, A, B, B, C, A):
    //   1A (register S_) → S_ (reuse A) → 1B (register S0_) →
    //   S0_ (reuse B) → 1C (register S1_) → S_ (reuse A).
    ("mix(A,A,B,B,C,A)", "_Z3mix1AS_1BS0_1CS_"),
    // refs(const A&, const A&, A*, A*):
    //   Spelling `RK1A` registers three substitutions: A (S_), KA (S0_),
    //   RKA (S1_). Second arg reuses the full RKA as S1_.
    //   `P1A` would overlap: we register S_ for A (already there), then
    //   `PA` (S2_). Clang emits `PS_` to reference A via S_ and the
    //   pointer wrapper registers S2_. Fourth arg reuses PA as S2_.
    ("refs(const A&, const A&, A*, A*)", "_Z4refsRK1AS1_PS_S2_"),
];

#[test]
fn mangle_substitutions_golden_has_expected_symbols() {
    let d = load("mangle_substitutions");
    assert_all_present(&d, SUBSTITUTIONS_EXPECTED);
}

// -------- ctx.mangle() diff tests --------------------------------------
//
// These call the real mangler and compare against the expected Clang
// output. Cases that would require substitution-table lookups are
// excluded — the mangler's first pass spells every compound type out in
// full, so `self(const Bar&)` would disagree with Clang's `S1_`-using
// mangling.

fn int32(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    })
}

fn float64(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Float {
        kind: FloatKind::F64,
    })
}

fn void_type(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Void)
}

fn build_foo(ctx: &mut CxxTypeCtx) -> ClassId {
    ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Foo".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
}

fn build_nested_bar(ctx: &mut CxxTypeCtx) -> ClassId {
    ctx.define_class(ClassDef {
        name: NestedName(vec![
            NameSegment::Namespace(Ident("outer".into())),
            NameSegment::Namespace(Ident("inner".into())),
            NameSegment::Class(Ident("Bar".into())),
        ]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    })
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
fn mangle_basic_matches_clang() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);
    let dbl = float64(&mut ctx);
    let void = void_type(&mut ctx);
    let foo = build_foo(&mut ctx);

    let free_fn = Symbol::Function {
        scope: NestedName(Vec::new()),
        name: Ident("free_fn".into()),
        sig: sig(vec![int_], void, false),
    };
    assert_eq!(ctx.mangle(&free_fn), "_Z7free_fni");

    let bar_int = Symbol::Method {
        class: foo,
        name: MethodName::Ident(Ident("bar".into())),
        sig: sig(vec![int_], void, false),
    };
    assert_eq!(ctx.mangle(&bar_int), "_ZN3Foo3barEi");

    let bar_dbl_const = Symbol::Method {
        class: foo,
        name: MethodName::Ident(Ident("bar".into())),
        sig: sig(vec![dbl], void, true),
    };
    assert_eq!(ctx.mangle(&bar_dbl_const), "_ZNK3Foo3barEd");

    let ctor_default_c1 = Symbol::Ctor {
        class: foo,
        variant: CtorVariant::C1,
        sig: sig(Vec::new(), void, false),
    };
    assert_eq!(ctx.mangle(&ctor_default_c1), "_ZN3FooC1Ev");

    let ctor_default_c2 = Symbol::Ctor {
        class: foo,
        variant: CtorVariant::C2,
        sig: sig(Vec::new(), void, false),
    };
    assert_eq!(ctx.mangle(&ctor_default_c2), "_ZN3FooC2Ev");

    let ctor_int_c1 = Symbol::Ctor {
        class: foo,
        variant: CtorVariant::C1,
        sig: sig(vec![int_], void, false),
    };
    assert_eq!(ctx.mangle(&ctor_int_c1), "_ZN3FooC1Ei");

    let ctor_int_c2 = Symbol::Ctor {
        class: foo,
        variant: CtorVariant::C2,
        sig: sig(vec![int_], void, false),
    };
    assert_eq!(ctx.mangle(&ctor_int_c2), "_ZN3FooC2Ei");

    let dtor_d1 = Symbol::Dtor {
        class: foo,
        variant: DtorVariant::D1,
    };
    assert_eq!(ctx.mangle(&dtor_d1), "_ZN3FooD1Ev");

    let dtor_d2 = Symbol::Dtor {
        class: foo,
        variant: DtorVariant::D2,
    };
    assert_eq!(ctx.mangle(&dtor_d2), "_ZN3FooD2Ev");

    // Cross-check: every mangled string we produced above is present in
    // Clang's golden. If this fires, it means the hand-written Symbol
    // inputs have drifted from what the corpus actually defines.
    let golden = load("mangle_basic");
    let golden_set: HashSet<&str> =
        golden.symbols.iter().map(String::as_str).collect();
    for (sym, expected) in [
        (free_fn, "_Z7free_fni"),
        (bar_int, "_ZN3Foo3barEi"),
        (bar_dbl_const, "_ZNK3Foo3barEd"),
        (ctor_default_c1, "_ZN3FooC1Ev"),
        (ctor_default_c2, "_ZN3FooC2Ev"),
        (ctor_int_c1, "_ZN3FooC1Ei"),
        (ctor_int_c2, "_ZN3FooC2Ei"),
        (dtor_d1, "_ZN3FooD1Ev"),
        (dtor_d2, "_ZN3FooD2Ev"),
    ] {
        let produced = ctx.mangle(&sym);
        assert_eq!(produced, expected);
        assert!(
            golden_set.contains(expected),
            "{expected} produced but not in corpus golden"
        );
    }
}

#[test]
fn mangle_nested_matches_clang() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void = void_type(&mut ctx);
    let bar = build_nested_bar(&mut ctx);

    let baz = Symbol::Method {
        class: bar,
        name: MethodName::Ident(Ident("baz".into())),
        sig: sig(Vec::new(), void, false),
    };
    assert_eq!(ctx.mangle(&baz), "_ZN5outer5inner3Bar3bazEv");

    let ctor_c1 = Symbol::Ctor {
        class: bar,
        variant: CtorVariant::C1,
        sig: sig(Vec::new(), void, false),
    };
    assert_eq!(ctx.mangle(&ctor_c1), "_ZN5outer5inner3BarC1Ev");

    let ctor_c2 = Symbol::Ctor {
        class: bar,
        variant: CtorVariant::C2,
        sig: sig(Vec::new(), void, false),
    };
    assert_eq!(ctx.mangle(&ctor_c2), "_ZN5outer5inner3BarC2Ev");

    let dtor_d1 = Symbol::Dtor {
        class: bar,
        variant: DtorVariant::D1,
    };
    assert_eq!(ctx.mangle(&dtor_d1), "_ZN5outer5inner3BarD1Ev");

    let dtor_d2 = Symbol::Dtor {
        class: bar,
        variant: DtorVariant::D2,
    };
    assert_eq!(ctx.mangle(&dtor_d2), "_ZN5outer5inner3BarD2Ev");

    // `self(const Bar&)` — exercises the substitution table. The scope
    // emission registers `outer` at S_, `outer::inner` at S0_,
    // `outer::inner::Bar` at S1_. When the param type refers to Bar, we
    // emit `S1_` instead of spelling out the full nested name.
    let bar_record = ctx.intern_type(CxxType::Record(bar));
    let const_bar_ref = ctx.intern_type(CxxType::Ref {
        pointee: bar_record,
        kind: RefKind::Lvalue,
        cv: CvQual {
            is_const: true,
            is_volatile: false,
        },
    });
    let self_method = Symbol::Method {
        class: bar,
        name: MethodName::Ident(Ident("self".into())),
        sig: sig(vec![const_bar_ref], void, false),
    };
    assert_eq!(ctx.mangle(&self_method), "_ZN5outer5inner3Bar4selfERKS1_");
}

#[test]
fn mangle_types_matches_clang() {
    // Covers the `mangle_types` corpus entry end-to-end against
    // `ctx.mangle()`. Exercises pointer/reference/rvalue-ref wrappers,
    // CV preservation on pointees, and — newly — unscoped and scoped
    // enum mangling now that `CxxType::Enum` carries its source name.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void = void_type(&mut ctx);
    let int_ = int32(&mut ctx);

    let s_class = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("S".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let s_record = ctx.intern_type(CxxType::Record(s_class));
    let const_cv = CvQual {
        is_const: true,
        is_volatile: false,
    };

    let s_ptr = ctx.intern_type(CxxType::Ptr {
        pointee: s_record,
        cv: CvQual::default(),
    });
    let s_cptr = ctx.intern_type(CxxType::Ptr {
        pointee: s_record,
        cv: const_cv,
    });
    let s_ref = ctx.intern_type(CxxType::Ref {
        pointee: s_record,
        kind: RefKind::Lvalue,
        cv: CvQual::default(),
    });
    let s_cref = ctx.intern_type(CxxType::Ref {
        pointee: s_record,
        kind: RefKind::Lvalue,
        cv: const_cv,
    });
    let s_rref = ctx.intern_type(CxxType::Ref {
        pointee: s_record,
        kind: RefKind::Rvalue,
        cv: CvQual::default(),
    });
    let e_type = ctx.intern_type(CxxType::Enum {
        name: NestedName(vec![NameSegment::Enum(Ident("E".into()))]),
        underlying: int_,
        scoped: false,
    });
    let scoped_type = ctx.intern_type(CxxType::Enum {
        name: NestedName(vec![NameSegment::Enum(Ident("Scoped".into()))]),
        underlying: int_,
        scoped: true,
    });

    let mk_fn = |name: &str, params: Vec<TypeId>| Symbol::Function {
        scope: NestedName(Vec::new()),
        name: Ident(name.into()),
        sig: sig(params, void, false),
    };

    let cases: Vec<(Symbol, &str)> = vec![
        (mk_fn("f_ptr", vec![s_ptr]), "_Z5f_ptrP1S"),
        (mk_fn("f_cptr", vec![s_cptr]), "_Z6f_cptrPK1S"),
        (mk_fn("f_ref", vec![s_ref]), "_Z5f_refR1S"),
        (mk_fn("f_cref", vec![s_cref]), "_Z6f_crefRK1S"),
        (mk_fn("f_rref", vec![s_rref]), "_Z6f_rrefO1S"),
        (mk_fn("f_enum", vec![e_type]), "_Z6f_enum1E"),
        (mk_fn("f_scoped", vec![scoped_type]), "_Z8f_scoped6Scoped"),
    ];

    // Cross-check each hand-built mangling against Clang's golden —
    // catches drift if the corpus .cpp file is edited without refreshing
    // the test.
    let golden = load("mangle_types");
    let golden_set: HashSet<&str> =
        golden.symbols.iter().map(String::as_str).collect();
    for (sym, expected) in &cases {
        let produced = ctx.mangle(sym);
        assert_eq!(produced, *expected, "{sym:?}");
        assert!(
            golden_set.contains(expected),
            "{expected} produced but not in corpus golden"
        );
    }
}

#[test]
fn mangle_operators_matches_clang() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void = void_type(&mut ctx);
    let int_ = int32(&mut ctx);

    let vec = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Vec".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let vec_record = ctx.intern_type(CxxType::Record(vec));
    let const_vec_ref = ctx.intern_type(CxxType::Ref {
        pointee: vec_record,
        kind: RefKind::Lvalue,
        cv: CvQual {
            is_const: true,
            is_volatile: false,
        },
    });

    // Vec::operator+(const Vec&) const → _ZNK3VecplERKS_
    // The class scope registers `Vec` at S_; the param's `const Vec&`
    // is emitted as `RK` + substitution `S_` for Vec.
    let op_plus = Symbol::Method {
        class: vec,
        name: MethodName::Operator(OperatorKind::Plus),
        sig: sig(vec![const_vec_ref], void, true),
    };
    assert_eq!(ctx.mangle(&op_plus), "_ZNK3VecplERKS_");

    // Vec::operator=(const Vec&) → _ZN3VecaSERKS_
    let op_assign = Symbol::Method {
        class: vec,
        name: MethodName::Operator(OperatorKind::Assign),
        sig: sig(vec![const_vec_ref], void, false),
    };
    assert_eq!(ctx.mangle(&op_assign), "_ZN3VecaSERKS_");

    // Vec::operator[](int) → _ZN3VecixEi (no substitution needed)
    let op_index = Symbol::Method {
        class: vec,
        name: MethodName::Operator(OperatorKind::Index),
        sig: sig(vec![int_], void, false),
    };
    assert_eq!(ctx.mangle(&op_index), "_ZN3VecixEi");
}

#[test]
fn mangle_substitutions_matches_clang() {
    // mix(A, A, B, B, C, A):
    //   1A (register S_) → S_ (reuse A) → 1B (register S0_) →
    //   S0_ (reuse B) → 1C (register S1_) → S_ (reuse A).
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void = void_type(&mut ctx);

    let a = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("A".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let b = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("B".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let c = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("C".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let a_ty = ctx.intern_type(CxxType::Record(a));
    let b_ty = ctx.intern_type(CxxType::Record(b));
    let c_ty = ctx.intern_type(CxxType::Record(c));

    let mix = Symbol::Function {
        scope: NestedName(Vec::new()),
        name: Ident("mix".into()),
        sig: sig(vec![a_ty, a_ty, b_ty, b_ty, c_ty, a_ty], void, false),
    };
    assert_eq!(ctx.mangle(&mix), "_Z3mix1AS_1BS0_1CS_");

    // refs(const A&, const A&, A*, A*):
    //   Arg 1 `RK1A` registers A (S_), KA (S0_), RKA (S1_).
    //   Arg 2 reuses the full RKA → `S1_`.
    //   Arg 3 `P A` — inside, A is already S_; emit `PS_`. Register PA (S2_).
    //   Arg 4 reuses PA → `S2_`.
    let mut ctx2 = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void2 = void_type(&mut ctx2);
    let a2 = ctx2.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("A".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let a2_record = ctx2.intern_type(CxxType::Record(a2));
    let const_a_ref = ctx2.intern_type(CxxType::Ref {
        pointee: a2_record,
        kind: RefKind::Lvalue,
        cv: CvQual {
            is_const: true,
            is_volatile: false,
        },
    });
    let a_ptr = ctx2.intern_type(CxxType::Ptr {
        pointee: a2_record,
        cv: CvQual::default(),
    });

    let refs = Symbol::Function {
        scope: NestedName(Vec::new()),
        name: Ident("refs".into()),
        sig: sig(
            vec![const_a_ref, const_a_ref, a_ptr, a_ptr],
            void2,
            false,
        ),
    };
    assert_eq!(ctx2.mangle(&refs), "_Z4refsRK1AS1_PS_S2_");
}

#[test]
fn mangle_rtti_symbols() {
    // Exercises _ZTV / _ZTI / _ZTS against the goldens, which include
    // the class-mangled form as a nested-or-source-name.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let base = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Base".into()))]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_eq!(ctx.mangle(&Symbol::VTable(base)), "_ZTV4Base");
    assert_eq!(ctx.mangle(&Symbol::TypeInfo(base)), "_ZTI4Base");
    assert_eq!(ctx.mangle(&Symbol::TypeInfoName(base)), "_ZTS4Base");
}

// -------- mangle_templates_nttp (non-type + template-template) -------

/// Build a `ClassDef` for a single-segment `TemplateSpec` and intern it
/// as a `Record` type, returning the type id.
fn intern_spec(
    c: &mut CxxTypeCtx,
    name: &str,
    args: Vec<TemplateArg>,
) -> TypeId {
    let cid = c.define_class(ClassDef {
        name: NestedName(vec![NameSegment::TemplateSpec {
            name: Ident(name.into()),
            args,
        }]),
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    c.intern_type(CxxType::Record(cid))
}

fn free_fn(c: &CxxTypeCtx, name: &str, param: TypeId, ret: TypeId) -> String {
    c.mangle(&Symbol::Function {
        scope: NestedName(vec![]),
        name: Ident(name.into()),
        sig: FnSig {
            params: vec![param],
            ret,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
    })
}

#[test]
fn mangle_templates_nttp_golden_has_expected_symbols() {
    let d = load("mangle_templates_nttp");
    assert_all_present(
        &d,
        &[
            ("take_arr4(Arr<int,4>)", "_Z9take_arr43ArrIiLi4EE"),
            ("take_arrneg(Arr<int,-1>)", "_Z11take_arrneg3ArrIiLin1EE"),
            (
                "take_sizearr(SizeArr<int,4ull>)",
                "_Z12take_sizearr7SizeArrIiLy4EE",
            ),
            ("take_flag(Flag<true>)", "_Z9take_flag4FlagILb1EE"),
            (
                "take_charbox(CharBox<'A'>)",
                "_Z12take_charbox7CharBoxILc65EE",
            ),
            ("take_stack(Stack<int,Box>)", "_Z10take_stack5StackIi3BoxE"),
        ],
    );
}

#[test]
fn mangle_templates_nttp_matches_clang() {
    let mut c = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let v = c.intern_type(CxxType::Void);
    let i = c.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
    let ul =
        c.intern_type(CxxType::Int { signed: false, width: IntWidth::I64 });
    let b = c.intern_type(CxxType::Bool);
    let ch =
        c.intern_type(CxxType::Int { signed: true, width: IntWidth::I8 });

    // Arr<int, 4>  ->  3ArrIiLi4EE
    let arr4 = intern_spec(
        &mut c,
        "Arr",
        vec![TemplateArg::Type(i), TemplateArg::Integral { value: 4, ty: i }],
    );
    assert_eq!(free_fn(&c, "take_arr4", arr4, v), "_Z9take_arr43ArrIiLi4EE");

    // Arr<int, -1>  ->  3ArrIiLin1EE  (negative -> `n` prefix)
    let arrneg = intern_spec(
        &mut c,
        "Arr",
        vec![TemplateArg::Type(i), TemplateArg::Integral { value: -1, ty: i }],
    );
    assert_eq!(
        free_fn(&c, "take_arrneg", arrneg, v),
        "_Z11take_arrneg3ArrIiLin1EE"
    );

    // SizeArr<int, 4ull>  ->  7SizeArrIiLy4EE  (unsigned long long -> `y`).
    // NB: the IR collapses `long`/`long long` to one 64-bit width, so a
    // `size_t` (= `unsigned long` on LP64) NTTP mangles as `y`, not `m`.
    // That long/long-long ambiguity is a pre-existing, codebase-wide
    // limitation (see `int_code`), not specific to template arguments.
    let sizearr = intern_spec(
        &mut c,
        "SizeArr",
        vec![TemplateArg::Type(i), TemplateArg::Integral { value: 4, ty: ul }],
    );
    assert_eq!(
        free_fn(&c, "take_sizearr", sizearr, v),
        "_Z12take_sizearr7SizeArrIiLy4EE"
    );

    // Flag<true>  ->  4FlagILb1EE
    let flag = intern_spec(
        &mut c,
        "Flag",
        vec![TemplateArg::Integral { value: 1, ty: b }],
    );
    assert_eq!(free_fn(&c, "take_flag", flag, v), "_Z9take_flag4FlagILb1EE");

    // CharBox<'A'>  ->  7CharBoxILc65EE  ('A' == 65)
    let charbox = intern_spec(
        &mut c,
        "CharBox",
        vec![TemplateArg::Integral { value: 65, ty: ch }],
    );
    assert_eq!(
        free_fn(&c, "take_charbox", charbox, v),
        "_Z12take_charbox7CharBoxILc65EE"
    );

    // Stack<int, Box>  ->  5StackIi3BoxE  (template-template arg `Box`)
    let stack = intern_spec(
        &mut c,
        "Stack",
        vec![
            TemplateArg::Type(i),
            TemplateArg::Template(NestedName(vec![NameSegment::Class(
                Ident("Box".into()),
            )])),
        ],
    );
    assert_eq!(
        free_fn(&c, "take_stack", stack, v),
        "_Z10take_stack5StackIi3BoxE"
    );
}

/// v1.13.10: function-pointer parameters mangle as `PF<ret><params>E`
/// (Itanium §5.1.5.1) with full substitution participation — pinned
/// against clang for FLTK's `Fl_Callback` shapes. Was a literal `F?E`,
/// which made every callback-taking method unlinkable.
#[test]
fn fn_pointer_params_match_clang() {
    use rustc_abi_cxx::{
        ClassDef, CvQual, CxxType, CxxTypeCtx, FnSig, Ident, IntWidth,
        MethodName, NameSegment, NestedName, RecordKind, Symbol, Target,
    };
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let void = ctx.intern_type(CxxType::Void);
    let int = ctx.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
    let chr = ctx.intern_type(CxxType::Int { signed: true, width: IntWidth::I8 });
    let widget = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Fl_Widget".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let widget_ty = ctx.intern_type(CxxType::Record(widget));
    let p_widget = ctx.intern_type(CxxType::Ptr { pointee: widget_ty, cv: CvQual::default() });
    let p_void = ctx.intern_type(CxxType::Ptr { pointee: void, cv: CvQual::default() });
    // void (*)(Fl_Widget*, void*)
    let cb_fn = ctx.intern_type(CxxType::Fn(FnSig {
        params: vec![p_widget, p_void],
        ret: void,
        cv: CvQual::default(),
        ref_q: None,
        variadic: false,
        noexcept: false,
    }));
    let p_cb = ctx.intern_type(CxxType::Ptr { pointee: cb_fn, cv: CvQual::default() });

    // void Fl_Widget::callback(Fl_Callback*)  ->  _ZN9Fl_Widget8callbackEPFvPS_PvE
    let sym = ctx.mangle_itanium(&Symbol::Method {
        class: widget,
        name: MethodName::Ident(Ident("callback".into())),
        sig: FnSig {
            params: vec![p_cb],
            ret: void,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
    });
    assert_eq!(sym, "_ZN9Fl_Widget8callbackEPFvPS_PvE");

    // void M::add(const char*, int, Fl_Callback*, void*, int)
    //   ->  _ZN1M3addEPKciPFvP9Fl_WidgetPvES4_i
    let m = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("M".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let pkc = ctx.intern_type(CxxType::Ptr {
        pointee: chr,
        cv: CvQual { is_const: true, is_volatile: false },
    });
    let sym2 = ctx.mangle_itanium(&Symbol::Method {
        class: m,
        name: MethodName::Ident(Ident("add".into())),
        sig: FnSig {
            params: vec![pkc, int, p_cb, p_void, int],
            ret: void,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
    });
    assert_eq!(sym2, "_ZN1M3addEPKciPFvP9Fl_WidgetPvES4_i");
}

/// v1.14 phase 1: pointer-to-member-function params mangle as
/// `M<class>[K]F…E` (Itanium §5.1.5) with substitution — pinned against
/// clang (`take(AddFn)` / const member fn / repeated param).
#[test]
fn member_fn_pointer_params_match_clang() {
    use rustc_abi_cxx::{
        ClassDef, CvQual, CxxType, CxxTypeCtx, FnSig, Ident, IntWidth,
        MethodName, NameSegment, NestedName, RecordKind, Symbol, Target,
    };
    let _ = MethodName::ident_name; // keep import shape uniform
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let void = ctx.intern_type(CxxType::Void);
    let int = ctx.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
    let recv = ctx.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Receiver".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let memfn = |ctx: &mut CxxTypeCtx, is_const: bool| {
        let f = ctx.intern_type(CxxType::Fn(FnSig {
            params: vec![int],
            ret: int,
            cv: CvQual { is_const, is_volatile: false },
            ref_q: None,
            variadic: false,
            noexcept: false,
        }));
        ctx.intern_type(CxxType::MemberPtr { class: recv, pointee: f })
    };
    let mp = memfn(&mut ctx, false);
    let mp_c = memfn(&mut ctx, true);
    let free = |name: &str, params: Vec<rustc_abi_cxx::TypeId>| Symbol::Function {
        scope: NestedName(vec![]),
        name: Ident(name.into()),
        sig: FnSig { params, ret: void, cv: CvQual::default(), ref_q: None, variadic: false, noexcept: false },
    };
    // void take(int (Receiver::*)(int))        -> _Z4takeM8ReceiverFiiE
    assert_eq!(ctx.mangle_itanium(&free("take", vec![mp])), "_Z4takeM8ReceiverFiiE");
    // void take_c(int (Receiver::*)(int) const) -> _Z6take_cM8ReceiverKFiiE
    assert_eq!(ctx.mangle_itanium(&free("take_c", vec![mp_c])), "_Z6take_cM8ReceiverKFiiE");
    // void take2(AddFn, AddFn)                  -> _Z5take2M8ReceiverFiiES1_
    assert_eq!(ctx.mangle_itanium(&free("take2", vec![mp, mp])), "_Z5take2M8ReceiverFiiES1_");
}
