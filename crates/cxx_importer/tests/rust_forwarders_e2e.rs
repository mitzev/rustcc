//! End-to-end: the forwarder generator produces Rust source that
//! rustc compiles cleanly, and the resulting object file carries
//! the expected Itanium-mangled symbols. If either fails the pre-
//! fork pipeline can't produce working bodies — this test is the
//! gate that proves it does.
//!
//! These tests run on both stock rustc (CI) and the rustcc fork
//! (local dev). The forwarder generator's record-return shape is
//! ABI-coincidence-on-x86_64 under `extern "C"` (stock-compatible)
//! versus per-target-correct under `extern "C++"` (fork-only). We
//! probe rustc's calling-conventions list at test entry to pick:
//! stock + x86_64 keeps the legacy `__sret`-first-arg shape, the
//! fork (any arch) opts into `extern "C++"`. Stock + aarch64 is
//! unsupported and skipped — see `pick_record_return_abi` below.

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::rust_forwarders::{
    default_rust_name, generate_rust_forwarders, generate_rust_forwarders_with,
    ForwarderConfig, RecordReturnAbi,
};
use rustc_abi_cxx::{
    ClassDef, CxxType, CxxTypeCtx, CvQual, FieldDef, FnSig, Ident, IntWidth,
    MethodDef, MethodName, NameSegment, NestedName, RecordKind, SpecialMember,
    Target, Virtuality,
};

mod common;

