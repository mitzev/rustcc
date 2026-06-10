//! MSVC vtable corpus — per-class layout cross-validated against
//! `clang -target x86_64-pc-windows-msvc -fdump-vtable-layouts`.
//!
//! Tiny line-oriented golden format: one `vtable` block per
//! sub-table, each entry tagged `rtti` (COL pointer) or `fn`
//! (function-pointer slot) with the most-derived overrider name
//! in `Class::method` form.

use std::collections::HashMap;
use std::path::PathBuf;

use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, CxxTypeCtx,
    FnSig, Ident, IntWidth, MethodDef, MethodName, NameSegment, NestedName,
    RecordKind, SpecialMember, Target, VTableEntry, Virtuality,
};

#[derive(Debug, Default, Clone)]
struct ExpectedVtable {
    class: String,
    entries: Vec<ExpectedEntry>,
}

#[derive(Debug, Clone)]
enum ExpectedEntry {
    Rtti(String),
    Fn(String),
}

fn parse_vtable_golden(text: &str) -> (String, Vec<ExpectedVtable>) {
    let mut target = String::new();
    let mut tables = Vec::new();
    let mut cur: Option<ExpectedVtable> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("target ") {
            target = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("vtable ") {
            if let Some(v) = cur.take() {
                tables.push(v);
            }
            let mut tokens = rest.split_whitespace();
            let class = tokens.next().unwrap().to_string();
            cur = Some(ExpectedVtable {
                class,
                entries: Vec::new(),
            });
            continue;
        }
        if let (Some(rest), Some(v)) = (line.strip_prefix("entry "), cur.as_mut()) {
            let mut tokens = rest.split_whitespace();
            let _idx = tokens.next();
            let kind = tokens.next().unwrap();
            let name = tokens.collect::<Vec<_>>().join(" ");
            match kind {
                "rtti" => v.entries.push(ExpectedEntry::Rtti(name)),
                "fn" => v.entries.push(ExpectedEntry::Fn(name)),
                other => panic!("unknown vtable entry kind {other:?}"),
            }
        }
    }
    if let Some(v) = cur {
        tables.push(v);
    }
    (target, tables)
}

