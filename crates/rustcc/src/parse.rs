//! Libclang invocation + input fingerprinting.
//!
//! Implements the "parse once" step of `docs/build_integration.md §4`
//! plus the fingerprint bookkeeping that §6's incremental story builds
//! on. The fingerprint is a SHA256 over sorted header contents, sorted
//! include paths, and ordered clang flags; it's written to
//! `target/rustcc/<crate>.fingerprint` only after the full interop
//! pipeline succeeds, so a crash between phases doesn't leave a stale
//! fingerprint that would make later runs skip real work.
//!
//! API split:
//!
//! - [`check_fingerprint`] hashes the inputs and reads any prior marker;
//!   cheap, libclang-free, always safe to call.
//! - [`parse_headers`] runs libclang via `cxx_importer::Driver` and
//!   returns the [`ParsedIr`] that M4/M5 consume.
//! - [`commit_fingerprint`] atomically writes the current hash so the
//!   next run recognizes the inputs. Call at the very end of a
//!   successful pipeline.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::CppInteropConfig;

#[derive(Debug)]
pub struct FingerprintState {
    /// SHA256 of the current inputs (hex).
    pub current: String,
    /// Prior run's fingerprint, if the marker file existed and parsed.
    pub prior: Option<String>,
    /// Where the marker lives on disk.
    pub path: PathBuf,
}

impl FingerprintState {
    /// True when `current` differs from `prior` (including "no prior").
    pub fn inputs_changed(&self) -> bool {
        self.prior.as_deref() != Some(self.current.as_str())
    }
}

/// Parsed IR ready to hand to M4 (shim emission) and M5 (`.hpp`
/// emission). Only present when the `libclang` feature is on — this
/// struct type is gated out entirely otherwise so downstream code
/// can't accidentally depend on it in no-libclang builds.
#[cfg(feature = "libclang")]
pub struct ParsedIr {
    pub ctx: rustc_abi_cxx::CxxTypeCtx,
    pub classes: Vec<rustc_abi_cxx::ClassId>,
}

