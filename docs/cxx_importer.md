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

The roadmap is organized into three phases. **Phase A (M1–M10)** is the
foundation — the design's original milestone set, mostly shipped.
**Phase B (M11–M14)** is the smallest viable surface for "FLTK Hello
World runs": four items the foundation can't handle today that block
even a one-window FLTK program. **Phase C (M15–M21)** is the surface
for "useful subset of FLTK": callbacks, custom widgets, basic styling.

### Phase A — Foundation

| M# | Deliverable                                                                                                               | Status |
|----|---------------------------------------------------------------------------------------------------------------------------|--------|
| 1  | Clang driver: parse header graph, produce `CXTranslationUnit`                                                             | ✅ shipped |
| 2  | Lower primitive types, free functions, namespaces                                                                         | 🟨 partial (primitive types + namespaces ✓; free functions deferred to M11) |
| 3  | Lower POD classes with fields and non-virtual methods                                                                     | ✅ shipped |
| 4  | Overload renaming + operator mapping                                                                                      | ✅ shipped |
| 5  | Inheritance (non-virtual, single), `CxxBase` emission                                                                     | 🟨 partial (BaseSpec lowering ✓; upcast emission deferred to M19) |
| 6  | Virtual methods → vtable-index-aware emission                                                                             | ✅ shipped (single-inheritance; pure virtuals + secondary vtables open) |
| 7  | Annotation processing (inline attrs + sidecar YAML)                                                                       | ✅ shipped (libclang `[[clang::annotate("rustcc::…")]]` walker + emitter wiring; sidecar YAML parser already shipped) |
| 8  | Explicit template instantiation import                                                                                    | ✅ shipped (`HeaderGraph::template_instantiations` synthesizes a root that force-instantiates each entry) |
| 9  | Diagnostic translator, error recovery, poisoned nodes                                                                     | ✅ shipped (`SourceSpan` carry-through, `Diagnostic` formatter, poison-node side-table on `CxxTypeCtx`, `Importer::poison_class` recovery on forward-only decls) |
| 10 | Incremental compilation integration                                                                                       | ✅ shipped (cache feature, `Driver::load_or_parse` with SHA-256 of headers + clang-flags + version stamps; HIR-integrated lazy-import path remains a v2 follow-up) |

### Phase B — Tier 1: FLTK "Hello World"

The four items that block running a one-window FLTK program. Roughly
4 weeks of focused work.

| M#  | Deliverable                                                            | FLTK use site                                       | Status |
|-----|------------------------------------------------------------------------|-----------------------------------------------------|--------|
| 11  | Free functions + static methods + static data members at TU/namespace scope | `Fl::run()`, `Fl::wait()`, `fl_color(int)`, `fl_message(...)`, `Fl::scheme_` | 🟨 partial — static methods on classes ✅ (M11.a) + free functions ✅ (M11.b: `FreeFnSet` side-table, namespace-tree integration, `unsafe extern "C++"` block + `pub fn` wrappers, builtin filter); static data members tracked as M11.c |
| 12  | `#define` constant capture via clang's preprocessor record             | `FL_RED`, `FL_NORMAL_LABEL`, `FL_UP_BOX`, `FL_BOLD` | ✅ shipped (`MacroSet`, tokenize-and-parse approach) |
| 13  | Forward-declared opaque types                                          | `class Fl_Widget;` referenced before its def        | ✅ shipped (poison-node minted on forward-only decls; upgraded in place when full def appears later) |
| 14  | Heap-allocation shims (`new` / `delete`)                               | `new Fl_Window(340, 180)` — widgets MUST be heap-allocated; FLTK's parent tree owns by pointer | ✅ shipped (`__cxx_<class>_new_heap_<i>` + `__cxx_<class>_delete` thunks; paired with `::cxx::CxxHeap<T>` + `CxxDeletable` trait) |

### Phase C — Tier 2: FLTK useful subset

Adds callbacks, enums, inherited methods, and string ergonomics — the
shape of "do something interactive with FLTK." Roughly 6 weeks on top
of Phase B.

