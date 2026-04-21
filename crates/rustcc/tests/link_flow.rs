//! M6 end-to-end: the rustcc binary appends link flags (shim object,
//! user libraries, stdlib) to the rustc argv before exec.

#![cfg(all(unix, feature = "libclang"))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn rustcc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustcc"))
}

fn write_stub_rustc(script_path: &Path, argv_out: &Path) {
    // Stub captures argv one-per-line so the test can assert
    // forwarding + augmentation.
    let body = format!(
        "#!/bin/sh\n\
         : > {out}\n\
         for a in \"$@\"; do\n\
         \tprintf '%s\\n' \"$a\" >> {out}\n\
         done\n",
        out = shell_escape(argv_out.to_str().unwrap()),
    );
    std::fs::write(script_path, body).unwrap();
    let mut perms = std::fs::metadata(script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script_path, perms).unwrap();
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn read_argv(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn interop_mode_appends_link_args_for_shim_user_libs_and_stdlib() {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("mycrate");
    let src_dir = crate_dir.join("src");
    let cpp_dir = crate_dir.join("cpp");
    let libs_dir = crate_dir.join("libs");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&cpp_dir).unwrap();
    std::fs::create_dir_all(&libs_dir).unwrap();

    std::fs::write(
        cpp_dir.join("widget.hpp"),
        "struct Widget { void tick(); };\n",
    )
    .unwrap();
    let libs_abs = libs_dir.canonicalize().unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "\
[package]
name = \"mycrate\"
version = \"0.1.0\"

[cpp-interop]
headers = [\"cpp/widget.hpp\"]
clang-flags = [\"-std=c++17\"]
stdlib = \"libc++\"
link-libraries = [\"widget\", \"fmt\"]
link-search-paths = [{libs:?}]
",
            libs = libs_abs.display().to_string(),
        ),
    )
    .unwrap();

    let source = src_dir.join("lib.rs");
    std::fs::write(&source, "").unwrap();

    let captured = tmp.path().join("argv.txt");
    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub, &captured);

    let status = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--cfg", "cpp_interop", "--crate-name", "mycrate"])
        .arg(&source)
        .status()
        .expect("spawn rustcc");
    assert!(status.success(), "rustcc exited non-zero");

    let argv = read_argv(&captured);

    // Original argv is preserved at the head.
    assert!(argv.contains(&"--cfg".to_string()));
    assert!(argv.contains(&"cpp_interop".to_string()));
    assert!(argv.contains(&"--crate-name".to_string()));
    assert!(argv.contains(&"mycrate".to_string()));
    assert!(argv.iter().any(|a| a.ends_with("src/lib.rs")));

    // Shim object routed via -C link-arg=<path>.
    let shim_path =
        crate_dir.join("target/rustcc/mycrate.shims.o");
    let link_arg_value = format!("link-arg={}", shim_path.display());
    let pos = argv
        .iter()
        .position(|a| a == &link_arg_value)
        .expect(&format!("missing link-arg={} in {:?}", shim_path.display(), argv));
    assert_eq!(argv[pos - 1], "-C", "link-arg should follow a -C");

    // User libraries as -l pairs, in the manifest's order.
    let l_pairs: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "-l")
        .map(|w| w[1].as_str())
        .collect();
    let widget_idx =
        l_pairs.iter().position(|&l| l == "widget").expect("widget emitted");
    let fmt_idx =
        l_pairs.iter().position(|&l| l == "fmt").expect("fmt emitted");
    assert!(widget_idx < fmt_idx, "manifest order preserved");

    // libc++ runtime emitted after user libs.
    let cxx_idx =
        l_pairs.iter().position(|&l| l == "c++").expect("c++ emitted");
    assert!(
        fmt_idx < cxx_idx,
        "stdlib should come after user libs. got: {l_pairs:?}"
    );
    assert!(
        l_pairs.iter().any(|&l| l == "c++abi"),
        "c++abi emitted: {l_pairs:?}"
    );

    // -L native=<libs-dir> present.
    let l_capital: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "-L")
        .map(|w| w[1].as_str())
        .collect();
    let expected = format!("native={}", libs_abs.display());
    assert!(
        l_capital.iter().any(|&s| s == expected),
        "missing -L native={}; got: {l_capital:?}",
        libs_abs.display(),
    );
}

#[test]
fn passthrough_invocation_receives_unmodified_argv() {
    // Belt-and-suspenders: regression-guard that M6 doesn't leak link
    // flags into non-interop invocations.
    let tmp = TempDir::new().unwrap();
    let captured = tmp.path().join("argv.txt");
    let stub = tmp.path().join("fake-rustc.sh");
    write_stub_rustc(&stub, &captured);

    let status = Command::new(rustcc_bin())
        .env("RUSTC", &stub)
        .args(["--edition", "2021", "--crate-name", "foo", "src/lib.rs"])
        .status()
        .expect("spawn rustcc");
    assert!(status.success());

    let argv = read_argv(&captured);
    assert_eq!(
        argv,
        vec![
            "--edition".to_string(),
            "2021".to_string(),
            "--crate-name".to_string(),
            "foo".to_string(),
            "src/lib.rs".to_string(),
        ],
    );
    assert!(
        !argv.iter().any(|a| a.starts_with("link-arg=")),
        "passthrough must not inject link args"
    );
}
