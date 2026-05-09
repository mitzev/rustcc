//! M24 exploration probe — template-specialization method extraction.
//!
//! libclang's cursor traversal doesn't surface the instantiated
//! methods of a `ClassTemplateSpecializationDecl`. Fields come
//! through via `Type::get_fields()` with per-field
//! `get_canonical_type()` resolving `T` → `int`. Methods are the
//! gap: needed for STL containers (`std::vector<int>::push_back`,
//! `std::string::c_str`) and any user template instantiated at
//! known argument types.
//!
//! These probes pin the *current* state and explore what data
//! libclang actually does expose so we can pick a strategy.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::import_header;
use rustc_abi_cxx::{CxxTypeCtx, NameSegment, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn temp(s: &str, tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rustcc_m24_{tag}_{}.hpp",
        std::process::id()
    ));
    std::fs::write(&p, s).unwrap();
    p
}

fn cleanup(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
}

/// Probe nested-type substitution (T*, T const*, T&) and
/// multi-parameter templates (Pair<K, V>).
#[test]
fn probe_nested_and_multi_param_substitution() {
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

template<typename K, typename V>
struct Pair {
    K key;
    V val;
    Pair(K k, V v) : key(k), val(v) {}
    K first() const { return key; }
    V second() const { return val; }
};

template struct Bag<int>;
template struct Pair<int, double>;
"#,
        "nested_multi",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    eprintln!("[probe] imported classes:");
    for &id in &class_ids {
        let class = ctx.class(id);
        eprintln!(
            "[probe]   {:?}  fields={} methods={}",
            class.name,
            class.fields.len(),
            class.methods.len(),
        );
        for (i, m) in class.methods.iter().enumerate() {
            eprintln!(
                "[probe]     m[{i}]: name={:?} sig.params={:?}",
                m.name.ident_name(),
                m.sig.params.len(),
            );
        }
    }

    cleanup(&header);
}

/// User-defined template, single specialization. The cleanest
/// case — no STL involved. Uses *explicit instantiation* so the
/// spec appears as a top-level entity (the simplest form M24
/// can target).
#[test]
fn probe_user_template_method_extraction() {
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

// Explicit instantiations make the spec a top-level entity.
template struct Box<int>;
template struct Box<double>;

// Force instantiation by referencing it (these alone don't make
// the spec top-level).
inline Box<int> make_int_box(int v) { return Box<int>(v); }
inline Box<double> make_double_box(double v) { return Box<double>(v); }
"#,
        "user_template",
    );

    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let class_ids = import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    // Find Box<int> specialization (look for TemplateSpec name segment).
    eprintln!("[probe] imported classes:");
    for &id in &class_ids {
        let class = ctx.class(id);
        eprintln!(
            "[probe]   {:?}  fields={} methods={} polymorphic={}",
            class.name,
            class.fields.len(),
            class.methods.len(),
            class.is_polymorphic,
        );
        for (i, m) in class.methods.iter().enumerate() {
            eprintln!(
                "[probe]     m[{i}]: name={:?} virt={:?} sig.params={:?}",
                m.name.ident_name(),
                m.virtuality,
                m.sig.params.len(),
            );
        }
    }

    // Today we expect Box<int> and Box<double> to be imported with
    // fields populated but methods=0 or close. The probe just
    // captures the state.
    cleanup(&header);
}

/// STL `std::vector<int>` — the canonical motivating case.
#[test]
fn probe_std_vector_int() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
#include <vector>
inline std::vector<int> make_vec() { return std::vector<int>(); }
"#,
        "std_vector",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = match import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    ) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("[probe] import failed: {e:?}");
            cleanup(&header);
            return;
        }
    };

    let mut vec_int_id = None;
    for &id in &class_ids {
        let class = ctx.class(id);
        // Look for a TemplateSpec segment whose name is "vector".
        for seg in &class.name.0 {
            if let NameSegment::TemplateSpec { name, .. } = seg {
                if name.0 == "vector" {
                    vec_int_id = Some(id);
                }
            }
        }
    }

    match vec_int_id {
        Some(id) => {
            let class = ctx.class(id);
            eprintln!(
                "[probe] std::vector<int>: fields={} methods={} polymorphic={}",
                class.fields.len(),
                class.methods.len(),
                class.is_polymorphic,
            );
            // Look for canonical methods — push_back, size, empty,
            // operator[], begin, end.
            let names: Vec<&str> = class
                .methods
                .iter()
                .filter_map(|m| m.name.ident_name())
                .collect();
            eprintln!("[probe] std::vector<int> method names: {names:?}");
            for needle in &["push_back", "size", "empty", "begin", "end"] {
                let has = names.iter().any(|n| n == needle);
                eprintln!("[probe]   has {needle}: {has}");
            }
        }
        None => {
            eprintln!("[probe] std::vector<int> not imported as a class");
        }
    }

    cleanup(&header);
}

/// Simpler STL: `std::pair<int, double>`. Less machinery than
/// vector (no allocator), so a clearer signal of where the gap
/// is. Uses explicit instantiation to surface the spec at top
/// level.
#[test]
fn probe_std_pair() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
#include <utility>
template struct std::pair<int, double>;
inline std::pair<int, double> make_pair() { return {1, 2.0}; }
"#,
        "std_pair",
    );
    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let class_ids = match import_header(
        &header,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    ) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("[probe] import failed: {e:?}");
            cleanup(&header);
            return;
        }
    };

    let mut pair_id = None;
    for &id in &class_ids {
        let class = ctx.class(id);
        for seg in &class.name.0 {
            if let NameSegment::TemplateSpec { name, .. } = seg {
                if name.0 == "pair" {
                    pair_id = Some(id);
                }
            }
        }
    }

    if let Some(id) = pair_id {
        let class = ctx.class(id);
        eprintln!(
            "[probe] std::pair<int, double>: fields={} methods={}",
            class.fields.len(),
            class.methods.len(),
        );
        for f in &class.fields {
            eprintln!("[probe]   field: {:?}", f.name);
        }
        for m in &class.methods {
            eprintln!("[probe]   method: {:?}", m.name.ident_name());
        }
    } else {
        eprintln!("[probe] std::pair not imported");
    }

    cleanup(&header);
}

