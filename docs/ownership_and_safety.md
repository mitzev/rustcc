# Ownership and safety model

**Status:** draft v0.1
**Depends on:** `codegen` design, `cxx_importer` annotations.

---

## 1. Goals

- A C++ type imported into Rust fits Rust's ownership/borrow model
  without forcing `unsafe` for routine operations.
- The model handles address-sensitive types (move-ctor with observable
  side effects) and reference-counted types idiomatically.
- Misuse is a compile-time error where possible, a pinning or drop
  check where not.
- An escape hatch exists for types the model can't capture correctly.

## 2. Ownership taxonomy

Every imported class has a kind; default is `value`. Overridable via
annotation.

| Kind                 | Annotation                        | Rust surface                              |
|----------------------|-----------------------------------|-------------------------------------------|
| value                | (default)                         | `CxxOwned<T>`, `&T`, `&mut T`             |
| shared_reference     | `[[rustcc::shared_reference]]`    | `CxxShared<T>` (like `Arc<T>`)            |
| unique               | `[[rustcc::unique]]`              | `CxxOwned<T>` only; no `cxx_clone`        |
| immortal             | `[[rustcc::immortal]]`            | `&'static T`                              |
| unsafe               | `[[rustcc::unsafe]]`              | Every method is `unsafe fn`               |

## 3. Smart wrappers

### 3.1 `CxxOwned<T>`

Owned C++ value. Non-`Copy`. Runs the dtor on `Drop`. Construction:

```rust
let s: CxxOwned<Widget> = Widget::new(42);
```

Internally a `Pin<Box<MaybeUninit<T>>>` with an init flag. Handing out
`&T` / `&mut T` is safe after init. A Rust-level move is a bitwise move
of the `Box` pointer — the underlying object's address never changes.
This is the key property that makes address-sensitive C++ types sound.

### 3.2 `CxxBox<T>`

Heap-owned; alias of `CxxOwned<T>` with explicit heap allocation. The
distinction is internal; users see one name.

### 3.3 `CxxStack<T>` via macro

```rust
cxx_stack!(w: Widget = Widget::new(42));
// w: Pin<&mut Widget>
```

Expands to `MaybeUninit<T>` on the current stack frame, constructs in
place, shadows `w` with a `Pin<&mut T>`. `Drop` runs at scope exit.
Avoids the heap of `CxxBox` when the user can prove lifetime.

The macro is deliberate: a plain `let` binding can't express
"stack-allocated, pinned, C++-constructed" because a Rust-level move
would invalidate self-references. The macro pins in place.

### 3.4 `CxxShared<T>`

For `shared_reference` types. `Clone` calls the annotated retain fn;
`Drop` calls release. Not `Send` / `Sync` unless annotation says
`atomic_refcount = true`.

```cpp
class [[rustcc::shared_reference(
    retain          = "Widget_retain",
    release         = "Widget_release",
    atomic_refcount = true
)]] Widget { ... };
```

## 4. Move and copy

### 4.1 No implicit Rust-side copies

Rust `Clone` is not automatically linked to C++ copy-ctor. The user
writes `.cxx_clone()`, which the importer emits as a method calling
the copy-ctor into a fresh `CxxOwned<T>`. Types with a deleted
copy-ctor have no `cxx_clone`.

### 4.2 Rust move ≠ C++ move

A Rust move is a memcpy of the handle (e.g. the `Box` pointer) that
doesn't touch the C++ object's address. To invoke the C++ move-ctor —
e.g. for a C++ API that consumes an rvalue reference — the user writes:

```rust
fn consume(v: CxxMove<Widget>);
consume(CxxMove::from(my_owned));
```

`CxxMove<T>` wraps a `CxxOwned<T>` and signals to codegen that the
call site should invoke the move-ctor (or move-assignment) rather than
pass by copy or reference.

### 4.2.1 Construction is in place (v1.14)

