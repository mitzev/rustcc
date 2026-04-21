# `rustc_abi_cxx` — Itanium C++ ABI layout & mangling crate

**Status:** draft v0.1
**Owner:** `rustcc` compiler team
**Consumers:** `rustcc` frontend (C++ header importer), `rustcc` backend (codegen
for `#[repr(cpp)]` types), `rustcc` linker driver.

---

## 1. Purpose

`rustc_abi_cxx` is the single source of truth inside `rustcc` for **how a C++
class looks in memory** and **what symbol names C++ code uses to refer to
things**, under the Itanium C++ ABI as implemented by Clang.

It is the load-bearing crate of the project. Every other C++ interop feature —
calling a C++ method from Rust, exposing a Rust type to C++, virtual dispatch,
RTTI, linking — bottoms out in a query to this crate. If its answers disagree
with Clang by even one byte or one character, every downstream feature is
silently wrong.

Therefore: the crate is source-agnostic (no libclang dependency at runtime),
deterministic, and tested against Clang as ground truth on every build.

## 2. Scope

### In scope for v1

- Itanium C++ ABI record layout, x86_64-\*-linux-gnu and
  aarch64-\*-{linux-gnu,darwin,apple-ios} targets.
- Single non-virtual inheritance, including empty base optimization (EBO) and
  tail-padding reuse.
- Non-static data members: scalars, pointers, references, arrays of known size,
  nested records.
- Polymorphic classes (have ≥1 virtual method): vptr placement, primary vtable
  layout including offset-to-top and RTTI slots.
- Itanium name mangling for:
  - Free and member functions (including `const`/ref-qualified methods).
  - Constructors (`C1`/`C2`), destructors (`D0`/`D1`/`D2`).
  - Overloaded operators.
  - Nested names, anonymous namespaces.
  - Substitutions.
  - Special symbols: `_ZTV` (vtable), `_ZTS` (type string), `_ZTI` (typeinfo).
- Conformance harness that diffs crate output against Clang record-layout and
  symbol dumps.

### Not in scope for v1 (deferred, listed so the IR doesn't preclude them)

- Virtual inheritance, virtual bases, vbase offsets, construction vtables.
- Multiple inheritance.
- Bit-fields. *(v1.1)*
- `__attribute__((packed))`, `#pragma pack`. *(v1.1)*
- Covariant-return thunks. *(v1.1; single-inheritance version is small.)*
- Templates beyond what the caller hands us already-instantiated.
- MSVC ABI. *(Separate crate when the time comes.)*
- `thread_local` storage mangling (`TH`, `TW`).
- Exception handling tables (not part of layout/mangling anyway).

## 3. Crate layout

```
crates/rustc_abi_cxx/
├── Cargo.toml           # no non-std deps; testing deps are dev-only
├── src/
│   ├── lib.rs           # re-exports; crate-level docs
│   ├── ty.rs            # CxxType IR: the input to every query
│   ├── ctx.rs           # CxxTypeCtx: arena + interning
│   ├── target.rs        # target triple → ABI parameters (ptr width, etc.)
│   ├── layout/
│   │   ├── mod.rs
│   │   ├── builder.rs   # Itanium record layout state machine
│   │   ├── field.rs     # field/base placement primitives
│   │   ├── ebo.rs       # empty base detection, "nearly empty" check
│   │   └── tail.rs      # nvsize / dsize accounting
│   ├── mangle/
│   │   ├── mod.rs       # public entry: mangle(&ctx, Symbol) -> String
│   │   ├── writer.rs    # incremental encoder + substitution table
│   │   ├── types.rs     # <type> production
│   │   ├── names.rs     # <nested-name> / <unqualified-name>
│   │   └── special.rs   # ctor/dtor/vtable/RTTI variants
│   ├── vtable/
│   │   ├── mod.rs       # VTable, VTableEntry
│   │   ├── primary.rs   # primary vtable construction (single-inherit subset)
│   │   └── rtti.rs      # _ZTS / _ZTI / _ZTV symbol synthesis
│   └── diag.rs          # error types
└── tests/
    ├── layout_corpus.rs # runs the C++ corpus through Clang + crate, diffs
    ├── mangle_corpus.rs
    ├── vtable_corpus.rs
    └── corpus/
        ├── *.cpp        # hand-authored test inputs
        └── *.golden     # frozen Clang output; regenerated via `cargo xtask
                         #   refresh-goldens`
