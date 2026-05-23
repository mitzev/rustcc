# rustcc v1.13.0 — Native `invoke` + landingpad for `#[rustc_cxx_throws]` (DRAFT)

**The "first time rustc itself catches a C++ exception" release.** Phase 1 of v1.13.0 ships nine fork rustc patches that lower `extern "C++" fn` declarations marked `#[rustc_cxx_throws]` to a native LLVM `invoke` instruction with a custom catch-all landingpad — replacing the v1.12.x shim-based approach for the cases where the fork rustc is available.

This is a draft of the release notes for the eventual v1.13.0 tag. It captures Phase 1 as runtime-validated. Phase 2 (MSVC funclet path, typed catches at codegen level, deeper integration) is still pending.

## Headline: zero-shim catches

In v1.12.x the user side wrote `Result<T, CxxException>` and a code-generated C++ shim wrapped the call in `try { … } catch (…) { … }`, populating a `CxxRawError` tagged union on throw. The Rust caller invoked the shim, not the C++ function directly.

With v1.13.0 Phase 1 + the fork rustc patches, the user can mark the `extern "C++" fn` declaration itself with `#[rustc_cxx_throws]`. rustc lowers the call to:

```llvm
invoke void @maybe_throws(ptr sret %ret, i32 %x)
        to label %ok_wrap unwind label %cxx_throws_catch

cxx_throws_catch:
  %lp  = landingpad { ptr, i32 } catch ptr null
  %exn = extractvalue { ptr, i32 } %lp, 0
  %raw = call { i32, ptr } @__rustcc_cxx_catch_unknown(ptr %exn)
  store { i32, ptr } %raw, ptr %raw_err_slot
  br label %err_wrap

ok_wrap:
  ; … builds Result::Ok(value) in destination …
  br label %join

err_wrap:
  ; … builds Result::Err(CxxRawError { kind, message }) in destination …
  br label %join
```

No shim. No extra C++ object file. The catch handler is synthesized at LLVM IR level inside the rustc-compiled rlib. Zero overhead on the happy path — just the `invoke` instead of `call`.

## What ships

Ten fork rustc patches (numbered 21–30 in `fork/patches/`):

| Patch | Phase | Component | Description |
|---|---|---|---|
| 21 | 1A | `rustc_attr_data_structures` | `#[rustc_cxx_throws]` attribute scaffolding + `CodegenFnAttrFlags::CXX_THROWS = 1<<20` |
| 22 | 1A | `rustc_ty_utils` | `fn_can_unwind` returns true for `CXX_THROWS` callees regardless of declared ABI |
| 23 | 1B | `rustc_codegen_ssa` + `rustc_codegen_llvm` | `BuilderMethods::cxx_catch_landing_pad` trait method emitting `landingpad { ptr, i32 } catch ptr null` |
| 24 | 1C | `rustc_codegen_ssa::mir::block` | Synthesize a custom catch BB for `CXX_THROWS` calls in `do_call`, override the default cleanup landingpad |
| 25 | 1D | `rustc_codegen_llvm::context` | Declare + call `__rustcc_cxx_catch_unknown` (the `cxx` runtime helper) in the catch BB |
| 26 | 1E | `rustc_mir_transform::cxx_throws_wrap` | MIR pass scaffold that identifies `CXX_THROWS` Call terminators |
| 27 | 1F | `rustc_mir_transform::cxx_throws_wrap` | MIR pass actually wraps the happy path in `Result::Ok` via `AggregateKind::Adt` |
| 28 | 1G | `rustc_middle::mir::syntax` + 14 visitor sites | New `UnwindAction::CxxThrowsCleanup { bb, raw_err_local }` variant + codegen plumbing |
| 29 | 1G' | follow-up wiring | 6 bug fixes that close the end-to-end loop (successors(), validator, visitor, pretty-print, is_cleanup, ...) |
| 30 | 1H | `rustc_codegen_ssa::mir::block` | ABI bridging on the happy path — rebuild fn_abi with the post-MIR destination type as return, preserve can_unwind |

The MIR pass `cxx_throws_wrap` runs in `run_runtime_lowering_passes`. For each `Call` to a `CXX_THROWS` callee with a destination shaped like `Result<T, E>`, it:

1. Allocates `raw_ok_local: T` and `raw_err_local: E`.
2. Rewrites the `Call` so destination is `raw_ok_local` and target is a new `bb_ok_wrap`.
3. Sets `unwind = CxxThrowsCleanup { bb: bb_err_wrap, raw_err_local }`.
4. `bb_ok_wrap` does `dest = Result::Ok(raw_ok_local); goto orig_target`.
5. `bb_err_wrap` does `dest = Result::Err(raw_err_local); goto orig_target`.

Codegen's `do_call` recognizes `CxxThrowsCleanup` and synthesizes a catch BB that lands the exception via Itanium catch-all, calls `__rustcc_cxx_catch_unknown(exn_ptr)`, stores the result into `raw_err_local`'s stack slot, then branches to `bb_err_wrap` at LLVM level. The branch rejoins normal control flow — the catch terminates the unwind, so `bb_err_wrap` is *not* marked `is_cleanup`, and the validator has a special-cased rule for `CxxThrowsCleanup` edges that allows normal-target on an unwind-side action.

## Coverage

| Target | Status |
|---|---|
| `aarch64-apple-darwin` Itanium | ✅ runtime-validated (LLVM IR inspection) |
| `x86_64-apple-darwin` Itanium | likely works (same codegen path) — not yet smoke-tested |
| `*-unknown-linux-gnu` Itanium | likely works (same codegen path) — not yet smoke-tested |
| `*-pc-windows-gnu` Itanium | likely works — not yet smoke-tested |
| `*-pc-windows-msvc` SEH funclet | ❌ falls back to `cleanup_pad + abort` (P09.66 pending) |

## Linker smoke test results

A standalone end-to-end test (extern `int32_t maybe_throws(int32_t)` in C++ throwing `std::runtime_error` on negative input, called from Rust with `#[rustc_cxx_throws]`) confirms:

| Path | Result | Notes |
|---|---|---|
| Happy-path call `maybe_throws(5)` | ✅ `Ok(10)` | direct register return, no sret; Ok wrap fires, match dispatches correctly |
| Throwing call `maybe_throws(-1)` | ✅ `Err(kind=42)` | landingpad fires, `__rustcc_cxx_catch_unknown` returns, Result::Err constructed, normal flow resumes |
| Process exit | ✅ exit=0 | clean shutdown, no abort, no leaked exception |

**Phase 1 is closed.** Both arms of the Result work end-to-end on the Itanium codegen path.

## Known gaps / next steps (Phase 2)

1. **P09.67**: MSVC funclet codegen — needs `catch_pad` / `catch_switch` instead of `cleanup_pad`. Currently the MSVC path emits a `cleanup_pad + abort` placeholder.
2. **P09.68**: Typed catches at codegen level (vs. swallow-all catch_throws_unknown). Currently every exception becomes `CxxRawError`; typed `Result<T, MyError>` only works via v1.12.x sidecar shims.
3. **P09.69**: `CxxException` ergonomic conversion. Today the Err payload is the raw `{ i32, ptr }` from the helper; users typically want a `From<CxxRawError> for CxxException` conversion at the wrap site.
4. **Real integration test** linking against the actual `cxx` runtime crate (not the inline stub the smoke test uses).

## Compatibility

The v1.12.x shim-based path **still works** in v1.13.0 without the fork rustc. If your bindings emitter generates `[[clang::annotate("rustcc::cxx_throws")]]` on the C++ side and your `Build::compile` writes the shims, you get the v1.12.x behavior with stock rustc. Use the v1.13.0 native path only when you're already using the fork rustc and the codegen path matches your target.

The MIR pass is gated on `CodegenFnAttrFlags::CXX_THROWS`, which is set only by the `#[rustc_cxx_throws]` attribute. Code not using that attribute is completely unaffected — same MIR, same LLVM IR, same binary.

## Building from source

The fork rustc is built via `fork/build.sh`. On a fast machine (e.g. M-series Mac), incremental rebuilds after a code change in `compiler/rustc_mir_transform` are ~12s; full rebuilds from clean are ~30 min. Patches apply via `git apply --3way` against rust-lang/rust at the pinned commit `ef0fb8a2563200e322fa4419f09f65a63742038c`.

## Status

Not yet tagged. This document is a draft to be promoted to `RELEASE-NOTES-v1.13.0.md` when Phase 1 is sign-off-complete. Once tagged, the v1.13.x arc continues with the MSVC + typed-catches work.
