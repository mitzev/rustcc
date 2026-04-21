//! End-to-end test for the `.hpp` emitter.
//!
//! Writes a consumer `.cpp` that `#include`s the generated header,
//! constructs dummy uses (sizeof / alignof assertions via
//! `static_assert`), and compiles it with `clang++`. If the header is
//! malformed or the opaque-storage contract is wrong (size/align
//! mismatch with `rustc_abi_cxx::layout`), compilation fails.
//!
//! No libclang dependency: the class is hand-built via the IR.

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::{Driver, HeaderGraph};
use rustc_abi_cxx::{
    ClassDef, CvQual, CxxType, CxxTypeCtx, FieldDef, FnSig, Ident, IntWidth,
    MethodDef, MethodName, NameSegment, NestedName, RecordKind, Target, TypeId,
    Virtuality,
};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rustcc_hpp_e2e_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir tmp");
    dir
}

fn write(path: &PathBuf, body: &str) {
    std::fs::write(path, body)
        .unwrap_or_else(|e| panic!("failed writing {}: {e}", path.display()));
}

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
fn generated_header_compiles_and_models_layout_faithfully() {
    let dir = tmpdir("widget");
    let hpp_path = dir.join("widget-cxx.hpp");
    let consumer_path = dir.join("consumer.cpp");
    let obj_path = dir.join("consumer.o");

    // 3 × i32 → expected size 12, align 4 under Itanium on x86_64.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let int_ = int32(&mut ctx);
    let void = void_type(&mut ctx);
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
        methods: vec![
            MethodDef {
                name: MethodName::Ident(Ident("compute".into())),
                sig: sig(vec![int_], int_, true),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            },
            MethodDef {
                name: MethodName::Ident(Ident("tick".into())),
                sig: sig(Vec::new(), void, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            },
        ],
        kind: RecordKind::Class,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });

    let driver = Driver::new(HeaderGraph {
        roots: Vec::new(),
        include_paths: Vec::new(),
        clang_flags: Vec::new(),
    });

    let hpp_src = driver.emit_hpp(&ctx, &[widget]).expect("emit_hpp");
    write(&hpp_path, &hpp_src);

    // Consumer exercises: include the header, static_assert the storage
    // dimensions match what the .hpp generator claimed. If either the
    // header syntax is wrong or the opaque-storage size/align don't
    // match expectations, clang++ fails here.
    let consumer_src = "\
#include \"widget-cxx.hpp\"
static_assert(sizeof(acme::Widget) == 12, \"Widget should be 12 bytes\");
static_assert(alignof(acme::Widget) == 4, \"Widget should align to 4\");
";
    write(&consumer_path, consumer_src);

    let status = Command::new("clang++")
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj_path)
        .arg(&consumer_path)
        .current_dir(&dir)
        .status()
        .expect("spawn clang++ (is it on PATH?)");
    assert!(
        status.success(),
        "consumer failed to compile against generated header. header:\n{hpp_src}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
