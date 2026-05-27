//! `build.rs` orchestrator (M26).
//!
//! Closes the "click run, see a window" gap from the FLTK demo:
//! a thin layer over the importer + emitter + the [`cc`] crate
//! that gives downstream crates a `build.rs` ergonomic similar
//! to `cc::Build` or `bindgen::Builder`.
//!
//! ## Pipeline
//!
//! `Build::compile()` chains every step a real `build.rs` needs:
//!
//! 1. **Parse** the configured headers with libclang via
//!    [`crate::Driver::parse_all`].
//! 2. **Emit Rust bindings** via
//!    [`crate::rust_bindings::generate_rust_bindings_full`] —
//!    full-fidelity output covering every Phase A / B / C
//!    side-table (annotations, aliases, enums, free fns,
//!    static data members).
//! 3. **Emit C++ shims** via [`crate::Driver::emit_shims`] —
//!    the Itanium-mangled trampoline source the Rust extern
//!    decls link against.
//! 4. **Compile shims** with [`cc::Build`] into a static
//!    library (`lib<name>.a` on Unix, `<name>.lib` on Windows).
//! 5. **Tell Cargo** about the link inputs:
//!    `cargo:rustc-link-search=` for `OUT_DIR`,
//!    `cargo:rustc-link-lib=static=<name>` for the shim lib,
//!    plus user-configured `link()` / `framework()` /
//!    `weak_framework()` directives. Also writes
//!    `cargo:rerun-if-changed=` for every header so the
//!    build re-runs when the source moves.
//!
//! ## Typical usage
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     cxx_importer::build::Build::new()
//!         .header("cpp/fltk_umbrella.hpp")
//!         .include_path("/opt/homebrew/include")
//!         .clang_flag("-std=c++17")
//!         .cstr_ergonomics(true)
//!         .link("fltk")
//!         .framework("Cocoa")
//!         .compile("fltk_bindings")
//!         .expect("FLTK bindings compile");
//! }
//! ```
//!
//! Then in `lib.rs`:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
//! ```
//!
//! ## What's NOT in this layer
//!
//! - Header-graph discovery (`#include` transitive close).
//!   Users hand-list root headers; transitively-included files
//!   are picked up by libclang automatically. Only the roots
//!   get wired into `cargo:rerun-if-changed=`. Adding `pkg-
//!   config` / `fltk-config` style auto-discovery is a
//!   follow-up.
//! - Cross-compilation. The `cc` crate handles target-specific
//!   compiler selection; the libclang side parses with the
//!   *host* clang regardless. For most C++ APIs that's
//!   identical, but cross-target ABI nuances are an open
//!   question for v0.

use std::path::{Path, PathBuf};

use rustc_abi_cxx::{CxxTypeCtx, Target};

use crate::aliases::AliasSet;
use crate::annotations::AnnotationSet;
use crate::diagnostics::ImportError;
use crate::driver::{Driver, HeaderGraph};
use crate::enums::EnumSet;
use crate::free_fns::FreeFnSet;
use crate::rust_bindings::{
    generate_rust_bindings_full, BindingsBackend, BindingsError,
    RustBindingsConfig,
};
use crate::shims::ShimError;
use crate::static_data::StaticDataSet;

/// One link directive forwarded to Cargo via
/// `cargo:rustc-link-lib=` after the shim static lib has been
/// built. Mirrors the spec form Cargo accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkSpec {
    /// `cargo:rustc-link-lib=<name>` — let the linker pick
    /// dylib vs. static via its default rules.
    Lib(String),
    /// `cargo:rustc-link-lib=dylib=<name>` — force shared.
    Dylib(String),
    /// `cargo:rustc-link-lib=static=<name>` — force static.
    Static(String),
    /// `cargo:rustc-link-lib=framework=<name>` — macOS only.
    Framework(String),
    /// macOS-only `-weak_framework <name>`. Emitted as
    /// `cargo:rustc-link-arg=-weak_framework=<name>`. FLTK's
    /// `fltk-config` ships these for `UniformTypeIdentifiers`
    /// and `ScreenCaptureKit` to keep the binary loadable on
    /// older macOS versions.
    WeakFramework(String),
}

impl LinkSpec {
    fn cargo_directive(&self) -> String {
        match self {
            LinkSpec::Lib(n) => format!("cargo:rustc-link-lib={n}"),
            LinkSpec::Dylib(n) => format!("cargo:rustc-link-lib=dylib={n}"),
            LinkSpec::Static(n) => format!("cargo:rustc-link-lib=static={n}"),
            LinkSpec::Framework(n) => format!("cargo:rustc-link-lib=framework={n}"),
            LinkSpec::WeakFramework(n) => {
                // `rustc-link-arg` is the catch-all for raw
                // linker flags. Two args: `-weak_framework` then
                // the framework name as a separate token.
                format!(
                    "cargo:rustc-link-arg=-weak_framework\ncargo:rustc-link-arg={n}",
                )
            }
        }
    }
}

/// Cargo `build.rs` orchestrator for `cxx_importer`.
///
/// Builder pattern: each setter takes `&mut self` and returns
/// `&mut Self` so calls chain. Terminate with [`Build::compile`].
pub struct Build {
    headers: Vec<PathBuf>,
    include_paths: Vec<PathBuf>,
    clang_flags: Vec<String>,
    cpp_std: String,
    cc_extra_flags: Vec<String>,
    cstr_ergonomics: bool,
    crate_module: Option<String>,
    libs: Vec<LinkSpec>,
    rustc_link_search: Vec<PathBuf>,
    bindings_filename: String,
    shims_filename: String,
    extra_rerun_paths: Vec<PathBuf>,
    /// Optional override for `OUT_DIR`. Production builds pick
    /// it up from the env var; tests may want to point at a
    /// `tempfile::tempdir()` instead.
    out_dir_override: Option<PathBuf>,
    target: Option<Target>,
    /// When `false`, `compile()` emits all the source files +
    /// prints the Cargo directives but skips the actual
    /// `cc::Build` invocation. Useful for tests that don't have
    /// a C++ compiler set up. Default: `true`.
    invoke_cc: bool,
    /// Explicit class-template instantiations to force-materialize,
    /// e.g. `"std::vector<int>"`. Each becomes a
    /// `template class <inst>;` line in a synthetic root so the
    /// specialization is imported as a concrete class. Combined with
    /// auto-discovered specs when [`Self::auto_instantiate`] is on.
    template_instantiations: Vec<String>,
    /// When `true` (default), `compile()` pre-scans the headers for
    /// class-template specializations referenced by value / pointer /
    /// reference (e.g. a function returning `std::vector<int>`) and
    /// force-instantiates them — so template specializations "just
    /// work" without a hand-written instantiation list. Set `false`
    /// to import only the explicitly-listed instantiations.
    auto_instantiate: bool,
}

impl Default for Build {
    fn default() -> Self {
        Self::new()
    }
}

