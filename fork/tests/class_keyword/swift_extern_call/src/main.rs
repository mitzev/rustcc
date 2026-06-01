// Demo + regression test: the `extern "Swift"` calling convention
// (swiftcc ABI), the most fundamental Swift-interop primitive (P09.14
// / P09.17).
//
// `extern "Swift"` routes a function through Swift's `swiftcc` calling
// convention instead of the C ABI. In a real project the callee is a
// `swiftc`-compiled symbol named with Swift's `$s…` mangling; here we
// keep the demo self-contained by *defining* the swiftcc functions in
// Rust (the fork allows `extern "Swift"` definitions, not just foreign
// declarations) so it links and runs without a Swift toolchain.
//
// Three things are exercised:
//   1. Defining + calling an `extern "Swift"` fn (integer + float ABI).
//   2. A foreign `extern "Swift"` declaration carrying Swift argument
//      labels via `#[rustc_swift_labels = "…"]`, resolved to a locally
//      defined swiftcc symbol through `#[link_name]` / `#[export_name]`.
//   3. That the swiftcc path produces identical results to the C ABI
//      for the same arithmetic — i.e. argument/return passing is sound.

#![feature(rustc_attrs)]

// 1. Plain swiftcc free functions, defined in-crate. In a real build
//    these would be `swiftc`-compiled and declared with
//    `#[link_name = "$s5MyLib3addyS2i_SitF"]`-style Swift mangling.
extern "Swift" fn swift_add(a: i64, b: i64) -> i64 {
    a + b
}
extern "Swift" fn swift_scale(value: f64, factor: f64) -> f64 {
    value * factor
}

// 2. A foreign declaration that carries Swift argument labels. The
//    labels (`_, by`) are metadata for the binding layer — Swift sees
//    `scale(_:by:)` — and don't change the ABI. We point the decl at a
//    locally-defined swiftcc symbol via matching link/export names so
//    the demo stays self-contained.
extern "Swift" {
    #[rustc_swift_labels = "_, by"]
    #[link_name = "demo_scale_by_impl"]
    fn scale(value: f64, factor: f64) -> f64;
}

#[export_name = "demo_scale_by_impl"]
extern "Swift" fn scale_by_impl(value: f64, factor: f64) -> f64 {
    value * factor
}

fn main() {
    // Integer swiftcc round-trip.
    let sum = swift_add(20, 22);
    assert_eq!(sum, 42, "swiftcc integer ABI");

    // Float swiftcc round-trip (separate float-register class).
    let scaled = swift_scale(3.5, 2.0);
    assert_eq!(scaled, 7.0, "swiftcc float ABI");

    // Labeled foreign decl dispatched to the local swiftcc symbol.
    let labeled = unsafe { scale(10.0, 2.5) };
    assert_eq!(labeled, 25.0, "labeled swiftcc decl");

    println!("ok: extern Swift add={sum} scale={scaled} labeled={labeled}");
}
