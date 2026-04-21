//! M6: Link-step argv augmentation.
//!
//! Implements `docs/build_integration.md §4, §7` — pulling the shim
//! object, user-supplied libraries, and the C++ stdlib into rustc's
//! final link invocation. We do this by appending flags to the argv
//! we exec `rustc` with:
//!
//! - `-C link-arg=<shim.o>` routes the Clang-compiled shim object
//!   directly onto the linker command line.
//! - `-L native=<path>` / `-l <name>` mirror the manifest's
//!   `link-search-paths` / `link-libraries` entries into rustc's
//!   library discovery and link set.
//! - `-l c++` + `-l c++abi` (or `-l stdc++`) satisfy the C++ runtime
//!   per the manifest's `stdlib =` key.
//!
//! For rlib crates these flags get recorded in the rlib and activated
//! when a downstream binary links; for binary crates they apply at
//! this rustc invocation directly. Either way, the shim stays on the
//! final link line so the exception trampolines are callable.

use std::path::Path;

use crate::manifest::{CppInteropConfig, Stdlib};

/// Produce the rustc flags that should be appended to argv before
/// `exec`-ing rustc. Pure function of the config + shim object path —
/// no I/O, safe to call regardless of feature flags.
pub fn compute_link_args(
    config: &CppInteropConfig,
    shim_obj: &Path,
) -> Vec<String> {
    let mut out = Vec::new();

    // 1) Shim object — link-arg passes the path verbatim to the linker.
    out.push("-C".to_string());
    out.push(format!("link-arg={}", shim_obj.display()));

    // 2) User-supplied library search paths. `native=` scopes the
    //    path to object-file linking; rustc's default interprets a
    //    bare `-L path` as a dependency path, which is different.
    for path in &config.link_search_paths {
        out.push("-L".to_string());
        out.push(format!("native={}", path.display()));
    }

    // 3) User-supplied libraries. These are `-l name` — rustc looks up
    //    `libname.{a,so,dylib}` on the search paths.
    for lib in &config.link_libraries {
        out.push("-l".to_string());
        out.push(lib.clone());
    }

    // 4) C++ runtime. libc++abi/supc++ provides RTTI and exception
    //    unwinding support; required because the shim body uses
    //    `try/catch`. Ordering matters only for some linkers (GNU ld)
    //    but clang/lld tolerate either; we emit a conservative order.
    match config.stdlib {
        Some(Stdlib::LibCxx) => {
            out.push("-l".to_string());
            out.push("c++".to_string());
            out.push("-l".to_string());
            out.push("c++abi".to_string());
        }
        Some(Stdlib::LibStdCxx) => {
            out.push("-l".to_string());
            out.push("stdc++".to_string());
        }
        None => {
            // Neither was selected — the user either doesn't need the
            // C++ runtime (no exceptions, no STL) or they'll provide
            // it themselves via `link-libraries`.
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CppInteropConfig;
    use std::path::PathBuf;

    fn config_with(
        libs: Vec<String>,
        search: Vec<PathBuf>,
        stdlib: Option<Stdlib>,
    ) -> CppInteropConfig {
        CppInteropConfig {
            headers: Vec::new(),
            header_search_paths: Vec::new(),
            clang_flags: Vec::new(),
            stdlib,
            sidecar: None,
            link_libraries: libs,
            link_search_paths: search,
            emit_stubs: false,
            emit_forwarders: false,
        }
    }

    #[test]
    fn emits_shim_obj_first_as_link_arg() {
        let cfg = config_with(Vec::new(), Vec::new(), None);
        let args =
            compute_link_args(&cfg, &PathBuf::from("/cache/shims.o"));
        assert_eq!(args[0], "-C");
        assert_eq!(args[1], "link-arg=/cache/shims.o");
    }

    #[test]
    fn user_search_paths_use_native_prefix() {
        let cfg = config_with(
            Vec::new(),
            vec![PathBuf::from("/libs/one"), PathBuf::from("/libs/two")],
            None,
        );
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        // Find both -L entries.
        let pairs: Vec<(&str, &str)> = args
            .windows(2)
            .filter(|w| w[0] == "-L")
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1, "native=/libs/one");
        assert_eq!(pairs[1].1, "native=/libs/two");
    }

    #[test]
    fn libraries_emit_as_dash_l_pairs() {
        let cfg = config_with(
            vec!["widget".into(), "fmt".into()],
            Vec::new(),
            None,
        );
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        let libs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-l")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(libs, vec!["widget", "fmt"]);
    }

    #[test]
    fn libcxx_stdlib_adds_cxx_and_cxxabi() {
        let cfg = config_with(Vec::new(), Vec::new(), Some(Stdlib::LibCxx));
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        let libs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-l")
            .map(|w| w[1].as_str())
            .collect();
        assert!(libs.contains(&"c++"), "libs missing c++: {libs:?}");
        assert!(libs.contains(&"c++abi"), "libs missing c++abi: {libs:?}");
    }

    #[test]
    fn libstdcxx_stdlib_adds_stdcxx() {
        let cfg =
            config_with(Vec::new(), Vec::new(), Some(Stdlib::LibStdCxx));
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        let libs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-l")
            .map(|w| w[1].as_str())
            .collect();
        assert!(libs.contains(&"stdc++"), "libs missing stdc++: {libs:?}");
        assert!(!libs.contains(&"c++"), "shouldn't add libc++ alongside");
    }

    #[test]
    fn no_stdlib_emits_no_runtime_libs() {
        let cfg = config_with(Vec::new(), Vec::new(), None);
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        let libs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-l")
            .map(|w| w[1].as_str())
            .collect();
        assert!(libs.is_empty(), "expected no -l entries, got {libs:?}");
    }

    #[test]
    fn emits_user_libs_before_stdlib() {
        // Linker order matters on GNU ld: later libs satisfy earlier
        // references, so stdlib should come after user libs.
        let cfg = config_with(
            vec!["widget".into()],
            Vec::new(),
            Some(Stdlib::LibCxx),
        );
        let args = compute_link_args(&cfg, &PathBuf::from("/s.o"));
        let widget_pos =
            args.iter().position(|s| s == "widget").expect("widget emitted");
        let cxx_pos =
            args.iter().position(|s| s == "c++").expect("c++ emitted");
        assert!(
            widget_pos < cxx_pos,
            "user libs must precede stdlib for correct symbol resolution"
        );
    }
}