/// Probe the active rustc for `extern "C++"` support. The fork
/// accepts the ABI string; stock rustc rejects it with E0703.
/// `RUSTC` env var is honored to mirror what the test bodies use
/// when invoking rustc to compile generated forwarders.
fn rustc_supports_extern_cpp() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let dir = tmpdir("abi_probe");
    let src = dir.join("probe.rs");
    let out = dir.join("probe.rlib");
    if std::fs::write(&src, b"pub unsafe extern \"C++\" fn _x() {}\n").is_err() {
        return false;
    }
    Command::new(&rustc)
        .args(["--edition=2021", "--crate-type", "lib"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decide the record-return ABI shape the test should drive. Stock
/// rustc is restricted to `ExternCExplicitSret` and only works on
/// x86_64 (SysV ABI coincidence). The fork uses `ExternCpp` on any
/// arch. Stock + aarch64 has no working shape — return `None` so
/// the caller skips with a clear message.
fn pick_record_return_abi() -> Option<RecordReturnAbi> {
    if rustc_supports_extern_cpp() {
        Some(RecordReturnAbi::ExternCpp)
    } else if cfg!(target_arch = "x86_64") {
        Some(RecordReturnAbi::ExternCExplicitSret)
    } else {
        None
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_fwd_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn forwarders_compile_with_rustc_and_export_itanium_symbols() {
    // Build a small IR: Point with `new(i32,i32)`, `get_x(&self)`,
    // `translate(&mut self, i32, i32)`.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let i32_ = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let void_ = ctx.intern_type(CxxType::Void);
    let _ = ctx.define_rust_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
        bases: vec![],
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: i32_,
                explicit_align: None,
            },
        ],
        methods: vec![
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("new".into())),
                sig: FnSig {
                    params: vec![i32_, i32_],
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::OtherCtor),
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("get_x".into())),
                sig: FnSig {
                    params: vec![],
                    ret: i32_,
                    cv: CvQual {
                        is_const: true,
                        is_volatile: false,
                    },
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            },
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("translate".into())),
                sig: FnSig {
                    params: vec![i32_, i32_],
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

    let fwd_src = generate_rust_forwarders(&ctx, default_rust_name(&ctx))
        .expect("forwarder gen");

    // Assemble a tiny Rust crate: Point definition + impl + the
    // generated forwarders via include!.
    let dir = tmpdir("point_fwd");
    let lib_rs = dir.join("lib.rs");
    let fwd_rs = dir.join("forwarders.rs");
    // Build a cdylib rather than a staticlib: static archives pull
    // in compiler_builtins's LLVM-bitcode CGUs which system `nm`
    // can't parse (LLVM version skew between Rust nightly and
    // Xcode's nm). A shared lib is a single Mach-O object with
    // clean symbols.
    let obj = dir.join("libpoint.dylib");

    std::fs::write(&fwd_rs, &fwd_src).unwrap();
    let fwd_path_literal = fwd_rs.to_str().expect("utf8 path");
    let mut lib_source = String::new();
    lib_source.push_str("#![crate_type = \"cdylib\"]\n\n");
    lib_source.push_str("#[repr(C)]\n");
    lib_source.push_str("pub struct Point { pub x: i32, pub y: i32 }\n\n");
    lib_source.push_str("impl Point {\n");
    lib_source.push_str("    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }\n");
    lib_source.push_str("    pub fn get_x(&self) -> i32 { self.x }\n");
    lib_source.push_str("    pub fn translate(&mut self, dx: i32, dy: i32) {\n");
    lib_source.push_str("        self.x += dx;\n        self.y += dy;\n    }\n");
    lib_source.push_str("}\n\n");
    // include! takes a string literal naming the file; we embed it
    // verbatim since the path is a temp dir we just created.
    lib_source.push_str(&format!("include!(\"{fwd_path_literal}\");\n"));
    std::fs::write(&lib_rs, lib_source).unwrap();

    // Resolve rustc: honor the RUSTC env var (set by cargo) so we
    // use the same nightly that's building this test.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let status = Command::new(&rustc)
        .args(["--edition", "2021"])
        .args(["--crate-type", "cdylib"])
        .arg("-o")
        .arg(&obj)
        .arg(&lib_rs)
        .current_dir(&dir)
        .output()
        .expect("spawn rustc");
    assert!(
        status.status.success(),
        "rustc failed.\nstderr:\n{}\nforwarders:\n{}",
        String::from_utf8_lossy(&status.stderr),
        fwd_src
    );

    // nm the static library and assert our Itanium symbols are
    // defined. `-U` hides undefined entries on BSD nm so the
    // listing only has what this object exports; `-g` asks for
    // external (globally-visible) symbols, which is what
    // `#[export_name]` produces.
    let nm = Command::new("nm")
        .args(["-g"])
        .arg(&obj)
        .output()
        .expect("spawn nm");
    assert!(
        nm.status.success(),
        "nm failed:\nstderr: {}",
        String::from_utf8_lossy(&nm.stderr)
    );
    let symbols = String::from_utf8_lossy(&nm.stdout);

    // The Itanium mangled name starts with `_Z`. macOS's `nm`
    // prepends an extra leading underscore (`__Z...`) while Linux
    // `nm` prints the ELF symbol verbatim (`_Z...`). Checking for
    // the bare Itanium prefix matches both.
    let required = [
        "_ZN5PointD1Ev",    // ~Point()
        "_ZN5PointC1Eii",   // Point(int, int)
        "_ZNK5Point5get_xEv", // Point::get_x() const
        "_ZN5Point9translateEii", // Point::translate(int, int)
    ];
    for sym in required {
        assert!(
            symbols.contains(sym),
            "missing symbol {sym} — nm output:\n{symbols}\nforwarders:\n{fwd_src}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn record_returned_by_value_roundtrips_across_cxx_boundary() {
    // Record-by-value return: matches Clang's Itanium ABI on the
    // C++ side. Stock-rustc-compatible shape (`extern "C"` +
    // explicit `__sret`) only works on x86_64 SysV. The fork
    // shape (`extern "C++"` + return-by-value) routes sret per-
    // target via the `compute_cxx_abi_info` overlay. Skip if
    // we're on stock rustc + a non-x86_64 target — neither shape
    // is correct there.
    let abi = match pick_record_return_abi() {
        Some(a) => a,
        None => {
            eprintln!(
                "skipping: stock rustc + non-x86_64 has no working \
                 record-return ABI shape; rerun with RUSTC=fork-rustc"
            );
            return;
        }
    };

    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };

    // IR: Point with Point::new(i32, i32) -> Self and
    // Point::translated(&self, i32, i32) -> Point.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let i32_ = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let void_ = ctx.intern_type(CxxType::Void);
    let point_id = ctx.define_rust_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
        bases: vec![],
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: i32_,
                explicit_align: None,
            },
        ],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let point_ty = ctx.intern_type(CxxType::Record(point_id));
    ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("new".into())),
        sig: FnSig {
            params: vec![i32_, i32_],
            ret: void_,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: true,
        },
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: Some(SpecialMember::OtherCtor),
    });
    ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("translated".into())),
        sig: FnSig {
            params: vec![i32_, i32_],
            ret: point_ty,
            cv: CvQual {
                is_const: true,
                is_volatile: false,
            },
            ref_q: None,
            variadic: false,
            noexcept: true,
        },
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: None,
    });

    let fwd_src = generate_rust_forwarders_with(
        &ctx,
        default_rust_name(&ctx),
        ForwarderConfig { record_return_abi: abi },
    )
    .expect("forwarder gen");
    // Sanity: the chosen shape is reflected in the source.
    match abi {
        RecordReturnAbi::ExternCExplicitSret => {
            assert!(
                fwd_src.contains("__sret: *mut Point"),
                "forwarders didn't emit sret arg:\n{fwd_src}"
            );
            assert!(
                fwd_src.contains("::core::ptr::write(__sret,"),
                "forwarders didn't write into sret slot:\n{fwd_src}"
            );
        }
        RecordReturnAbi::ExternCpp => {
            assert!(
                fwd_src.contains("extern \"C++\""),
                "forwarders didn't switch to extern \"C++\":\n{fwd_src}"
            );
            assert!(
                fwd_src.contains("-> Point"),
                "forwarders didn't return Point by value:\n{fwd_src}"
            );
        }
    }

    let dir = tmpdir("point_return");
    let lib_rs = dir.join("lib.rs");
    let fwd_rs = dir.join("forwarders.rs");
    let dylib = dir.join("libpointret.dylib");
    let consumer_cpp = dir.join("consumer.cpp");
    let bin = dir.join("runner");

    std::fs::write(&fwd_rs, &fwd_src).unwrap();
    let fwd_path_literal = fwd_rs.to_str().unwrap();
    let mut lib_source = String::new();
    lib_source.push_str("#![crate_type = \"cdylib\"]\n\n");
    lib_source.push_str("#[repr(C)]\n");
    lib_source.push_str("pub struct Point { pub x: i32, pub y: i32 }\n\n");
    lib_source.push_str("impl Point {\n");
    lib_source
        .push_str("    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }\n");
    lib_source.push_str(
        "    pub fn translated(&self, dx: i32, dy: i32) -> Point {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20Point { x: self.x + dx, y: self.y + dy }\n\
         \x20\x20\x20\x20}\n",
    );
    lib_source.push_str("}\n\n");
    lib_source.push_str(&format!("include!(\"{fwd_path_literal}\");\n"));
    std::fs::write(&lib_rs, lib_source).unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_out = Command::new(&rustc)
        .args(["--edition", "2021"])
        .args(["--crate-type", "cdylib"])
        .arg("-o")
        .arg(&dylib)
        .arg(&lib_rs)
        .current_dir(&dir)
        .output()
        .expect("spawn rustc");
    assert!(
        rustc_out.status.success(),
        "rustc failed:\n{}\n\nforwarders:\n{fwd_src}",
        String::from_utf8_lossy(&rustc_out.stderr)
    );

    // C++ side: declare Point with opaque storage matching the
    // computed layout, construct one via Point(3, 4), call
    // translated(10, 20), return the resulting x (13) as exit code.
    let consumer = r#"
