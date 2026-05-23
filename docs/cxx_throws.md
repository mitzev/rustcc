# `cxx_throws` — catching C++ exceptions from Rust

**Status**: Phase 0 (C++-side catch shim) is feature-complete and
production-ready as of v1.12.18. Phase 1 (Itanium native `invoke`)
ships runtime scaffolding in v1.12.5; codegen lowering still
aspirational pending a fork rustc bootstrap. Phase 2 (MSVC funclet)
ships cross-platform runtime in v1.12.6; codegen likewise pending.
See `fork/CXX-THROW-PLAN.md` for the full phasing.

## What it does

A function (or method, or constructor) annotated
`[[clang::annotate("rustcc::cxx_throws")]]` (or sidecar-marked
`throws: true`) gets:

- A C++ `extern "C" noexcept` wrapper that catches any exception
  and packs it into a `CxxRawError` (kind tag + message pointer).
- A Rust safe wrapper that returns
  `Result<T, ::cxx::CxxException>` instead of `T`.

The user code stays idiomatic on both sides — C++ throws
normally, Rust handles the result with `?` or `match`.

## Three opt-in mechanisms

### 1. Inline `[[clang::annotate(…)]]` markup

```cpp
[[clang::annotate("rustcc::cxx_throws")]]
int do_divide(int a, int b);
```

The cxx_importer's libclang walker captures the annotation and
stores it under the function's FQN in the `AnnotationSet`. Works
for free fns, instance methods, virtual methods (auto-routed
through the shim), and constructors (placement-new shape).

### 2. Sidecar YAML

```yaml
schema: 1
free_functions:
  do_divide:
    throws: true
types:
  Calc:
    methods:
      "divide(int, int)":
        throws: true
      "divide_strict":
        throws_types: [DomainError, RangeError]   # typed catches
```

Useful for headers you can't modify (vendored libraries, system
headers). Top-level `free_functions:` (v1.12.4) keys plain free
fns; per-method `throws` / `throws_types` (v1.12.9) attaches to
methods under each type entry.

### 3. Config knob (legacy v1.12.1 API)

```rust
let cfg = RustBindingsConfig {
    cxx_throws_functions: BTreeSet::from(["do_divide".into()]),
    ..Default::default()
};
```

Free-fn only. Predates the annotation system. Still supported
for backward compatibility.

## Typed catches (Phase 3)

Catch specific C++ exception types and dispatch on them:

```cpp
[[clang::annotate("rustcc::cxx_throws(DomainError, RangeError)")]]
int compute(int x);
```

Or via sidecar (free-fn or per-method `throws_types`).

The C++ shim emits per-type `catch (const Ti&)` arms before the
standard `std::exception` / `(...)` fallbacks. Each matched type
packs a distinct kind tag (`CXX_EXC_TYPED_BASE + index`) into
`CxxRawError`, which the Rust decoder maps to
`CxxExceptionKind::Typed(index)`. Users dispatch on the index
via the ergonomic helpers:

```rust
match compute(x) {
    Ok(v) => use_v(v),
    Err(e) if e.is_typed_at(0) => handle_domain_error(&e),
    Err(e) if e.is_typed_at(1) => handle_range_error(&e),
    Err(e) => handle_other(&e),
}
```

Empty parens (`cxx_throws()`) collapse to the bare `cxx_throws`
form. Nested generics in the type list are respected —
`cxx_throws(A, Pair<int, double>)` parses as a 2-element list.

## End-to-end pipeline via `Build::compile`

```rust
// build.rs
fn main() {
    cxx_importer::build::Build::new()
        .header("include/library.hpp")
        .cpp_std("c++17")
        .compile("library_bindings")
        .unwrap();
}
```

That's it. As of v1.12.14–v1.12.18 the orchestrator:

1. Parses each header via libclang.
2. Harvests `[[clang::annotate("rustcc::…")]]` markup AND any
   sidecar YAML the user pointed at.
3. Generates the Rust bindings — Result-returning wrappers
   appear automatically for annotated functions, with overload
   disambiguation when needed.
4. Generates the matching C++ shim source, including:
   - `__rustcc_throws_<name>` shims for free fns (v1.12.15).
   - `__rustcc_throws_<Class>_<method>` shims for class methods,
     with per-overload-unique suffixes when collisions exist
     (v1.12.16, v1.12.18).
   - `__rustcc_throws_<Class>_new` placement-new shims for
     annotated constructors (v1.12.17).
5. Compiles the shim source via `cc::Build`.
6. Emits Cargo directives so the downstream `bindings.rs`
   `include!`'s into the user's lib.rs and the static archive
   links cleanly.

## Manual pipeline (no `Build::compile`)

For users wiring the importer directly into a non-Cargo build:

```rust
use cxx_importer::{
    collect_class_method_throws_catches, collect_throws_catches,
    import_header_with_extras, render_all_throws_shims_cpp,
    ThrowsShimSpec,
};

// 1. Import the header.
let mut ctx = CxxTypeCtx::new(host_target());
let (classes, extras) = import_header_with_extras(
    &header_path,
    &["-x", "c++", "-std=c++17"],
    &mut ctx,
)?;

// 2. Generate the Rust bindings.
let bindings_rs = generate_rust_bindings_full(
    &ctx, &classes,
    &extras.annotations,
    &extras.aliases, &extras.enums,
    &extras.free_fns, &extras.static_data,
    &cfg,
)?;

// 3. Surface typed-catches lists for shim consumers.
let free_fn_typed = collect_throws_catches(&extras.annotations, &extras.free_fns);
let class_typed = collect_class_method_throws_catches(
    &ctx, &extras.annotations, &classes,
);

// 4. Build ThrowsShimSpec entries (one per annotated fn / method).
let specs: Vec<ThrowsShimSpec> = build_specs(&extras, &free_fn_typed, &class_typed);

// 5. Render the matching C++ TU.
let cpp_src = render_all_throws_shims_cpp(&[header_path.as_str()], &specs);
std::fs::write(&shim_cpp_path, cpp_src)?;
```

## Runtime types

The runtime side lives in the `cxx` crate (not `cxx_importer`):

```rust
pub struct CxxException {
    pub kind: CxxExceptionKind,
    pub message: Cow<'static, str>,
}

pub enum CxxExceptionKind {
    Std,           // caught via catch (const std::exception&)
    Unknown,       // caught via catch (...)
    Typed(u32),    // matched a typed-catch arm (index N)
}
```

### Ergonomic helpers (v1.12.13)

Match on kinds without writing out the variants:

```rust
e.is_std()              // bool
e.is_unknown()          // bool
e.is_typed()            // bool — any typed kind
e.is_typed_at(idx)      // bool — typed at specific index
e.typed_index()         // Option<u32>
```

### FFI shape (v1.12.0)

```rust
#[repr(C)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}
```

Constants:

- `CXX_EXC_OK = 0` — success tag.
- `CXX_EXC_STD = 1` — `std::exception` fallback arm.
- `CXX_EXC_UNKNOWN = 2` — `catch (...)` arm.
- `CXX_EXC_TYPED_BASE = 16` — first typed-catch arm. Typed
  arms occupy `[16, u32::MAX)`.

## Method + ctor coverage

| Kind | Throws supported? | Shim shape | Wrapper return |
|---|---|---|---|
| Free fn | ✅ since v1.12.0 | `__rustcc_throws_<name>` | `Result<T, _>` |
| Instance method | ✅ since v1.12.3 | `__rustcc_throws_<Class>_<method>` | `Result<T, _>` |
| Static method | ✅ since v1.12.3 | `__rustcc_throws_<Class>_<method>` | `Result<T, _>` |
| Virtual method | ✅ since v1.12.4 (downgrades to instance — shim does virtual dispatch C++-side) | `__rustcc_throws_<Class>_<method>` | `Result<T, _>` |
| Constructor | ✅ since v1.12.4 + build.rs v1.12.17 | `__rustcc_throws_<Class>_new` (placement-new) | `Result<Self, _>` |
| Destructor | ❌ deliberately unsupported — `Drop` can't return `Result` | — | — |
| Copy/move ctor | ❌ rejected by bindings emitter | — | — |
| Operators / conversions | ❌ silently skipped | — | — |
| Overloaded methods | ✅ disambiguated via param-type suffix since v1.12.18 | `__rustcc_throws_<Class>_<method>_<sig>` | `Result<T, _>` |

## What's deferred

- **Phase 1 codegen** (native `invoke` + Itanium landingpad):
  runtime helpers + the `#[rustc_cxx_throws]` attribute
  scaffolding patch are shipped
  (`cxx::native_invoke`,
  `fork/patches/21-rustc-cxx-throws-attr.patch`).
  The codegen patch that rewrites `call` to `invoke` is a
  separate 3-4 week fork rustc effort tracked separately.
- **Phase 2 codegen** (MSVC funclet): cross-platform runtime
  shipped in v1.12.6; same codegen story as Phase 1.
- **Typed-catch enum synthesis**: today users get
  `Result<T, ::cxx::CxxException>` and `match` on
  `e.is_typed_at(N)`. A future release can auto-generate
  per-fn typed enums so the user-facing shape becomes
  `Result<T, MyFnError>` with one variant per typed catch.
- **Iterator-typedef auto-discovery** for STL containers
  (libstdc++ vs libc++ specific). Users list iterator types
  explicitly in the sidecar today.

## See also

- `fork/CXX-THROW-PLAN.md` — Phase 0/1/2/3 design + LoC
  estimates.
- `crates/cxx/src/exception.rs` — runtime types.
- `crates/cxx/src/native_invoke.rs` — Phase 1 runtime helpers.
- `crates/cxx_importer/src/cxx_exception.rs` — renderer +
  helpers.
- `crates/cxx_importer/src/build.rs` — `Build::compile`
  orchestrator with throws shim emission for free fns + class
  methods + ctors.
- `crates/cxx_importer/tests/cxx_throws_*.rs` + the
  build-integration tests — test corpus covering every
  annotation form, every emitter path, end-to-end
  compile-link-run, and the `Build::compile` orchestrator
  pipeline.
