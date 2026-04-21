//! `rustcc` binary entry point.
//!
//! Thin wrapper: classify the invocation, then either exec `rustc`
//! verbatim (passthrough) or run the interop pipeline and then invoke
//! `rustc`. See `docs/build_integration.md §2, §4`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustcc::toolchain;
use rustcc::{
    cache_dir, classify_invocation, commit_fingerprint, emit_rust_forwarders,
    emit_rust_hpp, emit_rust_stubs, find_crate_name, find_manifest,
    fingerprint_with_toolchain, load_config, resolve_rustc, rustc_command,
    scan_rust_sources, CppInteropConfig, FingerprintState, Invocation,
    RustScanResult,
};

#[cfg(feature = "libclang")]
use rustcc::{compute_link_args, emit_and_compile, emit_hpp, parse_headers};

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let rustc = resolve_rustc();

    match classify_invocation(&argv) {
        Invocation::Passthrough => exec_or_spawn(&rustc, &argv),
        Invocation::Interop { source_file } => {
            match run_interop_phases(&argv, source_file.as_deref()) {
                Ok(extra) => {
                    argv.extend(extra);
                    exec_or_spawn(&rustc, &argv)
                }
                Err(msg) => {
                    eprintln!("rustcc: {msg}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn run_interop_phases(
    argv: &[String],
    source_file: Option<&Path>,
) -> Result<Vec<String>, String> {
    let (cfg, manifest_dir) = load_interop(source_file)?;
    eprintln!(
        "rustcc: interop mode — loaded [cpp-interop] with {h} header(s), \
         {i} include path(s), {f} clang flag(s)",
        h = cfg.headers.len(),
        i = cfg.header_search_paths.len(),
        f = cfg.clang_flags.len(),
    );

    let crate_name =
        find_crate_name(argv).unwrap_or_else(|| "crate".to_string());
    let cache = cache_dir(&manifest_dir);

    // Rust-side scan runs always — it has no libclang dependency.
    let scan = scan_rust_sources(&manifest_dir)
        .map_err(|e| format!("rust scan: {e}"))?;
    eprintln!(
        "rustcc: rust-scan — {n} #[repr(cpp)] type(s) across {s} source file(s)",
        n = scan.rust_classes.len(),
        s = scan.sources.len(),
    );

    // Toolchain detection — probes rustc + clang++, caches the pair so
    // upgrades surface in the banner. Non-fatal: if probing fails (no
    // clang++ on the path, offline mode, etc.) we proceed without it
    // rather than blocking a crate that doesn't even need clang.
    let toolchain = match toolchain::detect() {
        Ok(tc) => {
            let _ = toolchain::save(&cache, &tc);
            eprintln!(
                "rustcc: toolchain rustc=\"{r}\" clang=\"{c}\"",
                r = tc.rustc.version,
                c = tc.clang.version,
            );
            Some(tc)
        }
        Err(e) => {
            eprintln!("rustcc: toolchain probe failed (continuing): {e}");
            None
        }
    };

    let fp_state = build_fingerprint(
        &cfg,
        &scan.sources,
        toolchain.as_ref(),
        &cache,
        &crate_name,
    )?;
    eprintln!(
        "rustcc: fingerprint {fp_short} ({status})",
        fp_short = &fp_state.current[..16.min(fp_state.current.len())],
        status = if fp_state.inputs_changed() {
            "changed"
        } else {
            "unchanged"
        },
    );

    // Emit the Rust-side `.hpp` (if any Rust types exist). Always
    // available — no libclang needed.
    let rust_hpp = emit_rust_hpp(
        &scan.ctx,
        &cache,
        &crate_name,
        fp_state.inputs_changed(),
    )
    .map_err(|e| format!("rust-hpp phase: {e}"))?;
    if rust_hpp.class_count > 0 {
        eprintln!(
            "rustcc: rust-hpp {status} → {path} ({n} class(es))",
            status = if rust_hpp.emitted { "emitted" } else { "cached" },
            path = rust_hpp.path.display(),
            n = rust_hpp.class_count,
        );
    }

    // Mutual exclusion: both stubs and forwarders emit bodies for the
    // same Itanium-mangled symbols. Pick forwarders when both are on
    // (they produce real Rust bodies, not abort placeholders) and
    // warn the user about the override.
    let emit_stubs = cfg.emit_stubs && !cfg.emit_forwarders;
    if cfg.emit_stubs && cfg.emit_forwarders {
        eprintln!(
            "rustcc: warning — both emit-stubs and emit-forwarders are set; \
             forwarders win (stubs suppressed)"
        );
    }

    // Stub `.cpp` emission (opt-in via emit-stubs = true, no
    // forwarders).
    if emit_stubs {
        let stubs = emit_rust_stubs(
            &scan.ctx,
            &cache,
            &crate_name,
            fp_state.inputs_changed(),
        )
        .map_err(|e| format!("rust-stubs phase: {e}"))?;
        if stubs.class_count > 0 {
            eprintln!(
                "rustcc: rust-stubs {status} → {path}",
                status = if stubs.emitted { "emitted" } else { "cached" },
                path = stubs.path.display(),
            );
        }
    }

    // Forwarder `.rs` emission (opt-in via emit-forwarders = true).
    // When enabled, set RUSTCC_FORWARDERS_PATH so the user's crate
    // can `include!(env!("RUSTCC_FORWARDERS_PATH"))` and add
    // `--cfg rustcc_forwarders` so the include guard is active.
    let mut cfg_flags_for_rustc: Vec<String> = Vec::new();
    if cfg.emit_forwarders {
        let fwd = emit_rust_forwarders(
            &scan.ctx,
            &cache,
            &crate_name,
            fp_state.inputs_changed(),
        )
        .map_err(|e| format!("rust-forwarders phase: {e}"))?;
        if fwd.class_count > 0 {
            eprintln!(
                "rustcc: rust-forwarders {status} → {path}",
                status = if fwd.emitted { "emitted" } else { "cached" },
                path = fwd.path.display(),
            );
            // SAFETY: set_var is racy if multiple threads mutate env
            // concurrently; this is the main thread before exec.
            unsafe {
                std::env::set_var(
                    "RUSTCC_FORWARDERS_PATH",
                    fwd.path.as_os_str(),
                );
            }
            cfg_flags_for_rustc.push("--cfg".into());
            cfg_flags_for_rustc.push("rustcc_forwarders".into());
        }
    }

    let mut extra_argv = cfg_flags_for_rustc;
    extra_argv.extend(
        run_parse_and_shim_phases(&cfg, &fp_state, &cache, &crate_name)?,
    );

    commit_fingerprint(&fp_state)
        .map_err(|e| format!("failed to commit fingerprint: {e}"))?;
    Ok(extra_argv)
}

fn build_fingerprint(
    cfg: &CppInteropConfig,
    rust_sources: &[std::path::PathBuf],
    toolchain: Option<&toolchain::Toolchain>,
    cache: &Path,
    crate_name: &str,
) -> Result<FingerprintState, String> {
    let current = fingerprint_with_toolchain(cfg, rust_sources, toolchain)
        .map_err(|e| format!("fingerprint phase: {e}"))?;
    let path = cache.join(format!("{crate_name}.fingerprint"));
    let prior = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string());
    Ok(FingerprintState { current, prior, path })
}

// Silence dead-code from cfg-flagged helpers (kept so passthrough tests
// can still import from main.rs-adjacent symbols if needed later).
#[allow(dead_code)]
fn _touch_rust_scan_result_used_from_main(_s: &RustScanResult) {}

#[cfg(feature = "libclang")]
fn run_parse_and_shim_phases(
    cfg: &CppInteropConfig,
    fp_state: &rustcc::FingerprintState,
    cache: &Path,
    crate_name: &str,
) -> Result<Vec<String>, String> {
    let ir = parse_headers(cfg).map_err(|e| format!("parse phase: {e}"))?;
    eprintln!("rustcc: parsed {n} class(es)", n = ir.classes.len());

    let art = emit_and_compile(
        cfg,
        &ir,
        cache,
        crate_name,
        fp_state.inputs_changed(),
    )
    .map_err(|e| format!("shim phase: {e}"))?;
    eprintln!(
        "rustcc: shims {status} → {obj}",
        status = if art.compiled { "compiled" } else { "cached" },
        obj = art.obj_path.display(),
    );

    let hpp_art = emit_hpp(
        cfg,
        &ir,
        cache,
        crate_name,
        fp_state.inputs_changed(),
    )
    .map_err(|e| format!("hpp phase: {e}"))?;
    eprintln!(
        "rustcc: hpp {status} → {path}",
        status = if hpp_art.emitted { "emitted" } else { "cached" },
        path = hpp_art.path.display(),
    );

    let link_args = compute_link_args(cfg, &art.obj_path);
    eprintln!(
        "rustcc: link — +{n} args for shim obj, {libs} user lib(s), stdlib={stdlib:?}",
        n = link_args.len(),
        libs = cfg.link_libraries.len(),
        stdlib = cfg.stdlib,
    );
    Ok(link_args)
}

#[cfg(not(feature = "libclang"))]
fn run_parse_and_shim_phases(
    _cfg: &CppInteropConfig,
    _fp_state: &rustcc::FingerprintState,
    _cache: &Path,
    _crate_name: &str,
) -> Result<Vec<String>, String> {
    // Soft-fail: interop-mode user opted in, but rustcc was built
    // without libclang. Warn loudly and continue — no shim object is
    // produced, so downstream link steps will fail if they expect
    // one, but that's a clearer error than silently succeeding.
    eprintln!(
        "rustcc: warning — libclang feature not enabled; \
         skipping header parse and shim compilation (rebuild with \
         `--features libclang` once you need C++ interop)"
    );
    Ok(Vec::new())
}

fn load_interop(
    source_file: Option<&Path>,
) -> Result<(CppInteropConfig, PathBuf), String> {
    let source = source_file.ok_or_else(|| {
        "interop mode detected (--cfg cpp_interop) but no source file \
         positional argument was found; cannot locate Cargo.toml"
            .to_string()
    })?;
    let manifest = find_manifest(source).ok_or_else(|| {
        format!(
            "interop mode: no Cargo.toml found walking upward from {}",
            source.display(),
        )
    })?;
    let cfg = load_config(&manifest).map_err(|e| e.to_string())?;
    let manifest_dir = manifest
        .parent()
        .expect("manifest has a parent")
        .to_path_buf();
    Ok((cfg, manifest_dir))
}

#[cfg(unix)]
fn exec_or_spawn(rustc: &std::path::Path, argv: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;

    let err = rustc_command(rustc, argv).exec();
    eprintln!("rustcc: failed to exec {}: {err}", rustc.display());
    ExitCode::from(127)
}

#[cfg(not(unix))]
fn exec_or_spawn(rustc: &std::path::Path, argv: &[String]) -> ExitCode {
    match rustc_command(rustc, argv).status() {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(code.clamp(0, 255) as u8),
            None => ExitCode::from(137),
        },
        Err(err) => {
            eprintln!("rustcc: failed to spawn {}: {err}", rustc.display());
            ExitCode::from(127)
        }
    }
}
