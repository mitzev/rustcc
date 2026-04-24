# Release notes template

Starter text for the body of a GitHub Release. The release
workflow creates draft releases with a minimal auto-generated
body; replace that body with something shaped like this when
promoting the draft to public.

---

## rustcc vX.Y.Z

**Brief summary** — one sentence on what changed since the last
release. Example: "Ships 1.02: rust-analyzer fork for the `class`
keyword (Phase 1) and the `#[swift_value]` built-in attribute
macro."

### What's new

- `<P09.NN>` — headline change (one per bullet). Keep the patch
  number so users can cross-reference `fork/PATCHES.md`.
- ...

### Breaking changes

- `<if any>` — note the user-visible migration path. Rare; the
  fork's semver-ish policy is to deprecate-then-remove across
  at least one release cycle.

### Prebuilt binaries

This release ships stage-1 toolchains for:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

Install: see [`fork/INSTALL.md`](fork/INSTALL.md) for the
curl-and-extract recipe. TL;DR:

```bash
TARGET=aarch64-apple-darwin   # pick yours
VERSION=vX.Y.Z
BASE="https://github.com/rustcc/rustcc/releases/download/$VERSION"
curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

### CI integration

```yaml
- uses: rustcc/rustcc/.github/actions/install-rustcc@main
  with:
    version: vX.Y.Z
```

### Patch series

Applies against `rust-lang/rust` commit `<pinned SHA from
fork/build.sh>`. Rebuilding from source:

```bash
git clone https://github.com/rustcc/rustcc.git
cd rustcc && git checkout vX.Y.Z
./fork/build.sh
```

### Known issues

- `<list anything users should know about, e.g. a target that
  regressed or a test that's flaky>`
- Nothing? Then delete this section.

### Validation

- rustcc workspace: `cargo test --workspace` → <count>/0
- In-tree probes: `RUSTC=<stage1> ./fork/tests/run.sh` → N/N
- Cross-target (optional): `./fork/tests/run_targets.sh` → M/M

### Contributors

`git log vA.B.C..vX.Y.Z --format='%an' | sort -u` — list
contributors to this release. Co-authored-by lines count.
