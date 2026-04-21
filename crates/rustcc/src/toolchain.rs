//! M7: toolchain detection.
//!
//! Probes the `rustc` and `clang++` versions once per pipeline run
//! and caches the result in `<cache>/rustcc-toolchain.json`. The
//! version strings flow into the fingerprint (see
//! [`crate::fingerprint_with_toolchain`]) so a toolchain upgrade
//! busts any artifact that was produced with a different version —
//! which is the only way to be sure the generated shims/hpp/stubs
//! agree with what the compiler is now producing.
//!
//! Environment overrides:
//!
//! - `RUSTCC_CLANG` — clang++ executable path (defaults to `clang++`
//!   on `$PATH`).
//! - `RUSTC` — rustc executable path (cargo sets this).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    pub rustc: ToolVersion,
    pub clang: ToolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVersion {
    /// Executable path as resolved at probe time.
    pub path: PathBuf,
    /// First line of `--version` output, trimmed.
    pub version: String,
}

#[derive(Debug)]
pub enum ToolchainError {
    Probe { tool: &'static str, path: PathBuf, message: String },
    Io { path: PathBuf, error: std::io::Error },
    Serde(String),
}

impl std::fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Probe { tool, path, message } => write!(
                f,
                "could not probe {tool} at {}: {message}",
                path.display()
            ),
            Self::Io { path, error } => {
                write!(f, "I/O error on {}: {error}", path.display())
            }
            Self::Serde(m) => write!(f, "toolchain json: {m}"),
        }
    }
}

impl std::error::Error for ToolchainError {}

/// Detect the current rustc and clang++ versions. Honors `RUSTC` and
/// `RUSTCC_CLANG` for the executable paths; falls back to the names
/// on `$PATH` otherwise.
pub fn detect() -> Result<Toolchain, ToolchainError> {
    let rustc_path = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let clang_path = std::env::var_os("RUSTCC_CLANG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("clang++"));

    let rustc = probe("rustc", &rustc_path)?;
    let clang = probe("clang++", &clang_path)?;
    Ok(Toolchain { rustc, clang })
}

fn probe(tool: &'static str, path: &Path) -> Result<ToolVersion, ToolchainError> {
    let output = Command::new(path).arg("--version").output().map_err(|e| {
        ToolchainError::Probe {
            tool,
            path: path.to_path_buf(),
            message: format!("spawn: {e}"),
        }
    })?;
    if !output.status.success() {
        return Err(ToolchainError::Probe {
            tool,
            path: path.to_path_buf(),
            message: format!(
                "exit {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return Err(ToolchainError::Probe {
            tool,
            path: path.to_path_buf(),
            message: "empty --version output".into(),
        });
    }
    Ok(ToolVersion {
        path: path.to_path_buf(),
        version: first,
    })
}

/// Read the cached toolchain file if present. Returns `None` on a
/// first run or when the file is unreadable/malformed (callers should
/// re-probe and rewrite).
pub fn load_cached(cache_dir: &Path) -> Option<Toolchain> {
    let path = cache_dir.join("rustcc-toolchain.json");
    let body = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persist `toolchain` to the cache.
pub fn save(
    cache_dir: &Path,
    toolchain: &Toolchain,
) -> Result<PathBuf, ToolchainError> {
    std::fs::create_dir_all(cache_dir).map_err(|error| ToolchainError::Io {
        path: cache_dir.to_path_buf(),
        error,
    })?;
    let path = cache_dir.join("rustcc-toolchain.json");
    let body = serde_json::to_string_pretty(toolchain)
        .map_err(|e| ToolchainError::Serde(e.to_string()))?;
    std::fs::write(&path, &body).map_err(|error| ToolchainError::Io {
        path: path.clone(),
        error,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let tc = Toolchain {
            rustc: ToolVersion {
                path: PathBuf::from("/usr/bin/rustc"),
                version: "rustc 1.75.0 (82e1608df 2023-12-21)".into(),
            },
            clang: ToolVersion {
                path: PathBuf::from("/usr/bin/clang++"),
                version: "clang version 16.0.6".into(),
            },
        };
        let path = save(dir.path(), &tc).unwrap();
        assert!(path.is_file());
        let back = load_cached(dir.path()).unwrap();
        assert_eq!(back, tc);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_cached(dir.path()).is_none());
    }

    #[test]
    fn load_malformed_json_returns_none_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rustcc-toolchain.json"),
            "not json",
        )
        .unwrap();
        assert!(load_cached(dir.path()).is_none());
    }
}
