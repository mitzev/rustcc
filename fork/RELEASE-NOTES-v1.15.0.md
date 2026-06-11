# rustcc v1.15.0 — the GCC backend learns C++ interop, and an IDE to prove it

A feature release on **Rust 1.96.0 stable**. The patch series grows to
**50** patches (`0001`–`0050`). Three threads converge here: the
`rustc_codegen_gcc` backend gains the full C++-interop machinery, the
binding importer closes its inline/function-pointer shim gaps against
real-world headers, and the whole stack is exercised end-to-end by a
new capstone sample — an embedded-RTOS IDE written in fork Rust.

## 1. C++ interop on the GCC backend (patches 0047–0049)

`rustcc` no longer assumes LLVM. With `-Zcodegen-backend=gcc` the fork
emits the complete interop surface through libgccjit:

- **Class metadata** — Itanium vtables and RTTI (`_ZTV`/`_ZTI`/`_ZTS`)
  for Rust-defined classes. gccjit has no `linkonce_odr`, so weak
  semantics ride top-level asm `.weak` directives; imported
  declarations can't be upgraded to definitions mid-codegen, so
  metadata emission queues at predefine time and flushes before the
  define loop (patch 0047).
- **Constructor vptr install** — the ctor-return hook writes the
  derived address-point exactly as the LLVM backend does.
- **`cxx_throws` catch clauses** — gccjit has no
  `llvm.eh.typeid.for`, so typed catches dispatch through a small
  runtime matcher: `__rustcc_cxx_match_typeinfo(exception, typeinfos,
  n)` compares the in-flight `__cxa_exception`'s type against a
  per-landing-pad static `_ZTI` array (patch 0049).
- **cg_ssa relaxation** — `transmute_scalar`'s strict type pre-check
  dropped for typed-pointer backends (patch 0048).

The pure-GCC pipeline is proven end to end on CI: a g++-compiled C++
base, subclassed by Rust compiled **entirely through the GCC
backend**, with virtual dispatch, RTTI chains, and typed exception
catches all landing.

Alongside the backend, **g++/libstdc++ is now first-class on Linux**:
the importer test harness honors `$CXX`, and dedicated CI legs build
and run the subclass + throws examples with the system GNU toolchain.

## 2. Importer: the inline-shim gaps close (M15.c / M15.d)

Binding FLTK's full surface kept finding the same disease in new
forms — a Rust declaration whose C++ symbol nothing defines:

- **M15.c — function-pointer params/returns.** Header-inline methods
  like `Fl_Widget::callback(Fl_Callback*, void*)` (and the getter
  returning a fn-ptr) now get real trampolines: pointer-to-function
  renders as a type-id (`R (*)(A, B)`, trailing-return for returns),
  parameter declarators splice the name (`R (*name)(A, B)`,
  `R (*&name)(A)` for references).
- **M15.d — header-inline free functions.** FLTK's whole `fl_draw`
  API (`fl_rectf`, `fl_polygon`, `fl_arc`, …) is one-line inline
  wrappers with no out-of-line symbols. `FreeFnDef` records
  `is_inline`, the shim generator emits `__rustcc_shim_` trampolines
  (instantiating the inline definition), and the bindings route
  through them — with one skip predicate shared by both sides so a
  routed extern always has a shim behind it.
- **Variadic mangling fix** — Itanium symbols for variadic
  functions/methods were missing the trailing `z` (every one was
  unlinkable); found by calling `fl_choice`.

Deliberate refusals are documented where the IR can't distinguish
`long` from `long long` (exact spelling matters inside function types
and overload sets).

The new Linux legs immediately earned their keep: the g++ run of
`examples/member_fn_ptr` exposed that the v1.14 PMF-return
exemption was missing from the Itanium **x86-64** callconv overlay
(present on AArch64 + MSVC) — Rust passed a hidden sret buffer that
g++, returning the pair in RAX:RDX, never wrote. Fixed as patch
0050 and re-validated end to end on g++/Linux.

## 3. Validation matrix: bare metal + FreeRTOS, executed on qemu

- **Bare-metal subclassing** (`examples/bare_metal_arm`): Rust class
  hierarchies *including a Rust subclass of an imported g++-compiled
  base* run heap-free on Cortex-M4 (qemu `mps2-an386`) and RISC-V
  rv32 (qemu `virt`) — vtables, ctor vptr install, and cross-compiler
  RTTI verified by executed dispatch, not just by linking.
- **FreeRTOS V11.2.0** (`examples/freertos_cpp`): the same dispatch
  checks run as preemptive RTOS tasks with forced context switches on
  four flavors — STM32-class **ARM_CM4F**, ESP32-C3-class
  **rv32imac**, **ESP32-C2 ISA-exact rv32imc** (qemu CPU runs with
  the A extension *disabled*, so a single stray atomic would trap),
  and Raspberry Pi Pico-class **ARM_CM0**.
- MSVC runtime, multi-arch, and importer suites re-validated.

## 4. The rustcc IDE (`examples/rustcc_ide`)

The capstone sample: a complete embedded-RTOS IDE **written in fork
Rust**, where nearly every widget is a `class` subclassing imported
FLTK — `RustEditor : Fl_Text_Editor`, `FileNav : Fl_Hold_Browser`,
`FileTabs : Fl_Tabs` (with native close buttons), `FindBar` /
`ReplaceBar` / `WatchInput : Fl_Input`, and `IconButton : Fl_Button`
whose `draw()` override paints the toolbar's vector icon set with
`fl_draw` primitives.

It scaffolds per-board projects (RAK11161 dual-core, STM32, ESP32,
Pico, Host), builds and **executes firmware under qemu** with live
console streaming, flashes via configurable tools
(`STM32_Programmer_CLI` / `esptool.py` / `picotool`), and drives an
**in-IDE lldb debugger**: breakpoints (red), current-line highlight
(amber), step in/over/out, a Variables pane that re-captures locals
on every stop, and a watch box that reads globals via
`target variable`. Draggable splitters, tabs, find/replace,
autocompletion, per-project Target persistence.

Its self-test is the broadest fork test in the repo: headless checks
plus a FULL gate that builds CM4 firmware, runs it under qemu to the
`PASS` line, and scripts a complete lldb session — breakpoint → step
into the fork-emitted C++ constructor (`Greeter::Greeter(excitement=3)`
with the parameter readable) → locals captured → a global read by
name → clean exit.

## Install

Prebuilt stage-1 tarballs attach to this release
(`rustcc-aarch64-apple-darwin.tar.xz`,
`rustcc-x86_64-unknown-linux-gnu.tar.xz`). Extract and link:

```sh
tar xJf rustcc-<target>.tar.xz
rustup toolchain link rustcc <extracted>/stage1
```

Full recipe and from-source instructions: `fork/INSTALL.md`.
Authoritative per-patch spec: `fork/PATCHES.md`.
