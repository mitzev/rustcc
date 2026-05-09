//! `rustcc` — developer-experience CLI for rustcc projects.
//!
//! Three subcommands cover the friction points new users hit
//! before they get to write a line of interop code:
//!
//! - `rustcc install`   — download a prebuilt toolchain tarball and
//!                        register it with rustup. ~3 min.
//! - `rustcc doctor`    — run health checks against the local
//!                        environment (libclang reachable, rustup
//!                        toolchain linked, rust-toolchain.toml
//!                        pinning correctly).
//! - `rustcc init`      — scaffold a new rustcc project with the
//!                        right Cargo.toml, rust-toolchain.toml, and
//!                        a starter source file picked from one of
//!                        the three supported surfaces (`class`
//!                        keyword / cxx_class!  macro / cxx_class_native!).
//!
//! All three shell out to `rustup` / `curl` / `tar` rather than
//! pulling in HTTP / archive crates as deps. Keeps the binary
//! small (~700 KB) and the dependency tree shallow.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod doctor;
mod init;
mod install;

#[derive(Parser, Debug)]
#[command(
    name = "rustcc",
    version,
    about = "rustcc developer-experience CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Download a prebuilt rustcc toolchain tarball and link it with rustup.
    Install(install::InstallArgs),
    /// Diagnose common rustcc setup issues.
    Doctor(doctor::DoctorArgs),
    /// Scaffold a new rustcc project.
    Init(init::InitArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Install(args) => install::run(args),
        Command::Doctor(args) => doctor::run(args),
        Command::Init(args) => init::run(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
