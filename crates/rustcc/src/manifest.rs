//! `[cpp-interop]` manifest section parsing.
//!
//! Implements `docs/build_integration.md §3`. A user's `Cargo.toml`
//! may contain:
//!
//! ```toml
//! [cpp-interop]
//! headers = ["cpp/include/widget.hpp"]
//! header-search-paths = ["cpp/include", "third-party/fmt/include"]
//! clang-flags = ["-std=c++20"]
//! stdlib = "libc++"
//! sidecar = "rustcc-api.yaml"
//! link-libraries = ["widget", "fmt"]
//! link-search-paths = ["target/cpp"]
//! ```
//!
//! The driver walks upward from the source file to find the manifest
//! (cargo doesn't tell us which manifest it used, and looking at the
//! positional source path is the reliable way), reads the TOML, and
//! surfaces a typed [`CppInteropConfig`]. Paths are resolved against
//! the manifest directory so downstream code (the `cxx_importer`
//! Driver, `clang++` invocations) can consume absolute paths.

use std::path::{Path, PathBuf};

use cxx_importer::HeaderGraph;
use serde::Deserialize;

/// Parsed `[cpp-interop]` section from a `Cargo.toml`, with all paths
/// resolved to absolute form against the manifest directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CppInteropConfig {
    pub headers: Vec<PathBuf>,
    pub header_search_paths: Vec<PathBuf>,
    pub clang_flags: Vec<String>,
    pub stdlib: Option<Stdlib>,
    pub sidecar: Option<PathBuf>,
    pub link_libraries: Vec<String>,
    pub link_search_paths: Vec<PathBuf>,
    /// Emit pre-fork stub bodies (`<crate>-cxx-stubs.cpp`). Off by
    /// default — with the rustc fork in place, real bodies come from
    /// the Rust side and duplicate symbols from the stub file would
    /// cause link errors. Users opt in when they want the link to
    /// resolve today via abort-ing placeholders.
    pub emit_stubs: bool,
    /// Emit Rust-side forwarder thunks (`<crate>-cxx-forwarders.rs`)
    /// that export Itanium-mangled symbols delegating to the user's
    /// Rust methods. Users `include!(env!("RUSTCC_FORWARDERS_PATH"))`
    /// under `#[cfg(rustcc_forwarders)]` in their crate root. The
    /// rustcc driver sets both envs when this flag is on. Mutually
    /// exclusive with `emit_stubs` — both produce bodies for the
    /// same mangled symbols.
    pub emit_forwarders: bool,
}

/// Which C++ standard library the user's C++ side is built against.
/// Selected by the `stdlib` key; enforced at link time because mixing
/// libc++ and libstdc++ in one binary is an ABI hazard (§7).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Stdlib {
    LibCxx,
    LibStdCxx,
}

