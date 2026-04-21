# Verification recipes

One per patch (or patch pair). Each recipe ships a minimal
reproducer that demonstrates the upstream behavior (error/reject)
vs the forked behavior (accept/succeed). Run against both
`RUSTC=<upstream nightly>` and `RUSTC=<fork build>` to confirm.

## First-run outputs (captured when P01–P06 were merged)

### Fork status

- **P01–P06 applied cleanly** to `rust-lang/rust` master (38 lines
  across 6 files).
- **Stage-1 build (`./x.py build --stage 1 compiler`) succeeds**
  in ~9 min on x86_64-apple-darwin with `download-ci-llvm = true`
  and `profile = "compiler"`.
- **Stage-1 library build (`./x.py build --stage 1 library`)**
  succeeds in ~2 min afterward, producing a usable sysroot at
  `build/x86_64-apple-darwin/stage1/lib/rustlib/`.

### Probe outputs

Against `rust-lang-rust/build/x86_64-apple-darwin/stage1/bin/rustc`:

**Upstream nightly (workspace-pinned):**
```
$ rustc --edition 2021 --crate-type=lib probe.rs -o /dev/null
error[E0552]: unrecognized representation hint
 --> probe.rs:3:8
  |
3 | #[repr(cpp)]
  |        ^^^
```

**Fork (same input):**
```
$ $FORK_RUSTC --edition 2021 --crate-type=lib probe.rs -o probe.rlib
$ echo $?
0
```

Layout assertions on `#[repr(cpp)]` types via `const _: () =
assert!(...)`:

```rust
#[repr(cpp)]
pub struct Point { pub x: i32, pub y: i32 }
const _: () = assert!(std::mem::size_of::<Point>() == 8);
const _: () = assert!(std::mem::align_of::<Point>() == 4);

#[repr(cpp, align(16))]
pub struct Vec4 { pub a: f32, pub b: f32, pub c: f32, pub d: f32 }
const _: () = assert!(std::mem::size_of::<Vec4>() == 16);
const _: () = assert!(std::mem::align_of::<Vec4>() == 16);
```

All assertions pass under the fork.

### P07 probe — empty struct size divergence

The first layout case where Rust's stock path and Itanium actually
disagree:

```rust
#[repr(cpp)] pub struct Empty;       // Itanium: size 1
#[repr(cpp)] pub struct Point { pub x: i32, pub y: i32 }
#[repr(C)]   pub struct EmptyC;      // Rust: size 0 (control)
```

Runtime output from a program linked against the P07 fork:

```
size_of::<Empty>()  = 1  (expect 1, Itanium-style)
align_of::<Empty>() = 1
size_of::<Point>()  = 8  (expect 8)
size_of::<EmptyC>() = 0  (expect 0, repr(C) unchanged)
```

Three things are proven at once:

1. `rustc_abi_cxx` is vendored, links, and runs inside the
   compiler's layout query (the bridge's `ctx.layout()` call
   returned without panic).
2. The Itanium "distinct instances, distinct addresses" rule
   actually reaches users via `size_of::<Empty>() == 1`.
3. The correction is scoped — stock `#[repr(C)]` empty structs
   still report size 0. The bridge does not bleed into the C path.

### Full P07 coverage matrix

Richer probe run against the final P07 bridge (fields mirrored
per-type into `rustc_abi_cxx::FieldDef`s), *after* the P07
follow-up that fixed the empty-aligned case:

```
repr(cpp) Empty:         size=1 align=1   (Itanium override active)
repr(cpp) Point:         size=8 align=4   (agrees with stock)
repr(cpp) Tight:         size=8 align=4   (u8,u8,[2 pad],u32)
repr(cpp,align(16)) Vec4: size=16 align=16 (alignment honored)
repr(cpp,align(4)) AlignedEmpty: size=4 align=4 (empty+alignas bump)
repr(C) EmptyC:          size=0            (control, untouched)
repr(C) PointC:          size=8            (control, untouched)
```

Seven of seven cases now land on the expected Itanium value. The
`AlignedEmpty` case was initially stuck in the bridge's defensive
fallback because `rustc_abi_cxx::layout` produced `size=1 align=4`
(violating `size % align == 0`). The follow-up swapped the order
of the "bump-to-1" and `align_up` steps in `compute_layout`, so
an empty class with `alignas(N)` now lands at `size=N align=N`
(matching Clang).

### Iteration notes

Three builds got P07 to this final state:

