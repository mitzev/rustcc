# `#[repr(cpp)]` — Rust types visible to C++

**Status:** draft v0.1
**Depends on:** `rustc_abi_cxx`, `codegen`, `build_integration`.

---

## 1. Purpose

The symmetric direction of interop: a Rust struct that C++ code can
`#include`, construct, store by value, and call methods on. C++ sees
Itanium ABI layout and mangled symbols.

Swift doesn't do this well. Rust can, because its codegen already runs
through a build-driver step that can emit auxiliary files.

## 2. Declaration

```rust
#[repr(cpp)]
#[cpp_namespace = "acme"]
#[cpp_name      = "Widget"]
pub struct Widget {
    pub id:   i32,
    pub name: CxxString,
}

extern "C++" impl Widget {
    pub fn new(id: i32, name: CxxString) -> CxxOwned<Widget> { ... }
    pub fn id(&self) -> i32 { self.id }
    pub fn rename(&mut self, new_name: CxxString) { self.name = new_name; }
}
```

- `#[repr(cpp)]` alone forces layout to match Itanium.
- `#[cpp_namespace]` / `#[cpp_name]` control the mangled symbol. If
  omitted the symbol uses the Rust path, which C++ can't `#include`.
  Most users set these.
- `extern "C++" impl` blocks: methods get Itanium mangling and the C++
  calling convention. Methods in a regular `impl` use Rust calling
  convention as usual and are invisible to C++.

## 3. Generated header

At build time, rustcc emits a `<crate>-cxx.hpp`:

```cpp
// generated: my-crate-cxx.hpp
namespace acme {

class Widget {
public:
    Widget(std::int32_t id, rust::String name);
    Widget(const Widget&) = delete;
    Widget(Widget&&) noexcept;
    ~Widget();

    std::int32_t id() const;
    void rename(rust::String new_name);

private:
    alignas(8) unsigned char __rust_storage[24]; // from rustc_abi_cxx
};

}
```

- Size and alignment from `rustc_abi_cxx::layout`.
- Storage is opaque to C++; fields aren't declared individually so C++
  can't rearrange or pad them.
- Signatures mirror the Rust `extern "C++"` block. Mangled symbols
  match exactly what C++ would emit for this declaration, so a C++
  call site emits the same `_ZN...` that the Rust side provides.

Copy-ctor is `= delete` unless the Rust type has a `cxx_copy` method
(§6). Move-ctor and dtor are always generated.

## 4. Constraints on `#[repr(cpp)]` types

- No `enum`s with data-bearing variants. Fieldless C-like enums are
  allowed and map to `enum class`.
- No generic parameters. A generic `#[repr(cpp)]` type would require
  Rust monomorphization to drive C++ template instantiation — v1.5 at
  earliest.
- No `Drop` impl that panics. (Enforced by the panic barrier anyway.)
- No fields whose Rust layout relies on niche optimization.

Violating a constraint is a hard error at check-time, diagnostic
pointing at the `#[repr(cpp)]` attribute and the offending item.

## 5. Method dispatch

Non-virtual methods are regular calls: codegen emits the body as a
function with the mangled symbol and C++ calling convention. C++
callers emit an ordinary call.

Virtual methods on **Rust-defined** classes are **shipped**: mark an
inherent method `virtual` / `override` (or `#[cpp_virtual]`) inside a
`class`. The compiler emits the vtable globals (`_ZTV`/`_ZTI`/`_ZTS`),
the ctor installs the vptr, and a derived class's `override` replaces
the base's slot — so a C++ caller dispatching through a base pointer
lands in the Rust override. See `examples/virtual_override/` for the
round-trip and `examples/shape_hierarchy/` for the surface.

### Subclassing an *imported* C++ class — not supported

You can define a whole Rust class hierarchy (`class Derived : Base`)
where **both** base and derived are Rust-defined, and C++ dispatches
into it correctly. You **cannot** currently make a Rust `class`
inherit from an *imported* C++ class (e.g. `class MyWidget :
Fl_Widget`) with working cross-boundary virtual dispatch. The reason
is structural:

- `#[cpp_virtual]` may only mark **inherent** methods; an imported C++
  class's methods live in `extern "C++"` blocks (foreign items), which
  the attribute rejects.
- The vtable-chain + override-verify passes only scan inherent
  `#[cpp_virtual]` methods, so an imported base contributes no
  overridable slots — `override fn` errors, and a plain `virtual fn`
  would build a *new* Rust vtable rather than extending the C++ base's.

A true Rust-subclasses-C++ feature would need the Rust derived ctor to
install a vtable that **extends** the C++ base's (the C++
derived-ctor-overwrites-vptr dance, across the language boundary).
That's a substantial future feature. **Workaround today:** use
*composition* — hold the C++ object and call its methods from Rust
(this is what `examples/fltk_text_editor/` does), rather than
subclassing it.

## 6. Constructors and destructors

### Ctors

```rust
extern "C++" impl Widget {
    pub fn new(id: i32, name: CxxString) -> CxxOwned<Widget> { ... }
}
```

lowers to a function with ctor mangling `_ZN4acme6WidgetC1E...`. The
body runs Rust code populating the struct in place via `MaybeUninit`
mechanics identical to Rust's normal struct-init codegen.

Copy-ctor isn't generated by default; C++ header declares it
`= delete`. Users opt in by providing `cxx_copy`.

Move-ctor is auto-generated from Rust's move semantics: it memcpys
the storage. Valid because `#[repr(cpp)]` types are
address-insensitive in v1 (next paragraph).

### Address-insensitivity requirement

v1 `#[repr(cpp)]` types may not self-reference or register their
address anywhere. Design constraint, not technical — lets us generate
a trivial move-ctor. Users needing address-sensitive types (intrusive
list hooks, self-ref) wait for v1.5, which adds
`#[cpp_move_ctor = "fn_name"]`.

### Dtor

Generated from the Rust `Drop` impl; body calls Rust drop glue.

## 7. Build-time flow

1. rustc parses `#[repr(cpp)]` types in the crate.
2. `rustc_abi_cxx` computes layout for each.
3. rustc emits methods with mangled symbols and C++ calling convention.
4. rustcc's build driver emits `<crate>-cxx.hpp` listing all exported
   types and methods.
5. Users `#include "<crate>-cxx.hpp"` and link against the Rust rlib,
   which contains the exported symbols.

## 8. Milestones

| M# | Deliverable                                                      |
|----|------------------------------------------------------------------|
| 1  | `#[repr(cpp)]` on struct, layout override                        |
| 2  | `#[cpp_namespace]` / `#[cpp_name]` symbol control                |
| 3  | `extern "C++" impl` for non-virtual methods                      |
| 4  | Ctor / dtor mangling emission                                    |
| 5  | Generated `.hpp` emission                                        |
| 6  | Move-ctor generation (address-insensitive types only)            |
| 7  | `cxx_copy` opt-in for copyable types                             |
