# `cxx_throws` — catching C++ exceptions from Rust

**Status (v1.13.0)**: feature-complete on both the v1.12.x
shim path (stable rustc, no fork required) and the v1.13.0
native-invoke path (fork rustc + `#[rustc_cxx_throws]`).
Both paths are runtime-validated:

| Path | Toolchain | Itanium catch-all | Itanium typed | MSVC catch-all | MSVC typed |
|---|---|---|---|---|---|
| v1.12.x shim | stable rustc | ✅ | ✅ | ✅ | ✅ |
| v1.13.0 native invoke | fork rustc | ✅ | ✅ | ✅ | ✅ |

The shim path uses C++ `try`/`catch` wrappers compiled into
the C++ side; the native-invoke path uses LLVM `invoke` +
catch landingpads (Itanium) or `catch_switch`/`catch_pad`
funclets (MSVC) emitted by the fork rustc directly. The
v1.12.x section below covers the shim path; the
[v1.13.0 native invoke](#v1130-native-invoke) section
covers the fork path.

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

## v1.13.0 native invoke

The fork rustc (built via `fork/build.sh`, patches 21–35)
recognizes `#[rustc_cxx_throws]` and lowers each call to an
LLVM `invoke` instruction. On Itanium the personality is
`__gxx_personality_v0` and the landingpad carries one
`catch ptr @<typeinfo>` clause per typed catch plus a
final catch-all; on MSVC it's `__CxxFrameHandler3` plus a
`catch_switch` over one `catch_pad` per Microsoft
TypeDescriptor.

Three runtime helpers from `crates/cxx` are linked in:

- `__rustcc_cxx_catch_unknown` — Itanium catch-all returns
  `CxxRawError { kind: CXX_EXC_UNKNOWN, message: ... }`.
- `__rustcc_cxx_catch_typed` — Itanium typed catch hands the
  catch-clause index back as `CxxRawError.kind`.
- (MSVC catches synthesize the `CxxRawError` inline — no
  helper call.)

The MIR pass `cxx_throws_wrap` then converts the call's
`Result<T, CxxException>` destination into an Ok-wrap on
success and an Err-wrap (via `From<CxxRawError>`) on the
catch path, so the user-visible signature is the same as
on the shim path.

### Using the fork directly

For hand-written bindings, emit the attributes directly (no feature
gate needed since v1.14 — the fork's interop attrs are ungated):

```rust
unsafe extern "C++" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, ::cxx::CxxException>;
}

let r = unsafe { maybe_throws(-1) };
// r: Result<i32, CxxException>
```

For typed catches, add both Itanium and MSVC type-info lists:

```rust
unsafe extern "C++" {
    #[rustc_cxx_throws]
    #[rustc_cxx_throws_typeinfos     = "_ZTI11DomainError,_ZTI10RangeError"]
    #[rustc_cxx_throws_msvc_typedescs = ".?AVDomainError@@,.?AVRangeError@@"]
    fn parse(s: *const c_char) -> Result<Value, MyError>;
}

impl From<::cxx::CxxRawError> for MyError {
    fn from(raw: ::cxx::CxxRawError) -> Self {
        match raw.kind {
            cxx::CXX_EXC_TYPED_BASE      => MyError::Domain,
            cxx::CXX_EXC_TYPED_BASE + 1  => MyError::Range,
            _                            => MyError::Other,
        }
    }
}
```

The two lists must be in the same order so the
`CXX_EXC_TYPED_BASE + N` indexing matches across both
platforms.

### Using cxx_importer (v1.13.0 P09.71)

`Build::compile` can emit the v1.13.0 attributes
automatically from `[[clang::annotate("rustcc::cxx_throws(T1, T2)")]]`
annotations. Set the new flag on `RustBindingsConfig`:

```rust
cxx_importer::build::Build::new()
    .header("my_lib.hpp")
    .rust_bindings_config(RustBindingsConfig {
        cxx_throws_use_native_invoke: true,
        ..Default::default()
    })
    .compile("my_lib_bindings")
    .unwrap();
```

When `cxx_throws_use_native_invoke` is true, throws-tagged
functions are emitted as `extern "C++"` decls with
`#[rustc_cxx_throws]` + typeinfos attributes instead of
calling through `__rustcc_throws_<name>` shim wrappers. The
v1.12.x C++ shims aren't emitted, so the C++ side compiles
to the original symbol names directly.

`cxx::CxxRawError` must be visible to the fork rustc's MIR
pass — enable the `rustcc-fork` cargo feature on the
`cxx` dep so its `CxxRawError` carries
`#[rustc_diagnostic_item = "CxxRawError"]`:

```toml
[dependencies]
cxx = { version = "...", features = ["rustcc-fork"] }
```

The feature is opt-in; stable-rustc users leave it off and
keep using the v1.12.x shim path.

### Picking a path

| Constraint | Use |
|---|---|
| Stable rustc, no fork | v1.12.x shim path |
| Fork rustc + nightly | either; native invoke is lower-overhead |
| Mix of stable + nightly consumers | shim path (still works on both) |
| Want to drop C++ shim TUs | native invoke |
| Want zero-overhead happy path | native invoke (no shim sret) |

The native-invoke path requires:
- fork rustc (`fork/build.sh`)
- nightly toolchain
- `cxx = { features = ["rustcc-fork"] }`

(Until v1.13.x it also needed `#![feature(rustc_attrs)]` in the
consuming crate; the attrs are ungated since v1.14.)

The shim path requires none of these.

## What's deferred

- **GCC backend** — `cxx_throws` codegen is LLVM-backend
  only; the GCC backend has stubs that panic with a clear
  message if a `#[rustc_cxx_throws]` call reaches codegen.
  Tracked as P09.64-gcc / P09.68-gcc.
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
