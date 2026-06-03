// SwiftUI app entry point. `@main` + the `App` protocol are Swift-only
// constructs — this is why the UI layer can't be driven from Rust and
// the example is "inverted" (Swift on top, Rust as the model).

import SwiftUI

@main
struct CounterApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
