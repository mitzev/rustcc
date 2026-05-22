//! MSVC layout corpus — per-class layout cross-validated against
//! `clang -target x86_64-pc-windows-msvc -fdump-record-layouts`.
//!
//! The goldens use a simple line-oriented format (one struct per
//! record-block) that's both human-editable and easy to regenerate.
//! See `fork/tests/refresh-msvc-corpus.sh` for the refresh recipe.
//!
//! Tests build the same record graph by hand against `CxxTypeCtx::new(
//! Target::x86_64_pc_windows_msvc())`, run `ctx.layout(class)`, and
//! diff sizes / alignments / field offsets / base offsets against the
//! golden. Any divergence is a real MSVC ABI bug.

use std::collections::HashMap;
use std::path::PathBuf;

use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CxxType, CxxTypeCtx, FieldDef,
    Ident, IntWidth, NameSegment, NestedName, RecordKind, Target,
};

#[derive(Debug, Default, Clone)]
struct ExpectedRecord {
    name: String,
    size: u64,
    align: u64,
    nvsize: u64,
    nvalign: u64,
    has_vptr: bool,
    bases: Vec<ExpectedBase>,
    fields: Vec<ExpectedField>,
}

#[derive(Debug, Clone)]
struct ExpectedBase {
    name: String,
    offset: u64,
    /// Parsed from `empty=true` markers in the golden but not yet
    /// asserted against. Reserved for an empty-subobject corpus
    /// check that's left to a follow-up; for now we just confirm
    /// the base offset matches.
    #[allow(dead_code)]
    empty: bool,
}

#[derive(Debug, Clone)]
struct ExpectedField {
    name: String,
    offset: u64,
}

fn parse_layout_golden(text: &str) -> (String, Vec<ExpectedRecord>) {
    let mut target = String::new();
    let mut records = Vec::new();
    let mut cur: Option<ExpectedRecord> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("target ") {
            target = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("record ") {
            if let Some(r) = cur.take() {
                records.push(r);
            }
            let mut r = ExpectedRecord::default();
            let mut tokens = rest.split_whitespace();
            r.name = tokens.next().unwrap().to_string();
            for tok in tokens {
                if let Some(v) = tok.strip_prefix("size=") {
                    r.size = v.parse().unwrap();
                } else if let Some(v) = tok.strip_prefix("align=") {
                    r.align = v.parse().unwrap();
                } else if let Some(v) = tok.strip_prefix("nvsize=") {
                    r.nvsize = v.parse().unwrap();
                } else if let Some(v) = tok.strip_prefix("nvalign=") {
                    r.nvalign = v.parse().unwrap();
                } else if tok == "has_vptr=true" {
                    r.has_vptr = true;
                }
            }
            cur = Some(r);
            continue;
        }
        if let (Some(rest), Some(r)) =
            (line.strip_prefix("base "), cur.as_mut())
        {
            let mut tokens = rest.split_whitespace();
            let name = tokens.next().unwrap().to_string();
            let mut offset = 0;
            let mut empty = false;
            for tok in tokens {
                if let Some(v) = tok.strip_prefix("offset=") {
                    offset = v.parse().unwrap();
                } else if tok == "empty=true" {
                    empty = true;
                }
            }
            r.bases.push(ExpectedBase { name, offset, empty });
            continue;
        }
        if let (Some(rest), Some(r)) =
            (line.strip_prefix("field "), cur.as_mut())
        {
            let mut tokens = rest.split_whitespace();
            let name = tokens.next().unwrap().to_string();
            let mut offset = 0;
            for tok in tokens {
                if let Some(v) = tok.strip_prefix("offset=") {
                    offset = v.parse().unwrap();
                }
            }
            r.fields.push(ExpectedField { name, offset });
            continue;
        }
    }
    if let Some(r) = cur {
        records.push(r);
    }
    (target, records)
}

fn load_golden(basename: &str) -> Vec<ExpectedRecord> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus_msvc")
        .join(format!("{basename}.layout.golden"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let (target, records) = parse_layout_golden(&text);
    assert_eq!(target, "x86_64-pc-windows-msvc");
    records
}

fn ctx() -> CxxTypeCtx {
    CxxTypeCtx::new(Target::x86_64_pc_windows_msvc())
}

fn intern_int(ctx: &mut CxxTypeCtx, signed: bool, width: IntWidth) -> rustc_abi_cxx::TypeId {
    ctx.intern_type(CxxType::Int { signed, width })
}

fn class_def(name: &str, kind: RecordKind) -> ClassDef {
    ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident(name.into()))]),
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    }
}