/// What does libclang surface for an STL header that uses
/// explicit instantiation of `std::pair<int, double>`?
#[test]
fn probe_stl_explicit_instantiation_surface() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
#include <utility>
template struct std::pair<int, double>;
"#,
        "stl_inst",
    );

    use clang::{Clang, Index};
    let clang = Clang::new().expect("clang");
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(&header)
        .arguments(&["-x", "c++", "-std=c++17"])
        .parse()
        .expect("parse");

    eprintln!("[probe] === ALL top-level (only first 30) ===");
    let mut shown = 0;
    for top in tu.get_entity().get_children() {
        if shown < 30 {
            eprintln!("[probe] {:?} {:?} display={:?}",
                top.get_kind(),
                top.get_name().unwrap_or_default(),
                top.get_display_name().unwrap_or_default(),
            );
            shown += 1;
        }
    }
    let n_top = tu.get_entity().get_children().len();
    eprintln!("[probe] (total top-level: {n_top})");

    eprintln!("[probe] === diagnostics ===");
    for d in tu.get_diagnostics() {
        eprintln!("[probe]   {:?}", d);
    }

    cleanup(&header);
}

/// What does libclang *actually* expose for a template
/// specialization cursor? Walk children directly so we can see
/// what kinds appear (method? template? function template?).
#[test]
fn probe_libclang_spec_cursor_children() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let header = temp(
        r#"
template<typename T>
struct Box {
    T value;
    Box(T v) : value(v) {}
    T get() const { return value; }
};
inline Box<int> make() { return Box<int>(0); }
template struct Box<int>;  // explicit instantiation
"#,
        "spec_children",
    );

    use clang::{Clang, EntityKind, Index};
    let clang = Clang::new().expect("clang");
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(&header)
        .arguments(&["-x", "c++", "-std=c++17"])
        .parse()
        .expect("parse");

    fn dfs(e: &clang::Entity, depth: usize) {
        let prefix = "  ".repeat(depth);
        eprintln!(
            "[probe] {prefix}{:?} name={:?} kind_repr={:?}",
            e.get_kind(),
            e.get_name().unwrap_or_default(),
            e.get_display_name().unwrap_or_default(),
        );
        if depth > 2 { return; }
        for c in e.get_children() {
            dfs(&c, depth + 1);
        }
    }

    eprintln!("[probe] === ALL TU top-level entities ===");
    for top in tu.get_entity().get_children() {
        eprintln!(
            "[probe] {:?} name={:?} display={:?}",
            top.get_kind(),
            top.get_name().unwrap_or_default(),
            top.get_display_name().unwrap_or_default(),
        );
    }
    let _: () = ();

    eprintln!("[probe] === ClassTemplate / spec full DFS ===");
    for top in tu.get_entity().get_children() {
        match top.get_kind() {
            EntityKind::ClassTemplate
            | EntityKind::ClassDecl
            | EntityKind::StructDecl => {
                eprintln!("[probe] top: {:?} {:?} (is_def={})",
                    top.get_kind(),
                    top.get_name().unwrap_or_default(),
                    top.is_definition(),
                );
                eprintln!("[probe]   get_template returns: {:?}", top.get_template().map(|t| t.get_kind()));
                dfs(&top, 1);
            }
            _ => {}
        }
    }

    // Look specifically for the implicit specialization Box<int>.
    // It should be reachable from `make`'s return type.
    eprintln!("[probe] === make()'s return type analysis ===");
    for top in tu.get_entity().get_children() {
        if top.get_kind() == EntityKind::FunctionDecl
            && top.get_name().as_deref() == Some("make")
        {
            if let Some(rt) = top.get_result_type() {
                eprintln!("[probe] make() return type: {:?}, kind={:?}",
                    rt.get_display_name(), rt.get_kind());
                if let Some(d) = rt.get_declaration() {
                    eprintln!("[probe]   decl: kind={:?} name={:?} is_def={}",
                        d.get_kind(),
                        d.get_name().unwrap_or_default(),
                        d.is_definition(),
                    );
                    eprintln!("[probe]   spec children:");
                    for c in d.get_children() {
                        eprintln!("[probe]     {:?} {:?}",
                            c.get_kind(),
                            c.get_name().unwrap_or_default(),
                        );
                    }
                    if let Some(tmpl) = d.get_template() {
                        eprintln!("[probe]   spec.get_template(): {:?} {:?}",
                            tmpl.get_kind(),
                            tmpl.get_name().unwrap_or_default(),
                        );
                    }
                    if let Some(targs) = rt.get_template_argument_types() {
                        eprintln!("[probe]   template arg types ({}): ", targs.len());
                        for (i, ta) in targs.iter().enumerate() {
                            eprintln!("[probe]     arg[{i}]: {:?}", ta.as_ref().map(|t| t.get_display_name()));
                        }
                    }
                }
            }
        }
    }

    cleanup(&header);
}
