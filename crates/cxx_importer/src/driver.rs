//! Owns the libclang `CXIndex` and one `CXTranslationUnit` per header graph.
//!
//! See `docs/cxx_importer.md §3`.
//!
//! The driver is the high-level entry point that consumers of
//! `cxx_importer` use: hand it a [`HeaderGraph`] describing the headers
//! and clang flags, then call [`Driver::parse_all`] to populate a
//! [`CxxTypeCtx`] and [`Driver::emit_shims`] to get a compile-ready
//! `.cpp` trampoline source.

use std::path::PathBuf;

use rustc_abi_cxx::ClassId;

#[cfg(feature = "libclang")]
use crate::diagnostics::ImportError;
use crate::hpp::{self, HppError, HppOptions};
use crate::rust_bindings::{self, BindingsError, RustBindingsConfig};
use crate::shims::{self, ShimError, ShimOptions};

pub struct HeaderGraph {
    pub roots: Vec<PathBuf>,
    pub include_paths: Vec<PathBuf>,
    pub clang_flags: Vec<String>,
    /// Explicit template instantiations to materialize during parse,
    /// e.g. `["std::vector<int>", "std::map<std::string, int>"]`.
    /// Each entry becomes a `template class <inst>;` line in a
    /// synthetic root that `#include`s every entry in `roots`. The
    /// resulting class-template specializations are picked up by
    /// the regular import path as concrete classes — no special
    /// handling at the lowering layer.
    ///
    /// Empty by default. Populate this field (or use the
    /// `Driver::with_instantiations(...)` helper) to opt in.
    pub template_instantiations: Vec<String>,
}

impl Default for HeaderGraph {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            include_paths: Vec::new(),
            clang_flags: Vec::new(),
            template_instantiations: Vec::new(),
        }
    }
}

impl HeaderGraph {
    /// M25: pull any `instantiations:` entries from a parsed
    /// [`crate::annotations::SidecarSchema`] into this graph's
    /// `template_instantiations` list. Duplicates against existing
    /// entries are skipped. The schema's iteration order is
    /// preserved (BTreeMap on type name, then source order within
    /// each type's list).
    ///
    /// Typical pipeline:
    ///
    /// ```ignore
    /// let schema = cxx_importer::annotations::load_sidecar(&path)?;
    /// let mut graph = HeaderGraph { roots: …, .. HeaderGraph::default() };
    /// graph.extend_from_sidecar(&schema);
    /// let driver = Driver::new(graph);
    /// driver.parse_all(&mut ctx)?;
    /// ```
    ///
    /// Equivalent to manually appending
    /// `schema.collect_template_instantiations()`, but dedups
    /// against the graph's existing list so calling this twice
    /// (or against a graph that already has hand-added entries)
    /// is safe.
    pub fn extend_from_sidecar(
        &mut self,
        schema: &crate::annotations::SidecarSchema,
    ) {
        // Snapshot existing entries as owned strings so we can
        // append to `self.template_instantiations` without
        // overlapping borrows. The dedup set stays small
        // (typical sidecar has < 20 instantiations).
        let existing: std::collections::HashSet<String> = self
            .template_instantiations
            .iter()
            .cloned()
            .collect();
        for inst in schema.collect_template_instantiations() {
            if !existing.contains(&inst) {
                self.template_instantiations.push(inst);
            }
        }
    }
}

pub struct Driver {
    graph: HeaderGraph,
}

impl Driver {
    pub fn new(graph: HeaderGraph) -> Self {
        Self { graph }
    }

    pub fn graph(&self) -> &HeaderGraph {
        &self.graph
    }

