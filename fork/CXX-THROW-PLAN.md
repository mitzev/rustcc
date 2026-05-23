# Catching C++ exceptions from Rust — design

**Status**:
- **Phase 0** (C++-side catch shim): **shipped** in v1.12.0–v1.12.4. See `crates/cxx/src/exception.rs` + `crates/cxx_importer/src/cxx_exception.rs`.
- **Phase 1** (Itanium native `invoke` + landingpad): **scaffolding shipped** in v1.12.5 (`crates/cxx/src/native_invoke.rs` + `fork/patches/21-rustc-cxx-throws-attr.patch`); codegen lowering is v1.12.6.
- **Phase 2** (MSVC `catchpad`/`catchswitch` funclet EH): v1.12.7.
- **Phase 3** (typed catches + `what()` extraction): v1.12.8.

## What we want

```rust
extern "C++" {
    #[rustc_cxx_throws]
    fn open_file(path: &CStr) -> *mut File;
}

match unsafe { open_file(path) } {
    Ok(file) => use_file(file),
    Err(exc) => {
        // exc: CxxException — wraps the C++ exception object
        eprintln!("C++ threw: {:?}", exc.what());
    }
}
```

## Phasing — what's shipped vs what remains

### Phase 0 — C++-side catch shim (shipped v1.12.0–v1.12.4)

The cxx_importer emits a C++ `extern "C" noexcept` wrapper around every throwing function. The wrapper does the `try` / `catch (std::exception&)` / `catch (...)` itself and returns a tagged-union `CxxRawError` to Rust. The bindings emitter then wraps the FFI call in a Rust `Result`-returning wrapper.

**Properties**:
- ✅ Works on stock rustc — no fork patches required.
- ✅ Works on both Itanium and MSVC.
- ❌ Costs one extra function call per throwing call site.
- ❌ Loses the exception's dynamic type (best-effort `what()` extraction via `dynamic_cast<std::exception*>`).
- ❌ Doesn't compose across DSO boundaries when the throwing function lives in a shared library the C++ catch shim isn't visible from.

**Implementation**: `render_throws_shim_cpp` in `crates/cxx_importer/src/cxx_exception.rs` emits the C++ shim; `crates/cxx_importer/src/rust_bindings.rs` partitions free fns + class methods into throws vs plain extern blocks; runtime types in `crates/cxx/src/exception.rs`.

