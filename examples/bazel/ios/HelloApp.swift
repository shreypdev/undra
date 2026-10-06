// A minimal SwiftUI app over the generated bindings (ADR-061): it loads the iOS core on launch and calls one core function,
// `greeting`, so a breakpoint on that Rust function is hit while the app starts. Built by `//ios:app`.
import HelloCore
import SwiftUI
import UndraRuntime

@main
struct HelloApp: App {
    @State private var message = "Loading the core"

    var body: some Scene {
        WindowGroup {
            Text(message)
                .padding()
                .task { message = start() }
        }
    }

    /// Loads the core linked into the app and calls it once: the Swift frame above the Rust one.
    private func start() -> String {
        do {
            _ = try Self.core.get()
            return try greeting(name: "Xcode")
        } catch {
            return "The core did not start: \(error)"
        }
    }

    /// The core, loaded the first time it is asked for. A core loads once per process, and SwiftUI runs `.task` again for a
    /// scene it makes again.
    private static let core = Result { try UndraHelloCore.load(.inproc()) }
}
