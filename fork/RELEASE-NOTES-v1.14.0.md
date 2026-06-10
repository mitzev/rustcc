# rustcc v1.14.0 — zero-boilerplate classes, construct-in-place, full FLTK

A feature release on **Rust 1.96.0 stable**. The patch series grows to
**46** patches (`0001`–`0046`). This release folds in the unreleased
v1.13.10 correctness campaign (see `RELEASE-NOTES-v1.13.10.md` for the
deep-dive on those fixes) and adds the v1.14 feature work on top.

The headline: a Rust `class` is now boilerplate-free at the crate root,
constructs **at its final address** like C++, and the importer binds
real-world C++ libraries (FLTK 1.4) completely — inline methods,
macros, enums, nested records, and global data included.

## New in v1.14

### 1. Zero-boilerplate crate roots

The fork's interop attributes (`#[cpp_virtual]`, `#[constructor]`,
`#[rustc_cxx_throws]`, `#[rustc_cxx_imported_vtable]`, …) are **ungated
built-ins**: `#![feature(rustc_attrs)]` and
`#![allow(internal_features)]` are no longer needed (or useful) in fork
crates. Generated bindings are wrapped in a `__rustcc_ffi` module that
carries its own lint allows, so `#![allow(dead_code)]` is gone too. A
minimal fork crate is now literally:

```rust
pub class Widget {
    x: i32,

    #[constructor]
    pub fn new(x: i32) -> Self { Self { x } }

    #[cpp_virtual]
    pub fn poke(&self) -> i32 { self.x }
}
```

### 2. Construct-in-place (`cxx_ctor_inplace` MIR pass)

C++ constructors may *escape `this`* — FLTK's `Fl_Text_Display` ctor
creates child scrollbars whose back-pointers capture the object's
address. Rust's construct-then-bitwise-move protocol left those
dangling; the documented workaround was a manual re-parent fix-up.

The new MIR pass removes the temporaries along the whole construction
chain:

- `ptr.write(D::new(..))` constructs directly into `*ptr`;
- a `#[constructor]` body's `Self { __base: Base::new(..), .. }`
  constructs the base directly into the base subobject;
- an imported binding's by-value `new` constructs into its sret return
  slot (`&raw mut _0`).

Ctor-time self-references are born at the final address. The advanced
FLTK editor's children-parent-pointer probe flips from DANGLED to
VALID with **no fix-up** — the last documented workaround is gone.
(`new_at` placement ctors remain available for C++-side placement.)

### 3. Member function pointers (MI phase 1)

`CxxMemberFnPtr<T>` (Itanium `{ptr_or_voff, adj}` pair) with
`M<class>F…E` / `P F…E` manglings, parameter substitutions, and the
ABI triviality exemption so the pair passes by value exactly like
clang's. Covers non-virtual and virtual members, null checks, and
passing Rust member fns to C++ callback registries. End-to-end example:
`examples/member_fn_ptr`. This is phase 1 of the multiple-inheritance
roadmap (`fork/MI-DESIGN-v1.14.md`); the importer now also guards
MI/virtual-base chains with a clear diagnostic instead of silently
mis-binding.

### 4. Full FLTK binding coverage (importer)

- **Header-inline methods** — bound via per-method shim trampolines in
  the generated shim TU (441 FLTK methods, including
  `Fl_Widget::callback`). Inline *free functions* (e.g. `fl_color`)
  still need a hand anchor — tracked.
- **`#define` constants** — a macro-scrape pre-pass emits 140 FLTK
  constants (`FL_CTRL`, …) as typed `pub const`s.
- **Class-scope enums** — named nested enums flatten to transparent
  structs with associated consts; **anonymous enums** emit prefixed
  consts (`Fl_Text_Display_WRAP_AT_BOUNDS`).
- **Nested records** — `Fl_Text_Display::Style_Table_Entry` and
  friends flatten to `Outer_Inner` structs (synthetic anonymous-union
  segments filtered), enabling FLTK syntax highlighting from Rust.