    /// Invoke libclang on every root header and accumulate the imported
    /// classes into `ctx`. Returns the full set of top-level classes
    /// imported across all roots.
    ///
    /// Classes that appear in more than one TU (e.g. a shared
    /// `common.h` `#include`d from two roots) are deduplicated by
    /// Clang USR — the second TU reuses the `ClassId` minted during the
    /// first TU rather than allocating a duplicate.
    ///
    /// The argv passed to libclang is derived from the graph:
    /// `-x c++` first (forces C++ parsing of `.h` files), then one
    /// `-I<path>` per entry in `include_paths`, then `clang_flags`
    /// verbatim. If `clang_flags` doesn't contain a `-std=...` the caller
    /// gets clang's default.
    #[cfg(feature = "libclang")]
    pub fn parse_all(
        &self,
        ctx: &mut rustc_abi_cxx::CxxTypeCtx,
    ) -> Result<Vec<ClassId>, ImportError> {
        let mut argv: Vec<String> = vec!["-x".into(), "c++".into()];
        for inc in &self.graph.include_paths {
            argv.push(format!("-I{}", inc.display()));
        }
        argv.extend(self.graph.clang_flags.iter().cloned());
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

        let mut all: Vec<ClassId> = Vec::new();
        let mut seen: std::collections::HashSet<ClassId> =
            std::collections::HashSet::new();
        let mut usr_cache: std::collections::HashMap<String, ClassId> =
            std::collections::HashMap::new();

        // If the graph requests explicit template instantiations,
        // synthesize a single root that `#include`s every user
        // header and force-instantiates each template via
        // `template class <name>;`. libclang then surfaces the
        // resulting `ClassTemplateSpecialization` cursors as
        // regular class definitions, which the importer's standard
        // path picks up. The synthetic file is parsed alongside
        // (not instead of) the user roots to keep namespace
        // ordering / first-import semantics consistent across
        // configurations.
        let synth = synthesize_instantiation_root(&self.graph)?;
        let mut roots: Vec<PathBuf> = self.graph.roots.clone();
        if let Some((path, _guard)) = synth.as_ref() {
            roots.push(path.clone());
        }

        // Hoist `Clang::new()` above the per-header loop. libclang's
        // global init/dispose cycle is brittle on macOS arm64
        // (libclang 17+) — re-initing per header has been observed
        // to segfault when the second parse touches AST state from
        // the first. Reusing one `Clang` instance across the whole
        // walk avoids that.
        let clang = clang::Clang::new().map_err(|e| ImportError::ClangDiagnostic {
            file: roots
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            line: 0,
            message: format!("failed to initialize libclang: {e}"),
        })?;
        for root in &roots {
            let (ids, _aliases, _enums, _free_fns) =
                crate::import::import_header_with_clang(
                    &clang,
                    root,
                    &argv_refs,
                    ctx,
                    &mut usr_cache,
                )?;
            for id in ids {
                if seen.insert(id) {
                    all.push(id);
                }
            }
        }
        // _guard drops here, removing the temp file.
        let _ = synth;
        Ok(all)
    }

    /// Emit the C++ trampoline source for `classes`, using the driver's
    /// root headers as `#include` directives.
    ///
    /// The output is compilable with the user's Clang toolchain against
    /// the same headers the importer saw. Caller writes it to disk and
    /// compiles it — the driver deliberately stops short of invoking
    /// clang itself so build orchestration (cargo, bazel, …) stays in
    /// the user's hands.
    pub fn emit_shims(
        &self,
        ctx: &rustc_abi_cxx::CxxTypeCtx,
        classes: &[ClassId],
    ) -> Result<String, ShimError> {
        let header_strs: Vec<&str> = self
            .graph
            .roots
            .iter()
            .map(|p| p.to_str().expect("header path must be valid UTF-8"))
            .collect();
        shims::generate_shims(
            ctx,
            &ShimOptions {
                headers: &header_strs,
                classes,
            },
        )
    }

    /// Emit a C++ `.hpp` that exposes `classes` to C++ callers. This is
    /// the reverse direction of `emit_shims`: the shim generator lets
    /// Rust call C++, whereas this header lets C++ `#include` a type
    /// that was authored in Rust with `#[repr(cpp)]`. See
    /// `docs/repr_cpp.md §3` for the output shape.
    pub fn emit_hpp(
        &self,
        ctx: &rustc_abi_cxx::CxxTypeCtx,
        classes: &[ClassId],
    ) -> Result<String, HppError> {
        hpp::generate_hpp(ctx, &HppOptions { classes })
    }

