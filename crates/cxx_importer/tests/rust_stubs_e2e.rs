//! End-to-end: generated `.hpp` + rust-stubs `.cpp` compile and LINK
//! together with a consumer `.cpp`, producing a runnable binary. The
//! binary then exercises a stubbed method and is expected to abort
//! (because the stub always aborts). The test verifies (a) the link
//! actually resolves every declared symbol and (b) the abort path
//! runs as designed.

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::hpp::generate_hpp_for_rust_types;
use cxx_importer::rust_stubs::generate_rust_stub_shims;
use rustc_abi_cxx::{
    ClassDef, CxxType, CxxTypeCtx, CvQual, FieldDef, FnSig, Ident, IntWidth,
    MethodDef, MethodName, NameSegment, NestedName, RecordKind, Target,
    Virtuality,
};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_stubs_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn hpp_plus_stubs_compile_link_and_abort_on_call() {
    // Build a tiny IR: one Rust-origin Point type with a method.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let i32_ = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let void_ = ctx.intern_type(CxxType::Void);
    let _ = ctx.define_rust_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Counter".into()))]),
        bases: vec![],
        fields: vec![FieldDef {
            name: Ident("n".into()),
            ty: i32_,
            explicit_align: None,
        }],
        methods: vec![
            MethodDef {
                name: MethodName::Ident(Ident("Counter".into())),
                sig: FnSig {
                    params: vec![],
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(rustc_abi_cxx::SpecialMember::DefaultCtor),
            },
            MethodDef {
                name: MethodName::Ident(Ident("bump".into())),
                sig: FnSig {
                    params: vec![],
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
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
    });

    let hpp = generate_hpp_for_rust_types(&ctx).expect("hpp");
    let stubs = generate_rust_stub_shims(&ctx, "counter-cxx.hpp").expect("stubs");

    let dir = tmpdir("counter");
    let hpp_path = dir.join("counter-cxx.hpp");
    let stubs_path = dir.join("counter-cxx-stubs.cpp");
    let main_path = dir.join("main.cpp");
    let bin_path = dir.join("demo");

    std::fs::write(&hpp_path, &hpp).unwrap();
    std::fs::write(&stubs_path, &stubs).unwrap();
    let main_src = r#"
#include "counter-cxx.hpp"
int main() {
    Counter c;
    c.bump();
    return 0;
}
"#;
    std::fs::write(&main_path, main_src).unwrap();

    let out = Command::new("clang++")
        .args(["-std=c++17", "-o"])
        .arg(&bin_path)
        .arg(&stubs_path)
        .arg(&main_path)
        .current_dir(&dir)
        .output()
        .expect("spawn clang++");
    assert!(
        out.status.success(),
        "link failed.\nstderr:\n{}\nhpp:\n{hpp}\nstubs:\n{stubs}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Run the binary — expected to abort (signal or nonzero exit).
    // `clang++` doesn't pipe our binary's output to stdout by default,
    // but Command::output captures it for us.
    let run = Command::new(&bin_path).output().expect("spawn demo");
    assert!(
        !run.status.success(),
        "expected the binary to abort; it succeeded. stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("rustcc: call to"),
        "abort message missing — stderr was: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
