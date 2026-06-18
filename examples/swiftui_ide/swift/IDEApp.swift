// SwiftUI entry point. `@main` + the `App`/`Scene`/`WindowGroup`
// surface is pure Swift-compiler machinery (it crosses no ABI), which
// is exactly why the UI must live in Swift while the IDE engine lives
// in fork-Rust on the other side of `extern "Swift"`.

import SwiftUI

@main
struct RustccIDEApp: App {
    var body: some Scene {
        WindowGroup("rustcc IDE (SwiftUI)") {
            ContentView()
                .frame(minWidth: 900, minHeight: 600)
        }
        .windowStyle(.titleBar)
    }
}
