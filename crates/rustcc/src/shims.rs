//! M4: Shim `.cpp` emission and Clang compilation.
//!
//! Implements `docs/build_integration.md §4` steps "emit foo.shims.cpp"
//! and "invoke clang foo.shims.cpp -o foo.shims.o". Consumes the
//! [`ParsedIr`] produced by `M3 (parse)`, runs `cxx_importer::Driver::
//! emit_shims`, writes the generated `.cpp` to the cache directory,
//! and invokes `clang++ -c` to produce the matching `.o`.
//!
//! The `.o` path is what the eventual M6 link step (deferred) will
//! add to rustc's `--link-arg`s; today the object sits idle in the
//! cache dir for inspection.
//!
//! This module is feature-gated on `libclang` because the input IR
//! type is — without it, there's nothing to emit.

#![cfg(feature = "libclang")]

use std::path::{Path, PathBuf};
use std::process::Command;

use cxx_importer::Driver;

use crate::manifest::{CppInteropConfig, Stdlib};
use crate::parse::ParsedIr;

#[derive(Debug)]
pub struct ShimArtifacts {
    /// The generated shim source file.
    pub cpp_path: PathBuf,
    /// The compiled object produced by `clang++ -c`.
    pub obj_path: PathBuf,
    /// True when we ran the generator and invoked clang. False when
    /// fingerprint-check showed inputs unchanged and both artifacts
    /// already existed, so we left the cached outputs in place.
    pub compiled: bool,
}

#[derive(Debug)]
pub enum ShimError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The `cxx_importer::shims::generate_shims` call failed — usually
    /// an unsupported type or method-name kind (Display form preserved).
    Generator(String),
    /// Failed to spawn `clang++` (binary missing, permissions, etc.).
    ClangSpawn {
        clang: PathBuf,
        error: std::io::Error,
    },
    /// `clang++` ran but exited non-zero; we surface its stderr so
    /// users can debug bad shim source or flag mismatches.
    Clang {
        clang: PathBuf,
        status: std::process::ExitStatus,
        stderr: String,
        cpp_path: PathBuf,
    },
}

impl std::fmt::Display for ShimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Generator(m) => {
                write!(f, "shim generator failed: {m}")
            }
            Self::ClangSpawn { clang, error } => write!(
                f,
                "failed to spawn {}: {error}",
                clang.display()
            ),
            Self::Clang {
                clang,
                status,
                stderr,
                cpp_path,
            } => write!(
                f,
                "{} {status:?} while compiling {}:\n{stderr}",
                clang.display(),
                cpp_path.display(),
            ),
        }
    }
}

impl std::error::Error for ShimError {}

/// Resolve the C++ compiler to invoke. Honors `RUSTCC_CLANG` per
/// `docs/build_integration.md §5`, otherwise falls through to
/// `clang++` on `PATH`.
pub fn resolve_clang() -> PathBuf {
    if let Some(explicit) = std::env::var_os("RUSTCC_CLANG") {
        return PathBuf::from(explicit);
    }
    PathBuf::from("clang++")
}

