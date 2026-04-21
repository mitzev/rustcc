# `cxx_importer` — C++ headers → rustcc HIR

**Status:** draft v0.1
**Depends on:** `rustc_abi_cxx` for all layout/mangling queries.
**Consumers:** rustcc frontend — name resolution, HIR lowering, typeck.

---

## 1. Purpose

`cxx_importer` is the `rustcc`-internal analog of Swift's `ClangImporter`.
It consumes C++ headers via libclang and makes the declarations they contain
visible to the Rust compilation as genuine `DefId`-bearing items — modules,
structs, impls, methods. It does not generate textual Rust source, and it
is not `bindgen`. Its output is HIR, not `.rs` files.

Two consequences follow from that choice:

1. Name resolution and type checking see C++ types as first-class, so
   borrow-checker diagnostics can point at C++ declarations the same way
   they point at Rust declarations.
2. Imports are lazy. A header that declares 20 000 classes but whose
   consumer references 3 of them only pays for those 3. `bindgen`-style
   eager generation does not scale to real-world codebases (LLVM, Chromium,
   Qt); we must.

## 2. Non-goals (v1)

- Parsing `.cpp` source files. We only read headers.
- Generating `.rs` text output. There is no user-facing artifact.
- Round-tripping. The importer is one-way: C++ → HIR.
- Replacing the `cxx` crate at runtime. `cxx` remains valid for projects
  that don't want a forked compiler.
- Understanding uninstantiated `<template>` bodies.

## 3. Architecture

```
+-------------------------+       +------------------------+
|  Rust source (.rs)      |       |  C++ headers (.h/.hpp) |
+-----------+-------------+       +-----------+------------+
            |                                 |
            v                                 v
   +--------+----------+            +---------+---------+
   |  rustcc frontend  |<--queries--|   cxx_importer    |
   |   (resolve, hir,  |            |   (libclang-backed)|
   |    typeck, ...)   |--facts---->|                   |
   +--------+----------+            +---------+---------+
            |                                 |
            v                                 v
            +------- both populate -----------+
                      CxxTypeCtx
                  (in rustc_abi_cxx)
```

The importer is three layers:

- **Driver** (`cxx_importer::driver`): owns the libclang `CXIndex` and a
  `CXTranslationUnit` per physical header graph in the manifest.
- **Entity resolver** (`cxx_importer::resolve`): given a
  mangled-name-ish key (e.g. `ns::foo::Bar`), finds the `CXCursor` and
  schedules lowering.
- **Lowering** (`cxx_importer::lower`): `CXCursor` → `CxxType` + rustcc
  HIR stubs. Bulk of the crate.

## 4. Lazy import

Compilation entry points make no `cxx_importer` calls. The importer is
driven by `rustc_resolve`: when path resolution encounters a segment it
can't satisfy locally, it queries the importer's root modules for
matches. The importer has pre-registered a skeleton `Module` HIR node
for each top-level C++ namespace; descending triggers on-demand lowering.

Consequences:

- `use cxx::std::string::String;` forces lowering of exactly
  `std::string` and its direct transitive dependencies. Siblings are
  untouched.
- Recompilation of Rust code that doesn't reference C++ does zero
  libclang work.
- The HIR node for a C++ class is stable across incremental builds,
  keyed by Clang's USR + the header-graph fingerprint (§12).

## 5. Annotation model

The importer needs information the C++ type system doesn't carry: which
classes are reference-counted, which methods steal ownership, which
pointer parameters may be null. Two channels provide it:

### 5.1 Inline attributes

```cpp
class [[rustcc::shared_reference(retain="Foo_retain",
                                 release="Foo_release")]]
Foo { ... };

[[rustcc::name("push_back_move")]]
void push_back(T&& value);

void bar([[rustcc::nullable]]     int* p,
         [[rustcc::lifetimebound]] const std::string& s);
```

### 5.2 Sidecar YAML

For headers we don't own:

```yaml
schema: 1
types:
  "std::vector":
    kind: value
    methods:
      "push_back(T const&)":
        rust_name: push_back
      "push_back(T&&)":
        rust_name: push_back_move
```

Inline attributes win over sidecar entries on conflict; the importer
emits a warning that names both sources. The YAML schema is versioned so
additions don't silently misread old files.

## 6. Name mapping

Default transformations (each overridable by annotation):

| C++                             | Rust                                   |
|---------------------------------|----------------------------------------|
| `namespace foo`                 | `mod foo` under the `cxx` crate root   |
| `class Foo`                     | `struct Foo` with `#[repr(cpp)]`       |
| `Foo::bar(...) const`           | `fn bar(&self, ...)`                   |
| `Foo::bar(...)` non-const       | `fn bar(&mut self, ...)`               |
| `Foo::operator+`                | `fn op_add(&self, ...)` (no trait)     |
| `Foo::Foo(args)` (ctor)         | `fn new(args) -> CxxOwned<Foo>`        |
| `Foo::~Foo()`                   | `impl Drop` (not a user-visible fn)    |
| `Foo::Foo(const Foo&)`          | `fn cxx_clone(&self) -> CxxOwned<Foo>` |
| `using Bar = Foo;`              | `type Bar = Foo;`                      |
| anonymous namespace             | `mod __anon_<hash>`                    |
| `enum class E`                  | `#[repr(cpp)] enum E`                  |

