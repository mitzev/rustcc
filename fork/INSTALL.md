# Installing rustcc

Two paths, pick one:

- **Fast path** (~3 min): download a prebuilt tarball, extract,
  register with rustup. Works on x86_64/aarch64 macOS and Linux.
- **Source path** (~30-90 min): clone rust-lang/rust, apply the
  rustcc patch series, build stage-1 yourself. Works everywhere;
  needed if you want to modify the fork.

## Prerequisites by platform

Both paths need `rustup` (the standard rust toolchain manager) plus
`curl` and `tar`/`xz`. The **source path** additionally needs a host
C/C++ toolchain, `cmake`, `git`, `python3`, `pkg-config`, and
`libssl-dev`. The **C++ interop examples** (FLTK, fmtlib) want
`libclang` (for `cxx_importer`) and the relevant native library.

Pick the one-liner for your OS:

### macOS (Apple Silicon or Intel)

```bash
# rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Source-path build prereqs (Xcode CLT covers clang + git + make)
xcode-select --install
brew install cmake pkg-config xz

# Optional: C++ interop demos
brew install llvm fltk        # llvm gives libclang for cxx_importer
```

`brew install llvm` exposes libclang at
`$(brew --prefix llvm)/lib/libclang.dylib`. If `cxx_importer`'s build
script can't find it, set `LIBCLANG_PATH=$(brew --prefix llvm)/lib`.

### Debian / Ubuntu

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Fast-path needs: curl, xz-utils (usually preinstalled)
sudo apt-get update
sudo apt-get install -y curl xz-utils

# Source-path additional prereqs
sudo apt-get install -y \
  build-essential cmake pkg-config libssl-dev git python3

# Optional: C++ interop demos
sudo apt-get install -y libclang-dev libfltk1.3-dev
```

### Fedora / RHEL / Rocky

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

sudo dnf install -y \
  gcc gcc-c++ cmake pkgconf-pkg-config openssl-devel git python3 \
  xz curl

# Optional: C++ interop demos
sudo dnf install -y clang-devel fltk-devel
```

### Arch Linux

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

sudo pacman -S --needed \
  base-devel cmake pkgconf openssl git python xz curl

# Optional: C++ interop demos
sudo pacman -S --needed clang fltk
```

### Alpine

```bash
apk add curl xz tar
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Source-path additional prereqs
apk add build-base cmake pkgconfig openssl-dev git python3 \
        linux-headers musl-dev

# Optional: C++ interop demos
apk add clang-dev fltk-dev
```

Note: prebuilt rustcc tarballs are built against glibc. On Alpine
(musl) you'll typically want the **source path** unless you're
running glibc-compat shims.

### Windows

Native Windows MSVC C++ ABI support is shipped and **prebuilt
tarballs are published** for `x86_64-pc-windows-msvc` and
`aarch64-pc-windows-msvc` (fast path below). The fork rustc emits real
PE32+ binaries that link against the MSVC CRT and route through
`??_7Class@@6B@` vftables + `??_G` scalar-deleting dtors, with SEH
funclet exception handling at runtime.

For Mac/Linux hosts cross-compiling to MSVC, see
[`fork/CROSS-COMPILE-MSVC.md`](CROSS-COMPILE-MSVC.md) for the `xwin`
+ `lld-link` + Wine toolchain setup. (Editor support — the prebuilt
`rust-analyzer-rustcc` — currently ships for macOS/Linux hosts only;
on Windows build the patched RA from source.)

## Fast path

### 1. Download and extract

```bash
# Pick the triple for your host. Prebuilt triples:
#   aarch64-apple-darwin        (Apple Silicon Mac)
#   x86_64-unknown-linux-gnu    (most x86_64 Linux)
#   aarch64-unknown-linux-gnu   (arm64 Linux)
#   x86_64-pc-windows-msvc      (Windows x64)
#   aarch64-pc-windows-msvc     (Windows arm64)
# (Intel Mac, x86_64-apple-darwin, is source-build only — see below.)
TARGET=aarch64-apple-darwin
VERSION=latest

# Resolve `latest` to the most recent release tag. Replace with a
# specific tag (e.g. `v1.13.3`) if you want to pin.
if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSL https://api.github.com/repos/mitzev/rustcc/releases/latest \
            | grep -m1 '"tag_name"' \
            | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
fi
echo "Installing rustcc $VERSION for $TARGET"

BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"
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
git clone https://github.com/mitzev/rustcc.git
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

rustcc's `class` keyword confuses stock rust-analyzer. A patched
RA that accepts `class` and gives it full IDE parity with structs
lives under [`fork/ra-patches/`](ra-patches/) (12 patches).

**Fast path** — download the prebuilt tarball (macOS/Linux hosts):

