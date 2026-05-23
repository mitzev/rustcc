# rustcc v1.12.0

**The "catching C++ exceptions from Rust" release.** Nineteen PRs across the v1.12.x arc deliver the full Phase 0 throws system end-to-end: annotation parsing, bindings emission, runtime types, Phase 1 codegen scaffolding, build-orchestrator wiring, and documentation. Ships on top of the v1.10/v1.11 stretch work (MI runtime, multi-level method flatten, STL auto-discovery).

## Headline: `[[clang::annotate("rustcc::cxx_throws")]]` actually works

A C++ function annotated `rustcc::cxx_throws` now flows through `Build::compile` end-to-end:

```cpp
// my_lib.hpp
class Calc {
public:
    Calc(int seed);

    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int a, int b);

    [[clang::annotate("rustcc::cxx_throws(DomainError, RangeError)")]]
    int compute(int x);
};
```

```rust
// in your build.rs:
fn main() {
    cxx_importer::build::Build::new()
        .header("my_lib.hpp")
        .compile("my_lib_bindings")
        .unwrap();
}

// in your code:
let calc = Calc::new(42);
match calc.divide(10, 0) {
    Ok(v) => println!("{v}"),
    Err(e) if e.is_std() => println!("std exception: {}", e.what()),
    Err(e) => println!("other: {}", e.what()),
}
match calc.compute(5) {
    Ok(v) => println!("{v}"),
    Err(e) if e.is_typed_at(0) => println!("domain error"),
    Err(e) if e.is_typed_at(1) => println!("range error"),
    Err(e) => println!("fallback: {}", e.what()),
}
```

Bindings emitted as `Result<T, ::cxx::CxxException>`. C++ shim emitted via try/catch + `CxxRawError` tagged-union. Both sides compile + link as one static archive. No fork rustc required.

## Coverage matrix

