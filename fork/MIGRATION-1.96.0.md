# Migrating the rustcc fork to Rust 1.96.0

This documents a completed, validated rebase of the fork patch series
from the 1.97.0-dev master snapshot onto the **1.96.0 stable** release.

**As of v1.13.8 this is the active base.** The 42-patch 1.96.0 series
is now [`fork/patches/`](patches/) (what `fork/build.sh` applies by
default); the previous 41-patch 1.97-dev series is archived under
[`fork/patches-1.97dev/`](patches-1.97dev/). To build against the old
1.97-dev base, see "How to build on 1.97-dev" below.

## Direction note

The fork was developed against **1.97.0-dev** (master commit
`e22c616e4e87914135c1db261a03e0437255335e`, 2026-04-19 — the
`introduce-unnormalized` commit `#155083`). **1.96.0 stable**
(`ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96`, released 2026-05-25)
branched from master earlier and is therefore a *sibling, slightly
older* lineage. v1.13.8 pins 1.96.0 deliberately: it is the **current
official stable release**, so rustcc tracks a real, reproducible
toolchain that downstream consumers can pin until 1.97.0 ships
(≈early July), at which point re-pinning to 1.97 is the cheaper target
since the 1.97-dev series is preserved under `fork/patches-1.97dev/`.

## How to build (default — 1.96.0)

`fork/build.sh` already defaults to the 1.96.0 base and applies
`fork/patches/`; a plain `./fork/build.sh` is all that's needed. It sets
`PINNED_COMMIT=ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96` and
`PATCHES_DIR=patches`.

### How to build on 1.97-dev

```sh
PINNED_COMMIT=e22c616e4e87914135c1db261a03e0437255335e \
  PATCHES_DIR=patches-1.97dev ./fork/build.sh
```

The 1.96.0 checkout's `bootstrap.toml` needs two settings the master
pin doesn't (`fork/build.sh` now writes both automatically):

```toml
[rust]
channel = "nightly"          # 1.96.0 is a *stable* tag; the fork needs
                             # `#![feature(rustc_attrs)]`, forbidden on stable
[llvm]
download-ci-llvm = false     # rust-lang CI prunes the LLVM artifact for
                             # older commits; even this release commit 404s,
                             # so build LLVM from source (same as the master pin)
```

All 42 patches apply cleanly via `git am --3way` onto the 1.96.0 tag
(verified on a fresh checkout).

## What changed vs the 1.97 series

The migration needed **3 conflict resolutions** (folded into their
patches) and **one API-drift commit** (patch 42). Everything else
applied unchanged — notably `ItemKind::Class`, the codegen vtable/ctor
hooks, and most of the C++-exceptions series.

| Area | 1.96.0 vs 1.97 difference | Fix |
|---|---|---|
| `rustc_feature` `builtin_attrs.rs` | `rustc_attr!` is 5-arg (`name, type, template, dup, encode_cross_crate, desc`); simplified to 2-arg after 1.96 | Added a 2-arg compatibility arm (permissive `Word`+`NameValueStr` template, `EncodeCrossCrate::Yes`); the new-system parser governs real validation (old template check is skipped for parsed attrs) |
| `rustc_attr_parsing` `codegen_attrs.rs` | `NoArgsAttributeParser` requires `ON_DUPLICATE` (defaulted later) | `rustcc_noargs_attr!` sets `ON_DUPLICATE = OnDuplicate::Error` |
| `rustc_ty_utils` `abi.rs`, `rustc_symbol_mangling` `itanium.rs`/`swift.rs` | `ty::FnSig::abi`/`c_variadic` are **fields** (became methods after 1.96) | Field access instead of method calls; kept the fork's `effective_abi` |
| `rustc_codegen_ssa` `mir/block.rs` | `mk_fn_sig` is 5-arg; `FnSig` exposes `c_variadic`/`safety`/`abi` (1.97 bundled into `fn_sig_kind` + 3-arg ctor) | 5-arg `mk_fn_sig`, preserve variadic/safety/abi |
| `rustc_passes` `check_attr.rs` | tail-of-file layout (`check_duplicates`) | keep both 1.96.0's fn and the fork's `cxx_*` helpers |
| `rustc_mir_transform` `cxx_throws_wrap.rs` | `From` is a **diagnostic item**, not `LangItem::From` (promoted later) | `get_diagnostic_item(sym::From)` |

No `Unnormalized` revert was needed: the v1.13.7 vtable/dtor code already
uses `Ty::new_adt` rather than `type_of().instantiate_identity()`.

## Validation (on `aarch64-apple-darwin`)

- stage1 `rustc 1.96.0-nightly (669deee97 2026-06-03)` builds clean.
- `./fork/tests/run.sh` — **9/9 pass**.
- `examples/subclass_cpp_base` (Rust subclass of an imported C++ base
  **with a virtual destructor**) — green: `foo=105 describe=205
  only_mine=15 | base_ctor=1 base_dtor=1 derived_drop=1`
  (no leak / no double-free across the boundary).

## Effort

Matched the estimate: 3 conflict resolutions + 5 small build-drift fixes
across 5 files, ~5 incremental build cycles. The C++-exceptions series
and `ItemKind::Class` (flagged as the high-risk areas) applied with only
the two small `FnSig`/`mk_fn_sig`/`From` adjustments.