```bash
TARGET=aarch64-apple-darwin   # pick yours; same triples as rustcc
VERSION=v1.13.3
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"
curl -fsSL -o ra.tar.xz "$BASE/rust-analyzer-rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf ra.tar.xz -C "$HOME/.rustcc/$VERSION"

# Point your editor at:
#   $HOME/.rustcc/$VERSION/rust-analyzer-rustcc/rust-analyzer
# VS Code (settings.json):
#   "rust-analyzer.server.path": "/Users/you/.rustcc/v1.13.3/rust-analyzer-rustcc/rust-analyzer"
```

VS Code users with the rustcc extension installed (see below) can
skip the curl/tar dance: `Cmd-Shift-P → rustcc: Install RA Fork
(latest)` does the download + `rust-analyzer.server.path` wiring in
one command.

**Source path** — build the patched RA yourself (~5 min):

```bash
./fork/ra-patches/build.sh
# Produces $HOME/rust-analyzer-rustcc/target/release/rust-analyzer
```

See [`fork/ra-patches/README.md`](ra-patches/README.md) for the
per-patch breakdown.

## Developer tooling (optional but recommended)

Three pieces of optional tooling:

### `rustcc-cli`

CLI wrapper that bundles the install + doctor + project-scaffolding
workflow. From a source checkout:

```bash
cargo install --path crates/rustcc-cli
rustcc install              # download tarball + register with rustup
rustcc doctor               # 6 health checks
rustcc init my-app          # scaffold a new rustcc project
rustcc init my-app --surface cxx-class   # opt for the macro surface
```

Dep tree is just `clap` + std; shells out to `curl` / `tar` /
`rustup` / `shasum` (same recipe as the fast path above). No Node /
no extra runtime.

### `vscode-rustcc` extension

Sideloadable VS Code extension under `tools/vscode-rustcc/`:

```bash
cd tools/vscode-rustcc
npm install
npm run package
code --install-extension rustcc-tools-*.vsix
```

Provides: grammar overlay for `class` / `extern "C++"` /
`extern "Swift"` / rustcc attributes, snippets, commands
(including **Install RA Fork (latest)**), a status-bar pin
indicator, and Problems-pane diagnostics fed by
`bindings.skips.json`.

### Prebuilt rust-analyzer fork binary

Covered in the [Rust-analyzer](#rust-analyzer-for-editor-support)
section above — ships alongside the rustcc tarball on every release
(macOS/Linux hosts).

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
      - uses: mitzev/rustcc/.github/actions/install-rustcc@main
        with:
          version: latest           # or a pinned tag like v1.13.3
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
| `x86_64-apple-darwin` | — (build from source) | ✅ |
| `x86_64-unknown-linux-gnu` | ✅ | ✅ |
| `aarch64-unknown-linux-gnu` | ✅ | ✅ |
| `i686-unknown-linux-gnu` | — | ✅ |
| `x86_64-pc-windows-gnu` | — | ✅ (mingw-w64, Itanium ABI) |
| `x86_64-pc-windows-msvc` | ✅ | ✅ |
| `aarch64-pc-windows-msvc` | ✅ | ✅ |

Notes:
- `x86_64-apple-darwin` (Intel Mac) is **source-build only** — its
  prebuilt is not published (Intel-mac release runners are
  unreliable). Apple Silicon (`aarch64-apple-darwin`) has a prebuilt.
- Prebuilt **`rust-analyzer-rustcc`** ships for the three macOS/Linux
  prebuilt triples; on Windows, build the patched RA from source.

Cross-compilation targets (ESP32-C3, STM32, Raspberry Pi Pico,
etc.) work from any supported host — the C++ ABI is derived from the
session `--target`, not the build host. See
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
internal"**: only applies to pre-v1.14 toolchains. Since v1.14 the
fork's interop attributes are ungated — remove any leftover
`#![feature(rustc_attrs)]` / `#![allow(internal_features)]` from your
crate roots. On older toolchains, keep both attributes.

**"libclang.so/dylib not found"** when building `cxx_importer` or
running the FLTK demos: install libclang for your OS (see
[Prerequisites by platform](#prerequisites-by-platform)) and, if
the build script still can't locate it, set `LIBCLANG_PATH`
explicitly:

```bash
# macOS Homebrew
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"
# Debian/Ubuntu
export LIBCLANG_PATH=/usr/lib/llvm-14/lib   # adjust version
# Fedora
export LIBCLANG_PATH=/usr/lib64
```

**"rust-analyzer doesn't recognize `class`"**: you're still on
stock RA. Install the prebuilt RA fork binary (see
[Rust-analyzer](#rust-analyzer-for-editor-support)) and confirm
`rust-analyzer --version` reports a build from the rustcc tree.