/// Emit the shim source and compile it. If `inputs_changed == false`
/// and both artifacts already exist on disk, skip the generator and
/// the clang invocation; return the existing paths with
/// `compiled == false`.
pub fn emit_and_compile(
    config: &CppInteropConfig,
    ir: &ParsedIr,
    cache_dir: &Path,
    crate_name: &str,
    inputs_changed: bool,
) -> Result<ShimArtifacts, ShimError> {
    let cpp_path = cache_dir.join(format!("{crate_name}.shims.cpp"));
    let obj_path = cache_dir.join(format!("{crate_name}.shims.o"));

    if !inputs_changed && cpp_path.is_file() && obj_path.is_file() {
        return Ok(ShimArtifacts {
            cpp_path,
            obj_path,
            compiled: false,
        });
    }

    std::fs::create_dir_all(cache_dir).map_err(|error| ShimError::Io {
        path: cache_dir.to_path_buf(),
        error,
    })?;

    // Rebuild the Driver from config so `emit_shims` sees the header
    // paths it needs to `#include`.
    let graph = config.to_header_graph();
    let driver = Driver::new(graph);
    let cpp_body = driver
        .emit_shims(&ir.ctx, &ir.classes)
        .map_err(|e| ShimError::Generator(format!("{e:?}")))?;

    std::fs::write(&cpp_path, &cpp_body).map_err(|error| ShimError::Io {
        path: cpp_path.clone(),
        error,
    })?;

    let clang = resolve_clang();
    let mut cmd = Command::new(&clang);
    cmd.arg("-c").arg("-o").arg(&obj_path).arg(&cpp_path);
    for inc in &config.header_search_paths {
        cmd.arg(format!("-I{}", inc.display()));
    }
    // Stdlib selection precedes user clang_flags so the user can override.
    match config.stdlib {
        Some(Stdlib::LibCxx) => {
            cmd.arg("-stdlib=libc++");
        }
        Some(Stdlib::LibStdCxx) => {
            cmd.arg("-stdlib=libstdc++");
        }
        None => {}
    }
    for flag in &config.clang_flags {
        cmd.arg(flag);
    }

    let output = cmd.output().map_err(|error| ShimError::ClangSpawn {
        clang: clang.clone(),
        error,
    })?;
    if !output.status.success() {
        return Err(ShimError::Clang {
            clang,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            cpp_path: cpp_path.clone(),
        });
    }

    Ok(ShimArtifacts {
        cpp_path,
        obj_path,
        compiled: true,
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
    fn emit_and_compile_produces_obj_file_on_first_run() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(
            &header,
            "struct Widget { int compute(int x) const; };\n",
        )
        .unwrap();

        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).expect("parse");
        let cache = tmp.path().join("cache");

        let art =
            emit_and_compile(&cfg, &ir, &cache, "mycrate", true).unwrap();
        assert!(art.compiled, "first run should invoke clang");
        assert!(art.cpp_path.is_file(), "cpp written");
        assert!(art.obj_path.is_file(), "obj compiled");

        let body = std::fs::read_to_string(&art.cpp_path).unwrap();
        assert!(
            body.contains("__rustcc_shim__ZNK6Widget7computeEi"),
            "shim source missing expected symbol\n{body}"
        );
    }

    #[test]
    fn emit_and_compile_skips_work_when_inputs_unchanged() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(
            &header,
            "struct Widget { void tick(); };\n",
        )
        .unwrap();
        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).expect("parse");
        let cache = tmp.path().join("cache");

        let first =
            emit_and_compile(&cfg, &ir, &cache, "mycrate", true).unwrap();
        assert!(first.compiled);
        let cpp_mtime = std::fs::metadata(&first.cpp_path).unwrap().modified().unwrap();

        // Second call with inputs_changed=false and both files present
        // should short-circuit.
        let second =
            emit_and_compile(&cfg, &ir, &cache, "mycrate", false).unwrap();
        assert!(!second.compiled, "should have skipped work");
        let cpp_mtime_after =
            std::fs::metadata(&second.cpp_path).unwrap().modified().unwrap();
        assert_eq!(cpp_mtime, cpp_mtime_after, "cpp file shouldn't be rewritten");
    }

    #[test]
    fn emit_and_compile_recompiles_when_obj_missing() {
        let _g = libclang_lock();
        let tmp = tempfile::tempdir().unwrap();
        let header = tmp.path().join("widget.hpp");
        std::fs::write(
            &header,
            "struct Widget { void tick(); };\n",
        )
        .unwrap();
        let cfg = cfg_with_header(header.canonicalize().unwrap());
        let ir = parse_headers(&cfg).expect("parse");
        let cache = tmp.path().join("cache");

        let first =
            emit_and_compile(&cfg, &ir, &cache, "mycrate", true).unwrap();
        assert!(first.compiled);

        // User deletes the .o. Next call with inputs_changed=false
        // should still recompile because the cached output is missing.
        std::fs::remove_file(&first.obj_path).unwrap();
        let second =
            emit_and_compile(&cfg, &ir, &cache, "mycrate", false).unwrap();
        assert!(second.compiled, "should re-emit when .o is missing");
        assert!(second.obj_path.is_file());
    }

    #[test]
    fn resolve_clang_honors_env_var() {
        let prev = std::env::var_os("RUSTCC_CLANG");
        unsafe { std::env::set_var("RUSTCC_CLANG", "/tmp/fake-clang") };
        assert_eq!(resolve_clang(), PathBuf::from("/tmp/fake-clang"));
        match prev {
            Some(v) => unsafe { std::env::set_var("RUSTCC_CLANG", v) },
            None => unsafe { std::env::remove_var("RUSTCC_CLANG") },
        }
    }
}