| M#  | Deliverable                                                                  | FLTK use site                                              | Effort |
|-----|------------------------------------------------------------------------------|------------------------------------------------------------|--------|
| 15  | Function pointer types + safe-closure callback wrappers                     | `widget->callback(my_func, user_data)` — FLTK is callback-driven | ✅ shipped (importer lowers `void (*)(int)` and bare function-prototype to `CxxType::Fn(FnSig)`; renderer emits `Option<unsafe extern "C" fn(...) -> ret>`. New `::cxx::CxxCallback<F>` runtime helper wraps a Rust closure into the `(fn-ptr, *mut c_void)` shape C++ APIs expect; per-arity richer wrappers are tracked as M15.b.) |
| 16  | `enum class` + plain `enum` body lowering                                   | `enum class Fl_Boxtype { … }`, `enum Fl_When { … }`         | ✅ shipped (`EnumSet` side-table; emitter picks `#[repr(int)] pub enum` for scoped+unique vs. `#[repr(transparent)] pub struct + assoc consts` for unscoped/aliasing; class-scope + anonymous enums deferred) |
| 17  | Type aliases (`using` / `typedef`) emission                                 | `typedef unsigned int Fl_Color;`, `using Fl_Callback = …;`  | ✅ shipped (`AliasSet`, `import_header_with_extras`, namespace-tree integration; emits `pub type X = Y;` inside owning `pub mod`; class-scope aliases deferred) |
| 18  | Default-argument fan-out (max-arity wrapper + documented defaults)          | `void redraw(int delay = 0)`                                | 🟨 partial — count-only v0 shipped (`ctx.default_arg_count` per `(class, method_idx)`; emitter prepends doc-comment hint to each affected method). Per-arity convenience wrappers tracked as M18.b for the next iteration. |
| 19  | M5 finish — `CxxBase<T>` upcast emission OR derived-class method flattening | `Fl_Button btn; btn.show();` (inherits `Fl_Widget::show`)   | ✅ shipped (one `impl ::cxx::CxxBase<Base> for Derived` per non-virtual base, offset baked in from `RecordLayout::base_offsets`; offset-0 cases elide `.add(0)`. Virtual bases deferred to M22 — their offsets are dynamic via the vtable.) |
| 20  | `const char*` ↔ `&CStr` / `&str` ergonomics layer                            | Labels, tooltips, file paths                                | ✅ shipped (configurable: `RustBindingsConfig::cstr_ergonomics: bool`. Off by default. When on, byte-sized integer pointers (`*[const|mut] i8` / `*[const|mut] u8`) render as `*[const|mut] ::core::ffi::c_char` so `CStr::as_ptr()` plugs in without casts. Higher-level `&CStr` / `&str` parameter wrappers are tracked as M20.b.) |
| 21  | Bitfield-aware layout in `rustc_abi_cxx`                                    | Some FLTK structs use `unsigned when_:8;`-style fields. Verify `rustc_abi_cxx::layout` handles them; add support if missing. | ✅ shipped (probe phase: `rustc_abi_cxx::layout` does NOT model Itanium bit-packing today; importer poisons any class containing a bitfield with a clear M21 reason. Proper packing is tracked as M21.b for a follow-up — until then, bitfield-bearing classes emit as opaque `pub struct` with the poison reason in a doc comment. Ships safe-fail rather than silent layout corruption.) |

### Out of FLTK's path but still tracked

These come up in *other* real-world libraries; they're not on the FLTK
critical path but are on the larger v2 roadmap.

| M#  | Deliverable                                                                  | Why                                                        |
|-----|------------------------------------------------------------------------------|------------------------------------------------------------|
| 22  | Multi-inheritance + virtual-base `this`-pointer adjustments + secondary vtables | Required for Qt, LLVM, Chromium. FLTK uses single inheritance only. |
| 23  | Pure virtual handling (`__cxa_pure_virtual` shim or skip-with-marker)        | Common in any abstract-base-class-heavy library.           |
| 24  | Method extraction on template specializations                                | Long-standing libclang gap; required for STL-using libraries. |
| 25  | Sidecar YAML → `HeaderGraph::template_instantiations` plumbing               | ✅ shipped (`SidecarSchema::collect_template_instantiations()` aggregates across type entries with dedup; `HeaderGraph::extend_from_sidecar(&schema)` plumbs them into the graph idempotently). |
| 26  | Build-system integration (drive cmake / collect link inputs from `Cargo.toml`) | Today users build the C++ side themselves. A full `cxx_importer::build` story is a release-worthy feature on its own. |