```

The crate has **no runtime dependency on libclang or clang-sys.** libclang is
used only by the test harness and by the sibling `CxxImporter` crate, which
feeds facts into `rustc_abi_cxx` — it does not live inside it.

## 4. Core IR (`ty.rs`)

Everything downstream speaks in terms of `CxxType`, a small, closed IR that can
be populated from either a Clang AST walk or from Rust HIR for `#[repr(cpp)]`
types.

```rust
pub struct CxxTypeCtx<'a> { /* arena + interners */ }

// Opaque handles. Cheap Copy, compared by identity.
pub struct ClassId(u32);
pub struct FieldId(u32);
pub struct MethodId(u32);
pub struct TypeId(u32);

pub enum CxxType {
    Void,
    Bool,
    Int   { signed: bool, width: IntWidth },
    Float { kind: FloatKind },  // Float32, Float64, LongDouble (target-dep)
    Ptr   { pointee: TypeId, cv: CvQual },
    Ref   { pointee: TypeId, kind: RefKind, cv: CvQual }, // Lvalue | Rvalue
    Array { elem: TypeId, len: u64 },
    Record(ClassId),
    Enum  { underlying: TypeId, scoped: bool },
    Fn    (FnSig),
    MemberPtr { class: ClassId, pointee: TypeId },
}

pub struct FnSig {
    pub params: Vec<TypeId>,
    pub ret:    TypeId,
    pub cv:     CvQual,         // only meaningful for member fns
    pub ref_q:  Option<RefKind>,
    pub variadic: bool,
    pub noexcept: bool,         // affects mangling in C++17+
}

pub struct ClassDef {
    pub name: NestedName,       // e.g. ns::outer::Foo
    pub bases: Vec<BaseSpec>,   // ordered; v1: at most one, non-virtual
    pub fields: Vec<FieldDef>,  // in declaration order
    pub methods: Vec<MethodDef>,
    pub kind: RecordKind,       // Class | Struct | Union
    pub is_polymorphic: bool,   // ≡ declares or inherits a virtual method
    pub is_final: bool,
    pub source_alignment: Option<u64>, // explicit alignas
}

pub struct BaseSpec {
    pub class: ClassId,
    pub virtual_: bool,   // v1: always false; kept for forward-compat
    pub access:  Access,
}

pub struct FieldDef {
    pub name: Ident,
    pub ty:   TypeId,
    pub explicit_align: Option<u64>,
    // bit_width: Option<u32>,  // v1.1
}

pub struct MethodDef {
    pub name: Ident,
    pub sig:  FnSig,
    pub virtuality: Virtuality,   // NonVirtual | Virtual | PureVirtual
    pub vtable_index: Option<u32>, // filled in by vtable builder
    pub special: Option<SpecialMember>, // ctor/dtor/copy-ctor/...
}
```

Key invariants:

- `CxxTypeCtx` is the arena; all IDs are valid only within it.
- `ClassDef.is_polymorphic` is authoritative. The caller computes it from the
  Clang AST (or Rust HIR); the layout algorithm does not re-derive it.
- Declaration order of fields and bases is preserved exactly — the ABI depends
  on it.

## 5. Record layout algorithm

The algorithm mirrors `clang/lib/AST/RecordLayoutBuilder.cpp` for the v1 subset.
Reference: Itanium C++ ABI §2.4. What follows is the state machine we
implement; it is precise enough to code from.

### 5.1 State

```rust
struct LayoutState {
    size:    u64,   // current object size in bits
    align:   u64,   // required alignment in bits
    dsize:   u64,   // "data size" — offset past last occupied bit,
                    // used by *derived* classes to reuse our tail padding
    nvsize:  u64,   // non-virtual size (== size for our v1 subset)
    nvalign: u64,
    has_vptr: bool,
    field_offsets: Vec<u64>,
    base_offsets:  Vec<(ClassId, u64)>,
    empty_subobjects: EmptySubobjectMap, // see §5.4
}
```