fn load_vtable_golden(basename: &str) -> Vec<ExpectedVtable> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus_msvc")
        .join(format!("{basename}.vtable.golden"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let (target, tables) = parse_vtable_golden(&text);
    assert_eq!(target, "x86_64-pc-windows-msvc");
    tables
}

fn ctx() -> CxxTypeCtx {
    CxxTypeCtx::new(Target::x86_64_pc_windows_msvc())
}

/// Demangle an MSVC class-method symbol back to `Class::method`
/// form for matching against the golden's human-readable expected
/// entries.
///
/// This is a lossy parser — it covers the cases our vtable corpus
/// uses (ctor/method/dtor on a single class). Multi-namespace
/// scopes and operators aren't decoded; if/when the corpus grows,
/// extend this.
fn demangle_msvc_method(sym: &str) -> Option<String> {
    // ?<name>@<class>@@<info>  -> "<class>::<name>"
    // ??1<class>@@<info>       -> "<class>::~<class>"
    // ??_G<class>@@<info>      -> "<class>::~<class>" (scalar deleting dtor)
    if let Some(rest) = sym.strip_prefix("??1") {
        // ??1Foo@@...
        let class = rest.split('@').next()?;
        return Some(format!("{class}::~{class}"));
    }
    if let Some(rest) = sym.strip_prefix("??_G") {
        let class = rest.split('@').next()?;
        return Some(format!("{class}::~{class}"));
    }
    if let Some(rest) = sym.strip_prefix('?') {
        // ?method@Class@@QEAA...
        let name = rest.split('@').next()?;
        let after = rest.strip_prefix(name)?.strip_prefix('@')?;
        let class = after.split('@').next()?;
        return Some(format!("{class}::{name}"));
    }
    None
}

fn assert_vtable_matches(
    ctx: &CxxTypeCtx,
    class: ClassId,
    expected: &ExpectedVtable,
) {
    let vt = ctx
        .vtable(class)
        .unwrap_or_else(|| panic!("no vtable for {}", expected.class));
    assert_eq!(
        vt.sub_tables.len(),
        1,
        "{}: only single-inheritance subtables expected",
        expected.class
    );
    let primary = &vt.sub_tables[0];
    assert_eq!(
        primary.entries.len(),
        expected.entries.len(),
        "{}: entry count mismatch — got {} entries, expected {}",
        expected.class,
        primary.entries.len(),
        expected.entries.len()
    );

    for (i, (got, exp)) in primary.entries.iter().zip(&expected.entries).enumerate() {
        match (got, exp) {
            (VTableEntry::Rtti(_), ExpectedEntry::Rtti(_class)) => {
                // RTTI just needs to be present at the right slot;
                // the exact symbol shape is mangler-territory and
                // covered by mangle_corpus_msvc.
            }
            (
                VTableEntry::FunctionPointer { mangled_target, .. },
                ExpectedEntry::Fn(expected_name),
            ) => {
                let demangled = demangle_msvc_method(mangled_target).unwrap_or_else(
                    || panic!("could not demangle {mangled_target}"),
                );
                // MSVC's scalar-deleting dtor differs from the base
                // dtor — both demangle to `Class::~Class` though,
                // so the golden's `Class::~Class` matches either
                // `??1Class@@...` or `??_GClass@@...`.
                assert_eq!(
                    demangled, *expected_name,
                    "{}: entry {i} mismatch — got `{demangled}` ({mangled_target}), expected `{expected_name}`",
                    expected.class
                );
            }
            _ => panic!(
                "{}: entry {i} kind mismatch — got {got:?}, expected {exp:?}",
                expected.class
            ),
        }
    }
}

// -------- vtable_simple ----------------------------------------------

#[test]
fn msvc_vtable_simple_matches_clang() {
    let goldens = load_vtable_golden("vtable_simple");
    let by_name: HashMap<&str, &ExpectedVtable> =
        goldens.iter().map(|v| (v.class.as_str(), v)).collect();

    let mut c = ctx();
    let i = c.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
    let v = c.intern_type(CxxType::Void);

    let f_method = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("f".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let g_method = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("g".into())),
        sig: FnSig {
            params: vec![i],
            ret: i,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let dtor_a = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("~A".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: Some(SpecialMember::Dtor),
    };
    let a = c.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("A".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![f_method, g_method, dtor_a],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_vtable_matches(&c, a, by_name["A"]);
}

// -------- vtable_inherit ---------------------------------------------

#[test]
fn msvc_vtable_inherit_matches_clang() {
    let goldens = load_vtable_golden("vtable_inherit");
    let by_name: HashMap<&str, &ExpectedVtable> =
        goldens.iter().map(|v| (v.class.as_str(), v)).collect();

    let mut c = ctx();
    let i = c.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
    let v = c.intern_type(CxxType::Void);

    // A
    let f_a = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("f".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let g_a = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("g".into())),
        sig: FnSig {
            params: vec![i],
            ret: i,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let dtor_a = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("~A".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: Some(SpecialMember::Dtor),
    };
    let a = c.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("A".into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![f_a.clone(), g_a.clone(), dtor_a.clone()],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_vtable_matches(&c, a, by_name["A"]);

    // B : A. Methods: g override, h new virtual, ~B virtual.
    let g_b = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("g".into())),
        sig: g_a.sig.clone(),
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let h_b = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("h".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: None,
    };
    let dtor_b = MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("~B".into())),
        sig: FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        },
        virtuality: Virtuality::Virtual,
        vtable_index: None,
        special: Some(SpecialMember::Dtor),
    };
    let b = c.define_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("B".into()))]),
        bases: vec![BaseSpec {
            class: a,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![],
        methods: vec![g_b, h_b, dtor_b],
        kind: RecordKind::Struct,
        is_polymorphic: true,
        is_final: false,
        source_alignment: None,
    });
    assert_vtable_matches(&c, b, by_name["B"]);
}
