# Fork rustc patches — MSVC ABI (v1.09.0 B.4)

This document is the per-patch design plan for the rustc fork
changes that complete v1.09.0's MSVC C++ ABI support. The patches
in this list land in `fork/patches/` after the rust-lang/rust tree
is checked out + the existing Itanium patches (`01-…` through
`15-cpp-abi-force-sret-record-ret.patch`) apply cleanly.

These patches are sequenced so each can be applied independently
(verify with `./fork/build.sh --apply-only`) and each builds + tests
on its own once dependencies are in place.

## Patch series

### `16-msvc-abi-target-routing.patch`

**Goal**: teach `rustc_abi_cxx_bridge` (the in-tree shim that
bridges fork rustc to the out-of-tree `rustc_abi_cxx` crate) to
pick the right ABI flavor based on the target triple.

**Touches**:
- `compiler/rustc_abi_cxx_bridge/src/ctx.rs` — propagate
  `target.options.is_like_msvc` into the constructed
  `rustc_abi_cxx::Target::abi_flavor`.
- `compiler/rustc_session/src/config.rs` — sanity-check that
  `class`-keyword targets see a matching `abi_flavor`.

**Test**: extend the in-tree `tests/ui/rustcc/abi-flavor-routing.rs`
to compile a single `class C { virtual void f(); };` declaration
against `x86_64-pc-windows-msvc` and confirm the emitted `.o`
carries MSVC-mangled symbols.

**LoC estimate**: 80–120.

### `17-msvc-vtable-emission.patch`

**Goal**: replace the Itanium vtable emission path with an
ABI-routing one. The MSVC variant emits per-base subobject
vftables instead of Itanium's primary+secondary subtable layout
in a single `_ZTV<class>` global.

**Touches**:
- `compiler/rustc_codegen_llvm/src/cpp_class/vtable.rs` (a new
  module created in patch 07; this patch adds the MSVC branch).
- The codegen call sites that take `ctx.vtable(class)` already
  route through the dispatcher introduced in this sprint — no
  call-site changes needed; only the inside of the codegen path
  needs to know which symbol shape to emit.

**Test**:
- `tests/codegen/rustcc/vtable-msvc.rs` — compile a polymorphic
  class for an MSVC target, check that the emitted IR has a
  single `??_7Class@@6B@` global and that the address-point
  offset is 0 (not `pointer_width * 2` as for Itanium).

**LoC estimate**: 400–600.

### `18-msvc-dtor-single-slot.patch`

**Goal**: collapse the D1/D0/D2 dtor variants to MSVC's single
"scalar deleting destructor" (`??_G`) + base dtor (`??1`)
convention. The vftable's dtor slot points at the scalar deleting
form which takes a hidden `int __flags` parameter; the runtime
calls `delete this` when bit 1 is set.

**Touches**:
- `compiler/rustc_mir_transform/src/cpp_class/dtor_synth.rs`
  (added in patch 09 — modify for MSVC routing).
- `compiler/rustc_codegen_llvm/src/cpp_class/dtor.rs` — emit the
  scalar-deleting wrapper that fans out to base dtor + optional
  delete call.

**Test**:
- `tests/codegen/rustcc/dtor-msvc.rs` — compile a class with a
  virtual dtor; check that the IR has both `??1Class@@` (base
  form) and `??_GClass@@` (scalar deleting form), and that the
  vtable slot points at the deleting form.

**LoC estimate**: 500–800.

### `19-msvc-sret-rcx-routing.patch`

**Goal**: x86_64-pc-windows-msvc passes the sret pointer in RCX,
not RDI. Existing patch `15-cpp-abi-force-sret-record-ret.patch`
handles the Itanium "RDI" convention; this patch teaches the same
codepath to look at `target.options.is_like_msvc` and route to
RCX.

**Touches**:
- The single function added by patch 15
  (`force_sret_for_record_returns` or similar).
- `compiler/rustc_target/src/abi/call/x86_64_win64.rs` — already
  in rust-lang/rust upstream, but the fork's `extern "C++"` ABI
  shim needs to route through it.

**Test**:
- Compile `struct Big { int a[8]; }; Big f();` against
  x86_64-msvc; check the LLVM IR shows `Big* sret %0` as the
  first parameter and that the caller passes it in RCX.

**LoC estimate**: 150–250.

### `20-msvc-seh-personality.patch` (the long pole)

**Goal**: emit MSVC-flavored SEH exception lowering for `class`
items, including a `personality(__CxxFrameHandler3)` clause on
emitted functions that may throw or catch C++ exceptions.
Itanium's `__cxa_throw` / `__cxa_begin_catch` runtime is
replaced by `_CxxThrowException` + funclet-based EH.

**Touches**:
- `compiler/rustc_codegen_llvm/src/intrinsic.rs` — recognize
  `extern "C++"` `throw` calls and route to `_CxxThrowException`
  on MSVC targets.
- `compiler/rustc_codegen_llvm/src/llvm_util.rs` — set the
  personality function based on `target.options.is_like_msvc`.
- `compiler/rustc_mir_transform/src/cpp_class/throw_lowering.rs`
  (new module) — funclet-vs-landingpad branching.

**Test**:
- Wine-based runtime test on the CI runner (`brew install wine`
  on macOS-13, choco install on windows-latest): compile a small
  `try { throw Foo(); } catch (Foo&) { ... }` example and
  assert the exception is caught.
- LLVM IR diff test: confirm the emitted IR uses
  `cleanuppad` / `catchswitch` / `catchpad` instructions
  (funclet EH) rather than `landingpad` (Itanium EH) when
  targeting MSVC.

