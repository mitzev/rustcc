# P09.65 design — MIR-level Result wrapping for `#[rustc_cxx_throws]`

**Status**: design (Strategy 2 selected per user choice). Implementation gated on bootstrap iteration; deferred from v1.13.0-alpha to v1.13.0-beta because the surface area is genuinely multi-day work.

## What's already in place (bootstrap-validated)

- **P09.60–P09.64** ship on `main`. End-to-end: `#[rustc_cxx_throws]` calls lower to `invoke` → catch-all landingpad → `__rustcc_cxx_catch_unknown(exc_ptr)` runtime helper → **clean abort**.
- The catch BB has the helper's `CxxRawError` return in scope (currently discarded). All the codegen plumbing is done.

## What P09.65 needs to deliver

User writes:
```rust
extern "C++" {
    #[rustc_cxx_throws]
    fn open_file(path: &CStr) -> *mut File;
}

let p: Result<*mut File, CxxException> = unsafe { open_file(path) };
match p {
    Ok(f) => use_file(f),
    Err(e) => eprintln!("C++ threw: {}", e.what()),
}
```

Note the user writes the **return type as `Result<*mut File, CxxException>`** — i.e., the bindings emitter already produces the Result-typed declaration. The MIR pass bridges the gap between the Rust-visible type (`Result<T, _>`) and the IR-visible extern signature (`T`).

## Two competing sub-strategies

### 2A: Per-call MIR rewrite (no type-system change)

Constraint: the foreign-fn decl in Rust source has signature `fn open_file(path) -> Result<*mut File, CxxException>` already. The IR extern symbol has the original C++ signature (`*mut File` return).

For each Call to a CXX_THROWS callee:

1. Construct a fresh MIR local of type `Result<T, CxxException>` (where `T` is the C++-side return).
2. The Call terminator's destination becomes the Ok payload offset within that local.
3. UnwindAction becomes `Cleanup(new_bb)`.
4. The new BB synthesizes the Err variant:
   - Receives the `CxxRawError` from P09.64's helper call (via a local slot)
   - Constructs `CxxException::from_raw(raw.kind, raw.message)` via a function call
   - Stores `Result::Err(cxxexception)` into the same local
   - Branches to the call's normal target
5. The merge point sees the local in Result-shape regardless of which path was taken.

**Files touched**:
- `compiler/rustc_mir_transform/src/cxx_throws_wrap.rs` (new) — the pass.
- `compiler/rustc_mir_transform/src/lib.rs` — register the pass in `mir_built` query.
- `compiler/rustc_codegen_ssa/src/mir/block.rs` — adjust `cxx_throws_catch_pad` to write the Err variant into the same local (instead of abort).

**Estimated LoC**: ~400.

**Pros**: No type-system rewrite, contained in MIR transform layer.

**Cons**: User MUST write the Rust source with the Result return type (matching what the bindings emitter produces). The HIR-level `#[rustc_cxx_throws]` attribute is purely a codegen hint.

### 2B: HIR type rewrite + MIR codegen helper

The `#[rustc_cxx_throws]` attribute on a foreign-fn decl is processed at HIR level: the visible return type becomes `Result<T, ::cxx::CxxException>` automatically (T is the original).

This means user code doesn't need the bindings emitter — the type changes invisibly. But it's substantially more invasive:

**Files touched**:
- `compiler/rustc_hir_typeck/src/fn_ctxt/checks.rs` — adjust signature checking for CXX_THROWS foreign fns.
- `compiler/rustc_middle/src/ty/sig.rs` (or wherever) — return-type query rewrite.
- Plus the MIR pass from 2A.
- Plus a `lang_item` for `CxxException` so the type checker can refer to it.

**Estimated LoC**: ~600.

**Pros**: Cleaner user experience — write `fn foo() -> T;`, get `Result<T, _>` automatically.

**Cons**: Multi-system rewrite, langitem registration, requires CxxException to be a recognized type at the language level. Bigger risk of frontend regressions.

## Recommendation: Strategy 2A

Strategy 2A keeps the surface area in MIR transform + codegen — territories I've now iterated on through P09.61–P09.64. Strategy 2B touches the type checker which is genuinely new ground that needs more iteration.

2A also has the property that the existing v1.12.x bindings emitter is COMPATIBLE — Phase 0 emits Result-returning Rust wrappers + Result-returning safe interfaces; Phase 1 can reuse the same wrapper shape, with codegen handling the actual Result construction instead of the C++ shim handling it.

## Detailed Strategy 2A plan

### Step 1: New MIR transform pass

`compiler/rustc_mir_transform/src/cxx_throws_wrap.rs`:

