//! P09.x (1.13.7): the importer emits `#[rustc_cxx_imported_vtable]` on
//! a polymorphic C++ class so a Rust `class D : CppBase` can subclass it
//! with cross-boundary virtual dispatch (and, with a virtual destructor,
//! cross-boundary `delete`). This test checks the *generated Rust
//! source* shape; it needs libclang but not the rustcc fork rustc.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::import_header;
use cxx_importer::rust_bindings::{generate_rust_bindings, BindingsBackend, RustBindingsConfig};
use rustc_abi_cxx::{CxxTypeCtx, Target};

/// `Clang::new()` errors out on a second concurrent instance.
static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_imported_vtable_{tag}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host_target() -> Target {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Target::aarch64_apple_darwin()
        } else {
            Target::x86_64_apple_darwin()
        }
    } else if cfg!(target_arch = "aarch64") {
        Target::aarch64_unknown_linux_gnu()
    } else {
        Target::x86_64_unknown_linux_gnu()
    }
}

fn emit(header_src: &str, tag: &str) -> String {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir(tag);
    let header = dir.join("h.hpp");
    std::fs::write(&header, header_src).unwrap();
    let mut ctx = CxxTypeCtx::new(host_target());
    let ids = import_header(&header, &["-x", "c++", "-std=c++17"], &mut ctx).expect("import");
    generate_rust_bindings(
        &ctx,
        &ids,
        &RustBindingsConfig { backend: BindingsBackend::DirectExternCpp, ..Default::default() },
    )
    .expect("emit")
}

/// Concrete + pure virtual, non-virtual destructor: the attribute lists
/// both method slots in C++ vtable order; no `vdtor` record.
#[test]
fn polymorphic_class_emits_imported_vtable_attribute() {
    // Plain `int` keeps the header parseable without a C++ sysroot.
    let src = "struct CppBase {\n  int x;\n  explicit CppBase(int x_);\n  virtual int foo();\n  virtual int describe() = 0;\n};\n";
    let out = emit(src, "concrete");

    assert!(
        out.contains("#[rustc_cxx_imported_vtable = \""),
        "expected imported-vtable attribute, got:\n{out}"
    );
    assert!(out.contains("slot=foo,_ZN7CppBase3fooEv"), "missing concrete foo slot:\n{out}");
    assert!(
        out.contains("slot=describe,__cxa_pure_virtual"),
        "pure virtual should slot __cxa_pure_virtual:\n{out}"
    );
    assert!(!out.contains("vdtor=1"), "non-virtual dtor must NOT mark vdtor:\n{out}");
    assert!(out.contains("zti=_ZTI7CppBase"), "missing base typeinfo symbol:\n{out}");
}

/// Virtual destructor: the attribute carries `vdtor=1`, and the Rust
/// `Drop` targets the base-object destructor `D2` (subobject-safe), not
/// the complete-object `D1`.
#[test]
fn virtual_destructor_marks_vdtor_and_uses_base_object_dtor() {
    let src = "struct CppBase {\n  int x;\n  explicit CppBase(int x_);\n  virtual ~CppBase();\n  virtual int foo();\n  virtual int describe() = 0;\n};\n";
    let out = emit(src, "vdtor");

    assert!(out.contains("vdtor=1"), "virtual dtor must mark vdtor=1:\n{out}");
    assert!(out.contains("slot=foo,_ZN7CppBase3fooEv"), "missing foo slot:\n{out}");
    // The dtor slots are encoded by `vdtor=1`, not listed individually.
    assert!(
        !out.contains("slot=drop,") && !out.contains("slot=~"),
        "dtor must not be emitted as a named slot:\n{out}"
    );
    // Base-object destructor D2 (not complete-object D1) for the subobject.
    assert!(
        out.contains("_ZN7CppBaseD2Ev"),
        "polymorphic base Drop must call the base-object dtor D2:\n{out}"
    );
    assert!(
        !out.contains("_ZN7CppBaseD1Ev"),
        "polymorphic base Drop must NOT call the complete-object dtor D1:\n{out}"
    );
}

