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
        for root in &self.graph.roots {
            let ids = crate::import::import_header_with_cache(
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
}
