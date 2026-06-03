// Pure SwiftUI view layer. Note there is no FFI here — the view only
// talks to `CounterModel`, which is the single seam to the Rust model.
// This is the part that *cannot* live in Rust: `some View`, the
// `@ViewBuilder` body, and `@StateObject` are Swift-compiler features.

import SwiftUI

struct ContentView: View {
    @StateObject private var model = CounterModel(step: 5, range: 0...100)

    var body: some View {
        VStack(spacing: 24) {
            Text("rustcc · SwiftUI ↔ Rust")
                .font(.headline)
                .foregroundStyle(.secondary)

            Text("\(model.value)")
                .font(.system(size: 72, weight: .bold, design: .rounded))
                .monospacedDigit()
                .contentTransition(.numericText())

            Text("step \(model.step) · range \(model.range.lowerBound)…\(model.range.upperBound)")
                .font(.caption)
                .foregroundStyle(.tertiary)

            HStack(spacing: 16) {
                Button {
                    withAnimation { model.decrement() }
                } label: {
                    Image(systemName: "minus.circle.fill").font(.largeTitle)
                }

                Button("Reset") {
                    withAnimation { model.reset() }
                }
                .buttonStyle(.bordered)

                Button {
                    withAnimation { model.increment() }
                } label: {
                    Image(systemName: "plus.circle.fill").font(.largeTitle)
                }
            }

            Text("every value above is computed by Rust")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .padding(40)
        .frame(minWidth: 320, minHeight: 360)
    }
}

// (Add `#Preview { ContentView() }` when editing in Xcode — it needs
// Xcode's preview macro plugin, so it's omitted here to keep a bare
// `swiftc` build working.)
