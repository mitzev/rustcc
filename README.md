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

> **Status — v1.13.3 (current).** v1 shipped 2026-04-21; the feature
> matrix below is the cumulative state of the v1.0x–v1.13x line.
> Supported hosts: x86_64/aarch64 Linux & macOS, x86_64/aarch64
> Windows MSVC, i686 Linux, and bare-metal ARM Cortex-M. Major
> additions since v1: the full `cxx_importer` C++→Rust binding
> generator, the Windows MSVC C++ ABI, C++ exception catching
> (`cxx_throws`), C++ templates (incl. non-type arguments), the
> developer-experience layer (`rustcc-cli`, the `vscode-rustcc`
> extension, and a patched rust-analyzer), and the Swift interop
> surface. See [`fork/PATCHES.md`](fork/PATCHES.md) for per-patch
> history and the `fork/RELEASE-NOTES-*.md` files for each release.

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

## Feature matrix

Cumulative across the v1.0x–v1.13x line. Everything below is shipped.

### C++ — types, dispatch, ABI

| Capability | Notes |
|---|---|
| `#[repr(cpp)]` struct layout | Itanium rules: empty-class size, `alignas`, subobject placement |
| `extern "C++"` free functions | Itanium mangling incl. nested module → namespace |
| Inherent methods, constructors, destructors | member-function mangling; `#[constructor]`; `impl Drop` → D0/D1/D2 |
| Virtual methods / vtables / override | `#[cpp_virtual]`; emits `_ZTV`/`_ZTI`/`_ZTS`, auto vptr init, slot override |
| Single, multiple & virtual inheritance | secondary vtables, this-adjusting thunks, vbase offsets, VTT + construction vtables |
| `dynamic_cast` across inheritance | via libc++abi runtime typeinfo walk |
| Bit-fields, packing | `place_bitfield` + `__attribute__((packed))` / `#pragma pack`, clang-validated |
| Copy / move special members | copy ctor → `impl Clone`; move ctor → `move_from`; `operator=` → `copy_assign`/`move_assign` |
| C++ operator overloading | `#[operator = "Plus"]` and friends |
| Parser-level `class` keyword | weak keyword, desugars to `#[repr(cpp)]` struct + impl |
| **Subclass an imported C++ class** | `class D : CppBase` over a `cxx_importer`-imported base: override concrete + pure virtuals and the virtual destructor; C++ dispatches through `CppBase*` into the Rust `override`, `delete` runs Rust `Drop` (v1.13.7, single inheritance) |
| Cross-crate polymorphic classes | ctor / wrapper / virtual attributes encode across crates |
| **Windows MSVC C++ ABI** | vftables, scalar-deleting dtor, SEH funclets, sret-via-RCX/X8, dllexport; Wine-validated |

### C++ — binding generation & interop

| Capability | Notes |
|---|---|
| `cxx_importer` C++ → Rust generator | parse headers → emit Rust bindings + C++ shims; `Build::compile()` build.rs driver |
| C++ templates | type + non-type (integral) + template-template arguments; both ABIs, clang-validated |
| Auto-instantiation of STL specs | `std::vector<int>` referenced in a user API is discovered + force-instantiated |
| C++ exception catching | `[[rustcc::cxx_throws]]` → `Result<T, CxxException>`; catch-all + typed; Itanium + MSVC |

### Swift

| Capability | Notes |
|---|---|
| `extern "Swift"` calling convention | swiftcc ABI + Swift symbol mangling (calling `swiftc`-built functions) |
| `#[repr(swift)]` value types | layout + Drop/Clone via the value-witness table |
| Swift class bindings (ARC) | `swift_retain` / `swift_release` on a held class pointer |
| `#[swift_value]` (built-in attribute) | auto-synthesizes `Drop` + `Clone` for value and class types |
| Swift `throws` | `#[rustc_swift_throws]` + `SwiftError` → `Result` (swifterror register) |

### Platforms

| Capability | Notes |
|---|---|
| Cross-compilation | C++ ABI derives from the session `--target`, not the build host |
| Bare-metal ARM Cortex-M | `thumbv7em` / `thumbv7m` / `thumbv8m.*` |

**Hosts:** `x86_64`/`aarch64` `-apple-darwin` and `-unknown-linux-gnu`,
`x86_64`/`aarch64` `-pc-windows-msvc`, `i686-unknown-linux-gnu`,
`x86_64-pc-windows-gnu`, and bare-metal `thumbv7em-*` / `thumbv7m-*` /
`thumbv8m.*`. Cross-builds across these are supported.

## Editor & developer tooling