**Annotation pathways**:
- Inline: `[[clang::annotate("rustcc::cxx_throws")]]` on the C++ declaration.
- Sidecar YAML: `free_functions: { name: { throws: true } }` or per-method `throws: true` under a type entry.
- Config knob: `RustBindingsConfig::cxx_throws_functions` (still supported for callers that don't use annotations).

### Phase 1 — Itanium native `invoke` + landingpad (v1.12.5 scaffolding, v1.12.6 codegen)

Fork rustc lowers a `#[rustc_cxx_throws]`-marked `extern "C++"` call from LLVM `call` to LLVM `invoke` with an Itanium catch landingpad. The landingpad calls `cxx::native_invoke::__rustcc_cxx_catch_unknown` to convert the raw exception ptr into a `CxxRawError`, which the catch block then turns into `Result::Err(CxxException)`.

**Properties** (vs Phase 0):
- ✅ No C++ shim TU — bindings are pure Rust + fork rustc codegen.
- ✅ Composes across DSO boundaries — the unwind machinery is the standard Itanium libcxxabi flow.
- ✅ ~1 instruction fewer per call site (no shim trampoline).
- ❌ Requires fork rustc patches.
- ❌ Itanium-only (Phase 2 mirrors for MSVC).

**v1.12.5 shipped** (this milestone):
- `cxx::native_invoke` runtime module — Rust helper `__rustcc_cxx_catch_unknown(exc_ptr) -> CxxRawError`. Calls `__cxa_begin_catch` / `__cxa_end_catch` from libcxxabi/libsupc++; returns the catch-all (`Unknown`) tagged error with a fixed message string. Build-side smoke-tested; runtime round-trip pending v1.12.6 codegen.
- `fork/patches/21-rustc-cxx-throws-attr.patch` — registers the `#[rustc_cxx_throws]` attribute (parser → HIR variant → codegen_fn_attrs `CXX_THROWS` flag). Mirrors P09.48's `rustc_swift_throws` plumbing.

**v1.12.6 will ship**:
- `fork/patches/22-rustc-cxx-throws-itanium-invoke.patch` — codegen rewrite from `call` → `invoke` for marked decls. Emits the catch landingpad block + the call to `__rustcc_cxx_catch_unknown`. Wraps the visible return type as `Result<T, ::cxx::CxxException>` at the MIR / signature level so callers see the rewritten shape.
- Integration test: `crates/cxx/tests/native_invoke_phase1_runtime.rs` — links a C++ throwing function, calls it via `#[rustc_cxx_throws]` extern decl, asserts `Result::Err(_)` round-trips.

### Phase 2 — MSVC funclet EH (v1.12.7)

Same Rust API surface as Phase 1; different LLVM IR shape inside fork codegen:

```text
   %cs = catchswitch within none [label %catchpad] unwind to caller
catchpad:
   %cp = catchpad within %cs [ptr null]   ; null type-info = catch-all
   %exc = call ptr @__rustcc_msvc_extract_exception(ptr %cp)
   %raw = call { i32, ptr } @__rustcc_cxx_catch_unknown(ptr %exc)
   catchret from %cp to label %resume
```

**Requires**: separate runtime helper to extract the exception ptr from the funnellet (`__rustcc_msvc_extract_exception`) — the MSVC C++ EH ABI hides this inside the funclet's intrinsic params rather than the personality fn's structured output. The cxx runtime crate gates this behind `cfg(all(windows, target_env = "msvc"))`.

**Status**: stub in `cxx::native_invoke` (Itanium-only `#[cfg]` today); MSVC-side TBD in v1.12.7.

### Phase 3 — Typed catches (v1.12.8)

`[[rustcc::cxx_throws(std::runtime_error)]]` — catch only `std::runtime_error` and subclasses; let other exception types propagate. Itanium emits a non-null type-info in the catch clause:

```text
   %lpad = landingpad { ptr, i32 } catch ptr @_ZTISt13runtime_error
```

Multi-catch via `[[rustcc::cxx_throws(A, B, C)]]` maps to multiple clauses on the same landingpad, with dispatch into a Rust enum variant on the Err side.

**Also enables**: real `what()` extraction. Since the catch clause filters to types we know, we can `dynamic_cast<std::exception*>` (or use the type-info directly) to pull the message in a typed way, without the libstdc++/libc++-link dependency the current catch-all path needs to inspect.

## Estimated work (remaining)

| Piece | LoC | Time (focused) | Phase |
|---|---|---|---|
| Codegen: `call` → `invoke` for marked calls (Itanium) | ~300 | 4 days | 1 (v1.12.6) |
| Return-type rewriting MIR pass | ~150 | 3 days | 1 (v1.12.6) |
| MSVC funclet catch + runtime helper | ~400 | 6 days | 2 (v1.12.7) |
| Phase 3 typed catches + dispatch | ~400 | 5 days | 3 (v1.12.8) |

## What's done (cumulative)

- Phase 0 runtime + emitter + tests: ~2,500 LoC across `cxx`, `cxx_importer`, tests.
- Phase 1 scaffolding (runtime helper + attribute patch): ~400 LoC.

## ABI contracts established

### `__rustcc_cxx_catch_unknown(exc_ptr: *mut c_void) -> CxxRawError`

Itanium catch-all helper. Calls `__cxa_begin_catch(exc_ptr)` to take ownership of the exception, then `__cxa_end_catch()` to release it. Returns a fixed `CxxRawError` with `kind == CXX_EXC_UNKNOWN` and a static `"non-std::exception C++ exception"` message pointer.

This signature is what fork rustc codegen will emit a `call` to from inside the catch landingpad. The contract is **single-call-per-exception** — calling it more than once on the same `exc_ptr` is UB (double-end-catch).

### `__rustcc_cxx_catch_std(exc_ptr: *mut c_void) -> CxxRawError` (Phase 3)

Same shape, but does `dynamic_cast<std::exception*>` on the caught object and returns the `what()` string in the message slot. Requires linking against `libc++abi` / `libsupc++` for `__dynamic_cast`.

## Open questions

1. **Cross-DSO `what()` lifetime** — Phase 0's `thread_local std::string` keeps the message alive between FFI return and Rust copy-out. Phase 1's Rust-side static buffer can't do that for dynamic strings; either we (a) copy-into-a-heap-Box on the Rust side, returning a `*const c_char` to a leak, or (b) introduce a thread-local string slot on the Rust side. (b) is the lighter option; tracking for v1.12.6 if we add dynamic `what()` in Phase 1.

2. **Rust-side `Result<T, E>` shape after invoke lowering** — the codegen MIR pass that wraps the return type needs to handle existing return-type ABI conventions (sret, swiftcall) without breaking them. The simplest first cut: codegen rewrites the LLVM-level return AND the MIR signature; the rust frontend sees the same `T` it always saw. Trade-off captured in v1.12.6 spec.

3. **Interaction with `extern "C++"` calls that aren't `#[rustc_cxx_throws]`** — must remain `call` (no landingpad attached). The Itanium personality fn does still get attached to the Rust function as a whole — when a non-marked `extern "C++"` call throws, the unwind passes through and eventually hits a Rust-side `catch_unwind` or aborts. This matches the existing stock-rustc behavior, no regression.
