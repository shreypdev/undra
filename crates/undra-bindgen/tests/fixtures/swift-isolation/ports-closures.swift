// The closure form of the `ports` golden (ADR-066): one closure per method, nothing conforms, and the
// async closures reach state that lives on the main actor with `await` and `MainActor.run`. Compiled as is and
// under default main-actor isolation, where a closure literal passed to a `@Sendable` parameter is
// nonisolated whatever the file's default is.
import Foundation
import GoldenPorts
import UndraRuntime

/// The app's main-actor state: what its screens read too.
@MainActor final class Session {
    static let shared = Session()
    var cache: [String: [UInt8]] = [:]
    var online = true
}

/// The three ports from closures, as adapters the core is loaded with.
func closureAdapters() -> Adapters {
    let clock = clockPortImpl(
        nowMs: { Int64(Date().timeIntervalSince1970 * 1000) },
        monotonicNs: { DispatchTime.now().uptimeNanoseconds },
        log: { level, target, message in _ = (level, target, message) }
    )
    // A closure that throws names its error type: Swift infers `any Error` for it otherwise.
    let http = httpPortImpl(request: { req async throws(GoldenPorts.HttpError) in
        if await Session.shared.online {
            throw GoldenPorts.HttpError.timeout
        }
        throw GoldenPorts.HttpError.network("offline: \(req.url)")
    })
    let kv = kvPortImpl(
        get: { key in await MainActor.run { Session.shared.cache[key] } },
        set: { key, value in await MainActor.run { Session.shared.cache[key] = value } },
        list: { prefix in
            await MainActor.run { Session.shared.cache.keys.filter { $0.hasPrefix(prefix) }.sorted() }
        },
        flush: {}
    )
    return Adapters([
        PortImplAdapter(portId: UndraIds.Ports.Clock.portId, impl: clock),
        PortImplAdapter(portId: UndraIds.Ports.Http.portId, impl: http),
        PortImplAdapter(portId: UndraIds.Ports.Kv.portId, impl: kv),
    ])
}
