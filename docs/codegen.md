# `rustcc` codegen for C++ interop

**Status:** draft v0.1
**Depends on:** `rustc_abi_cxx`, `cxx_importer`.
**Produces:** LLVM IR for Rust compilation units and a side-channel
list of C++ shims the build driver hands to Clang.

---

## 1. Purpose

Codegen translates HIR from `cxx_importer` and from `#[repr(cpp)]` Rust
definitions into LLVM IR that respects the Itanium C++ ABI: correct
symbol references, calling convention, return-value handling, object
layout, virtual dispatch.

Property to preserve: a Rust call site and a C++ call site targeting
the same function produce identical machine code at the call boundary,
modulo the exception shim.

## 2. Calling C++ from Rust

### 2.1 Free functions

`extern "C++" fn foo(args) -> R` lowers to a call to the mangled
symbol from `rustc_abi_cxx::mangle` using the C++ calling convention
(SysV AMD64 / AAPCS64 on v1 targets), including:

- Aggregate returns ≥ target threshold via `sret` out-parameter.
- Small aggregates packed into registers per target rules.
- No member `this` argument.

### 2.2 Non-virtual methods

`obj.method(args)` lowers to `call <mangled>(&obj, args)`, with `&obj`
adjusted if `method` is inherited from a non-primary base (§2.4).

### 2.3 Virtual methods

```
let vptr = *(self as *const *const VTableEntry);
let slot = *(vptr.add(method.vtable_index));
let fn_ptr: fn(*const Self, ...) -> R = transmute(slot);
fn_ptr(self, args)
```

The vtable index comes from `rustc_abi_cxx::vtable`; the load sequence
is standard Itanium. No per-call thunk emitted.

### 2.4 Base-subobject `this` adjustment

If `Derived` inherits `base_method` from `Base` at non-zero offset:

```
call base_method(&derived + offsetof(Base))
```

Offset from `rustc_abi_cxx::layout().base_offsets`. We emit the
adjustment unconditionally; LLVM folds zero offsets.

### 2.5 Return values

C++ functions returning a non-trivially-copyable class use an sret
out-parameter; the callee invokes the relevant ctor into that storage.
Bridge:

- Caller allocates `MaybeUninit<T>` on the stack.
- Passes `&mut MaybeUninit<T>` as the sret pointer.
- On return, `MaybeUninit::assume_init()` and wrap in `CxxOwned<T>`.

Trivially copyable `T` follows register-return rules.

### 2.6 Argument passing

- **By value**, trivially copyable: registers per ABI.
- **By value**, non-trivial: C++ requires the *caller* to invoke the
  copy constructor into a stack slot the callee owns. Codegen inserts
  a `cxx_clone` call before the main call.
- **By reference** (`const T&`, `T&`, `T&&`): pass a pointer; Rust
  surface uses `&T`, `&mut T`, or `CxxMove<T>`.
- **By pointer**: pass a pointer. Rust surface uses `*const T` /
  `*mut T` (in `unsafe`) unless annotations promote to references.

### 2.7 Exception shim indirection

Every C++ call site emits a call to `__rustcc_shim_<mangled>` rather
than `<mangled>` directly, unless the target is `noexcept` or
annotated `[[rustcc::noexcept]]`. See `exception_boundary.md`.

## 3. Constructors and destructors

### 3.1 Ctor codegen

`Foo::new(args)` lowers to:

```
let storage = MaybeUninit::<Foo>::uninit();
call __rustcc_shim__ZN3FooC1E<args>(&mut storage, args);
CxxOwned::from_raw(storage.assume_init())
```

`C1` (complete-object) is always used for user-visible constructors.
`C2` (base-object) is only emitted internally when rustcc codegen
constructs a base-subobject during a synthesized derived ctor (rare;
mostly for `#[repr(cpp)]` types on the Rust side).

### 3.2 Dtor codegen

```rust
impl Drop for Foo {
    fn drop(&mut self) {
        unsafe { __rustcc_shim__ZN3FooD1Ev(self); }
    }
}
```

`D1` (complete-object) because `Drop` runs on the object Rust owns.

For `shared_reference` types, `Drop` calls the annotated release
function instead — the destructor runs from release when the count
hits zero.

## 4. `#[repr(cpp)]` type definitions

When rustc encounters a `#[repr(cpp)]` struct:

1. Build a `ClassDef` in the shared `CxxTypeCtx` from the HIR.
2. Query `rustc_abi_cxx::layout` for size / align / field offsets.
3. Override the usual Rust layout algorithm with that result.
4. For each method in an `extern "C++"` impl block, query `mangle` for
   the Itanium symbol, emit the function with that link name, use the
   C++ calling convention.
5. For virtual methods (v1.5), emit a vtable via
   `rustc_abi_cxx::vtable` and install the vptr in the Rust-emitted
   constructor.

Niche optimization is disabled for `#[repr(cpp)]` types: `Option<T>`
where `T: repr(cpp)` has size `usize + T`, not `T`.

## 5. Vtable and RTTI emission

For Rust-defined `#[repr(cpp)]` polymorphic types (v1.5):

- Vtable emitted as an LLVM global with the mangled symbol `_ZTV<name>`
  and external linkage, at the layout specified by `rustc_abi_cxx::vtable`.
- RTTI (`_ZTS`, `_ZTI`) emitted likewise, referencing base RTTI.
  Base RTTI may live in a Clang-compiled TU and resolve at link time.
- Vtable slots referencing Rust-defined method bodies use the Rust
  monomorphization's mangled symbol.

For imported C++ types, rustcc never emits `_ZTV` / `_ZTI` — those
live in the C++ side.

## 6. Object-file split

A compilation unit using C++ interop produces two object files:

- `foo.o`: rustcc-emitted Rust object; contains functions that call
  `__rustcc_shim_*` symbols.
- `foo.cxx-shim.o`: Clang-compiled object containing exception shims
  and any forced template instantiations.

The build driver (`build_integration.md`) invokes Clang with a
generated `.cpp` whose content comes from the shim generator. Both
objects link against the user's own C++ objects and libc++ / libstdc++.

## 7. LLVM integration points

Minimum-viable changes to rustc's backend:

- Introduce calling convention `CXX` which is `C` on v1 targets but
  gives us a hook for future AArch64 HFA/HVA differences and for MSVC
  ABI when we get there.
- Allow `repr(cpp)` to drive the `LayoutCalculator` via a trait object
  delegating to `rustc_abi_cxx`. Niche computation is bypassed for
  these types.
- Emit a link-name attribute on `extern "C++"` functions, bypassing
  Rust name mangling.

## 8. Thunks (deferred)

Covariant return types generate a this-adjusting thunk. In single
inheritance the adjustment is straightforward but needs the vtable
builder to track adjustment pairs. v1.1.

## 9. Milestones

| M# | Deliverable                                                      |
|----|------------------------------------------------------------------|
| 1  | Free function calls with `extern "C++"` linkage                  |
| 2  | Non-virtual method calls with `this` and base adjustment         |
| 3  | sret returns and non-trivial parameter passing                   |
| 4  | Ctor / dtor codegen with shim dispatch                           |
| 5  | Virtual method dispatch via vtable load                          |
| 6  | `#[repr(cpp)]` layout override                                   |
| 7  | Rust-emitted `_ZTV` / `_ZTI` for `#[repr(cpp)]` polymorphic types|
