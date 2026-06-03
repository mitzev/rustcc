# rustcc v1.13.5 — `class`-body method-modifier keywords

**A fork-toolchain release.** v1.13.5 adds C++-style **method-modifier
keywords** inside a `class` body, so the lowest-boilerplate surface now
mirrors C++ at the declaration site without attributes. This changes
the fork rustc patch series (new patch 37), so — unlike v1.13.1–v1.13.4
— the prebuilt toolchain tarballs are **rebuilt from the patched
compiler**, not reused from v1.13.0.

## What ships

### A. Method-modifier keywords inside `class`

Inside a `class` body you may now write:

```rust
pub class Widget {
    x: i32,

    constructor fn new(x: i32) -> Self { Self { x } }

    virtual fn poke(&self) -> i32 { self.x }
}

pub class Derived : Base {
    extra: i32,

    constructor fn new(x: i32, e: i32) -> Self {
        Self { __base: Base::new(x), extra: e }
    }

    override fn poke(&self) -> i32 { self.__base.x + self.extra }
}
```

The grammar is `(pub)? (virtual | override | constructor)* fn …`.
These are **pure parser sugar** that desugar to the existing
attributes — the emitted LLVM IR is byte-identical to the attribute
form, so all layout/codegen/mangling is unchanged:

| Keyword       | Desugars to                                | Kind                      |
|---------------|--------------------------------------------|---------------------------|
| `constructor` | `#[constructor]`                           | **contextual** keyword    |
| `virtual`     | `#[cpp_virtual]`                           | reserved keyword          |
| `override`    | `#[cpp_virtual]` + `#[rustc_cxx_override]` | reserved keyword          |

- **`constructor` is contextual** — it's only a modifier when a method
  follows, so a field literally named `constructor` (`constructor: i32`)
  still parses as a field. `virtual` and `override` are reserved Rust
  keywords, so there's no ambiguity there.
- **`override` is verified.** It is a hard compile error if the method
  does not override a virtual declared by some class in the
  polymorphic base chain (matched by name). This catches the footgun
  where a *misspelled* override silently appends a **new** vtable slot
  instead of replacing the base's — overrides are matched by name, so
  a typo would otherwise be a silent bug.

  ```
  error: method `wiggle` is marked `override` but does not override a
         virtual method from a base class
    = help: use `virtual` instead to declare a new virtual method, or
            check that a base class declares a `virtual` method with
            this name
  ```

- You can't combine a keyword modifier with the matching attribute on
  the same method (`#[cpp_virtual] virtual fn …` is an error) — pick
  one form. The attribute forms remain fully supported.

`#[rustc_cxx_override]` is a new internal no-args marker attribute. It
has no codegen effect; it exists only to drive the verify-override
check in `check_attr`.

### B. Editor support

- **VS Code extension v0.1.4**: the injection grammar now highlights
  `virtual` / `override` / `constructor` as method modifiers (only
  when a `fn` follows, so a `constructor` field is left alone), and
  `#[rustc_cxx_override]` joins the recognized attribute set. The class
  snippets and the `rustcc: New Project` scaffold now use the keyword
  forms; a new `virtual-fn` / `override-fn` / `constructor-fn` snippet
  inserts a single modified method.
- **rust-analyzer fork** (`fork/ra-patches/`, new patch 13): the
  class-body parser accepts the modifier run (`constructor` added as a
  new contextual keyword `CONSTRUCTOR_KW`; `virtual`/`override` already
  exist as reserved keywords), so the editor no longer shows false
  parse errors on the keyword surface. A `syntax` regression test
  asserts the keyword forms parse with zero errors.

### C. Tests

- New runtime probe `fork/tests/class_keyword/keyword_modifiers`
  exercises `constructor fn`, `virtual fn`, `pub virtual fn`,
  `override fn`, and a field literally named `constructor`. The
  class-keyword probe matrix is now **10/10** (5 cpp_class + 5 swift).
- Validation: the keyword forms emit **byte-identical LLVM IR** to the
  attribute forms; `override` accepts a real override, and errors on a
  non-overriding name and on a class with no base.

## Toolchain

Fork patch series grows by one: **patch 37
(`37-class-method-keywords.patch`)**. The change touches the parser
(`parse_cxx_class_item`), a new `#[rustc_cxx_override]` attribute
(symbol, `AttributeKind`, cross-crate encoding, parser + registration,
`builtin_attrs`), and the `check_attr` verify-override pass. Patches
01–36 are unchanged.

**Prebuilt toolchain tarballs for v1.13.5 are rebuilt from the patched
compiler** (5 triples: macOS arm64, Linux x86_64/arm64, Windows MSVC
x64/arm64). If you build from source, `fork/build.sh` applies 01–37
and builds stage 1 as before.

## Test status

Full workspace green; the `fork/tests/class_keyword` probe matrix is
10/10. The RA fork's `parser` (316) + `syntax` (52) test suites pass,
including the new keyword-modifier regression test.
