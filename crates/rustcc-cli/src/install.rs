//! `rustcc install` — download a prebuilt toolchain and register it with rustup.
//!
//! Mirrors the bash recipe from `fork/INSTALL.md` step-for-step,
//! but auto-detects the host triple, resolves `latest` against the
//! GitHub releases API, verifies the sha256, and registers the
//! toolchain with rustup.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Release tag to install. Defaults to `latest` (resolved
    /// against the GitHub releases API).
    #[arg(long, default_value = "latest")]
    version: String,

    /// Override the host triple. Defaults to whatever
    /// `rustc -vV` reports for `host:`.
    #[arg(long)]
    target: Option<String>,

    /// Toolchain name to register with rustup. Default `rustcc`
    /// (so users invoke the fork via `cargo +rustcc build`).
    #[arg(long, default_value = "rustcc")]
    toolchain_name: String,

    /// Skip the rustup toolchain link step. Useful for CI runners
    /// that want the tarball extracted somewhere predictable but
    /// manage rustup themselves.
    #[arg(long)]
    skip_rustup: bool,
}

pub fn run(args: InstallArgs) -> Result<(), String> {
    let target = match args.target {
        Some(t) => t,
        None => detect_host_triple()?,
    };
    let version = if args.version == "latest" {
        resolve_latest()?
    } else {
        args.version.clone()
    };
    println!("rustcc: installing {version} for {target}");

    let install_dir = home_dir()?
        .join(".rustcc")
        .join(&version);
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("create {}: {e}", install_dir.display()))?;

    let base = format!(
        "https://github.com/rustcc/rustcc/releases/download/{version}",
    );
    let tarball = format!("rustcc-{target}.tar.xz");
    let sha = format!("{tarball}.sha256");
    let work_dir = std::env::temp_dir()
        .join(format!("rustcc-install-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| format!("create {}: {e}", work_dir.display()))?;

    println!("  downloading {tarball}...");
    run_cmd(
        Command::new("curl")
            .arg("-fsSL")
            .arg("-o")
            .arg(work_dir.join(&tarball))
            .arg(format!("{base}/{tarball}")),
    )?;
    run_cmd(
        Command::new("curl")
            .arg("-fsSL")
            .arg("-o")
            .arg(work_dir.join(&sha))
            .arg(format!("{base}/{sha}")),
    )?;

    println!("  verifying sha256...");
    let sha_check = if which::which_exists("shasum") {
        Command::new("shasum")
            .args(["-a", "256", "--check"])
            .arg(work_dir.join(&sha))
            .current_dir(&work_dir)
            .status()
    } else {
        Command::new("sha256sum")
            .arg("--check")
            .arg(work_dir.join(&sha))
            .current_dir(&work_dir)
            .status()
    };
    let status = sha_check.map_err(|e| format!("sha-check spawn failed: {e}"))?;
    if !status.success() {
        return Err("sha256 verification failed".into());
    }

    println!("  extracting to {}...", install_dir.display());
    run_cmd(
        Command::new("tar")
            .arg("-xJf")
            .arg(work_dir.join(&tarball))
            .arg("-C")
            .arg(&install_dir),
    )?;

    let stage1 = install_dir.join("stage1");
    let rustc_bin = stage1.join("bin").join("rustc");
    if !rustc_bin.exists() {
        return Err(format!(
            "extracted toolchain doesn't have stage1/bin/rustc; \
             did the tarball layout change? checked {}",
            rustc_bin.display(),
        ));
    }

    if !args.skip_rustup {
        println!("  registering with rustup as `{}`...", args.toolchain_name);
        // `rustup toolchain link` errors if the name is already
        // bound; remove first then re-link.
        let _ = Command::new("rustup")
            .args(["toolchain", "uninstall"])
            .arg(&args.toolchain_name)
            .status();
        run_cmd(
            Command::new("rustup")
                .args(["toolchain", "link"])
                .arg(&args.toolchain_name)
                .arg(&stage1),
        )?;
    }

    println!();
    println!("rustcc {version} installed at {}", install_dir.display());
    if !args.skip_rustup {
        println!(
            "  use:  cargo +{} build      (per-invocation)",
            args.toolchain_name,
        );
        println!(
            "  or:   echo '[toolchain]\\nchannel = \"{}\"' > rust-toolchain.toml      (per-project)",
            args.toolchain_name,
        );
    }
    Ok(())
}

fn detect_host_triple() -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("invoking rustc -vV failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("host: ") {
            return Ok(rest.trim().to_string());
        }
    }
    Err("rustc -vV didn't surface a `host:` line".into())
}

fn resolve_latest() -> Result<String, String> {
    // GitHub's `releases/latest` redirect is the canonical way to
    // resolve the tag without an auth token. Use `curl -fsSLI` and
    // grep for the Location header.
    let output = Command::new("curl")
        .args([
            "-fsSLI",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            "https://github.com/rustcc/rustcc/releases/latest",
        ])
        .output()
        .map_err(|e| format!("curl spawn failed: {e}"))?;
    if !output.status.success() {
        return Err("could not resolve latest release tag".into());
    }
    let url = String::from_utf8_lossy(&output.stdout);
    // The redirect ends in `/tag/<version>` (URL-encoded plain tag).
    url.rsplit('/')
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "empty resolved URL".into())
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".into())
}

fn run_cmd(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("spawning {:?} failed: {e}", cmd.get_program()))?;
    if !status.success() {
        return Err(format!(
            "{:?} exited with {status}",
            cmd.get_program(),
        ));
    }
    Ok(())
}

mod which {
    use std::path::PathBuf;
    pub fn which_exists(name: &str) -> bool {
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let p = dir.join(name);
                if p.is_file() {
                    return true;
                }
                let _ = PathBuf::from(&p);
            }
        }
        false
    }
}