impl Build {
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            include_paths: Vec::new(),
            clang_flags: Vec::new(),
            cpp_std: "c++17".into(),
            cc_extra_flags: Vec::new(),
            cstr_ergonomics: false,
            crate_module: None,
            libs: Vec::new(),
            rustc_link_search: Vec::new(),
            bindings_filename: "bindings.rs".into(),
            shims_filename: "cxx_shims.cpp".into(),
            extra_rerun_paths: Vec::new(),
            out_dir_override: None,
            target: None,
            invoke_cc: true,
            template_instantiations: Vec::new(),
            auto_instantiate: true,
        }
    }

    /// Add a root header. The libclang parser includes it via
    /// `-include`-style entry; transitively-included headers
    /// get processed automatically. Cargo `rerun-if-changed=`
    /// is emitted for every header path you add here.
    pub fn header<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.headers.push(p.as_ref().to_path_buf());
        self
    }

    /// Add an `-I<path>` to the libclang argv *and* the
    /// `cc::Build` include path so headers and shim source see
    /// the same view of the world.
    pub fn include_path<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.include_paths.push(p.as_ref().to_path_buf());
        self
    }

    /// Pass an extra flag straight through to libclang. Use
    /// this for `-D<MACRO>` and similar — `-std=` is set via
    /// [`Self::cpp_std`]. The same flag also gets forwarded to
    /// `cc::Build::flag` so the shim compile sees it.
    pub fn clang_flag(&mut self, f: impl Into<String>) -> &mut Self {
        self.clang_flags.push(f.into());
        self
    }

    /// Force-instantiate a class-template specialization so it imports
    /// as a concrete class, e.g. `.instantiate("std::vector<int>")`.
    /// Use this for specializations the auto-discovery pass can't see
    /// (composed only inside another template body, or selected at
    /// runtime). Auto-discovered specs are added on top unless
    /// [`Self::auto_instantiate`] is turned off.
    pub fn instantiate(&mut self, spec: impl Into<String>) -> &mut Self {
        self.template_instantiations.push(spec.into());
        self
    }

    /// Toggle automatic discovery of class-template specializations.
    ///
    /// On by default: `compile()` pre-scans the headers for
    /// specializations referenced by value / pointer / reference (e.g.
    /// a function returning `std::vector<int>`) and force-instantiates
    /// them, so templates "just work" without a hand-written list. Pass
    /// `false` to import only the specs added via [`Self::instantiate`].
    pub fn auto_instantiate(&mut self, yes: bool) -> &mut Self {
        self.auto_instantiate = yes;
        self
    }

    /// C++ language standard. Default `"c++17"`. Passed to
    /// libclang as `-std=<v>` and to `cc::Build` via
    /// `cpp(true).flag_if_supported("-std=c++<v>")`.
    pub fn cpp_std(&mut self, v: impl Into<String>) -> &mut Self {
        self.cpp_std = v.into();
        self
    }

    /// Extra `cc::Build::flag` argument applied only to the
    /// shim compile, not to libclang. Useful for things like
    /// `-fno-rtti` or `-Wno-unused-parameter` that are
    /// compiler-level rather than language-level.
    pub fn cc_flag(&mut self, f: impl Into<String>) -> &mut Self {
        self.cc_extra_flags.push(f.into());
        self
    }

    /// Forward a directory to `cargo:rustc-link-search=native=`.
    /// Useful when the system libraries aren't on the default
    /// linker search path.
    pub fn lib_search_path<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.rustc_link_search.push(p.as_ref().to_path_buf());
        self
    }

    /// `cargo:rustc-link-lib=<name>` directive. The linker
    /// picks dylib vs. static.
    pub fn link(&mut self, lib: impl Into<String>) -> &mut Self {
        self.libs.push(LinkSpec::Lib(lib.into()));
        self
    }

    /// `cargo:rustc-link-lib=static=<name>` — force static.
    pub fn link_static(&mut self, lib: impl Into<String>) -> &mut Self {
        self.libs.push(LinkSpec::Static(lib.into()));
        self
    }

    /// `cargo:rustc-link-lib=dylib=<name>` — force shared.
    pub fn link_dylib(&mut self, lib: impl Into<String>) -> &mut Self {
        self.libs.push(LinkSpec::Dylib(lib.into()));
        self
    }

    /// macOS framework — `cargo:rustc-link-lib=framework=<name>`.
    /// FLTK on macOS needs `Cocoa`.
    pub fn framework(&mut self, name: impl Into<String>) -> &mut Self {
        self.libs.push(LinkSpec::Framework(name.into()));
        self
    }

    /// macOS weak framework — `-weak_framework <name>` so the
    /// resulting binary stays loadable on older macOS versions
    /// that don't ship the framework.
    pub fn weak_framework(&mut self, name: impl Into<String>) -> &mut Self {
        self.libs.push(LinkSpec::WeakFramework(name.into()));
        self
    }

    /// M20 opt-in: render `char *` / `const char *` as
    /// `*[const|mut] ::core::ffi::c_char` in the generated
    /// bindings so `CStr::as_ptr()` plugs in directly.
    pub fn cstr_ergonomics(&mut self, on: bool) -> &mut Self {
        self.cstr_ergonomics = on;
        self
    }

    /// Wrap the entire generated bindings file in a `pub mod
    /// {name} { ... }` block. Off by default — generated
    /// bindings emit at the top level so a single
    /// `include!(concat!(env!("OUT_DIR"), "/bindings.rs"))`
    /// brings them in.
    pub fn crate_module(&mut self, name: impl Into<String>) -> &mut Self {
        self.crate_module = Some(name.into());
        self
    }

    /// Override the bindings filename inside `OUT_DIR`. Default
    /// `"bindings.rs"`.
    pub fn bindings_filename(&mut self, name: impl Into<String>) -> &mut Self {
        self.bindings_filename = name.into();
        self
    }

    /// Override the shim source filename inside `OUT_DIR`.
    /// Default `"cxx_shims.cpp"`.
    pub fn shims_filename(&mut self, name: impl Into<String>) -> &mut Self {
        self.shims_filename = name.into();
        self
    }

    /// Add a path to `cargo:rerun-if-changed=`. The headers
    /// listed via `header()` are added automatically; this is
    /// for additional inputs (sidecar YAML, included headers
    /// you want to track even though libclang sees them
    /// transitively, …).
    pub fn rerun_if_changed<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.extra_rerun_paths.push(p.as_ref().to_path_buf());
        self
    }

    /// Override the target the importer reports when computing
    /// layout for the captured C++ types. Defaults to
    /// `Target::host_default()` based on `CARGO_CFG_TARGET_*`
    /// env vars.
    pub fn target(&mut self, t: Target) -> &mut Self {
        self.target = Some(t);
        self
    }

    /// Override `OUT_DIR`. In a real Cargo build this is set
    /// automatically; tests pass a `tempfile::tempdir()` path.
    pub fn out_dir<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.out_dir_override = Some(p.as_ref().to_path_buf());
        self
    }

    /// When `false`, skip the actual `cc::Build` invocation —
    /// useful in tests that don't have a C++ compiler set up.
    /// Source files still get written; Cargo directives still
    /// get printed. Default `true`.
    pub fn invoke_cc(&mut self, on: bool) -> &mut Self {
        self.invoke_cc = on;
        self
    }

    /// Run the full pipeline. `static_lib_name` becomes the
    /// `lib<name>.a` shim archive that Cargo links against.
    /// Returns paths to every generated artifact.
    pub fn compile(&self, static_lib_name: &str) -> Result<BuildOutputs, BuildError> {
        let out_dir = self.resolve_out_dir()?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            BuildError::Io(format!("create OUT_DIR {}: {e}", out_dir.display()))
        })?;

        // ----- 1. Build the HeaderGraph + Driver. -----
        let mut full_clang_flags: Vec<String> = Vec::new();
        full_clang_flags.push(format!("-std={}", self.cpp_std));
        // Resolve the target before we build clang flags so a
        // cross-target (e.g. Windows MSVC from a Linux host)
        // gets `-target <triple>` + `-fms-compatibility` injected
        // into the libclang argv. If the user already supplied
        // `-target` in `clang_flags`, we don't override.
        let target_for_argv = self
            .target
            .clone()
            .unwrap_or_else(target_from_cargo_env);
        let user_set_target = self
            .clang_flags
            .iter()
            .any(|f| f == "-target" || f.starts_with("-target="));
        if !user_set_target {
            full_clang_flags.push("-target".into());
            full_clang_flags.push(target_for_argv.triple.clone());
            if matches!(
                target_for_argv.abi_flavor,
                rustc_abi_cxx::AbiFlavor::Msvc
            ) {
                // MSVC mode: enable MS extensions + ABI compat so
                // libclang parses Microsoft headers correctly.
                full_clang_flags.push("-fms-compatibility".into());
                full_clang_flags.push("-fms-extensions".into());
            }
        }
        full_clang_flags.extend(self.clang_flags.iter().cloned());
        let graph = HeaderGraph {
            roots: self.headers.clone(),
            include_paths: self.include_paths.clone(),
            clang_flags: full_clang_flags.clone(),
            ..HeaderGraph::default()
        };
        let driver = Driver::new(graph);

        // ----- 2. Parse + collect side-tables. ------
        // We use the full-fidelity per-header pipeline (one
        // `import_header_with_extras` call per root) so the
        // resulting `ImportExtras` carries every M11/M16/M17/M18
        // side-table the emitter wants. The single-Clang-instance
        // segfault fix from PR #9 (`Driver::parse_all` Clang
        // hoist) is preserved by routing through the same
        // helper internally.
        // Use the same Target we used to build the argv so the
        // Rust-side ctx and the libclang TU agree on ABI flavor.
        let mut ctx = CxxTypeCtx::new(target_for_argv);
        // v1.12.14: harvest inline `[[clang::annotate("rustcc::…")]]`
        // markup. The annotation walker re-parses each header (a
        // tracked perf gap, see `import::collect_annotations`'s
        // doc comment) but the cost is negligible next to the
        // downstream `cc::Build::compile` step.
        let mut annotations = AnnotationSet::default();
        let mut aliases = AliasSet::default();
        let mut enums = EnumSet::default();
        let mut free_fns = FreeFnSet::default();
        let mut static_data = StaticDataSet::default();

        let argv_strs = build_argv(&self.include_paths, &full_clang_flags);
        let argv: Vec<&str> = argv_strs.iter().map(String::as_str).collect();

        let mut all_class_ids: Vec<rustc_abi_cxx::ClassId> = Vec::new();
        let mut seen: std::collections::HashSet<rustc_abi_cxx::ClassId> =
            std::collections::HashSet::new();
        let mut usr_cache: std::collections::HashMap<String, rustc_abi_cxx::ClassId> =
            std::collections::HashMap::new();
        let clang = clang::Clang::new().map_err(|e| {
            BuildError::Import(ImportError::ClangDiagnostic {
                file: String::new(),
                line: 0,
                message: format!("Clang::new: {e}"),
            })
        })?;

        // ----- Template instantiation roots. -----
        // Combine the explicitly-listed instantiations with any
        // auto-discovered specializations (referenced by value /
        // pointer / reference in the headers), then materialize them in
        // a synthetic root that `#include`s the user headers and emits
        // one `template class X<...>;` per spec. The synthetic root is
        // imported in an extra pass *after* the per-header loop —
        // classes only, since its re-surfaced user-header
        // aliases / enums / free-fns would otherwise duplicate the
        // per-header captures.
        let synth = {
            let mut instantiations = self.template_instantiations.clone();
            if self.auto_instantiate {
                let explicit: std::collections::HashSet<&String> =
                    self.template_instantiations.iter().collect();
                let mut discovered: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for header in &self.headers {
                    let found =
                        crate::import::discover_template_instantiations(
                            &clang, header, &argv,
                        )
                        .map_err(BuildError::Import)?;
                    discovered.extend(found);
                }
                // STL containers compose helper specs (allocator, pair,
                // default_delete) internally that the reference scan
                // misses but the importer needs force-instantiated.
                let companions =
                    crate::import::synthesize_stl_companions(&discovered);
                discovered.extend(companions);
                let mut extra: Vec<String> = discovered
                    .into_iter()
                    .filter(|s| !explicit.contains(s))
                    .collect();
                extra.sort();
                instantiations.extend(extra);
            }
            let synth_graph = HeaderGraph {
                roots: self.headers.clone(),
                include_paths: self.include_paths.clone(),
                clang_flags: full_clang_flags.clone(),
                template_instantiations: instantiations,
                ..HeaderGraph::default()
            };
            crate::driver::synthesize_instantiation_root(&synth_graph)
                .map_err(BuildError::Import)?
        };

        for header in &self.headers {
            let (
                ids,
                captured_aliases,
                captured_enums,
                captured_free_fns,
                captured_static_data,
            ) = crate::import::import_header_with_clang(
                &clang,
                header,
                &argv,
                &mut ctx,
                &mut usr_cache,
            )
            .map_err(BuildError::Import)?;
            for id in ids {
                if seen.insert(id) {
                    all_class_ids.push(id);
                }
            }
            aliases.entries.extend(captured_aliases);
            enums.entries.extend(captured_enums);
            free_fns.entries.extend(captured_free_fns);
            static_data.entries.extend(captured_static_data);

            // v1.12.14: harvest annotations from this header
            // using the same Clang instance the main import
            // pass uses — re-initing libclang per parse has
            // been observed to segfault on libclang 17+ when
            // ASTs from earlier parses are still in scope.
            let collected = crate::import::collect_annotations_with_clang(
                &clang,
                header,
                &argv,
            )
            .map_err(BuildError::Import)?;
            for (key, anns) in collected {
                annotations.inline.entry(key).or_default().extend(anns);
            }
        }

        // ----- 2b. Import the synthetic instantiation root. -----
        // This surfaces the force-instantiated template specializations
        // as concrete classes. Only the *new* classes are kept — the
        // synth root `#include`s the user headers, so its side-tables
        // and annotations duplicate the per-header passes above and are
        // intentionally dropped. The guard keeps the temp file alive
        // across this parse, then deletes it on scope exit.
        if let Some((synth_path, _guard)) = synth.as_ref() {
            let (ids, ..) = crate::import::import_header_with_clang(
                &clang,
                synth_path,
                &argv,
                &mut ctx,
                &mut usr_cache,
            )
            .map_err(BuildError::Import)?;
            for id in ids {
                if seen.insert(id) {
                    all_class_ids.push(id);
                }
            }
        }

        // ----- 3. Emit Rust bindings. ------
        // Use the full ctx class set (not just the per-root
        // returned class_ids): forward-declared opaque types
        // referenced via pointer params get minted as poison
        // nodes that aren't in any root's TU-scope class list,
        // but the bindings emitter needs them to render
        // unbound `*const T` types correctly.
        let all_ctx_classes: Vec<rustc_abi_cxx::ClassId> =
            ctx.class_ids().collect();
        let cfg = RustBindingsConfig {
            backend: BindingsBackend::DirectExternCpp,
            cstr_ergonomics: self.cstr_ergonomics,
            crate_module: self.crate_module.clone(),
            ..RustBindingsConfig::default()
        };
        let bindings_src = generate_rust_bindings_full(
            &ctx,
            &all_ctx_classes,
            &annotations,
            &aliases,
            &enums,
            &free_fns,
            &static_data,
            &cfg,
        )
        .map_err(BuildError::Bindings)?;
        let bindings_path = out_dir.join(&self.bindings_filename);
        std::fs::write(&bindings_path, &bindings_src).map_err(|e| {
            BuildError::Io(format!(
                "write bindings to {}: {e}",
                bindings_path.display(),
            ))
        })?;

        // ----- 3.b Skip-log sidecar. -----
        // Parse the `// N method[s] skipped by the v0 bindings emitter:`
        // comment blocks the bindings renderer emits inside each
        // `impl Class { ... }` block. Editor tooling (vscode-rustcc
        // Problems pane, rustcc-cli `doctor`) consumes the JSON to
        // surface gaps without re-grepping the generated source on
        // every keystroke. The bindings.rs file remains the source
        // of truth; this is a structured projection.
        let skip_records = parse_skip_records(&bindings_src);
        let skip_log_path = out_dir.join("bindings.skips.json");
        let skip_log_json = serde_json::to_string_pretty(&SkipLog {
            schema_version: 1,
            generator: env!("CARGO_PKG_NAME").into(),
            generator_version: env!("CARGO_PKG_VERSION").into(),
            skips: skip_records.clone(),
        })
        .unwrap_or_else(|_| "{\"skips\":[]}".into());
        std::fs::write(&skip_log_path, &skip_log_json).map_err(|e| {
            BuildError::Io(format!(
                "write skip log to {}: {e}",
                skip_log_path.display(),
            ))
        })?;

        // ----- 4. Emit C++ shims. ------
        let mut shims_src =
            driver.emit_shims(&ctx, &all_class_ids).map_err(BuildError::Shim)?;

        // v1.12.15: append the throws-shim source for every
        // free fn carrying `cxx_throws` / `cxx_throws(T1, T2)`
        // annotations. Without this step the generated bindings
        // reference `__rustcc_throws_*` symbols that don't
        // exist in the final static lib, producing link errors.
        // v1.12.16: same now for class methods — collect both
        // free-fn + class-method ThrowsShimSpec entries into a
        // single render pass so `CxxRawError` + `#include`s
        // stay deduplicated.
        let mut combined_specs: Vec<crate::cxx_exception::ThrowsShimSpec> =
            Vec::new();
        combined_specs.extend(collect_throws_specs_for_free_fns(
            &ctx,
            &annotations,
            &free_fns,
        ));
        combined_specs.extend(collect_throws_specs_for_class_methods(
            &ctx,
            &annotations,
            &all_class_ids,
        ));
        // v1.12.17: also emit shims for throws-annotated ctors.
        combined_specs.extend(collect_throws_specs_for_ctors(
            &ctx,
            &annotations,
            &all_class_ids,
        ));
        if !combined_specs.is_empty() {
            let header_refs: Vec<&str> = self
                .headers
                .iter()
                .filter_map(|h| h.to_str())
                .collect();
            let throws_shim_src = crate::cxx_exception::render_all_throws_shims_cpp(
                &header_refs,
                &combined_specs,
            );
            shims_src.push('\n');
            shims_src.push_str(&throws_shim_src);
        }

        let shims_path = out_dir.join(&self.shims_filename);
        std::fs::write(&shims_path, &shims_src).map_err(|e| {
            BuildError::Io(format!(
                "write shims to {}: {e}",
                shims_path.display(),
            ))
        })?;

        // ----- 5. Compile shims. ------
        let static_lib_path = if self.invoke_cc {
            let mut cc_build = cc::Build::new();
            cc_build
                .cpp(true)
                .file(&shims_path)
                .flag_if_supported(&format!("-std={}", self.cpp_std));
            for inc in &self.include_paths {
                cc_build.include(inc);
            }
            for f in &self.clang_flags {
                cc_build.flag_if_supported(f);
            }
            for f in &self.cc_extra_flags {
                cc_build.flag(f);
            }
            // `cc::Build::compile` invokes the compiler, builds
            // an archive, prints `cargo:rustc-link-lib=static=`
            // and `cargo:rustc-link-search=` directives for
            // its own archive, and returns the archive path
            // implicitly. We recompute the path so callers
            // can introspect.
            cc_build.compile(static_lib_name);
            // cc writes lib<name>.a to OUT_DIR (Unix) or
            // <name>.lib (Windows). We don't need to be exact
            // here — Cargo already knows from cc's directives.
            #[cfg(unix)]
            let archive_name = format!("lib{static_lib_name}.a");
            #[cfg(windows)]
            let archive_name = format!("{static_lib_name}.lib");
            #[cfg(not(any(unix, windows)))]
            let archive_name = format!("lib{static_lib_name}.a");
            out_dir.join(archive_name)
        } else {
            // Test path: don't invoke cc, just stub the path.
            out_dir.join(format!("lib{static_lib_name}.a"))
        };

        // ----- 6. Cargo directives for user-supplied libs. ------
        // (cc::Build already emitted the directives for the
        // static lib it built. We add the system libs / search
        // paths / rerun-if-changed entries here.)
        let mut directives: Vec<String> = Vec::new();
        for path in &self.rustc_link_search {
            directives.push(format!(
                "cargo:rustc-link-search=native={}",
                path.display(),
            ));
        }
        for spec in &self.libs {
            directives.push(spec.cargo_directive());
        }
        for h in &self.headers {
            directives.push(format!("cargo:rerun-if-changed={}", h.display()));
        }
        for p in &self.extra_rerun_paths {
            directives.push(format!("cargo:rerun-if-changed={}", p.display()));
        }
        // `BUILD_RS_PRINTS_DIRECTIVES` is the de-facto contract
        // between build.rs and Cargo: anything starting with
        // `cargo:` on stdout is parsed by the cargo build
        // driver. Do this last so the cc-emitted ones (the
        // archive's link-lib + search-path) come first.
        for d in &directives {
            println!("{d}");
        }

        Ok(BuildOutputs {
            bindings_path,
            shims_path,
            static_lib_path,
            cargo_directives: directives,
            skip_log_path,
            skips: skip_records,
        })
    }

    /// Resolve `OUT_DIR` from the env or the override.
    fn resolve_out_dir(&self) -> Result<PathBuf, BuildError> {
        if let Some(p) = &self.out_dir_override {
            return Ok(p.clone());
        }
        std::env::var_os("OUT_DIR").map(PathBuf::from).ok_or_else(|| {
            BuildError::EnvMissing(
                "OUT_DIR not set; cargo runs build.rs with this env var \
                 set to the per-target output directory. For tests, call \
                 `Build::out_dir(...)` to override."
                    .into(),
            )
        })
    }
}

