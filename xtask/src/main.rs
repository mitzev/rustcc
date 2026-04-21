//! Developer tasks for the rustcc workspace.
//!
//! Invoke via `cargo xtask <subcommand>` (alias set in `.cargo/config.toml`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use test_support::clang::{
    self, ClangInvocation, CorpusKind, Directives,
};
use test_support::golden::{self, MangleDump, VtableDump, VtableSubTableDump};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("refresh-goldens") => refresh_goldens(),
        Some("check") => ExitCode::SUCCESS,
        Some("run-demo") => run_point_demo(),
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            print_usage();
            ExitCode::from(2)
        }
        None => {
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  refresh-goldens    regenerate tests/corpus/*.{{layout,mangle,vtable}}.golden");
    eprintln!("  check              no-op sanity check");
    eprintln!("  run-demo           build and run examples/point_demo end-to-end");
}

/// Drive `examples/point_demo` through the full rustcc pipeline and
/// run the resulting binary. Intended for ad-hoc manual inspection —
/// the automated equivalent is
/// `crates/rustcc/tests/point_demo_flow.rs`.
fn run_point_demo() -> ExitCode {
    use std::process::Command;

    let root = workspace_root();
    let demo = root.join("examples/point_demo");
    if !demo.join("Cargo.toml").is_file() {
        eprintln!("missing example: {}", demo.display());
        return ExitCode::from(1);
    }

    // Build rustcc first so we call the freshly-built binary.
    eprintln!("xtask: building rustcc...");
    let build = Command::new("cargo")
        .args(["build", "-p", "rustcc"])
        .current_dir(&root)
        .status();
    match build {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("cargo build failed: {s}");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("cargo build spawn failed: {e}");
            return ExitCode::from(1);
        }
    }

    let rustcc_bin = root.join("target/debug/rustcc");
    if !rustcc_bin.is_file() {
        eprintln!("expected rustcc binary at {}", rustcc_bin.display());
        return ExitCode::from(1);
    }

    // Stub RUSTC so the driver's forward-to-rustc step is a no-op —
    // we only want the interop artifacts today.
    let stub_path = root.join("target/xtask_stub_rustc.sh");
    if let Err(e) =
        fs::write(&stub_path, "#!/bin/sh\nexit 0\n")
    {
        eprintln!("write stub rustc: {e}");
        return ExitCode::from(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = match fs::metadata(&stub_path) {
            Ok(m) => m.permissions(),
            Err(e) => {
                eprintln!("stub rustc metadata: {e}");
                return ExitCode::from(1);
            }
        };
        perms.set_mode(0o755);
        if let Err(e) = fs::set_permissions(&stub_path, perms) {
            eprintln!("stub rustc perms: {e}");
            return ExitCode::from(1);
        }
    }

    let cache = demo.join("target");
    let source = demo.join("src/lib.rs");
    eprintln!("xtask: driving rustcc against {}", source.display());
    let driver = Command::new(&rustcc_bin)
        .env("RUSTC", &stub_path)
        .env("CARGO_TARGET_DIR", &cache)
        .args(["--crate-name", "point_demo"])
        .args(["--cfg", "cpp_interop"])
        .arg(&source)
        .status();
    match driver {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("rustcc driver failed: {s}");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("rustcc spawn failed: {e}");
            return ExitCode::from(1);
        }
    }

    let rustcc_cache = cache.join("rustcc");
    let hpp = rustcc_cache.join("point_demo-cxx.hpp");
    let stubs = rustcc_cache.join("point_demo-cxx-stubs.cpp");
    let bin = rustcc_cache.join("point_demo_binary");
    let consumer = demo.join("cpp/consumer.cpp");
    for p in [&hpp, &stubs, &consumer] {
        if !p.is_file() {
            eprintln!("missing expected artifact: {}", p.display());
            return ExitCode::from(1);
        }
    }

    eprintln!("xtask: compiling consumer with clang++");
    let compile = Command::new("clang++")
        .args(["-std=c++17", "-I"])
        .arg(&rustcc_cache)
        .arg("-o")
        .arg(&bin)
        .arg(&stubs)
        .arg(&consumer)
        .status();
    match compile {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("clang++ failed: {s}");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("clang++ spawn failed: {e}");
            return ExitCode::from(1);
        }
    }

    eprintln!("xtask: running {}", bin.display());
    eprintln!("---");
    let run = Command::new(&bin).status();
    eprintln!("---");
    match run {
        Ok(s) if s.success() => {
            eprintln!("xtask: run-demo ok");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("demo binary exited: {s}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("demo spawn failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

fn corpus_dir() -> PathBuf {
    workspace_root().join("crates/rustc_abi_cxx/tests/corpus")
}

fn refresh_goldens() -> ExitCode {
    let inv = ClangInvocation::default();
    let target = match clang::host_target_triple(&inv) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to detect clang target triple: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("refreshing goldens for target: {target}");

    let dir = corpus_dir();
    let entries = match list_cpp_files(&dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("corpus scan failed: {e}");
            return ExitCode::from(1);
        }
    };
    if entries.is_empty() {
        eprintln!("no .cpp files found under {}", dir.display());
        return ExitCode::from(1);
    }

    let mut failures = 0;
    for source in entries {
        match refresh_one(&inv, &target, &source) {
            Ok(golden_path) => {
                eprintln!("  wrote {}", golden_path.display());
            }
            Err(e) => {
                eprintln!("  FAILED {}: {e}", source.display());
                failures += 1;
            }
        }
    }

    if failures == 0 {
        eprintln!("refresh-goldens: ok");
        ExitCode::SUCCESS
    } else {
        eprintln!("refresh-goldens: {failures} failure(s)");
        ExitCode::from(1)
    }
}

fn refresh_one(
    inv: &ClangInvocation,
    target: &str,
    source: &Path,
) -> Result<PathBuf, String> {
    let directives = clang::read_directives(source)?;
    match directives.kind {
        CorpusKind::Layout => refresh_layout(inv, target, source, &directives),
        CorpusKind::Mangle => refresh_mangle(inv, target, source),
        CorpusKind::Vtable => refresh_vtable(inv, target, source, &directives),
    }
}

fn refresh_layout(
    inv: &ClangInvocation,
    target: &str,
    source: &Path,
    directives: &Directives,
) -> Result<PathBuf, String> {
    let target_class = directives.target.clone().ok_or_else(|| {
        format!(
            "layout corpus {} requires a `// @target ClassName` directive",
            source.display()
        )
    })?;
    let dump_text = clang::dump_record_layouts(inv, source)?;
    let mut dump = clang::parse_dump(&dump_text, &target_class)?;
    dump.target = target.to_string();

    let golden_path = source.with_extension("layout.golden");
    fs::write(&golden_path, golden::render(&dump))
        .map_err(|e| format!("writing {}: {e}", golden_path.display()))?;
    Ok(golden_path)
}

fn refresh_mangle(
    inv: &ClangInvocation,
    target: &str,
    source: &Path,
) -> Result<PathBuf, String> {
    let ir = clang::dump_llvm_ir(inv, source)?;
    let symbols = clang::extract_cxx_symbols(&ir);
    if symbols.is_empty() {
        return Err(format!(
            "no C++ mangled symbols found in LLVM IR of {}",
            source.display()
        ));
    }
    let dump = MangleDump {
        target: target.to_string(),
        symbols,
    };
    let golden_path = source.with_extension("mangle.golden");
    fs::write(&golden_path, golden::render_mangle(&dump))
        .map_err(|e| format!("writing {}: {e}", golden_path.display()))?;
    Ok(golden_path)
}

fn refresh_vtable(
    inv: &ClangInvocation,
    target: &str,
    source: &Path,
    directives: &Directives,
) -> Result<PathBuf, String> {
    let target_class = directives.target.clone().ok_or_else(|| {
        format!(
            "vtable corpus {} requires a `// @target ClassName` directive",
            source.display()
        )
    })?;
    let mangled_target =
        clang::mangle_unqualified_class_name(&target_class);
    let ir = clang::dump_llvm_ir(inv, source)?;
    let entries = clang::extract_vtable(&ir, &mangled_target)?;
    // The single-array LLVM-IR extractor produces one sub-table worth
    // of entries, serving the class's own vptr (subobject == the class
    // itself, offset 0). Multi-inherit polymorphic classes have
    // additional sub-tables concatenated into the same `_ZTV<class>`
    // global; those aren't parsed here yet and are instead covered by
    // in-process `ctx.vtable()` tests in `cxx_importer`.
    let dump = VtableDump {
        target: target.to_string(),
        class: target_class.clone(),
        sub_tables: vec![VtableSubTableDump {
            for_subobject: target_class,
            subobject_offset: 0,
            address_point_slot: 2,
            entries,
        }],
    };
    let golden_path = source.with_extension("vtable.golden");
    fs::write(&golden_path, golden::render_vtable(&dump))
        .map_err(|e| format!("writing {}: {e}", golden_path.display()))?;
    Ok(golden_path)
}

fn list_cpp_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let read = fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in read {
        let entry =
            entry.map_err(|e| format!("iterating {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("cpp") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
