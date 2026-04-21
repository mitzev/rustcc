# Repository layout

rustcc ships as **two linked repositories** rather than a monorepo.
This doc explains the split, what lives where, and how the two
repos are kept in sync.

## The two repos

### 1. `rustcc` (this repo) — workspace

Home for everything that is *not* the compiler fork itself:

- **`crates/`** — the support workspace: `rustc_abi_cxx`,
  `rustcc_attr`, `rustcc_codegen`, `rustcc_macros`, `cxx` runtime
  types, the `rustcc` build driver. Predates the fork; still
  exercised by integration tests and used by the forwarders
  path.
- **`fork/`** — the *spec* of the fork. Contains:
  - `PATCHES.md` — authoritative narrative + per-patch history.
  - `patches/NN-<slug>.patch` — human-readable narrative patches
    documenting each P09.* milestone. These are documentation,
    not code; the actual git commits on the compiler-fork repo
    are the authoritative source.
  - `build.sh` — clones the compiler fork at the pinned nightly
    and runs `./x.py build --stage 1`.
  - `getting-started.html` — user-facing guide.
- **`examples/`** — runnable end-to-end demos.
- **`docs/`** — per-crate design docs (ABI, codegen, ownership,
  etc.).

### 2. `rustcc-rustc` (separate repo) — the actual compiler fork

A branch off `rust-lang/rust` at the pinned nightly, with the
patches applied as real git commits on a branch. This is what
users build to get a working forked `rustc`.

- Contains the full rustc source tree with the P09.22–P09.36
  changes committed directly.
- Kept on a long-lived branch (e.g. `rustcc/main`) off the pinned
  upstream nightly.
- Tagged at each milestone (`v1.0`, etc.).

The compiler fork lives in a separate repo — not a submodule —
because:

1. Cloning the workspace should be fast. The rustc tree is
   gigabytes; the workspace is megabytes.
2. Most workspace contributors never touch the fork. Splitting
   keeps the compiler tree out of their clone and history.
3. Upstream rebases are a compiler-fork-only concern and should
   not churn the workspace repo's history.
4. CI for the two repos has wildly different shape: workspace
   CI is fast (cargo test); compiler CI is slow (stage-1 build +
   test suite on multiple targets).

## How the two stay in sync

- `fork/patches/*.patch` in the workspace repo is the **narrative
  spec**: what the fork changes and why. Each patch maps 1:1 to a
  commit on `rustcc-rustc`.
- `fork/PATCHES.md` appends a section per milestone. It is the
  authoritative history for the project.
- When a patch lands on `rustcc-rustc`:
  1. The commit on `rustcc-rustc` is the source of truth for
     code.
  2. A matching `fork/patches/NN-<slug>.patch` (narrative form)
     and `fork/PATCHES.md` entry land in the workspace repo.
  3. `fork/build.sh` is updated if the pinned nightly or clone
     arguments change.
- Users build the compiler via `fork/build.sh`, which clones
  `rustcc-rustc` at the pinned tag and runs stage-1.

## Upstream rebase policy

- `rustcc-rustc` rebases onto a new upstream nightly at a
  cadence of the project's choosing (not every nightly — too
  much churn).
- Rebases are batched per milestone: pick a nightly, rebase the
  feature branch, re-run VERIFY.md, tag, update the workspace
  `fork/build.sh` pin.
- If a patch conflicts with upstream, the narrative patch file
  in the workspace repo is updated alongside.

## What ships where — quick reference

| Artifact | Repo | Notes |
|---|---|---|
| rustc fork source | `rustcc-rustc` | Branch off upstream nightly |
| Pinned upstream nightly | `fork/build.sh` (workspace) | Version bump on rebase |
| Narrative patch files | `fork/patches/` (workspace) | 1:1 with compiler commits |
| PATCHES.md history | `fork/PATCHES.md` (workspace) | Authoritative narrative |
| Workspace support crates | `crates/` (workspace) | — |
| User guide | `fork/getting-started.html` (workspace) | — |
| Examples | `examples/` (workspace) | Require forked rustc to build |
| Per-crate design docs | `docs/` (workspace) | — |

## FAQ

**Why not a git submodule?**
A submodule would pin exactly one upstream SHA into the workspace
repo's history, forcing a workspace commit for every rebase.
Decoupling is cheaper: `fork/build.sh` pins via a nightly name
and a tag on `rustcc-rustc`.

**Can I hack on the compiler from the workspace clone?**
No — the workspace doesn't contain the rustc tree. Clone
`rustcc-rustc` separately, or run `./fork/build.sh` once, which
leaves a buildable tree under `$CLONE_DIR` (defaults to
`$HOME/rust-lang-rust-fork`).

**What if I want just the workspace, no fork?**
`crates/` and the `emit-forwarders` mode of the `rustcc` driver
build against stable rustc today. The forwarders path covers the
v1 subset and does not require the fork.
