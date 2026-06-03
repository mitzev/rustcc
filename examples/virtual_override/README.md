# virtual_override

Virtual-method override across single inheritance — Rust defines both
`Animal` and `Dog` using the `class` keyword, and C++ calls through
an `Animal*` that actually points at a `Dog`. Dispatch hits the
`Dog` override exactly the way C++-native virtual dispatch would.

This demonstrates:

- `class` keyword with inheritance (`class Dog : Animal`).
- Method-modifier keywords (v1.13.5): `constructor fn` / `virtual fn`
  / `override fn` instead of the `#[constructor]` / `#[cpp_virtual]`
  attributes. rustc emits `_ZTV` / `_ZTI` / `_ZTS`.
- Override semantics: `override fn speak` (verified against the base)
  replaces `Animal::speak`'s vtable slot (P09.34); `virtual fn wag`
  adds a new virtual at a fresh slot.
- `__si_class_type_info` chain so `static_cast<Animal*>(d)` is a
  no-op at codegen and the `Animal*`'s vtable is the `Dog` vtable.

## Build and run

Requires the forked `rustc` (see top-level README for build
instructions). With `rustup default rustcc`:

```sh
cd examples/virtual_override
cargo +rustcc build --release
clang++ -c caller.cpp -o caller.o
clang   -c runner.c  -o runner.o
clang++ runner.o caller.o target/release/libvirtual_override.a -o demo
./demo
# speak-on-Dog-via-Animal* = 1003 (override OK); legs-inherited = 4;
# wag-new = 6; speak-on-plain-Animal = 50 -- OK
```

Expected exit code: 0. Non-zero output identifies which expected
value was wrong.