| Kind | Status | Shim shape | Wrapper return |
|---|---|---|---|
| Free fn | ✅ | `__rustcc_throws_<name>` | `Result<T, _>` |
| Instance method | ✅ | `__rustcc_throws_<Class>_<method>` | `Result<T, _>` |
| Static method | ✅ | `__rustcc_throws_<Class>_<method>` | `Result<T, _>` |
| Virtual method | ✅ (downgrades to instance — shim does virtual dispatch C++-side) | same | `Result<T, _>` |
| Constructor | ✅ (placement-new) | `__rustcc_throws_<Class>_new` | `Result<Self, _>` |
| Overloaded methods | ✅ (param-type suffix) | `__rustcc_throws_<Class>_<method>_<sig>` | `Result<T, _>` |
| Typed catches | ✅ (per-type arm + index dispatch) | catch arms in source order | `Result<T, _>` |
| Destructor | ❌ deliberately (Drop can't return Result) | — | — |
| Operators / conversions | ❌ silently skipped | — | — |

## Three opt-in mechanisms

- **Inline annotation**: `[[clang::annotate("rustcc::cxx_throws")]]` or
  `[[clang::annotate("rustcc::cxx_throws(T1, T2)")]]` for typed.
- **Sidecar YAML**: per-method `throws: true` / `throws_types: [T1, T2]`,
  top-level `free_functions:` map for free fns.
- **Config knob**: `RustBindingsConfig::cxx_throws_functions` (free-fn only, legacy).

Inline beats sidecar on conflict. Annotation-parser respects nested generics — `cxx_throws(A, Pair<int, double>)` parses as a 2-element list.

## Phase 1 / Phase 2 scaffolding

`cxx::native_invoke` runtime crate ships the helper future fork rustc codegen will call from the catch landingpad (Itanium) / catchpad (MSVC). Cross-platform — Itanium does `__cxa_begin_catch` / `__cxa_end_catch`; MSVC's funclet handles lifetime via `catchret`. `fork/patches/21-rustc-cxx-throws-attr.patch` registers the `#[rustc_cxx_throws]` attribute in the fork rustc patch series.

The codegen-side `call → invoke` rewrite is the remaining work (estimated 3-4 weeks per `fork/CXX-THROW-PLAN.md`). v1.12.0 ships the runtime + attribute scaffolding so that when the codegen patch lands, the runtime crate is already there and the boundary contract is stable.

## Public API additions

`cxx` runtime crate:
- `CxxException`, `CxxExceptionKind { Std, Unknown, Typed(u32) }`.
- `CxxRawError { kind: u32, message: *const c_char }` — FFI boundary.
- `decode_cxx_raw_error<T>(raw, ok) -> Result<T, CxxException>`.
- `CXX_EXC_OK`, `CXX_EXC_STD`, `CXX_EXC_UNKNOWN`, `CXX_EXC_TYPED_BASE`.
- `CxxException::{is_std, is_unknown, is_typed, is_typed_at, typed_index, what}`.
- `cxx::native_invoke::__rustcc_cxx_catch_unknown` (Phase 1 helper).

`cxx_importer` crate:
- `Annotation::{CxxThrows, CxxThrowsTyped(Vec<String>)}`.
- `MethodEntry.throws`, `MethodEntry.throws_types`, `FreeFunctionEntry`.
- `RustBindingsConfig.cxx_throws_functions`.
- `render_throws_shim_cpp` + `render_throws_shim_cpp_typed`.
- `ThrowsShimSpec` + `render_all_throws_shims_cpp` (batch renderer).
- `collect_throws_catches` + `collect_class_method_throws_catches`.

`cxx_importer::build::Build::compile` now harvests annotations + emits matching shims for free fns + class methods + ctors with overload disambiguation.

## STL container auto-discovery — companion synth (v1.12.8)

When `HeaderGraph::auto_discover_template_specs = true` finds `std::vector<int>` in user code, the importer now also force-instantiates `std::allocator<int>` (the implicit companion). Same treatment for the rest of the STL container family:

| Container | Companions |
|---|---|
| `std::vector` / `deque` / `list` / `forward_list` | `std::allocator<T>` |
| `std::set` / `multiset` / `unordered_set` / `unordered_multiset` | `std::allocator<T>` |
| `std::map` / `multimap` / `unordered_map` / `unordered_multimap` | `std::pair<const K, V>` + `std::allocator<std::pair<const K, V>>` |
| `std::unique_ptr<T>` | `std::default_delete<T>` |
| `std::shared_ptr<T>` / `weak_ptr<T>` | `std::__shared_ptr<T>` |

Iterator typedefs are still libstdc++-vs-libc++-specific; users list those explicitly in sidecar.

## What's next

`v1.13.0` — Phase 1 codegen: fork rustc `call → invoke` lowering for `#[rustc_cxx_throws]`-marked `extern "C++"` decls. 3-4 weeks of focused work in the codegen layer. The runtime crate + attribute patch from v1.12.0 are the foundation.

`v1.13+` — typed-enum synthesis. Today users get `Result<T, CxxException>` and pattern-match on `e.is_typed_at(N)`. A future release will auto-generate per-fn typed enums so the user-facing shape becomes `Result<T, MyFnError>` with one variant per typed catch.

## Prebuilt binaries

Stage-1 toolchain binaries are bit-for-bit identical to v1.11.x (the rustc fork itself hasn't changed in this release — Phase 1 codegen is v1.13.0 work). The version bump exists so users can pin `rust-toolchain.toml` to a version that:
- includes the full Phase 0 throws system (cxx_importer + cxx runtime)
- includes the `Build::compile` orchestrator with throws shim emission
- includes the Phase 1 + Phase 2 runtime scaffolding
- includes the v1.12.x stretch work (M24 STL companion synth)

Same 4 host triples + macOS-13 caveat as previous releases.

```bash
TARGET=aarch64-apple-darwin
VERSION=v1.12.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"

curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
curl -fsSL -o ra.tar.xz     "$BASE/rust-analyzer-rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
tar -xJf ra.tar.xz     -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

## PR summary

19 PRs in the v1.12.x arc (#41–#61):

- **v1.12.0** (#41) — Phase 0 runtime + C++ shim renderer.
- **v1.12.1** (#42) — config-knob driven emitter integration.
- **v1.12.2** (#44) — inline `[[clang::annotate("rustcc::cxx_throws")]]` on free fns.
- **v1.12.3** (#45) — class-method emission.
- **v1.12.4** (#46) — virtual + ctor throws + sidecar `free_functions:` schema.
- **v1.12.5** (#47) — Phase 1 Itanium runtime + attribute scaffolding patch.
- **v1.12.6** (#48) — Phase 2 MSVC cross-platform runtime.
- **v1.12.7** (#49) — Phase 3 typed catches via renderer.
- **v1.12.8** (#50) — STL companion-spec synthesis for M24.
- **v1.12.9** (#51) — `cxx_throws(T1, T2)` annotation parser + `collect_throws_catches`.
- **v1.12.10** (#52) — `render_all_throws_shims_cpp` end-to-end emitter.
- **v1.12.11** (#53) — `collect_class_method_throws_catches`.
- **v1.12.12** (#54) — `docs/cxx_throws.md` initial.
- **v1.12.13** (#55) — `CxxException` ergonomic helpers.
- **v1.12.14** (#56) — `Build::compile` annotation harvest + Skip on free fns.
- **v1.12.15** (#57) — throws shim emission for free fns in Build pipeline.
- **v1.12.16** (#58) — class-method throws shim emission in Build pipeline.
- **v1.12.17** (#59) — ctor throws placement-new shim emission.
- **v1.12.18** (#60) — overload disambiguation for class-method shims.
- **v1.12.19** (#61) — docs refresh.

Plus the incidental fork-target fix (#43) for Windows MSVC `Target::host()`.