"Default" means the rule that fires absent any annotation.
`[[rustcc::name(...)]]` overrides on any mapped entity.

## 7. Overload resolution

C++ allows `void f(int)` and `void f(double)` to coexist; Rust doesn't.
Two-stage strategy:

1. **Unique-by-arity renaming.** If exactly one overload has a given
   arity, it keeps the base name. Others get renames only on collision.
2. **Disambiguator suffix.** On an arity tie, suffixed from parameter
   types: `push_back_ref`, `push_back_rref`, `push_back_int`. The
   algorithm is documented so outputs are predictable, not hash-derived.

An annotation always wins. If two annotations or two generated names
collide, the importer emits a fatal error with both source locations.

## 8. Templates

- Uninstantiated templates (`CXCursor_ClassTemplate`) are skipped.
- Explicit instantiations (`template class std::vector<int>;`) become
  concrete imported classes.
- `[[rustcc::instantiate(std::vector<int>)]]` on a header (or
  `instantiations:` in sidecar YAML) forces instantiation Clang-side;
  the importer lowers the result as a concrete class.
- Rust-side generics that bottom out in C++ templates are deferred.

A method template inside a non-template class is itself a template and
follows the same rules.

## 9. Inheritance

Single non-virtual base mapping:

```cpp
struct Base    { void base_method(); };
struct Derived : Base { void derived_method(); };
```

becomes:

```rust
#[repr(cpp(layout = "itanium"))]
pub struct Base { /* opaque */ }
impl Base { pub fn base_method(&self) { ... } }

#[repr(cpp(layout = "itanium", bases = [Base]))]
pub struct Derived { /* opaque */ }
impl Derived { pub fn derived_method(&self) { ... } }

impl CxxBase<Base> for Derived {
    fn upcast(&self)         -> &Base     { ... }
    fn upcast_mut(&mut self) -> &mut Base { ... }
}
```

`CxxBase` lives in the `cxx` runtime crate and is how derived→base
references form. There is no implicit deref coercion: users write
`derived.upcast()` explicitly. This is intentional — silent upcasts make
borrow-checker errors harder to read and invite slicing mistakes.

Virtual methods: the importer records `Virtuality::Virtual` and asks
`rustc_abi_cxx` for the vtable index. Codegen emits the indirect call.
See `codegen.md §2.3`.

## 10. Diagnostics

Every lowering failure produces a diagnostic with **two** source
locations — the C++ declaration and the Rust use site that triggered
the import:

```
error[E_CXX_0007]: cannot import `ns::Widget`: virtual inheritance
                   not supported
  --> headers/widget.hpp:14:7
   |
14 | class Widget : virtual public Base { ... };
   |       ^^^^^^   ^^^^^^^ virtual inheritance
   |
note: triggered by
  --> src/main.rs:8:9
   |
 8 |     let w = ns::Widget::new();
   |             ^^^^^^^^^^^^^^^^^
```

The importer owns a diagnostic translator that converts Clang's own
diagnostics into rustc format, preserving spans.

## 11. Error recovery

If lowering a class fails, the importer emits a **poison** HIR node
with the correct name but no methods and a failure flag. Consumers see
`error[E_CXX_POISONED]: type cannot be used because its import failed
(see previous error)`. This prevents one broken class from cascading
into dozens of "unknown identifier" errors downstream.

## 12. Caching & incremental

- Per-TU Clang AST is cached on disk, keyed by (SHA-256 of header graph
  contents, SHA-256 of compile flags, libclang version string).
- Lowered HIR stubs are cached in rustc's incr-comp store keyed by USR
  + header-graph fingerprint. A change to an unrelated header
  invalidates nothing.
- Annotation contents participate in the USR key; changing an
  annotation invalidates only the affected entities.

## 13. Open questions

1. **libclang version floor.** Clang's record-layout output has changed
   subtly between releases. Pin Clang 17 as the floor and probe at
   startup? Leaning yes.
2. **Header graph roots.** Manifest specifies roots; large projects
   don't enumerate. Add glob support.
3. **ODR across roots.** If two header graphs declare the same class,
   reject with a clear error in v1; merging later if needed.
4. **`friend` declarations.** Ignore in v1.

## 14. Milestones

| M# | Deliverable                                                     |
|----|------------------------------------------------------------------|
| 1  | Clang driver: parse header graph, produce `CXTranslationUnit`    |
| 2  | Lower primitive types, free functions, namespaces                |
| 3  | Lower POD classes with fields and non-virtual methods            |
| 4  | Overload renaming + operator mapping                             |
| 5  | Inheritance (non-virtual, single), `CxxBase` emission            |
| 6  | Virtual methods → vtable-index-aware HIR                         |
| 7  | Annotation processing (inline attrs + sidecar YAML)              |
| 8  | Explicit template instantiation import                           |
| 9  | Diagnostic translator, error recovery, poisoned nodes            |
| 10 | Incremental compilation integration                              |
