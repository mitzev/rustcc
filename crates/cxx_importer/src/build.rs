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
        let target = self
            .target
            .clone()
            .unwrap_or_else(target_from_cargo_env);
        let mut ctx = CxxTypeCtx::new(target);
        let annotations = AnnotationSet::default();
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

        // ----- 4. Emit C++ shims. ------
        let shims_src =
            driver.emit_shims(&ctx, &all_class_ids).map_err(BuildError::Shim)?;
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
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok();
    let os = std::env::var("CARGO_CFG_TARGET_OS").ok();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").ok();
    match (arch.as_deref(), os.as_deref(), env.as_deref()) {
        (Some("aarch64"), Some("macos"), _) => Target::aarch64_apple_darwin(),
        (Some("x86_64"), Some("macos"), _) => Target::x86_64_apple_darwin(),
        (Some("aarch64"), Some("linux"), _) => Target::aarch64_unknown_linux_gnu(),
        (Some("x86_64"), Some("linux"), _) => Target::x86_64_unknown_linux_gnu(),
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
            #[cfg(not(any(
                all(target_arch = "aarch64", target_os = "macos"),
                all(target_arch = "x86_64", target_os = "macos"),
                all(target_arch = "aarch64", target_os = "linux"),
                all(target_arch = "x86_64", target_os = "linux"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
