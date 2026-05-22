# rustcc

**Native C++ and Swift interop for Rust — no `bindgen`, no `cxx` shim
crate, no build-script voodoo.**

`rustcc` is a fork of `rustc` that teaches the compiler to emit
**Itanium-C++-ABI-compatible symbols and layouts** for items tagged
`#[repr(cpp)]`, and **Swift-ABI-compatible calls and metadata-driven
witnesses** for items tagged `#[repr(swift)]`. Rust code written in
the fork links directly against Clang-compiled C++ and
`swiftc`-compiled Swift modules — including virtual dispatch,
constructors, destructors, single inheritance, `dynamic_cast`, ARC,
and Swift value-witness tables.

> **Status — v1.07.0 published 2026-05-09.** v1 shipped 2026-04-21
> with the feature matrix below on x86_64/aarch64 Linux/Darwin,
> i686 Linux, and bare-metal ARM Cortex-M. Five tagged point
> releases since: v1.02 / v1.03 closed initial adoption infra (prebuilt
> binaries) and ABI follow-ups; v1.04 / v1.06 shipped the full
> 26-milestone `cxx_importer` roadmap (Phases A + B + C through M26
> — multi-inheritance, template-spec method extraction, build
> orchestrator, real FLTK text editor demo); v1.07 shipped the
> developer-experience layer (`rustcc-cli`, `vscode-rustcc`
> extension) and **rust-analyzer Phase 2** — twelve patches in
> `fork/ra-patches/` that give `class` items full IDE parity with
> structs (hover, go-to-def, method completion, assists). See
> [`fork/PATCHES.md`](fork/PATCHES.md) for per-patch history
> (P09.22–P09.50) and `fork/RELEASE-NOTES-v1.07.0.md` for the
> latest release.

## Why this project exists

Rust has excellent FFI for *C* APIs. Everything else (C++ classes,
Swift `class` / `struct` types, vtable dispatch across language
boundaries) today requires a translation layer: `cxx`, `bindgen` +
hand-written shims, `autocxx`, or custom build scripts. Every layer
leaks incidental complexity — build steps, stubs, lifetime
bookkeeping that doesn't match either language's native semantics.

rustcc's thesis is that the *right* place to handle this is the
compiler itself, because the compiler already knows everything needed
— type layout, calling conventions, symbol mangling, drop semantics.
A modest set of targeted changes to `rustc` lets Rust programs speak
C++ and Swift natively, without any intermediate representation or
runtime shim.

If you've written `impl Drop for Widget { fn drop(&mut self) { unsafe
{ cxx_widget_destroy(self); } } }` before, this project is for you.

## v1 feature matrix

| Capability | v1 | Notes |
|---|---|---|
| `#[repr(cpp)]` struct layout | ✓ | Itanium rules: empty-class size, `alignas`, inheritance subobject placement |
| `extern "C++"` free functions | ✓ | Itanium mangling incl. nested module → namespace |
| Inherent methods on `#[repr(cpp)]` | ✓ | Automatic C++ member-function mangling + calling convention |
| C++ constructors | ✓ | `#[constructor]` on Rust impl methods or foreign-fn decls |
| C++ destructors | ✓ | Via `impl Drop`. D0/D1/D2 variants auto-emitted |
| Virtual methods / vtables | ✓ | `#[cpp_virtual]`; compiler emits `_ZTV` / `_ZTI` / `_ZTS` and auto-initializes vptr |
| Single inheritance | ✓ | `class Derived : Base { ... }`; `__si_class_type_info` chain |
| Virtual method override | ✓ | Derived virtual replaces base's vtable slot (P09.34) |
| `dynamic_cast` across inheritance | ✓ | Via libc++abi's runtime typeinfo walk |
| Parser-level `class` keyword | ✓ | Weak keyword, desugars to `#[repr(cpp)]` struct + impl |
| Cross-crate polymorphic classes | ✓ | Ctor / wrapper / virtual attributes encode correctly across crates |
| C++ operator overloading | ✓ | `#[operator = "Plus"]` and friends |
| `extern "Swift"` calling convention | ✓ | swiftcc ABI + Swift symbol mangling for structs and classes |
| Swift `#[repr(swift)]` value types | ✓ | Layout, retain/release semantics via VWT |
| Swift class bindings | ✓ | ARC via `swift_retain` / `swift_release` |
| `swift_value!` macro | ✓ | Auto-generates `impl Drop` + `impl Clone` for `#[repr(swift)]` |
| Cross-compilation | ✓ | Per-invocation target params flow into C++ ABI paths (P09.35) |
| Bare-metal ARM Cortex-M | ✓ | `thumbv7em`, `thumbv7m`, `thumbv8m.*` — zero compiler changes after P09.35 |