---

## 15. Phase B — Tier 1 design notes (FLTK Hello World)

### M11. Free functions + static methods + static data

**The gap.** `walk_top_level` in `import.rs` recognizes `Namespace`,
`StructDecl`, `ClassDecl`, `UnionDecl` — not `FunctionDecl` or
`VarDecl`. Static methods on classes flow through the existing
`EntityKind::Method` path but the importer's static-vs-instance
heuristic isn't clang-flag-driven; we infer "instance" from the
default `cv` qualifier, which is wrong for `Fl::run()` (no receiver
at all in C++; libclang exposes `is_static_method`).

**The shape.** Two new IR additions in `rustc_abi_cxx`:

- `FreeFunctionDef { scope: NestedName, name: Ident, sig: FnSig }` —
  parallel to `MethodDef` but no `enclosing_class`.
- `VarDef { scope: NestedName, name: Ident, ty: TypeId, mutability:
  CvQual }` — for static class members and namespace-scope `extern`
  variables.

`Importer` walks `EntityKind::FunctionDecl` and `EntityKind::VarDecl`
at TU/namespace scope. `MethodDef` gets a `is_static: bool` populated
from `Entity::is_static_method`. The `rust_bindings` emitter renders
free functions as top-level `unsafe extern "C++" fn` decls plus
inline-fn wrappers, and static fields as `pub static FOO: T` with
`#[link_name]`.

**Effort.** ~2 weeks. The bulk is the IR shape change in
`rustc_abi_cxx` and threading the new entity kinds through
`Driver::parse_all` + the emitter.

### M12. `#define` constant capture

**The gap.** `Index_TranslationUnit_DetailedPreprocessingRecord` is
off by default in our libclang setup. Even with it on, `clang_visit`
exposes `MacroDefinition` cursors that the current `walk_top_level`
ignores.

**The shape.**

1. Pass `clang_TranslationUnit_None | clang_TranslationUnit_Detailed
   PreprocessingRecord` to `Index::parser(...)`.
2. New walker pass: visit `EntityKind::MacroDefinition`. Skip
   function-like macros (no expansion-context-free way to render
   them as Rust). For object-like macros, call
   `clang_Cursor_Evaluate` (clang-rs exposes this as
   `Entity::evaluate()`) to get a typed value.
3. Recognize signed integer, unsigned integer, float, and
   string-literal results. Reject anything else with a soft
   diagnostic ("macro `FOO` expands to `(x + 1)`; not lowered").
4. Emit Rust `pub const FOO: i32 = 42;` etc. into the same
   namespace tree the bindings emitter already builds.

**Caveat.** Clang's evaluator runs the macro through its own AST
expansion. It correctly handles `#define FL_RED 88`, `#define
FL_BOLD 1`, `#define FL_BOLD_ITALIC (FL_BOLD | FL_ITALIC)`. But
expressions involving symbols that aren't yet defined return
`Unevaluated`; we render those as a comment-emitted skip rather
than failing the whole import.

**Effort.** ~1 week. The libclang side is a few hundred LOC; the
emitter side is a new `pub const` template in `rust_bindings`.

### M13. Forward-declared opaque types

**The gap.** `Importer::import_class` currently rejects classes
with no definition (line 234 in `import.rs`: `if
!entity.is_definition() { return Err(...); }`). FLTK headers
forward-declare extensively: every `Fl_Widget*` in a function
signature pulls in a forward decl that we currently fail on.

**The shape.** When we encounter a forward-only `EntityKind::
ClassDecl` / `StructDecl`, register an *opaque* class in the IR:
zero size, alignment 1, `is_polymorphic: false`, no fields, no
methods, plus a new `is_opaque: bool` flag on `ClassDef`. The
emitter renders opaque classes as `#[repr(C)] pub struct Foo {
_priv: [u8; 0] }` — usable through pointers only.

When the same class is later imported with a full definition (in
the same TU or via a USR-cache hit from another header), we
"upgrade" the opaque entry to a concrete `ClassDef`. This works
because the existing dedup keys on Clang USR.

