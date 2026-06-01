# rustcc v1.13.4 — importer robustness, Swift demos, tooling & docs

**A host-side release.** v1.13.4 hardens the C++→Rust binding generator
against real-world STL-heavy headers, adds Swift interop demos + docs,
ships a VS Code **New Project** command, and refreshes the
documentation. **The fork rustc patches are unchanged from v1.13.0**, so
the prebuilt toolchain tarballs are byte-identical to v1.13.0's — if you
already run the v1.13.0 (or later) fork rustc, you only need the updated
workspace crates / tooling.

## What ships

### A. `cxx_importer` robustness — survives the STL

A user header that `#include`s `<stdexcept>` (or any STL header), or a
type that derives from a system class like `std::exception`, used to
drag the entire reachable STL graph into the generated bindings and emit
it as un-compilable Rust — ~hundreds of errors on Linux (libstdc++ on
the default include path), while macOS happened to dodge it. Five
fixes, layered, make `Build::compile` robust:

- **`long double` is now lowered** (`TypeKind::LongDouble` →
  `FloatKind::LongDouble`) instead of aborting the import with
  "unsupported clang type kind" — it appears in libc's `max_align_t` on
  x86-Linux. (rustc_abi_cxx already had the layout/mangling.)
- **Unrepresentable template arguments** (parameter packs,
  template-template args — e.g. `std::conjunction`) now **skip just that
  type** via a poison node rather than aborting the whole translation
  unit.
- **System-header types are not eagerly imported**: `walk_top_level`
  and the auto-instantiate discovery skip class/enum/typedef/free-fn
  declarations in `<...>` headers; recursively-reached system types
  (e.g. a `: std::exception` base) bind as **opaque** without recursing
  into their internals. Types the user actually *uses* still arrive via
  recursion / explicit instantiation.
- **`[[clang::annotate("rustcc::skip")]]` now prunes at import time** —
  a skipped class's base/field graph is no longer walked.
- The bindings emitter **skips un-nameable (empty-name) classes** rather
  than failing the whole emission.

Net effect: `examples/cxx_throws_demo` now builds + runs on Linux, and
real STL-deriving headers import cleanly. New regression tests:
`imports_long_double_field`,
`unsupported_template_arg_skips_type_without_aborting_import`,
`skip_prunes_base_graph`, plus the `compile_auto_instantiates_*` path.

### B. Swift interop — demos + design doc

- New runtime probes `fork/tests/class_keyword/swift_extern_call`
  (the `extern "Swift"` / swiftcc calling convention + argument labels)
  and `swift_value_type` (a `#[swift_value]` **value** type routed
  through the value-witness table). The class-keyword probe matrix is
  now 9/9 (4 cpp_class + 5 swift).
- New **`docs/swift.md`** — the first dedicated Swift reference:
  `extern "Swift"`, `#[repr(swift)]`, `#[swift_value]` (value vs class),
  `#[rustc_swift_throws]` + `SwiftError`, mangling, and limitations
  (notably: you can *call* Swift and hold Swift class instances, but a
  Rust type cannot *subclass* a Swift class).

### C. Tooling

- **VS Code extension v0.1.3**: new **`rustcc: New Project`** command
  (scaffolds a toolchain-pinned, debuggable project — `.gitignore`,
  CodeLLDB `launch.json`, recommended extensions — the way `rustcc init`
  does + the bits it skips); fixed the RA-fork download repo
  (`rustcc/rustcc` → `mitzev/rustcc`); class-keyword starter now adds
  `#![allow(internal_features)]` so scaffolded projects build
  warning-free.
- **`fork/build.sh`** installs the Rust LLDB pretty-printers into the
  stage-1 sysroot, so fork-built binaries debug out of the box (no more
  "Could not find LLDB data formatters").
- **`fork/tests/run.sh` / `run_targets.sh`** default `$RUSTC` to
  `~/rust-lang-rust-fork` (matching `build.sh`/`INSTALL.md`), so the
  probe runners work with no `RUSTC` override.
- **CI**: installs libc++/libc++abi and compiles the `cxx_throws` shim
  tests with `-stdlib=libc++`, so the Linux workspace test job is green.

### D. Documentation

- **README + `fork/INSTALL.md`** refreshed to v1.13.x reality: a VS Code
  / editor-setup section, a Swift section, the feature matrix updated to
  cumulative-and-shipped (MSVC ABI, MI/VTT, bit-fields, copy/move,
  templates+NTTP, cxx_throws), the stale "future roadmap" replaced with
  an honest remaining-gaps list, prebuilt-triple matrix corrected, and
  repo URLs fixed (`mitzev/rustcc`).
- **`CLAUDE.md`** added so Claude Code defaults to the `class` syntax in
  fork-toolchain code while leaving the plain-Rust infrastructure crates
  alone.

## Test status

Full workspace green; the `fork/tests/class_keyword` probe matrix is
9/9 (4 cpp_class + 5 swift). `cxx_importer` suite green including the
new importer-robustness regression tests.

## Toolchain

No fork patch changes. The `fork/patches/` set is identical to v1.13.0
(patches 01–36). Prebuilt toolchain tarballs for v1.13.4 are
byte-identical to v1.13.0's; users on a v1.13.x toolchain need only
update the workspace crates / tooling.
