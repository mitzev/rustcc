# Migrating the rustcc fork to Rust 1.96.0

This documents a completed, validated rebase of the fork patch series
from its current base (1.97.0-dev master) onto the **1.96.0 stable**
release. The result lives in [`fork/patches-1.96.0/`](patches-1.96.0/)
(42 patches), separate from the in-use [`fork/patches/`](patches/) so
the current release path is undisturbed.

## Direction note

The fork normally tracks **1.97.0-dev** (master commit
`e22c616e4e87914135c1db261a03e0437255335e`, 2026-04-19 — the
`introduce-unnormalized` commit `#155083`). **1.96.0 stable**
(`ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96`, released 2026-05-25)
branched from master earlier and is therefore a *sibling, slightly
older* lineage — this is a lateral/backward re-pin, not an upgrade. If
the goal is simply "build on a released, reproducible toolchain,"
**1.97.0 (≈early July) is the cheaper target** since the fork already
tracks 1.97-dev. Use 1.96.0 only if a downstream consumer is pinned to
it.

## How to build on 1.96.0

In `fork/build.sh` (or a copy):

```sh
PINNED_COMMIT=ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96   # 1.96.0
# apply fork/patches-1.96.0/*.patch  (instead of fork/patches/)
```

The 1.96.0 checkout's `bootstrap.toml` needs two settings the master
pin doesn't:

```toml
[rust]
channel = "nightly"          # 1.96.0 is a *stable* tag; the fork needs
                             # `#![feature(rustc_attrs)]`, forbidden on stable
[llvm]
download-ci-llvm = true      # 1.96.0 is a real release -> CI LLVM exists
                             # (the master pin can't use it, hence false there)
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
