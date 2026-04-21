//! `rustcc` — rustc-compatible driver with C++ interop layered on.
//!
//! See `docs/build_integration.md §2`.
//!
//! ## Current scope (M1–M6)
//!
//! - **M1**: rustc-argv passthrough with an `Invocation` classifier.
//! - **M2**: `[cpp-interop]` manifest parsing.
//! - **M3**: libclang invocation + input fingerprinting.
//! - **M4**: Shim `.cpp` emission and Clang compilation.
//! - **M5**: Generated `.hpp` emission.
//! - **M6 — now**: Link step mixing rustc and clang objects.
//!
//! The binary entry point (`main.rs`) stays thin; all logic lives here
//! so tests can drive it without spawning processes.

#[cfg(feature = "libclang")]
mod hpp;
mod link;
mod manifest;
mod parse;
pub mod rust_forwarders;
pub mod rust_hpp;
pub mod rust_scan;
pub mod rust_stubs;
pub mod toolchain;
#[cfg(feature = "libclang")]
mod shims;

/// libclang's `Clang::new()` is process-exclusive, so parallel tests
/// within this binary that each run the importer would race. All
/// `#[cfg(feature = "libclang")]` tests lock this before invoking
/// `parse_headers` (or anything else that ends up at `Clang::new`).
#[cfg(test)]
pub(crate) static LIBCLANG: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use manifest::{
    find_manifest, load_config, CppInteropConfig, ManifestError, Stdlib,
};
pub use link::compute_link_args;
pub use parse::{
    cache_dir, check_fingerprint, commit_fingerprint, compute_fingerprint,
    fingerprint_with_rust_sources, fingerprint_with_toolchain, parse_headers,
    FingerprintState, ParseError,
};
#[cfg(feature = "libclang")]
pub use parse::ParsedIr;
pub use rust_hpp::{emit as emit_rust_hpp, RustHppArtifact, RustHppError};
pub use rust_scan::{scan as scan_rust_sources, RustScanError, RustScanResult};
pub use rust_stubs::{
    emit as emit_rust_stubs, RustStubsArtifact, RustStubsError,
};
pub use rust_forwarders::{
    emit as emit_rust_forwarders, RustForwardersArtifact, RustForwardersError,
};
#[cfg(feature = "libclang")]
pub use shims::{
    emit_and_compile, resolve_clang, ShimArtifacts, ShimError,
};
#[cfg(feature = "libclang")]
pub use hpp::{emit as emit_hpp, HppArtifact, HppError};

use std::path::{Path, PathBuf};
use std::process::Command;

/// What kind of rustc invocation this is, derived from argv alone.
///
/// The rustc CLI doesn't carry manifest info — cargo just hands us
/// compiler flags and source paths. For M1 we detect interop crates
/// via the `--cfg cpp_interop` flag that the user (or a future cargo
/// plugin) sets. M2 will also walk up from `source_file` to
/// `Cargo.toml` and check for `[cpp-interop]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// No interop marker present. Driver should exec rustc verbatim.
    Passthrough,
    /// Interop crate. `source_file` is the first positional argument
    /// (`src/lib.rs` in cargo invocations), used by M2+ to locate
    /// `Cargo.toml`.
    Interop { source_file: Option<PathBuf> },
}

/// Inspect `argv` (without the program name) and decide whether this
/// is an interop build or a plain rustc passthrough.
///
/// Passthrough is the default: most crates in a workspace don't use
/// C++ interop, and the driver must not slow those down. We only take
/// over when there's an explicit marker.
pub fn classify_invocation(argv: &[String]) -> Invocation {
    if has_cfg_flag(argv, "cpp_interop") {
        Invocation::Interop {
            source_file: find_source_file(argv),
        }
    } else {
        Invocation::Passthrough
    }
}

/// Locate the `rustc` executable to dispatch to. Honors the `RUSTC`
/// env var (cargo always sets this) then falls through to `rustc` on
/// `PATH`.
pub fn resolve_rustc() -> PathBuf {
    if let Some(explicit) = std::env::var_os("RUSTC") {
        return PathBuf::from(explicit);
    }
    PathBuf::from("rustc")
}

/// Build a `Command` that will invoke rustc with `argv`. Stdio inherits
/// the parent process by default. Intended for tests and non-unix
/// platforms where `exec` isn't available.
pub fn rustc_command(rustc: &Path, argv: &[String]) -> Command {
    let mut cmd = Command::new(rustc);
    cmd.args(argv);
    cmd
}

fn has_cfg_flag(argv: &[String], cfg: &str) -> bool {
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        if arg == "--cfg" {
            if let Some(val) = iter.next() {
                if val == cfg {
                    return true;
                }
            }
        } else if let Some(rest) = arg.strip_prefix("--cfg=") {
            if rest == cfg {
                return true;
            }
        }
    }
    false
}

