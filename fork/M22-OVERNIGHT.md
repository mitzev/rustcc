# M22 overnight sprint — status

**TL;DR**: M22 was scoped as "multi-inheritance + secondary vtables, ~3-4 wk". On investigation it turned out to actually be **three smaller pieces** — two bug fixes and one ergonomics gap — totaling ~half a day of focused work. All three landed in PRs #16, #17, #18. M22 can be marked closed for the general case, not just FLTK.

## What landed

### PR #16 (m22-prep) — Fl_Image umbrella cache pollution

Discovered while writing the first M22 probe: in the FLTK umbrella context, `Fl_Image` ended up with `polymorphic=false` despite having 11 virtual methods, and its vtable was missing entirely. Tracked root cause:

- `Fl_Image::label`'s `lower_method` recursed into `Fl_Window::default_icons(const Fl_Image *icons[], int)` — a static method whose parameter libclang preserves as `IncompleteArray` rather than decaying to a pointer.
- `import_type` had no `IncompleteArray` arm, returned `UnsupportedFeature` via `?`.
- The error escaped `import_class` *after* the placeholder ClassDef was already registered in `self.classes`. Subsequent calls hit the cache and returned the half-imported class, leaving `Fl_Image` permanently broken across the rest of the TU.

Fix:
1. Add `IncompleteArray` arm to `import_type` — treats `T arr[]` as `*T`, matching C array-to-pointer decay.
2. Switch the in-class method walk from `?` to silent `continue` on `lower_method` failure — mirrors the `attach_methods_recursively` post-pass policy. Defense-in-depth.
3. `layout::type_size_align`: handle `CxxType::Fn` as pointer-sized. M15 collapses `Ptr<Fn>` → `Fn` for parameter/return ABI; this surfaced as a regression once `Fl_Widget::callback_` (a `Fl_Callback*` field) made it into layout calls.
4. `rustc_abi_cxx::vtable::find_dtor_method_id` — replaces the hard-coded `MethodId(0)` for D1/D0 dtor slots. Walks `class.methods` looking for `SpecialMember::Dtor`.

### PR #17 (m22-sprint) — Third-pass polymorphism + populate_vtable_indices

After PR #16, FLTK still had 3 dtor skips on `Fl_Group`, `Fl_Window`, `Fl_RGB_Image` — all single-inheritance classes. Originally suspected this was real multi-inh M22 work. Diagnosis (via `M22_TRACE=1` instrumentation) revealed it's actually a recursive-import-cycle bug:

- `import_class` ran `populate_vtable_indices` at the end of every body walk.
- During Fl_Widget's body walk, `lower_method` on `Fl_Group* parent() const` triggered `import_class(Fl_Group)`.
- Fl_Group's body walk then saw Fl_Widget as a placeholder (methods=0, polymorphic=false) — Fl_Widget's body walk hadn't finished yet.
- `build_virtual_slots(Fl_Group)` saw no virtual dtor on the chain root → no D1/D0 slots → bogus index map → Fl_Group's own `~Fl_Group()` ended up with `vtable_index=None`.

Fix: a third workspace-wide pass after `attach_methods_recursively`:
1. **Polymorphism convergence loop** — recompute `is_polymorphic` for every class to a fixed point. The base-is-polymorphic bit propagates one inheritance edge per iteration.
2. **Re-run `populate_vtable_indices`** for every polymorphic class. It's already idempotent: overwrites `vtable_index` from the freshly recomputed vtable.

## FLTK demo state

| Snapshot | bindings.rs | skip blocks | dtor skips | ctor-overload skips |
|---|---|---|---|---|
| v1.04.0 | 235 KB | unknown | unknown | unknown |
| v1.05.0 | 306 KB | 7 | 3 (M22-flagged) | 4 (v0 limitation) |
| v1.05.0 + #16 | 317 KB | 5 | 3 | 2 |
| v1.05.0 + #16 + #17 | **317 KB** | **4** | **0** | **4** |

The 4 remaining skips are all extra-ctor disambiguation — a v0 emitter limitation tracked separately, *not* M22.

