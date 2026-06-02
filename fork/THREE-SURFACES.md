# Three surfaces for writing C++-interop classes

rustcc ships three different ways to declare a class that speaks
the Itanium C++ ABI. Picking the right one depends on whether you
need stock rustc compatibility, what level of boilerplate you
want, and whether you're willing to use fork-only syntax.

At a glance:

| Surface                 | Works on stock rustc? | Syntax                            | Boilerplate          |
|-------------------------|-----------------------|-----------------------------------|----------------------|
| `cxx_class!`            | **Yes**               | Proc macro over a struct literal  | Medium               |
| `cxx_class_native!`     | No (needs fork)       | Declarative macro over Rust items | Low                  |
| `class` keyword         | No (needs fork)       | C++-style `class Foo { ... }`     | Lowest               |

All three produce identical machine code. They differ in syntax
ergonomics and whether stock-rustc compatibility matters to you.

---

## `cxx_class!` — stock-rustc compatible

A proc macro exported from `rustcc_macros`. Works on any rustc —
including the nightly pinned in the workspace — because it
produces ordinary Rust items (a `#[repr(C)]` struct plus
`extern "C"` function decls, with attributes that the fork's
compiler passes handle but stock rustc ignores silently).

```rust
rustcc_macros::cxx_class! {
    pub struct Widget {
        x: i32,
        y: i32,
    }

    impl Widget {
        #[constructor]
        pub fn new(x: i32, y: i32) -> Self;

        pub fn sum(&self) -> i32;
    }
}
```

**Use when**:
- You want your code to compile on stable / nightly rustc as well
  as the fork, and you're only getting the behavior you care
  about on the fork.
- You're building a library that should degrade gracefully on
  non-fork toolchains.
- You need to work alongside existing `cxx` crate bindings.

**Give up**:
- Doesn't support the parser-level `class` keyword syntax.
- Can't express parser-synthesized features like inheritance via
  `class D : B` (for that, use the keyword).

---

## `cxx_class_native!` — fork-only declarative macro

A declarative (`macro_rules!`) macro exported from
`rustcc_macros`. Requires the fork's `#[rustc_cxx_wrapper]`,
`#[rustc_cxx_drop_wrapper]`, and `#[rustc_cxx_virtual]`
attributes — stock rustc rejects all three.

```rust
rustcc_macros::cxx_class_native! {
    pub struct Widget {
        x: i32,
        y: i32,
    }

    impl Widget {
        #[constructor]
        pub fn new(x: i32, y: i32) -> Self;

        pub fn sum(&self) -> i32;
    }
}
```

Output is essentially the same as `cxx_class!` but with the
fork-only attributes wired up directly. The macro is simpler —
no proc-macro invocation cost, easier to reason about, easier to
fix when something breaks.

**Use when**:
- You're writing fork-only code and want faster compile times
  than `cxx_class!`'s proc-macro invocation costs.
- You want the macro expansion to be reviewable in `cargo expand`
  without proc-macro plumbing between you and the result.

**Give up**:
- Source files won't compile on stock rustc.
- Slightly more verbose than the `class` keyword.

---

## `class` keyword — fork-only syntactic sugar

The lowest-boilerplate surface. Uses the fork parser's
recognition of `class` as a weak keyword (P09.30). Lowers to a
`#[repr(cpp)]` struct plus an inherent impl at AST → HIR
(P09.39).

```rust
pub class Widget {
    x: i32,
    y: i32,

    constructor fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    virtual fn sum(&self) -> i32 {
        self.x + self.y
    }
}
```

Supports single inheritance directly in the syntax:

```rust
pub class Derived : Base {
    extra: i32,

    constructor fn new(x: i32, extra: i32) -> Self {
        Self { __base: Base::new(x), extra }
    }

    override fn sum(&self) -> i32 {
        // `self.x` reaches the base field transparently (v1.13.6) —
        // no `self.__base.x` needed.
        self.x + self.extra
    }
}
```

The parser synthesizes a `__base: Base` field with
`#[rustc_cxx_base]` so downstream layout and vtable emission
recognizes the polymorphic base subobject automatically.

### Transparent base-member access (v1.13.6)

For a derived `class D : B`, the fork synthesizes
`impl Deref for D { type Target = B; … }` (and `DerefMut`) targeting
the `__base` subobject, so **base members are reachable directly**
through Rust's existing autoderef:

```rust
let d = Derived::new(10, 5);
let _ = d.x;          // base field      (was: d.__base.x)
let _ = d.get_x();    // base method     (was: d.__base.get_x())
d.x = 7;             // base field write (via DerefMut)
fn takes_base(b: &Base) {}
takes_base(&d);       // &Derived -> &Base upcast coercion
```