- **Raw global statics** — TU/namespace `VarDecl`s bind as
  `pub static` in `unsafe extern "C++"` blocks with correct manglings.
- **Evaluated default-arg values** and `new_at` placement-ctor
  siblings ride along from the v1.13.10 line.

### 5. Examples

- **`examples/fltk_text_editor`** — a TextEdit-equivalent editor in
  ~600 lines of fork Rust: menus, native file chooser, find bar
  (`Fl_Input` subclass), undo/redo, wrap, dirty-title tracking, and
  syntax highlighting via `highlight_data` + the nested
  `Style_Table_Entry` record. Headless `--self-test` covers the lot.
- **`examples/fltk_editor_advanced`** — Rust subclasses of
  `Fl_Text_Editor` and `Fl_Box` with virtual `handle`/`draw`/`resize`
  overrides, a paint probe (`fl_draw` text into a custom `draw`), and
  the construct-in-place probe. No fix-ups.
- **`examples/member_fn_ptr`** — member-pointer round-trips, virtual
  member pointers, null member pointers, Rust member fns dispatched
  from C++.

### 6. Tooling

- **VSCode plugin 0.2.0** — scaffold templates updated for
  zero-boilerplate crate roots.
- `rustcc-stage1` rustup link + `fork/tests/run_msvc_runtime.sh`
  validated against this series.

## Folded in from the (untagged) v1.13.10 campaign

Correctness fixes, each with regression coverage — full details in
`RELEASE-NOTES-v1.13.10.md`:

1. **Virtual destructor slots at their declaration position** (the
   `[early, D1, D0, late]` layout bug — C++ calling `early()` on a
   Rust subclass could dispatch into the destructor).
2. **Signature-carrying vtable slots** — overrides are matched and
   *checked* against the base slot's parameter signature; overloaded
   virtuals bind correctly.
3. **Collector emits vtable-referenced drop glue at `-O3`** (no more
   `codegen-units = 1` pin / force-function workarounds).
4. **MSVC deleting-destructor (`??_G`) correctness** + most-derived
   dtor selection and overload-order fixes.
5. **Doc comments allowed on class-body methods** (parser).
6. **`new_at` placement constructors** + soundness markers in the
   generated bindings.
7. Importer keeps **non-public virtuals** as (non-callable) vtable
   slots so derived layouts stay correct.

## Validation

- 11 fork probes (`fork/tests/run.sh`) — classes, virtuals, throws,
  Swift value types.
- End-to-end demos: `subclass_cpp_base`, `subclass_cpp_deep`
  (3-level), `subclass_dtor_position`, `member_fn_ptr`,
  `virtual_override`, both FLTK editors (`--self-test` + GUI).
- **MSVC**: mangling smoke + 7 PE32+ runtime binaries (lld-link +
  xwin) all passing under Wine — polymorphic dispatch, virtual dtor,
  override, typed C++ exceptions.
- **Bare metal**: `thumbv7em-none-eabihf` firmware links with zero
  undefined symbols; vtable/typeinfo/ctor symbols verified in the ELF.
- **Intel**: `subclass_cpp_base` end-to-end under Rosetta
  (`x86_64-apple-darwin`).
- **ARM64 macOS** (host): full battery.
- Workspace: `cargo test --workspace` + `cxx_importer` suite with
  libclang.

## Breaking / migration notes

- Crates that still carry `#![feature(rustc_attrs)]` /
  `#![allow(internal_features)]` build fine, but the attributes are
  now unnecessary; remove them. (On pre-v1.14 toolchains they're still
  required.)
- C++ declarations of Rust `fn (&self)` methods must be
  `const`-qualified (`_ZNK…`) — the signature-carrying slots make the
  mismatch a hard link error instead of silent UB (caught one in our
  own `bare_metal_arm` example).
- Generated bindings now live inside a `__rustcc_ffi` module and are
  re-exported — `include!` consumers are unaffected; code that named
  the module explicitly must use the re-exports.
