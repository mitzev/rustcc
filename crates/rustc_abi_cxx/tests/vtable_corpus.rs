//! Vtable corpus — golden integrity + `#[ignore]`-d diff tests that
//! call `ctx.vtable()` and compare against Clang's output.
//!
//! Both ignored tests unignore together when `rustc_abi_cxx::vtable()` is
//! implemented (docs §7). They also transitively depend on `mangle()`
//! because vtable entries carry Itanium mangled symbols as their target.

use std::path::PathBuf;

use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, CxxTypeCtx, FieldDef,
    FnSig, Ident, IntWidth, MethodDef, MethodName, NameSegment, NestedName,
    RecordKind, SpecialMember, Target, TypeId, VTable, VTableEntry, Virtuality,
};
use test_support::golden::{
    self, VtableDump, VtableEntryDump, VtableSubTableDump,
};

fn corpus_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus")
        .join(filename)
}

fn load(basename: &str) -> VtableDump {
    let path = corpus_path(&format!("{basename}.vtable.golden"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read {} ({e}). Run `cargo xtask refresh-goldens`.",
            path.display()
        )
    });
    golden::parse_vtable(&text).unwrap_or_else(|e| {
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

// -------- Builders ------------------------------------------------------

fn int_ty(ctx: &mut CxxTypeCtx) -> TypeId {
    ctx.intern_type(CxxType::Int {
        signed: true,
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

fn sig_no_args(ret: TypeId, cv: CvQual, noexcept: bool) -> FnSig {
    FnSig {
        params: Vec::new(),
        ret,
        cv,
        ref_q: None,
        variadic: false,
        noexcept,
    }
}

fn build_base(ctx: &mut CxxTypeCtx) -> ClassId {
    let void = void_ty(ctx);
    let int = int_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Base"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("~Base"))),
                sig: sig_no_args(void, CvQual::default(), true),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("render"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("area"))),
                sig: sig_no_args(
                    int,
                    CvQual {
                        is_const: true,
                        is_volatile: false,
                    },
                    false,
                ),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn build_abstract(ctx: &mut CxxTypeCtx) -> ClassId {
    let void = void_ty(ctx);
    let int = int_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Abstract"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("~Abstract"))),
                sig: sig_no_args(void, CvQual::default(), true),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("compute"))),
                sig: sig_no_args(
                    int,
                    CvQual {
                        is_const: true,
                        is_volatile: false,
                    },
                    false,
                ),
                virtuality: Virtuality::PureVirtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("render"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::PureVirtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn build_middle(ctx: &mut CxxTypeCtx) -> ClassId {
    // Root → Middle chain; target is Middle (intermediate in a larger
    // chain, but here the most-derived at query time).
    let void = void_ty(ctx);
    let root = ctx.define_class(ClassDef {
        name: nested(&["Root"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("~Root"))),
                sig: sig_no_args(void, CvQual::default(), true),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("a"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    ctx.define_class(ClassDef {
        name: nested(&["Middle"]),
        bases: vec![BaseSpec {
            class: root,
            virtual_: false,
            access: Access::Public,
        }],
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("a"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("b"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn build_leaf_chain(ctx: &mut CxxTypeCtx) -> ClassId {
    let void = void_ty(ctx);
    let root = ctx.define_class(ClassDef {
        name: nested(&["Root"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("~Root"))),
                sig: sig_no_args(void, CvQual::default(), true),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("a"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    let middle = ctx.define_class(ClassDef {
        name: nested(&["Middle"]),
        bases: vec![BaseSpec {
            class: root,
            virtual_: false,
            access: Access::Public,
        }],
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("a"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("b"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    ctx.define_class(ClassDef {
        name: nested(&["Leaf"]),
        bases: vec![BaseSpec {
            class: middle,
            virtual_: false,
            access: Access::Public,
        }],
        fields: Vec::new(),
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("a"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("b"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident(String::from("c"))),
                sig: sig_no_args(void, CvQual::default(), false),
                virtuality: Virtuality::Virtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn build_base_and_derived(ctx: &mut CxxTypeCtx) -> ClassId {
    let base = build_base(ctx);
    let void = void_ty(ctx);
    let int = int_ty(ctx);
    ctx.define_class(ClassDef {
        name: nested(&["Derived"]),
        bases: vec![BaseSpec {
            class: base,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![FieldDef {
            name: Ident(String::from("value")),
            ty: int,
            explicit_align: None,
        }],
        methods: vec![MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident(String::from("render"))),
            sig: sig_no_args(void, CvQual::default(), false),
            virtuality: Virtuality::Virtual,
            vtable_index: None,
            special: None,
        }],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    })
}

fn vtable_to_dump(
    ctx: &CxxTypeCtx,
    vtable: &VTable,
    class_name: String,
    target: String,
) -> VtableDump {
    let ptr_bytes = (ctx.target().pointer_width_bits as u64) / 8;
    let sub_tables = vtable
        .sub_tables
        .iter()
        .map(|st| {
            let entries = st
                .entries
                .iter()
                .map(|e| match e {
                    VTableEntry::VbaseOffset(v) => {
                        VtableEntryDump::VbaseOffset(*v)
                    }
                    VTableEntry::OffsetToTop(v) => {
                        VtableEntryDump::OffsetToTop(*v)
                    }
                    VTableEntry::Rtti(s) => {
                        VtableEntryDump::Rtti(s.clone())
                    }
                    VTableEntry::FunctionPointer {
                        mangled_target, ..
                    } => VtableEntryDump::FunctionPointer(
                        mangled_target.clone(),
                    ),
                })
                .collect();
            // `address_point_offset` is measured from the start of
            // `_ZTV<class>`. Convert it to a slot index within this
            // sub-table by subtracting the sub-table's own byte start.
            let sub_start = if ctx
                .class(st.for_subobject)
                .is_polymorphic
            {
                st.address_point_offset.saturating_sub(2 * ptr_bytes)
            } else {
                0
            };
            VtableSubTableDump {
                for_subobject: subobject_name(ctx, st.for_subobject),
                subobject_offset: st.subobject_offset,
                address_point_slot: (st.address_point_offset - sub_start)
                    / ptr_bytes,
                entries,
            }
        })
        .collect();
    VtableDump {
        target,
        class: class_name,
        sub_tables,
    }
}

fn subobject_name(ctx: &CxxTypeCtx, class_id: ClassId) -> String {
    match ctx.class(class_id).name.0.last() {
        Some(NameSegment::Class(i)) | Some(NameSegment::Namespace(i)) => {
            i.0.clone()
        }
        _ => String::from("(unnamed)"),
    }
}

// -------- Golden-integrity tests (run today) ---------------------------

#[test]
fn vtable_simple_golden_is_valid() {
    let d = load("vtable_simple");
    assert_eq!(d.class, "Base");
    assert_eq!(d.sub_tables[0].address_point_slot, 2);
    assert_eq!(d.sub_tables[0].entries.len(), 6);
    assert_eq!(d.sub_tables[0].entries[0], VtableEntryDump::OffsetToTop(0));
    assert_eq!(d.sub_tables[0].entries[1], VtableEntryDump::Rtti("_ZTI4Base".into()));
    assert!(matches!(
        &d.sub_tables[0].entries[2],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN4BaseD1Ev"
    ));
    assert!(matches!(
        &d.sub_tables[0].entries[3],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN4BaseD0Ev"
    ));
    assert!(matches!(
        &d.sub_tables[0].entries[4],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN4Base6renderEv"
    ));
    assert!(matches!(
        &d.sub_tables[0].entries[5],
        VtableEntryDump::FunctionPointer(s) if s == "_ZNK4Base4areaEv"
    ));
}

#[test]
fn vtable_derived_golden_shows_final_overrider_inheritance() {
    let d = load("vtable_derived");
    assert_eq!(d.class, "Derived");
    assert_eq!(d.sub_tables[0].entries.len(), 6);
    // render is overridden → points at Derived's definition.
    assert!(matches!(
        &d.sub_tables[0].entries[4],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN7Derived6renderEv"
    ));
    // area is inherited → final overrider is Base's definition.
    assert!(matches!(
        &d.sub_tables[0].entries[5],
        VtableEntryDump::FunctionPointer(s) if s == "_ZNK4Base4areaEv"
    ));
}

#[test]
fn vtable_pure_virtual_golden_is_valid() {
    let d = load("vtable_pure_virtual");
    assert_eq!(d.class, "Abstract");
    assert_eq!(d.sub_tables[0].entries.len(), 6);
    // Dtor pair is defined → real mangled symbols.
    assert!(matches!(
        &d.sub_tables[0].entries[2],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN8AbstractD1Ev"
    ));
    // Pure virtual slots → `__cxa_pure_virtual`.
    assert!(matches!(
        &d.sub_tables[0].entries[4],
        VtableEntryDump::FunctionPointer(s) if s == "__cxa_pure_virtual"
    ));
    assert!(matches!(
        &d.sub_tables[0].entries[5],
        VtableEntryDump::FunctionPointer(s) if s == "__cxa_pure_virtual"
    ));
}

#[test]
fn vtable_multi_level_golden_shows_leaf_overriders_only() {
    let d = load("vtable_multi_level");
    assert_eq!(d.class, "Leaf");
    assert_eq!(d.sub_tables[0].entries.len(), 7);
    // Slots 2..=6 all point at Leaf's override; Middle's versions must
    // not appear.
    for i in 2..=6 {
        if let VtableEntryDump::FunctionPointer(s) = &d.sub_tables[0].entries[i] {
            assert!(
                s.contains("4Leaf"),
                "slot {i} should resolve to a Leaf symbol, got {s:?}"
            );
            assert!(
                !s.contains("Middle"),
                "slot {i} leaks a Middle override ({s:?})"
            );
        } else {
            panic!("slot {i} should be a function pointer, got {:?}", d.sub_tables[0].entries[i]);
        }
    }
}

#[test]
fn vtable_middle_golden_is_valid() {
    let d = load("vtable_middle");
    assert_eq!(d.class, "Middle");
    assert_eq!(d.sub_tables[0].entries.len(), 6);
    // Dtor pair resolves to Middle (most-derived at query time).
    assert!(matches!(
        &d.sub_tables[0].entries[2],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN6MiddleD1Ev"
    ));
    // Middle overrides Root::a.
    assert!(matches!(
        &d.sub_tables[0].entries[4],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN6Middle1aEv"
    ));
    // Middle introduces b.
    assert!(matches!(
        &d.sub_tables[0].entries[5],
        VtableEntryDump::FunctionPointer(s) if s == "_ZN6Middle1bEv"
    ));
}

#[test]
fn vtable_middle_matches_clang() {
    let expected = load("vtable_middle");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_middle(&mut ctx);
    let vt = ctx.vtable(class_id).expect("Middle is polymorphic");
    let actual = vtable_to_dump(
        &ctx,
        &vt,
        String::from("Middle"),
        expected.target.clone(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn vtable_corpus_builders_compile() {
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let _ = build_base(&mut ctx);
    let _ = build_base_and_derived(&mut ctx);
    let _ = build_abstract(&mut ctx);
    let _ = build_leaf_chain(&mut ctx);
    let _ = build_middle(&mut ctx);
}

// -------- vtable() diff tests (unignore when docs §7 lands) ------------

#[test]
fn vtable_simple_matches_clang() {
    let expected = load("vtable_simple");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_base(&mut ctx);
    let vt = ctx.vtable(class_id).expect("Base is polymorphic");
    let actual = vtable_to_dump(
        &ctx,
        &vt,
        String::from("Base"),
        expected.target.clone(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn vtable_derived_matches_clang() {
    let expected = load("vtable_derived");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_base_and_derived(&mut ctx);
    let vt = ctx.vtable(class_id).expect("Derived is polymorphic");
    let actual = vtable_to_dump(
        &ctx,
        &vt,
        String::from("Derived"),
        expected.target.clone(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn vtable_pure_virtual_matches_clang() {
    let expected = load("vtable_pure_virtual");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_abstract(&mut ctx);
    let vt = ctx.vtable(class_id).expect("Abstract is polymorphic");
    let actual = vtable_to_dump(
        &ctx,
        &vt,
        String::from("Abstract"),
        expected.target.clone(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn vtable_multi_level_matches_clang() {
    let expected = load("vtable_multi_level");
    let target = target_from_golden(&expected.target).expect("supported target");
    let mut ctx = CxxTypeCtx::new(target);
    let class_id = build_leaf_chain(&mut ctx);
    let vt = ctx.vtable(class_id).expect("Leaf is polymorphic");
    let actual = vtable_to_dump(
        &ctx,
        &vt,
        String::from("Leaf"),
        expected.target.clone(),
    );
    assert_eq!(actual, expected);
}

// -------- Dtor declaration-position regression (v1.13.10) ---------------
//
// Itanium §2.5.2 places vtable components in DECLARATION order; the
// D1/D0 destructor pair occupies the virtual destructor's declaration
// position. The old model pinned the pair to the front (and dropped
// dtors introduced off-root entirely). All three shapes below are
// pinned against `clang++ -fdump-vtable-layouts`.

fn virt(name: &str, ret: TypeId, special: Option<SpecialMember>) -> MethodDef {
    MethodDef {
        access: Default::default(),
        name: MethodName::Ident(Ident(name.to_string())),
        sig: sig_no_args(ret, CvQual::default(), special.is_some()),
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special,
    }
}

fn fn_ptrs(ctx: &CxxTypeCtx, class_id: ClassId) -> Vec<String> {
    let vt = ctx.vtable(class_id).expect("polymorphic");
    vt.sub_tables[0]
        .entries
        .iter()
        .filter_map(|e| match e {
            VTableEntry::FunctionPointer { mangled_target, .. } => {
                Some(mangled_target.clone())
            }
            _ => None,
        })
        .collect()
}

/// `struct A { virtual void f(); virtual ~A(); virtual void g(); }`
/// clang: [f, D1, D0, g].
#[test]
fn dtor_pair_sits_at_declaration_position() {
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let void = void_ty(&mut ctx);
    let a = ctx.define_class(ClassDef {
        name: nested(&["A"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            virt("f", void, None),
            virt("~A", void, Some(SpecialMember::Dtor)),
            virt("g", void, None),
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_eq!(
        fn_ptrs(&ctx, a),
        vec!["_ZN1A1fEv", "_ZN1AD1Ev", "_ZN1AD0Ev", "_ZN1A1gEv"],
    );
}

/// `struct R { virtual void rf(); }; struct M : R { virtual ~M();
/// virtual void mh(); }` — dtor introduced at level 1. clang for M:
/// [rf, D1, D0, mh]. The old model dropped the pair entirely here.
#[test]
fn dtor_introduced_mid_chain_keeps_its_position() {
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let void = void_ty(&mut ctx);
    let r = ctx.define_class(ClassDef {
        name: nested(&["R"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![virt("rf", void, None)],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    let m = ctx.define_class(ClassDef {
        name: nested(&["M"]),
        bases: vec![BaseSpec { class: r, virtual_: false, access: Access::Public }],
        fields: Vec::new(),
        methods: vec![
            virt("~M", void, Some(SpecialMember::Dtor)),
            virt("mh", void, None),
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_eq!(
        fn_ptrs(&ctx, m),
        vec!["_ZN1R2rfEv", "_ZN1MD1Ev", "_ZN1MD0Ev", "_ZN1M2mhEv"],
    );
}

/// `struct D : A { ~D() override; virtual void d2(); }` — re-declared
/// dtor overrides IN PLACE (clang for D: [f, D1(D), D0(D), g, d2]).
#[test]
fn redeclared_dtor_overrides_in_place() {
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let void = void_ty(&mut ctx);
    let a = ctx.define_class(ClassDef {
        name: nested(&["A"]),
        bases: Vec::new(),
        fields: Vec::new(),
        methods: vec![
            virt("f", void, None),
            virt("~A", void, Some(SpecialMember::Dtor)),
            virt("g", void, None),
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    let d = ctx.define_class(ClassDef {
        name: nested(&["D"]),
        bases: vec![BaseSpec { class: a, virtual_: false, access: Access::Public }],
        fields: Vec::new(),
        methods: vec![
            virt("~D", void, Some(SpecialMember::Dtor)),
            virt("d2", void, None),
        ],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_eq!(
        fn_ptrs(&ctx, d),
        vec!["_ZN1A1fEv", "_ZN1DD1Ev", "_ZN1DD0Ev", "_ZN1A1gEv", "_ZN1D2d2Ev"],
    );
}
