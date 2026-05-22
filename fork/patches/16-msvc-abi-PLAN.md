# Patches 16-22 — MSVC C++ ABI series (placeholder)

The MSVC ABI patches (`16-msvc-abi-target-routing.patch` through
`22-msvc-dllexport-dllimport.patch`) are not yet committed to the
fork patch stack. See [`fork/MSVC-PATCHES.md`](../MSVC-PATCHES.md)
for the per-patch design plan + LoC + time estimates.

Patches will land here once:

1. The rust-lang/rust tree at the pinned commit
   (`e22c616e4e87914135c1db261a03e0437255335e`) is checked out
   via `./fork/build.sh --apply-only`.
2. The Rust-side workspace `crates/rustc_abi_cxx/` MSVC modules
   (already landed in this sprint — `mangle_msvc.rs`,
   `layout_msvc.rs`, `vtable_msvc.rs`) are vendored into
   `compiler/rustc_abi_cxx/` by refreshing patch 01.
3. Patch 16 (routing) is generated via `git format-patch` against
   the rust-lang/rust tree with the new ABI flavor dispatch
   wired into `rustc_ty_utils::layout::cxx_bridge` +
   `rustc_symbol_mangling`.

Until then, the MSVC ABI is reachable from the workspace side
(cxx_importer-driven binding generation against an MSVC target
produces MSVC-mangled link names), but the fork rustc itself
still routes through the Itanium codegen path when invoked
against a Windows MSVC target. The remaining patches close that
gap.