/// Non-public virtuals occupy vtable slots and drive final-overrider
/// resolution even though they get no callable wrappers — the FLTK
/// shape: a pure public root virtual overridden by a *protected*
/// mid-chain virtual, plus protected hook virtuals between public ones
/// (`Fl_Group::on_insert/on_move/on_remove` sit between `as_gl_window`
/// and `delete_child` in the real Fl vtable). Dropping them used to
/// shift every later slot index and misresolve `draw` to
/// `__cxa_pure_virtual`, mis-classifying the class as abstract.
#[test]
fn non_public_virtuals_keep_their_vtable_slots() {
    let src = "\
struct Root {\n\
  int x;\n\
  explicit Root(int x_);\n\
  virtual ~Root();\n\
  virtual void draw() = 0;\n\
  virtual int handle(int e);\n\
};\n\
struct Mid : Root {\n\
  explicit Mid(int x_);\n\
protected:\n\
  void draw() override;\n\
  virtual int hook_a(int v);\n\
  virtual void hook_b();\n\
public:\n\
  virtual int tail();\n\
};\n";
    let out = emit(src, "nonpublic");

    // Mid's flattened primary vtable (after the D1/D0 pair):
    //   draw (final overrider = protected Mid::draw — CONCRETE),
    //   handle, hook_a, hook_b, tail — in exactly this order.
    let attr_line = out
        .lines()
        .find(|l| l.contains("rustc_cxx_imported_vtable") && l.contains("_ZTV3Mid"))
        .expect("Mid must carry an imported-vtable attribute");

    // 1. The protected override is the final overrider — NOT pure.
    assert!(
        attr_line.contains("slot=draw,_ZN3Mid4drawEv"),
        "protected Mid::draw must be draw's final overrider:\n{attr_line}"
    );
    assert!(
        !attr_line.contains("slot=draw,__cxa_pure_virtual"),
        "draw must not fall back to __cxa_pure_virtual:\n{attr_line}"
    );

    // 2. Protected hooks occupy their slots, in declaration order,
    //    BETWEEN the public methods.
    let pos = |needle: &str| {
        attr_line
            .find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in:\n{attr_line}"))
    };
    let p_handle = pos("slot=handle,");
    let p_hook_a = pos("slot=hook_a,_ZN3Mid6hook_aEi");
    let p_hook_b = pos("slot=hook_b,_ZN3Mid6hook_bEv");
    let p_tail = pos("slot=tail,");
    assert!(
        p_handle < p_hook_a && p_hook_a < p_hook_b && p_hook_b < p_tail,
        "slot order must be handle < hook_a < hook_b < tail:\n{attr_line}"
    );

    // 3. The protected override makes Mid concrete: the inherited-vdtor
    //    Drop must be emitted (it is suppressed for abstract classes).
    assert!(
        out.contains("__cxx_Mid_inherited_dtor"),
        "Mid is concrete (protected draw override) so the inherited-dtor \
         Drop must be emitted:\n{out}"
    );

    // 4. No callable wrappers for the protected members.
    assert!(
        !out.contains("fn hook_a") && !out.contains("fn hook_b"),
        "protected virtuals must not get callable Rust wrappers:\n{out}"
    );
}

