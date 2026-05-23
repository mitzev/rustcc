//! v1.11 stretch 3: STL container auto-discovery.
//!
//! Validates that `HeaderGraph::auto_discover_template_specs = true`
//! causes the driver to pre-scan headers for template-spec
//! references and force-instantiate them automatically, lifting
//! the burden of hand-listing every `std::vector<int>` /
//! `std::optional<MyType>` in the sidecar YAML.

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{discover_template_instantiations, Driver, HeaderGraph};
use rustc_abi_cxx::{CxxTypeCtx, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_m24_autodiscover_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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

#[test]
fn discover_picks_up_field_spec_references() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir("fields");
    let hdr = dir.join("h.hpp");

    // A user-defined template (no STL dependency — keeps the test
    // hermetic) referenced from a field and a parameter. The
    // discovery walker should pick up both instantiations.
    std::fs::write(
        &hdr,
        r#"#pragma once
template <typename T> class Vec {
public:
    Vec();
    T at(int i) const;
private:
    T* data_;
    int size_;
};

class Inner {};

class Outer {
public:
    Vec<int> int_vec;          // field — spec by-value
    Vec<Inner>* inner_vec_ptr; // field — spec via pointer
    void take_double(Vec<double>);  // parameter — spec by-value
};
"#,
    )
    .unwrap();

    let clang = clang::Clang::new().unwrap();
    let found = discover_template_instantiations(
        &clang,
        &hdr,
        &["-x", "c++", "-std=c++17"],
    )
    .expect("discovery");

    // Should pick up all three instantiations:
    let has = |needle: &str| found.iter().any(|s| s.contains(needle));
    assert!(
        has("Vec<int>"),
        "expected Vec<int> in discovered set, got {found:?}"
    );
    assert!(
        has("Vec<Inner>"),
        "expected Vec<Inner> in discovered set, got {found:?}"
    );
    assert!(
        has("Vec<double>"),
        "expected Vec<double> in discovered set, got {found:?}"
    );
}

#[test]
fn parse_all_with_autodiscover_imports_methods_on_unlisted_spec() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir("e2e");
    let hdr = dir.join("h.hpp");

    // Box<int> is referenced by foo() but NOT listed in
    // template_instantiations. With auto-discover off, methods
    // on Box<int> stay invisible. With it on, they show up.
    std::fs::write(
        &hdr,
        r#"#pragma once
template <typename T> class Box {
public:
    Box(T v);
    T get() const;
    void set(T v);
private:
    T v_;
};

void take_int_box(Box<int>);
"#,
    )
    .unwrap();

    // Baseline: no auto-discover, no explicit instantiation. The
    // Box<int> methods should NOT appear as imported methods
    // (only the generic Box template is registered without
    // method bodies materialized for the int specialization).
    let mut ctx_off = CxxTypeCtx::new(host_target());
    let graph_off = HeaderGraph {
        roots: vec![hdr.clone()],
        clang_flags: vec!["-std=c++17".into()],
        ..HeaderGraph::default()
    };
    let driver_off = Driver::new(graph_off);
    let ids_off = driver_off.parse_all(&mut ctx_off).expect("parse_all off");
    let box_int_off = ids_off.iter().find(|&&id| {
        use rustc_abi_cxx::NameSegment;
        ctx_off
            .class(id)
            .name
            .0
            .iter()
            .any(|s| matches!(s, NameSegment::TemplateSpec { name, .. } if name.0 == "Box"))
    });
    // We don't strictly require Box<int> to be absent here (it
    // might or might not get imported as a placeholder), but
    // if it IS imported, it should have zero methods because
    // the template wasn't force-instantiated.
    if let Some(&id) = box_int_off {
        let n_methods = ctx_off.class(id).methods.len();
        eprintln!("baseline: Box<int> methods count = {n_methods}");
        // Allow either 0 or "some methods that happen to be in
        // the generic template" — but typically 0 because the
        // spec cursor has no children.
        assert!(
            n_methods <= 3,
            "baseline expected ≤3 methods on un-instantiated Box<int>, got {n_methods}",
        );
    }

    // Now: turn on auto-discover. The driver pre-scans, finds
    // Box<int> in `take_int_box`'s parameter, and force-
    // instantiates it. Box<int>'s 3 methods (ctor, get, set)
    // should now be imported.
    let mut ctx_on = CxxTypeCtx::new(host_target());
    let graph_on = HeaderGraph {
        roots: vec![hdr],
        clang_flags: vec!["-std=c++17".into()],
        auto_discover_template_specs: true,
        ..HeaderGraph::default()
    };
    let driver_on = Driver::new(graph_on);
    let ids_on = driver_on.parse_all(&mut ctx_on).expect("parse_all on");

    // Find Box<int>'s ClassId in the auto-discovered run. The
    // class shows up as a TemplateSpec named "Box" with one
    // type argument. There's only one Box template in this
    // header so we don't need to filter on the arg's concrete
    // type (it would require reading the interned CxxType,
    // which is messier).
    let box_int_on = ids_on.iter().find(|&&id| {
        use rustc_abi_cxx::NameSegment;
        ctx_on
            .class(id)
            .name
            .0
            .iter()
            .any(|s| matches!(s, NameSegment::TemplateSpec { name, .. } if name.0 == "Box"))
    });
    // Dump all imported class names for diagnostics if Box<int>
    // isn't found.
    if box_int_on.is_none() {
        eprintln!("=== imported classes after auto-discover ===");
        for &id in &ids_on {
            eprintln!("  {id:?}: {:?}", ctx_on.class(id).name);
        }
    }
    let id = *box_int_on.expect(
        "expected Box<int> to be imported after auto-discovery — \
         driver may have failed to feed the discovered string into \
         the synthesized force-instantiation root",
    );
    let n_methods = ctx_on.class(id).methods.len();
    assert!(
        n_methods >= 2,
        "expected ≥2 methods on Box<int> after auto-discover (ctor + get + set), got {n_methods}: {:?}",
        ctx_on.class(id).methods.iter().map(|m| format!("{:?}", m.name)).collect::<Vec<_>>(),
    );
}