A Rust `class` value used to be built in a temporary and bitwise-moved
to its binding — fatal for C++ constructors that *escape `this`* (FLTK
widgets registering ctor-created children). Since v1.14 the fork's
`cxx_ctor_inplace` MIR pass removes the temporaries along the whole
construction chain: `ptr.write(D::new(..))` constructs directly into
`*ptr`, a `#[constructor]` body's `Self { __base: Base::new(..), .. }`
constructs the base directly into the base subobject, and an imported
binding's by-value `new` constructs into its sret return slot. Ctor-time
self-references are therefore born at the final address. Moves *after*
construction remain bitwise — pinning guidance below still applies to
any later relocation.

### 4.3 No `Copy`

`#[repr(cpp)]` types never implement `Copy`, even if the underlying
C++ class is trivially copyable. Rationale: triviality can change
silently when a header adds a dtor, and we don't want automatic `Copy`
to break on upgrade. Users who want a `Copy` wrapper for a genuinely
trivially-copyable type can write one explicitly.

## 5. Drop integration

`Drop` runs where Rust expects (scope end, temporary end, panic
unwind). Generated `drop` calls `D1` via the exception shim, so a C++
destructor that throws calls `std::terminate` rather than unwinding
into Rust frames. Matches the standard recommendation that dtors be
`noexcept` and enforces it structurally.

Drop order follows Rust: fields drop in declaration order, locals in
reverse. For imported derived types, Rust `Drop` calls `D1` only;
base-dtor chaining is internal to the C++ ABI. Rust does not
separately invoke base dtors.

## 6. References

### 6.1 Defaults

`&T` ↔ `const T&`. `&mut T` ↔ `T&`. Lifetimes come from the method
signature after elision. Always safe-default because Rust references
are non-null and valid for their lifetime.

### 6.2 Raw pointers

C++ `T*` → `*mut T` by default; methods become `unsafe fn`. Null-ness
is unknown.

- `[[rustcc::nonnull]]` promotes `*mut T` to `&mut T` with an inferred
  lifetime.
- `[[rustcc::nullable]]` keeps it as `Option<&mut T>` (still safe).

### 6.3 Lifetime bounds

`[[clang::lifetimebound]]` on a parameter means "the return value's
lifetime is bound by this parameter." The importer emits a real Rust
lifetime:

```cpp
const std::string& get() const [[clang::lifetimebound]];
```

becomes

```rust
fn get<'a>(&'a self) -> &'a CxxString;
```

Without the attribute, the default for methods is "return lifetime
= `&self` lifetime" — conservative but usually right. For free
functions without `self`, absence forces a raw pointer return.

## 7. Borrow checker interaction

`#[repr(cpp)]` types participate in the borrow checker like any Rust
type. A `&mut T` is exclusive; `&T` is shared. There is no magic
escape for C++ interior mutability — users who import a class that
mutates through `const&` (common in legacy C++ for "logically const")
must either annotate `[[rustcc::interior_mutable]]` (which wraps
fields in `UnsafeCell` in the Rust view) or accept that `&T` is
unsound and use `*const T`.

## 8. Escape hatch

`[[rustcc::unsafe]]` makes every method of the class `unsafe fn` and
drops all lifetime inference on its references. For legacy APIs or
ownership stories that don't fit the taxonomy. Users wrap it
themselves.

## 9. Milestones

| M# | Deliverable                                                      |
|----|------------------------------------------------------------------|
| 1  | `CxxOwned<T>` core with `Drop` integration                       |
| 2  | Method dispatch with `&self` / `&mut self`                       |
| 3  | `cxx_stack!` macro and `Pin` integration                         |
| 4  | `cxx_clone` for copyable types                                   |
| 5  | `CxxMove<T>` and move-ctor codegen                               |
| 6  | `CxxShared<T>` and `shared_reference` annotation                 |
| 7  | Lifetime-bound inference from `[[clang::lifetimebound]]`         |
| 8  | Escape hatch annotation + `unsafe` auto-propagation              |
