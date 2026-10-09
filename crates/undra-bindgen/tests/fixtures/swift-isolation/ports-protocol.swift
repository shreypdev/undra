// The protocol form of the `ports` golden, as an app writes it: a plain class per port (ADR-066).
// Compiled as a target of the typecheck package as is, and under default main-actor isolation
// (`-default-isolation MainActor`, Xcode 26's default for a new project): in both, a class that
// conforms to a generated port protocol is nonisolated, so it keeps its state in `let`s or behind
// a lock and touches no main-actor state in a synchronous method. The golden declares its own
// `HttpRequest`, `HttpError` and `FsError` beside the runtime's, hence the qualified names.
import Foundation
import GoldenPorts
import UndraRuntime

/// The sync port: stateless.
final class AppClock: Clock {
    func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }
    func monotonicNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
    func log(level: UInt8, target: String, message: String) {}
}

/// The async port with a typed error.
final class AppHttp: Http {
    func request(_ req: GoldenPorts.HttpRequest) async throws(GoldenPorts.HttpError) -> GoldenPorts.HttpResponse {
        throw GoldenPorts.HttpError.network("offline")
    }
}

/// The async port with state, and a synchronous method with a typed error. The state is behind a
/// lock, the one way a nonisolated `Sendable` class may keep a `var`.
final class AppKv: Kv {
    private let lock = NSLock()
    private nonisolated(unsafe) var store: [String: [UInt8]] = [:]

    func get(key: String) async -> [UInt8]? { lock.withLock { store[key] } }
    func set(key: String, value: [UInt8]) async { lock.withLock { store[key] = value } }
    func list(prefix: String) async -> [String] {
        lock.withLock { store.keys.filter { $0.hasPrefix(prefix) }.sorted() }
    }
    func flush() throws(GoldenPorts.FsError) {}
}

/// Registers the three on a loaded core.
func registerConformers(on core: UndraCore) {
    core.registerPort(UndraIds.Ports.Clock.portId, clockPortImpl(AppClock()))
    core.registerPort(UndraIds.Ports.Http.portId, httpPortImpl(AppHttp()))
    core.registerPort(UndraIds.Ports.Kv.portId, kvPortImpl(AppKv()))
}
