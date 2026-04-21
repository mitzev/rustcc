//! Driver-level wrapper around `cxx_importer::rust_forwarders`.
//!
//! Writes the generated forwarders to `<cache>/<crate>-cxx-forwarders.rs`
//! and returns the path so `main.rs` can export it to rustc via the
//! `RUSTCC_FORWARDERS_PATH` env var. Users `include!` the file from
//! their crate root under `#[cfg(rustcc_forwarders)]`; the driver
//! then passes `--cfg rustcc_forwarders` to rustc.
//!
//! Opt-in via `emit-forwarders = true` in the manifest. Mutually
//! exclusive with `emit-stubs`: both emit bodies for the same
//! mangled symbols, and combining them would produce duplicate-symbol
//! link errors.

use std::path::{Path, PathBuf};

use cxx_importer::rust_forwarders::{
    default_rust_name, generate_rust_forwarders,
};
use rustc_abi_cxx::CxxTypeCtx;

#[derive(Debug)]
pub struct RustForwardersArtifact {
    pub path: PathBuf,
    pub emitted: bool,
    pub class_count: usize,
}

#[derive(Debug)]
pub enum RustForwardersError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Generator(String),
}

impl std::fmt::Display for RustForwardersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Generator(m) => write!(f, "forwarder generator: {m}"),
        }
    }
}

impl std::error::Error for RustForwardersError {}

pub fn emit(
    ctx: &CxxTypeCtx,
    cache_dir: &Path,
    crate_name: &str,
    inputs_changed: bool,
) -> Result<RustForwardersArtifact, RustForwardersError> {
    let path = cache_dir.join(format!("{crate_name}-cxx-forwarders.rs"));
    let class_count = ctx.rust_classes().count();

    if class_count == 0 {
        return Ok(RustForwardersArtifact {
            path,
            emitted: false,
            class_count,
        });
    }
    if !inputs_changed && path.is_file() {
        return Ok(RustForwardersArtifact {
            path,
            emitted: false,
            class_count,
        });
    }

    std::fs::create_dir_all(cache_dir).map_err(|error| {
        RustForwardersError::Io {
            path: cache_dir.to_path_buf(),
            error,
        }
    })?;

    let body = generate_rust_forwarders(ctx, default_rust_name(ctx))
        .map_err(|e| RustForwardersError::Generator(format!("{e:?}")))?;

    std::fs::write(&path, &body).map_err(|error| RustForwardersError::Io {
        path: path.clone(),
        error,
    })?;

    Ok(RustForwardersArtifact {
        path,
        emitted: true,
        class_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, CvQual, CxxType, FieldDef, FnSig, Ident, IntWidth, MethodDef,
        MethodName, NameSegment, NestedName, RecordKind, SpecialMember, Target,
        Virtuality,
    };

    fn sample_ctx() -> CxxTypeCtx {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let void_ = ctx.intern_type(CxxType::Void);
        let _ = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
            bases: vec![],
            fields: vec![FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            }],
            methods: vec![MethodDef {
                name: MethodName::Ident(Ident("new".into())),
                sig: FnSig {
                    params: vec![i32_],
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::DefaultCtor),
            }],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        ctx
    }

    #[test]
    fn emits_forwarders_file_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx();
        let art = emit(&ctx, dir.path(), "c", true).unwrap();
        assert!(art.emitted);
        assert_eq!(
            art.path.file_name().and_then(|s| s.to_str()),
            Some("c-cxx-forwarders.rs"),
        );
        let body = std::fs::read_to_string(&art.path).unwrap();
        assert!(
            body.contains("__rustcc_fwd_Widget_dtor"),
            "body:\n{body}"
        );
    }

    #[test]
    fn skip_on_second_call_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx();
        let first = emit(&ctx, dir.path(), "c", true).unwrap();
        assert!(first.emitted);
        let second = emit(&ctx, dir.path(), "c", false).unwrap();
        assert!(!second.emitted);
    }

    #[test]
    fn skip_when_no_rust_classes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let art = emit(&ctx, dir.path(), "empty", true).unwrap();
        assert!(!art.emitted);
        assert_eq!(art.class_count, 0);
        assert!(!art.path.is_file());
    }
}
