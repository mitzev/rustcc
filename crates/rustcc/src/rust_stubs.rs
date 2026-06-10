//! Driver-level wrapper around `cxx_importer::rust_stubs`.
//!
//! Writes the generated `.cpp` stub source to `<crate>-cxx-stubs.cpp`
//! in the cache dir, skipping when inputs are unchanged and the file
//! already exists. Opt-in via the manifest: `emit_stubs = true` under
//! `[cpp-interop]` (off by default so production users with real Rust
//! bodies from the fork don't get duplicate-symbol conflicts).

use std::path::{Path, PathBuf};

use cxx_importer::rust_stubs::generate_rust_stub_shims;
use rustc_abi_cxx::CxxTypeCtx;

#[derive(Debug)]
pub struct RustStubsArtifact {
    pub path: PathBuf,
    pub emitted: bool,
    pub class_count: usize,
}

#[derive(Debug)]
pub enum RustStubsError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Generator(String),
}

impl std::fmt::Display for RustStubsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Generator(m) => write!(f, "stubs generator failed: {m}"),
        }
    }
}

impl std::error::Error for RustStubsError {}

pub fn emit(
    ctx: &CxxTypeCtx,
    cache_dir: &Path,
    crate_name: &str,
    inputs_changed: bool,
) -> Result<RustStubsArtifact, RustStubsError> {
    let path = cache_dir.join(format!("{crate_name}-cxx-stubs.cpp"));
    let class_count = ctx.rust_classes().count();

    if class_count == 0 {
        return Ok(RustStubsArtifact {
            path,
            emitted: false,
            class_count,
        });
    }
    if !inputs_changed && path.is_file() {
        return Ok(RustStubsArtifact {
            path,
            emitted: false,
            class_count,
        });
    }

    std::fs::create_dir_all(cache_dir).map_err(|error| RustStubsError::Io {
        path: cache_dir.to_path_buf(),
        error,
    })?;

    let header_include = format!("{crate_name}-cxx.hpp");
    let body = generate_rust_stub_shims(ctx, &header_include)
        .map_err(|e| RustStubsError::Generator(format!("{e:?}")))?;

    std::fs::write(&path, &body).map_err(|error| RustStubsError::Io {
        path: path.clone(),
        error,
    })?;

    Ok(RustStubsArtifact {
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
            name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
            bases: vec![],
            fields: vec![FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            }],
            methods: vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Point".into())),
                sig: FnSig {
                    params: vec![],
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
    fn emits_file_on_first_run_and_caches_after() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx();
        let first = emit(&ctx, dir.path(), "c", true).unwrap();
        assert!(first.emitted);
        assert_eq!(
            first.path.file_name().and_then(|s| s.to_str()),
            Some("c-cxx-stubs.cpp"),
        );
        let body = std::fs::read_to_string(&first.path).unwrap();
        assert!(body.contains("#include \"c-cxx.hpp\""));
        assert!(body.contains("Point::~Point"));

        let second = emit(&ctx, dir.path(), "c", false).unwrap();
        assert!(!second.emitted);
    }

    #[test]
    fn skip_on_empty_ctx() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let art = emit(&ctx, dir.path(), "c", true).unwrap();
        assert!(!art.emitted);
        assert_eq!(art.class_count, 0);
        assert!(!art.path.is_file());
    }
}