**LoC estimate**: 2500–4000. This is the multi-week piece.

### `21-msvc-operator-new-delete.patch`

**Goal**: when emitting `__cxx_<class>_new_heap_<i>` thunks in
shim code, the underlying call to `operator new` must use MSVC's
mangling (`??2@YAPEAX_K@Z`) instead of Itanium's (`_Znwm`).

**Touches**:
- `crates/cxx_importer/src/shims.rs` — emit the right operator
  new/delete signature based on `ctx.target().abi_flavor`.
- Generated `cxx_shims.cpp` includes `<new>` (works on both ABIs
  via the standard library implementations).

**Test**:
- `tests/codegen/rustcc/heap-thunks-msvc.rs` — compile a class
  with the heap-alloc opt-in; check that the C++ shim source
  contains explicit `operator new(size_t)` calls (the C++
  compiler handles MSVC mangling natively).

**LoC estimate**: 100–200.

### `22-msvc-dllexport-dllimport.patch`

**Goal**: when a `#[repr(cpp)]` class is exported from a Rust
crate that targets `x86_64-pc-windows-msvc`, attach `dllexport`
to its emitted symbols (vftable, methods, dtor, RTTI). Importers
get `dllimport` automatically via the standard cdylib mechanism.

This patch is the smallest of the series in terms of new code but
requires coordinating with `#[no_mangle]` / `#[link_name]`
behavior.

**Touches**:
- `compiler/rustc_codegen_llvm/src/declare.rs` — set
  `dllimport_storage_class` / `dllexport_storage_class` based
  on target + `extern_crate` boundary.
- `compiler/rustc_passes/src/lib.rs` — diagnostic for "missing
  dllexport on cross-crate class".

**Test**:
- Build a `cdylib` crate exposing `#[repr(cpp)] class Widget`,
  link it into a separate consumer crate, confirm symbols
  resolve at link time on x86_64-pc-windows-msvc.

**LoC estimate**: 200–300.

## Ordering + dependencies

```
16 (routing)
 ├── 17 (vtable emission) ── 18 (dtor)
 ├── 19 (sret)
 ├── 20 (SEH) ────────────────── depends on 16
 ├── 21 (operator new)
 └── 22 (dll storage)
```

16 must land first (it's the routing layer); the others can
land in any order though 17 → 18 makes logical sense
(vtable emission gates the dtor slot's behavior).

## Per-patch LoC + time estimates

| Patch | LoC | Time (focused) | Time (agent-accelerated) |
|---|---|---|---|
| 16 routing | 80–120 | 2–3 days | 1 day |
| 17 vtable | 400–600 | 1 week | 2–3 days |
| 18 dtor | 500–800 | 1.5 weeks | 3–5 days |
| 19 sret | 150–250 | 3 days | 1 day |
| 20 SEH | 2500–4000 | 4–6 weeks | 1–2 weeks |
| 21 operator new | 100–200 | 2 days | 1 day |
| 22 dll storage | 200–300 | 3 days | 1 day |
| **Total** | **~6000** | **~8 weeks** | **~3 weeks** |

These slot into Phase 2's overall 3–4 month estimate. Patch 20
is the long pole; everything else is mechanical once 16 + 17 are
in place.

## Test infrastructure

Patches 17 + 18 + 19 + 21 are testable with `tests/codegen/`
filecheck tests against the LLVM IR — no Windows runner needed.
Patch 20 (SEH) and patch 22 (dll storage) are the only ones that
need a Windows or Wine environment.

The CI matrix added in patch 23 (= B.6) ships a `windows-latest`
runner that exercises the full chain end-to-end. Until then,
the Mac dev loop uses:

```bash
cargo +rustcc-stage1 build --target x86_64-pc-windows-msvc
clang -target x86_64-pc-windows-msvc -fms-compatibility \
      -c rustcc-stage1-out/foo.s -o foo.obj
lld-link foo.obj /entry:_start /subsystem:console
wine foo.exe
```

`brew install lld wine llvm` gets all of these.

## Open questions

These should be settled before patch 17 starts:

1. **Vtable RTTI layout** — MSVC's COL is at vftable[-1] but the
   full RTTI tree (type descriptor, class hierarchy descriptor,
   complete object locator) involves multiple separate symbols
   per class. Patch 17 needs to emit all of them or use
   `__declspec(thread)`-style late binding. Picking: **emit all
   four** (`??_R0`, `??_R1`, `??_R2`, `??_R3`, `??_R4` symbol
   families) because `dynamic_cast` won't work otherwise.

2. **Member-pointer representation** — MSVC's data-member
   pointers are 4 bytes (single offset); function-member
   pointers are 8/12/16 bytes depending on inheritance kind.
   Itanium uses a uniform `{ ptrdiff_t offset; ptrdiff_t adj }`
   pair. Patch 17 needs to emit the right one. Decision: **add
   `Target::member_pointer_repr` field** so layout + codegen
   stay aligned.

3. **Whether to ship `_RTC*` runtime check symbols by default**.
   MSVC's `/RTC1` flag emits `_RTC_CheckEsp`, `_RTC_CheckStackVars`,
   etc., calls. Recommendation: **off by default**, opt-in via
   `RUSTFLAGS="-C target-feature=+rtc"`.

## Status

Skeleton committed during the v1.09.0 overnight sprint
(2026-05-22). No patches land until the rust-lang/rust tree is
checked out (`./fork/build.sh --apply-only` first). When that
sprint kicks off, patches 16 + 17 are the first targets.