/// What `Build::compile` produced. Mostly diagnostic — Cargo
/// directives are already printed by the time `compile` returns
/// — but downstream tests or build introspection tooling may
/// want to assert paths exist.
#[derive(Debug, Clone)]
pub struct BuildOutputs {
    pub bindings_path: PathBuf,
    pub shims_path: PathBuf,
    pub static_lib_path: PathBuf,
    pub cargo_directives: Vec<String>,
    /// Path to the JSON skip-log sidecar (`bindings.skips.json`).
    /// Each record names a class + method + drop reason. Editor
    /// tooling consumes this to surface gaps in the Problems pane.
    pub skip_log_path: PathBuf,
    /// Same content as the JSON file, also returned in-memory so
    /// callers can introspect without a re-read.
    pub skips: Vec<SkipRecord>,
}

/// One method-emission skip recorded by the v0 bindings renderer.
/// Parsed from the `// N method[s] skipped by the v0 bindings
/// emitter:` comment blocks the renderer emits inside each
/// `impl Class { ... }` block. Stable across emitter internals
/// because we go through the source-text representation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkipRecord {
    /// Rust identifier of the enclosing class — matches the
    /// `impl <Class> { ... }` block this skip was found in.
    pub class: String,
    /// Resolved Rust method name as the renderer would have used
    /// (for ctor overloads: `new_<param-suffix>`; for failed
    /// virtual dispatch: `drop`, `handle`, etc.).
    pub method: String,
    /// Free-form reason as the renderer emits it. Editor tooling
    /// may want to bucket on substring matches like
    /// `extra ctor`, `virtual method without populated vtable_index`.
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SkipLog {
    schema_version: u32,
    generator: String,
    generator_version: String,
    skips: Vec<SkipRecord>,
}