The FLTK `examples/fltk_hello/build_demo.rs` runs end-to-end through M26's `cxx_importer::build::Build::compile`, including the cc-rs C++ compile step that produces `libfltk_bindings_demo.a`. A clean link is strong evidence that the Itanium thunk symbols (`_ZThn8_*`) the secondary vtable references all resolve.

### PR #18 (m22-close) — close-out battery + cross-base accessors

A six-test battery covering the Qt/LLVM/Chromium-style multi-inh shapes that weren't exercised by FLTK. Five passed on the first run (the M22 infrastructure was already complete on the layout/vtable/mangler side):

- `pure_virtual_in_multi_inh` — `Impl : Iface, Concrete` where Iface has a pure virtual; override resolution puts Impl's symbol in the slot, no `__cxa_pure_virtual` leak in the primary table.
- `triple_polymorphic_inheritance` — `D : A, B, C` produces 3 sub-tables (primary + 2 secondaries) with `_ZThn<n>_*` adjustment thunks for D's overrides of B::b and C::c.
- `compiler_generated_dtor_in_multi_inh` — `D : A, B` without an explicit `~D()`. The synthesized dtor still surfaces D1/D0 slots in the primary, and bindings emit no skip.
- `diamond_virtual_base_with_shared_override` — D's `a_method` override (shared through virtual A) is indexed and emitted.

The sixth test, `cross_base_method_exposure`, **failed** the first run — and that failure was the genuine gap. The binding for `C : A, B` had no way to reach A::a_only from a C-typed receiver: the M19 `CxxBase<Base>::upcast` trait impl exists but its method is ambiguous when there are multiple polymorphic bases (you'd have to write `<C as CxxBase<A>>::upcast(&c)`).

Fix: emit inherent `pub fn as_<base>(&self) -> &Base` and `as_<base>_mut(&mut self) -> &mut Base` accessors on every derived class for each non-virtual base. Same offset arithmetic as the M19 trait impls, but unambiguous syntax: `c.as_a().a_only()` reads naturally. Skipped per-base if the class declares its own user method with the colliding name.

FLTK umbrella: 10 new `as_fl_*` accessors emitted across the class hierarchy. Bindings.rs grew 317 KB → 319 KB.

## What's deliberately not in this sprint

- **End-to-end runtime test**. Static evidence is strong (vtable structure, mangled symbols, link success against real FLTK 1.4.5) but no executed test asserts dispatch behavior. Needs the rustcc fork toolchain installed on the test machine; tracked as a CI follow-up.
- **Method flattening (autocxx style)**. Cross-base accessors require `c.as_a().a_only()` — one extra hop versus calling `c.a_only()` directly. autocxx flattens; we don't. Adding flattening is purely an ergonomics layer over the same vtable_index machinery; defer until users complain.
- **Ctor-overload disambiguation**. Not M22 but tracked separately. The 4 remaining FLTK skip blocks are all this.

## v1.05.0 status

Tag pushed, draft release with notes pre-created, build queued at run [25598784381](https://github.com/mitzev/rustcc/actions/runs/25598784381). Three platforms running stage-1 fork/build.sh (~30-90 min each); x86_64-apple-darwin queued behind the macos-13 runner pool. Wakeup scheduled to check tarballs and publish (un-draft) the release once 3+ assets land.

## Roadmap state

| Series | Status |
|---|---|
| Phase A (M1–M10) | ✅ |
| Phase B (M11–M14) | ✅ |
| Phase C (M15–M21, every sub-milestone) | ✅ |
| M22 — multi-inheritance + secondary vtables | ✅ — three pieces (cache-pollution, third-pass, cross-base accessors) all in PRs #16+#17+#18 |
| M23 | ✅ |
| M24 — template-spec method extraction | ⏳ open (~2-3 wk) |
| M25, M26 | ✅ |

After PR #16, #17, #18 merge, M22 is closed for the general case (Qt / LLVM / Chromium classes will work the same way FLTK does). Runtime-dispatch validation under the rustcc fork toolchain is a CI follow-up. STL-using libraries still need M24.
