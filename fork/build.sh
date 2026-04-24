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

set -euo pipefail

FORK_DIR="$(cd "$(dirname "$0")" && pwd)"
WS_ROOT="$(cd "$FORK_DIR/.." && pwd)"

# Full upstream SHA the patch series is authored against. GitHub's
# uploadpack only resolves full 40-char SHAs for fetch-by-sha, so a short
# prefix here will cause `git fetch` to fail.
PINNED_COMMIT="${PINNED_COMMIT:-e22c616e4e87914135c1db261a03e0437255335e}"

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

# 2. Apply the patch series in order. Use `git am --3way` so the
#    blob hashes embedded in the format-patch output drive a
#    three-way merge on any context drift. Patches 10+ were
#    generated against a cumulative local fork state, so strict
#    `git apply` can reject them even when the semantic diff
#    would apply cleanly. `--3way` handles that gracefully.
#
#    Git needs a user identity for `am` to produce commits;
#    seed one if the caller doesn't already have a `.gitconfig`
#    (happens in clean CI runners and fresh containers).
echo "==> applying rustcc patches"
(
  cd "$CLONE_DIR"
  if ! git config --get user.email >/dev/null 2>&1; then
    git config user.email "rustcc@localhost"
  fi
  if ! git config --get user.name >/dev/null 2>&1; then
    git config user.name "rustcc-build"
  fi
  for patch in "$FORK_DIR"/patches/*.patch; do
    echo "    apply $(basename "$patch")"
    if ! git am --3way "$patch"; then
      echo "==> patch $(basename "$patch") failed to apply; aborting" >&2
      git am --abort >/dev/null 2>&1 || true
      exit 1
    fi
  done
)

if [[ $APPLY_ONLY -eq 1 ]]; then
  echo "==> --apply-only set; skipping stage-1 build"
  exit 0
fi

# 4. Copy the stock config template and build.
if [[ ! -f "$CLONE_DIR/bootstrap.toml" ]]; then
  cp "$CLONE_DIR/bootstrap.example.toml" "$CLONE_DIR/bootstrap.toml"
  # rust-lang/rust's CI prunes `download-ci-llvm` artifacts for
  # older commits. Our pinned commit is old enough that the
  # prebuilt LLVM tarball has been deleted, so bootstrap falls
  # back to a 404 on every retry. Force-build LLVM from source
  # — adds ~30 min to a cold build but is the only path that
  # reliably works across rebase cycles. Also turn off
  # `assertions` for speed.
  python3 - "$CLONE_DIR/bootstrap.toml" <<'PY'
import sys, re
path = sys.argv[1]
with open(path) as f: text = f.read()
text = re.sub(r'^#?\s*assertions\s*=.*$', 'assertions = false', text, flags=re.M)
# Append a final `[llvm]` block that turns off download-ci-llvm.
# Template versions use dotted (`llvm.download-ci-llvm = ...`)
# or sectioned (`[llvm]\ndownload-ci-llvm = ...`) syntax, and
# the commented-out defaults don't match a single regex. TOML
# resolves later values last, so appending an explicit section
# at the end overrides any earlier setting regardless of form.
if not text.endswith('\n'):
    text += '\n'
text += (
    '\n# rustcc fork override — see fork/build.sh for why the\n'
    '# pinned upstream commit can no longer use the CI LLVM.\n'
    '[llvm]\n'
    'download-ci-llvm = false\n'
)
with open(path, 'w') as f: f.write(text)
PY
fi

# If bootstrap.toml existed before build.sh ran (e.g. leftover
# from a previous invocation), the block above is skipped — but
# we still need to guarantee the LLVM override. Append the same
# block idempotently every run.
if ! grep -q "^# rustcc fork override" "$CLONE_DIR/bootstrap.toml"; then
  {
    printf '\n# rustcc fork override — see fork/build.sh for why the\n'
    printf '# pinned upstream commit can no longer use the CI LLVM.\n'
    printf '[llvm]\n'
    printf 'download-ci-llvm = false\n'
  } >> "$CLONE_DIR/bootstrap.toml"
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