/// Walk the rendered bindings source, locate `impl ClassName {`
/// blocks, and inside each look for the
/// `// N method[s] skipped by the v0 bindings emitter:` header
/// followed by `//   <method>: <reason>` lines.
///
/// The renderer emits these comments deterministically (sorted by
/// method name); we parse line-by-line without a full Rust parser.
/// Heuristic on the impl-line shape (`impl Foo {`) keeps the parser
/// tiny — the bindings file is generated, so the input shape is
/// stable enough for this to work.
fn parse_skip_records(src: &str) -> Vec<SkipRecord> {
    let mut out = Vec::new();
    let mut current_class: Option<String> = None;
    let mut in_skip_block = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("impl ") {
            // `impl Foo {` — capture class name. Skip trait impls
            // (`impl ::cxx::CxxBase<Bar> for Foo`) by checking for
            // a `for` token before the `{`.
            let head = rest.split('{').next().unwrap_or("").trim();
            if head.contains(" for ") {
                continue;
            }
            current_class = Some(head.split_whitespace().next().unwrap_or("").to_string());
            in_skip_block = false;
            continue;
        }
        if trimmed.starts_with("//") {
            // Header line introduces a skip block.
            if trimmed.contains("skipped by the v0 bindings emitter:") {
                in_skip_block = true;
                continue;
            }
            // Skip-block body: `//   method: reason`. The renderer
            // uses three spaces after the `//` to indent each entry.
            if in_skip_block {
                if let Some(body) = trimmed
                    .strip_prefix("//   ")
                    .or_else(|| trimmed.strip_prefix("// "))
                {
                    if let Some((method, reason)) = body.split_once(": ") {
                        if let Some(class) = current_class.clone() {
                            out.push(SkipRecord {
                                class,
                                method: method.trim().to_string(),
                                reason: reason.trim().to_string(),
                            });
                            continue;
                        }
                    }
                }
                // Any non-skip-shape comment line ends the block.
                in_skip_block = false;
            }
            continue;
        }
        // Non-comment, non-impl line: outside any skip block.
        in_skip_block = false;
    }
    out
}

