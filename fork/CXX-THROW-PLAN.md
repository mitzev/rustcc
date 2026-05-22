# Catching C++ exceptions from Rust — design

**Status**: design doc; implementation deferred to v1.09.3 (a focused
2-3 week sprint on its own).

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

The `#[rustc_cxx_throws]` attribute on an `extern "C++"` decl
transforms the call site from a direct `call` to an `invoke` with
a catch landingpad (Itanium) or funclet (MSVC). The catch
clause converts the raw exception into `CxxException` and the
fn's return type is auto-wrapped as `Result<T, CxxException>`.

## Why this is a separate sprint

C++ exception flow is fundamentally cross-language ABI work. The
pieces:

1. **Runtime type** — `CxxException` lives in the `cxx` runtime
   crate. Wraps a `*mut std::exception::__exception_ptr`-shaped
   smart pointer that the Rust side knows how to release through
   the corresponding C++ ABI's release function
   (`__cxa_end_catch` on Itanium, `_CxxThrowException` machinery
   on MSVC).

2. **Personality function**. Already set correctly: rustc
   upstream routes to `__CxxFrameHandler3` on MSVC via
   `wants_msvc_seh`; Itanium uses `rust_eh_personality` which
   handles both Rust panics and (in theory) foreign exceptions.

3. **Call lowering**. Currently `extern "C++"` calls are emitted
   as direct `call` instructions. Need to:
   - Detect `#[rustc_cxx_throws]` on the callee in the codegen
     layer.
   - Emit `invoke` instead, with a catch landingpad (Itanium) or
     `catchpad`/`catchswitch` (MSVC).
   - The landingpad's clause depends on platform:
     - Itanium: `catch (...)` matches via a null type-info — the
       catch-all that catches any C++ exception type. We extract
       the exception ptr via `__cxa_begin_catch`.
     - MSVC: `catchpad` within `catchswitch`, with the catch-type
       being `i8*` (catches-all). Convert the funclet's local
       exception object to a `*mut c_void` for Rust.

4. **Catch fragment** — small synthetic basic block that:
   - Extracts the exception value from the personality function's
     return.
   - Calls into a runtime helper (`__rustcc_cxx_catch_to_cxxexception`)
     that copies the exception's metadata + ends the catch
     (Itanium) or terminates the funclet (MSVC).
   - Returns `Err(CxxException { ... })`.

5. **Return-type rewriting** — when `#[rustc_cxx_throws]` is on
   `fn foo(...) -> T`, the visible Rust signature becomes
   `fn foo(...) -> Result<T, CxxException>`. The original `T`
   value lands in `Ok(T)` on the happy path; `Err(CxxException)`
   lands in the catch fragment.

## Estimated work

| Piece | LoC | Time (focused) |
|---|---|---|
| `cxx::CxxException` runtime type | ~200 | 3 days |
| `#[rustc_cxx_throws]` attr parser | ~80 | 1 day |
| Codegen: `call` → `invoke` for marked calls | ~300 | 4 days |
| Itanium catch fragment + runtime helper | ~250 | 4 days |
| MSVC funclet catch + runtime helper | ~400 | 6 days |
| Return-type rewriting | ~150 | 3 days |
| Tests + corpus | ~300 | 3 days |
| **Total** | **~1700** | **3-4 weeks** |

The biggest unknown is the MSVC funclet path — Rust's existing
`landingpad`-based EH lowering needs to be extended to
`catchpad`/`catchswitch` for the MSVC target. LLVM supports both
natively but the rustc-side lowering path is mostly Itanium-
shaped today.

## Phasing

**Phase 1**: Itanium only. Land `#[rustc_cxx_throws]`, the catch
fragment for `landingpad`-based EH, and the runtime type. Wine
testing on x86_64-unknown-linux-gnu confirms catch works
end-to-end.

**Phase 2**: MSVC funclet EH. Mirror the Itanium pattern with
`catchpad`/`catchswitch`. Runtime validation via the existing
v1.09.2 Wine pipeline.

**Phase 3**: Polish — type-specific catches (`#[rustc_cxx_throws(MyExceptionType)]`),
multi-catch dispatch, exception-spec validation.

## What's blocking

Nothing technical — this is mostly mechanical lowering work,
with the MSVC funclet path being the only new territory. The
gating concern is that it's substantial enough that it deserves
its own sprint rather than getting wedged into v1.09.2.

## Open questions

1. **`CxxException::what()`** — Itanium exposes the C++
   exception's `what()` method via `std::exception::what()`.
   MSVC's equivalent goes through the COL pointer at vftable[-1].
   Plumbing these through the runtime crate is doable but
   requires the Rust side to know the C++ exception's vftable
   layout — which means importing it from a header. Either:
   (a) Make `CxxException::what()` go through `std::exception`
       via a header-imported pointer-to-member.
   (b) Use a runtime-helper-side trampoline that returns the
       `what()` string by value.

2. **`std::exception` vs custom exception types**. Catching
   `std::runtime_error` is different from catching a user-defined
   `MyException`. Phase 3 work — for v1.09.3's initial scope,
   catch-all (any C++ exception) is sufficient.

3. **Cross-target unification** — should `CxxException` have the
   same Rust API on Itanium and MSVC? Yes — runtime crate hides
   the platform-specific exception ptr representation behind a
   single type.
