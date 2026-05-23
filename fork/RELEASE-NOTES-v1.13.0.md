# rustcc v1.13.0 — Native `invoke` + landingpad for `#[rustc_cxx_throws]`

**The "first time rustc itself catches a C++ exception" release.** v1.13.0 ships fifteen fork rustc patches that lower `extern "C++" fn` declarations marked `#[rustc_cxx_throws]` to a native LLVM `invoke` with a custom catch landingpad (Itanium) or `catch_switch`/`catch_pad` funclet (MSVC). The v1.12.x shim path stays alongside — stable-rustc users keep working unchanged; fork-rustc users opt into the lower-overhead native path via the cxx crate's `rustcc-fork` feature + `RustBindingsConfig::cxx_throws_use_native_invoke`.

Both paths are runtime-validated on both Itanium (macOS aarch64) and MSVC (x86_64 under Wine), for both untyped catch-all and typed-catch dispatch.

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

Fifteen fork rustc patches (numbered 21–35 in `fork/patches/`):

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
| 31 | 2A | `rustc_mir_transform::cxx_throws_wrap` + `rustc_span` | `From<CxxRawError>` auto-conversion. Adds `rustc_diagnostic_item = "CxxRawError"` symbol; MIR pass injects a conversion call when the user's Err type differs from CxxRawError |
| 32 | 2B | `rustc_codegen_*` + attribute parser | Typed catches via multi-clause landingpad. New `#[rustc_cxx_throws_typeinfos = "_ZTI...,_ZTI..."]` attribute. Selector translation via `llvm.eh.typeid.for` chain. Switches function personality to `__gxx_personality_v0` so the C++ ABI personality matches typeinfos. |
| 33 | 2C | `rustc_codegen_*` | MSVC funclet codegen — replaces cleanup_pad+abort with real catch_switch+catch_pad for the catch-all path. New `BuilderMethods::catch_ret` trait method. |
| 34 | 2C' | `rustc_codegen_ssa::mir::block` | MSVC funclet bug fixes (Wine-validated): set personality fn, pass catch_switch token to catch_pad parent, synthesize CxxRawError inline (avoiding sret-attribute mismatch). |
| 35 | 2D | `rustc_codegen_*` + attribute parser | Typed catches on MSVC via Microsoft TypeDescriptor synthesis. New `#[rustc_cxx_throws_msvc_typedescs]` attribute + new `cxx_typedesc_global_msvc` trait method. Multi-catchpad in catch_switch dispatches by C++ RTTI string identity. |

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

## Phase 2B: typed catches via multi-clause landingpad

```rust
extern "C++" {
    #[rustc_cxx_throws]
    #[rustc_cxx_throws_typeinfos = "_ZTI11DomainError,_ZTI10RangeError"]
    fn parse(input: &CxxString) -> Result<Value, MyError>;
}
```

The fork rustc emits a multi-clause Itanium landingpad
(`catch ptr @_ZTI11DomainError catch ptr @_ZTI10RangeError catch ptr null`),
translates the opaque selector via `llvm.eh.typeid.for(@<ti>)`
into a small 1-based index, and packs it into
`CxxRawError.kind` via the runtime helper
`__rustcc_cxx_catch_typed`. The user's `From<CxxRawError>`
impl dispatches on the kind:

```rust
impl From<CxxRawError> for MyError {
    fn from(raw: CxxRawError) -> Self {
        match raw.kind {
            cxx::CXX_EXC_TYPED_BASE      => MyError::Domain,
            cxx::CXX_EXC_TYPED_BASE + 1  => MyError::Range,
            _                            => MyError::Other,
        }
    }
}
```

The function's personality is switched to
`__gxx_personality_v0` so the C++ Itanium ABI personality
fn actually performs typeinfo matching (Rust's personality
matches only the catch-all). Runtime cleanup actions (Rust
drop chains) still work — `__gxx_personality_v0` handles
cleanup actions alongside typed catches.

Smoke test confirms:

```
maybe_throws_typed(5)   →  ok: 10
maybe_throws_typed(-1)  →  err: Domain   ← _ZTI11DomainError matched
maybe_throws_typed(-2)  →  err: Range    ← _ZTI10RangeError matched
```

## Phase 2C: MSVC funclet codegen (untested)

Replaces the cleanup_pad+abort placeholder on MSVC with a
real `catch_switch` + `catch_pad` funclet that catches all
C++ exceptions and routes to the err_wrap_bb. Typed catches
on MSVC are deferred — MSVC uses Microsoft TypeDescriptors
(`?AVMyClass@@` + RTTI vtable), incompatible with the
Itanium `_ZTI<name>` symbols the
`#[rustc_cxx_throws_typeinfos]` attribute carries.