**Effort.** ~3 days. The IR shape change is small; the trickier
part is the upgrade dance when a forward decl is followed by a
definition.

### M14. Heap-allocation shims (`new` / `delete`)

**The gap.** Today's emitter does stack construction via
`MaybeUninit::<Self>::uninit() + ctor + assume_init`. FLTK widgets
must be heap-allocated — the parent window owns child widgets by
pointer, and a stack-allocated widget's destructor would fire when
its scope ends, leaving the parent with a dangling pointer.

**The shape.** Emit, per non-trivial class:

```cpp
// shims.cpp (added by the existing shim generator)
extern "C" Fl_Window* __cxx_Fl_Window_new_heap(int w, int h) {
    return new Fl_Window(w, h);
}
extern "C" void __cxx_Fl_Window_delete(Fl_Window* p) {
    delete p;
}
```

```rust
// bindings.rs (new emission in rust_bindings)
impl Fl_Window {
    pub fn new_boxed(w: i32, h: i32) -> Box<Fl_Window> {
        unsafe {
            let raw = __cxx_Fl_Window_new_heap(w, h);
            Box::from_raw(raw)
        }
    }
}
// `Box<Fl_Window>` already gets the right Drop via the existing
// `impl Drop for Fl_Window` that calls the C++ dtor.
```

The C++ shim file is the existing `shims.rs` output, just augmented
with the heap-creation thunks. Rust side adds `pub fn new_boxed` (or
similar) variants alongside the stack-`new`.

**Why both?** Some classes are stack-friendly (simple value types
like `Fl_Color`); some are tree-rooted in a parent that takes
ownership. Annotation `[[rustcc::ownership("heap")]]` could
escalate certain types to heap-only emission, but v0 of M14 just
emits both and lets the caller pick.

**Effort.** ~1 week. The shim generator already exists; the new
work is the heap-creation thunk template plus the Rust-side
`Box<T>`-returning wrapper.

---

## 16. Phase C — Tier 2 design notes (useful subset)

### M15. Function pointer types + safe-closure callbacks

**The gap.** `render_rust_type` in `rust_bindings.rs` doesn't
handle `CxxType::Fn(_)` (the `MemberPtr` arm rejects it as
unsupported). FLTK's API is callback-driven; without renderable
function pointer types, nothing useful compiles.

**The shape.** Two layers:

1. **Type rendering.** Add a `CxxType::Fn` arm in `render_rust_type`
   that emits `unsafe extern "C++" fn(*mut Fl_Widget, *mut
   ::core::ffi::c_void)` from the `FnSig` payload. This unblocks
   passing a raw C function as a callback today.

2. **Closure-wrapping convenience.** A typical user wants to pass a
   Rust closure, not a raw `extern "C"` function. Pattern (autocxx-
   inspired):

   ```rust
   pub struct CallbackBox<W> {
       callback: Box<dyn FnMut(&mut W) + 'static>,
   }
   impl Fl_Widget {
       pub fn set_rust_callback<F>(&mut self, f: F)
       where F: FnMut(&mut Self) + 'static
       {
           let boxed = Box::into_raw(Box::new(CallbackBox { callback: Box::new(f) }));
           extern "C" fn trampoline(w: *mut Fl_Widget, ud: *mut c_void) {
               let cb = unsafe { &mut *(ud as *mut CallbackBox<Fl_Widget>) };
               (cb.callback)(unsafe { &mut *w });
           }
           unsafe { self.callback(trampoline, boxed as *mut c_void); }
       }
   }
   ```

   This lives in the `cxx` runtime crate, not in the generated
   bindings. The generator just exposes the raw `callback(fn,
   void*)` API; the closure layer is opt-in.

**Effort.** ~1.5 weeks total: ~3 days for the type rendering, the
rest for the closure-wrapping infrastructure.

### M16. `enum class` + plain `enum` body lowering

**The gap.** `NameSegment::Enum` exists in the IR, but enum *bodies*
(constants + values) aren't lowered. The importer treats enums as
opaque — fine for type-position uses, useless for users who need
the constants.