- **VS Code extension** (`tools/vscode-rustcc/`) — syntax highlighting
  for `class`, `extern "C++"`/`extern "Swift"`, and the rustcc
  attributes; snippets; commands (toolchain + RA-fork install); a
  status-bar pin indicator; and Problems-pane diagnostics fed by
  `cxx_importer`'s `bindings.skips.json`. See
  [Editor setup](#editor-setup-vs-code--rust-analyzer) below.
- **Patched rust-analyzer** (`fork/ra-patches/`) — gives `class` items
  full IDE parity with structs: hover, go-to-def, find-references,
  completion, inherent-method dispatch, and class-aware assists.
  Stock RA chokes on the `class` keyword; this fork doesn't.
- **`rustcc-cli`** (`crates/rustcc-cli/`) — `rustcc install` / `doctor`
  / `init` for toolchain install + project scaffolding.

## Remaining gaps

Honest list of what is **not** yet implemented:

- **C++ templates beyond instantiation** — uninstantiated generic
  templates (no Rust representation), and template-template /
  pointer-to-member non-type *arguments* (they mangle correctly when
  supplied but can't be auto-recovered from libclang's type view).
- **Swift inheritance** — you can *call* Swift and hold/retain Swift
  class instances, but a Rust type cannot *subclass* a Swift class or
  override its methods. See the Swift section below.
- **Deep / multiple-inheritance C++ bases for Rust subclasses** —
  subclassing an imported C++ class (above) is single-inheritance from a
  *root* polymorphic base; deeper chains (e.g. FLTK's
  `Fl_Text_Editor → … → Fl_Widget`) and multiple/virtual inheritance of
  the base are future work.
- **Member pointers** (partial), **covariant-return thunks**, and
  **GCC-backend `cxx_throws`** (the catch path is Itanium/MSVC LLVM).
- **Recursive STL import** — a user type that *derives from* a system
  type (e.g. `: std::exception`) binds that base as opaque rather than
  importing its whole graph; force-instantiate specs you want in full.

## Quick start

### Install the forked compiler

```sh
git clone https://github.com/mitzev/rustcc
cd rustcc/fork
./build.sh               # applies patches, builds stage-1 rustc (~30–90 min)
```

After the build, the toolchain lives at
`rust-lang-rust-fork/build/host/stage1/bin/rustc`. Register it with
`rustup`:

```sh
rustup toolchain link rustcc <absolute-path-to>/rust-lang-rust-fork/build/host/stage1
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

## Editor setup (VS Code + rust-analyzer)

The `class` keyword and `extern "C++"`/`extern "Swift"` blocks confuse
stock tooling. rustcc ships a VS Code extension and a patched
rust-analyzer that fix both.

**1. Install the extension** (sideload from a source checkout):

```sh
cd tools/vscode-rustcc
npm install && npm run package
code --install-extension rustcc-tools-*.vsix
```

It adds: syntax highlighting for `class` / `extern "C++"` /
`extern "Swift"` / the rustcc attributes (`#[cpp_virtual]`,
`#[constructor]`, `#[rustc_cxx_throws]`, `#[swift_value]`, …); code
snippets; a status-bar pin showing the active `rustcc` toolchain; and
Problems-pane diagnostics sourced from the `bindings.skips.json` that
`cxx_importer::Build::compile()` writes (so importer skips show up
inline).

**2. Point rust-analyzer at the patched server.** Stock RA reports
errors on every `class`. With the extension installed, run
`Cmd/Ctrl-Shift-P → rustcc: Install RA Fork (latest)` — it downloads
the prebuilt `rust-analyzer-rustcc` binary (shipped on every release)
and wires `rust-analyzer.server.path` for you. To do it by hand, see
[`fork/INSTALL.md`](fork/INSTALL.md#rust-analyzer-for-editor-support).

The patched RA gives `class` items full parity with structs: hover,
go-to-definition, find-references, completion, inherent-method
dispatch, and class-aware assists (generate `new`, change visibility,
find overriders, implement override).

## Swift interop

rustcc speaks Swift's ABI directly — no C shim. What's supported:

- **Calling Swift** — `extern "Swift"` routes through the `swiftcc`
  calling convention with Swift symbol mangling, so you can call
  `swiftc`-compiled functions (including `throws`, via
  `#[rustc_swift_throws]` + `SwiftError`).
- **Swift value types** — `#[repr(swift)]` + `#[swift_value]`
  synthesize `Drop`/`Clone` routed through Swift's value-witness table.
- **Swift classes** — bind by holding the class pointer; lifetime is
  managed with ARC (`swift_retain` / `swift_release`).

**Not supported: inheriting from Swift classes.** A Rust type cannot
subclass a Swift class, override its methods, or participate in Swift's
metadata/witness dispatch as a subclass — you can call into and hold
Swift objects, but not *be* one. (Swift subclasses require emitting
Swift type metadata + an isa layout the fork doesn't generate.)

Full reference: [`docs/swift.md`](docs/swift.md).

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
| [`docs/cxx_throws.md`](docs/cxx_throws.md) | `[[rustcc::cxx_throws]]` — catching C++ exceptions from Rust as `Result` (shim + native-invoke paths, Itanium + MSVC) |
| [`docs/repr_cpp.md`](docs/repr_cpp.md) | Rust types exposed to C++ with matching layout |
| [`docs/swift.md`](docs/swift.md) | Swift interop — `extern "Swift"`, `#[repr(swift)]`, `#[swift_value]`, `throws` |
| [`docs/build_integration.md`](docs/build_integration.md) | Driver, cargo manifest extensions, linking, toolchain detection |

## Non-goals

- **Replacing the `cxx` crate.** `cxx` remains the right tool for
  projects that want a user-space FFI layer without a forked
  compiler.
- **Silent compatibility with old Clang.** rustcc pins a Clang floor
  and rejects older toolchains at build time.
- **Subclassing imported *Swift* types from Rust.** Subclassing an
  imported *C++* polymorphic class is **supported** (v1.13.7): a Rust
  `class D : CppBase` overrides the base's virtuals — concrete and pure
  — and its virtual destructor, with C++ dispatching through a
  `CppBase*` into the Rust `override` and `delete` running the Rust
  `Drop` (see [`docs/repr_cpp.md §5`](docs/repr_cpp.md) and
  `examples/subclass_cpp_base/`). The equivalent for imported *Swift*
  classes (participating in Swift's own dispatch as a derived class)
  remains out of scope.

## License

Dual-licensed under **MIT** ([LICENSE-MIT](LICENSE-MIT)) or
**Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)) at your option,
matching the upstream rustc license.
