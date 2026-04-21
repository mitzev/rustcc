#!/usr/bin/env bash
# Clone rust-lang/rust at our pinned commit, apply the rustcc patch
# series, and build a stage-1 forked rustc.
#
# Usage:
#   ./fork/build.sh              # full flow: clone + apply + build
#   ./fork/build.sh --apply-only # skip the stage-1 build (handy
#                                # for CI that only wants to confirm
#                                # the patches apply cleanly)
#
# Prerequisites:
#   - A linux or macOS host with enough disk (~10 GB).
#   - git, python3, cmake, ninja, clang/clang++ on PATH.
#   - The workspace this script lives in must already have the
#     rustcc sub-crates built so far (rustc_abi_cxx etc.); the
#     layout bridge (P06) pulls them in via path-dep.

set -euo pipefail

FORK_DIR="$(cd "$(dirname "$0")" && pwd)"
WS_ROOT="$(cd "$FORK_DIR/.." && pwd)"

# The nightly we ship against. Update when changing rust-toolchain.
PINNED_COMMIT="${PINNED_COMMIT:-5c7ae0c7e}"

CLONE_DIR="${CLONE_DIR:-$HOME/rust-lang-rust-fork}"
APPLY_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --apply-only) APPLY_ONLY=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# 1. Clone (or update) rust-lang/rust at the pinned commit.
if [[ ! -d "$CLONE_DIR/.git" ]]; then
  echo "==> cloning rust-lang/rust into $CLONE_DIR"
  git clone --filter=blob:none https://github.com/rust-lang/rust.git "$CLONE_DIR"
fi
(
  cd "$CLONE_DIR"
  echo "==> checking out $PINNED_COMMIT"
  git fetch origin "$PINNED_COMMIT" --depth 1 2>/dev/null || true
  git checkout "$PINNED_COMMIT"
)

# 2. Apply the patch series in order. `git apply --check` first so a
#    failure is loud and doesn't leave the tree half-patched.
echo "==> applying rustcc patches"
(
  cd "$CLONE_DIR"
  for patch in "$FORK_DIR"/patches/*.patch; do
    echo "    apply $(basename "$patch")"
    git apply --check "$patch"
  done
  for patch in "$FORK_DIR"/patches/*.patch; do
    git apply "$patch"
  done
)

# 3. Vendor rustc_abi_cxx into compiler/ so P07's layout bridge can
#    path-depend on it. Step is idempotent — re-vendors on every run
#    so the two trees stay in sync during active development.
if [[ -f "$FORK_DIR/patches/07-ty-utils-dep.patch" ]]; then
  "$FORK_DIR/scripts/vendor_abi_cxx.sh" "$CLONE_DIR" "$WS_ROOT"
fi

if [[ $APPLY_ONLY -eq 1 ]]; then
  echo "==> --apply-only set; skipping stage-1 build"
  exit 0
fi

# 4. Copy the stock config template and build.
if [[ ! -f "$CLONE_DIR/bootstrap.toml" ]]; then
  cp "$CLONE_DIR/bootstrap.example.toml" "$CLONE_DIR/bootstrap.toml"
  # Enable stage-1 build + LLVM-asserts off for speed.
  python3 - "$CLONE_DIR/bootstrap.toml" <<'PY'
import sys, re
path = sys.argv[1]
with open(path) as f: text = f.read()
# Flip a couple of keys if present; leave the rest at template defaults.
text = re.sub(r'^#?\s*assertions\s*=.*$', 'assertions = false', text, flags=re.M)
with open(path, 'w') as f: f.write(text)
PY
fi

echo "==> stage-1 build (expect 30-90 min)"
(
  cd "$CLONE_DIR"
  ./x.py build --stage 1 compiler
)

# 5. Print the path to the freshly-built rustc.
BUILT="$(ls -d "$CLONE_DIR"/build/*/stage1/bin/rustc 2>/dev/null | head -1)"
if [[ -n "$BUILT" ]]; then
  echo
  echo "==> fork built: $BUILT"
  echo
  echo "To use it with the rustcc workspace:"
  echo "  RUSTC=\"$BUILT\" cargo test --workspace"
else
  echo "build appears to have finished but stage1 rustc not found; check $CLONE_DIR/build/" >&2
  exit 1
fi
