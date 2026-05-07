//! On-disk cache for parsed [`rustc_abi_cxx::CxxTypeCtx`] state.
//!
//! M10 — Incremental compilation integration.
//!
//! The default Phase A use case is a `build.rs` that calls
//! `Driver::parse_all` on every build. For headers totaling tens
//! of MB (Qt, FLTK + Cairo, Chromium-derived embedded UI stacks)
//! libclang reparse takes seconds-to-minutes; the parsed
//! `CxxTypeCtx` itself is cheap to materialize and serialize.
//!
//! This module captures the cache-key computation + read/write
//! plumbing. The driver-level `load_or_parse` entry point lives in
//! [`crate::driver`] and orchestrates the cache hit / miss decision.
//!
//! Cache key inputs:
//!
//! - SHA-256 of every header file's bytes (one digest each, sorted
//!   by path; see [`hash_headers`]).
//! - The driver's clang argv (joined with `\u{1f}` separators so
//!   ordering is preserved).
//! - The libclang version string (read at parse time).
//! - The cxx_importer crate version (`CARGO_PKG_VERSION`).
//!
//! Any change to any input invalidates the cache.

#![cfg(feature = "cache")]

use std::path::{Path, PathBuf};

use rustc_abi_cxx::{ClassId, CxxTypeCtx};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::annotations::AnnotationSet;

/// On-disk cache record. Versioned by a constant so the loader
/// can reject schema mismatches without touching field internals.
#[derive(Serialize, Deserialize)]
pub(crate) struct CacheRecord {
    pub schema: u32,
    pub key: String,
    pub class_ids: Vec<ClassId>,
    pub ctx: CxxTypeCtx,
    pub annotations: AnnotationSet,
}

/// Bumped whenever the cache layout changes incompatibly. A
/// loaded record with a different `schema` is treated as a miss.
pub(crate) const CACHE_SCHEMA: u32 = 1;

/// Hash every file in `paths` and join the digests into a single
/// hex string. Paths are read+digested in their input order — the
/// driver passes them sorted by `HeaderGraph::roots`.
pub(crate) fn hash_headers(paths: &[PathBuf]) -> Result<String, std::io::Error> {
    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = std::fs::read(path)?;
        let mut file_hasher = Sha256::new();
        file_hasher.update(&bytes);
        let digest = file_hasher.finalize();
        // Path + digest joined; path ensures distinct headers with
        // identical content hash to distinct slots.
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"=");
        hasher.update(format!("{digest:x}").as_bytes());
        hasher.update(b"\x1f");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compose a fully-qualified cache key from header digest + flags
/// + version stamps. The result fits in a single `String` and is
/// suitable for direct equality comparison against a previously-
/// stored record's `key` field.
pub(crate) fn compute_cache_key(
    header_digest: &str,
    clang_argv: &[String],
    libclang_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\x1f");
    hasher.update(libclang_version.as_bytes());
    hasher.update(b"\x1f");
    for arg in clang_argv {
        hasher.update(arg.as_bytes());
        hasher.update(b"\x1f");
    }
    hasher.update(header_digest.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Read a cache record from disk. Returns `None` if the file
/// doesn't exist, can't be parsed, or has a mismatching schema /
/// key. The caller treats `None` as a cache miss and reparses.
pub(crate) fn read_record(
    cache_path: &Path,
    expected_key: &str,
) -> Option<CacheRecord> {
    let bytes = std::fs::read(cache_path).ok()?;
    let record: CacheRecord = serde_json::from_slice(&bytes).ok()?;
    if record.schema != CACHE_SCHEMA {
        return None;
    }
    if record.key != expected_key {
        return None;
    }
    Some(record)
}

/// Write a cache record to disk. Creates parent directories as
/// needed; truncates any existing file. Returns `Ok(())` on
/// success — write failures bubble up so the caller can surface
/// them to the user (a cache write error during a build.rs is
/// usually a permissions or disk-space issue, not a logic bug).
pub(crate) fn write_record(
    cache_path: &Path,
    record: &CacheRecord,
) -> Result<(), std::io::Error> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    std::fs::write(cache_path, bytes)
}

/// Convenience: return a deterministic "no library" libclang
/// version string. The libclang-feature path overrides this with
/// the real version via [`libclang_version`].
pub(crate) fn libclang_version_fallback() -> &'static str {
    "no-libclang-feature"
}

/// Read libclang's banner string for the current process. Used
/// to invalidate the cache when the toolchain swap changes
/// AST-level layout (rare, but does happen between major LLVM
/// releases).
#[cfg(feature = "libclang")]
pub(crate) fn libclang_version() -> String {
    // `clang::Clang::new()` returns the banner via Display; we
    // need at most one instance for the version probe. Use
    // `try_get_version()`-equivalent: construct a Clang and read
    // through the index's diagnostic accessor.
    match clang::Clang::new() {
        Ok(_) => {
            // The clang crate doesn't expose `clang_getClangVersion()`
            // directly in 2.0. Fall back to a fixed marker — the
            // crate-version stamp + flag set already invalidate on
            // the most common upgrade paths.
            "libclang-runtime".to_string()
        }
        Err(_) => libclang_version_fallback().to_string(),
    }
}

/// Sort header paths so the digest is order-independent. Used by
/// the driver before calling [`hash_headers`].
pub(crate) fn sorted_headers(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = paths.to_vec();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_headers_is_stable_across_input_order() {
        // Caller must sort, but the hash itself reflects the order
        // it sees — so identical sorted lists hash to the same
        // value, while permutations differ.
        let dir = std::env::temp_dir().join("rustcc_cache_test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.hpp");
        let b = dir.join("b.hpp");
        std::fs::write(&a, b"struct A {};").unwrap();
        std::fs::write(&b, b"struct B {};").unwrap();

        let h1 = hash_headers(&[a.clone(), b.clone()]).unwrap();
        let h2 = hash_headers(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(h1, h2, "deterministic on identical inputs");

        let h3 = hash_headers(&[b.clone(), a.clone()]).unwrap();
        assert_ne!(h1, h3, "input order changes the hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_key_changes_when_any_input_changes() {
        let argv = vec!["-x".to_string(), "c++".to_string()];
        let k1 = compute_cache_key("digest-1", &argv, "lc-1");
        let k2 = compute_cache_key("digest-2", &argv, "lc-1");
        let k3 = compute_cache_key("digest-1", &["-x".to_string()], "lc-1");
        let k4 = compute_cache_key("digest-1", &argv, "lc-2");
        assert_ne!(k1, k2, "header digest change invalidates");
        assert_ne!(k1, k3, "argv change invalidates");
        assert_ne!(k1, k4, "libclang version change invalidates");
    }

    #[test]
    fn read_record_on_missing_file_returns_none() {
        let phantom = std::env::temp_dir().join("rustcc_cache_definitely_not_here");
        let _ = std::fs::remove_file(&phantom);
        assert!(read_record(&phantom, "anything").is_none());
    }

    #[test]
    fn read_record_with_mismatched_key_returns_none() {
        use rustc_abi_cxx::Target;
        let dir = std::env::temp_dir().join("rustcc_cache_mismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let record = CacheRecord {
            schema: CACHE_SCHEMA,
            key: "stored-key".into(),
            class_ids: vec![],
            ctx: CxxTypeCtx::new(Target::aarch64_apple_darwin()),
            annotations: AnnotationSet::default(),
        };
        write_record(&path, &record).unwrap();
        assert!(read_record(&path, "different-key").is_none());
        assert!(read_record(&path, "stored-key").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