    /// Emit a C++ `.hpp` for every `TypeOrigin::RustReprCpp` class in
    /// the ctx. This is the normal path driver callers want — it
    /// excludes imported C++ classes (which are already declared in the
    /// user's headers). Equivalent to `emit_hpp(ctx, &ctx.rust_classes().collect())`.
    pub fn emit_hpp_for_rust_types(
        &self,
        ctx: &rustc_abi_cxx::CxxTypeCtx,
    ) -> Result<String, HppError> {
        hpp::generate_hpp_for_rust_types(ctx)
    }

    /// Emit Rust source bridging the imported `classes` so downstream
    /// Rust code can `use` and call them directly. The output is a
    /// `String` the caller writes to disk (typically into `OUT_DIR`
    /// from a `build.rs` and `include!`'d from `lib.rs`).
    ///
    /// Pairs with [`Driver::emit_shims`]: shims provide the C++ side
    /// of the Rust → C++ direction, this provides the Rust side. See
    /// `crates/cxx_importer/src/rust_bindings.rs` for backend choices
    /// and per-backend caveats.
    pub fn emit_rust_bindings(
        &self,
        ctx: &rustc_abi_cxx::CxxTypeCtx,
        classes: &[ClassId],
        config: &RustBindingsConfig,
    ) -> Result<String, BindingsError> {
        rust_bindings::generate_rust_bindings(ctx, classes, config)
    }

    /// Cache-aware variant of [`Driver::parse_all`]. On a cache hit
    /// (matching schema + key), loads the previously-serialized
    /// `CxxTypeCtx` and `AnnotationSet` from `cache_path` and skips
    /// the libclang invocation entirely. On a miss (stale cache,
    /// schema mismatch, or first run), parses via libclang and
    /// writes the cache for the next build.
    ///
    /// Cache key is the SHA-256 of every header file's bytes
    /// joined with the clang argv, the libclang version banner,
    /// and the cxx_importer crate version. Any change to any of
    /// those invalidates the cache.
    ///
    /// `caller_ctx` is mutated in place — on a hit, it's *replaced*
    /// with the deserialized contents; on a miss, it's populated
    /// by the libclang parse the same way `parse_all` does.
    /// `caller_annotations` is treated identically.
    ///
    /// Both arguments accept any starting state — pass freshly-
    /// constructed values for the typical build.rs flow.
    #[cfg(all(feature = "libclang", feature = "cache"))]
    pub fn load_or_parse(
        &self,
        cache_path: &std::path::Path,
        caller_ctx: &mut rustc_abi_cxx::CxxTypeCtx,
        caller_annotations: &mut crate::annotations::AnnotationSet,
    ) -> Result<Vec<ClassId>, ImportError> {
        // Compose the same argv `parse_all` constructs so the cache
        // key sees exactly what libclang sees.
        let mut argv: Vec<String> = vec!["-x".into(), "c++".into()];
        for inc in &self.graph.include_paths {
            argv.push(format!("-I{}", inc.display()));
        }
        argv.extend(self.graph.clang_flags.iter().cloned());

        let sorted = crate::cache::sorted_headers(&self.graph.roots);
        let header_digest = crate::cache::hash_headers(&sorted).map_err(|e| {
            ImportError::ClangDiagnostic {
                file: cache_path.display().to_string(),
                line: 0,
                message: format!("hash_headers: {e}"),
            }
        })?;
        let lc_version = crate::cache::libclang_version();
        let key = crate::cache::compute_cache_key(&header_digest, &argv, &lc_version);

        if let Some(record) = crate::cache::read_record(cache_path, &key) {
            // Cache hit. Replace caller state with the loaded
            // record. Note: this drops any pre-populated content
            // on the caller's side — the standard build.rs flow
            // passes freshly-constructed values, so this is the
            // intended path.
            *caller_ctx = record.ctx;
            *caller_annotations = record.annotations;
            return Ok(record.class_ids);
        }

        // Cache miss. Run a normal parse, then write the result
        // back to the cache.
        let class_ids = self.parse_all(caller_ctx)?;
        // Re-collect annotations into a fresh set for the write.
        // (`parse_all` doesn't surface annotations; the cache is
        // intentionally a bit overcounting on writes — paying a
        // second annotation walk is cheaper than restructuring
        // `parse_all` into a `parse_all_with_annotations` that
        // shares state.)
        let mut anns = crate::annotations::AnnotationSet::default();
        for root in &self.graph.roots {
            let mut tmp_cache = std::collections::HashMap::new();
            // Best-effort: ignore annotation-pass errors so a
            // sidecar problem doesn't poison the whole cache.
            let mut tmp_aliases = crate::aliases::AliasSet::default();
            let mut tmp_enums = crate::enums::EnumSet::default();
            let mut tmp_free_fns = crate::free_fns::FreeFnSet::default();
            if let Ok(_) = crate::import::import_header_full(
                root,
                &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                caller_ctx,
                &mut tmp_cache,
                &mut anns,
                &mut tmp_aliases,
                &mut tmp_enums,
                &mut tmp_free_fns,
            ) {
                // ok — annotations merged into `anns`. Aliases,
                // enum bodies, and free functions are dropped for
                // now: the cache record schema doesn't carry them
                // yet (tracked as a follow-up; load_or_parse
                // callers that want them use
                // `import_header_with_extras` directly).
            }
        }
        *caller_annotations = anns;

        let record = crate::cache::CacheRecord {
            schema: crate::cache::CACHE_SCHEMA,
            key,
            class_ids: class_ids.clone(),
            ctx: caller_ctx.clone(),
            annotations: caller_annotations.clone(),
        };
        crate::cache::write_record(cache_path, &record).map_err(|e| {
            ImportError::ClangDiagnostic {
                file: cache_path.display().to_string(),
                line: 0,
                message: format!("write_record: {e}"),
            }
        })?;

        Ok(class_ids)
    }
}