All sizes and alignments are in **bits** internally, converted to bytes at
the API boundary. This matches Clang and makes future bit-field support a
local change.

### 5.2 Main pass

```
fn layout_record(ctx, class) -> RecordLayout:
  s := LayoutState::new(target)

  # 1. Primary base or vptr
  primary := pick_primary_base(class)     # §5.3
  if primary is Some(b):
      layout_base_subobject(s, b, offset=0, as_primary=true)
      # Inherits b's vptr; no new vptr allocated.
  elif class.is_polymorphic:
      allocate_vptr(s)                    # vptr at offset 0, advance dsize
      s.has_vptr = true

  # 2. Non-primary non-virtual bases (v1: at most zero of these, since
  #    single inheritance means one base total, and it was the primary
  #    if polymorphic. We still implement the loop for forward-compat.)
  for base in class.bases where base != primary:
      off := place_base(s, base)          # with EBO (§5.4) and tail reuse
      layout_base_subobject(s, base, off, as_primary=false)

  # 3. Fields
  for field in class.fields:
      off := place_field(s, field)        # §5.5
      s.field_offsets.push(off)

  # 4. Finalize
  s.nvsize  = s.dsize
  s.nvalign = s.align
  s.size    = round_up(max(s.size, s.dsize), s.align)
  if s.size == 0:
      s.size = bits_per_byte              # [class]/4: non-empty class, no
                                          # size 0
```

### 5.3 Primary base selection (single-inheritance subset)

For v1:

- If the class is not polymorphic: no primary base.
- Else if it has a non-virtual base that is itself polymorphic: that base is
  the primary base. (With single inheritance there is at most one base, so
  no ambiguity.)
- Else: no primary base; a fresh vptr is allocated.

The full Itanium rule involves "nearly empty" virtual bases; we reject virtual
bases at parse time, so that branch is unreachable.

### 5.4 Empty base optimization

An empty base (`nvsize == 0`) normally takes 1 byte, but a derived class can
place it at any offset where no other object of the same type already exists.
We track this with `EmptySubobjectMap: HashMap<ClassId, Vec<u64>>` — for each
empty class, the offsets at which an instance already exists in the current
layout.

```
fn place_empty_base(s, base):
  for off in candidate_offsets(s, base):   # 0, then increasing by align
      if !s.empty_subobjects.conflicts(base.class, off):
          s.empty_subobjects.record(base.class, off)
          return off
```

This is the most bug-prone part of Itanium layout and must be covered by a
dense conformance test (see §10).

### 5.5 Field placement with tail-padding reuse

```
fn place_field(s, field):
  align := max(field.ty.align, field.explicit_align.unwrap_or(0))
  off   := round_up(s.dsize, align)
  end   := off + field.ty.size_bits

  s.field_offsets.push(off)
  s.size   = max(s.size, end)
  s.dsize  = end
  s.align  = max(s.align, align)
  return off
```

Note that fields are placed starting from `dsize`, not `size`. This is what
lets a derived class's fields sit inside the tail padding of its base. A
common early-prototype bug is to advance from `size`; it produces correct
sizes for isolated classes and silently wrong offsets for derived classes.

### 5.6 `RecordLayout` output

```rust
pub struct RecordLayout {
    pub size_bytes:   u64,
    pub align_bytes:  u64,
    pub data_size_bytes: u64,       // dsize
    pub nv_size_bytes:   u64,
    pub nv_align_bytes:  u64,
    pub has_vptr: bool,
    pub field_offsets: Vec<u64>,    // bytes, aligned to byte boundaries
                                    // in v1 (no bit-fields)
    pub base_offsets:  Vec<(ClassId, u64)>,
    pub empty_subobjects: Vec<(ClassId, u64)>,
}
```

Bit-bytes are converted at the boundary. Callers never see bit offsets in v1.

## 6. Name mangling

Itanium ABI §5. We implement the encoding only; decoding is not needed because
the importer uses libclang to read symbols and operates on structured names,
not raw mangled strings.

### 6.1 Public API

