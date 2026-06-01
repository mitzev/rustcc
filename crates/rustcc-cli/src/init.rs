//! `rustcc init` — scaffold a new rustcc project.
//!
//! Picks one of the three documented surfaces (see
//! `fork/THREE-SURFACES.md`) and writes a starter Cargo.toml,
//! rust-toolchain.toml, src/main.rs, and a one-line README.
//!
//! Default surface is `class-keyword` (lowest boilerplate, fork-only).
//! For users who want stable-rustc compatibility, pass
//! `--surface=cxx-class` to get the proc-macro form.

use std::path::PathBuf;

use clap::{Args, ValueEnum};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Project directory name. Created relative to cwd.
    name: String,

    /// Which interop surface to use. See `fork/THREE-SURFACES.md`.
    #[arg(long, value_enum, default_value_t = Surface::ClassKeyword)]
    surface: Surface,

    /// Toolchain to pin in rust-toolchain.toml.
    #[arg(long, default_value = "rustcc")]
    toolchain: String,

    /// Overwrite the project directory if it exists.
    #[arg(long)]
    force: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Surface {
    /// `class Foo { ... }` — fork-only, lowest boilerplate.
    ClassKeyword,
    /// `cxx_class! { ... }` — stable-rustc compatible proc macro.
    CxxClass,
}

pub fn run(args: InitArgs) -> Result<(), String> {
    let dir = PathBuf::from(&args.name);
    if dir.exists() {
        if args.force {
            std::fs::remove_dir_all(&dir).map_err(|e| {
                format!("removing existing {}: {e}", dir.display())
            })?;
        } else {
            return Err(format!(
                "directory {} exists; pass --force to overwrite",
                dir.display(),
            ));
        }
    }
    std::fs::create_dir_all(dir.join("src"))
        .map_err(|e| format!("create {}/src: {e}", dir.display()))?;

    let cargo_toml = match args.surface {
        Surface::ClassKeyword => CARGO_TOML_CLASS,
        Surface::CxxClass => CARGO_TOML_CXX_CLASS,
    };
    let main_rs = match args.surface {
        Surface::ClassKeyword => MAIN_RS_CLASS,
        Surface::CxxClass => MAIN_RS_CXX_CLASS,
    };
    let toolchain_toml = format!(
        "[toolchain]\nchannel = \"{}\"\n",
        args.toolchain,
    );
    let readme = format!(
        "# {name}\n\
         \n\
         Scaffolded by `rustcc init`. Surface: `{surface:?}`.\n\
         \n\
         Build:\n\
         \n\
         ```bash\n\
         cargo +{tc} build\n\
         ```\n\
         \n\
         If `+{tc}` errors out, run `rustcc install` first.\n",
        name = args.name,
        surface = args.surface,
        tc = args.toolchain,
    );

    write(&dir.join("Cargo.toml"), cargo_toml.replace("__NAME__", &args.name))?;
    write(&dir.join("rust-toolchain.toml"), toolchain_toml)?;
    write(&dir.join("src").join("main.rs"), main_rs.into())?;
    write(&dir.join("README.md"), readme)?;

    println!("✔ Scaffolded {} ({:?} surface)", args.name, args.surface);
    println!();
    println!("  cd {} && cargo build", args.name);
    println!();
    Ok(())
}

fn write(path: &std::path::Path, body: String) -> Result<(), String> {
    std::fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

const CARGO_TOML_CLASS: &str = r#"[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;

const MAIN_RS_CLASS: &str = r#"// Scaffolded by `rustcc init --surface class-keyword`.
//
// The `class` keyword is fork-only; needs a rustcc toolchain to
// compile. See `fork/THREE-SURFACES.md` in the rustcc repo for
// the alternative surfaces that work on stable rustc.

#![feature(rustc_attrs)]
// rustcc's class/ctor/virtual attributes ride `rustc_attrs`, an
// internal feature — allow it so the build is warning-free.
#![allow(internal_features)]

pub class Counter {
    n: i64,

    #[constructor]
    pub fn new() -> Self {
        Counter { n: 0 }
    }

    pub fn bump(&mut self, by: i64) {
        self.n += by;
    }

    #[cpp_virtual]
    pub fn value(&self) -> i64 {
        self.n
    }
}

fn main() {
    let mut c = Counter::new();
    c.bump(7);
    println!("counter = {}", c.value());
}
"#;

const CARGO_TOML_CXX_CLASS: &str = r#"[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[dependencies]
# rustcc_macros publishes the stable-rustc-compatible cxx_class!
# proc macro. Replace the path/version with whatever you've
# vendored or pulled from crates.io.
rustcc_macros = { path = "../crates/rustcc_macros" }
"#;

const MAIN_RS_CXX_CLASS: &str = r#"// Scaffolded by `rustcc init --surface cxx-class`.
//
// `cxx_class!` is the stable-rustc-friendly proc-macro surface.
// Compiles under any rustc with `rustc_attrs` available; only
// the rustcc fork understands the runtime semantics.

use rustcc_macros::cxx_class;

cxx_class! {
    pub struct Counter {
        n: i64,
    }

    impl Counter {
        #[constructor]
        pub fn new() -> Self;

        pub fn bump(&mut self, by: i64);

        pub fn value(&self) -> i64;
    }
}

fn main() {
    let mut c = Counter::new();
    c.bump(7);
    println!("counter = {}", c.value());
}
"#;