fn assert_record_matches(
    ctx: &CxxTypeCtx,
    class: ClassId,
    expected: &ExpectedRecord,
) {
    let layout = ctx
        .layout(class)
        .unwrap_or_else(|e| panic!("layout failed for {}: {e:?}", expected.name));
    assert_eq!(
        layout.size_bytes, expected.size,
        "{}: size mismatch (got {}, expected {})",
        expected.name, layout.size_bytes, expected.size
    );
    assert_eq!(
        layout.align_bytes, expected.align,
        "{}: align mismatch (got {}, expected {})",
        expected.name, layout.align_bytes, expected.align
    );
    assert_eq!(
        layout.nv_size_bytes, expected.nvsize,
        "{}: nvsize mismatch (got {}, expected {})",
        expected.name, layout.nv_size_bytes, expected.nvsize
    );
    assert_eq!(
        layout.nv_align_bytes, expected.nvalign,
        "{}: nvalign mismatch (got {}, expected {})",
        expected.name, layout.nv_align_bytes, expected.nvalign
    );
    assert_eq!(
        layout.has_vptr, expected.has_vptr,
        "{}: has_vptr mismatch", expected.name
    );
    assert_eq!(
        layout.field_offsets.len(),
        expected.fields.len(),
        "{}: field count mismatch", expected.name
    );
    for (got, exp) in layout.field_offsets.iter().zip(&expected.fields) {
        assert_eq!(
            *got, exp.offset,
            "{}: field `{}` offset mismatch (got {got}, expected {})",
            expected.name, exp.name, exp.offset
        );
    }
    assert_eq!(
        layout.base_offsets.len(),
        expected.bases.len(),
        "{}: base count mismatch", expected.name
    );
    for ((_, got_off), exp) in layout.base_offsets.iter().zip(&expected.bases) {
        assert_eq!(
            *got_off, exp.offset,
            "{}: base `{}` offset mismatch", expected.name, exp.name
        );
    }
}

// -------- layout_basic ------------------------------------------------

#[test]
fn msvc_layout_basic_matches_clang() {
    let goldens = load_golden("layout_basic");
    let by_name: HashMap<&str, &ExpectedRecord> =
        goldens.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut c = ctx();
    let i = intern_int(&mut c, true, IntWidth::I32);
    let ch = intern_int(&mut c, true, IntWidth::I8);
    let ll = intern_int(&mut c, true, IntWidth::I64);

    // struct S { int a; char b; int c; };
    let s = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("c".into()), ty: i, explicit_align: None },
        ],
        ..class_def("S", RecordKind::Struct)
    });
    assert_record_matches(&c, s, by_name["S"]);

    // struct Empty {};
    let empty = c.define_class(class_def("Empty", RecordKind::Struct));
    assert_record_matches(&c, empty, by_name["Empty"]);

    // struct Aligned { alignas(16) int x; };
    let aligned = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("x".into()),
            ty: i,
            explicit_align: Some(16),
        }],
        source_alignment: Some(16),
        ..class_def("Aligned", RecordKind::Struct)
    });
    assert_record_matches(&c, aligned, by_name["Aligned"]);

    // struct WithPad { char a; int b; char c; long long d; };
    let with_pad = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("c".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("d".into()), ty: ll, explicit_align: None },
        ],
        ..class_def("WithPad", RecordKind::Struct)
    });
    assert_record_matches(&c, with_pad, by_name["WithPad"]);
}

// -------- layout_inherit ----------------------------------------------

#[test]
fn msvc_layout_inherit_matches_clang() {
    let goldens = load_golden("layout_inherit");
    let by_name: HashMap<&str, &ExpectedRecord> =
        goldens.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut c = ctx();
    let i = intern_int(&mut c, true, IntWidth::I32);
    let ch = intern_int(&mut c, true, IntWidth::I8);
    let v = c.intern_type(CxxType::Void);
    let _ = v;

    // struct Base { int x; char y; };
    let base = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("x".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("y".into()), ty: ch, explicit_align: None },
        ],
        ..class_def("Base", RecordKind::Struct)
    });
    assert_record_matches(&c, base, by_name["Base"]);

    // struct Derived : Base { char z; };
    let derived = c.define_class(ClassDef {
        bases: vec![BaseSpec { class: base, virtual_: false, access: Access::Public }],
        fields: vec![FieldDef {
            name: Ident("z".into()),
            ty: ch,
            explicit_align: None,
        }],
        ..class_def("Derived", RecordKind::Struct)
    });
    assert_record_matches(&c, derived, by_name["Derived"]);

    // struct Poly { virtual void f(); int x; };
    let poly = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("x".into()),
            ty: i,
            explicit_align: None,
        }],
        is_polymorphic: true,
        ..class_def("Poly", RecordKind::Struct)
    });
    assert_record_matches(&c, poly, by_name["Poly"]);

    // struct PolyDerived : Poly { int y; };
    let poly_derived = c.define_class(ClassDef {
        bases: vec![BaseSpec { class: poly, virtual_: false, access: Access::Public }],
        fields: vec![FieldDef {
            name: Ident("y".into()),
            ty: i,
            explicit_align: None,
        }],
        is_polymorphic: true,
        ..class_def("PolyDerived", RecordKind::Struct)
    });
    assert_record_matches(&c, poly_derived, by_name["PolyDerived"]);

    // struct EmptyBase {};
    let empty_base = c.define_class(class_def("EmptyBase", RecordKind::Struct));
    assert_record_matches(&c, empty_base, by_name["EmptyBase"]);

    // struct WithEmptyBase : EmptyBase { int x; };
    let with_empty_base = c.define_class(ClassDef {
        bases: vec![BaseSpec {
            class: empty_base,
            virtual_: false,
            access: Access::Public,
        }],
        fields: vec![FieldDef {
            name: Ident("x".into()),
            ty: i,
            explicit_align: None,
        }],
        ..class_def("WithEmptyBase", RecordKind::Struct)
    });
    assert_record_matches(&c, with_empty_base, by_name["WithEmptyBase"]);
}

