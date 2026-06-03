# rustcc v1.13.8 — rebased onto Rust 1.96.0 stable + deep C++ inheritance

**A fork-base release.** v1.13.8 moves the fork from the 1.97.0-dev
master snapshot it was pinned to onto **Rust 1.96.0 stable** — the
current official release — so rustcc tracks a real, reproducible
toolchain until Rust 1.97.0 ships. It also lands **deep (multi-level)
inheritance** for subclassing imported C++ classes, carries everything
from v1.13.7, and re-validates the bare-metal codegen path.

## What ships

### A. Rebased onto Rust 1.96.0 stable

`fork/build.sh` now pins **`ac68faa20c58…` (Rust 1.96.0)** and applies
`fork/patches/` (the 42-patch 1.96.0 series; the old 1.97-dev series is
archived under `fork/patches-1.97dev/`). The 1.96.0 base needs two
config tweaks `build.sh` now sets automatically: `channel = "nightly"`
(a stable tag forbids the `feature(rustc_attrs)` the fork uses) and
`download-ci-llvm = true` (a release tag has CI LLVM — much faster than
building LLVM from source). See `fork/MIGRATION-1.96.0.md` for the
3 conflict resolutions + 5 small API-drift fixes (notably the
`rustc_attr!` 2-arg shim, `FnSig` field-vs-method, `mk_fn_sig` arity,
and `From` diagnostic-item). All 42 patches apply cleanly via
`git am --3way`; stage1 builds; `./fork/tests/run.sh` is 9/9.

### B. Deep (multi-level) inheritance for subclassing imported C++ bases

A Rust `class D : Leaf` can now subclass a **deep** imported polymorphic
base — e.g. FLTK's `Fl_Text_Editor → Fl_Text_Display → Fl_Group →
Fl_Widget` — and override virtuals introduced at **any** level
(including a grandparent's pure virtual). C++ dispatching through a base
pointer at any level lands in the Rust `override`.

Workspace-only (no fork-patch change, so it applies on both the 1.96.0
and 1.97 toolchains):
- `rustc_abi_cxx::CxxTypeCtx::primary_vtable_slots` returns the fully
  flattened primary vtable with each slot's override-matching name
  resolved via its *declaring* class.
- `cxx_importer` emits the complete `#[rustc_cxx_imported_vtable]`
  (all inherited slots, correct names + final-overrider symbols) and a
  `Drop` for classes that *inherit* (not declare) a virtual destructor.

Validated end-to-end: a 3-level chain with a grandparent-pure override
dispatches correctly via every base pointer; with a concrete leaf,
`delete` through the grandparent runs the Rust `Drop` + full C++ dtor
chain + frees, each once (balanced counters).

**Limitation:** when the deepest imported base is itself **abstract**
(an unoverridden pure virtual, e.g. `Fl_Widget::draw`) *and* C++
owns/`delete`s the object, destruction needs that base's base-object
dtor (`D2`), which clang doesn't emit for a Rust-only subclass. Add a
one-line C++ force-dtor stub (a concrete subclass overriding the pure
virtuals) until the importer emits one. Dispatch itself is unaffected.

### C. Carried from v1.13.7

Subclass an imported C++ class with cross-boundary virtual dispatch
(concrete + pure overrides) and a virtual destructor; MSVC vftable
support; ARM + Intel (Itanium) validation.

## Verification

- `aarch64-apple-darwin`: stage1 builds; 9/9 probes; `subclass_cpp_base`
  (shallow) and a 3-level deep chain both green.
- **Bare-metal**: a heap-free `no_std` subclass compiles for
  `thumbv7m-none-eabi` and emits a correct `_ZTV…`/`_ZTI…` vtable in an
  ELF ARM EABI5 object — confirming the subclassing codegen is
  target-agnostic. RISC-V uses the same path + the fork's RISC-V Itanium
  overlay (patch 09); a local riscv `core` rebuild needs
  `riscv32-unknown-elf-gcc` and was not exercised this release.
- Full multi-platform toolchain build via the GitHub release workflow
  (linux x86_64/aarch64, windows-msvc x86_64/aarch64, macos aarch64).

## Notes

- The virtual-destructor ownership model uses `operator new`/`delete`
  (heap) — applicable to hosted targets, not bare-metal `no_std`.
- Single-inheritance chains only; multiple/virtual inheritance of the
  base remains future work.