/// Failures `Build::compile` can return. Each variant pins the
/// pipeline stage so caller errors land with maximum context.
#[derive(Debug)]
pub enum BuildError {
    /// libclang parse failed on one of the headers.
    Import(ImportError),
    /// `rust_bindings::generate_rust_bindings_full` rejected.
    Bindings(BindingsError),
    /// `Driver::emit_shims` rejected.
    Shim(ShimError),
    /// `cc::Build` compile failed (typically a syntax error in
    /// the generated shim source — surface as a bug report).
    /// `cc` itself prints a detailed diagnostic before
    /// returning.
    Compile(String),
    /// Filesystem I/O on the artifact files.
    Io(String),
    /// Required env var (e.g. `OUT_DIR`) wasn't set.
    EnvMissing(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Import(e) => write!(f, "import: {e:?}"),
            BuildError::Bindings(e) => write!(f, "bindings: {e:?}"),
            BuildError::Shim(e) => write!(f, "shim: {e:?}"),
            BuildError::Compile(m) => write!(f, "cc compile: {m}"),
            BuildError::Io(m) => write!(f, "io: {m}"),
            BuildError::EnvMissing(m) => write!(f, "env: {m}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Pick a [`Target`] for the importer's layout / mangling
/// computation. Real `build.rs` runs see `CARGO_CFG_TARGET_*`
/// env vars; non-build-script callers fall back to the
/// `Target::host_default()` (which is what tests get).
fn target_from_cargo_env() -> Target {
    // The `Target` struct in `rustc_abi_cxx` exposes named
    // constructors per-platform; pick by inspecting Cargo's
    // CFG vars. The build.rs runtime environment is the
    // canonical source — these strings match what
    // `rustc --print cfg` emits.
    //
    // Windows targets distinguish `-msvc` from `-gnu` via the
    // `target_env` slot: `msvc` => MSVC C++ ABI (mangler =
    // `mangle_msvc`, vtable = MSVC layout); `gnu` => Itanium ABI
    // via mingw-w64 (same backend as Linux Itanium).
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok();
    let os = std::env::var("CARGO_CFG_TARGET_OS").ok();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").ok();
    match (arch.as_deref(), os.as_deref(), env.as_deref()) {
        (Some("aarch64"), Some("macos"), _) => Target::aarch64_apple_darwin(),
        (Some("x86_64"), Some("macos"), _) => Target::x86_64_apple_darwin(),
        (Some("aarch64"), Some("linux"), _) => Target::aarch64_unknown_linux_gnu(),
        (Some("x86_64"), Some("linux"), _) => Target::x86_64_unknown_linux_gnu(),
        (Some("x86_64"), Some("windows"), Some("msvc")) => {
            Target::x86_64_pc_windows_msvc()
        }
        (Some("aarch64"), Some("windows"), Some("msvc")) => {
            Target::aarch64_pc_windows_msvc()
        }
        (Some("x86_64"), Some("windows"), Some("gnu")) => {
            Target::x86_64_pc_windows_gnu()
        }
        // Outside a build.rs run (or unrecognized triple) —
        // pick a sensible default. The generated bindings
        // are syntactically the same on every target; the
        // layout differences only matter for record-layout
        // assertions, which aren't in the build.rs hot path.
        _ => {
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            { Target::aarch64_apple_darwin() }
            #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
            { Target::x86_64_apple_darwin() }
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            { Target::aarch64_unknown_linux_gnu() }
            #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
            { Target::x86_64_unknown_linux_gnu() }
            #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
            { Target::x86_64_pc_windows_msvc() }
            #[cfg(all(target_arch = "aarch64", target_os = "windows", target_env = "msvc"))]
            { Target::aarch64_pc_windows_msvc() }
            #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "gnu"))]
            { Target::x86_64_pc_windows_gnu() }
            #[cfg(not(any(
                all(target_arch = "aarch64", target_os = "macos"),
                all(target_arch = "x86_64", target_os = "macos"),
                all(target_arch = "aarch64", target_os = "linux"),
                all(target_arch = "x86_64", target_os = "linux"),
                all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"),
                all(target_arch = "aarch64", target_os = "windows", target_env = "msvc"),
                all(target_arch = "x86_64", target_os = "windows", target_env = "gnu"),
            )))]
            { Target::x86_64_unknown_linux_gnu() }
        }
    }
}

