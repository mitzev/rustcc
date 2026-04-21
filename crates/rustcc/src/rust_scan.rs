//! Rust-side `#[repr(cpp)]` discovery phase.
//!
//! Scans the crate's `src/` tree via `rustcc_attr`, lowers the parsed
//! items into `rustc_abi_cxx::CxxTypeCtx` as `TypeOrigin::RustReprCpp`
//! classes, and returns both the populated context and the source-file
//! list (so the fingerprint can include Rust bodies and flip when they
//! change).
//!
//! Always available — no libclang dependency. This is the pre-fork
//! stand-in for what will later be a rustc `HIR -> TyCtxt` query in
//! the rustc fork; see `docs/repr_cpp.md §7`.

use std::path::{Path, PathBuf};

use rustc_abi_cxx::{ClassId, CxxTypeCtx, Target};
use rustcc_attr::{lower_into_ctx, scan_crate_sources};

pub struct RustScanResult {
    /// The context, populated with any `#[repr(cpp)]` structs found.
    pub ctx: CxxTypeCtx,
    /// Every `#[repr(cpp)]` class id minted, in source order. Equal to
    /// `ctx.rust_classes().collect()` but exposed directly for
    /// convenience.
    pub rust_classes: Vec<ClassId>,
    /// Every `.rs` file visited, absolute paths, sorted. Used by the
    /// fingerprint so an edit to a Rust source that changes the IR
    /// invalidates the cache.
    pub sources: Vec<PathBuf>,
}

impl std::fmt::Debug for RustScanResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `CxxTypeCtx` doesn't derive Debug to keep the IR crate light;
        // we project the information drivers actually want.
        f.debug_struct("RustScanResult")
            .field("rust_classes", &self.rust_classes)
            .field("sources", &self.sources)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RustScanError {
    /// A file read or parse error.
    Attr(String),
}

impl std::fmt::Display for RustScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Attr(m) => write!(f, "rust scan: {m}"),
        }
    }
}

impl std::error::Error for RustScanError {}

/// Scan `crate_root/src/` for `#[repr(cpp)]` items, lower them into a
/// fresh `CxxTypeCtx`, and return the result. `crate_root` is the
/// directory containing `Cargo.toml`.
pub fn scan(crate_root: &Path) -> Result<RustScanResult, RustScanError> {
    let (module, sources) = scan_crate_sources(crate_root)
        .map_err(|e| RustScanError::Attr(format!("{e:?}")))?;
    let mut ctx = CxxTypeCtx::new(Target::host());
    lower_into_ctx(&module, &mut ctx)
        .map_err(|e| RustScanError::Attr(format!("{e:?}")))?;
    let rust_classes: Vec<ClassId> = ctx.rust_classes().collect();
    Ok(RustScanResult { ctx, rust_classes, sources })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn scan_empty_crate_produces_empty_ctx() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        write(&tmp.path().join("src/lib.rs"), "pub fn plain() {}\n");
        let r = scan(tmp.path()).unwrap();
        assert!(r.rust_classes.is_empty());
        assert_eq!(r.sources.len(), 1);
    }

    #[test]
    fn scan_discovers_repr_cpp_structs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        write(
            &tmp.path().join("src/lib.rs"),
            r#"
                pub mod point;
                #[repr(cpp)] pub struct Counter { pub n: i64 }
            "#,
        );
        std::fs::create_dir_all(tmp.path().join("src/point")).ok();
        write(
            &tmp.path().join("src/point.rs"),
            r#"
                #[repr(cpp)] pub struct Point { pub x: i32, pub y: i32 }
                impl Point {
                    pub fn new(x: i32, y: i32) -> Self { Point { x, y } }
                }
            "#,
        );
        let r = scan(tmp.path()).unwrap();
        assert_eq!(r.rust_classes.len(), 2);
        // Both .rs files scanned.
        assert_eq!(r.sources.len(), 2);
    }

    #[test]
    fn scan_on_crate_without_src_dir_is_empty_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let r = scan(tmp.path()).unwrap();
        assert!(r.rust_classes.is_empty());
        assert!(r.sources.is_empty());
    }

    #[test]
    fn scan_surface_parse_error_in_attr_parse() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        write(
            &tmp.path().join("src/bad.rs"),
            "#[repr(cpp, C)] pub struct Bad { pub x: i32 }\n",
        );
        let err = scan(tmp.path()).unwrap_err();
        match err {
            RustScanError::Attr(msg) => {
                assert!(msg.contains("BadRepr"), "unexpected: {msg}");
            }
        }
    }
}
