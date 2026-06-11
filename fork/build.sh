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
#
# v1.13.8: the fork is now based on **Rust 1.96.0 stable** (the current
# official release) rather than a 1.97.0-dev master snapshot — see
# fork/MIGRATION-1.96.0.md. To build against the old 1.97-dev base,
# `PINNED_COMMIT=e22c616e4e87914135c1db261a03e0437255335e PATCHES_DIR=patches-1.97dev ./fork/build.sh`.
PINNED_COMMIT="${PINNED_COMMIT:-ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96}"
PATCHES_DIR="${PATCHES_DIR:-patches}"

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
  # Fetch the pin with FULL ancestry (the blob:none filter from the
  # clone keeps it cheap — trees/commits only). The pin sits on a
  # release branch, not master; a --depth 1 fetch would leave
  # bootstrap unable to walk history to the LLVM-bump commit, so
  # `download-ci-llvm` resolves a non-served commit and 404s.
  git fetch origin "$PINNED_COMMIT" 2>/dev/null \
    || git fetch origin "$PINNED_COMMIT" --depth 1 2>/dev/null \
    || true
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
  for patch in "$FORK_DIR"/"$PATCHES_DIR"/*.patch; do
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
#
# LLVM source, in priority order:
#   1. RUSTCC_LLVM_CONFIG=<path to llvm-config>  — link a SYSTEM LLVM
#      (>= 21; e.g. apt.llvm.org's llvm-config-21). Deterministic and
#      fast; the right choice on CI runners.
#   2. RUSTCC_DOWNLOAD_CI_LLVM=1 — bootstrap's download-ci-llvm.
#      Works only when the git ancestry resolves a SERVED commit;
#      for stable-tag pins the heuristic lands on release-branch
#      backports the artifact bucket never carries, so don't rely on
#      this outside trees where it's been seen to work.
#   3. default — build LLVM from source (slow but never bit-rots).
: "${RUSTCC_DOWNLOAD_CI_LLVM:=0}"
: "${RUSTCC_LLVM_CONFIG:=}"
if [[ ! -f "$CLONE_DIR/bootstrap.toml" ]]; then
  cp "$CLONE_DIR/bootstrap.example.toml" "$CLONE_DIR/bootstrap.toml"
  python3 - "$CLONE_DIR/bootstrap.toml" "$RUSTCC_DOWNLOAD_CI_LLVM" "$RUSTCC_LLVM_CONFIG" "$(uname -m)" <<'PY'
import sys, re
path = sys.argv[1]
download_ci_llvm = 'true' if sys.argv[2] == '1' else 'false'
llvm_config = sys.argv[3]
machine = sys.argv[4]
# Explicit UTF-8 encoding on both read + write — Python on Windows
# defaults to cp1252 which encodes ASCII-only safely but emits
# Latin-1 bytes for any non-ASCII character. bootstrap.py opens
# this file expecting UTF-8 and chokes on the cp1252 bytes;
# ASCII-only content sidesteps the issue.
with open(path, encoding='utf-8') as f: text = f.read()
text = re.sub(r'^#?\s*assertions\s*=.*$', 'assertions = false', text, flags=re.M)
# Append a final `[llvm]` block that turns off download-ci-llvm.
# Template versions use dotted (`llvm.download-ci-llvm = ...`)
# or sectioned (`[llvm]\ndownload-ci-llvm = ...`) syntax, and
# the commented-out defaults don't match a single regex. TOML
# resolves later values last, so appending an explicit section
# at the end overrides any earlier setting regardless of form.
#
# Keep this block ASCII-only (no em-dashes, smart quotes, etc.)
# so it round-trips through Windows' cp1252-default fs.
if not text.endswith('\n'):
    text += '\n'
text += (
    '\n# rustcc fork override (Rust 1.96.0 base).\n'
    '# 1.96.0 is a *stable* channel, which forbids `#![feature(...)]`;\n'
    '# the fork needs `feature(rustc_attrs)`, so force the nightly\n'
    '# channel on the built toolchain.\n'
    '[llvm]\n'
    'download-ci-llvm = ' + download_ci_llvm + '\n'
    '[rust]\n'
    'channel = "nightly"\n'
)
if llvm_config:
    # System LLVM: highest priority. Only Linux triples are needed
    # here (the CI runners); macOS/local builds use the defaults.
    # The template already declares `[target.<triple>]` (TOML forbids
    # redefining a table), so insert into the existing section when
    # present, else append a new one.
    triple = machine + '-unknown-linux-gnu'
    header = '[target.' + triple + ']'
    line = 'llvm-config = "' + llvm_config + '"'
    if re.search('^' + re.escape(header) + '$', text, flags=re.M):
        text = re.sub(
            '^' + re.escape(header) + '$',
            header + '\n' + line,
            text,
            count=1,
            flags=re.M,
        )
    else:
        text += header + '\n' + line + '\n'
with open(path, 'w', encoding='utf-8') as f: f.write(text)
PY
fi

# If bootstrap.toml existed before build.sh ran (e.g. leftover
# from a previous invocation), the block above is skipped — but
# we still need to guarantee the LLVM override. Append the same
# block idempotently every run.
if ! grep -q "^# rustcc fork override" "$CLONE_DIR/bootstrap.toml"; then
  # ASCII-only — see Windows-cp1252 caveat in the python block above.
  {
    printf '\n# rustcc fork override (v1.13.8, Rust 1.96.0 base).\n'
    printf '# CI LLVM artifact for this commit is pruned (404) -> build\n'
    printf '# LLVM from source; stable channel forbids feature gates, so\n'
    printf '# force nightly for the toolchain.\n'
    printf '[llvm]\n'
    printf 'download-ci-llvm = false\n'
    printf '[rust]\n'
    printf 'channel = "nightly"\n'
  } >> "$CLONE_DIR/bootstrap.toml"
fi

echo "==> stage-1 build (expect 30-90 min)"
(
  cd "$CLONE_DIR"
  # `library` (which builds the compiler first) — a bare `compiler`
  # build leaves the stage1 sysroot without host std, so driving
  # cargo with RUSTC=<stage1> fails E0463 on every host compile.
  ./x.py build --stage 1 library
)

# 5. Print the path to the freshly-built rustc.
BUILT="$(ls -d "$CLONE_DIR"/build/*/stage1/bin/rustc 2>/dev/null | head -1)"
if [[ -n "$BUILT" ]]; then
  echo
  echo "==> fork built: $BUILT"
  echo

  # Install the Rust LLDB pretty-printers into the stage-1 sysroot.
  # A bare stage-1 build doesn't copy these (upstream only does it
  # during `dist`), so debuggers — CodeLLDB, `rust-lldb` — can't find
  # the Rust data formatters and warn "Could not find LLDB data
  # formatters in your Rust toolchain". Copy them from the source tree
  # so the fork toolchain is debuggable out of the box.
  STAGE1_SYSROOT="$(dirname "$(dirname "$BUILT")")"
  ETC_DEST="$STAGE1_SYSROOT/lib/rustlib/etc"
  if [[ -f "$CLONE_DIR/src/etc/lldb_lookup.py" ]]; then
    mkdir -p "$ETC_DEST"
    cp "$CLONE_DIR"/src/etc/lldb_lookup.py \
       "$CLONE_DIR"/src/etc/lldb_commands \
       "$CLONE_DIR"/src/etc/lldb_providers.py \
       "$CLONE_DIR"/src/etc/lldb_batchmode.py \
       "$ETC_DEST/" 2>/dev/null || true
    echo "==> installed Rust LLDB formatters into $ETC_DEST"
    echo
  fi

  echo "To use it with the rustcc workspace:"
  echo "  RUSTC=\"$BUILT\" cargo test --workspace"
else
  echo "build appears to have finished but stage1 rustc not found; check $CLONE_DIR/build/" >&2
  exit 1
fi
