#!/usr/bin/env bash
# Bootstrap a rust-analyzer fork with the rustcc patches applied.
#
# Mirrors fork/build.sh's shape but for rust-analyzer. Clones at
# the pinned commit (PINNED_COMMIT in this directory), applies
# every `??-*.patch` in lexical order, optionally builds.
#
# Usage:
#   ./fork/ra-patches/build.sh                 # clone + apply + build
#   ./fork/ra-patches/build.sh --apply-only    # skip the cargo build
#
# Env:
#   RA_CLONE_DIR — where to put the working clone (default
#                  $HOME/rust-analyzer-rustcc)

set -euo pipefail

PATCHES_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLONE_DIR="${RA_CLONE_DIR:-$HOME/rust-analyzer-rustcc}"
PINNED_COMMIT="$(cat "$PATCHES_DIR/PINNED_COMMIT" | tr -d '[:space:]')"

apply_only=0
case "${1:-}" in
  --apply-only) apply_only=1 ;;
  -h|--help)
    sed -n '1,/^set -euo/p' "$0" | grep -E '^# ?' | sed 's/^# ?//'
    exit 0
    ;;
  "") : ;;
  *) echo "unknown arg: $1" >&2; exit 2 ;;
esac

if [[ ! -d "$CLONE_DIR/.git" ]]; then
  echo "==> cloning rust-analyzer to $CLONE_DIR"
  git clone --filter=blob:none https://github.com/rust-lang/rust-analyzer.git "$CLONE_DIR"
fi

cd "$CLONE_DIR"
echo "==> resetting to pinned commit $PINNED_COMMIT"
git fetch --depth=200 origin master
git checkout --quiet "$PINNED_COMMIT"

# `git am` wants a configured user. Set fork-local defaults if
# the user hasn't set their own; this won't override an existing
# global config.
if ! git config user.email >/dev/null 2>&1; then
  git config user.email "rustcc@localhost"
  git config user.name "rustcc-build"
fi

echo "==> applying patches from $PATCHES_DIR"
for patch in "$PATCHES_DIR"/[0-9][0-9]-*.patch; do
  [[ -e "$patch" ]] || continue
  echo "  -> $(basename "$patch")"
  git am "$patch"
done

if (( apply_only )); then
  echo
  echo "Patches applied. Skipping build (--apply-only)."
  exit 0
fi

echo "==> building rust-analyzer (release)"
cargo build --release -p rust-analyzer

BUILT="$CLONE_DIR/target/release/rust-analyzer"
echo
echo "Built: $BUILT"
echo "Point your editor at it via:"
echo "  rust-analyzer.server.path: $BUILT"