#[derive(Debug)]
pub enum ManifestError {
    /// No `Cargo.toml` was reachable by walking upward from the start
    /// directory. Typical cause: source file lives outside any cargo
    /// project.
    NotFound { start: PathBuf },
    /// The manifest was found and read but contained no
    /// `[cpp-interop]` section. The caller likely detected interop via
    /// `--cfg cpp_interop` but the manifest disagrees.
    MissingSection { manifest: PathBuf },
    /// The `stdlib` key was present but its value wasn't one of the
    /// accepted literals (`"libc++"`, `"libstdcxx"`).
    InvalidStdlib { manifest: PathBuf, value: String },
    Io { path: PathBuf, error: std::io::Error },
    Parse { path: PathBuf, error: toml::de::Error },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { start } => write!(
                f,
                "no Cargo.toml found walking upward from {}",
                start.display()
            ),
            Self::MissingSection { manifest } => write!(
                f,
                "{} has no [cpp-interop] section",
                manifest.display()
            ),
            Self::InvalidStdlib { manifest, value } => write!(
                f,
                "{}: stdlib = {value:?} is not one of \"libc++\", \"libstdcxx\"",
                manifest.display()
            ),
            Self::Io { path, error } => {
                write!(f, "I/O error reading {}: {error}", path.display())
            }
            Self::Parse { path, error } => {
                write!(f, "parse error in {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Walk up from `start` (a file or directory) looking for `Cargo.toml`.
/// Returns the manifest path if found.
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = if start.is_file() {
        start.parent()
    } else {
        Some(start)
    };
    while let Some(dir) = cur {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// Read `manifest_path` and extract `[cpp-interop]`. Relative paths in
/// the section are resolved against the manifest's parent directory.
pub fn load_config(
    manifest_path: &Path,
) -> Result<CppInteropConfig, ManifestError> {
    let body = std::fs::read_to_string(manifest_path).map_err(|error| {
        ManifestError::Io {
            path: manifest_path.to_path_buf(),
            error,
        }
    })?;
    let parsed: RawManifest =
        toml::from_str(&body).map_err(|error| ManifestError::Parse {
            path: manifest_path.to_path_buf(),
            error,
        })?;
    // Prefer the cargo-blessed `[package.metadata.cpp-interop]`
    // location (no warning); fall back to the legacy top-level
    // `[cpp-interop]` for existing demos.
    let raw = parsed
        .package
        .and_then(|p| p.metadata)
        .and_then(|m| m.cpp_interop)
        .or(parsed.cpp_interop)
        .ok_or_else(|| ManifestError::MissingSection {
            manifest: manifest_path.to_path_buf(),
        })?;

    let manifest_dir = manifest_path
        .parent()
        .expect("manifest path has a parent by construction");

    let stdlib = match raw.stdlib.as_deref() {
        None => None,
        Some("libc++") => Some(Stdlib::LibCxx),
        Some("libstdcxx") => Some(Stdlib::LibStdCxx),
        Some(other) => {
            return Err(ManifestError::InvalidStdlib {
                manifest: manifest_path.to_path_buf(),
                value: other.to_string(),
            });
        }
    };

    Ok(CppInteropConfig {
        headers: resolve_paths(manifest_dir, &raw.headers),
        header_search_paths: resolve_paths(
            manifest_dir,
            &raw.header_search_paths,
        ),
        clang_flags: raw.clang_flags,
        stdlib,
        sidecar: raw.sidecar.map(|s| resolve_single(manifest_dir, &s)),
        link_libraries: raw.link_libraries,
        link_search_paths: resolve_paths(
            manifest_dir,
            &raw.link_search_paths,
        ),
        emit_stubs: raw.emit_stubs.unwrap_or(false),
        emit_forwarders: raw.emit_forwarders.unwrap_or(false),
    })
}

impl CppInteropConfig {
    /// Build the [`HeaderGraph`] that `cxx_importer::Driver` consumes.
    /// All paths are already absolute from [`load_config`], so the
    /// graph is immediately usable regardless of the driver's cwd.
    pub fn to_header_graph(&self) -> HeaderGraph {
        HeaderGraph {
            roots: self.headers.clone(),
            include_paths: self.header_search_paths.clone(),
            clang_flags: self.clang_flags.clone(),
        }
    }
}

fn resolve_paths(base: &Path, raw: &[String]) -> Vec<PathBuf> {
    raw.iter().map(|p| resolve_single(base, p)).collect()
}

fn resolve_single(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

// -------- Raw TOML shape (serde) ---------------------------------------

#[derive(Deserialize, Debug, Default)]
struct RawManifest {
    /// Top-level `[cpp-interop]` section. Works but cargo prints
    /// "unused manifest key: cpp-interop" because cargo doesn't
    /// recognize the key. Kept for back-compat with existing demos.
    #[serde(rename = "cpp-interop", default)]
    cpp_interop: Option<RawCppInterop>,
    /// Cargo-blessed location: `[package.metadata.cpp-interop]`.
    /// Cargo accepts anything under `package.metadata` without
    /// warnings, so new users should prefer this form.
    #[serde(default)]
    package: Option<RawPackage>,
}

#[derive(Deserialize, Debug, Default)]
struct RawPackage {
    #[serde(default)]
    metadata: Option<RawMetadata>,
}

#[derive(Deserialize, Debug, Default)]
struct RawMetadata {
    #[serde(rename = "cpp-interop", default)]
    cpp_interop: Option<RawCppInterop>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
struct RawCppInterop {
    #[serde(default)]
    headers: Vec<String>,
    #[serde(rename = "header-search-paths", default)]
    header_search_paths: Vec<String>,
    #[serde(rename = "clang-flags", default)]
    clang_flags: Vec<String>,
    #[serde(default)]
    stdlib: Option<String>,
    #[serde(default)]
    sidecar: Option<String>,
    #[serde(rename = "link-libraries", default)]
    link_libraries: Vec<String>,
    #[serde(rename = "link-search-paths", default)]
    link_search_paths: Vec<String>,
    #[serde(rename = "emit-stubs", default)]
    emit_stubs: Option<bool>,
    #[serde(rename = "emit-forwarders", default)]
    emit_forwarders: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("Cargo.toml");
        std::fs::write(&path, body).expect("write manifest");
        path
    }

    #[test]
    fn find_manifest_walks_up_from_source_file() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("crate/src");
        std::fs::create_dir_all(&nested).unwrap();
        let manifest = write_manifest(
            &tmp.path().join("crate"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        );
        let source = nested.join("lib.rs");
        std::fs::write(&source, "").unwrap();

        assert_eq!(
            find_manifest(&source).unwrap().canonicalize().unwrap(),
            manifest.canonicalize().unwrap()
        );
    }

    #[test]
    fn find_manifest_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(find_manifest(&nested).is_none());
    }

    #[test]
    fn load_config_parses_all_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[package]
name = \"demo\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"cpp/widget.hpp\"]
header-search-paths = [\"cpp/include\"]
clang-flags = [\"-std=c++20\"]
stdlib = \"libc++\"
sidecar = \"rustcc-api.yaml\"
link-libraries = [\"widget\"]
link-search-paths = [\"target/cpp\"]
",
        );

        let cfg = load_config(&manifest).expect("parse");
        assert_eq!(cfg.headers, vec![tmp.path().join("cpp/widget.hpp")]);
        assert_eq!(
            cfg.header_search_paths,
            vec![tmp.path().join("cpp/include")]
        );
        assert_eq!(cfg.clang_flags, vec!["-std=c++20"]);
        assert_eq!(cfg.stdlib, Some(Stdlib::LibCxx));
        assert_eq!(cfg.sidecar, Some(tmp.path().join("rustcc-api.yaml")));
        assert_eq!(cfg.link_libraries, vec!["widget"]);
        assert_eq!(cfg.link_search_paths, vec![tmp.path().join("target/cpp")]);
    }

    #[test]
    fn load_config_handles_omitted_keys_as_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[package]
name = \"demo\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"x.hpp\"]
",
        );
        let cfg = load_config(&manifest).unwrap();
        assert_eq!(cfg.headers, vec![tmp.path().join("x.hpp")]);
        assert!(cfg.header_search_paths.is_empty());
        assert!(cfg.clang_flags.is_empty());
        assert_eq!(cfg.stdlib, None);
        assert_eq!(cfg.sidecar, None);
        assert!(cfg.link_libraries.is_empty());
        assert!(cfg.link_search_paths.is_empty());
    }

    #[test]
    fn missing_section_returns_specific_error() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        );
        let err = load_config(&manifest).unwrap_err();
        assert!(matches!(err, ManifestError::MissingSection { .. }));
    }

    #[test]
    fn invalid_stdlib_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[cpp-interop]
stdlib = \"msvc\"
",
        );
        let err = load_config(&manifest).unwrap_err();
        match err {
            ManifestError::InvalidStdlib { value, .. } => {
                assert_eq!(value, "msvc");
            }
            other => panic!("expected InvalidStdlib, got {other:?}"),
        }
    }

