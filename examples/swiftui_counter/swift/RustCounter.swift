// Bridge from Swift to the Rust model.
//
// The Rust crate defines these as `extern "Swift"` functions (Swift's
// `swiftcc` calling convention) with stable `#[export_name]`s. We bind
// to those symbols here with `@_silgen_name` — no body means "this is
// an external symbol", and a Swift global func is `swiftcc` by default,
// so the ABI matches the Rust `extern "Swift"` definition.
//
// `@_silgen_name` is an underscored Swift attribute (the standard tool
// for low-level symbol binding). The symbols are resolved at link time
// from `libswiftui_counter.a` (see the README for the build).

import Foundation

@_silgen_name("rc_counter_bump")
func rc_counter_bump(_ value: Int64, _ step: Int64, _ by: Int64) -> Int64

@_silgen_name("rc_counter_clamp")
func rc_counter_clamp(_ value: Int64, _ lo: Int64, _ hi: Int64) -> Int64

@_silgen_name("rc_counter_reset")
func rc_counter_reset() -> Int64

/// Idiomatic SwiftUI model. SwiftUI owns the UI + the observable state;
/// every state transition is computed by the Rust model across the
/// `swiftcc` boundary. This is the only place the FFI is touched —
/// views stay pure SwiftUI.
final class CounterModel: ObservableObject {
    @Published private(set) var value: Int64 = 0
    let step: Int64
    let range: ClosedRange<Int64>

    init(step: Int64 = 1, range: ClosedRange<Int64> = 0...100) {
        self.step = step
        self.range = range
        self.value = rc_counter_reset()
    }

    func bump(by multiples: Int64) {
        let next = rc_counter_bump(value, step, multiples)
        value = rc_counter_clamp(next, range.lowerBound, range.upperBound)
    }

    func increment() { bump(by: 1) }
    func decrement() { bump(by: -1) }
    func reset() { value = rc_counter_reset() }
}
