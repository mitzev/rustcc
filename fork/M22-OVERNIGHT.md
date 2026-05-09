# M22 overnight sprint — status

**TL;DR**: M22 was scoped as "multi-inheritance + secondary vtables, ~3-4 wk". On investigation it turned out to actually be **two unrelated bugs** worth ~2-3 hours each, plus one more complex multi-inh emission concern that did *not* reproduce in practice. Both bugs fixed; FLTK demo bindings are now zero-dtor-skip clean. PRs #16 + #17 ready for review.

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

## What I did *not* do

The synthetic multi-inh probe (`C : A, B` with overrides on both) **already passes**:

- 2 sub-tables (primary + secondary)
- Secondary entries include this-adjustment thunks (`_ZThn8_N1CD1Ev`, `_ZThn8_N1C8b_methodEv`)
- All 3 virtual methods on `C` get a populated `vtable_index`
- Bindings emit zero skips
- The C++ shim links cleanly via `cc::Build`

So the multi-inheritance + secondary-vtable infrastructure on the layout/vtable/mangler side **was already complete** before tonight. The cxx_importer side was the gap, and the two fixes above close it for the FLTK case.

What I did *not* test (no rustcc fork toolchain installed locally, would need a separate runner):

- **Runtime dispatch correctness** — when a `&B` that actually points into a `C` calls `b_method()`, does the secondary-vtable thunk fire? Strong static evidence (vtable structure, mangled symbols, link success) suggests yes, but no executed test asserts it.
- **Cross-base method dispatch** — currently the binding for `C` only exposes methods that exist on `C`'s class definition. Calling `B::b_method` *non-virtually* through the B subobject of a C isn't ergonomic from Rust today. Tracked as M22 follow-up if needed (Qt requires this for some signal/slot patterns).
- **Diamond + virtual base** with overrides on shared methods. Probe (`probe_virtual_base_layout_and_vtable`) only checks layout/vtable shape, not bindings emission for an override.

## What I'd do next (if continuing)

1. **End-to-end runtime test**. Either: (a) install the rustcc fork toolchain on this machine and run a "C++ creates a C, hands a B* to Rust, Rust calls b_method, expects C::b_method to fire" test; or (b) add a CI job that does the same on a runner that already has rustcc.
2. **Cross-base method ergonomics**. Add `C::as_a()` / `C::as_b()` methods on the binding side that perform the offset adjustment, so Rust can hold `&B` and `&A` references into a C explicitly. This is what most C++/Rust binding tools (cxx, autocxx) do for upcasts.
3. **Diamond + virtual base override emission**. The probe shows layout works; methods that are reached through a virtual base offset slot may need a special dispatch path different from secondary-vtable thunks.
4. **Ctor-overload disambiguation**. Not M22, but the 4 remaining FLTK skips are this. The renderer produces one ctor per class today; named ctors keyed by `_<param-types>` would close the gap.

## v1.05.0 status

Tag pushed, draft release with notes pre-created, build queued at run [25598784381](https://github.com/mitzev/rustcc/actions/runs/25598784381). Three platforms running stage-1 fork/build.sh (~30-90 min each); x86_64-apple-darwin queued behind the macos-13 runner pool. Wakeup scheduled to check tarballs and publish (un-draft) the release once 3+ assets land.

## Roadmap state

| Series | Status |
|---|---|
| Phase A (M1–M10) | ✅ |
| Phase B (M11–M14) | ✅ |
| Phase C (M15–M21, every sub-milestone) | ✅ |
| M22 — multi-inheritance + secondary vtables | ✅ for the cxx_importer side; runtime dispatch validation is follow-up work |
| M23 | ✅ |
| M24 — template-spec method extraction | ⏳ open (~2-3 wk) |
| M25, M26 | ✅ |

After PR #16 and PR #17 merge, M22 can be marked closed for the FLTK use case. STL-using libraries still need M24.
