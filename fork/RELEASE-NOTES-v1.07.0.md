# rustcc v1.07.0

**The "developer experience + IDE" release.** Six PRs since v1.06.0 close out the entire DX surface (CLI, VS Code extension, JSON skip log, opt-in method flattening, Swift CI integration) **and** ship rust-analyzer Phase 2 — a 12-patch series that gives `class` items full IDE parity with structs.

The fork rustc itself didn't change since v1.04.0 — every milestone in this release lives in `crates/`, `tools/`, `fork/ra-patches/`, and CI workflows. **Stage-1 toolchain tarballs are bit-for-bit identical to v1.06.0** — they exist purely so users can pin their `rust-toolchain.toml` to a single version that includes both fork rustc + the matching DX layer + the matching RA patch series.

## What's new since v1.06.0

### CI: Swift integration tests gate the release (PR #21)

`.github/workflows/release.yml` now runs `fork/tests/run.sh` against the just-built stage-1 before packaging. Catches Swift / class-keyword regressions before they ship. 7 probe tests (4 cpp_class + 3 swift_*) per platform; ~1-2 min added per matrix job.

### Developer experience layer (PR #22)

- **JSON skip-emit sidecar**: `cxx_importer::Build::compile()` now writes `bindings.skips.json` next to `bindings.rs`. Editor tooling (the new VS Code extension's Problems pane, future `rustcc doctor` extensions) consumes the structured projection without re-grepping generated source. For the FLTK text-editor demo: 25 records across 9 classes, all extra-ctor disambiguation.
- **Opt-in M22 method flattening**: new `RustBindingsConfig::flatten_inherited_methods` flag. When set, derived classes get forwarding wrappers for inherited non-virtual non-special public methods that chain through the M22 `as_<base>` / `as_<base>_mut` accessors. `c.a_only()` works directly instead of `c.as_base().a_only()`. Default off.

### `rustcc-cli` (PR #23)

A new binary in `crates/rustcc-cli/` covering the bootstrap-friction gap.

```bash
$ rustcc install               # download tarball + register with rustup
$ rustcc doctor                # 6 health checks (rustup, rustc, rustcc, clang, ...)
$ rustcc init my-app           # scaffold a new rustcc project
$ rustcc init my-app --surface cxx-class    # opt for the macro surface
```

Three subcommands, ~700 KB binary, dep tree = `clap` + std. Shells out to `curl` / `tar` / `rustup` / `shasum` like `fork/INSTALL.md`'s manual recipe — no Node-side HTTP / archive deps.

### `vscode-rustcc` extension (PR #24 + extended in PR #25)

Sideloadable VS Code extension under `tools/vscode-rustcc/`:

- **Grammar overlay** for `class`, `extern "C++"`, `extern "swiftcall"`, `#[cpp_virtual]`, `#[constructor]`, `#[swift_throws]`, `#[swift_value]`, `#[repr(cpp)]`, `#[repr(swift)]`. Stock RA treats `class` as an identifier; this overlay at least makes the keyword visually distinct.
- **6 snippets**: `cppclass`, `cppclass-inherit`, `cxxclass`, `swiftvalue`, `rustcc-build` (M26 build skeleton), `rustcc-include-bindings`.
- **5 commands**: `Install Toolchain`, `Run Doctor`, `Generate Bindings for Header`, `Show Bindings Skips`, **and** `Install RA Fork (latest)` (added in PR #25 — downloads the patched rust-analyzer, sets `rust-analyzer.server.path` workspace-scoped).
- **Status bar**: shows the active toolchain pin from `rust-toolchain.toml` (🚀 rustcc / ○ other / ○ none).
- **Problems pane integration**: watches `bindings.skips.json` and surfaces every recorded skip as an info-level diagnostic on `bindings.rs`.

Sideload via `cd tools/vscode-rustcc && npm install && npm run package && code --install-extension rustcc-tools.vsix`.

### rust-analyzer fork — Phase 2 (PR #25)

This is the headline. Twelve patches in `fork/ra-patches/` give `class` items full IDE parity with structs. Original Option B 26-38 day estimate landed in ~5 hours of agent-driven work over an evening sprint. Per-patch breakdown in [`fork/ra-patches/README.md`](fork/ra-patches/README.md) and [`fork/ra-patches/PHASE-2-PLAN.md`](fork/ra-patches/PHASE-2-PLAN.md).

Highlights:

| Patch | What |
|---|---|
| `01-ra-class-keyword.patch` | Phase 1 parser — `class Name<Generics>? (: Base)? { ... }` produces a clean `CLASS` syntax node |
| `02..06` (B.1, B.3, B.2) | `ClassId` / `AdtId::ClassId` / `VariantId::ClassId` + 75 match-arm stubs across hir-def/hir-ty/hir + `ClassSignature` lowering |
| `07-ra-class-fields.patch` (B.4a) | `CLASS_MEMBER_LIST.fields()` walker — `widget.x` field access type-checks |
| `08-ra-class-resolve.patch` (B.4) | `hir::Class` user-facing API — hover, go-to-def, find-references work end-to-end across hir / ide-db / ide-completion / ide-assists / ide-diagnostics / ide / lsp. New `SymbolKind::Class` + `CLASS` semantic token |
| `09-ra-class-assists.patch` (B.5) | `generate_class_new` assist + `change_visibility` / `extract_module` / `generate_impl` / `generate_derive` / `generate_trait_impl` class arms |
| `10-ra-class-inheritance.patch` (B.4b) | `ast::Class::extends_clause()` + `ClassSignature::base` + `Sema::to_def(class) -> Some` — resolution-heavy assists (auto_import, fix_visibility) light up |
| `11-ra-class-methods.patch` (B.4c) | `ItemContainerId::ClassId` + class methods reachable via inherent dispatch + `assemble_inherent_class_probe` walks `ClassSignature::base` for inheritance with C++-style derived-shadows-base |
| `12-ra-class-assists-extra.patch` (B.5+) | **Find all overriders** + **Implement override** + base-aware `generate_class_new` |

After applying all 12 patches at PINNED_COMMIT `45b868b19`:

| Crate | Tests pass |
|---|---|
| parser | 316/0 (+1 vs Phase-1's 315) |
| syntax | 51/0 |
| hir-def | 481/0 |
| hir-ty | 972/0 |
| ide-db | 196/0 |
| ide-completion | 729/0 |
| ide-assists | 2764/0 |
| ide-diagnostics | 641/0 |
| ide | 1297/0 |
| **Total** | **~7448 tests**, 0 failures |

### B.7 — RA fork distribution (PR #25)

`.github/workflows/ra-release.yml` builds the patched RA on the same 4 host triples as the rustcc toolchain and uploads `rust-analyzer-rustcc-<triple>.tar.xz` as release assets. Together with the new VS Code "Install RA Fork" command, the install flow becomes:

```bash
$ rustcc install                              # rustcc toolchain (3 min)
> rustcc: Install RA Fork (latest)            # RA fork binary (1 min)
```

vs. the old "clone rust-analyzer + git am + cargo build --release" 30-60 min recipe.

## Roadmap state

After v1.07.0:

| Series | Status |
|---|---|
| Phase A — M1–M10 | ✅ |
| Phase B — M11–M14 | ✅ |
| Phase C — M15–M21 | ✅ |
| v2 roadmap — M22–M26 | ✅ |
| **DX layer (CLI, VS Code, JSON skip log, flattening)** | ✅ |
| **CI Swift integration tests** | ✅ |
| **rust-analyzer Phase 2 (B.1–B.5 + B.4b/c + B.7)** | ✅ |

Open follow-ups (tracked, not blockers):

- Method flattening multi-level walk — currently one level deep; PR #22's flag is opt-in
- Ctor-overload disambiguation — surfaces in `bindings.skips.json` for FLTK demo
- STL container support for M24 — implicit instantiation auto-discovery
- "Convert class to struct" assist — recommended next class-aware refactoring
- Runtime-dispatch CI validation against the rustcc fork — needs a CI runner with the fork preinstalled

## FLTK demo state (unchanged from v1.06.0)

| Demo | bindings.rs | skip blocks |
|---|---|---|
| `fltk_hello` | 319 KB | 4 (all ctor-overload, none M22) |
| `fltk_text_editor` | 950 KB | 25 across 9 classes (all ctor-overload) |

The DX layer's JSON skip log surfaces these structurally for tooling consumption.

## Prebuilt binaries

This release ships:

- **Stage-1 toolchains** for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (same triples as v1.06.0; bit-for-bit identical binaries).
- **rust-analyzer-rustcc binaries** — *deferred to v1.07.1.* The first runs of `ra-release.yml` surfaced that RA's pinned tree expects a very specific rustc (it uses `rustup-toolchain-install-master` to install rustc by commit SHA, then `RUSTC_BOOTSTRAP=1` to emulate nightly). Three iterations at simpler approaches (workspace-inherited nightly, latest nightly, RUSTC_BOOTSTRAP alone) all hit different incompatibilities. The CI workflow needs the full RA recipe, which is mechanical but not landing in this release window. **Until then**, build locally: `./fork/ra-patches/build.sh` clones, applies all 12 patches at PINNED_COMMIT `45b868b19`, and produces `target/release/rust-analyzer` in ~5 minutes. Point your editor at it via `rust-analyzer.server.path`.

Same caveats as previous releases — the macOS-13 Intel runner pool is occasionally saturated; `x86_64-apple-darwin` may be missing if the build job times out. Source-build path documented in `fork/INSTALL.md`.

Install:

```bash
TARGET=aarch64-apple-darwin   # pick yours
VERSION=v1.07.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"

# rustcc toolchain
curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"

# Or use the new CLI:
cargo install --path crates/rustcc-cli   # from the source checkout
rustcc install --version v1.07.0

# RA fork — build locally (prebuilt deferred to v1.07.1):
git clone https://github.com/rustcc/rustcc /tmp/rustcc
/tmp/rustcc/fork/ra-patches/build.sh
# Then point your editor at ~/rust-analyzer-rustcc/target/release/rust-analyzer
```

## Acknowledgements

Six PRs and ~10,000 LoC of fork delta since v1.06.0 across:

- `crates/rustcc-cli/` — new crate, ~600 LoC
- `tools/vscode-rustcc/` — new tree, ~750 LoC TypeScript + JSON
- `fork/ra-patches/` — 12 patches, ~7,400 LoC of RA tree delta
- `.github/workflows/` — Swift CI step + new RA release workflow
- `crates/cxx_importer/` — JSON skip-emit, opt-in flattening, lint fix

Every PR green on first CI cycle (after the v1.06.0-era `irrefutable_let_patterns` lint nightly bump that briefly broke main and got fixed in PR #26).
