# v1.14 design: subclassing multiple-inheritance C++ bases

Target driver: wxWidgets (`wxEvtHandler : wxObject, wxTrackable` sits
under every widget; `wxTextCtrl : wxControl, wxTextEntry`). Scope:
**non-virtual MI, Itanium first**; virtual bases (diamonds) and MSVC
follow separately.

## Current state (v1.13.10)

- Model (`rustc_abi_cxx`): already builds secondary sub-tables
  (`vtable_itanium`), `layout.base_offsets`, VTT (`vtt.rs`), and the
  `_ZThn<n>_` thunk mangling. Gap: `primary_vtable_slots` /
  `collect_chain` walk only the primary chain.
- Importer: as of v1.13.10 it **guards** MI/virtual-base chains (no
  attr → fork rejects `override fn`) instead of silently linearizing
  the first base.
- Fork: single-`__base` layout, one vptr, one vtable, `__si` RTTI.

## Design

### 1. Attr format (v4) — per-subobject groups

```
ztv=…;zti=…;kind=vmi;
base=<Name>@<off>[,primary];vdtor=1?;slot=…   ; repeated per base
```

Each `base=` opens a group: the subobject's class name, its byte
offset in the most-derived imported base, and its flattened slot list
(same `slot=name,sym,psig` records as today, incl. `~dtor`/`~op`).
Group 0 is the primary (offset 0). Legacy v3 attrs remain valid
(single implicit group).

### 2. Layout (fork `cxx_bridge.rs` + parser)

`class D : A, B` → fields `__base: A` at 0 (primary, shares vptr) and
`__base2: B` at `offset(B)` from the attr/layout. Each polymorphic
non-primary base keeps its own vptr inside its subobject. Parser
accepts a base list; only the first gets Deref (M22-style accessors
for the rest, mirroring the importer's `as_<base>_mut`).

### 3. Vtable group emission (fork `cxx_vtable.rs`)

One `_ZTV<D>` global containing N sub-tables:
- primary: `[0, &_ZTI<D>, slots…]`
- per non-primary base at offset k: `[-k, &_ZTI<D>, slots…]`
Address points recorded per subobject; ctor installs **N vptrs** at
return (generalize `maybe_emit_vptr_init` over `(offset,
address_point)` pairs — the existing every-return-terminator store
already handles the clobber problem).

### 4. This-adjusting thunks

A Rust override reachable through non-primary base B at offset k gets
a thunk `_ZThn<k>_<method-symbol>`: `this -= k; musttail call real`.
Mechanically the deleting-dtor-thunk pattern (GEP + tail call). Dtor
slots in secondary tables point at thunked D1/D0. Pure-virtual slots
stay `__cxa_pure_virtual` un-thunked.

### 5. RTTI

`__vmi_class_type_info` (new emission next to the existing `__si`):
flags, base_count, and per-base `{zti, offset<<8|flags}` words.
Required for `dynamic_cast` across bases — pin against clang output.

### 6. Override matching / check_attr

Search slots across **all** groups (name+psig as today). A name+sig
matching slots in two groups (same signature inherited from two
bases) is a C++ ambiguity → reject with a diagnostic.

### 7. Member function pointers (independent track)

- Mangling `M<class>F…E` (clone of the v1.13.10 `PF…E` fix).
- `#[repr(C)] CxxMemberFnPtr { ptr_or_voff: usize, adj: isize }` in
  `crates/cxx`; importer renders param types as it.
- Constructors: Rust `extern "C++"` methods → `{addr, 0}`; C++
  targets via tiny `&Class::method` shims. Invoking-through deferred.

## Phases & effort

| Phase | Deliverable | Est. |
|---|---|---|
| 0 ✅ | Importer MI guard (shipped v1.13.10 branch) | done |
| 1 | Member fn pointers end to end + probe | ~1 wk |
| 2 | Model: full base-graph slots + corpus vs clang | 3–5 d |
| 3 | Attr v4 + importer emission | 2–3 d |
| 4 | Fork: layout, vtable group, N-vptr init, thunks, `__vmi` | 1.5–2.5 wk |
| 5 | E2e: `examples/subclass_mi` (hand-written A,B bases) then wx probe | 2–3 d |
| 6 | MSVC parity (adjustor thunks, per-base vftables) | ~1 wk |

Out of scope: virtual bases, `Bind<>` templates (use `Connect`),
covariant-return thunks across bases.

## Risks

Thunk ABI (musttail vs regular call+ret), `__vmi` flag correctness,
ambiguous-member diagnostics, construct-then-move with N vptrs
(covered: return-terminator installs + `new_at`), wxString's inline
API (needs the shim-based inline-method binding — separate track).
