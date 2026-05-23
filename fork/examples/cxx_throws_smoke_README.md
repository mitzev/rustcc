# cxx_throws smoke test (v1.13.0 Phase 1)

End-to-end smoke test that drove the v1.13.0 Phase 1 codegen
work. Compiles a tiny C++ function that may throw, calls it
from Rust via `#[rustc_cxx_throws]`, and verifies the catch
+ landingpad + runtime helper chain.

## Build

```bash
clang++ -c cxx_throws_smoke_maybe_throws.cpp -o maybe_throws.o -O0 -fexceptions
ar rcs libmaybe_throws.a maybe_throws.o

/path/to/fork/build/host/stage1/bin/rustc \
    -Cpanic=unwind \
    --crate-type=bin \
    -o smoke cxx_throws_smoke.rs \
    -l static=maybe_throws -L . \
    -lc++
```

## Run

```bash
./smoke
```

Expected output (current state, as of v1.13.0 Phase 1):

```
ok(<garbage>)       # ⚠ known ABI gap — see release notes
err(kind=42)        # ✅ catch path works
```

The second line confirms:
1. The C++ exception was caught via the Itanium landingpad
2. `__rustcc_cxx_catch_unknown` (inline stub) was called
3. The returned `CxxRawError` was stored into the
   raw_err_local slot
4. The MIR-level err_wrap_bb constructed
   `Result::Err(CxxRawError { kind: 42, .. })`
5. Normal control flow resumed and the match printed the
   error kind

The first line's garbage is the result of the happy-path
ABI mismatch (see "Known gaps" in
`RELEASE-NOTES-v1.13.0-DRAFT.md`). The C++ function returns
`int32_t` in a register, but rustc lowers the call with an
sret pointer based on the Rust-declared `Result<T, E>`
return type. The sret slot is never written.

## Why the inline `__rustcc_cxx_catch_unknown` stub?

The real helper lives in `crates/cxx/src/native_invoke.rs`.
Linking the smoke test against the full `cxx` runtime crate
would require a Cargo workspace setup and bring in the
v1.12.x dependency stack. Inlining the helper directly into
the smoke test isolates the codegen-side wiring from the
runtime crate, so failures here are unambiguously in the
fork rustc patches.

For a "real" integration test that links against the full
`cxx` runtime crate, see the future P09.69 task.