    #[test]
    fn unknown_key_in_section_is_rejected_early() {
        // `deny_unknown_fields` catches typos before they silently
        // become no-ops.
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[cpp-interop]
headerz = [\"x\"]
",
        );
        let err = load_config(&manifest).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    #[test]
    fn libstdcxx_alias_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[cpp-interop]
stdlib = \"libstdcxx\"
",
        );
        let cfg = load_config(&manifest).unwrap();
        assert_eq!(cfg.stdlib, Some(Stdlib::LibStdCxx));
    }

    #[test]
    fn package_metadata_form_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[package]
name = \"demo\"
version = \"0.1.0\"

[package.metadata.cpp-interop]
headers = [\"h.hpp\"]
",
        );
        let cfg = load_config(&manifest).unwrap();
        assert_eq!(cfg.headers, vec![tmp.path().join("h.hpp")]);
    }

    #[test]
    fn top_level_form_wins_when_both_present() {
        // If a user accidentally has both (e.g., migrated a demo),
        // `[package.metadata.cpp-interop]` takes precedence so the
        // cargo-friendly form is the definitive one.
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            "\
[package]
name = \"demo\"
version = \"0.1.0\"

[package.metadata.cpp-interop]
headers = [\"new.hpp\"]

[cpp-interop]
headers = [\"old.hpp\"]
",
        );
        let cfg = load_config(&manifest).unwrap();
        assert_eq!(cfg.headers, vec![tmp.path().join("new.hpp")]);
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("absolute.hpp");
        let manifest = write_manifest(
            tmp.path(),
            &format!(
                "[cpp-interop]\nheaders = [{:?}]\n",
                abs.display().to_string(),
            ),
        );
        let cfg = load_config(&manifest).unwrap();
        assert_eq!(cfg.headers, vec![abs]);
    }

    #[test]
    fn to_header_graph_mirrors_fields() {
        let cfg = CppInteropConfig {
            headers: vec![PathBuf::from("/a/h.hpp")],
            header_search_paths: vec![PathBuf::from("/a/inc")],
            clang_flags: vec!["-std=c++17".into()],
            stdlib: Some(Stdlib::LibCxx),
            sidecar: None,
            link_libraries: vec!["x".into()],
            link_search_paths: vec![PathBuf::from("/a/lib")],
            emit_stubs: false,
            emit_forwarders: false,
        };
        let g = cfg.to_header_graph();
        assert_eq!(g.roots, cfg.headers);
        assert_eq!(g.include_paths, cfg.header_search_paths);
        assert_eq!(g.clang_flags, cfg.clang_flags);
    }
}