```rust
//! P09.65: MIR-level Result wrapping for #[rustc_cxx_throws] calls.
//!
//! For each Call terminator targeting a #[rustc_cxx_throws] foreign
//! fn, rewrite the surrounding MIR to wrap the call's return in
//! Result<T, CxxException> and route an unwind landingpad into the
//! Err arm.

pub struct CxxThrowsWrap;

impl<'tcx> MirPass<'tcx> for CxxThrowsWrap {
    fn is_enabled(&self, sess: &Session) -> bool {
        // Run on all crates — the pass is no-op when no Call targets
        // a CXX_THROWS callee.
        true
    }

    fn run_pass(&self, tcx: TyCtxt<'tcx>, body: &mut mir::Body<'tcx>) {
        let mut rewrite_list = Vec::new();
        for (bb, bbdata) in body.basic_blocks.iter_enumerated() {
            let mir::TerminatorKind::Call { ref func, .. } = bbdata.terminator().kind else { continue };
            let Some(callee_def_id) = extract_callee_def_id(tcx, body, func) else { continue };
            if !tcx.codegen_fn_attrs(callee_def_id).flags.contains(CodegenFnAttrFlags::CXX_THROWS) {
                continue;
            }
            rewrite_list.push(bb);
        }
        for bb in rewrite_list {
            rewrite_call(tcx, body, bb);
        }
    }
}

fn rewrite_call<'tcx>(tcx: TyCtxt<'tcx>, body: &mut mir::Body<'tcx>, bb: BasicBlock) {
    // 1. Compute T (the call's actual return type) + Result<T, CxxException> layout
    // 2. Allocate a fresh local of Result<T, _> type
    // 3. Move the existing call destination's content into Ok(local)
    // 4. Add UnwindAction::Cleanup(new_bb) to the Call terminator
    // 5. Synthesize new_bb that constructs Err and stores into the local
    // ...
}
```

The hard parts (~ 200 LoC each):
- **Step 1 layout query**: `tcx.layout_of(Result<T, CxxException>)`. Need CxxException's DefId via a langitem (queued: P09.65a langitem registration).
- **Step 3 destination rewriting**: original destination `Place` points at T-shaped storage. Need to allocate Result-shaped storage + rewrite the Place + insert post-call MIR to extract Ok payload.
- **Step 5 Err construction**: synthesize MIR for `Result::Err(CxxException::from_raw(raw.kind, raw.message))`. Requires inserting a Call terminator to `CxxException::from_raw` — which means resolving the Instance for that function at MIR transform time.

### Step 2: Adjust codegen catch BB

The current P09.64 catch BB calls the runtime helper + aborts. P09.65 changes the catch BB to:
- Receive a pointer to the Result-shaped local (from the MIR pass)
- Store the CxxRawError into a slot the post-MIR-pass MIR knows about
- Branch to the Err-construction MIR block synthesized by the pass

This requires the catch BB to know which MIR-level local to write to. Plumbing it through requires either:
- A side-channel in FunctionCx (`per-call CXX_THROWS metadata`)
- A second sret-style parameter added by the MIR pass

### Step 3: Langitem for `CxxException`

So the type checker + MIR pass can refer to `cxx::CxxException`:
```rust
#[lang = "cxx_exception"]
pub struct CxxException { ... }
```

In the cxx runtime crate. Plus `rustc_hir::lang_items` registration.

### Step 4: Langitem for `CxxException::from_raw`

Same idea — so the MIR pass can synthesize a Call to it.

## Bootstrap iteration estimate

Per the v1.12.x throws-arc cycle:
- P09.65a langitem registration: 2-3 iterations
- P09.65b MIR pass core: 5-8 iterations (the layout query + Place rewriting are the most error-prone)
- P09.65c codegen catch BB rewrite: 2-3 iterations
- P09.65d integration test: 2-3 iterations to confirm `Result::Err` actually reaches the caller correctly across various T types

Total: ~12-17 incremental rebuilds. Each rebuild is ~10-20s on this machine. So ~3-6 hours of focused work *if everything goes well*. Higher variance than P09.61–P09.64 because the MIR transform layer is deeper into compiler internals.

## Decision point

P09.65 is a multi-hour commitment with real design choices (2A vs 2B, langitem placement, codegen↔MIR plumbing). Worth pausing here and shipping `v1.13.0-alpha` with P09.61–P09.64 first — those are bootstrap-validated, deliver real value (contained-failure on exceptions), and are at a natural stopping point.

After v1.13.0-alpha is tagged, P09.65 work resumes in a focused session with:
- Clear before/after state (alpha as the baseline)
- This design doc as the implementation spec
- Bootstrap iteration cycles measured + budgeted upfront
- A real integration test (e.g., the cxx_throws_demo crate) that validates the end-to-end Result behavior

## Open questions to resolve before implementation

1. **`CxxException` as a langitem**: should the type live in `cxx` (workspace runtime crate) or in `core`/`alloc`/`std`? Langitems traditionally live in std/core; an extension lang item in a non-std crate is unusual but technically possible. Tracking decision.

2. **Compatibility with v1.12.x Phase 0 bindings**: the bindings emitter currently generates Result-returning wrappers that use the C++ shim. Phase 1 codegen would need a config knob to switch between Phase 0 (C++ shim) and Phase 1 (native invoke) emission paths in cxx_importer.

3. **Test coverage**: the cxx_throws_demo example crate is the natural integration test. Once P09.65 lands, it should build + run with both Phase 0 (today's default) AND Phase 1 (new). Both paths should produce identical user-visible behavior. CI should run both.