#include <cstdint>
#include <cstdio>
#include <cstring>

class Point {
public:
    Point(int, int);
    ~Point();
    Point translated(int dx, int dy) const;
private:
    alignas(4) unsigned char __rust_storage[8];
};

int main() {
    Point p(3, 4);
    Point t = p.translated(10, 20);
    // `t` is a freshly-constructed value; reinterpret to read its
    // x field the same way the Rust side laid it out. `#[repr(C)]`
    // + `alignof(4)` guarantees agreement.
    int x;
    std::memcpy(&x, reinterpret_cast<const char*>(&t), sizeof(int));
    int y;
    std::memcpy(&y,
                reinterpret_cast<const char*>(&t) + sizeof(int),
                sizeof(int));
    std::fprintf(stdout, "t.x=%d t.y=%d\n", x, y);
    return x + y;  // 13 + 24 = 37
}
"#;
    std::fs::write(&consumer_cpp, consumer).unwrap();

    let compile = Command::new(&tc.compiler)
        .args(["-std=c++17"])
        .arg("-o")
        .arg(&bin)
        .arg(&consumer_cpp)
        .arg(&dylib)
        .args(["-Wl,-rpath,@loader_path"])
        .output()
        .expect("spawn clang++");
    assert!(
        compile.status.success(),
        "clang++ link failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("spawn runner");
    assert_eq!(
        run.status.code(),
        Some(37),
        "expected exit code 37 (13 + 24).\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("t.x=13 t.y=24"),
        "unexpected record contents: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn record_passed_by_value_roundtrips_across_cxx_boundary() {
    // Record-by-value params go through the Itanium caller-
    // destroys convention: the caller allocates a temp, passes its
    // address, and destroys the temp after the call. The forwarder
    // takes `*const T`, `ptr::read`s to take ownership, and hands
    // the owned value to the user's Rust method.

    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let i32_ = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let void_ = ctx.intern_type(CxxType::Void);
    let point_id = ctx.define_rust_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
        bases: vec![],
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: i32_,
                explicit_align: None,
            },
        ],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: None,
    });
    let point_ty = ctx.intern_type(CxxType::Record(point_id));
    // `Point::new(i32, i32) -> Self`
    ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("new".into())),
        sig: FnSig {
            params: vec![i32_, i32_],
            ret: void_,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: true,
        },
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: Some(SpecialMember::OtherCtor),
    });
    // `Point::sum_coords(&self, other: Point) -> i32`
    //   computes self.x + self.y + other.x + other.y
    // Exercises: record-by-value param (`other`) and scalar return.
    ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
        name: MethodName::Ident(Ident("sum_coords".into())),
        sig: FnSig {
            params: vec![point_ty],
            ret: i32_,
            cv: CvQual {
                is_const: true,
                is_volatile: false,
            },
            ref_q: None,
            variadic: false,
            noexcept: true,
        },
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: None,
    });

    let fwd_src = generate_rust_forwarders(&ctx, default_rust_name(&ctx))
        .expect("forwarder gen");
    // Sanity: the generated signature uses *const Point + ptr::read.
    assert!(
        fwd_src.contains("arg0: *const Point"),
        "record param didn't render as *const T:\n{fwd_src}"
    );
    assert!(
        fwd_src.contains("::core::ptr::read(arg0)"),
        "record param didn't ptr::read:\n{fwd_src}"
    );

    let dir = tmpdir("point_param");
    let lib_rs = dir.join("lib.rs");
    let fwd_rs = dir.join("forwarders.rs");
    let dylib = dir.join("libpointparam.dylib");
    let consumer_cpp = dir.join("consumer.cpp");
    let bin = dir.join("runner");

    std::fs::write(&fwd_rs, &fwd_src).unwrap();
    let fwd_path_literal = fwd_rs.to_str().unwrap();
    let mut lib_source = String::new();
    lib_source.push_str("#![crate_type = \"cdylib\"]\n\n");
    lib_source.push_str("#[repr(C)]\n");
    lib_source.push_str("pub struct Point { pub x: i32, pub y: i32 }\n\n");
    lib_source.push_str("impl Point {\n");
    lib_source
        .push_str("    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }\n");
    lib_source.push_str(
        "    pub fn sum_coords(&self, other: Point) -> i32 {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20self.x + self.y + other.x + other.y\n\
         \x20\x20\x20\x20}\n",
    );
    lib_source.push_str("}\n\n");
    lib_source.push_str(&format!("include!(\"{fwd_path_literal}\");\n"));
    std::fs::write(&lib_rs, lib_source).unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_out = Command::new(&rustc)
        .args(["--edition", "2021"])
        .args(["--crate-type", "cdylib"])
        .arg("-o")
        .arg(&dylib)
        .arg(&lib_rs)
        .current_dir(&dir)
        .output()
        .expect("spawn rustc");
    assert!(
        rustc_out.status.success(),
        "rustc failed:\n{}\nforwarders:\n{fwd_src}",
        String::from_utf8_lossy(&rustc_out.stderr)
    );

    // C++ main:
    //   Point a(1, 2);  Point b(10, 20);  int s = a.sum_coords(b);
    //   expect s == 1 + 2 + 10 + 20 == 33
    let consumer = r#"