**The shape.** New IR: `EnumDef { name, underlying_ty: TypeId,
constants: Vec<(Ident, i128)> }`. Importer walks
`EntityKind::EnumConstantDecl` children of an `EntityKind::EnumDecl`.
Emitter renders as `#[repr(<underlying>)] pub enum Fl_Boxtype {
FL_NO_BOX = 0, FL_FLAT_BOX = 1, … }`.

For C-style (non-`enum class`) enums, also emit each constant as
`pub const FL_NO_BOX: Fl_Boxtype = Fl_Boxtype::FL_NO_BOX` so user
code can write `FL_NO_BOX` unscoped, matching the C++ idiom.

**Effort.** ~1 week.

### M17. Type aliases / `using`

**The gap.** `EntityKind::TypedefDecl` and `EntityKind::TypeAliasDecl`
are skipped. `Fl_Color` (a `typedef unsigned int`) ends up referenced
as a literal `c_uint` everywhere.

**The shape.** Walk the typedefs at TU/namespace scope; resolve the
underlying type; emit `pub type Fl_Color = u32;`. For function-type
aliases (`using Fl_Callback = void(Fl_Widget*, void*);`), emit the
function-pointer type alias.

**Effort.** ~3 days.

### M18. Default arguments

**The gap.** C++ default arguments don't have a Rust analog. Today
the importer ignores them and the emitter's wrapper takes all args
positionally. Users see `widget.redraw(0)` even when they meant
"default delay."

**The shape.** v1: emit only the maximum-arity wrapper and capture
the default values in a doc comment on the wrapper. v2 (later
release): emit a `Builder` pattern when there are 3+ default args.

**Effort.** ~3 days for v1.

### M19. `CxxBase<T>` upcast / inherited method visibility

**The gap.** `Fl_Button btn; btn.show();` — `show()` is declared on
`Fl_Widget`. The emitter only puts methods declared on the class
itself into the `impl` block. The user has to upcast manually,
except there's no upcast mechanism.

**The shape.** Two design choices:

- **(a) `CxxBase<T>` emission** (the design doc's M5 plan): emit
  `impl CxxBase<Fl_Widget> for Fl_Button { fn upcast(&self) ->
  &Fl_Widget { … } }`. Users write `btn.upcast().show()`. Explicit,
  matches Rust's "no implicit coercion" idiom.
- **(b) Method flattening**: walk the inheritance chain at emission
  time and copy each base method into the derived class's `impl`
  block. Users write `btn.show()` directly. Less explicit, but
  matches the C++ programmer's expectation.

Recommendation: ship (a) first because it's smaller and lets the
user opt-in. Add (b) as an optional emitter setting after.

**Effort.** ~1.5 weeks for (a). Another ~1 week for (b) if added.

### M20. `const char*` ergonomics

**The gap.** FLTK passes labels and tooltips as `const char*`.
Today's emitter renders `*const i8`; users write `b"Hello\0".as_ptr()
as *const i8`. Painful.

**The shape.** Two modes, opt-in via `RustBindingsConfig`:

- `CStrErgonomics::None` (default): emit `*const i8` as today.
- `CStrErgonomics::CStr`: render `&CStr` for input parameters,
  `&CStr` for return types. Emitter inserts a `CStr::from_ptr(...)`
  conversion at the wrapper's call boundary.
- `CStrErgonomics::String`: same as `CStr` but converts at the
  boundary to `&str` / `String`. Allocates per-call.

**Effort.** ~1 week.

### M21. Bitfield-aware layout

**The gap.** Some FLTK structs use `unsigned when_:8;`-style
bitfields. `rustc_abi_cxx::layout` may or may not preserve correct
field offsets across bitfield boundaries — this needs a probe before
we know if there's actual work. Bitfields don't have a Rust direct
analog (Rust uses `bitfield-struct` crates), but **field offsets
beyond the bitfield region must still match** for ABI compatibility.

**The shape.** Probe first: write a libclang test that imports a
struct with bitfields, queries `ctx.layout()`, and compares to
Clang's own offsets via `clang_Type_getOffsetOf`. If offsets agree,
we're good (offsets are what matters for cross-language ABI; Rust
side just gets `_pad` opaque storage for the bitfield region). If
they disagree, fix the layout engine.

**Effort.** ~3 days probe + however much fix-up the probe surfaces.