```rust
pub fn mangle(ctx: &CxxTypeCtx, sym: Symbol) -> String;

pub enum Symbol {
    Function { name: NestedName, sig: FnSig },
    Method   { class: ClassId, name: MethodName, sig: FnSig },
    Ctor     { class: ClassId, variant: CtorVariant, sig: FnSig },
    Dtor     { class: ClassId, variant: DtorVariant },
    VTable   (ClassId),
    TypeInfo (ClassId),
    TypeInfoName (ClassId),
    Variable { name: NestedName, ty: TypeId },
    GuardVariable { for_var: NestedName },
}

pub enum MethodName {
    Ident(Ident),
    Operator(OperatorKind),     // binds to the op-name table in §6.3
    ConversionTo(TypeId),
}

pub enum CtorVariant { C1, C2, C3 }   // complete, base, allocating
pub enum DtorVariant { D0, D1, D2 }   // deleting, complete, base
```

### 6.2 Encoder

`mangle/writer.rs` is a small stateful encoder:

```rust
struct Writer<'a> {
    ctx:  &'a CxxTypeCtx<'a>,
    out:  String,
    subs: Vec<SubKey>,   // ordered substitution table (S_, S0_, S1_, ...)
}
```

On every emission of a type or nested-name, the writer checks if the entity
(by `SubKey`, a structural hash of the IR node) has been emitted before.
If yes, it emits `S<n>_` (or `S_` for index 0). If no, it emits the full
encoding and appends to `subs`.

The **substitution rules are the single hardest part of Itanium mangling.**
They are non-obvious: some forms (like builtin types) are never substituted;
others (like `std`, `std::allocator`, `std::basic_string`) have dedicated
abbreviations (`Sa`, `Ss`, etc.). Full table is in Itanium §5.1.6; we
implement it verbatim in `mangle::special::std_abbrevs`.

### 6.3 Special forms

- Ctors: `<nested-name>C1E`, `C2E`, `C3E`. We emit `C1` for complete-object
  and `C2` for base-object; `C3` is unused in the Itanium ABI as implemented
  by Clang.
- Dtors: `D0` (deleting), `D1` (complete), `D2` (base). We emit `D1` for
  `Drop` glue entry points and `D2` for base-subobject destruction during
  derived-class dtor chaining.
- Operators: table mapping `OperatorKind` → two-letter codes (`pl` for
  `operator+`, `eq` for `operator=`, etc.). Full table in Itanium §5.1.4.3.
- Conversion operators: `cv<type>`.
- Vtable: `_ZTV<nested-name>`.
- Typeinfo: `_ZTI<nested-name>`.
- Typeinfo name: `_ZTS<nested-name>`.
- Guard variable: `_ZGV<nested-name>`.

### 6.4 Determinism

Given the same `CxxTypeCtx` and `Symbol`, `mangle` must return byte-identical
output across runs and host platforms. This is a test invariant. In
particular, hash-based substitution keys use a fixed seed.

## 7. Vtable construction

v1 covers primary vtables for single-inheritance polymorphic classes. No
secondary vtables (no non-primary polymorphic bases in this subset), no
construction vtables (no virtual bases), no vtt.

### 7.1 Layout

For a class `C` with primary-chain virtual functions `f0, f1, ..., fn`:

```
_ZTV<C>:
  offset_to_top        : ptrdiff_t  = 0
  rtti_pointer         : const std::type_info*   -> _ZTI<C>
  virtual_fn_slot_0    : fn ptr -> C's final overrider for f0
  virtual_fn_slot_1    : fn ptr -> C's final overrider for f1
  ...
```

Clients of the vtable see a pointer into the vtable at the first virtual
function slot — i.e. `vptr = &_ZTV<C> + 2 * sizeof(void*)`. This is the
"vtable address point." The two preceding slots are implementation detail.
`rustc_abi_cxx::vtable::primary` computes the address-point offset and
exposes it separately so codegen can emit the right symbol reference.

### 7.2 Builder

```rust
pub struct VTable {
    pub class: ClassId,
    pub entries: Vec<VTableEntry>,
    pub address_point_offset: u64,  // bytes from _ZTV<C> symbol
    pub symbol: String,             // _ZTV<mangled>
}

pub enum VTableEntry {
    OffsetToTop(i64),
    Rtti(String),                   // _ZTI<mangled>
    FunctionPointer {
        mangled_target: String,     // final overrider symbol
        method: MethodId,
    },
    // ThisAdjustingThunk { ... },  // v1.1 for covariant returns
}
```

