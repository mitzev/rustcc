# rustcc v1.05.0

**The "almost-everything-on-the-roadmap" release.** Six PRs since v1.04.0 close out **15 milestones / sub-milestones** spanning every part of the cxx_importer pipeline — from build orchestration (M26) through bitfield packing (M21.b/c) and per-callback wrapper structs (M15.b). The original 26-milestone roadmap is now 24 ✅ shipped + 2 open (M22 + M24, both genuinely large items).

The fork itself didn't change since v1.04.0 — every milestone in this release lives in `crates/cxx_importer`, `crates/rustc_abi_cxx`, and `crates/cxx`, none of which require the rustc fork to build. P09.50 (the v1.03.0 aarch64 sret fix) is still the most recent fork patch.

## What's new since v1.04.0

### Build-system integration (PR #12)

- **M26 — `cxx_importer::build::Build`**. A `cc::Build`-style orchestrator that drives the entire pipeline from a downstream `build.rs` in ~10 lines: parse headers via libclang → emit Rust + C++ shims → invoke `cc` → produce `lib<name>.a` archive + the right Cargo `link-search`/`link-lib`/`link-arg` directives. Closes the "click run, see a window" gap modulo the fork-rustc requirement for `extern "C++"`.

### First-sprint close-outs (PR #11)

- **M25 — sidecar YAML → `HeaderGraph::template_instantiations`**. New `SidecarSchema::collect_template_instantiations()` aggregator + `HeaderGraph::extend_from_sidecar(&schema)` plumber.
- **M11.b — free functions at TU/namespace scope**. New `free_fns.rs` module + `FreeFnSet`. Importer walks `EntityKind::FunctionDecl` (compiler builtins filtered); renderer emits one shared `unsafe extern "C++" { … }` block per scope plus per-fn safe `pub fn` wrappers with Itanium-mangled `#[link_name]`.
- **M11.c — class-scope static data members**. New `static_data.rs` module + `StaticDataSet`. Renderer emits `pub static [mut]` extern + `pub fn <name>_ptr()` accessors returning raw pointers.
- **M16.b — class-scope enum bodies**. `enum class State { ... }` inside a class flattens to `pub enum Widget_State` at module root.
- **M17.b — class-scope using aliases**. `using Tag = int;` inside a class becomes `pub type Widget_Tag = i32;` at module root.

### Third sprint — FLTK API ergonomics (PR #13)

- **M15.b — per-callback-type wrapper structs**. For aliases like `using Fl_Callback = void(Fl_Widget*, void*)` (the FLTK / GTK / X11 callback shape), emit a `<Alias>_Wrapper<F: Fn(args...)>` struct that boxes a Rust closure and exposes `(fn_ptr, user_data)` ready for the C++ API.
- **M18.b — per-arity convenience wrappers w/ synthesized defaults**. `<name>_with_defaults` (all defaults filled) + `<name>_default_<n>` (partial drops). Synthesizes Rust default literals for primitives, raw pointers, unscoped enums.
- **M20.b — `&CStr` smart parameter wrappers**. Per-method `<name>_cstr` wrapper takes `&::core::ffi::CStr` for each `*const c_char` slot and forwards via `.as_ptr()`.

### Fourth sprint — bitfields, pure virtuals, more cstr (PR #14)

- **M20.c — `&str` + `Option<&CStr>` wrappers**. `_str(arg: &str)` allocates a temp `CString` and panics on interior nul; `_opt_cstr(arg: Option<&CStr>)` maps `None` → `null()` for FLTK's nullable-label pattern.
- **M23 — pure virtual via `__cxa_pure_virtual` fall-through**. `populate_vtable_indices` now walks vtable slots by `MethodId` instead of mangled-symbol matching. Pure virtuals get a populated `vtable_index` and emit through the regular vtable-lookup path. Calling on the actually-abstract base hits `__cxa_pure_virtual`; calling on a derived override fires the override.
- **M21.b — real Itanium bitfield packing in the layout engine**. New `CxxTypeCtx::bitfield_widths` sidecar; layout engine packs consecutive same-container bitfields into one allocation unit. `RecordLayout` gains `field_bit_offsets` + `field_bit_widths` parallel arrays. The importer no longer poisons bitfield-bearing classes.

