//! v1.12.23: `[[clang::annotate("rustcc::skip")]]` must prune a
//! skipped class's recursive base / field import, not just drop the
//! class at emit time.
//!
//! Regression for the `cxx_throws_demo` Linux CI gate: the demo's
//! `DomainError` / `RangeError` are `rustcc::skip`-annotated C++-only
//! exception types that derive from `std::exception`. Before this
//! fix, `import_class` walked their bases regardless of the skip
//! marker, recursively importing `std::exception` and its entire
//! reachable libstdc++ graph (`basic_string`, `basic_ostream`,
//! detail types, …). The emit-time `Skip` filter dropped the two
//! annotated classes but NOT those transitively-imported STL types,
//! which then rendered as un-compilable `extern "C++"` blocks +
//! references to never-emitted types (~228 errors on ubuntu, where
//! libstdc++ is on the default include path; macOS libclang doesn't
//! find the C++ stdlib without `-isysroot`, so the cascade — and the
//! breakage — never surfaced there).

#![cfg(feature = "libclang")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use cxx_importer::import_header_with_extras;
use rustc_abi_cxx::{CxxTypeCtx, NameSegment, Target};

// libclang's global init/dispose cycle is brittle when several
// parses run concurrently; serialize the importer-driving tests.
static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_skip_prune_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Does the ctx hold a class whose name path contains `needle` as a
/// `Class` / `TemplateSpec` segment? Used to assert that a skipped
/// type's bases never made it into the import.
fn has_named_class(ctx: &CxxTypeCtx, needle: &str) -> bool {
    ctx.class_ids().any(|id| {
        ctx.class(id).name.0.iter().any(|seg| match seg {
            NameSegment::Class(i) | NameSegment::Enum(i) => i.0 == needle,
            NameSegment::TemplateSpec { name, .. } => name.0 == needle,
            _ => false,
        })
    })
}

fn class_by_ident<'c>(
    ctx: &'c CxxTypeCtx,
    needle: &str,
) -> Option<&'c rustc_abi_cxx::ClassDef> {
    ctx.class_ids()
        .map(|id| ctx.class(id))
        .find(|c| match c.name.0.last() {
            Some(NameSegment::Class(i)) => i.0 == needle,
            _ => false,
        })
}

/// The skip marker prunes the recursive base import: a skipped class
/// derived from a base records *no* bases, while an unannotated
/// sibling derived from the same base keeps its base edge. Hermetic
/// (no system headers) so it runs on every platform.
#[test]
fn skip_prunes_base_edge() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("hermetic");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
struct Base { int a; virtual int poke(); };

class [[clang::annotate("rustcc::skip")]] Skipped : public Base {
public:
    int own() const;
};

struct Kept : public Base {
    int k;
};
"#,
    )
    .unwrap();

    let mut ctx = CxxTypeCtx::new(Target::host());
    let _ = import_header_with_extras(
        &hdr,
        &["-x", "c++", "-std=c++17"],
        &mut ctx,
    )
    .expect("import");

    let skipped = class_by_ident(&ctx, "Skipped")
        .expect("Skipped class should still be registered");
    assert!(
        skipped.bases.is_empty(),
        "skip-annotated class must not import its bases; got {:?}",
        skipped.bases,
    );

    let kept = class_by_ident(&ctx, "Kept").expect("Kept class");
    assert_eq!(
        kept.bases.len(),
        1,
        "unannotated class must keep its base edge; got {:?}",
        kept.bases,
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The motivating case: a skipped class deriving from
/// `std::exception` must not drag `namespace std`'s internals into
/// the ctx. Best-effort — if libclang can't find the C++ standard
/// library (macOS without `-isysroot`), `std::exception` never
/// resolves and there's nothing to prune, so the test reports and
/// returns instead of failing.
#[test]
fn skip_does_not_pull_in_std_exception_graph() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tmpdir("stl");
    let hdr = dir.join("h.hpp");
    std::fs::write(
        &hdr,
        r#"#pragma once
#include <stdexcept>

class [[clang::annotate("rustcc::skip")]] DomainError : public std::exception {
public:
    DomainError(const char* m) : msg_(m) {}
    const char* what() const noexcept override { return msg_; }
private:
    const char* msg_;
};
"#,
    )
    .unwrap();

    // On macOS, libclang needs the SDK sysroot to find `<stdexcept>`.
    // On Linux, libstdc++ is on the default search path. Build the
    // arg list accordingly.
    let mut args: Vec<String> =
        vec!["-x".into(), "c++".into(), "-std=c++17".into()];
    if cfg!(target_os = "macos") {
        if let Some(sdk) = Command::new("xcrun")
            .args(["--show-sdk-path"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            args.push(format!("-isysroot{sdk}"));
        }
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut ctx = CxxTypeCtx::new(Target::host());
    let _ = import_header_with_extras(&hdr, &arg_refs, &mut ctx).expect("import");

    let domain = class_by_ident(&ctx, "DomainError")
        .expect("DomainError should still be registered");

    if !domain.bases.is_empty() {
        // The base resolved (the C++ stdlib was found) yet survived
        // the prune — that's the regression.
        panic!(
            "skip-annotated DomainError still recorded bases: {:?}",
            domain.bases,
        );
    }

    // Whether or not the base resolved, none of `std::exception`'s
    // graph may have leaked into the ctx.
    for leaked in ["exception", "basic_string", "basic_ostream"] {
        assert!(
            !has_named_class(&ctx, leaked),
            "skip-annotated class pulled `std::{leaked}` into the import \
             (recursive base-graph cascade not pruned)",
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
