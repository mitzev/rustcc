//! Non-libclang hpp emission path for Rust-side `#[repr(cpp)]` types.
//!
//! The existing `rustcc::hpp` module is libclang-gated because the M5
//! pipeline was specced before the Rust-scan existed — it takes a
//! `ParsedIr` that only exists when libclang is compiled in. This
//! module fills the gap: emit `.hpp` from a `CxxTypeCtx` populated
//! purely by the Rust scan, with no libclang dependency.
//!
//! The on-disk layout mirrors `rustcc::hpp::emit`: one
//! `<crate>-cxx.hpp` per crate, written to the cache dir, skipped when
//! `inputs_changed` is false and the file already exists.

use std::path::{Path, PathBuf};

use cxx_importer::hpp::generate_hpp_for_rust_types;
use rustc_abi_cxx::CxxTypeCtx;

#[derive(Debug)]
pub struct RustHppArtifact {
    pub path: PathBuf,
    pub emitted: bool,
    pub class_count: usize,
}

#[derive(Debug)]
pub enum RustHppError {
    Io { path: PathBuf, error: std::io::Error },
    Generator(String),
}

impl std::fmt::Display for RustHppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Generator(m) => write!(f, "hpp generator failed: {m}"),
        }
    }
}

impl std::error::Error for RustHppError {}

pub fn emit(
    ctx: &CxxTypeCtx,
    cache_dir: &Path,
    crate_name: &str,
    inputs_changed: bool,
) -> Result<RustHppArtifact, RustHppError> {
    let path = cache_dir.join(format!("{crate_name}-cxx.hpp"));
    let class_count = ctx.rust_classes().count();

    if class_count == 0 {
        // Nothing to declare. If a stale file exists from a prior run,
        // leave it alone — the user may be intentionally shrinking
        // their crate, and removing the file during an incremental
        // build could break downstream IDE tooling that expects a
        // stable artifact path. A future cleanup pass can handle it.
        return Ok(RustHppArtifact { path, emitted: false, class_count });
    }

    if !inputs_changed && path.is_file() {
        return Ok(RustHppArtifact { path, emitted: false, class_count });
    }

    std::fs::create_dir_all(cache_dir).map_err(|error| RustHppError::Io {
        path: cache_dir.to_path_buf(),
        error,
    })?;

    let body = generate_hpp_for_rust_types(ctx)
        .map_err(|e| RustHppError::Generator(format!("{e:?}")))?;

    std::fs::write(&path, &body).map_err(|error| RustHppError::Io {
        path: path.clone(),
        error,
    })?;

    Ok(RustHppArtifact { path, emitted: true, class_count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, FieldDef, Ident, IntWidth, CxxType, NameSegment, NestedName,
        RecordKind, Target,
    };

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_ctx() -> CxxTypeCtx {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let _ = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
            bases: vec![],
            fields: vec![
                FieldDef {
                    name: Ident("x".into()),
                    ty: i32_,
                    explicit_align: None,
                },
                FieldDef {
                    name: Ident("y".into()),
                    ty: i32_,
                    explicit_align: None,
                },
            ],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        ctx
    }

    #[test]
    fn emit_writes_hpp_when_classes_present() {
        let dir = tmpdir();
        let ctx = sample_ctx();
        let art = emit(&ctx, dir.path(), "test_crate", true).unwrap();
        assert!(art.emitted);
        assert_eq!(art.class_count, 1);
        assert_eq!(
            art.path.file_name().and_then(|s| s.to_str()),
            Some("test_crate-cxx.hpp")
        );
        let body = std::fs::read_to_string(&art.path).unwrap();
        assert!(body.contains("class Point {"), "body:\n{body}");
    }

    #[test]
    fn emit_skips_when_inputs_unchanged_and_file_exists() {
        let dir = tmpdir();
        let ctx = sample_ctx();
        let first = emit(&ctx, dir.path(), "test_crate", true).unwrap();
        assert!(first.emitted);
        let second = emit(&ctx, dir.path(), "test_crate", false).unwrap();
        assert!(!second.emitted);
    }

    #[test]
    fn emit_rewrites_when_file_missing_despite_cache_claim() {
        let dir = tmpdir();
        let ctx = sample_ctx();
        let first = emit(&ctx, dir.path(), "test_crate", true).unwrap();
        std::fs::remove_file(&first.path).unwrap();
        let second = emit(&ctx, dir.path(), "test_crate", false).unwrap();
        assert!(second.emitted, "should regenerate missing file");
    }

    #[test]
    fn emit_skips_silently_when_no_rust_classes() {
        let dir = tmpdir();
        let ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let art = emit(&ctx, dir.path(), "empty", true).unwrap();
        assert!(!art.emitted);
        assert_eq!(art.class_count, 0);
        assert!(!art.path.is_file());
    }
}