**Targets:** `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, `i686-*`
(non-Windows), bare-metal `thumbv7em-*` / `thumbv7m-*` / `thumbv8m.*`.
Cross-builds across these are supported.

## v2 roadmap — shipped

The v2 stretch list from the original v1 README is **fully delivered**
across v1.02–v1.07:

- **Multi-inheritance and virtual bases** — shipped in v1.06.0
  (M22). Secondary vtables, this-adjusting thunks, cross-base
  `as_<base>` accessors, recursive-import convergence pass.
- **True compiler auto-synthesis for `#[repr(swift)]`** — shipped
  in v1.02.0 (P09.46). `#[swift_value]` is now a built-in
  attribute macro; the proc-macro wrapper is retired.
- **Multi-field class-backed Swift bindings with non-POD extra
  fields** — shipped in 1.01 (P09.42). Class-backed `swift_value!`
  Clone now does per-field clone for non-POD extras.
- **Const generics on class headers** — shipped in 1.01
  (P09.41). `class Array<T, const N: usize> { ... }` parses,
  resolves, and emits correctly; the two-DefId class split
  threads class generics through both the struct and impl halves.
- **`cxx_importer` C++ → Rust binding generator** — shipped
  across v1.04.0 (Phases A + B + C, M1–M21) and v1.06.0 (v2
  roadmap M22–M26). Real-world target: `examples/fltk_text_editor`
  pulls in ~50 FLTK classes and produces a working 800x600 editor.
- **Developer experience layer** — shipped in v1.07.0.
  `crates/rustcc-cli` (install / doctor / init); `tools/vscode-rustcc`
  VS Code extension (grammar overlay, snippets, commands, status
  bar, problems-pane integration with the JSON skip log);
  `cxx_importer::Build::compile()` writes a structured
  `bindings.skips.json` sidecar.
- **rust-analyzer Phase 2** — shipped in v1.07.0
  (`fork/ra-patches/01..12`). `class` items get full IDE parity
  with structs: hover, go-to-def, find-references, completion,
  inherent-method dispatch with C++-style derived-shadows-base,
  plus class-aware assists (`generate_class_new`,
  `change_visibility`, `find all overriders`, `implement override`).

Remaining stretch items (not in any near-term release):

- **Windows MSVC ABI** — out of scope for the Itanium-focused
  fork; would be a separate targeting effort. Tracked as v1.09.0
  scoping.
- **Runtime-dispatch CI validation** — the M22 work has strong
  static evidence (vtable structure, mangled symbols, FLTK link
  success) but no executed test asserting `&B`-pointing-into-a-C
  routes through the secondary thunk. Needs a CI runner with the
  rustcc fork toolchain pre-installed.
- **STL container support for M24** — implicit instantiation
  auto-discovery.
- **Method flattening multi-level walk** — v1.07.0's `flatten_inherited_methods`
  flag walks one level deep; multi-level is a follow-up.

## Quick start

### Install the forked compiler

```sh
git clone https://github.com/Mitzev/rustcc
cd rustcc/fork
./build.sh               # applies patches, builds stage-1 rustc (~30–90 min)
```

After the build, the toolchain lives at
`rust-lang-rust/build/host/stage1/bin/rustc`. Register it with
`rustup`:

```sh
rustup toolchain link rustcc <absolute-path-to>/rust-lang-rust/build/host/stage1
rustup default rustcc
```

### Hello from C++

Three files — Rust calls C++:

```cpp
// hello.cpp
#include <cstdint>
extern "C++" {
    uint64_t cpp_double(uint64_t x) { return x * 2; }
}
```

```rust
// probe.rs
#![crate_type = "staticlib"]
extern "C++" {
    fn cpp_double(x: u64) -> u64;
}
#[unsafe(no_mangle)]
pub extern "C" fn rust_entry() -> u64 {
    unsafe { cpp_double(21) }
}
```

```c
// runner.c
#include <stdio.h>
#include <stdint.h>
extern uint64_t rust_entry(void);
int main(void) { return (int)rust_entry(); }
```

Build and run:

```sh
rustc probe.rs                     # emits libprobe.a with _Z9cpp_doubley referenced
clang++ -c hello.cpp -o hello.o
clang   -c runner.c -o runner.o
clang++ runner.o hello.o libprobe.a -o demo
./demo; echo $?                    # prints 42
```

