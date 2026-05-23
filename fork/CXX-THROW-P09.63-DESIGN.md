# P09.63 design — wiring `cxx_catch_landing_pad` into `do_call`

**Status**: design — implementation requires fork rustc bootstrap iteration. P09.61 (fn_can_unwind) + P09.62 (`cxx_catch_landing_pad` trait method + LLVM/GCC impls) are shipped scaffolding; this patch wires them together and adds the runtime-helper call. Bootstrap validation is the canonical correctness check.

## What's already in place

- **P09.60** (v1.12.5, patch 21): `#[rustc_cxx_throws]` attribute registered, sets `CodegenFnAttrFlags::CXX_THROWS = 1 << 20`.
- **P09.61** (v1.13.0, patch 22): `fn_can_unwind` returns true for CXX_THROWS callees regardless of declared ABI. This forces `do_call` into the `invoke`-with-landingpad branch.
- **P09.62** (v1.13.0, patch 23): `BuilderMethods::cxx_catch_landing_pad(pers_fn) -> (exn_ptr, ti_idx)` emits the Itanium catch-all landingpad. LLVM impl uses `landingpad { ptr, i32 } catch ptr null`. GCC stubs delegate to `cleanup_landing_pad` for now.
- **Runtime helper** (v1.12.5): `cxx::native_invoke::__rustcc_cxx_catch_unknown(exc_ptr: *mut c_void) -> CxxRawError`. Lives in the runtime crate; lookup-by-symbol-name at codegen time.

## P09.63 scope

Modify `do_call` in `compiler/rustc_codegen_ssa/src/mir/block.rs` so that when the callee has `CXX_THROWS`:

1. Synthesize a custom unwind basic block (instead of using `llbb_with_cleanup`).
2. Inside that BB:
   - Call `cxx_catch_landing_pad(pers_fn)` to get `(exn_ptr, _ti)`.
   - Look up + call `__rustcc_cxx_catch_unknown(exn_ptr)` to get a `CxxRawError`.
   - Store the `CxxRawError` somewhere the caller can read it.
   - Branch to the call's success-path basic block.

The "store somewhere the caller can read it" is the wrinkle — see [§return-type rewriting](#p0963b-return-type-rewriting) below.

## Code shape

```rust
// In rustc_codegen_ssa::mir::block::do_call, immediately
// after the `caller_attrs` lookup at ~line 206:

let callee_throws = instance
    .map(|inst| {
        bx.tcx()
            .codegen_fn_attrs(inst.def_id())
            .flags
            .contains(CodegenFnAttrFlags::CXX_THROWS)
    })
    .unwrap_or(false);

let unwind_block = if callee_throws {
    // P09.63: synthesize a catch BB instead of the default
    // cleanup path. The BB lives in the caller function's
    // body and gets used as the `invoke`'s unwind target.
    let catch_bb = Bx::append_block(fx.cx, fx.llfn, "cxx_throws_catch");
    let mut catch_bx = Bx::build(fx.cx, catch_bb);

    // Itanium catch-all landingpad: { ptr, i32 } catch ptr null.
    let pers_fn = fx.get_personality_slot(&mut catch_bx).get_personality_fn();
    let (exn_ptr, _ti_idx) = catch_bx.cxx_catch_landing_pad(pers_fn);

    // Look up __rustcc_cxx_catch_unknown by symbol name. Lives
    // in the user's `cxx` runtime crate; declared `extern "C"
    // fn(*mut c_void) -> CxxRawError` so the codegen-level
    // signature is `i32, ptr` returned via the sysv-aggregate
    // shape `{ i32, ptr }` (2-word struct).
    let helper_fn = fx.cx.get_external_fn(
        "__rustcc_cxx_catch_unknown",
        FnAbi {
            args: vec![ArgAbi::indirect_passing_pointer()],
            ret: ArgAbi::direct_aggregate(2_words),
            conv: Conv::C,
            can_unwind: false,
        },
    );
    let raw_err = catch_bx.call(
        helper_ty,
        None,
        Some(&helper_abi),
        helper_fn,
        &[exn_ptr],
        None,
        None,
    );

    // P09.63b: store raw_err into the function's
    // implicit-sret-style out slot for the Result::Err variant.
    // See return-type-rewriting section.

    // After the catch handler, branch to the regular success
    // block — the Result wrapping happens via the rewritten
    // function signature.
    let ret_llbb = if let Some((_, target)) = destination {
        fx.llbb(target)
    } else {
        fx.unreachable_block()
    };
    catch_bx.br(ret_llbb);
    Some(catch_bb)
} else {
    // Existing path — cleanup landingpad or none per
    // UnwindAction.
    match unwind {
        mir::UnwindAction::Cleanup(cleanup) => Some(self.llbb_with_cleanup(fx, cleanup)),
        mir::UnwindAction::Continue => None,
        mir::UnwindAction::Unreachable => None,
        // ... existing Terminate arm ...
    }
};
```

## §P09.63b: Return-type rewriting

For the catch BB to "produce" a Result::Err, the function's MIR-level return type must already be `Result<T, CxxException>` (not `T`). Two strategies:

### Strategy A: MIR pass that wraps the foreign fn

A new `cxx_throws_wrap` MIR pass synthesizes a wrapper function per `#[rustc_cxx_throws]` foreign decl:

```rust
// User wrote:
extern "C++" {
    #[rustc_cxx_throws]
    fn open_file(path: &CStr) -> *mut File;
}

// Pass synthesizes (at MIR level):
fn open_file(path: &CStr) -> Result<*mut File, CxxException> {
    let raw = open_file_raw(path);   // calls into IR-level `open_file`
    Ok(raw)
    // Err arm injected by the codegen catch BB at IR level.
}

extern "C++" {
    #[rustc_cxx_throws]
    #[link_name = "_Z9open_filePKc"]
    fn open_file_raw(path: &CStr) -> *mut File;  // raw FFI signature
}
```

Pros: cleanly Rust-level, the rest of the compiler sees a normal `fn -> Result<T, _>`.
Cons: ~300 LoC across `rustc_mir_transform` + per-symbol renaming + caller-side resolution.

### Strategy B: Codegen-only return slot mutation

The codegen layer emits an LLVM IR signature where the return slot is the Result's layout. The caller already allocates a Result-shaped slot (via the bindings-emitter Rust source). Codegen ensures:
- Happy path: the call's actual return goes into the Ok variant's payload + the discriminant gets 0.
- Catch path: the helper's CxxRawError gets converted to `CxxException::from_raw(...)`, stored into the Err variant's payload + discriminant 1.

Pros: no MIR-level rewriting; works at the IR boundary only.
Cons: requires the codegen layer to know `Result<T, CxxException>`'s layout for the specific T — that's a layout query through `tcx.layout_of`.

### Recommended: Strategy B with bindings-emitter coordination

The bindings emitter (cxx_importer) already emits `fn open_file(path: &CStr) -> Result<*mut File, CxxException>` on the Rust side. The codegen layer trusts this — it computes the layout of `Result<T, CxxException>` for the return type and:
- Allocates a slot of that layout for the sret-style return.
- On the happy path: `call` returns `T`; codegen wraps into Ok variant.
- On the catch path: helper returns `CxxRawError`; codegen wraps into Err variant.

This keeps MIR / type-system unchanged.

## Files touched by P09.63

| File | What changes |
|---|---|
| `compiler/rustc_codegen_ssa/src/mir/block.rs` | The `do_call` change above. ~80 LoC. |
| `compiler/rustc_codegen_llvm/src/context.rs` (or similar) | Add a `get_external_fn(name, abi)` helper that declares the symbol on first reference per cgu. ~30 LoC. |
| `compiler/rustc_codegen_ssa/src/mir/place.rs` | Helper to construct Ok/Err variants of a known Result type into a memory slot. ~50 LoC. |
| Total | ~160 LoC. |

## Bootstrap iteration plan

1. Apply patches 01..23 (including P09.61 + P09.62) to a clean rust-lang/rust checkout.
2. Apply this patch (P09.63).
3. `./fork/build.sh` end-to-end. First bootstrap takes 2-3 hours on a fast machine.
4. Tests:
   - `tests/ui/cxx_throws/basic.rs` — `#[rustc_cxx_throws]` `extern "C++"` decl, throws, returns `Result::Err`. Skeleton needs writing alongside the patch.
   - The v1.12.21 `cxx_throws_demo` example crate, rebuilt against the new fork rustc — should produce identical user-visible behavior to today's Phase 0 path.

## Decision points

1. **Linkage of `__rustcc_cxx_catch_unknown`**: extern declaration on every CGU vs. langcall. Langcall is cleaner but requires modifying `rustc_lang_items`. Plain extern declaration is simpler — recommended.
2. **MSVC funclet path**: distinct from Itanium landingpad. The existing `catch_pad` / `catch_switch` trait methods cover the IR shape; P09.63 should branch per-platform inside `do_call`. MSVC implementation is P09.64.
3. **Personality fn**: Itanium uses `rust_eh_personality`. Catching foreign exceptions through this personality requires it to forward unmatched types (i.e., not match) — investigation needed during bootstrap.

## Status snapshot

- Plumbing (P09.61 + P09.62): **shipped**.
- Wiring + runtime helper call (P09.63): design only, implementation gated on bootstrap.
- MSVC funclet equivalent (P09.64): tracked separately.
- MIR-level Result wrapping: see Strategy A vs. B discussion; Strategy B recommended.