#[derive(Debug)]
pub enum ParseError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The `libclang` feature wasn't compiled in, so header parsing
    /// is unavailable. Callers in interop mode should surface this as
    /// a warning or hard error depending on whether downstream phases
    /// need the IR.
    LibclangDisabled,
    /// An error returned by `cxx_importer::Driver::parse_all`.
    Import(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::LibclangDisabled => f.write_str(
                "rustcc was built without the `libclang` feature; \
                 header parsing is disabled",
            ),
            Self::Import(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ParseError {}

/// Pure function of the config + on-disk header content. No libclang,
/// no cache writes. Does NOT include Rust sources — use
/// [`fingerprint_with_rust_sources`] when a Rust scan is part of the
/// pipeline and edits to Rust-declared types should invalidate the
/// cache.
pub fn compute_fingerprint(
    config: &CppInteropConfig,
) -> Result<String, ParseError> {
    let mut hasher = Sha256::new();
    hasher.update(b"rustcc-fingerprint-v1\n");
    hash_config_inputs(&mut hasher, config)?;
    Ok(hex_encode(&hasher.finalize()))
}

/// Fingerprint that also includes Rust-source bodies. Paths are sorted
/// so file-enumeration order doesn't matter; bodies are mixed in so
/// any edit to a `#[repr(cpp)]` struct (or even surrounding code)
/// flips the hash and busts the cache.
pub fn fingerprint_with_rust_sources(
    config: &CppInteropConfig,
    rust_sources: &[PathBuf],
) -> Result<String, ParseError> {
    fingerprint_with_toolchain(config, rust_sources, None)
}

/// Fingerprint that factors in rust sources and, optionally, the
/// detected toolchain versions. When `toolchain` is `Some`, changing
/// either `rustc --version` or `clang++ --version` invalidates every
/// cached artifact — the only safe move, since the emitters encode
/// target/ABI assumptions that can shift between compiler versions.
pub fn fingerprint_with_toolchain(
    config: &CppInteropConfig,
    rust_sources: &[PathBuf],
    toolchain: Option<&crate::toolchain::Toolchain>,
) -> Result<String, ParseError> {
    let mut hasher = Sha256::new();
    // Version bump: v2 added Rust sources, v3 adds optional
    // toolchain strings. Old v2 fingerprints won't collide because
    // the magic header changed.
    hasher.update(b"rustcc-fingerprint-v3\n");
    hash_config_inputs(&mut hasher, config)?;

    let mut rs: Vec<&PathBuf> = rust_sources.iter().collect();
    rs.sort();
    for r in rs {
        let body = std::fs::read(r).map_err(|error| ParseError::Io {
            path: r.to_path_buf(),
            error,
        })?;
        let path_str = r.to_string_lossy();
        hasher.update(b"rs:");
        hasher.update((path_str.len() as u64).to_le_bytes());
        hasher.update(path_str.as_bytes());
        hasher.update(b"body:");
        hasher.update((body.len() as u64).to_le_bytes());
        hasher.update(&body);
    }

    if let Some(tc) = toolchain {
        hasher.update(b"rustc:");
        hasher.update((tc.rustc.version.len() as u64).to_le_bytes());
        hasher.update(tc.rustc.version.as_bytes());
        hasher.update(b"clang:");
        hasher.update((tc.clang.version.len() as u64).to_le_bytes());
        hasher.update(tc.clang.version.as_bytes());
    }

    Ok(hex_encode(&hasher.finalize()))
}

fn hash_config_inputs(
    hasher: &mut Sha256,
    config: &CppInteropConfig,
) -> Result<(), ParseError> {
    let mut headers: Vec<&PathBuf> = config.headers.iter().collect();
    headers.sort();
    for header in headers {
        let body =
            std::fs::read(header).map_err(|error| ParseError::Io {
                path: header.to_path_buf(),
                error,
            })?;
        let path_str = header.to_string_lossy();
        hasher.update(b"header:");
        hasher.update((path_str.len() as u64).to_le_bytes());
        hasher.update(path_str.as_bytes());
        hasher.update(b"body:");
        hasher.update((body.len() as u64).to_le_bytes());
        hasher.update(&body);
    }

    let mut includes: Vec<&PathBuf> =
        config.header_search_paths.iter().collect();
    includes.sort();
    for inc in includes {
        let s = inc.to_string_lossy();
        hasher.update(b"include:");
        hasher.update((s.len() as u64).to_le_bytes());
        hasher.update(s.as_bytes());
    }

    // Flag order matters (e.g., `-DFOO -UFOO` vs. `-UFOO -DFOO`).
    for flag in &config.clang_flags {
        hasher.update(b"flag:");
        hasher.update((flag.len() as u64).to_le_bytes());
        hasher.update(flag.as_bytes());
    }
    Ok(())
}

/// Compute the current fingerprint and load any prior marker. No
/// side effects beyond reading.
pub fn check_fingerprint(
    config: &CppInteropConfig,
    cache_dir: &Path,
    crate_name: &str,
) -> Result<FingerprintState, ParseError> {
    let current = compute_fingerprint(config)?;
    let path = cache_dir.join(format!("{crate_name}.fingerprint"));
    let prior = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string());
    Ok(FingerprintState {
        current,
        prior,
        path,
    })
}

/// Write the current fingerprint. Callers invoke this at the end of a
/// successful pipeline so that partial failures don't leave a stale
/// marker that would make later runs skip real work.
pub fn commit_fingerprint(state: &FingerprintState) -> Result<(), ParseError> {
    if let Some(parent) = state.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ParseError::Io {
                path: parent.to_path_buf(),
                error,
            }
        })?;
    }
    std::fs::write(&state.path, &state.current).map_err(|error| {
        ParseError::Io {
            path: state.path.clone(),
            error,
        }
    })
}

/// Invoke libclang via `cxx_importer::Driver` and return the parsed IR.
#[cfg(feature = "libclang")]
pub fn parse_headers(
    config: &CppInteropConfig,
) -> Result<ParsedIr, ParseError> {
    use cxx_importer::Driver;
    use rustc_abi_cxx::{CxxTypeCtx, Target};

    let graph = config.to_header_graph();
    let driver = Driver::new(graph);
    let mut ctx = CxxTypeCtx::new(Target::host());
    let classes = driver
        .parse_all(&mut ctx)
        .map_err(|e| ParseError::Import(format!("{e}")))?;
    Ok(ParsedIr { ctx, classes })
}

#[cfg(not(feature = "libclang"))]
pub fn parse_headers(
    _config: &CppInteropConfig,
) -> Result<(), ParseError> {
    Err(ParseError::LibclangDisabled)
}