// -------- layout_packed ----------------------------------------------

#[test]
fn msvc_layout_packed_matches_clang() {
    let goldens = load_golden("layout_packed");
    let by_name: HashMap<&str, &ExpectedRecord> =
        goldens.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut c = ctx();
    let i = intern_int(&mut c, true, IntWidth::I32);
    let ch = intern_int(&mut c, true, IntWidth::I8);
    let ll = intern_int(&mut c, true, IntWidth::I64);

    // #pragma pack(1) struct Packed1 { char a; int b; };
    let packed1 = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: i, explicit_align: None },
        ],
        ..class_def("Packed1", RecordKind::Struct)
    });
    c.set_pragma_pack(packed1, 1);
    assert_record_matches(&c, packed1, by_name["Packed1"]);

    // #pragma pack(2) struct Packed2 { char a; int b; long long c; };
    let packed2 = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("c".into()), ty: ll, explicit_align: None },
        ],
        ..class_def("Packed2", RecordKind::Struct)
    });
    c.set_pragma_pack(packed2, 2);
    assert_record_matches(&c, packed2, by_name["Packed2"]);

    // struct Default { char a; int b; long long c; }; (no pragma)
    let default_class = c.define_class(ClassDef {
        fields: vec![
            FieldDef { name: Ident("a".into()), ty: ch, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("c".into()), ty: ll, explicit_align: None },
        ],
        ..class_def("Default", RecordKind::Struct)
    });
    assert_record_matches(&c, default_class, by_name["Default"]);
}

// -------- layout_multi_inherit ---------------------------------------

#[test]
fn msvc_layout_multi_inherit_matches_clang() {
    let goldens = load_golden("layout_multi_inherit");
    let by_name: HashMap<&str, &ExpectedRecord> =
        goldens.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut c = ctx();
    let i = intern_int(&mut c, true, IntWidth::I32);

    // struct A { int a; };
    let a = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("a".into()),
            ty: i,
            explicit_align: None,
        }],
        ..class_def("A", RecordKind::Struct)
    });
    assert_record_matches(&c, a, by_name["A"]);

    // struct B { int b; };
    let b = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("b".into()),
            ty: i,
            explicit_align: None,
        }],
        ..class_def("B", RecordKind::Struct)
    });
    assert_record_matches(&c, b, by_name["B"]);

    // struct C : A, B { int c; };
    let c_class = c.define_class(ClassDef {
        bases: vec![
            BaseSpec { class: a, virtual_: false, access: Access::Public },
            BaseSpec { class: b, virtual_: false, access: Access::Public },
        ],
        fields: vec![FieldDef {
            name: Ident("c".into()),
            ty: i,
            explicit_align: None,
        }],
        ..class_def("C", RecordKind::Struct)
    });
    assert_record_matches(&c, c_class, by_name["C"]);

    // struct VA { virtual void f(); int x; };
    let va = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("x".into()),
            ty: i,
            explicit_align: None,
        }],
        is_polymorphic: true,
        ..class_def("VA", RecordKind::Struct)
    });
    assert_record_matches(&c, va, by_name["VA"]);

    // struct VB { virtual void g(); int y; };
    let vb = c.define_class(ClassDef {
        fields: vec![FieldDef {
            name: Ident("y".into()),
            ty: i,
            explicit_align: None,
        }],
        is_polymorphic: true,
        ..class_def("VB", RecordKind::Struct)
    });
    assert_record_matches(&c, vb, by_name["VB"]);

    // struct VC : VA, VB { int z; };
    let vc = c.define_class(ClassDef {
        bases: vec![
            BaseSpec { class: va, virtual_: false, access: Access::Public },
            BaseSpec { class: vb, virtual_: false, access: Access::Public },
        ],
        fields: vec![FieldDef {
            name: Ident("z".into()),
            ty: i,
            explicit_align: None,
        }],
        is_polymorphic: true,
        ..class_def("VC", RecordKind::Struct)
    });
    assert_record_matches(&c, vc, by_name["VC"]);
}