/// Compose the libclang argv from the configured include
/// paths + clang flags. Mirrors what `Driver::parse_all`
/// builds internally.
fn build_argv(
    include_paths: &[PathBuf],
    clang_flags: &[String],
) -> Vec<String> {
    let mut argv: Vec<String> = vec!["-x".into(), "c++".into()];
    for inc in include_paths {
        argv.push(format!("-I{}", inc.display()));
    }
    argv.extend(clang_flags.iter().cloned());
    argv
}

/// Inspect an effective annotation list for a throws marker.
/// Returns `Some(typed_list)` for the typed form (possibly
/// empty for plain `cxx_throws`) and `None` when the entity
/// is not throws-tagged. Helper for the
/// `collect_throws_specs_for_*` walkers.
fn extract_throws_types(anns: &[crate::annotations::Annotation]) -> Option<Vec<String>> {
    use crate::annotations::Annotation;
    let mut is_throws = false;
    for ann in anns {
        match ann {
            Annotation::CxxThrowsTyped(types) => {
                return Some(types.clone());
            }
            Annotation::CxxThrows => {
                is_throws = true;
            }
            _ => {}
        }
    }
    if is_throws {
        Some(Vec::new())
    } else {
        None
    }
}

/// v1.12.15: build `ThrowsShimSpec` entries for every
/// throws-annotated free fn in `free_fns`. Returns an empty
/// vec when no fn is throws-tagged. Unsupported return / param
/// types drop the offending fn (the resulting link error then
/// surfaces the gap loudly).
fn collect_throws_specs_for_free_fns(
    ctx: &CxxTypeCtx,
    annotations: &AnnotationSet,
    free_fns: &FreeFnSet,
) -> Vec<crate::cxx_exception::ThrowsShimSpec> {
    use crate::cxx_exception::ThrowsShimSpec;
    use rustc_abi_cxx::NameSegment;

    let mut specs: Vec<ThrowsShimSpec> = Vec::new();
    for ff in &free_fns.entries {
        let fqn = if ff.parent.is_empty() {
            ff.name.0.clone()
        } else {
            let mut parts: Vec<String> = ff
                .parent
                .iter()
                .filter_map(|seg| match seg {
                    NameSegment::Namespace(id) => Some(id.0.clone()),
                    _ => None,
                })
                .collect();
            parts.push(ff.name.0.clone());
            parts.join("::")
        };

        let anns = annotations.effective(&fqn);
        let Some(typed_catches) = extract_throws_types(&anns) else {
            continue;
        };

        let return_type_cpp = match crate::shims::render_cxx_type(
            ctx,
            ff.sig.ret,
            &format!("throws shim return for {fqn}"),
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut param_decls: Vec<String> = Vec::with_capacity(ff.sig.params.len());
        let mut forward_args: Vec<String> = Vec::with_capacity(ff.sig.params.len());
        let mut params_ok = true;
        for (i, &p_ty) in ff.sig.params.iter().enumerate() {
            match crate::shims::render_cxx_type(
                ctx,
                p_ty,
                &format!("throws shim param {i} for {fqn}"),
            ) {
                Ok(ty_src) => {
                    param_decls.push(format!("{ty_src} __a{i}"));
                    forward_args.push(format!("__a{i}"));
                }
                Err(_) => {
                    params_ok = false;
                    break;
                }
            }
        }
        if !params_ok {
            continue;
        }

        specs.push(ThrowsShimSpec {
            wrapper_name: format!("__rustcc_throws_{}", ff.name.0),
            return_type_cpp,
            param_decls,
            forward_args,
            original_callsite: fqn,
            typed_catches,
            is_ctor: false,
        });
    }
    specs
}

/// v1.12.16: build `ThrowsShimSpec` entries for class methods
/// carrying `cxx_throws` / `cxx_throws(T1, T2)` annotations.
/// Today's scope: plain identifier-named **non-virtual,
/// non-static, non-special** instance methods. (Static, virtual,
/// ctor / dtor, operator, and conversion functions are all
/// silently skipped — the generated bindings will produce a
/// link error if they reference a `__rustcc_throws_*` symbol
/// we didn't emit, which is the desired "surface the gap"
/// behavior.)
///
/// The C++ shim body for an instance method is:
///
/// ```cpp
/// extern "C" CxxRawError __rustcc_throws_<Class>_<method>(
///     <ClassFQN>* __this,
///     <args>...,
///     <RetTy>* __out
/// ) noexcept {
///     try {
///         *__out = __this-><method>(<args>...);
///         return { 0, nullptr };
///     } catch (…) { … }
/// }
/// ```
///
/// Overloads: the v1.12.16 minimum drops every overload after
/// the first method of a given name (the second emission would
/// produce a duplicate symbol). Per-overload disambiguation is
/// tracked for a follow-on.
fn collect_throws_specs_for_class_methods(
    ctx: &CxxTypeCtx,
    annotations: &AnnotationSet,
    class_ids: &[rustc_abi_cxx::ClassId],
) -> Vec<crate::cxx_exception::ThrowsShimSpec> {
    use crate::cxx_exception::ThrowsShimSpec;
    use crate::name_mapping::{disambiguate_overloads, OverloadEntry};
    use rustc_abi_cxx::{MethodName, NameSegment, Virtuality};

    // Pre-rendered per-overload slot. We collect everything we
    // need for emission in a first pass, then v1.12.18 feeds
    // base names + param-type disambiguator strings through
    // `disambiguate_overloads` so each overload gets a unique
    // wrapper symbol (e.g. `divide` / `divide_int` /
    // `divide_double` for `int divide(int)`, `int divide(double)`).
    struct PreSpec {
        class_short: String,
        method_name: String,
        return_type_cpp: String,
        param_decls: Vec<String>,
        forward_args: Vec<String>,
        typed_catches: Vec<String>,
        // The C++-side callsite expression. Kept as a separate
        // field rather than re-derived during the second pass
        // because callers may want to override this in future
        // (e.g. for explicit-this static-style calls).
        callsite: String,
        // Param-type signature used as the disambiguator string.
        disambiguator: String,
    }
    let mut pre_specs: Vec<PreSpec> = Vec::new();

    for &class_id in class_ids {
        let class = ctx.class(class_id);

        // Build the class's C++ FQN (namespaces + class name).
        let class_fqn: String = class
            .name
            .0
            .iter()
            .map(|seg| match seg {
                NameSegment::Namespace(id) | NameSegment::Class(id) => id.0.clone(),
                NameSegment::TemplateSpec { name, .. } => name.0.clone(),
                NameSegment::AnonymousNamespace => String::new(),
                NameSegment::Enum(id) => id.0.clone(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::");

        let class_short = class_fqn
            .rsplit("::")
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if class_short.is_empty() {
            continue;
        }

        for method in &class.methods {
            if method.special.is_some() {
                continue;
            }
            if !matches!(method.virtuality, Virtuality::NonVirtual) {
                continue;
            }
            let method_name = match &method.name {
                MethodName::Ident(id) => id.0.clone(),
                _ => continue,
            };

            let fqn = format!("{class_fqn}::{method_name}");
            let anns = annotations.effective(&fqn);
            let Some(typed_catches) = extract_throws_types(&anns) else {
                continue;
            };

            let return_type_cpp = match crate::shims::render_cxx_type(
                ctx,
                method.sig.ret,
                &format!("throws shim return for {fqn}"),
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let this_decl = if method.sig.cv.is_const {
                format!("const {class_fqn}* __this")
            } else {
                format!("{class_fqn}* __this")
            };
            let mut param_decls: Vec<String> = vec![this_decl];
            let mut forward_args: Vec<String> = Vec::with_capacity(method.sig.params.len());
            let mut rendered_params: Vec<String> = Vec::with_capacity(method.sig.params.len());
            let mut params_ok = true;
            for (i, &p_ty) in method.sig.params.iter().enumerate() {
                match crate::shims::render_cxx_type(
                    ctx,
                    p_ty,
                    &format!("throws shim param {i} for {fqn}"),
                ) {
                    Ok(ty_src) => {
                        param_decls.push(format!("{ty_src} __a{i}"));
                        forward_args.push(format!("__a{i}"));
                        rendered_params.push(ty_src);
                    }
                    Err(_) => {
                        params_ok = false;
                        break;
                    }
                }
            }
            if !params_ok {
                continue;
            }

            // Build the disambiguator from the C++ param-type
            // list (e.g. `int_double` for `(int, double)`).
            // Empty for no-arg methods — `disambiguate_overloads`
            // handles the empty-disambiguator case by leaving
            // the base name unsuffixed when there's no
            // collision, and producing a suffix derived from
            // the position otherwise.
            let disamb = rendered_params.join("_");

            pre_specs.push(PreSpec {
                class_short: class_short.clone(),
                method_name: method_name.clone(),
                return_type_cpp,
                param_decls,
                forward_args,
                typed_catches,
                callsite: format!("__this->{method_name}"),
                disambiguator: disamb,
            });
        }
    }

    // Second pass: feed (base_name, disambiguator) pairs through
    // the workspace's `disambiguate_overloads` so each overload
    // gets a unique suffix. Same machinery the Rust-side bindings
    // emitter uses for safe-wrapper names — keeps the shim
    // symbols + the Rust wrapper symbols in lock-step.
    let entries: Vec<OverloadEntry<&str>> = pre_specs
        .iter()
        .map(|p| OverloadEntry {
            base_name: p.method_name.as_str(),
            disambiguator: p.disambiguator.as_str(),
        })
        .collect();
    let resolved = disambiguate_overloads(entries);

    pre_specs
        .into_iter()
        .zip(resolved)
        .map(|(pre, resolved_ident)| ThrowsShimSpec {
            wrapper_name: format!("__rustcc_throws_{}_{}", pre.class_short, resolved_ident.0),
            return_type_cpp: pre.return_type_cpp,
            param_decls: pre.param_decls,
            forward_args: pre.forward_args,
            original_callsite: pre.callsite,
            typed_catches: pre.typed_catches,
            is_ctor: false,
        })
        .collect()
}

/// v1.12.17: build `ThrowsShimSpec` entries for class
/// constructors carrying `cxx_throws` / `cxx_throws(T1, T2)`
/// annotations. Today's scope: `DefaultCtor` + `OtherCtor`
/// (copy/move ctors are not yet wired to the throws path).
///
/// Annotation key form: `Class::Class` — libclang names ctor
/// cursors after the class itself, so the FQN doubles the
/// last segment (matches the v1.12.4 emitter convention).
///
/// Overloads: first ctor with the throws annotation wins;
/// subsequent ctors with the same wrapper name (always
/// `__rustcc_throws_<Class>_new` in v1.12.17) are skipped.
/// Per-overload disambiguation tracks as a follow-on.
fn collect_throws_specs_for_ctors(
    ctx: &CxxTypeCtx,
    annotations: &AnnotationSet,
    class_ids: &[rustc_abi_cxx::ClassId],
) -> Vec<crate::cxx_exception::ThrowsShimSpec> {
    use crate::cxx_exception::ThrowsShimSpec;
    use rustc_abi_cxx::{NameSegment, SpecialMember};

    let mut specs: Vec<ThrowsShimSpec> = Vec::new();
    let mut seen_classes: std::collections::HashSet<rustc_abi_cxx::ClassId> =
        std::collections::HashSet::new();

    for &class_id in class_ids {
        let class = ctx.class(class_id);
        let class_fqn: String = class
            .name
            .0
            .iter()
            .map(|seg| match seg {
                NameSegment::Namespace(id) | NameSegment::Class(id) => id.0.clone(),
                NameSegment::TemplateSpec { name, .. } => name.0.clone(),
                NameSegment::AnonymousNamespace => String::new(),
                NameSegment::Enum(id) => id.0.clone(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::");
        let class_short = class_fqn
            .rsplit("::")
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if class_short.is_empty() {
            continue;
        }

        // libclang ctor annotation key: `Class::Class`
        // (matches the `method_source_name_for_fqn` path in
        // `render_direct_extern_class`).
        let fqn = format!("{class_fqn}::{class_short}");
        let anns = annotations.effective(&fqn);
        let Some(typed_catches) = extract_throws_types(&anns) else {
            continue;
        };

        // Find the first DefaultCtor / OtherCtor with throws.
        for method in &class.methods {
            if !matches!(
                method.special,
                Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor),
            ) {
                continue;
            }
            if !seen_classes.insert(class_id) {
                break;
            }

            // Render params (no return type — ctor doesn't
            // produce one; the `*__out` slot IS the result).
            let mut param_decls: Vec<String> = Vec::with_capacity(method.sig.params.len());
            let mut forward_args: Vec<String> = Vec::with_capacity(method.sig.params.len());
            let mut params_ok = true;
            for (i, &p_ty) in method.sig.params.iter().enumerate() {
                match crate::shims::render_cxx_type(
                    ctx,
                    p_ty,
                    &format!("throws ctor shim param {i} for {fqn}"),
                ) {
                    Ok(ty_src) => {
                        param_decls.push(format!("{ty_src} __a{i}"));
                        forward_args.push(format!("__a{i}"));
                    }
                    Err(_) => {
                        params_ok = false;
                        break;
                    }
                }
            }
            if !params_ok {
                continue;
            }

            specs.push(ThrowsShimSpec {
                wrapper_name: format!("__rustcc_throws_{class_short}_new"),
                // Return type isn't used in the ctor renderer
                // path; the shim is fixed to `CxxRawError`.
                return_type_cpp: String::new(),
                param_decls,
                forward_args,
                // For ctor specs, `original_callsite` is the
                // class FQN to placement-construct (the
                // renderer reads `is_ctor` and switches modes).
                original_callsite: class_fqn.clone(),
                typed_catches,
                is_ctor: true,
            });
            break;
        }
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skip_records_handles_zero_skips() {
        let src = "impl Foo {\n    pub fn bar(&self) {}\n}\n";
        assert!(parse_skip_records(src).is_empty());
    }

    #[test]
    fn parse_skip_records_picks_up_class_method_pairs() {
        let src = "\
impl Fl_Window {
    // 2 methods skipped by the v0 bindings emitter:
    //   new_const_fl_pixmap_u32: extra ctor — v0 emitter renders only one ctor per class
    //   drop: virtual method without populated vtable_index (multi-inh)
    pub fn new(_a: i32) -> Self { todo!() }
}

impl Fl_Box {
    // 1 method skipped by the v0 bindings emitter:
    //   new_fl_boxtype_i32_i32_i32_i32_const_i8: extra ctor
    pub fn new(_a: i32) -> Self { todo!() }
}
";
        let recs = parse_skip_records(src);
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].class, "Fl_Window");
        assert_eq!(recs[0].method, "new_const_fl_pixmap_u32");
        assert!(recs[0].reason.contains("extra ctor"));
        assert_eq!(recs[1].class, "Fl_Window");
        assert_eq!(recs[1].method, "drop");
        assert_eq!(recs[2].class, "Fl_Box");
    }

    #[test]
    fn parse_skip_records_ignores_trait_impls() {
        // `impl ::cxx::CxxBase<Bar> for Foo` blocks must not leak
        // into `current_class` because their bodies aren't shaped
        // like inherent skip blocks.
        let src = "\
impl ::cxx::CxxBase<Bar> for Foo {
    fn upcast(&self) -> &Bar { todo!() }
}

impl Foo {
    // 1 method skipped by the v0 bindings emitter:
    //   helper: extra ctor
    pub fn other(&self) {}
}
";
        let recs = parse_skip_records(src);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].class, "Foo");
        assert_eq!(recs[0].method, "helper");
    }

    #[test]
    fn link_spec_cargo_directives_have_the_expected_shape() {
        assert_eq!(
            LinkSpec::Lib("fltk".into()).cargo_directive(),
            "cargo:rustc-link-lib=fltk",
        );
        assert_eq!(
            LinkSpec::Static("foo".into()).cargo_directive(),
            "cargo:rustc-link-lib=static=foo",
        );
        assert_eq!(
            LinkSpec::Dylib("bar".into()).cargo_directive(),
            "cargo:rustc-link-lib=dylib=bar",
        );
        assert_eq!(
            LinkSpec::Framework("Cocoa".into()).cargo_directive(),
            "cargo:rustc-link-lib=framework=Cocoa",
        );
        // Weak frameworks emit two directives (separator: \n)
        // so cargo splits them on parse.
        let weak = LinkSpec::WeakFramework("ScreenCaptureKit".into())
            .cargo_directive();
        assert!(weak.contains("cargo:rustc-link-arg=-weak_framework"));
        assert!(weak.contains("cargo:rustc-link-arg=ScreenCaptureKit"));
    }

    #[test]
    fn builder_setters_compose_flags_in_order() {
        let mut b = Build::new();
        b.header("a.hpp")
            .header("b.hpp")
            .include_path("/inc1")
            .include_path("/inc2")
            .clang_flag("-DFOO=1")
            .clang_flag("-DBAR=2")
            .cpp_std("c++20")
            .cstr_ergonomics(true)
            .link("fltk")
            .framework("Cocoa")
            .weak_framework("UniformTypeIdentifiers")
            .lib_search_path("/opt/lib")
            .invoke_cc(false);
        assert_eq!(b.headers.len(), 2);
        assert_eq!(b.include_paths.len(), 2);
        assert_eq!(b.clang_flags.len(), 2);
        assert_eq!(b.cpp_std, "c++20");
        assert!(b.cstr_ergonomics);
        assert_eq!(b.libs.len(), 3);
        assert!(matches!(b.libs[0], LinkSpec::Lib(_)));
        assert!(matches!(b.libs[1], LinkSpec::Framework(_)));
        assert!(matches!(b.libs[2], LinkSpec::WeakFramework(_)));
        assert_eq!(b.rustc_link_search.len(), 1);
        assert!(!b.invoke_cc);
    }

    #[test]
    fn build_argv_prepends_x_cpp_then_includes_then_flags() {
        let inc = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let flags = vec!["-DFOO".to_string(), "-std=c++17".to_string()];
        let argv = build_argv(&inc, &flags);
        assert_eq!(
            argv,
            vec![
                "-x".to_string(),
                "c++".to_string(),
                "-I/a".to_string(),
                "-I/b".to_string(),
                "-DFOO".to_string(),
                "-std=c++17".to_string(),
            ],
        );
    }
}
