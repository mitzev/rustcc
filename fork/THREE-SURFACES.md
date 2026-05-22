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

    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub fn sum(&self) -> i32 {
        self.x + self.y
    }
}
```

Supports single inheritance directly in the syntax:

```rust
pub class Derived : Base {
    extra: i32,

    pub fn new(x: i32, extra: i32) -> Self {
        Self { __base: Base::new(x), extra }
    }
}
```

The parser synthesizes a `__base: Base` field with
`#[rustc_cxx_base]` so downstream layout and vtable emission
recognizes the polymorphic base subobject automatically.

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
| `#[constructor]`  | `cxx_class!`, `cxx_class_native!`| Marks a method as `Foo::Foo(...)` in C++ mangling   |
| `#[cpp_virtual]`  | All three                        | Adds a slot in the vtable; overridable              |
| `#[operator]`     | All three                        | Operator overload (`op_add` → `operator+`, etc.)    |
| `#[swift_type]`   | `swift_value!` macro             | Marks a type as `#[repr(swift)]`                    |
| `#[swift_symbol]` | `swift_value!` macro             | Swift demangled name for a foreign item             |

The parser `class` keyword doesn't need `#[constructor]` because
it uses Rust's normal `Self::new(...)` convention, which the
parser-level sugar translates to the Itanium ctor mangling.

---

## Further reading

- `fork/PATCHES.md` §§ P09.30 / P09.33 / P09.39 — history of how
  the three surfaces evolved.
- `fork/getting-started.html` — integration walkthrough with
  build recipes.
- `/tmp/p09-39-itemkind-class/` — minimal working probe of the
  `class` keyword including single inheritance.
