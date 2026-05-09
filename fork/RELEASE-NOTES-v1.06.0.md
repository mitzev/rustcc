# rustcc v1.06.0

**The "roadmap-complete" release.** Eleven PRs since v1.04.0 close out **all remaining roadmap milestones** — every milestone numbered M1 through M26 is now shipped. Highlights:

- **M22** (multi-inheritance + secondary vtables) — turned out to be three smaller pieces: a cache-pollution bug in the importer's recursive class-import path, a third-pass to converge polymorphism + vtable indices once *every* class is fully imported, and inherent `as_<base>` cross-base accessors for ergonomic upcasts.
- **M24** (template-spec method extraction) — extract methods from class-template specializations by walking the underlying generic template's children and substituting `T → int` / `K → int, V → double` / etc. by display-name lookup against the spec's argument types. Handles top-level `T`, nested `T*` / `const T*` / `T&`, and multi-parameter templates.
- **A real C++ application written in Rust** — `examples/fltk_text_editor` ships an 800x600 text editor that pulls in ~50 FLTK classes (Fl_Window, Fl_Text_Editor, Fl_Text_Buffer, Fl_Menu_Bar, Fl_Native_File_Chooser, ...). Surfaced one importer bug along the way (abstract-class ctor shims) which is fixed.

The original 26-milestone roadmap is **fully shipped** with this release. Future work tracks as v2 stretch items (CI runtime-dispatch validation, method flattening for ergonomic cross-base calls, ctor-overload disambiguation).

The fork itself didn't change since v1.04.0 — every milestone in this release lives in `crates/cxx_importer`, `crates/rustc_abi_cxx`, and `crates/cxx`, none of which require the rustc fork to build. P09.50 (the v1.03.0 aarch64 sret fix) is still the most recent fork patch. **Stage-1 toolchain tarballs in this release are bit-for-bit identical to v1.04.0's** — they exist purely so users can pin their `rust-toolchain.toml` to a single version that includes both fork rustc + the matching cxx_importer / rustc_abi_cxx crates.

## What's new since v1.04.0

