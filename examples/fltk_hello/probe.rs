//! Standalone probe: import FLTK's main public headers via
//! `cxx_importer` and report what the importer can/can't handle.
//! Not a workspace member — run with:
//!
//! ```sh
//! cd examples/fltk_hello
//! cargo run --manifest-path probe-Cargo.toml --release
//! ```
//!
//! This is a diagnostic tool — it surfaces the gap list between
//! what Phase A+B+C ships and what FLTK requires, so we can size
//! the M22+ work concretely.

use std::path::PathBuf;

use cxx_importer::{
    import_header, import_header_with_extras,
    rust_bindings::{
        generate_rust_bindings_with_extras, BindingsBackend, RustBindingsConfig,
    },
    AnnotationSet, Driver, HeaderGraph,
};
use rustc_abi_cxx::{CxxTypeCtx, Target};

fn main() {
    let fltk_include = PathBuf::from("/opt/homebrew/include");

    // Per-header probe. We process each FLTK header in its own
    // libclang TU so a segfault in one TU (libclang on the
    // M-series Mac chokes on `Fl.H` + `Fl_Window.H` parsed
    // together via `Driver::parse_all` — looks like a libclang
    // crash, not ours) is contained: we just skip that header
    // and keep going.
    let headers = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .map(|name| fltk_include.join("FL").join(name))
        .collect::<Vec<_>>();
    let headers = if headers.is_empty() {
        // Default: a single umbrella that #includes everything the
        // demo needs. One libclang TU avoids a known multi-header
        // parse_all crash on libclang 17+ on macOS arm64 (see the
        // comment on `import_header_with_clang`). The umbrella
        // sits in this crate's `cpp/` dir.
        vec![PathBuf::from(
            concat!(env!("CARGO_MANIFEST_DIR"), "/cpp/fltk_umbrella.hpp"),
        )]
    } else {
        headers
    };

    println!("=== FLTK import probe ===\n");
    for h in &headers {
        println!("  Header: {}", h.display());
    }
    println!();

    // Probe phase 1: try `Driver::parse_all` with shared USR cache
    // across roots. This is the API a real build.rs uses; if it
    // doesn't work, that's the gating gap. We isolate it under a
    // `--driver` arg so the per-header path stays runnable when
    // it crashes libclang.
    if std::env::args().any(|a| a == "--driver") {
        println!("=== Driver::parse_all (shared USR cache) ===");
        let mut driver_ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let driver = Driver::new(HeaderGraph {
            roots: headers.clone(),
            include_paths: vec![fltk_include.clone()],
            clang_flags: vec!["-x".into(), "c++".into(), "-std=c++17".into()],
            ..HeaderGraph::default()
        });
        match driver.parse_all(&mut driver_ctx) {
            Ok(ids) => println!(
                "  driver imported {} classes ({} poisoned)",
                ids.len(),
                ids.iter().filter(|&&i| driver_ctx.is_poisoned(i)).count(),
            ),
            Err(e) => println!("  driver error: {e:?}"),
        }
        println!();
        return;
    }

    let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let mut all_ids: Vec<rustc_abi_cxx::ClassId> = Vec::new();
    let mut seen: std::collections::HashSet<rustc_abi_cxx::ClassId> =
        std::collections::HashSet::new();

    let include_arg = format!("-I{}", fltk_include.display());
    let argv: Vec<&str> = vec![
        "-x", "c++", "-std=c++17", &include_arg,
    ];
    let mut all_extras = cxx_importer::ImportExtras::default();
    for h in &headers {
        match import_header_with_extras(h, &argv, &mut ctx) {
            Ok((ids, extras)) => {
                println!("  + {} → {} classes", h.file_name().unwrap().to_string_lossy(), ids.len());
                for id in ids {
                    if seen.insert(id) {
                        all_ids.push(id);
                    }
                }
                all_extras.aliases.entries.extend(extras.aliases.entries);
                all_extras.enums.entries.extend(extras.enums.entries);
            }
            Err(e) => {
                println!("  ! {} → import error: {e:?}", h.file_name().unwrap().to_string_lossy());
            }
        }
    }
    println!();
    let _ = import_header; // keep the back-compat path imported but not used.

    {
        let class_ids = all_ids;
        {
            let total = class_ids.len();
            let poisoned = class_ids
                .iter()
                .filter(|&&id| ctx.is_poisoned(id))
                .count();
            let healthy = total - poisoned;
            println!("=== Stats ===");
            println!("  total classes imported : {total}");
            println!("  healthy                : {healthy}");
            println!("  poisoned               : {poisoned}");
            println!();

            // Bucket poison reasons.
            let mut reason_buckets: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for &id in &class_ids {
                if let Some(reason) = ctx.poison_reason(id) {
                    let key = bucket_for(reason);
                    *reason_buckets.entry(key).or_insert(0) += 1;
                }
            }
            println!("=== Poison reason buckets ===");
            let mut buckets: Vec<_> = reason_buckets.into_iter().collect();
            buckets.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            for (reason, n) in &buckets {
                println!("  [{n:4}]  {reason}");
            }
            println!();

            // Sample first few poisoned classes for visibility.
            println!("=== Sample poisoned classes (up to 10) ===");
            for &id in class_ids.iter().filter(|&&i| ctx.is_poisoned(i)).take(10) {
                let class = ctx.class(id);
                let name = class
                    .name
                    .0
                    .last()
                    .and_then(|s| match s {
                        rustc_abi_cxx::NameSegment::Class(i)
                        | rustc_abi_cxx::NameSegment::Namespace(i) => Some(i.0.as_str()),
                        _ => None,
                    })
                    .unwrap_or("<anon>");
                let reason = ctx.poison_reason(id).unwrap_or("?");
                let short = reason.split(':').last().unwrap_or(reason).trim();
                println!("  {name:30}  →  {short}");
            }
            println!();

            // Try to generate Rust bindings. This is the next gate
            // — if classes import healthy but the emitter rejects
            // them, that's our M22+ work surface.
            //
            // Use the full ctx class set, not just `class_ids`:
            // forward-declared types (e.g. `Fl_Screen_Driver*`
            // pointer params) get minted as poisoned classes but
            // aren't returned by `import_header_*` because that
            // list only carries TU-scope class definitions. Without
            // emitting them as opaque structs here, the bindings
            // would reference undefined Rust types.
            println!("=== Bindings emission ===");
            let cfg = RustBindingsConfig {
                backend: BindingsBackend::DirectExternCpp,
                cstr_ergonomics: true,
                ..RustBindingsConfig::default()
            };
            let all_class_ids: Vec<rustc_abi_cxx::ClassId> =
                ctx.class_ids().collect();
            println!(
                "  emitting {} classes ({} requested + {} forward-decl/transitive)",
                all_class_ids.len(),
                class_ids.len(),
                all_class_ids.len() - class_ids.len(),
            );
            match generate_rust_bindings_with_extras(
                &ctx,
                &all_class_ids,
                &AnnotationSet::default(),
                &all_extras.aliases,
                &all_extras.enums,
                &cfg,
            ) {
                Ok(src) => {
                    let lines = src.lines().count();
                    println!("  bindings emitted ok: {lines} lines, {} bytes", src.len());
                    let out_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("generated_bindings.rs");
                    std::fs::write(&out_path, &src).expect("write bindings");
                    println!("  wrote {}", out_path.display());
                }
                Err(e) => {
                    println!("  bindings emission FAILED: {e:?}");
                }
            }
            println!();

            // Sample healthy classes — these are what we can use today.
            println!("=== Sample healthy classes (up to 10) ===");
            for &id in class_ids.iter().filter(|&&i| !ctx.is_poisoned(i)).take(10) {
                let class = ctx.class(id);
                let name = class
                    .name
                    .0
                    .last()
                    .and_then(|s| match s {
                        rustc_abi_cxx::NameSegment::Class(i)
                        | rustc_abi_cxx::NameSegment::Namespace(i) => Some(i.0.as_str()),
                        _ => None,
                    })
                    .unwrap_or("<anon>");
                println!(
                    "  {name:30}  fields={:3}  methods={:3}  bases={:3}",
                    class.fields.len(),
                    class.methods.len(),
                    class.bases.len(),
                );
            }
        }
    }
}

/// Reduce a span-prefixed poison reason to a stable bucket key,
/// stripping the leading "<file>:<line>:<col>: " noise so the
/// histogram aggregates by category, not by location.
fn bucket_for(reason: &str) -> String {
    let stripped = reason.find(':').map_or(reason, |p1| {
        let rest = &reason[p1 + 1..];
        rest.find(':').map_or(rest, |p2| {
            let rest2 = &rest[p2 + 1..];
            rest2.find(':').map_or(rest2, |p3| &rest2[p3 + 1..])
        })
    });
    let stripped = stripped.trim();
    // Truncate to first 80 chars so a long tail of unique
    // "in `Fl_<class>::<method>`" doesn't fragment the bucket.
    let mut s: String = stripped.chars().take(80).collect();
    if let Some(p) = s.rfind(" — ") {
        s.truncate(p);
    }
    s
}
