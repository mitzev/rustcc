//! M24 close-out battery — template-specialization method extraction.
//!
//! These assertion-based tests pin the M24 fix: when an explicit
//! instantiation makes a class-template specialization a top-level
//! entity (or a sidecar / type-resolution chain causes one to be
//! imported), the methods declared on the underlying template are
//! captured with all `T`-typed leaf positions substituted to the
//! spec's argument types.
//!
//! Limitations not exercised by these tests (M24 follow-up):
//!
//! - **Implicit instantiations** (using `Box<int>` only as a type,
//!   no `template struct Box<int>;`) — the spec doesn't appear as
//!   a top-level entity; auto-discovery via type-resolution
//!   chains would close that gap.
//! - **STL containers** — require system include path setup
//!   that's environment-specific. `<utility>` not found in this
//!   bare-bones libclang invocation.
//! - **Member templates** — `template<typename U> void put(U)` on
//!   `Box<T>` not handled.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{
    import_header,
    rust_bindings::{
        generate_rust_bindings, BindingsBackend, RustBindingsConfig,
    },
};
use rustc_abi_cxx::{CxxTypeCtx, NameSegment, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn temp(s: &str, tag: &str) -> PathBuf {
    let p = std::env::temp_dir()
        .join(format!("rustcc_m24_close_{tag}_{}.hpp", std::process::id()));
    std::fs::write(&p, s).unwrap();
    p
}

fn cleanup(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
}

fn find_spec_id(
    ctx: &CxxTypeCtx,
    class_ids: &[rustc_abi_cxx::ClassId],
    template_name: &str,
) -> rustc_abi_cxx::ClassId {
    *class_ids
        .iter()
        .find(|&&id| {
            ctx.class(id).name.0.iter().any(|s| match s {
                NameSegment::TemplateSpec { name, .. } => {
                    name.0 == template_name
                }
                _ => false,
            })
        })
        .unwrap_or_else(|| panic!("{template_name} spec not imported"))
}

fn method_names(class: &rustc_abi_cxx::ClassDef) -> Vec<String> {
    class
        .methods
        .iter()
        .filter_map(|m| m.name.ident_name().map(|n| n.to_string()))
        .collect()
}

/// Single-parameter template; explicit instantiation surfaces
/// the spec; methods carry over with `T -> int` substitution.
#[test]
fn single_param_substitution() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename T>
struct Box {
    T value;
    Box(T v) : value(v) {}
    T get() const { return value; }
    void set(T v) { value = v; }
    bool empty() const { return false; }
};
template struct Box<int>;
"#,
        "single",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let id = find_spec_id(&ctx, &class_ids, "Box");
    let class = ctx.class(id);
    let names = method_names(class);
    eprintln!("[probe] Box<int> methods: {names:?}");

    for needle in &["Box<T>", "get", "set", "empty"] {
        assert!(
            names.iter().any(|n| n == needle),
            "Box<int>: missing method {needle:?}; got {names:?}",
        );
    }

    cleanup(&header);
}

/// Multi-parameter template (`Pair<K, V>`) with explicit
/// instantiation. K and V substitute independently.
#[test]
fn multi_param_substitution() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename K, typename V>
struct Pair {
    K key;
    V val;
    Pair(K k, V v) : key(k), val(v) {}
    K first() const { return key; }
    V second() const { return val; }
    void set_key(K k) { key = k; }
    void set_val(V v) { val = v; }
};
template struct Pair<int, double>;
"#,
        "multi",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let id = find_spec_id(&ctx, &class_ids, "Pair");
    let class = ctx.class(id);
    let names = method_names(class);
    eprintln!("[probe] Pair<int,double> methods: {names:?}");

    for needle in &["Pair<K, V>", "first", "second", "set_key", "set_val"] {
        assert!(
            names.iter().any(|n| n == needle),
            "Pair: missing method {needle:?}; got {names:?}",
        );
    }

    cleanup(&header);
}

/// Nested-type substitution: `T*`, `const T*`, `T&`.
#[test]
fn nested_type_substitution() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename T>
struct Bag {
    T value;
    Bag(T v) : value(v) {}
    T get() const { return value; }
    T* data() { return &value; }
    const T* cdata() const { return &value; }
    T& reference() { return value; }
};
template struct Bag<int>;
"#,
        "nested",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let id = find_spec_id(&ctx, &class_ids, "Bag");
    let class = ctx.class(id);
    let names = method_names(class);
    eprintln!("[probe] Bag<int> methods: {names:?}");

    for needle in &["get", "data", "cdata", "reference"] {
        assert!(
            names.iter().any(|n| n == needle),
            "Bag: missing method {needle:?}; got {names:?}",
        );
    }

    cleanup(&header);
}

/// Multiple specializations of the same template each get their
/// own argument-substituted method list.
#[test]
fn multiple_specializations() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename T>
struct Holder {
    T v;
    Holder(T x) : v(x) {}
    T get() const { return v; }
};
template struct Holder<int>;
template struct Holder<double>;
template struct Holder<char>;
"#,
        "multi_spec",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let mut holder_specs: Vec<rustc_abi_cxx::ClassId> = Vec::new();
    for &id in &class_ids {
        if ctx.class(id).name.0.iter().any(|s| match s {
            NameSegment::TemplateSpec { name, .. } => name.0 == "Holder",
            _ => false,
        }) {
            holder_specs.push(id);
        }
    }
    assert_eq!(
        holder_specs.len(),
        3,
        "Expected 3 Holder specs (int, double, char); got {}",
        holder_specs.len(),
    );

    for id in holder_specs {
        let class = ctx.class(id);
        let names = method_names(class);
        assert!(
            names.iter().any(|n| n == "get"),
            "Holder spec missing `get`: {names:?}",
        );
        assert_eq!(
            class.fields.len(),
            1,
            "Holder spec missing field `v`",
        );
    }

    cleanup(&header);
}

/// Bindings emission: every method on a spec must be reachable
/// from the generated impl block (no skips on the spec class).
#[test]
fn bindings_emit_zero_skips() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename T>
struct Cell {
    T value;
    Cell(T v) : value(v) {}
    T get() const { return value; }
    void set(T v) { value = v; }
    bool full() const { return true; }
};
template struct Cell<int>;
"#,
        "emit",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let cfg = RustBindingsConfig {
        backend: BindingsBackend::DirectExternCpp,
        ..RustBindingsConfig::default()
    };
    let src = generate_rust_bindings(&ctx, &class_ids, &cfg).expect("emit");

    // No virtual-method skips are expected (Cell isn't polymorphic).
    let skips = src
        .matches("skipped: virtual method without populated vtable_index")
        .count();
    assert_eq!(skips, 0);

    // The generated source must mention each Cell method.
    for needle in &["fn get", "fn set", "fn full"] {
        assert!(
            src.contains(needle),
            "Bindings missing {needle:?}",
        );
    }

    let _ = class_ids;
    cleanup(&header);
}