This works transitively up a multi-level chain, and through generic
bases. A derived field shadows a base field of the same name (the
derived one wins — C++ name-hiding), and `self.__base.member` still
works for explicit access. Because it's plain autoderef, the emitted
code is just a field projection at offset 0 — no runtime cost.

> Editor note: rust-analyzer (fork) does not yet resolve the
> transparent form in its native `class` model, so `self.base_member`
> may show an unresolved-field/method diagnostic in the editor even
> though it compiles. Use `self.__base.member` for editor-clean code
> until the RA follow-up lands.

### Method-modifier keywords (v1.13.5)

Inside a `class` body you may write C++-style method modifiers
instead of the attribute forms — `(pub)? (virtual | override |
constructor)* fn`:

| Keyword       | Desugars to                            | Notes                                            |
|---------------|----------------------------------------|--------------------------------------------------|
| `constructor` | `#[constructor]`                       | Contextual keyword — a field named `constructor` still parses |
| `virtual`     | `#[cpp_virtual]`                       | Reserved keyword                                 |
| `override`    | `#[cpp_virtual]` + `#[rustc_cxx_override]` | Reserved keyword; **verified** — see below   |

These are pure parser sugar: they produce byte-identical machine
code to the attribute forms, so all downstream layout/codegen is
unchanged. `override` additionally triggers a **verify-override
check** — it is a hard compile error if the method does not
override a virtual declared by some class in the polymorphic base
chain (matched by name). That catches the footgun where a
misspelled override silently *adds* a new vtable slot instead of
replacing the base's (overrides are matched by name).

You cannot combine a keyword modifier with the matching attribute
on the same method (`#[cpp_virtual] virtual fn …` is an error) —
pick one form. The attribute forms remain fully supported; the
keywords are the lowest-boilerplate option and mirror C++ at the
declaration site.

**Use when**:
- You want the cleanest possible syntax — mirrors C++ at the
  declaration site, which eases porting.
- You need single inheritance.
- You're OK with fork-only source files.

**Give up**:
- Source files won't compile on stock rustc.
- Less grep-able than `struct Foo` + `impl Foo`: the struct name
  and method list are in the same AST item. Tooling that scans
  for `struct` won't find classes.

Editor support is shipped: the patched rust-analyzer in
`fork/ra-patches/` (Phase 1 parser via P09.45 + Phase 2 HIR / IDE
parity via the 12-patch series in v1.07.0) gives `class` items
full hover, go-to-def, find-references, completion, and assist
support. Install the RA fork via the `vscode-rustcc` extension's
`Install RA Fork (latest)` command, or build locally with
`./fork/ra-patches/build.sh`.

---

## Which should I use?

Rule of thumb:

- **Publishing a library?** → `cxx_class!`. Stock-rustc
  compatibility is worth the proc-macro cost.
- **Writing a fork-only internal crate that touches many
  classes?** → `class` keyword. The syntax compresses.
- **Writing a fork-only internal crate where you want
  `cargo expand` to be legible?** → `cxx_class_native!`.
- **Need inheritance?** → `class` keyword (the only surface that
  spells it out in the source).

All three interoperate. A class declared with the keyword can be
constructed, called, and inherited from by code that uses any of
the three surfaces. Vtable and RTTI layout are identical across
all three.

---

## Surface-specific attributes

The fork's `#[rustc_cxx_*]` / `#[rustc_swift_*]` attributes are
internal implementation details; users write friendlier names
that the macros re-emit. See P09.33 in `PATCHES.md` for the
rename table. Common user-facing attributes:

| User attribute    | Works on which surface?          | Purpose                                             |
|-------------------|----------------------------------|-----------------------------------------------------|
| `#[constructor]`  | All three                        | Marks a method as `Foo::Foo(...)` in C++ mangling   |
| `#[cpp_virtual]`  | All three                        | Adds a slot in the vtable; overridable              |
| `#[operator]`     | All three                        | Operator overload (`op_add` → `operator+`, etc.)    |
| `#[swift_type]`   | `swift_value!` macro             | Marks a type as `#[repr(swift)]`                    |
| `#[swift_symbol]` | `swift_value!` macro             | Swift demangled name for a foreign item             |

On the `class` keyword surface, the `constructor` / `virtual` /
`override` method-modifier keywords (v1.13.5) are equivalent to
the `#[constructor]` / `#[cpp_virtual]` attributes and are usually
preferred there — see *Method-modifier keywords* above. The class
keyword also accepts a bare `Self::new(...)` without `#[constructor]`
(the parser sugar applies the Itanium ctor mangling), but spelling
`constructor fn new` makes the intent explicit.

---

## Further reading

- `fork/PATCHES.md` §§ P09.30 / P09.33 / P09.39 — history of how
  the three surfaces evolved.
- `fork/getting-started.html` — integration walkthrough with
  build recipes.
- `/tmp/p09-39-itemkind-class/` — minimal working probe of the
  `class` keyword including single inheritance.
