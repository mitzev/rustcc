# Fork regression probes

Each subdirectory under `class_keyword/` is a standalone Cargo
project that exercises one fork feature. Probes are meant to run
against the stage-1 rustc produced by `./x.py build --stage 1
library` in the `rust-lang-rust` checkout.

## Running

```bash
# From the repo root:
RUSTC=<path to stage-1 rustc> ./fork/tests/run.sh
```

If `$RUSTC` is unset the runner defaults to
`$HOME/rust-lang-rust-fork/build/host/stage1/bin/rustc` (where
`fork/build.sh` clones and builds the fork).

The runner builds each probe with `cargo clean` first to avoid
stale-cache false passes, then invokes the resulting binary and
checks stdout for a known-good banner. Exit status is non-zero if
any probe fails.

## What each probe covers

| Probe                              | Feature                                                  | Patch(es)                          |
|------------------------------------|----------------------------------------------------------|------------------------------------|
| `class_keyword/basic`              | `class` keyword with fields + methods                    | P09.30, P09.39                     |
| `class_keyword/inheritance`        | `class D : B { ... }` single inheritance                 | P09.32, P09.39                     |
| `class_keyword/type_generics`      | `class Pair<A, B>` — impl/struct half DefId separation   | P09.41                             |
| `class_keyword/const_generics`     | `class Array<const N: usize>` — const-arg lowering       | P09.41                             |
| `class_keyword/swift_nonpod`       | `swift_value!` class Clone respects non-POD extras       | P09.42                             |
| `class_keyword/swift_value_attr`   | `#[swift_value]` built-in attr macro auto-synthesizes Drop + Clone | P09.46                 |
| `class_keyword/swift_throws`       | `#[rustc_swift_throws]` + `SwiftError` wrapper (Rust-side) | P09.48                 |
| `class_keyword/swift_extern_call`  | `extern "Swift"` (swiftcc) calls + `#[rustc_swift_labels]` | P09.14, P09.17                    |
| `class_keyword/swift_value_type`   | `#[swift_value]` **value** type → Drop/Clone via the VWT  | P09.17, P09.18, P09.46             |
| `targets_linux` (via run_targets.sh) | Polymorphic class IR on Linux x86_64/aarch64/armv7/rv64 | P09.44                             |

## Adding a new probe

1. Create `class_keyword/<name>/Cargo.toml` with a `[[bin]]`
   target and an empty `[workspace]` entry so Cargo treats the
   probe as standalone.
2. Write the probe's source at
   `class_keyword/<name>/src/main.rs`. `main()` must print a
   one-line banner starting with `ok:` on success and `assert!`
   or `panic!` on failure.
3. Add the probe to the `probes` and `expect` arrays in
   `run.sh`, matching the banner prefix the probe prints.

## Why these aren't `cargo test`s

Probes need the fork's stage-1 rustc, which isn't wired into the
workspace's `cargo test` pipeline (the workspace pins a stock
nightly via `rust-toolchain.toml`). The runner shell script is
the thinnest possible harness that uses the right toolchain per
probe.

A future cleanup could move these under an `xtask probe`
subcommand that resolves `$RUSTC` from a config file and runs the
same sequence.

## Target probes (`run_targets.sh`)

`run_targets.sh` is a separate runner for cross-target IR
probes. It compiles `targets_linux/` against each listed triple
with `-Zbuild-std=core,compiler_builtins -C opt-level=0` and
checks that the emitted LLVM IR contains the expected Itanium
symbols (`_ZN6Widget3newEi`, `_ZTI6Widget`, `_ZTS6Widget`,
`_ZTV6Widget`).

```bash
# Default: probe x86_64 / aarch64 / armv7 / rv64 Linux targets.
RUSTC=<stage1> ./fork/tests/run_targets.sh

# Or probe a single target on demand:
RUSTC=<stage1> ./fork/tests/run_targets.sh aarch64-linux-android
```

It's not run from the main `run.sh` because each target takes
~40s to build its stdlib — not worth paying every regression
check. Run this explicitly when adding target coverage to
the fork.