The first six PRs (#10–#15) close out the original Phase B/C sub-milestone backlog. The next five (#16–#20) close M22, M24, and the editor example.

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

### M22 — multi-inheritance + secondary vtables (PRs #16, #17, #18)

The roadmap estimate was 3-4 weeks; reality was three smaller bugs / fixes layered together.

- **PR #16 — cache-pollution fix.** Found while writing the first M22 probe: `Fl_Image` ended up `polymorphic=false` in the FLTK umbrella context despite having 11 virtual methods. Root cause: `Fl_Image::label`'s `lower_method` recursed into `Fl_Window::default_icons`'s `IncompleteArray` parameter; the resulting `?` escaped `import_class` *after* the placeholder ClassDef was registered, leaving Fl_Image permanently half-imported. Fix is three-pronged: add `IncompleteArray` arm to `import_type` (treat as pointer-to-element, matching C decay); switch the in-class method walk from `?` to silent `continue` on `lower_method` failure (mirrors the `attach_methods_recursively` post-pass policy); handle `CxxType::Fn` as pointer-sized in `type_size_align` (M15 collapses `Ptr<Fn>` → `Fn` for ABI; surfaces as a layout-error regression once Fl_Widget's `Fl_Callback*` field reaches the layout engine). Plus `find_dtor_method_id` in `rustc_abi_cxx::vtable` to replace the hardcoded `MethodId(0)` for D1/D0 dtor slots.

- **PR #17 — third-pass polymorphism + vtable indices.** Eliminated the last 3 dtor-related virtual-method skips on FLTK (Fl_Group, Fl_Window, Fl_RGB_Image — all single-inheritance, all declare their own dtors). Diagnosis revealed a recursive-import-cycle bug: during Fl_Widget's body walk, `lower_method` on `Fl_Group* parent() const` triggered `import_class(Fl_Group)`. Fl_Group's body walk then saw Fl_Widget as a placeholder (methods=0, polymorphic=false) — Fl_Widget's body walk hadn't finished yet — so `build_virtual_slots(Fl_Group)` saw no virtual dtor on the chain root, no D1/D0 slots got pushed, and `populate_vtable_indices(Fl_Group)` wrote a stale index map missing the dtor. Fix: a third workspace-wide pass after `attach_methods_recursively` that recomputes `is_polymorphic` to a fixed point + re-runs `populate_vtable_indices` for every polymorphic class.

- **PR #18 — cross-base `as_<base>` accessors + general-case battery.** Six new tests cover the multi-inh shapes that FLTK alone doesn't exercise: `pure_virtual_in_multi_inh`, `triple_polymorphic_inheritance`, `compiler_generated_dtor_in_multi_inh`, `diamond_virtual_base_with_shared_override`, `cross_base_method_exposure`, `cross_base_accessors_in_fltk_umbrella`. Five passed first run — the layout/vtable/mangler/thunk infrastructure was already complete. The sixth surfaced the genuine ergonomics gap: when `C : A, B`, calling `A::a_only` from a C-typed receiver required the awkward `<C as CxxBase<A>>::upcast(&c).a_only()`. Fix: emit inherent `pub fn as_<base>(&self) -> &Base` and `as_<base>_mut(&mut self) -> &mut Base` accessors per non-virtual base. Reads naturally: `c.as_a().a_only()`. Skipped per-base on user-method name collision.

### M24 — template-spec method extraction (PR #19)

libclang's child walk on a `ClassTemplateSpecialization` cursor returns no methods — they only live on the underlying generic `ClassTemplate` cursor, with parameters surfaced as `TypeKind::Unexposed` (display name `"T"`, `"const T"`, ...). Fix:

1. `import_class` for spec entities chains back via `entity.get_template()`, builds a name → TypeId substitution map by pairing the template's TemplateTypeParameter children with the spec's `get_template_argument_types()`, stashes on `importer.current_template_subst` for the duration of the template's child walk, and restores on exit (so recursive `import_class` calls don't inherit a stale subst).
2. `import_type`'s pre-dispatch substitution: when the active subst is non-empty and the type is `Unexposed`, look up the display name (with leading `const ` / `volatile ` stripped) against the map. Hit → return the substituted TypeId; cv qualifiers preserved by parent type's `cv_from_type(pointee)`.
3. `ident_of_class_with_ctx` + `type_arg_ident`: render `TemplateSpec` segments to unique Rust identifiers — `Box<int>` → `Box_i32`, `Pair<int, double>` → `Pair_i32_f64`. Walks `CxxType` recursively for nested cases.

Five close-out tests cover single/multi/nested/multiple-spec/zero-emit-skip patterns. STL and implicit-instantiation discovery are tracked as M24 follow-ups.

### Editor example + abstract-class shim fix (PR #20)

`examples/fltk_text_editor` is a real Rust binary that opens a window, hosts an editable text widget, and runs the FLTK event loop. Exercises every layer of the stack in a single `cargo +rustcc run`:

- `Fl_Window::new_cstr(800, 600, c"...")` — Itanium C1 ctor + M20.b cstr wrapper
- `Fl_Text_Buffer::new_with_defaults()` — M18.b synthesized-default ctor
- `editor.as_fl_text_display_mut().buffer(&mut buffer)` — M22 cross-base accessor + inherited method
- `window.as_fl_group_mut().end()` — same pattern, two levels up the chain
- Virtual dispatch through Fl_Widget's vtable: `Fl_Text_Editor::handle()` fires through the M22-walked vtable slot when you type

Surfaced one real importer bug: the umbrella header transitively pulls in `Fl_Menu_`, `Fl_Input_`, and `Fl_Device_Plugin` — three abstract intermediates whose ctor shims (`extern "C" Fl_Menu_* __cxx_Fl_Menu__new_heap_0(...) { try { return new Fl_Menu_(...); } ... }`) clang rejects with "error: allocating an object of abstract class type". Fix: `is_abstract_class(ctx, class_id)` walks the class's vtable and returns true iff any `FunctionPointer` slot still references `__cxa_pure_virtual` after override resolution. The shim emitter guards heap-ctor emission with this check. Catches both directly-declared and inherited pure virtuals.

