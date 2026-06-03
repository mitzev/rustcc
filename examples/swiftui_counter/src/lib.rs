//! Rust model for the SwiftUI counter demo, exposed to Swift through
//! the rustcc fork's `swiftcc` ABI.
//!
//! ## Architecture (inverted vs. the FLTK example)
//!
//! ```text
//!   ┌─────────────────────────┐   swiftcc (extern "Swift")   ┌────────────┐
//!   │  SwiftUI app (Swift)    │  ───────────────────────────▶ │ Rust model │
//!   │  @main, View, @State    │   @_silgen_name("rc_*")        │  (this lib)│
//!   └─────────────────────────┘  ◀─────────────────────────── └────────────┘
//!            UI layer                  scalar results               logic
//! ```
//!
//! SwiftUI itself can't be driven from Rust — `@main`, the `View`
//! protocol with `some View`, `@ViewBuilder` result builders, and the
//! `@State`/`@Observable` property wrappers are all Swift-compiler
//! constructs that cross no ABI. So the **view layer stays in Swift**
//! and only the model's data + logic crosses the boundary.
//!
//! ## What the fork provides
//!
//! The fork lets Rust *define* `extern "Swift"` functions: they use
//! Swift's `swiftcc` calling convention, so the Swift side can call
//! them as ordinary Swift functions (bound by symbol via
//! `@_silgen_name`). That's the same mechanism the `swift_extern_call`
//! probe validates; here we put a stable `#[export_name]` on each so
//! the Swift declarations have a fixed symbol to bind to.
//!
//! The counter API is intentionally scalar (`i64` in/out) — the most
//! robust shape across `swiftcc`. For passing *Swift-native value
//! types* across the boundary, use `#[repr(swift)]` / `#[swift_value]`
//! (see `docs/swift.md`); `CounterSnapshot` below shows the Rust-side
//! shape such a type would take.

#![feature(rustc_attrs)]
#![allow(internal_features)]

/// Pure-logic core. No ABI concerns — just the model rules. The
/// `extern "Swift"` shims below are thin wrappers over these so the
/// logic is unit-testable as plain Rust.
mod logic {
    pub fn bump(value: i64, step: i64, by: i64) -> i64 {
        value.saturating_add(by.saturating_mul(step))
    }
    pub fn clamp(value: i64, lo: i64, hi: i64) -> i64 {
        value.max(lo).min(hi)
    }
}

/// A snapshot of the model state. Shipped here as a plain Rust struct;
/// flip it to `#[repr(swift)]` (and declare a matching Swift `struct`)
/// to hand the whole record across the boundary as a Swift-native
/// value type instead of marshaling field-by-field.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub value: i64,
    pub step: i64,
}

// ---- The `swiftcc` API the SwiftUI app calls -------------------------
//
// Each is `extern "Swift"` (swiftcc) with a stable `#[export_name]`.
// The Swift side binds to these via `@_silgen_name("rc_…")`. They're
// also plain Rust fns, so the tests below call them directly.

/// `value + by * step`, saturating. The "+1 / -1 / +step" button logic.
#[export_name = "rc_counter_bump"]
pub extern "Swift" fn rc_counter_bump(value: i64, step: i64, by: i64) -> i64 {
    logic::bump(value, step, by)
}

/// Clamp the counter into `[lo, hi]` (e.g. a UI slider range).
#[export_name = "rc_counter_clamp"]
pub extern "Swift" fn rc_counter_clamp(value: i64, lo: i64, hi: i64) -> i64 {
    logic::clamp(value, lo, hi)
}

/// Reset to zero — returns the new value so the call site is uniform.
#[export_name = "rc_counter_reset"]
pub extern "Swift" fn rc_counter_reset() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the model the way the SwiftUI buttons would, through the
    // exact `extern "Swift"` entry points the app links against. This
    // validates the Rust half end-to-end without a Swift toolchain.
    #[test]
    fn counter_flow_through_swiftcc_api() {
        let step = 5;
        let mut v = rc_counter_reset();
        assert_eq!(v, 0);

        v = rc_counter_bump(v, step, 1); // "+5"
        assert_eq!(v, 5);
        v = rc_counter_bump(v, step, 3); // "+15"
        assert_eq!(v, 20);
        v = rc_counter_bump(v, step, -1); // "-5"
        assert_eq!(v, 15);

        v = rc_counter_clamp(v, 0, 10); // slider range [0,10]
        assert_eq!(v, 10);

        v = rc_counter_reset();
        assert_eq!(v, 0);
    }

    #[test]
    fn bump_saturates() {
        assert_eq!(rc_counter_bump(i64::MAX, 1, 10), i64::MAX);
    }

    #[test]
    fn snapshot_is_copy() {
        let a = CounterSnapshot { value: 3, step: 2 };
        let b = a; // Copy
        assert_eq!(a, b);
    }
}