1. **Initial bridge** (narrow): applied `rustc_abi_cxx::layout`'s
   empty-ClassDef size (1) to every `#[repr(cpp)]` struct —
   corrupted `Point` to size 1. Caught by rustc's
   `layout_sanity_check` ICE.
2. **Narrow-correction patch**: override only when stock size
   is zero. Safe but left the bridge's per-field logic unused.
3. **Full delegation**: mirror each Rust field into a matching
   `CxxType` with `explicit_align`, run `rustc_abi_cxx::layout`
   on the real shape, apply corrections on any divergence with
   a defensive size%align invariant check. Current state.


## P01–P03 — `#[repr(cpp)]` parses

Reproducer:

```rust
// repr-cpp-parses.rs
#![crate_type = "lib"]

#[repr(cpp)]
pub struct Widget {
    pub x: i32,
    pub y: i32,
}
```

Upstream behavior:
```
error: unrecognized representation hint
 --> repr-cpp-parses.rs:3:1
  |
3 | #[repr(cpp)]
  | ^^^^^^^^^^^^
```

Forked behavior: compiles cleanly. `rustc --crate-type=lib
repr-cpp-parses.rs -o /dev/null --edition 2021` exits 0.

## P04–P05 — flag threads through

Reproducer (uses an unstable internal probe the compiler team
typically exposes via `-Zprint-type-sizes`):

```
fork-build/bin/rustc -Zprint-type-sizes repr-cpp-parses.rs \
    --crate-type=lib --edition 2021 2>&1 \
  | grep -A2 Widget
```

Expected: a `print-type-sizes: type: Widget: 8 bytes, alignment: 4`
line. The upstream fails at P01 already so there's nothing to
compare; the fork-side confirms layout matches the `rustc_abi_cxx`
computation.

## P06 — layout delegation

Reproducer:

```rust
// layout-delegation.rs
#![crate_type = "lib"]

#[repr(cpp)]
pub struct Segment {
    pub a: i32,  // offset 0
    pub b: i32,  // offset 4
    pub c: i32,  // offset 8
}

const _: () = assert!(std::mem::size_of::<Segment>() == 12);
const _: () = assert!(std::mem::align_of::<Segment>() == 4);
```

Expected: compiles cleanly on the fork. Break P06 (remove the
`cxx_bridge::layout_cpp` early-return) and the size would still
happen to be 12 on this simple POD, but a test with nested
`#[repr(cpp)]` records is the real probe — rustc's native layout
doesn't agree with `rustc_abi_cxx` on tail-padding reuse or on
classes with a `#[repr(align(N))]` override.

More aggressive reproducer (requires P06):

```rust
#[repr(cpp)]
pub struct Tight {
    pub a: u8,   // C++ Itanium: offset 0
    pub b: u8,   // offset 1
    // 2 bytes padding
    pub c: u32,  // offset 4
}

const _: () = assert!(std::mem::size_of::<Tight>() == 8);
```

## P07 — `extern "C++"` mangling

Reproducer:

```rust
// extern-cpp-mangle.rs
#![crate_type = "staticlib"]

extern "C++" {
    pub fn cpp_thing(x: i32) -> i32;
}
```

Forked behavior: `nm` on the resulting `.a` shows an undefined
reference to `_Z9cpp_thingi` (Itanium-mangled), not `cpp_thing`
(unmangled extern-C). Upstream: rejects `extern "C++"` at parse
time with "invalid ABI".

## P08 — `CXX` calling convention

Reproducer:

```rust
#[repr(cpp)]
pub struct Big {
    pub data: [u8; 32],
}

extern "C++" {
    pub fn make_big() -> Big;
}
```

Forked behavior: `rustc --emit=llvm-ir` shows `make_big` lowered
with an explicit `sret` parameter on the LLVM function type —
matching what Clang emits for `Big make_big()` where `Big` is
non-trivial-for-calls.

Absent P08, the symbol exists but the call ABI differs from what
C++ expects, and any actual runtime use corrupts the stack.

## Regression guard

Upstream `#[repr(C)]` must keep working bit-identically. Sanity:

```
./fork-build/bin/rustc --emit=obj tests/codegen/abi-repr-c.rs -o /tmp/repr-c.o
diff <(./upstream/bin/rustc --emit=obj tests/codegen/abi-repr-c.rs -o /tmp/repr-c-up.o && nm /tmp/repr-c-up.o) \
     <(nm /tmp/repr-c.o)
```

Expected: identical output. Any divergence means P05's
`ReprFlags::IS_CPP | IS_C` is leaking into the C path somehow.
