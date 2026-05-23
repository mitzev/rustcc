# cxx_throws_demo

End-to-end demonstration of the rustcc Phase 0 throws system.
Runs on **stock rustc** — no fork required.

## What's exercised

- **Free fn** with no real exception path (`add`) — Result-returning
  but always `Ok`.
- **Catch-all free fn** (`do_divide`) — catches `std::runtime_error`
  via the shim's `std::exception` arm.
- **Typed-catches free fn** (`compute`) — discriminates between
  `DomainError`, `RangeError`, `std::runtime_error`, and `catch (...)`
  via `cxx_throws(DomainError, RangeError)`.
- **Throwing constructor** (`Calc::new`) — returns
  `Result<Self, ::cxx::CxxException>`.
- **Throwing class method** (`Calc::divide`) — Result-returning
  instance method.

## Run

```bash
cd examples/cxx_throws_demo
cargo run
```

Expected output:

```
add(2, 3) = Ok(5)
do_divide(10, 2) = Ok(5)
do_divide(10, 0) = Err(C++ exception: divide by zero)
compute(0) = Ok(100)
compute(1) = DomainError(bad domain)
compute(2) = RangeError(out of range)
compute(3) = Std(plain std exception)
compute(4) = Unknown(non-std::exception C++ exception)
c.read() = Ok(42)
c.divide(7) = Ok(6)
c.divide(0) = Err(C++ exception: calc divide by zero)
Calc::new(-1) = Err(negative seed)
```

## Layout

- `cpp/library.hpp` — C++ declarations with `[[clang::annotate("rustcc::cxx_throws")]]`
  markup (catch-all + typed variants).
- `cpp/library.cpp` — Implementations. Throws different exception
  types per call path.
- `build.rs` — Drives `cxx_importer::build::Build::compile`. The
  orchestrator harvests annotations, generates `bindings.rs`
  (Result-returning wrappers), and emits a matching `cxx_shims.cpp`
  with all the `__rustcc_throws_*` shim bodies. Then `cc::Build`
  compiles `library.cpp` into a second static archive.
- `src/main.rs` — Includes the generated `bindings.rs` and
  exercises every path.

## Why stock-rustc-friendly?

Every function in `library.hpp` carries `[[clang::annotate("rustcc::cxx_throws")]]`,
which routes the generated extern decls through `extern "C"`
(via the catch shim symbols `__rustcc_throws_*`) instead of
`extern "C++"`. Stock rustc rejects `extern "C++"`; fork rustc
accepts it. So every function being throws-annotated lets this
demo run on default toolchains.

Real users typically keep their non-throwing functions
**unannotated** and rely on fork rustc — at which point only the
throwing functions go through the catch-shim path, and the
non-throwing functions get direct `extern "C++"` decls with
their Itanium-mangled symbols.

The `DomainError` and `RangeError` exception classes carry
`[[clang::annotate("rustcc::skip")]]` so the bindings emitter
doesn't generate Rust types for them — Rust only needs to know
they exist as catch targets on the C++ side, not as Rust-callable
types.

## What this demo proves

1. The annotation parser recognizes `cxx_throws` and `cxx_throws(T1, T2)`
   forms.
2. The bindings emitter routes throws-tagged functions through
   the Phase 0 catch shim path.
3. The build orchestrator harvests annotations from the header
   and emits matching shim bodies in `cxx_shims.cpp`.
4. `cc::Build` compiles the shims + library impl into a single
   static archive.
5. The Rust runtime (`cxx::CxxException` + helpers) decodes the
   shim's `CxxRawError` return into `Result<T, _>`.
6. Typed catches dispatch via `CxxException::is_typed_at(idx)`.
