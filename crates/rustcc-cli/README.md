# rustcc-cli

Developer-experience CLI for rustcc projects. Three subcommands cover the friction points new users hit before they get to write a line of interop code.

```bash
cargo install --path crates/rustcc-cli
# or, from this repo:
cargo build --release -p rustcc-cli
# binary lands at target/release/rustcc
```

## `rustcc install [--version v1.06.0] [--target ...]`

Downloads a prebuilt rustcc toolchain tarball from the GitHub releases and registers it with rustup.

- `--version`: defaults to `latest`, resolved against the GitHub releases redirect
- `--target`: defaults to `rustc -vV` host triple (`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, ...)
- `--toolchain-name`: defaults to `rustcc` (so `cargo +rustcc build` works)
- `--skip-rustup`: extract only, don't link

Mirrors the bash recipe in `fork/INSTALL.md` step-for-step.

## `rustcc doctor`

Health checks against the local environment:

- `rustup` on PATH
- `rustc` on PATH
- `rustcc` toolchain registered with rustup + linked binary actually invokes
- `clang` on PATH (cxx_importer's libclang dep)
- `rust-toolchain.toml` in cwd (and whether it pins to rustcc)
- nightly toolchain available (some probes use `cargo +nightly`)

Each check prints OK / WARN / FAIL with a one-line fix hint. No early exit — you see the full picture in one pass.

## `rustcc init <name> [--surface class-keyword|cxx-class]`

Scaffolds a new rustcc project with the right `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, and a one-line README. Picks one of the documented [three surfaces](../../fork/THREE-SURFACES.md):

- **class-keyword** (default): fork-only, lowest boilerplate. Source uses `pub class Foo { ... }`.
- **cxx-class**: stable-rustc compatible. Source uses `cxx_class! { ... }` proc macro.

Add `--toolchain rustcc` (default) or `--toolchain stable` to control what the generated `rust-toolchain.toml` pins.