No `bindgen`, no manual mangling — the Rust-side `extern "C++"` block
produces the exact Itanium symbol Clang emits on the C++ side.

### A polymorphic class defined in Rust, called from C++

```rust
// widget.rs
#![feature(rustc_attrs)]

pub class Widget {
    v: i32,

    #[constructor]
    pub fn new(v: i32) -> Self { Widget { v } }

    #[cpp_virtual]
    pub fn foo(&self) -> i32 { self.v + 100 }
}

#[unsafe(no_mangle)]
pub extern "C" fn make_widget(v: i32) -> *mut Widget {
    Box::into_raw(Box::new(Widget::new(v)))
}
```

```cpp
// caller.cpp
struct Widget {
    virtual int32_t foo();
    int32_t v;
};
extern "C" Widget* make_widget(int32_t v);

int main(void) {
    Widget* w = make_widget(7);
    return w->foo();   // dispatches into Rust's `foo` — returns 107
}
```

More examples in [`examples/`](examples/). Full walkthrough:
[`fork/getting-started.html`](fork/getting-started.html).

## Repo layout

- **`fork/`** — the canonical rustc fork. Patches, build script, and
  `getting-started.html` (the full user guide). The patches in
  `fork/patches/` are the authoritative source-of-truth for what the
  fork adds; they apply cleanly to the pinned upstream nightly.
- **`crates/`** — the workspace that predates the fork. Hosts shared
  infrastructure (`rustc_abi_cxx` layout/mangling, the proc-macro
  crate, `cxx` runtime types). Still exercised by integration tests.
- **`examples/`** — runnable demos showcasing v1 features.
- **`docs/`** — per-crate design docs (Itanium ABI, codegen,
  ownership, exception boundary, `repr(cpp)`, build integration).

## Documentation

| Doc | Scope |
|---|---|
| [`fork/getting-started.html`](fork/getting-started.html) | User-facing guide — install, quickstart, feature reference, build recipes |
| [`fork/INSTALL.md`](fork/INSTALL.md) | Install recipes — fast path (prebuilt tarball + `rustup link`) and source-build path |
| [`fork/PATCHES.md`](fork/PATCHES.md) | Per-patch history (P09.22–P09.50), authoritative spec for the rustc fork |
| [`fork/ra-patches/`](fork/ra-patches/) | 12-patch rust-analyzer series adding `class` Phase 1 parser + Phase 2 HIR/IDE parity |
| [`crates/rustcc-cli/`](crates/rustcc-cli/) | DX CLI: `rustcc install` / `doctor` / `init` |
| [`tools/vscode-rustcc/`](tools/vscode-rustcc/) | VS Code extension — grammar overlay, snippets, commands, RA-fork installer |
| [`docs/cxx_importer.md`](docs/cxx_importer.md) | `cxx_importer` design + Phase A/B/C milestone table (M1–M26 all shipped) |
| [`docs/repo_layout.md`](docs/repo_layout.md) | Two-repo architecture: workspace vs `rustcc-rustc` compiler fork |
| [`docs/rustc_abi_cxx.md`](docs/rustc_abi_cxx.md) | Itanium layout, mangling, vtable construction |
| [`docs/codegen.md`](docs/codegen.md) | LLVM IR generation for cross-language calls, vtables, ctors/dtors |
| [`docs/ownership_and_safety.md`](docs/ownership_and_safety.md) | `CxxOwned<T>`, pinning, move/copy surface, borrow-checker contract |
| [`docs/exception_boundary.md`](docs/exception_boundary.md) | Terminate-on-throw barrier between Rust and C++ |
| [`docs/repr_cpp.md`](docs/repr_cpp.md) | Rust types exposed to C++ with matching layout |
| [`docs/build_integration.md`](docs/build_integration.md) | Driver, cargo manifest extensions, linking, toolchain detection |

## Non-goals

- **Replacing the `cxx` crate.** `cxx` remains the right tool for
  projects that want a user-space FFI layer without a forked
  compiler.
- **Silent compatibility with old Clang.** rustcc pins a Clang floor
  and rejects older toolchains at build time.
- **MSVC ABI in the main tree.** A separate fork can add it; Itanium
  and MSVC layout/mangling differ enough that sharing a crate across
  them is a net loss.

## License

Dual-licensed under **MIT** ([LICENSE-MIT](LICENSE-MIT)) or
**Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)) at your option,
matching the upstream rustc license.
