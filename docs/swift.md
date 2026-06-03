# Swift interop — `extern "Swift"`, `#[repr(swift)]`, `#[swift_value]`

**Status (v1.13.x)**: shipped and regression-tested. The fork teaches
`rustc` the Swift `swiftcc` calling convention, a `#[repr(swift)]`
layout marker, value/class lifetime management through Swift's
value-witness tables and ARC, and `throws`-bridging via `swifterror`.

**Depends on:** fork `rustc` patches (P09.14, P09.17, P09.18, P09.26,
P09.42, P09.46, P09.48) + the `rustcc_swift_rt` runtime crate.

| Capability | Surface | Patch(es) | Demo |
|---|---|---|---|
| `extern "Swift"` calls (swiftcc ABI) | calling convention | P09.14, P09.17 | `swift_extern_call` |
| Swift argument labels | `#[rustc_swift_labels = "…"]` | P09.17 | `swift_extern_call` |
| Swift value-type layout | `#[repr(swift)]` | P09.17 | `swift_value_type` |
| Value-type Drop/Clone via VWT | `#[swift_value]` (value) | P09.18, P09.26, P09.46 | `swift_value_type` |
| Class bindings (ARC) | `#[swift_value]` (class) | P09.42, P09.46 | `swift_nonpod`, `swift_value_attr` |
| Throwing functions | `#[rustc_swift_throws]` + `SwiftError` | P09.48 | `swift_throws` |

> Why this matters: stock Rust can only reach Swift through a C shim
> (`@_cdecl` on the Swift side, `extern "C"` on the Rust side), which
> flattens every value type to opaque bytes and forfeits ARC. The fork
> speaks Swift's ABI directly — values keep their witnesses, classes
> keep their reference counts, and throwing functions round-trip a real
> `Error`.

---

## 1. `extern "Swift"` — the calling convention

`extern "Swift"` routes a function through Swift's `swiftcc` calling
convention instead of the C ABI. Both **declarations** (calling a
`swiftc`-compiled symbol) and **definitions** (a Rust fn that Swift can
call back) are supported.

```rust
// Calling into a swiftc-compiled module. The link name is Swift's
// `$s…` mangling — copy it from `swift demangle`/`nm` on the built
// `.swiftmodule`/`.dylib`.
unsafe extern "Swift" {
    #[link_name = "$s5MyLib3addyS2i_SitF"]
    fn swift_add(a: i64, b: i64) -> i64;
}

let sum = unsafe { swift_add(20, 22) }; // 42
```

The fork also lets you **define** `extern "Swift"` functions, which is
how the `swift_extern_call` demo stays self-contained (no Swift
toolchain needed to run it):

```rust
extern "Swift" fn swift_add(a: i64, b: i64) -> i64 { a + b }
let sum = swift_add(20, 22); // 42, passed through swiftcc
```

Under the hood (P09.14): a new `ExternAbi::Swift { unwind }` variant
parses `"Swift"` / `"Swift-unwind"`, and `CanonAbi::Swift` maps to
LLVM's `swiftcc`. Symbol mangling for Swift items lives in
`rustc_symbol_mangling/src/swift.rs` (P09.17).

### Argument labels

Swift functions carry argument labels (`scale(_:by:)`). Attach them to
a foreign declaration with `#[rustc_swift_labels = "…"]` — one
comma-separated entry per parameter, `_` for an unlabeled position:

```rust
unsafe extern "Swift" {
    #[rustc_swift_labels = "_, by"]
    #[link_name = "$s5MyLib5scale_2byS2d_SdtF"]
    fn scale(value: f64, factor: f64) -> f64; // Swift: scale(_:by:)
}
```

Labels are binding metadata; they don't change the ABI.

---

## 2. `#[repr(swift)]` — Swift value-type layout

`#[repr(swift)]` marks a Rust struct as a Swift **value type**, the
mirror of `#[repr(cpp)]`. It widens `ReprFlags` with an `IS_SWIFT` bit
(P09.17). In the shipped implementation the in-memory layout follows
`repr(C)`; the Swift-specific behavior is in how the type is *managed*
(copy/destroy go through Swift's value-witness table, below) rather
than how its fields are arranged.

You rarely write `#[repr(swift)]` by hand — `#[swift_value]` adds it for
you (next section).

---

## 3. `#[swift_value]` — automatic Drop + Clone

A Swift value or class needs Rust `Drop` and `Clone` impls that defer to
Swift's lifetime rules. `#[swift_value]` (a compiler built-in attribute
as of P09.46; previously the `rustcc_macros::swift_value!` proc-macro)
synthesizes them. Pair it with `#[swift_type = "Module.Type"]`:

```rust
#[swift_value]
#[swift_type = "Demo.Counter"]
pub struct Counter {
    value: i64,
}
```

`#[swift_type]` accepts an optional kind suffix:

| Attribute | Kind | Lifetime mechanism |
|---|---|---|
| `#[swift_type = "Mod.T"]` or `…:struct` | value type | value-witness table (VWT) |
| `#[swift_type = "Mod.T:class"]` | class | ARC (`swift_retain`/`swift_release`) |

### 3a. Value types → value-witness table

For a value type, `#[swift_value]` synthesizes:

- an `extern "C"` **metadata-accessor** declaration, link-named with the
  Swift mangling `$s<modlen><module><typelen><type>VMa`
  (e.g. `Demo.Counter` → `$s4Demo7CounterVMa`);
- `impl Drop` → `rustcc_swift_rt::drop_swift_value(self, accessor)`,
  which loads the VWT and calls its **`destroy`** witness;
- `impl Clone` → `rustcc_swift_rt::clone_swift_value(dst, src,
  accessor)`, which calls the VWT's **`initialize_with_copy`** witness
  into a fresh `MaybeUninit<Self>`.

The VWT pointer lives one machine word *before* the metadata pointer
(`rustcc_swift_rt::vwt` reads `metadata[-1]`). See `swift_value_type`
for a self-contained demo that hand-builds a metadata block + VWT and
asserts the synthesized Drop/Clone dispatch through the `destroy` and
`initialize_with_copy` witnesses.

> **ABI footgun, fixed in P09.26.** Swift's VWT has
> `initializeBufferWithCopyOfBuffer` at slot 0 and `destroy` at slot 1.
> An earlier `ValueWitnessTable` had `destroy` at slot 0, so every
> `destroy` call dispatched into the init-buffer witness — silent for
> trivial types, a crash in `swift::RefCounts::incrementSlow` for types
> with class fields. The struct in `rustcc_swift_rt` now matches the
> `swiftc`-emitted layout exactly; don't reorder its fields.

### 3b. Classes → ARC

For a class (`:class`), there's no per-type metadata accessor — the
class pointer lives at offset 0 and Swift's C-runtime ARC entry points
manage it:

- `impl Drop` → null-safe `rustcc_swift_rt::release_swift_class(ptr)`
  (`swift_release`);
- `impl Clone` → `retain_swift_class(ptr)` (`swift_retain`) on the
  pointer field, **plus a per-field `Clone` of every other field**.

That per-field clone (P09.42) is the important subtlety: a class-backed
struct may carry non-`Copy` "extra" fields (`Box`, `String`, `Vec`)
alongside the Swift handle. A byte-wise copy would alias their heap
allocations and double-free on drop. `#[swift_value]` instead retains
the handle and clones each extra independently. See `swift_nonpod`
(proc-macro form) and `swift_value_attr` (built-in-attribute form).

```rust
#[swift_value]
#[swift_type = "Foo.Bar:class"]
pub struct Bar {
    _ptr: *mut core::ffi::c_void, // Swift class handle, offset 0
    extra: Box<i32>,              // non-POD extra, cloned independently
}
```

---

## 4. Throwing functions — `#[rustc_swift_throws]` + `SwiftError`

Swift `throws` functions take a hidden trailing error parameter pinned
to a dedicated register (Swift's `swifterror`: `r12` on x86-64, `x21`
on aarch64 Darwin). `#[rustc_swift_throws]` (P09.48) attaches LLVM's
`swifterror` attribute to that parameter on both the declaration and the
call site; the register allocator does the rest — no hand-rolled moves.

`rustcc_swift_rt::SwiftError` is the owned Rust handle. It releases the
underlying Swift `Error` on `Drop`, and `into_raw` transfers ownership
out without releasing.

```rust
use rustcc_swift_rt::SwiftError;

unsafe extern "Swift" {
    #[rustc_swift_throws]
    #[link_name = "$s5MyLib8do_thingSiSiAA5InputVtKF"]
    fn do_thing_raw(input: i64, err: *mut *mut core::ffi::c_void) -> i64;
}

fn do_thing(input: i64) -> Result<i64, SwiftError> {
    let mut err: *mut core::ffi::c_void = core::ptr::null_mut();
    let ret = unsafe { do_thing_raw(input, &mut err) };
    if err.is_null() {
        Ok(ret)
    } else {
        Err(unsafe { SwiftError::from_retained(err) })
    }
}
```

