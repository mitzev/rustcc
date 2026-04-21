# Build integration and toolchain driver

**Status:** draft v0.1
**Depends on:** all prior docs.

---

## 1. Purpose

How `rustcc` slots into a user's build pipeline: what cargo manifest
extensions it introduces, what external tools it invokes, how the
pieces link into a final binary.

## 2. The `rustcc` binary

`rustcc` is a drop-in replacement for `rustc` for crates that use C++
interop. A thin driver that:

- Recognizes the `[cpp-interop]` manifest table (or a stable
  rustc-internal `--cfg cpp_interop` flag when invoked directly).
- Shells out to libclang for header parsing.
- Invokes Clang for shim compilation.
- Invokes the linker with both rustcc- and Clang-emitted objects.
- Falls through to vanilla rustc for crates without interop, so users
  can set `RUSTC=rustcc` globally.

## 3. Manifest extension

```toml
[package]
name = "my-app"

[cpp-interop]
headers = [
    "cpp/include/widget.hpp",
    "cpp/include/stringpool.hpp",
]
header-search-paths = ["cpp/include", "third-party/fmt/include"]
clang-flags         = ["-std=c++20"]
stdlib              = "libc++"        # or "libstdcxx"
sidecar             = "rustcc-api.yaml"
link-libraries      = ["widget", "fmt"]
link-search-paths   = ["target/cpp"]
```

- `headers`: roots the importer consumes.
- `header-search-paths`: `-I` flags for libclang and Clang.
- `clang-flags`: raw Clang flags passed through.
- `stdlib`: selects `-stdlib=libc++` / default; also selects which
  STL curated bindings are enabled.
- `sidecar`: YAML annotation file (see `cxx_importer.md §5`).
- `link-libraries` / `link-search-paths`: the user's compiled C++
  artifacts to link.

## 4. Build flow

```
cargo build
  → rustcc <crate>
    → [if manifest has cpp-interop]
      → invoke libclang(headers, flags)           # parse C++ once
      → produce ast.bin                           # cached
    → rustc lowering & typeck (lazy import from ast.bin)
    → codegen
      → emit foo.o                                # Rust object
      → emit foo.shims.cpp                        # shim source
    → invoke clang foo.shims.cpp -o foo.shims.o   # shim compilation
    → emit foo-cxx.hpp                            # exported header
    → [linking]
      → link foo.o foo.shims.o <user libs> -lc++ -o binary
```

libclang invocation and Clang shim compilation are independent of
Rust typeck and can run in parallel on large crates.

## 5. Toolchain detection

First run:

- Locate a Clang matching libclang's version. Mismatches are fatal
  (ABI divergence risk).
- Locate the C++ stdlib the user selected.
- Cache to `target/rustcc-toolchain.json` with a fingerprint of
  relevant env vars (`CC`, `CXX`, `PATH`). Invalidated on
  fingerprint change.

Overrides: `RUSTCC_CLANG`, `RUSTCC_LIBCLANG`, `RUSTCC_LINKER`.

## 6. Incremental builds

- libclang AST cache keyed by header-content SHA + flag SHA. A header
  edit rebuilds only affected TUs.
- Shim generation keyed by the set of imported functions. A new Rust
  call site that imports a new method regenerates one shim `.cpp` and
  recompiles only that TU.
- `#[repr(cpp)]` type defs feed rustc's normal incremental hash.

## 7. Linking

- Rust stdlib, C++ stdlib, libc++abi (or libsupc++) all linked.
- User must ensure the C++ side and the shim side were compiled against
  the same C++ stdlib. rustcc verifies via the `stdlib =` manifest key
  and errors if linking a `libc++`-built shim against `libstdc++`
  user code.
- Final link uses the user's default linker (`ld`, `lld`, `mold`) with
  the C++ driver's link-flags (`-lc++ -lc++abi` or `-lstdc++`).

## 8. IDE integration (stub)

For rust-analyzer to surface imported C++ items, rustcc exposes
`rustcc --json-hir` that dumps imported HIR stubs. rust-analyzer
consumes this alongside its normal rustc query interface. Details
deferred.

## 9. Cross-compilation

Out of scope for v1.0. Cross-compilation requires a cross-libclang
and a cross-Clang, both of which add packaging complexity. v1.0
supports host == target only. v1.1 target.

## 10. Milestones

| M# | Deliverable                                                      |
|----|------------------------------------------------------------------|
| 1  | `rustcc` driver; fall-through to rustc for non-interop crates    |
| 2  | `[cpp-interop]` manifest parsing                                 |
| 3  | libclang invocation + AST caching                                |
| 4  | Shim `.cpp` emission and Clang compilation                       |
| 5  | Generated `.hpp` emission                                        |
| 6  | Linker invocation with mixed objects                             |
| 7  | Toolchain detection + `stdlib` consistency check                 |
| 8  | Incremental rebuild correctness                                  |
