# rustcc v1.13.10 — correctness campaign: vtable fidelity + soundness

A bug-fix release driven by a full project review (4 parallel audits of
`rustc_abi_cxx`, `cxx_importer`, the fork patches, and the `cxx`
runtime) plus the advanced FLTK editor sample built to stress the fork.
Every fix below was reproduced first and is covered by a regression
test or runnable example. Base toolchain unchanged: **Rust 1.96.0
stable**, patch series now **43** patches (new `0043`).

## Fixed

### 1. Destructor slots at their declaration position (HIGH)

Itanium places the `D1`/`D0` pair at the virtual destructor's
*declaration* position; rustcc pinned it to the front of the vtable.
For `struct B { virtual int early(); virtual ~B(); virtual int late(); }`
clang lays out `[early, D1, D0, late]` — the old fork emitted
`[D1, D0, early, late]`, so **C++ calling `early()` on a Rust subclass
dispatched into the destructor**. A destructor introduced mid-chain was
dropped entirely. (Invisible until now because every test/example —
and FLTK — declares the dtor first.)

- `rustc_abi_cxx` models the pair at its declaration position (any
  chain level); corpus tests pinned against
  `clang++ -fdump-vtable-layouts`.
- The attr gains a positional `slot=~dtor,~` record; the fork expands
  the pair in place. Legacy `vdtor=1`-only attrs keep the prepend
  semantics (correct exactly when the dtor is declared first), so
  **existing bindings keep working on the new toolchain**.
- Operator/conversion virtuals — previously silently dropped, shifting
  every later slot — now emit `~op<N>` placeholder records.
- New runnable proof: `examples/subclass_dtor_position/`.

### 2. Signature-checked overrides + overload disambiguation

`slot=` records carry a third field: the Itanium parameter encoding at
the declaring class (`slot=set,_ZN3Ovl3setEi,i`). The fork now matches
overrides by **name + parameters**:

- Overloaded base virtuals route correctly (name-only matching used to
  clobber an arbitrary overload's slot — validated with a
  `set(double)`/`set(int)` pair where the wrong one was declared first).
- An `override fn` whose parameters match no overload is a **compile
  error** ("`override fn get`'s parameters (Itanium encoding `f`) match
  no base virtual `get` overload (base declares: `(v)`)") instead of a
  silently ABI-corrupted slot.

### 3. Drop glue undefined at `-C opt-level >= 1`

The C++ vtable references a class's `drop_in_place` **by symbol** — a
codegen-time reference MIR-based `LocalCopy` placement can't see, so
optimized builds linked with `undefined symbol: drop_in_place<T>`.
`instantiation_mode` now forces a GloballyShared (COMDAT) instantiation
for `#[repr(cpp)]`+virtual-dtor drop glue, and the collector roots it
as an indirect use (like trait-object vtables). The advanced FLTK
editor now builds at stock release settings with no workarounds.

### 4. MSVC scalar-deleting destructor (`??_G`)

- The `??_G` thunk (and the exported trampoline) now **honor the flags
  argument** — bit 0 gates `operator delete`. MSVC calls the slot with
  flags=0 for destroy-without-free; unconditionally freeing corrupted
  the heap.
- The vftable dtor slot now carries the actual `??_G…UEAAPEAXI@Z`
  symbol (was the plain `??1` dtor — `delete p;` never freed) and
  always targets the **most-derived** class (implicit dtors resolved
  to the base before).
- Adjacent same-name overloads reverse per MSVC convention
  (`h(int); h(double); k()` → `[h(double), h(int), k]`).
- All pinned against `clang-cl -fdump-vtable-layouts`.

### 5. Soundness (crates/cxx + generated bindings)

- **`new_at` placement constructors**: every imported-class ctor now
  has an `unsafe fn new_at(this: *mut Self, …)` sibling that runs the
  C++ ctor at the FINAL address — for constructors that escape `this`
  (self-registration, internal children with back-pointers), where the
  by-value `new` + move could leave dangling self-references.
- **Imported types are no longer auto-`Send + Sync`**: generated
  structs carry `PhantomData<*mut u8>` (layout unchanged). C++ objects
  are not thread-portable by default; opt back in downstream where
  audited.
- **`CxxRawError` can no longer cause UB from safe code**: fields are
  crate-private, construction is `unsafe fn new`, reads go through
  `kind()`/`message_ptr()`. (Safe code could previously build one with
  a garbage pointer and feed it to the safe `From` impl, which runs
  `CStr::from_ptr`.)

### 6. Doc comments on `class`-body methods

`/// docs` before a method (including the `virtual`/`override`/
`constructor` keyword forms) no longer errors with "fields must appear
before methods"; the docs attach to the method.

## Compatibility

- **Old bindings → new toolchain: fine** (legacy attr forms preserved).
- **New bindings → old toolchain: NOT supported** — the 3-field
  `slot=` records and `~dtor` markers require the v1.13.10 fork.
  Regenerate bindings when upgrading, as usual.

## Validation

11/11 fork probes; `subclass_cpp_base`, `subclass_cpp_deep`,
`subclass_dtor_position` (new), overload + mismatch e2e probes;
`fltk_editor_advanced --self-test` ALL OK at default release settings;
full workspace test suite; all 43 patches apply cleanly onto the
1.96.0 base.