### Closeouts (PR #15)

- **M21.c — per-field bitfield getter/setter accessors**. For each bitfield: `pub fn <name>(&self) -> T` + `pub fn set_<name>(&mut self, v: T)` on the impl block. Uses `read_unaligned` / `write_unaligned`. Sign-extension on read for signed bitfields. Verified by hand-extracted runtime test: `set_a(7); a()` round-trips, `set_s(-3); s()` round-trips with sign extension.
- **M20.d — combined `_str_with_defaults` / `_opt_cstr_with_defaults`**. Cross-product of M18.b × M20.c. Suppresses when every cstr param is in the default-args tail.

### Infrastructure (PR #10)

- **Node 24 actions migration**. `actions/checkout@v4` → `@v5`, `actions/upload-artifact@v4` → `@v6` to stay ahead of GitHub's Node 20 deprecation.

## FLTK demo impact

`examples/fltk_hello/build_demo` was updated through this release to exercise the M26 pipeline end-to-end against real FLTK 1.4.5:

| Snapshot | bindings.rs | cxx_shims.cpp | archive | exported text symbols |
|----------|-------------|---------------|---------|----------------------|
| v1.04.0  | 235 KB      | 97 KB         | 182 KB  | 666 |
| v1.05.0  | **306 KB**  | 97 KB         | 182 KB  | 666 |

The +71 KB on `bindings.rs` is the M11.b/c + M15.b + M16.b/M17.b + M18.b + M20.b/c/d + M21.c convenience layers — pure Rust additions, no extra C++ trampolines.

## Roadmap state

After v1.05.0:

| Series | Status |
|--------|--------|
| Phase A — M1–M10 | ✅ |
| Phase B — M11 + M12 + M13 + M14 | ✅ |
| Phase C — M15 + M16 + M17 + M18 + M19 + M20 + M21 (every sub-milestone) | ✅ |
| v2 roadmap — M22 | ⏳ open: multi-inheritance + secondary vtables (~3-4 wk) |
| v2 roadmap — M23 | ✅ |
| v2 roadmap — M24 | ⏳ open: template-spec method extraction (~2-3 wk) |
| v2 roadmap — M25 + M26 | ✅ |

The two open items are genuinely large pieces of work. M22 is needed for Qt / LLVM / Chromium; M24 unlocks STL-using libraries.

## Breaking changes

None to user-facing APIs. `MethodId::as_index()` is new (not breaking); `RecordLayout` gained `field_bit_offsets` + `field_bit_widths` (additive); `MethodEmission` gained internal-only fields.

## Prebuilt binaries

This release ships stage-1 toolchains for:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

(Same caveats as v1.02.0/v1.03.0/v1.04.0 — the macOS-13 Intel runner pool is occasionally saturated; if the `x86_64-apple-darwin` job times out, that tarball will be missing. Source-build path documented in [`fork/INSTALL.md`](https://github.com/mitzev/rustcc/blob/main/fork/INSTALL.md).)

Install:

```bash
TARGET=aarch64-apple-darwin   # pick yours
VERSION=v1.05.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"
curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

## Acknowledgements

Six autonomous-merge sprints since v1.04.0, ~5,000 LoC delta across the cxx_importer / rustc_abi_cxx / cxx crates, 65 → 104 libclang-gated tests, every PR green on first CI cycle. The pace held because each milestone built on the foundation work from earlier sprints — by the time M21.b landed, the parser, layout engine, mangler, vtable walker, and emitter were all stable enough that bit-packing was a 1-day add against an estimated 5-8.