Final-overrider resolution for single inheritance is trivial: walk the base
chain from most-derived to root; the first class that provides a definition
for the virtual slot wins.

### 7.3 RTTI

We emit `_ZTS<mangled>` (the type-name string, a `const char[]`) and
`_ZTI<mangled>` (a `std::type_info` subobject) with layouts matching
`libc++abi`'s `__class_type_info` / `__si_class_type_info` (single-inherit
variant). These are required for `typeid` and `dynamic_cast` to work across
the Rust/C++ boundary.

Subtlety: `_ZTI` references `_ZTI` for the base class (if any). When the
base is in a different TU, we emit an external reference and rely on the
linker to resolve it against Clang's own emission. This is what makes
cross-toolchain RTTI work.

## 8. Target configuration

```rust
pub struct Target {
    pub pointer_width_bits: u32,   // 64 for our v1 targets
    pub long_double: LongDoubleKind,
    pub wchar_t_signed: bool,
    pub wchar_t_width: u32,
    pub aarch64_darwin_quirks: bool, // e.g. bool size/align
    // ...
}
```

Record layout and mangling both take `&Target` (inside `CxxTypeCtx`). We
ship profiles for:

- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `aarch64-apple-ios`

Each profile is derived from Clang's own target definitions and pinned by
the conformance corpus.

## 9. Public API surface (summary)

```rust
// Construction
let mut ctx = CxxTypeCtx::new(&target);
let class_id = ctx.define_class(ClassDef { ... });

// Queries
let layout   = ctx.layout(class_id)?;            // RecordLayout
let mangled  = ctx.mangle(Symbol::Method { ... });
let vtable   = ctx.vtable(class_id)?;            // Option<VTable>
let rtti_sym = ctx.rtti_symbol(class_id);

// Diagnostics
pub enum LayoutError {
    RecursiveValueMember { class: ClassId, field: FieldId },
    VirtualBaseUnsupported { class: ClassId, base: ClassId },  // v1 only
    MultipleBasesUnsupported { class: ClassId },               // v1 only
    UnsizedField { class: ClassId, field: FieldId },
    AlignmentOverflow { class: ClassId },
}
```

`CxxTypeCtx` is `Sync` for reads once all `define_*` calls have returned;
the intended use is "populate, then query many times from many threads."
Internal caches use `once_cell::sync::OnceCell`-style lazy initialization.

## 10. Conformance testing

**This is not optional.** Without it, the crate is unshippable. Bugs here
corrupt every downstream feature.

### 10.1 Corpus

A directory `tests/corpus/` of small C++ files, each under ~40 lines, each
exercising one corner:

- scalar-only POD
- single non-virtual base, derived adds fields
- tail-padding reuse (base has dsize < size, derived places field there)
- empty base optimization, one and two empty bases
- polymorphic class, primary vtable, three virtual methods
- derived polymorphic class, overrides and adds virtual methods
- `alignas` on field
- `alignas` on class
- reference member
- member pointer
- nested anonymous namespace
- operator overload mangling (all arithmetic + `operator=`)
- ctor/dtor mangling for each variant
- `noexcept` signature mangling
- class with name containing substitution-worthy repeats

For each `foo.cpp` we store `foo.layout.golden`, `foo.mangle.golden`,
`foo.vtable.golden` — frozen snapshots produced by `cargo xtask
refresh-goldens`, which runs:

```
clang++ -cc1 -fdump-record-layouts   foo.cpp  > foo.layout.golden
clang++ -S -emit-llvm                 foo.cpp |
    grep -E '^@_Z|^define.*@_Z'                > foo.mangle.golden
clang++ -cc1 -fdump-vtable-layouts    foo.cpp  > foo.vtable.golden
```

### 10.2 Runners

`tests/layout_corpus.rs` reads each `.cpp`, feeds it to a minimal libclang
driver in `xtask` that produces a `ClassDef`, then calls
`ctx.layout(class_id)` and diffs against `.layout.golden` after
normalizing Clang's output format. Same shape for mangle and vtable.