#include <cstdint>
#include <cstdio>

class Point {
public:
    Point(int, int);
    ~Point();
    int sum_coords(Point other) const;
private:
    alignas(4) unsigned char __rust_storage[8];
};

int main() {
    Point a(1, 2);
    Point b(10, 20);
    int s = a.sum_coords(b);
    std::fprintf(stdout, "sum=%d\n", s);
    return s;
}
"#;
    std::fs::write(&consumer_cpp, consumer).unwrap();

    let compile = Command::new(&tc.compiler)
        .args(["-std=c++17"])
        .arg("-o")
        .arg(&bin)
        .arg(&consumer_cpp)
        .arg(&dylib)
        .args(["-Wl,-rpath,@loader_path"])
        .output()
        .expect("spawn clang++");
    assert!(
        compile.status.success(),
        "clang++ link failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("spawn runner");
    assert_eq!(
        run.status.code(),
        Some(33),
        "expected exit code 33 (1+2+10+20).\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("sum=33"), "unexpected: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn panicking_rust_method_aborts_instead_of_unwinding_through_cxx() {
    // Soundness gate: a Rust panic crossing an `extern "C"` boundary
    // is UB. The forwarder's `__rustcc_guard` wraps every body in
    // `catch_unwind` + `abort()`, so the C++ caller sees a clean
    // process abort rather than undefined behavior. This test drives
    // a panicking method end-to-end and verifies the process died
    // via SIGABRT.
    use std::os::unix::process::ExitStatusExt as _;

    let tc = match common::find_cxx() {
        Some(c) => c,
        None => {
            eprintln!("skip: no C++ compiler available");
            return;
        }
    };

    // Build IR: Boom { } with `boom(&self)` that panics in the Rust
    // body.
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let void_ = ctx.intern_type(CxxType::Void);
    let i32_ = ctx.intern_type(CxxType::Int {
        signed: true,
        width: IntWidth::I32,
    });
    let _ = ctx.define_rust_class(ClassDef {
        name: NestedName(vec![NameSegment::Class(Ident("Boom".into()))]),
        bases: vec![],
        fields: vec![FieldDef {
            name: Ident("_pad".into()),
            ty: i32_,
            explicit_align: None,
        }],
        methods: vec![
            // `new()` → Self; makes a default-constructed Boom.
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("new".into())),
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
                special: Some(SpecialMember::DefaultCtor),
            },
            // `boom(&self)` — body panics in the user crate.
            MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("boom".into())),
                sig: FnSig {
                    params: vec![],
                    ret: void_,
                    cv: CvQual {
                        is_const: true,
                        is_volatile: false,
                    },
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

    let fwd_src =
        generate_rust_forwarders(&ctx, default_rust_name(&ctx))
            .expect("forwarder gen");

    let dir = tmpdir("panic_abort");
    let lib_rs = dir.join("lib.rs");
    let fwd_rs = dir.join("forwarders.rs");
    let lib_dylib = dir.join("libboom.dylib");
    let consumer_cpp = dir.join("consumer.cpp");
    let bin = dir.join("runner");

    std::fs::write(&fwd_rs, &fwd_src).unwrap();
    let fwd_path_literal = fwd_rs.to_str().unwrap();
    let mut lib_source = String::new();
    lib_source.push_str("#![crate_type = \"cdylib\"]\n\n");
    lib_source.push_str("#[repr(C)]\n");
    lib_source.push_str("pub struct Boom { _pad: i32 }\n\n");
    lib_source.push_str("impl Boom {\n");
    lib_source.push_str("    pub fn new() -> Self { Boom { _pad: 0 } }\n");
    // Deliberately panic on call — this is what the guard must
    // convert into abort().
    lib_source.push_str(
        "    pub fn boom(&self) { panic!(\"rust body panicked\") }\n",
    );
    lib_source.push_str("}\n\n");
    lib_source
        .push_str(&format!("include!(\"{fwd_path_literal}\");\n"));
    std::fs::write(&lib_rs, lib_source).unwrap();

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_out = Command::new(&rustc)
        .args(["--edition", "2021"])
        .args(["--crate-type", "cdylib"])
        .arg("-o")
        .arg(&lib_dylib)
        .arg(&lib_rs)
        .current_dir(&dir)
        .output()
        .expect("spawn rustc");
    assert!(
        rustc_out.status.success(),
        "rustc failed:\nstderr: {}\nforwarders:\n{fwd_src}",
        String::from_utf8_lossy(&rustc_out.stderr)
    );

    // Minimal C++ runner that just calls the mangled symbols.
    // Decl via `extern "C++"` block mirrors what the generated .hpp
    // would produce — we inline it here to keep the test
    // self-contained.
    let consumer = r#"
#include <cstdint>
#include <cstdio>

class Boom {
public:
    Boom();
    ~Boom();
    void boom() const;
private:
    alignas(4) unsigned char __rust_storage[4];
};

int main() {
    Boom b;
    std::fprintf(stderr, "about to call boom()\n");
    b.boom();  // must abort — Rust body panics
    std::fprintf(stderr, "SHOULD NOT REACH: forwarder failed to abort\n");
    return 0;
}
"#;
    std::fs::write(&consumer_cpp, consumer).unwrap();

    // Link against the Rust dylib. On macOS the rpath dance is
    // needed so the binary can find the dylib at runtime.
    let compile = Command::new(&tc.compiler)
        .args(["-std=c++17"])
        .arg("-o")
        .arg(&bin)
        .arg(&consumer_cpp)
        .arg(&lib_dylib)
        .args(["-Wl,-rpath,@loader_path"])
        .output()
        .expect("spawn clang++");
    assert!(
        compile.status.success(),
        "clang++ link failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("spawn runner");
    // Expect: killed by signal (SIGABRT = 6). `ExitStatus::signal()`
    // returns Some(signal_number) when the process died from a
    // signal. `status.success()` must be false.
    assert!(
        !run.status.success(),
        "runner succeeded — forwarder failed to abort on Rust panic.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let signal = run.status.signal();
    assert_eq!(
        signal,
        Some(6),
        "expected SIGABRT (6); got exit_code={:?}, signal={:?}.\n\
         stderr: {}\n\
         This usually means the panic crossed `extern \"C\"` without \
         being caught — check that __rustcc_guard is wrapping bodies.",
        run.status.code(),
        signal,
        String::from_utf8_lossy(&run.stderr),
    );

    let _ = std::fs::remove_dir_all(&dir);
}
