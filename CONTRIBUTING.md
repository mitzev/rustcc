# Contributing to rustcc

Thanks for looking at the project. This document is a quick map of how the
repo is laid out, how the fork is built, and what the contribution workflow
looks like.

## Repo shape

rustcc is two things stitched together:

1. **A workspace of crates** in `crates/` that compile with a stock nightly
   rustc. These provide the shared infrastructure (Itanium layout/mangling,
   proc macros, runtime types). `cargo test --workspace` runs here.

2. **A fork of `rustc` itself**, stored as a series of patches in
   `fork/patches/` rather than a vendored tree. `fork/build.sh` clones
   `rust-lang/rust` at a pinned commit, applies the series in order, and
   builds a stage-1 compiler.

See [`docs/repo_layout.md`](docs/repo_layout.md) for the full picture.

## Prerequisites

- A Linux or macOS host with ~10 GB free for the fork build.
- `git`, `python3`, `cmake`, `ninja`, `clang` / `clang++` on `PATH`.
- Stock nightly rustc via `rustup` — the workspace's `rust-toolchain.toml`
  pins the exact nightly. `rustup` will fetch it on first invocation.

## Working in the workspace

```sh
cargo build --workspace
cargo test  --workspace
```

The workspace baseline is **235 passing, 0 failing**. Regressions block
merges.

The swift-runtime crate `crates/rustcc_swift_rt` and the fork-syntax
examples (`examples/virtual_override`, `examples/bare_metal_arm`) are
deliberately excluded from the workspace — they require the forked
compiler. Probes that need them depend via explicit path entries and
build under the stage-1 toolchain.

## Working on the compiler fork

```sh
./fork/build.sh              # clone + apply + build stage-1 (~30–90 min)
./fork/build.sh --apply-only # CI-style: just verify patches apply cleanly
```

Register the stage-1 toolchain:

```sh
rustup toolchain link rustcc <rust-lang-rust>/build/host/stage1
cargo +rustcc build          # builds with the fork
```

Adding or changing a patch:

1. Check out the pinned upstream commit in your clone of `rust-lang/rust`.
2. Apply the existing series in order, then make your edit on top.
3. Export the new change as a patch file into `fork/patches/` using the
   existing naming scheme (`NN-<slug>.patch`, zero-padded, sequential).
4. Append a narrative entry to [`fork/PATCHES.md`](fork/PATCHES.md). Each
   section documents the intent, affected compiler crates, and validation
   probe. This is the authoritative spec for the fork; the `.patch` files
   are the mechanical implementation.
5. If the change is user-facing, update
   [`fork/getting-started.html`](fork/getting-started.html) (feature
   matrix, quickstart, or roadmap).

## Validation

Before opening a PR:

- [ ] `cargo test --workspace` passes.
- [ ] If you touched `fork/patches/`: `./fork/build.sh --apply-only` passes.
- [ ] If you touched the compiler itself: a stage-1 build succeeds and
      relevant `/tmp/p09-*`-style probes still run.
- [ ] `fork/PATCHES.md` has a new subsection describing the change.

## Commit and PR style

- Keep commits focused. Compiler-fork changes and workspace changes are
  usually cleanest as separate commits, even within one PR.
- Patch-file additions should reference the `PATCHES.md` section number
  they correspond to.
- Open PRs against `main`. No squash requirement; clean history is
  preferred over one monolithic commit.

## Scope

In-scope:

- Itanium C++ ABI compatibility (layout, mangling, vtables, RTTI).
- Swift 5.9+ ABI compatibility (swiftcc, retain/release, VWT).
- Targets listed in the README feature matrix.

Out of scope (see [README.md](README.md#non-goals)):

- Replacing the `cxx` crate for projects that don't want a forked compiler.
- Windows MSVC ABI — separate effort on a separate fork.
- Silent compatibility with pre-floor Clang versions.

## Reporting bugs

Open a GitHub issue with:

- The patch series HEAD (`git -C fork/patches rev-parse HEAD` or the
  newest patch filename).
- The stage-1 rustc version (`rustc +rustcc --version --verbose`).
- The target triple.
- A minimal reproducer — ideally in the `/tmp/p09-*` probe style (a tiny
  `Cargo.toml` + `lib.rs` + `runner.c` triple).

## License

By contributing you agree that your contributions are dual-licensed under
MIT and Apache-2.0, matching the rest of the project and upstream rustc.