Refreshing goldens requires explicit `cargo xtask refresh-goldens` and
produces a reviewable diff — goldens don't regenerate silently in CI.

### 10.3 Fuzzing (v1.1)

A structure-aware fuzzer that generates random `ClassDef`s (within the v1
subset), emits equivalent C++ source, and asserts crate output matches
Clang. This catches the layout corners we didn't think to hand-write.

## 11. Open questions

1. **`ClassDef` population source.** The importer uses libclang; the
   `#[repr(cpp)]` path uses rustc HIR. Do both funnel through a thin
   adapter trait `trait ClassSource` so the crate stays source-agnostic?
   Leaning yes — keeps tests honest.
2. **Target `long double` on x86_64-linux.** 80-bit x87, 16-byte aligned.
   Clang and GCC agree on layout but disagree occasionally on mangling of
   `long double` in nested templates. Pin to Clang's behavior; document.
3. **Should we emit `_ZTI` / `_ZTS` at all in v1, given no `dynamic_cast`
   support is planned from Rust?** Yes — C++ callers still need them for
   cross-TU `typeid`, and emitting them is cheap. Only the Rust-side
   consumption is deferred.
4. **`alignas` interaction with tail padding.** Clang's behavior here
   changed between Clang 12 and 15 for some edge cases. Pick "current
   Clang" (≥16) and refuse to claim compatibility with older toolchains.
5. **Interning strategy.** `TypeId` interning must be deep (by value of
   the `CxxType`) for the substitution table to work correctly. Cost is
   fine for v1; if it becomes hot, move to `hashbrown` with a custom
   hasher.

## 12. Milestones

| M# | Deliverable                                             | Exit criterion |
|----|---------------------------------------------------------|----------------|
| 1  | `CxxType` IR, `CxxTypeCtx`, target profiles             | `cargo test` on skeleton |
| 2  | Scalar + POD record layout                              | First 5 corpus entries pass |
| 3  | Inheritance + tail-padding + EBO                        | Inheritance corpus passes |
| 4  | Polymorphic layout + vptr                               | Polymorphic corpus passes |
| 5  | Itanium mangling: functions, methods, overloads         | Mangle corpus passes |
| 6  | Ctor/dtor variants + operator mangling                  | Special-name corpus passes |
| 7  | Primary vtable + RTTI symbols                           | Vtable corpus passes |
| 8  | Conformance harness green on all supported targets      | CI matrix green |
| 9  | Fuzzer lands; 24h clean run                             | No diffs vs Clang |

After M9, the crate is ready to be depended on by the importer (Phase 1 of
the overall plan) and by `#[repr(cpp)]` codegen (Phase 6).

---

## Appendix A — Worked layout example

```cpp
struct Base {
  int  x;      // offset 0, size 4
  char y;      // offset 4, size 1
};              // size 8, dsize 5, align 4

struct Derived : Base {
  char z;      // placed at dsize = 5 (reusing Base's tail padding),
               // offset 5, size 1
  int  w;      // placed at 8, size 4
};              // size 12, dsize 12, align 4
```

A prototype that advances from `size` instead of `dsize` produces
`z` at offset 8, making `sizeof(Derived) == 16` and misaligning every
field read. This class is entry #3 in the conformance corpus for exactly
that reason.

## Appendix B — Worked mangling example

```cpp
namespace ns {
  struct Foo {
    void bar(const Foo& other) const noexcept;
  };
}
```

Expected mangling of `ns::Foo::bar`:

```
_ZNK2ns3Foo3barERKS0_
```

Breakdown:
- `_Z` — Itanium prefix.
- `N` — nested name.
- `K` — `const` method qualifier.
- `2ns3Foo3bar` — `ns::Foo::bar` (length-prefixed source-names).
- `E` — end of nested name.
- `RKS0_` — parameter type: `const Foo&`. `R` = reference, `K` = const,
  `S0_` = second substitution, pointing to `ns::Foo` (substitution `S_`
  is `ns`).

Note: `noexcept` does **not** mangle into the function symbol for
non-template functions in the Itanium ABI (Clang implements
CWG 1330/P0012 only for template arguments). The `.mangle.golden` files
assert this explicitly to catch drift.
