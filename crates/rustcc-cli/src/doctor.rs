//! `rustcc doctor` — health check against the local environment.
//!
//! Each check prints OK / WARN / FAIL with a one-line explanation
//! plus a fix hint. Runs every check unconditionally (no early
//! exit) so users see the full picture in one pass.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;

#[derive(Args, Debug)]
pub struct DoctorArgs {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "  OK  ",
            Verdict::Warn => " WARN ",
            Verdict::Fail => " FAIL ",
        }
    }
}

fn check(name: &str, verdict: Verdict, msg: &str) {
    println!("[{}] {name}: {msg}", verdict.label());
}

pub fn run(_args: DoctorArgs) -> Result<(), String> {
    println!("rustcc doctor — environment health check\n");
    let mut any_fail = false;

    // 1. rustup itself.
    match Command::new("rustup").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("(unknown)")
                .to_string();
            check("rustup", Verdict::Ok, &v);
        }
        _ => {
            check(
                "rustup",
                Verdict::Fail,
                "not found on PATH; install via https://rustup.rs/",
            );
            any_fail = true;
        }
    }

    // 2. rustc on PATH (any toolchain).
    match Command::new("rustc").arg("--version").output() {
        Ok(o) if o.status.success() => {
            check(
                "rustc",
                Verdict::Ok,
                String::from_utf8_lossy(&o.stdout).trim(),
            );
        }
        _ => {
            check("rustc", Verdict::Fail, "not found on PATH");
            any_fail = true;
        }
    }

    // 3. rustcc toolchain registered with rustup.
    let toolchain_listed = Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim().split_whitespace().next() == Some("rustcc"))
        })
        .unwrap_or(false);
    if toolchain_listed {
        // Confirm the linked binary actually invokes.
        match Command::new("rustc")
            .args(["+rustcc", "--version"])
            .output()
        {
            Ok(o) if o.status.success() => {
                check(
                    "rustcc toolchain",
                    Verdict::Ok,
                    String::from_utf8_lossy(&o.stdout).trim(),
                );
            }
            _ => {
                check(
                    "rustcc toolchain",
                    Verdict::Warn,
                    "rustup lists `rustcc` but the linked binary doesn't run; \
                     re-run `rustcc install` to refresh.",
                );
            }
        }
    } else {
        check(
            "rustcc toolchain",
            Verdict::Warn,
            "no `rustcc` toolchain in `rustup toolchain list`; install with: \
             rustcc install",
        );
    }

    // 4. clang / libclang reachable for cxx_importer.
    let clang_available = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if clang_available {
        check(
            "clang",
            Verdict::Ok,
            "found on PATH (cxx_importer's libclang dep should resolve)",
        );
    } else {
        check(
            "clang",
            Verdict::Warn,
            "not found on PATH; cxx_importer needs libclang. \
             macOS: `brew install llvm`. Debian: `apt install libclang-dev`.",
        );
    }

    // 5. rust-toolchain.toml in cwd, if any.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let toolchain_toml = cwd.join("rust-toolchain.toml");
    if toolchain_toml.exists() {
        let body = std::fs::read_to_string(&toolchain_toml).unwrap_or_default();
        if body.contains("channel = \"rustcc\"") {
            check(
                "rust-toolchain.toml",
                Verdict::Ok,
                "pins channel = \"rustcc\" — `cargo build` will use the fork.",
            );
        } else {
            check(
                "rust-toolchain.toml",
                Verdict::Warn,
                "exists but doesn't pin to rustcc; running `cargo +rustcc build` \
                 explicitly will still work.",
            );
        }
    } else {
        check(
            "rust-toolchain.toml",
            Verdict::Warn,
            "no rust-toolchain.toml in cwd; you'll need `cargo +rustcc build` \
             to invoke the fork.",
        );
    }

    // 6. nightly cargo available (probe binaries use `+nightly` for cargo).
    let nightly_available = Command::new("cargo")
        .args(["+nightly", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if nightly_available {
        check(
            "nightly toolchain",
            Verdict::Ok,
            "available (some example crates use `cargo +nightly` cargo, \
             with the rustcc rustc via RUSTC env)",
        );
    } else {
        check(
            "nightly toolchain",
            Verdict::Warn,
            "not installed; `rustup toolchain install nightly --profile minimal` \
             if you want to run the example probes.",
        );
    }

    println!();
    if any_fail {
        Err("one or more critical checks failed; see above".into())
    } else {
        Ok(())
    }
}