/// RAII guard for a synthetic-root temp file. Deletes the file on
/// drop so a successful (or failing) parse leaves no stray temp
/// artifacts behind.
struct SyntheticRootGuard {
    path: PathBuf,
}

impl Drop for SyntheticRootGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(feature = "libclang")]
fn synthesize_instantiation_root(
    graph: &HeaderGraph,
) -> Result<Option<(PathBuf, SyntheticRootGuard)>, ImportError> {
    if graph.template_instantiations.is_empty() {
        return Ok(None);
    }
    // Build the synthetic source: include each user root, then one
    // explicit-instantiation line per requested template. We use
    // file-scope `template class <name>;` syntax (the standard
    // explicit-instantiation form). No leading `extern` — that
    // would be `extern template class …;`, the wrong direction.
    let mut src = String::new();
    src.push_str("// Generated by rustcc cxx_importer::driver. Synthetic root\n");
    src.push_str("// for forced template instantiations declared in HeaderGraph.\n");
    src.push_str("// Do not hand-edit; the file is regenerated and removed per\n");
    src.push_str("// `Driver::parse_all` invocation.\n\n");
    for root in &graph.roots {
        let p = root.to_str().ok_or_else(|| ImportError::ClangDiagnostic {
            file: root.display().to_string(),
            line: 0,
            message: "header path is not valid UTF-8".into(),
        })?;
        src.push_str(&format!("#include \"{p}\"\n"));
    }
    src.push('\n');
    for inst in &graph.template_instantiations {
        src.push_str(&format!("template class {inst};\n"));
    }

    let path = std::env::temp_dir().join(format!(
        "rustcc_cxx_importer_synth_{}_{}.cpp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, src).map_err(|e| ImportError::ClangDiagnostic {
        file: path.display().to_string(),
        line: 0,
        message: format!("failed to write synthetic instantiation root: {e}"),
    })?;
    let guard = SyntheticRootGuard { path: path.clone() };
    Ok(Some((path, guard)))
}
