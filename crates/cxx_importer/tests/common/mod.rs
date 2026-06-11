//! Shared C++-toolchain selection for the e2e tests.
//!
//! Honors `$CXX` first (the GCC-on-Linux CI leg sets `CXX=g++`), then
//! falls back to `clang++`, then `g++`. The two families need
//! different stdlib plumbing when a test LINKS a runtime binary:
//!
//! - clang++ compiles with `-stdlib=libc++` and links `-lc++`
//!   (matching symbols on macOS and on Linux runners with
//!   libc++-dev installed);
//! - g++ uses its default libstdc++ and links `-lstdc++`. On hosts
//!   where libstdc++ lives outside the default linker path (e.g.
//!   Homebrew gcc on macOS), `lib_search_dir()` exposes the
//!   compiler's own answer for an extra `-L`.
#![allow(dead_code)] // each test binary uses a subset

use std::process::Command;

pub struct CxxToolchain {
    pub compiler: String,
    /// GNU g++ family (else clang family).
    pub is_gxx: bool,
}

pub fn find_cxx() -> Option<CxxToolchain> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(cxx) = std::env::var("CXX") {
        if !cxx.trim().is_empty() {
            candidates.push(cxx);
        }
    }
    for c in ["clang++", "/usr/bin/clang++", "/usr/local/bin/clang++", "g++"] {
        candidates.push(c.to_string());
    }
    for cand in candidates {
        let out = Command::new(&cand).arg("--version").output();
        if let Ok(out) = out {
            if out.status.success() {
                let banner = String::from_utf8_lossy(&out.stdout).to_lowercase();
                // Apple aliases `g++` to clang — trust the banner, not
                // the binary name.
                let is_gxx = !banner.contains("clang");
                return Some(CxxToolchain { compiler: cand, is_gxx });
            }
        }
    }
    None
}

impl CxxToolchain {
    /// Flags for compiling a C++ TU that will later be LINKED into a
    /// Rust binary together with `link_libs()`.
    pub fn stdlib_compile_flags(&self) -> &'static [&'static str] {
        if self.is_gxx {
            &[] // g++ default: libstdc++
        } else {
            // Make both ends agree on libc++ (on Linux clang++
            // defaults to libstdc++, whose std::__cxx11 symbols would
            // not resolve against -lc++).
            &["-stdlib=libc++"]
        }
    }

    /// `-l` flags for the rustc link of the runtime binary.
    pub fn link_libs(&self) -> &'static [&'static str] {
        if self.is_gxx { &["-lstdc++"] } else { &["-lc++"] }
    }

    /// Directory holding the compiler's C++ stdlib, for an extra
    /// `-L` when it is outside the default linker path (Homebrew gcc
    /// on macOS). None when the compiler can't say or the answer
    /// carries no directory.
    pub fn lib_search_dir(&self) -> Option<std::path::PathBuf> {
        if !self.is_gxx {
            return None;
        }
        let file = if cfg!(target_os = "macos") { "libstdc++.dylib" } else { "libstdc++.so" };
        let out = Command::new(&self.compiler)
            .arg(format!("-print-file-name={file}"))
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let p = std::path::Path::new(&path);
        // When not found, the compiler echoes the bare file name back.
        let dir = p.parent()?;
        if dir.as_os_str().is_empty() || !p.is_absolute() {
            return None;
        }
        Some(dir.to_path_buf())
    }

    /// Soft-skip matcher for the link step: the host lacks the
    /// selected stdlib's dev package.
    pub fn stdlib_missing(&self, rustc_stderr: &str) -> bool {
        if self.is_gxx {
            rustc_stderr.contains("library 'stdc++' not found")
                || rustc_stderr.contains("cannot find -lstdc++")
        } else {
            rustc_stderr.contains("library 'c++' not found")
                || rustc_stderr.contains("cannot find -lc++")
        }
    }
}
