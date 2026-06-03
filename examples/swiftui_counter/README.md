# swiftui_counter — SwiftUI ↔ Rust model

A SwiftUI app whose **model/logic lives in Rust**, called across the
rustcc fork's Swift `swiftcc` ABI. It's the *inverted* shape of
[`fltk_text_editor`](../fltk_text_editor): there Rust drives a C++ UI
from `main()`; here **Swift drives the UI** and Rust is the engine.

```text
  ┌─────────────────────────┐   swiftcc (extern "Swift")   ┌────────────┐
  │  SwiftUI app (Swift)    │  ──────────────────────────▶ │ Rust model │
  │  @main · View · @State  │   @_silgen_name("rc_*")       │ (this lib) │
  └─────────────────────────┘  ◀────────────────────────── └────────────┘
           UI layer                 scalar results              logic
```

## Why it's inverted (and not "Rust drives SwiftUI")

SwiftUI **cannot be driven from Rust** the way FLTK can. Its surface —
`@main`, the `View` protocol with `associatedtype Body` + `some View`
opaque returns, `@ViewBuilder` result builders, and the
`@State`/`@Observable` property wrappers — is built entirely from
Swift-compiler constructs that cross no ABI. There is no function to
call to "make a view". So the view tree stays in Swift; only the
model's **data + logic** cross the boundary.

This matches how real Rust+SwiftUI apps are structured: SwiftUI on top,
a Rust core underneath.

## What the rustcc fork contributes

The fork lets Rust **define** `extern "Swift"` functions — they use
Swift's `swiftcc` calling convention, so Swift calls them as ordinary
Swift functions (same mechanism the `swift_extern_call` probe
validates). Each model entry point carries a stable `#[export_name]`:

```rust
#[export_name = "rc_counter_bump"]
pub extern "Swift" fn rc_counter_bump(value: i64, step: i64, by: i64) -> i64 { … }
```

and Swift binds to it by symbol:

```swift
@_silgen_name("rc_counter_bump")
func rc_counter_bump(_ value: Int64, _ step: Int64, _ by: Int64) -> Int64
```

A Swift global func is `swiftcc` by default, so the ABI matches the
Rust `extern "Swift"` definition — no C shim in between.

> The counter API is scalar (`i64`) — the most robust shape across
> `swiftcc`. To pass **Swift-native value types** as whole records,
> use `#[repr(swift)]` / `#[swift_value]` (see [`docs/swift.md`](../../docs/swift.md));
> `CounterSnapshot` in `src/lib.rs` shows the Rust-side shape.

## Layout

```
swiftui_counter/
  Cargo.toml            # builds a staticlib (+ rlib for tests)
  src/lib.rs            # Rust model + the extern "Swift" API + tests
  swift/
    CounterApp.swift    # @main App
    ContentView.swift   # pure SwiftUI view (no FFI)
    RustCounter.swift   # @_silgen_name bridge + ObservableObject model
```

## Build & run

### 1. Rust model (validated here, no Swift toolchain needed)

```sh
cd examples/swiftui_counter
cargo +rustcc test            # exercises the model through the swiftcc API
cargo +rustcc build --release # -> target/release/libswiftui_counter.a
```

`cargo +rustcc test` drives the exact `extern "Swift"` entry points the
app links against and asserts the counter logic — so the **Rust half is
CI-validatable** even though the SwiftUI app isn't.

### 2. SwiftUI app (needs macOS + Xcode / swiftc)

The app target isn't built by `cargo` — SwiftUI needs `swiftc`/Xcode.
Add the three `swift/*.swift` files to an Xcode app (or a SwiftPM
executable) and link the static library:

- **Xcode:** add `swift/*.swift` to the app target; under *Build
  Phases → Link Binary With Libraries* (or *Other Linker Flags*) add
  `target/release/libswiftui_counter.a`. Build for macOS and run.
- **swiftc (quick check):**

  ```sh
  cargo +rustcc build --release
  swiftc -parse-as-library swift/*.swift \
         -L target/release -lswiftui_counter \
         -o counter_app          # macOS; add a deployment target as needed
  ./counter_app
  ```

You should see a counter whose value, step math, and clamping are all
computed in Rust; the `+`/`−`/Reset buttons call across the boundary.

## Caveats (honest)

- The Rust half is CI-validatable (`cargo test`). The SwiftUI app
  **builds + links** with `swiftc` on macOS (verified — the
  `@_silgen_name` decls resolve against the Rust `extern "Swift"`
  symbols in the staticlib); only *running* the GUI window is left to
  you, since it needs a windowed session.
- `@_silgen_name` is an underscored Swift attribute — the standard tool
  for binding to specific symbols, but unstable API.
- `swiftcc` symbol/ABI details can vary across Swift toolchain
  versions; if a symbol doesn't resolve, confirm the Rust export name
  (`nm target/release/libswiftui_counter.a | grep rc_counter`) matches
  the `@_silgen_name` string.
- You **cannot** subclass a Swift class from Rust or drive SwiftUI's
  view tree from Rust — see [`docs/swift.md`](../../docs/swift.md) §8.