## FLTK demo impact

Two demos in `examples/`. Both exercise M26 end-to-end against real FLTK 1.4.5:

| Demo | Snapshot | bindings.rs | cxx_shims.cpp | archive | skip blocks |
|---|---|---|---|---|---|
| `fltk_hello` | v1.04.0 | 235 KB | 97 KB | 182 KB | unknown |
| `fltk_hello` | v1.05.0 (cancelled) | 306 KB | 97 KB | 182 KB | 7 (3 dtor + 4 ctor-overload) |
| `fltk_hello` | **v1.06.0** | **319 KB** | 98 KB | 182 KB | **4** (0 dtor + 4 ctor-overload) |
| `fltk_text_editor` | v1.06.0 (new) | **950 KB** | 3 MB | **470 KB** | small handful, all ctor-overload |

The +13 KB on `fltk_hello`'s bindings.rs (vs the cancelled v1.05.0 draft) is the new M22 cross-base accessors. The 3 dtor-related skips are gone — every FLTK class with a virtual destructor now emits a working `Drop` impl.

`fltk_text_editor` is the new headline example: 950 KB of generated Rust source covering ~50 FLTK classes, ready to run under the rustcc fork toolchain. See `examples/fltk_text_editor/README.md` for the build + run sequence.

## Roadmap state

After v1.06.0 — **the original 26-milestone roadmap is fully shipped**:

| Series | Status |
|--------|--------|
| Phase A — M1–M10 | ✅ |
| Phase B — M11 + M12 + M13 + M14 | ✅ |
| Phase C — M15 + M16 + M17 + M18 + M19 + M20 + M21 (every sub-milestone) | ✅ |
| v2 roadmap — M22 (multi-inheritance + secondary vtables) | ✅ |
| v2 roadmap — M23 (pure virtual via `__cxa_pure_virtual` fall-through) | ✅ |
| v2 roadmap — M24 (template-spec method extraction) | ✅ |
| v2 roadmap — M25 (sidecar template instantiations) | ✅ |
| v2 roadmap — M26 (`cxx_importer::build::Build` orchestrator) | ✅ |

Open follow-ups (tracked as v2 stretch items, not blockers):

- **Runtime-dispatch CI validation** — the M22 work has strong static evidence (vtable structure, mangled symbols, FLTK link success) but no executed test that asserts a `&B`-pointing-into-a-C call routes through the secondary thunk. Needs a CI runner with the rustcc fork toolchain pre-installed.
- **Method flattening** — `c.as_a().a_only()` is one extra hop versus `c.a_only()`. Pure ergonomics layer over the existing vtable_index machinery; defer until users complain.
- **Ctor-overload disambiguation** — the v0 emitter renders one `pub fn new()` per class. Multi-ctor classes (Fl_Window, Fl_Box, Fl_Bitmap, Fl_RGB_Image) get the rest skipped with a `///` comment naming the dropped overloads. Named-suffix disambiguation is a v0-emitter follow-up.
- **STL container support for M24** — implicit instantiations need type-resolution-chain auto-discovery; system include paths need wiring in the M26 Build helper.

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
VERSION=v1.06.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"
curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

## Acknowledgements

Eleven autonomous-merge sprints since v1.04.0, ~6,500 LoC delta across the cxx_importer / rustc_abi_cxx / cxx crates + the new `examples/fltk_text_editor/`, 65 → ~120 libclang-gated tests, every PR green on first CI cycle. The pace held because each milestone built on the foundation work from earlier sprints — by the time M22's third-pass landed, the parser, layout engine, mangler, vtable walker, importer, and emitter were all stable enough that closing what was estimated as a 3-4-week multi-inheritance milestone took an afternoon of focused diagnosis once the recursive-import-cycle pattern came into view. M24's two-week template-spec estimate dissolved similarly into a half-day of substitution-via-display-name once the libclang shape was understood.

The roadmap is done. Future work tracks separately.