/// Dtor at its DECLARATION position (not first) + operator virtuals:
/// the attr carries a positional `slot=~dtor,~` marker and `~op<N>`
/// placeholders so no slot index ever shifts. A dtor-FIRST class (the
/// FLTK shape) keeps the legacy flag-only form for fork back-compat.
#[test]
fn dtor_position_and_operator_slots_are_positional() {
    let src = "\
struct NotFirst {\n\
  int x;\n\
  explicit NotFirst(int x_);\n\
  virtual int early();\n\
  virtual ~NotFirst();\n\
  virtual bool operator==(const NotFirst& o) const;\n\
  virtual int late();\n\
};\n";
    let out = emit(src, "dtorpos");
    let attr = out
        .lines()
        .find(|l| l.contains("rustc_cxx_imported_vtable") && l.contains("_ZTV8NotFirst"))
        .expect("NotFirst must carry an imported-vtable attribute");

    // Records in C++ declaration order: early, ~dtor marker, ~op0
    // (operator==), late.
    let p = |needle: &str| {
        attr.find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in:\n{attr}"))
    };
    let p_early = p("slot=early,_ZN8NotFirst5earlyEv");
    let p_dtor = p("slot=~dtor,~");
    let p_op = p("slot=~op0,_ZNK8NotFirsteqERKS_");
    let p_late = p("slot=late,_ZN8NotFirst4lateEv");
    assert!(
        p_early < p_dtor && p_dtor < p_op && p_op < p_late,
        "positional order must be early < ~dtor < ~op0 < late:\n{attr}"
    );
    assert!(attr.contains("vdtor=1"), "vdtor flag still required:\n{attr}");

    // Control: a dtor-FIRST class emits NO ~dtor marker (legacy form).
    let src_first = "\
struct DtorFirst {\n\
  int x;\n\
  explicit DtorFirst(int x_);\n\
  virtual ~DtorFirst();\n\
  virtual int only();\n\
};\n";
    let out2 = emit(src_first, "dtorfirst");
    let attr2 = out2
        .lines()
        .find(|l| l.contains("rustc_cxx_imported_vtable") && l.contains("_ZTV9DtorFirst"))
        .expect("DtorFirst attr");
    assert!(attr2.contains("vdtor=1"), "{attr2}");
    assert!(
        !attr2.contains("~dtor"),
        "dtor-first must use the legacy flag-only form:\n{attr2}"
    );
}

/// Multiple inheritance — direct or anywhere up the chain — must
/// suppress the attr entirely: the format models one non-virtual
/// primary chain, and emitting a linearized first-base-only attr gave
/// a Rust subclass a vtable with no secondary sub-tables (UB through
/// the second base). wxWidgets-shaped regression (wxEvtHandler :
/// wxObject + wxTrackable sits under every widget).
#[test]
fn multiple_inheritance_suppresses_the_attr() {
    let src = "\
struct A { int a; virtual int fa(); };\n\
struct B { int b; virtual int fb(); };\n\
struct C : A, B { explicit C(int v); virtual int fc(); };\n\
struct D : C { virtual int fd(); };\n\
struct Single : A { virtual int fs(); };\n";
    let out = emit(src, "mi_guard");
    assert!(
        !out.contains("ztv=_ZTV1C") && !out.contains("ztv=_ZTV1D"),
        "MI class (direct or inherited) must not carry an imported-vtable attr:\n{out}"
    );
    assert!(
        out.contains("ztv=_ZTV6Single"),
        "single-inheritance sibling must still get its attr:\n{out}"
    );
}

/// v1.14 phase 1: member-function-pointer params/returns lower, render
/// as `::cxx::CxxMemberFnPtr<Class>`, and methods carrying them are no
/// longer skipped. The mangled link_names carry `M<class>F…E`.
#[test]
fn member_fn_pointers_render_and_mangle() {
    let src = "\
struct Receiver {\n\
  int base;\n\
  explicit Receiver(int b);\n\
  int add(int v);\n\
  virtual int vadd(int v);\n\
};\n\
typedef int (Receiver::*AddFn)(int);\n\
struct Caller {\n\
  int dummy;\n\
  static int invoke(Receiver* r, AddFn f, int v);\n\
  static AddFn get_add();\n\
};\n";
    let out = emit(src, "memfnptr");
    assert!(
        out.contains("::cxx::CxxMemberFnPtr<Receiver>"),
        "member-ptr params must render as CxxMemberFnPtr<Receiver>:\n{out}"
    );
    assert!(
        out.contains("MS0_FiiE"),
        "link_names must carry the Itanium M-encoding (substituted class ref):\n{out}"
    );
    assert!(
        out.contains("pub fn invoke(") && out.contains("pub fn get_add("),
        "member-ptr-taking/returning methods must not be skipped:\n{out}"
    );
}
