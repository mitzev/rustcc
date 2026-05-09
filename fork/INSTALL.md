# Installing rustcc

Two paths, pick one:

- **Fast path** (~3 min): download a prebuilt tarball, extract,
  register with rustup. Works on x86_64/aarch64 macOS and Linux.
- **Source path** (~30-90 min): clone rust-lang/rust, apply the
  rustcc patch series, build stage-1 yourself. Works everywhere;
  needed if you want to modify the fork.

## Fast path

### 1. Download and extract

```bash
# Pick the triple for your host. Supported triples:
#   aarch64-apple-darwin        (Apple Silicon Mac)
#   x86_64-apple-darwin         (Intel Mac)
#   x86_64-unknown-linux-gnu    (most x86_64 Linux)
#   aarch64-unknown-linux-gnu   (arm64 Linux)
TARGET=aarch64-apple-darwin
VERSION=latest

# Resolve `latest` to the most recent release tag. Replace with a
# specific tag (e.g. `v1.07.0`) if you want to pin.
if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSL https://api.github.com/repos/rustcc/rustcc/releases/latest \
            | grep -m1 '"tag_name"' \
            | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
fi
echo "Installing rustcc $VERSION for $TARGET"

BASE="https://github.com/rustcc/rustcc/releases/download/$VERSION"
curl -fsSL -o rustcc.tar.xz         "$BASE/rustcc-$TARGET.tar.xz"
curl -fsSL -o rustcc.tar.xz.sha256  "$BASE/rustcc-$TARGET.tar.xz.sha256"

# Verify the tarball.
( shasum -a 256 --check rustcc.tar.xz.sha256 \
  || sha256sum --check rustcc.tar.xz.sha256 )

# Extract to ~/.rustcc/<version>/
INSTALL_DIR="$HOME/.rustcc/$VERSION"
mkdir -p "$INSTALL_DIR"
tar -xJf rustcc.tar.xz -C "$INSTALL_DIR"
```

### 2. Register with rustup

```bash
rustup toolchain link rustcc "$INSTALL_DIR/stage1"
rustc +rustcc --version   # should print "rustc 1.97.0-dev ..."
```

If you want rustcc to be the default toolchain:

```bash
rustup default rustcc
```

Otherwise prefix all commands with `+rustcc`:

```bash
cargo +rustcc build
cargo +rustcc test
```

### 3. Pin a Cargo project to the fork (optional but recommended)

Drop a `rust-toolchain.toml` at your project root:

```toml
[toolchain]
channel = "rustcc"
```

Now anyone who `cargo build`s the project uses the fork
automatically, the same way rust-toolchain.toml pins a stable /
nightly channel. `rustup show` confirms it.

## Source path

Requires ~10 GB free disk and 30–90 minutes (laptop) to 15+ hours
(Raspberry Pi). Use this path if:

- Your host triple isn't in the prebuilt list.
- You're modifying the fork itself.
- You don't trust prebuilt binaries.

```bash
git clone https://github.com/rustcc/rustcc.git
cd rustcc
./fork/build.sh
# ... 30-90 min elapses ...
rustup toolchain link rustcc "$HOME/rust-lang-rust-fork/build/host/stage1"
```

Under the hood, `build.sh` clones `rust-lang/rust` at the pinned
commit (`e22c616e4e87914135c1db261a03e0437255335e`), `git am`s the
patches in `fork/patches/`, and runs `./x.py build --stage 1`. To
skip the build and only verify the patches apply cleanly:

```bash
./fork/build.sh --apply-only
```

After a source build, `./fork/tests/run.sh` runs the class-keyword
regression probes against the stage-1 binary (~1 min). See
[`fork/tests/README.md`](tests/README.md).

## Rust-analyzer (for editor support)

rustcc's `class` keyword confuses stock rust-analyzer. A parser
fork that accepts `class` as a weak keyword lives under
[`fork/ra-patches/`](ra-patches/). See the [RA install
recipe](ra-patches/README.md) for the one-line `git am` + build +
point-your-editor-at-the-binary workflow.

## CI: install rustcc in GitHub Actions

Use the `install-rustcc` composite action from this repo:

```yaml
# .github/workflows/ci.yml in YOUR project
name: ci
on: [push, pull_request]

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: rustcc/rustcc/.github/actions/install-rustcc@main
        with:
          version: latest           # or a pinned tag like v1.07.0
          set-default: 'true'       # makes `cargo build` use rustcc
      - run: cargo build --workspace
      - run: cargo test --workspace
```

The action auto-detects your runner's OS+arch, downloads the
matching tarball, verifies its sha256, extracts it, and
`rustup toolchain link`s it as `rustcc`. If a prebuilt isn't
published for your runner, the action fails with a pointer to
the source path.

See [`fork/examples/ci-snippet.yml`](examples/ci-snippet.yml) for
a complete minimal example.

## Supported host triples

| Triple | Prebuilt | Source build |
|---|---|---|
| `aarch64-apple-darwin` | ✅ | ✅ |
| `x86_64-apple-darwin` | ✅ | ✅ |
| `x86_64-unknown-linux-gnu` | ✅ | ✅ |
| `aarch64-unknown-linux-gnu` | ✅ | ✅ |
| `i686-unknown-linux-gnu` | — | ✅ |
| `x86_64-pc-windows-*` | — | — (out of scope: fork is Itanium-only) |

Cross-compilation targets (ESP32-C3, STM32, Raspberry Pi Pico,
etc.) work from any supported host — see
[`fork/tests/run_targets.sh`](tests/run_targets.sh) for the
validated list.

## Uninstalling

```bash
rustup toolchain uninstall rustcc
rm -rf "$HOME/.rustcc"
```

## Troubleshooting

**"toolchain 'rustcc' is not installed"**: the `rustup toolchain
link` step didn't run or ran with a different name. Re-run it
and confirm with `rustup toolchain list`.

**"error: archive does not contain stage1/bin/rustc"**: the
tarball's internal layout didn't match what the install action
expected. File a bug against rustcc with the release tag and
host triple.

**Stage-1 build fails with "could not find `libc`"**: on some
Linux distros you need `sudo apt install libc6-dev` (or the
equivalent). `./fork/build.sh` doesn't install system deps — see
its header for the prereq list.

**`cargo +rustcc build` reports "the feature `rustc_attrs` is
internal"**: that's a warning, not an error. The fork intentionally
uses `rustc_attrs` for its custom attributes. Add
`#![allow(internal_features)]` to silence if it bothers you.