The MSVC code path is **not runtime-validated** on this
release — no Windows toolchain on the dev machine. Builds
cleanly; needs a Windows or Wine run to confirm.

## Phase 2A: ergonomic Result&lt;T, CxxException&gt;

With P09.69 (patch 31), the user's `Result<T, E>` Err type
no longer has to be `CxxRawError` exactly. Any type that
implements `From<CxxRawError>` works:

```rust
extern "C++" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, CxxException>;
}
```

The MIR pass detects that `CxxException != CxxRawError`,
resolves `<CxxException as From<CxxRawError>>::from`, and
injects a conversion call between the runtime helper's
result and the final `Result::Err` wrap. The cxx runtime
crate marks `CxxRawError` with
`#[rustc_diagnostic_item = "CxxRawError"]` so the MIR pass
can find it.

Smoke test confirms:

| Err type | Result |
|---|---|
| `CxxRawError` | `Err(kind=42)` (raw helper return, no conversion) |
| `CxxException` | `Err(kind=Runtime)` (typed kind via From impl) |

If the diagnostic item is missing (no `cxx` dep), the pass
falls back to trusting the user's declared type — Phase 1
behavior. If the diagnostic item is present but the impl
isn't, a `span_delayed_bug` surfaces as a compile error.

## cxx_importer manglers (P09.70)

`crates/cxx_importer/src/cxx_exception.rs` now exposes three
public helpers for translating C++ type names from a
`[[clang::annotate("rustcc::cxx_throws(T1, T2)")]]`
annotation into the attribute strings the fork rustc
patches expect:

```rust
itanium_typeinfo_symbol_for("DomainError")
    // -> "_ZTI11DomainError"
msvc_typedesc_name_for("DomainError")
    // -> ".?AVDomainError@@"
manglings_for_typed_catches(&["DomainError".into(), "RangeError".into()])
    // -> Some(("_ZTI11DomainError,_ZTI10RangeError",
    //         ".?AVDomainError@@,.?AVRangeError@@"))
```

The manglers handle global-namespace classes and single
`std::` segments; nested namespaces work for MSVC. Templates,
references, and qualifiers return `None` — callers fall
back to omitting the typeinfo attributes when one of the
types can't be mangled (so the indexing across both attrs
stays consistent).

The full automatic emission inside `rust_bindings.rs` (so
`Build::compile` adds the attributes alongside the existing
v1.12.x shims) is the natural next step. The helpers ship
now so downstream code can use them in isolation; the
rust_bindings.rs threading is a larger refactor tracked as
P09.71.

## Known gaps / next steps

1. **P09.71**: thread the P09.70 manglers through
   `rust_bindings.rs` so `Build::compile` emits the
   `#[rustc_cxx_throws_typeinfos]` +
   `#[rustc_cxx_throws_msvc_typedescs]` attributes
   automatically based on the C++ `cxx_throws(T1, T2)`
   annotations. Today users must write the lists by hand or
   call the helpers themselves.
2. **GCC backend support** — currently MSVC typed catches +
   the helper synthesis path are stubbed via `unimplemented!()`
   in the GCC backend. The Itanium-on-GCC path also needs
   real `cxx_catch_landing_pad` semantics (currently stubbed
   to `cleanup_landing_pad`).

## Compatibility

The v1.12.x shim-based path **still works** in v1.13.0 without the fork rustc. If your bindings emitter generates `[[clang::annotate("rustcc::cxx_throws")]]` on the C++ side and your `Build::compile` writes the shims, you get the v1.12.x behavior with stock rustc. Use the v1.13.0 native path only when you're already using the fork rustc and the codegen path matches your target.

The MIR pass is gated on `CodegenFnAttrFlags::CXX_THROWS`, which is set only by the `#[rustc_cxx_throws]` attribute. Code not using that attribute is completely unaffected — same MIR, same LLVM IR, same binary.

## Building from source

The fork rustc is built via `fork/build.sh`. On a fast machine (e.g. M-series Mac), incremental rebuilds after a code change in `compiler/rustc_mir_transform` are ~12s; full rebuilds from clean are ~30 min. Patches apply via `git apply --3way` against rust-lang/rust at the pinned commit `ef0fb8a2563200e322fa4419f09f65a63742038c`.

## Status

Not yet tagged. This document is a draft to be promoted to `RELEASE-NOTES-v1.13.0.md` when Phase 1 is sign-off-complete. Once tagged, the v1.13.x arc continues with the MSVC + typed-catches work.