/// Figure out where to write per-crate cache artifacts. Honors
/// `CARGO_TARGET_DIR` when set (cargo passes this through for
/// out-of-tree target layouts); otherwise `manifest_dir/target/rustcc/`.
pub fn cache_dir(manifest_dir: &Path) -> PathBuf {
    if let Some(env) = std::env::var_os("CARGO_TARGET_DIR") {
        let p = PathBuf::from(env);
        if p.is_absolute() {
            return p.join("rustcc");
        }
        return manifest_dir.join(p).join("rustcc");
    }
    manifest_dir.join("target").join("rustcc")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_headers(headers: Vec<PathBuf>) -> CppInteropConfig {
        CppInteropConfig {
            headers,
            header_search_paths: Vec::new(),
            clang_flags: Vec::new(),
            stdlib: None,
            sidecar: None,
            link_libraries: Vec::new(),
            link_search_paths: Vec::new(),
            emit_stubs: false,
            emit_forwarders: false,
        }
    }

    #[test]
    fn fingerprint_is_stable_across_repeated_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("w.hpp");
        std::fs::write(&h, "struct W { int x; };\n").unwrap();

        let cfg = cfg_with_headers(vec![h]);
        let a = compute_fingerprint(&cfg).unwrap();
        let b = compute_fingerprint(&cfg).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_changes_on_header_body_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("w.hpp");
        std::fs::write(&h, "struct W { int x; };\n").unwrap();
        let cfg = cfg_with_headers(vec![h.clone()]);
        let before = compute_fingerprint(&cfg).unwrap();

        std::fs::write(&h, "struct W { int x; int y; };\n").unwrap();
        let after = compute_fingerprint(&cfg).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn fingerprint_changes_when_flags_reorder() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("w.hpp");
        std::fs::write(&h, "").unwrap();

        let mut cfg = cfg_with_headers(vec![h]);
        cfg.clang_flags = vec!["-std=c++17".into(), "-DFOO".into()];
        let a = compute_fingerprint(&cfg).unwrap();
        cfg.clang_flags = vec!["-DFOO".into(), "-std=c++17".into()];
        let b = compute_fingerprint(&cfg).unwrap();
        assert_ne!(a, b, "flag order matters to the compiler");
    }

    #[test]
    fn fingerprint_stable_when_header_list_reordered() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = tmp.path().join("a.hpp");
        let h2 = tmp.path().join("b.hpp");
        std::fs::write(&h1, "struct A {};\n").unwrap();
        std::fs::write(&h2, "struct B {};\n").unwrap();

        let mut cfg = cfg_with_headers(vec![h1.clone(), h2.clone()]);
        let a = compute_fingerprint(&cfg).unwrap();
        cfg.headers = vec![h2, h1];
        let b = compute_fingerprint(&cfg).unwrap();
        assert_eq!(a, b, "header order shouldn't matter");
    }

    #[test]
    fn missing_header_surfaces_as_io_error() {
        let cfg = cfg_with_headers(vec![PathBuf::from(
            "/definitely/not/a/real/path.hpp",
        )]);
        let err = compute_fingerprint(&cfg).unwrap_err();
        assert!(matches!(err, ParseError::Io { .. }));
    }

    #[test]
    fn check_and_commit_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("w.hpp");
        std::fs::write(&h, "struct W {};\n").unwrap();
        let cfg = cfg_with_headers(vec![h.clone()]);
        let cache = tmp.path().join("cache");

        let state =
            check_fingerprint(&cfg, &cache, "mycrate").unwrap();
        assert!(state.inputs_changed(), "first run is always a miss");
        assert!(state.prior.is_none());

        commit_fingerprint(&state).unwrap();
        assert!(state.path.is_file());

        let state2 =
            check_fingerprint(&cfg, &cache, "mycrate").unwrap();
        assert!(!state2.inputs_changed(), "unchanged inputs → hit");
        assert_eq!(state.current, state2.current);
        assert_eq!(state2.prior.as_deref(), Some(state.current.as_str()));
    }

    #[cfg(not(feature = "libclang"))]
    #[test]
    fn parse_headers_without_libclang_feature_errors() {
        let cfg = cfg_with_headers(Vec::new());
        let err = parse_headers(&cfg).unwrap_err();
        assert!(matches!(err, ParseError::LibclangDisabled));
    }

    #[cfg(feature = "libclang")]
    #[test]
    fn parse_headers_returns_ir_with_class_count() {
        let _g = crate::LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("w.hpp");
        std::fs::write(&h, "struct Widget { int x; };\n").unwrap();
        let cfg = cfg_with_headers(vec![h]);
        let ir = parse_headers(&cfg).unwrap();
        assert_eq!(ir.classes.len(), 1);
    }
}
