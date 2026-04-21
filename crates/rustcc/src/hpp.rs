//! M5: Generated `.hpp` emission.
//!
//! Implements the "emit `<crate>-cxx.hpp`" step of
//! `docs/build_integration.md §4`. The file is the Rust → C++
//! direction of the exchange: C++ callers `#include` it to see
//! opaque-storage declarations for the types Rust is exporting.
//!
//! **Scope note.** Today the driver only has imported C++ types in its
//! IR (the Rust-side `#[repr(cpp)]` discovery lives in the future
//! rustc fork per `docs/repr_cpp.md §7`). So the file rustcc emits in
//! this slice re-exposes the *imported* classes in opaque form, which
//! is primarily useful as a sanity check that the importer pinned down
//! names, namespaces, and layouts correctly. When the Rust-side
//! pipeline lands, the same plumbing will emit the types the user
//! actually wrote with `#[repr(cpp)]` — no driver changes needed.

#![cfg(feature = "libclang")]

use std::path::{Path, PathBuf};

use cxx_importer::Driver;

use crate::manifest::CppInteropConfig;
use crate::parse::ParsedIr;

#[derive(Debug)]
pub struct HppArtifact {
    /// On-disk location of the emitted `.hpp`.
    pub path: PathBuf,
    /// True when the generator ran and wrote this run. False when the
    /// fingerprint matched and the cached file was already present.
    pub emitted: bool,
}

#[derive(Debug)]
pub enum HppError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    /// `cxx_importer::hpp::generate_hpp` failed (typically an
    /// unsupported type in a method signature or layout).
    Generator(String),
}

impl std::fmt::Display for HppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Generator(m) => write!(f, "hpp generator failed: {m}"),
        }
    }
}

impl std::error::Error for HppError {}

/// Emit the `<crate>-cxx.hpp` file. Skip generation when
/// `inputs_changed == false` and the file is already on disk.
pub fn emit(
    config: &CppInteropConfig,
    ir: &ParsedIr,
    cache_dir: &Path,
    crate_name: &str,
    inputs_changed: bool,
) -> Result<HppArtifact, HppError> {
    let path = cache_dir.join(format!("{crate_name}-cxx.hpp"));
    if !inputs_changed && path.is_file() {
        return Ok(HppArtifact {
            path,
            emitted: false,
        });
    }

    std::fs::create_dir_all(cache_dir).map_err(|error| HppError::Io {
        path: cache_dir.to_path_buf(),
        error,
    })?;

    // Rebuild the Driver so `emit_hpp` pulls headers from the same
    // graph `parse_all` used. We don't actually need the headers for
    // hpp output today (the emitter only needs layout), but keeping
    // the driver construction identical across phases pays off later
    // when M6 wires link flags derived from the graph.
    let graph = config.to_header_graph();
    let driver = Driver::new(graph);
    let body = driver
        .emit_hpp(&ir.ctx, &ir.classes)
        .map_err(|e| HppError::Generator(format!("{e:?}")))?;

    std::fs::write(&path, &body).map_err(|error| HppError::Io {
        path: path.clone(),
        error,
    })?;

    Ok(HppArtifact {
        path,
        emitted: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CppInteropConfig;
    use crate::parse::parse_headers;

    fn libclang_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::LIBCLANG.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cfg_with_header(path: PathBuf) -> CppInteropConfig {
        CppInteropConfig {
            headers: vec![path],
            header_search_paths: Vec::new(),
            clang_flags: vec!["-std=c++17".into()],
            stdlib: None,
            sidecar: None,
            link_libraries: Vec::new(),
            link_search_paths: Vec::new(),
            emit_stubs: false,
            emit_forwarders: false,
        }
    }

    #[test]
    fn emit_writes_hpp_with_expected_content() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(
            &header,
            "struct Widget { int x; int y; };\n",
        )
        .unwrap();

        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).unwrap();
        let cache = tmp.path().join("cache");

        let art = emit(&cfg, &ir, &cache, "mycrate", true).unwrap();
        assert!(art.emitted);
        assert_eq!(
            art.path.file_name().and_then(|s| s.to_str()),
            Some("mycrate-cxx.hpp"),
        );

        let body = std::fs::read_to_string(&art.path).unwrap();
        assert!(body.contains("#pragma once"), "missing pragma once\n{body}");
        assert!(body.contains("class Widget {"), "missing class decl\n{body}");
        // 2 × i32 → 8 bytes, align 4.
        assert!(
            body.contains("alignas(4) unsigned char __rust_storage[8];"),
            "expected opaque storage sized from layout\n{body}"
        );
    }

    #[test]
    fn emit_skips_when_inputs_unchanged_and_file_exists() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(&header, "struct Widget { int x; };\n").unwrap();
        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).unwrap();
        let cache = tmp.path().join("cache");

        let first = emit(&cfg, &ir, &cache, "mycrate", true).unwrap();
        assert!(first.emitted);
        let mtime_before =
            std::fs::metadata(&first.path).unwrap().modified().unwrap();

        let second = emit(&cfg, &ir, &cache, "mycrate", false).unwrap();
        assert!(!second.emitted, "should have skipped work");
        let mtime_after =
            std::fs::metadata(&second.path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "file shouldn't be rewritten");
    }

    #[test]
    fn emit_rewrites_when_file_missing_even_with_cache_claim() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(&header, "struct Widget { int x; };\n").unwrap();
        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).unwrap();
        let cache = tmp.path().join("cache");

        let first = emit(&cfg, &ir, &cache, "mycrate", true).unwrap();
        std::fs::remove_file(&first.path).unwrap();

        // Prior call's inputs haven't changed from the fingerprint's
        // view, but the file is gone — regenerate.
        let second = emit(&cfg, &ir, &cache, "mycrate", false).unwrap();
        assert!(second.emitted, "should regenerate missing file");
        assert!(second.path.is_file());
    }
}
