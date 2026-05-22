# rustcc v1.08.0

**The "documentation in sync + RA binaries actually ship" release.** Two PRs since v1.07.0:

- **Docs refresh** so the repo's top-of-tree reflects the actual shipped state — README, `fork/RESTART.md`, `docs/cxx_importer.md`, `fork/PATCHES.md`, `fork/README.md`, `fork/THREE-SURFACES.md`, and `fork/getting-started.html` all carried "deferred to M22 / M24 / Phase 2" and "v1.02.0 + v1.03.0 published" framings dating to before v1.04.0. Brought current.
- **Fix `ra-release.yml`** — the workflow added in v1.07.0 never produced binaries because none of the stock nightlies match rust-analyzer's pinned-rustc expectations. Switch to RA's own CI recipe (`rustup-toolchain-install-master` at the SHA from RA's `rust-version` file + `RUSTC_BOOTSTRAP=1`). Prebuilt `rust-analyzer-rustcc-<triple>.tar.xz` binaries now ship alongside the toolchain tarballs.

Stage-1 toolchain binaries are still bit-for-bit identical to v1.06.0 / v1.07.0 (the rustc fork itself hasn't changed since v1.04.0's P09.50). The version bump exists so users can pin `rust-toolchain.toml` to a version that:
- includes the v2 roadmap (M22-M26)
- includes the DX layer (rustcc-cli, vscode-rustcc, JSON skip-emit)
- includes rust-analyzer Phase 2 (12-patch series for full IDE parity on class items)
- includes the **prebuilt** RA fork binary (new this release)

## What's next

`v1.09.0` — Windows MSVC C++ ABI support. Currently scoping (`fork/MSVC-PLAN.md` lands as the design doc). Phase 1 (mingw-w64 cross-target) is a 2–3 week stepping stone; Phase 2 (MSVC proper) is 3–4 months of focused work — new mangler module, new vtable layout, new record layout rules, SEH exception lowering in the fork rustc, MSVC parallel patch series, Windows CI + examples.

## Prebuilt binaries

Same 4 host triples as v1.04+. Each release now ships **two** sets of artifacts:
- `rustcc-<triple>.tar.xz` — fork rustc toolchain
- `rust-analyzer-rustcc-<triple>.tar.xz` — patched rust-analyzer (new in v1.08.0)

VS Code users can grab the RA binary with one command: `rustcc: Install RA Fork (latest)`. Otherwise:

```bash
TARGET=aarch64-apple-darwin
VERSION=v1.08.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"

curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
curl -fsSL -o ra.tar.xz     "$BASE/rust-analyzer-rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
tar -xJf ra.tar.xz     -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
# Point your editor at: $HOME/.rustcc/$VERSION/rust-analyzer-rustcc/rust-analyzer
```

Same caveat as previous releases — the macOS-13 Intel runner pool occasionally times out; if `x86_64-apple-darwin` is missing, source-build via `./fork/build.sh` and `./fork/ra-patches/build.sh`.
