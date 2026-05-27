# rustcc v1.13.3 — C++ templates: NTTPs + auto-instantiation

**A bindings-generator + ABI-library release.** v1.13.3 closes the
template-support gap that was the dominant blocker for adopting real
C++ libraries: non-type template parameters now mangle correctly, the
importer captures integral template arguments, and `Build` discovers +
force-instantiates referenced specializations automatically so the
common case needs no hand-written list.

All changes are in the host-side workspace crates (`rustc_abi_cxx`,
`cxx_importer`); **the fork rustc patches are unchanged from v1.13.0**,
so the prebuilt toolchain tarballs are byte-identical to v1.13.0's. If
you already run the v1.13.0 fork rustc you only need to update the
workspace crates.

This release also ships the previously-untagged **v1.13.2** work (move
constructor + copy/move assignment bindings; Itanium VTT + construction
vtables) — see the bottom section.

## What ships (v1.13.3)

### A. Non-type template parameters (NTTPs) — both ABIs, clang-validated

`TemplateArg` gained `Integral { value, ty }` and `Template(NestedName)`
variants. The Itanium and MSVC manglers encode them to match clang
exactly:

| C++                  | Itanium             | MSVC                  |
|----------------------|---------------------|-----------------------|
| `Arr<int, 4>`        | `3ArrIiLi4EE`       | `?$Arr@H$03@`         |
| `Arr<int, -1>`       | `3ArrIiLin1EE`      | `?$Arr@H$0?0@`        |
| `Flag<true>`         | `4FlagILb1EE`       | `?$Flag@$00@`         |
| `CharBox<'A'>`       | `7CharBoxILc65EE`   | `?$CharBox@$0EB@@`    |
| `SizeArr<int, 4ull>` | `7SizeArrIiLy4EE`   | `?$SizeArr@H$03@`     |
| `Stack<int, Box>`    | `5StackIi3BoxE`     | `?$Stack@HUBox@@@`    |

Itanium encodes integral args as the `L <type> <value> E` literal form
(negative → `n` prefix on the magnitude); MSVC as `$0<number>` (single
digit `0`-`9` for magnitudes 1..=10, base-16 `A`-`P` nibbles otherwise,
leading `?` for negatives). Template-template args mangle as the bare
name prefix (Itanium) / a `U<name>@@` struct-tag reference (MSVC). New
clang-validated golden fixtures: `corpus/mangle_templates_nttp` and
`corpus_msvc/mangle_templates_nttp`.

> Note: the IR collapses C++ `long` and `long long` to one 64-bit width
> (a pre-existing, codebase-wide choice — see `int_code`), so a `size_t`
> NTTP mangles as `y` (unsigned long long) rather than `m` (unsigned
> long). This affects `size_t`-parameterised specializations only.

### B. Importer captures integral template arguments

`lower_template_args` now reads libclang's cursor-based
`get_template_arguments()` (which surfaces NTTP *values*, unlike the
type-only `get_template_argument_types()`), pairing each integral arg
with its declared parameter type recovered from the primary template's
parameter list. End-to-end test: importing `template struct Arr<int,3>`
yields a `TemplateSpec` with `[Type(int), Integral{3, int}]` that
round-trips to clang's `_ZNK3ArrIiLi3EE3getEv`.

Template-template / pointer-to-member / nullptr / pack arguments aren't
recoverable from libclang's type view (the `clang` crate's `Template`
variant carries no name), so the importer **rejects** them with a clear
`UnsupportedFeature` diagnostic rather than silently mis-mangling — a
strict improvement over the previous blanket rejection of *all* non-type
arguments.

### C. Auto-instantiation in `Build` — templates "just work"

`Build` (the `build.rs` orchestrator) gained two methods and an
on-by-default discovery pass:

```rust
cxx_importer::build::Build::new()
    .header("api.hpp")
    // auto_instantiate is ON by default — std::vector<int> in a
    // signature is discovered + force-instantiated automatically.
    .instantiate("std::map<int, MyValue>")  // explicit, for specs the scan can't see
    .auto_instantiate(true)                  // or .auto_instantiate(false) to opt out
    .compile("mylib");
```

`compile()` pre-scans the headers for specializations referenced by
value / pointer / reference, force-instantiates them (plus STL helper
specs — allocator/pair/default_delete), and imports them as concrete
classes. End-to-end test: a `Wrapper<int>*`-typed field (referenced by
pointer, so *not* implicitly instantiated) is discovered and emitted as
a concrete `Wrapper_i32` with its `unwrap()` method bound
(`_ZNK7WrapperIiE6unwrapEv`); with auto-instantiate off it stays an
opaque forward-decl.

### D. Compound-`T` spec-method substitution — verified + locked in

Methods of a specialization whose signatures use the parameter as `T`,
`T*`, `const T&`, or `T&&` substitute correctly (libclang reports them
as structured pointer/reference types over the `Unexposed` leaf, which
the existing recursion resolves). This is now covered by a regression
test. The one remaining case — a method mentioning a *nested* template
specialization parameterised on `T` (e.g. returning `Box<T>`) — is
gracefully skipped (not mis-imported), since resolving it needs
mini-instantiation libclang doesn't surface from the primary template.

## Also in this release (untagged v1.13.2 work)

- **Move constructor → `move_from`; copy/move assignment →
  `copy_assign` / `move_assign`.** A user-declared `T(T&&)` emits
  `pub fn move_from(src: &mut Self) -> Self`; `operator=` overloads emit
  `&mut self` methods. Copy/move assignment mangle distinctly
  (`aSERKS_` vs `aSEOS_`).
- **Itanium VTT + construction vtables.** `_ZTT` (VTT) and `_ZTC`
  construction-vtable groups for virtual-base hierarchies, gated to the
  Itanium ABI (MSVC uses vbtables, no VTT). Clang-validated against the
  diamond `D : B, C` where `B, C : virtual A`.

## Test status

Full workspace green: **547 tests across 70 suites, 0 failures**
(includes the new `mangle_templates_nttp` Itanium + MSVC golden
corpora, `imports_class_template_with_nttp`,
`imports_template_spec_methods_with_compound_t`, and
`compile_auto_instantiates_referenced_template_specializations`).

## Toolchain

No fork patch changes. The `fork/patches/` set is identical to v1.13.0
(patches 01–36). Prebuilt toolchain tarballs for v1.13.3 are
byte-identical to v1.13.0's; users on the v1.13.0 toolchain need only
update the workspace crates.

## Genuinely-remaining gaps after v1.13.3

- Uninstantiated generic templates (no Rust representation for a generic
  C++ template; specializations import via explicit/auto instantiation).
- Nested-template-typed spec-method signatures (`Box<T>` return/param).
- Template-template / pointer-to-member non-type *arguments* mangle
  correctly when supplied but can't be auto-imported from libclang.
- Member pointers (partial), covariant-return thunks, exotic-triple ABI
  quirks (non-first-class targets use generic-Itanium defaults), and
  GCC-backend cxx_throws codegen.
