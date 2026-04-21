# Exception boundary

**Status:** draft v0.1
**Depends on:** `codegen`, `build_integration`.

---

## 1. Goal

No C++ exception ever unwinds into a Rust stack frame, and no Rust
panic ever unwinds into a C++ stack frame. Violations cause immediate
`std::abort` (from the C++ side) or `abort` (from the Rust side), not
UB.

Why not propagate? Itanium-ABI-level interop between LLVM's C++
exception tables and Rust's panic unwinding would need substantial
compiler work (matching personality functions, cleanup actions, RTTI),
and would not help users: propagating a C++ exception as a Rust panic
is semantically wrong (the user's `catch (const MyError&)` would not
run), and the reverse is worse.

Instead: a structural barrier.

## 2. Rust → C++ direction: shim wrapping

For every imported C++ function with mangled name `<M>`, the importer
enqueues a shim declaration:

```cpp
extern "C" auto __rustcc_shim_<M>(/* original args */) noexcept
    -> original_return_type {
    try {
        return <M>(args...);
    } catch (...) {
        std::terminate();
    }
}
```

- `extern "C"` → C calling convention, no name mangling. Rust links
  against the unmangled `__rustcc_shim_<M>` symbol.
- `noexcept` lets the C++ compiler elide unwinding tables in the caller.
- `catch (...)` catches any exception and calls `std::terminate`.

The shim-generation pass emits one `.cpp` per compilation unit with
one shim per unique imported function, plus `#include` directives for
the headers fed to the importer. The build driver compiles this with
the user's Clang toolchain and links it in. See `build_integration.md`.

### 2.1 Why shims, not a call-site `nounwind` + landingpad

We could annotate LLVM calls `nounwind` and let LLVM insert a landingpad
that calls `abort`. But:

- That still needs RTTI linkage to libc++abi / libstdc++'s personality
  routine in every binary touching C++. The shim approach pays that
  cost only in TUs that actually use interop.
- Interaction between Rust's panic personality and C++ personality is
  subtle. The shim approach doesn't require them to interact.
- Debugger-friendly: backtraces show a clean `__rustcc_shim_*` frame
  users can filter.

Fixed cost: one extra call frame per cross-boundary call. For tight
loops, users opt into `[[rustcc::noexcept]]` (§3) to eliminate it.

## 3. `noexcept` fast path

Functions marked `noexcept` in C++ can't throw. The shim is a no-op
wrapper and pessimizes codegen. The importer detects `noexcept` via
Clang and emits a "direct call" annotation; codegen calls the mangled
symbol directly, no shim.

Users can also opt in via `[[rustcc::noexcept]]` on functions they know
won't throw but which aren't declared `noexcept` for API reasons
(common in legacy code). The annotation is a promise: if such a
function throws, behavior is UB.

## 4. C++ → Rust direction

When C++ calls into Rust (for `#[repr(cpp)]` types with `extern "C++"`
methods), Rust panics must not escape. rustcc emits method bodies
wrapped in `catch_unwind`:

```rust
#[no_mangle]
extern "C++" fn <M>(this: *mut Self, args) -> R {
    match std::panic::catch_unwind(|| real_impl(this, args)) {
        Ok(r)  => r,
        Err(_) => std::process::abort(),
    }
}
```

`UnwindSafe` bounds are bypassed: we abort either way, so the soundness
condition motivating `UnwindSafe` doesn't apply.

## 5. Cross-toolchain concerns

Our shims are compiled with the same Clang the user's C++ code uses.
This matters for:

- **libc++ vs libstdc++.** Whichever standard library the C++ side
  links against, shims use.
- **Exception ABI version.** Itanium has been stable for exceptions
  since 2001, but mismatched Clang versions have had release-specific
  bugs. Shims inherit the user's Clang, so they inherit its behavior.

## 6. Future: structured exception translation

Opt-in later: `[[rustcc::throws(MyError)]]` changes the shim to catch
`MyError` specifically and encode it in a Rust-visible `Result`:

```cpp
MyError* parse(const char* s) [[rustcc::throws(MyError)]];
```

```rust
fn parse(s: &CStr) -> Result<CxxOwned<Widget>, CxxOwned<MyError>>;
```

The shim catches `MyError` by value (or pointer), moves it into a
caller-provided slot, and returns a discriminant. Other exception
types still hit `std::terminate`. v2 feature; v1 is terminate-on-throw.

## 7. Milestones

| M# | Deliverable                                                      |
|----|------------------------------------------------------------------|
| 1  | Shim generator: produce `.cpp` from import set                   |
| 2  | `noexcept` fast-path detection                                   |
| 3  | Rust → C++ panic barrier for `#[repr(cpp)]` methods              |
| 4  | Opt-in `[[rustcc::noexcept]]` annotation                         |