The codegen check is IR-level (the declaration carries `ptr swifterror`
on the error parameter); the `swift_throws` demo additionally exercises
the Rust-side `SwiftError` ownership semantics (construct from a
retained pointer, `into_raw` round-trip, null-handle check, `Drop`)
without a live Swift stdlib.

---

## 5. The `rustcc_swift_rt` runtime crate

`crates/rustcc_swift_rt` holds the runtime helpers the synthesized
impls call. It is **intentionally excluded from the workspace** because
it uses the fork-only `extern "Swift"` ABI, so it only builds under the
fork `rustc`. It is `no_std`.

Public surface:

| Item | Role |
|---|---|
| `Metadata`, `MetadataResponse` | opaque Swift type metadata + accessor return shape |
| `ValueWitnessTable` | the 6 witness slots (`initializeBufferWithCopyOfBuffer`, `destroy`, `initialize_with_copy`, `assign_with_copy`, `initialize_with_take`, `assign_with_take`) |
| `vwt(metadata)` | load the VWT (`metadata[-1]`) |
| `drop_swift_value::<T>(ptr, accessor)` | Drop via the `destroy` witness |
| `clone_swift_value::<T>(dst, src, accessor)` | Clone via the `initialize_with_copy` witness |
| `swift_retain` / `swift_release` (+ `retain_swift_class` / `release_swift_class`) | class ARC |
| `SwiftError` | owned Swift `Error` handle for `throws` |

---

## 6. Symbol mangling reference

| Symbol | Form | Example (`Demo.Counter` / `Foo.Bar`) |
|---|---|---|
| Value-type metadata accessor | `$s<modlen><module><typelen><type>VMa` | `$s4Demo7CounterVMa` |
| Class metadata accessor | `…CMa` | `$s3Foo3BarCMa` |
| Value-type outlined destroy | `…VWOh` | `$s4Demo7CounterVWOh` |

Module/type names are length-prefixed (`4Demo`, `7Counter`). Only
non-generic types with ASCII names are supported today (see Limitations).

---

## 7. Demos / regression probes

All four live under `fork/tests/class_keyword/` and run via
`fork/tests/run.sh` against the fork's stage-1 `rustc`. They stub Swift
runtime symbols (or hand-build a fake VWT) so they link and run
**without a Swift toolchain** — the ABI surface is what's under test.

| Probe | Demonstrates |
|---|---|
| `swift_extern_call` | `extern "Swift"` define + call (int + float ABI); labeled foreign decl via `#[rustc_swift_labels]` |
| `swift_value_type` | `#[swift_value]` **value** type — Drop→`destroy`, Clone→`initialize_with_copy`, asserted through a hand-built VWT |
| `swift_nonpod` | `swift_value!` **class** Clone preserves non-POD extras (no aliasing) |
| `swift_value_attr` | built-in `#[swift_value]` attribute auto-synthesizes class Drop + Clone |
| `swift_throws` | `#[rustc_swift_throws]` codegen reach + `SwiftError` ownership round-trip |

```bash
RUSTC=<path to fork stage-1 rustc> ./fork/tests/run.sh
```

These are part of the release pipeline: `.github/workflows/release.yml`
runs `fork/tests/run.sh` against the just-built stage-1 before
packaging, so a Swift/class regression fails the release build.

### End-to-end example — `examples/swiftui_counter`

A full **SwiftUI ↔ Rust** demo: a SwiftUI app (Swift `@main` + views)
calls down into a Rust model through `extern "Swift"` (swiftcc),
bound from the Swift side with `@_silgen_name`. The Rust half is
`cargo test`-validated; the SwiftUI app builds + links with `swiftc`
on macOS (the swiftcc symbols resolve from the Rust staticlib). This
is the *inverted* shape of the FLTK example — Swift drives the UI,
Rust is the model — because SwiftUI's declarative DSL can't be driven
from Rust (see §8). See that example's README for the architecture and
build steps.

---

## 8. Limitations

- **Generic Swift types** are not supported — the metadata accessor for
  a generic takes additional type-argument words.
- **Non-ASCII module/type names** are not supported (the mangler would
  need Punycode).
- **`#[repr(swift)]` layout** follows `repr(C)`; resilient (library-
  evolution) Swift types with opaque layout aren't modeled.
- Auto-synthesis covers Drop/Clone. Other protocol conformances
  (e.g. `Equatable`, custom operators) are not generated.

See `fork/PATCHES.md` (P09.14–P09.48) for the full implementation
history and `README.md` for the feature matrix.