/// Extract `--crate-name foo` / `--crate-name=foo` from argv. Returns
/// `None` if the flag isn't present (cargo always sets it, but direct
/// rustc invocations might skip it).
pub fn find_crate_name(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        if arg == "--crate-name" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--crate-name=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn find_source_file(argv: &[String]) -> Option<PathBuf> {
    // rustc's positional argv has one crate-root file, typically at
    // the tail. We skip flags and option values. Keep this conservative
    // — if the shape isn't what we expect, return None and let M2's
    // manifest walk handle the fallback path (it'll look relative to
    // the current directory).
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        if arg.starts_with("--") {
            // `--flag value` or `--flag=value`. For long options rustc
            // uniformly accepts `=`-separated, so only `--flag value`
            // needs an extra skip.
            if !arg.contains('=') && takes_value(arg) {
                iter.next();
            }
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            // Short options like `-C opt-level=3` or `-L path`. Assume
            // the value is in the next argv for options we care about.
            if takes_value_short(arg) {
                iter.next();
            }
            continue;
        }
        // Positional — rustc treats the first non-flag as the crate root.
        return Some(PathBuf::from(arg));
    }
    None
}

fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--edition"
            | "--crate-name"
            | "--crate-type"
            | "--emit"
            | "--out-dir"
            | "--target"
            | "--extern"
            | "--cfg"
            | "--check-cfg"
            | "--error-format"
            | "--json"
            | "--color"
            | "--sysroot"
            | "--codegen"
    )
}

fn takes_value_short(flag: &str) -> bool {
    // rustc's short options that take a space-separated value.
    // Long-form `-Copt-level=3` is self-contained; `-C opt-level=3`
    // takes the next argv as value.
    matches!(flag, "-C" | "-L" | "-l" | "-W" | "-A" | "-D" | "-F" | "-Z" | "-o")
        && flag.len() == 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_invocation_is_passthrough() {
        let argv = v(&["--edition", "2021", "--crate-name", "foo", "src/lib.rs"]);
        assert_eq!(classify_invocation(&argv), Invocation::Passthrough);
    }

    #[test]
    fn cfg_cpp_interop_space_form_detected() {
        let argv = v(&["--cfg", "cpp_interop", "src/lib.rs"]);
        match classify_invocation(&argv) {
            Invocation::Interop { source_file } => {
                assert_eq!(source_file, Some(PathBuf::from("src/lib.rs")));
            }
            other => panic!("expected Interop, got {other:?}"),
        }
    }

    #[test]
    fn cfg_cpp_interop_equals_form_detected() {
        let argv = v(&["--cfg=cpp_interop", "src/lib.rs"]);
        assert!(matches!(
            classify_invocation(&argv),
            Invocation::Interop { .. }
        ));
    }

    #[test]
    fn other_cfg_does_not_trigger_interop() {
        let argv = v(&["--cfg", "feature=\"bar\"", "src/lib.rs"]);
        assert_eq!(classify_invocation(&argv), Invocation::Passthrough);
    }

    #[test]
    fn find_source_file_skips_short_option_values() {
        // `-C opt-level=3` takes the next arg as its value. The source
        // file is the positional after.
        let argv = v(&["-C", "opt-level=3", "-L", "dep/path", "src/lib.rs"]);
        assert_eq!(find_source_file(&argv), Some(PathBuf::from("src/lib.rs")));
    }

    #[test]
    fn find_source_file_skips_long_option_values() {
        let argv = v(&[
            "--edition",
            "2021",
            "--crate-name",
            "foo",
            "--out-dir",
            "target/debug",
            "src/main.rs",
        ]);
        assert_eq!(find_source_file(&argv), Some(PathBuf::from("src/main.rs")));
    }

    #[test]
    fn find_source_file_accepts_equals_form_without_consuming_next() {
        // `--crate-type=lib` is self-contained; the next arg is the source.
        let argv = v(&["--crate-type=lib", "src/lib.rs"]);
        assert_eq!(find_source_file(&argv), Some(PathBuf::from("src/lib.rs")));
    }

    #[test]
    fn resolve_rustc_honors_env_var() {
        // Save/restore so we don't affect other tests.
        let prev = std::env::var_os("RUSTC");
        // SAFETY: single-threaded test; we restore before returning.
        unsafe { std::env::set_var("RUSTC", "/tmp/fake-rustc") };
        assert_eq!(resolve_rustc(), PathBuf::from("/tmp/fake-rustc"));
        match prev {
            Some(v) => unsafe { std::env::set_var("RUSTC", v) },
            None => unsafe { std::env::remove_var("RUSTC") },
        }
    }
}
