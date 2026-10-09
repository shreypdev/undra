import Foundation
import OSLog
import UndraRuntime
import PlaygroundCore

/// Attaches the app to its Rust core, once, before any store is created.
enum UndraBootstrap {
    /// The server the remote screen talks to. The address is made up: the app answers the `Http`
    /// port itself (`PlaygroundNetwork`), so nothing leaves the device.
    static let serverURL = "https://playground.undra.test"

    /// The server list the remote screen shows.
    static let inboxList = "inbox"

    /// Where failures that nobody could catch end up: a command (`todos.toggle(id:)`) that the core
    /// refused, a change a store could not apply. Undra has already logged each one; this puts it
    /// in the app's own log too. The handler runs on the thread that made the call and must not
    /// call back into Undra. In a debug build you could stop at the failing line instead:
    /// `{ assertionFailure("\($0)") }`.
    private static let onError: @Sendable (UndraUnhandledError) -> Void = { unhandled in
        Logger(subsystem: "dev.undra.playground", category: "undra")
            .error("\(unhandled.description, privacy: .public)")
    }

    /// Where the core's panic reports end up (ADR-046): the app's own log, as an app would hand them to its crash
    /// reporter (Crashlytics, Sentry), and `PanicLog` for the Debug section of the Remote tab. The runtime calls this
    /// on the main thread, once per panic, after the core has answered the call that panicked.
    private static let onPanic: @Sendable (UndraPanicReport) -> Void = { report in
        Logger(subsystem: "dev.undra.playground", category: "undra").error(
            "the core panicked in \(report.operation, privacy: .public): \(report.message, privacy: .public) at \(report.location, privacy: .public) (\(report.frames.count) frames, image \(report.imageId, privacy: .public))"
        )
        Task { @MainActor in PanicLog.shared.record(report) }
    }

    /// Asks the OS for background windows (ADR-046): offline mutations that are still queued replay, and the cached list
    /// refetches, while the app is not on screen. The identifiers, `dev.undra.playground.undra.processing` and
    /// `.refresh`, are in `Config/Info.plist`. Called from the app's `init`, before it finishes launching, and idempotent.
    /// Try it on a device (the simulator has no background scheduler): background the app with an offline item queued, pause it in the debugger and evaluate
    /// `e -l objc -- (void)[[BGTaskScheduler sharedScheduler] _simulateLaunchForTaskWithIdentifier:@"dev.undra.playground.undra.refresh"]`.
    private static func registerBackground() {
        UndraBackground.register(taskIdentifier: "dev.undra.playground.undra") {
            try UndraPlaygroundCore.load()
        }
    }

    /// Loads the core linked into the app (`undra build --platform ios`) with the default adapters,
    /// except that `Http` is the in-memory server, `Connectivity` is the one the Offline switch
    /// drives (the app's network is simulated, so its connectivity is too) and `Kv` is emptied at launch. In debug builds, when
    /// `UNDRA_DEV_URL` is set (for example `ws://192.168.1.20:7443`), attaches to the core that
    /// `undra dev` serves instead: edit the Rust, save, and the app is on the rebuilt core within a second, with its state
    /// and no rebuild of the app (ADR-053).
    /// The dev server this process uses (`UNDRA_DEV_URL`, debug builds), or `nil` for the in-process core.
    @MainActor static var devURL: String?

    /// The core `start()` loaded, so a view can show what its connection is doing (`core.connection`).
    @MainActor static var core: UndraCore?

    /// Called when `undra dev` restarted the core and could not carry its state over (a schema change, a state over the
    /// limit), so the objects of this app's core are gone: the app loads the new core and starts over on it
    /// (`PlaygroundApp.reload`).
    @MainActor static var coreLost: (() -> Void)?

    /// Called once the runtime is connected again to a core that `undra dev` reloaded with its state kept, after the server's
    /// address has been told to it again: the screens fetch what they tried before it knew (`PlaygroundApp`).
    @MainActor static var coreReconnected: (() -> Void)?

    /// Whether the connection dropped and has not come back yet.
    @MainActor private static var reconnecting = false

    /// What the connection did. A core that `undra dev` reloaded keeps its stores and its query handles (ADR-059), but
    /// not what it holds outside them: the server's address is set by a call, so it is told again when the runtime is back
    /// (the pattern of a web core that restarted after a crash, ADR-049).
    @MainActor private static func connectionChanged(_ state: UndraConnectionState) {
        switch state {
        case .closed(let reason):
            reconnecting = false
            if reason == .sessionLost { coreLost?() }
        case .reconnecting:
            reconnecting = true
        case .connected where reconnecting:
            reconnecting = false
            configureRemote(RemoteConfig(baseUrl: serverURL))
            coreReconnected?()
        default:
            break
        }
    }

    @MainActor
    static func start() throws {
        registerBackground()
        // The server the app carries forgets everything when the app quits, so the core's cache of
        // it must too: the key-value store (the query cache, the offline queue) lives in a temporary
        // directory that every launch starts empty.
        let store = FileManager.default.temporaryDirectory.appendingPathComponent("playground-kv", isDirectory: true)
        try? FileManager.default.removeItem(at: store)
        let adapters = Adapters.platformDefault
            .removing(portId: fnv1a32("port.Connectivity"))
            .replacing(PlaygroundNetwork.shared)
            .replacing(KvAdapter(directory: store))
            // The app's own port (ADR-049 decision 2) from closures (ADR-066): nothing conforms, and the closure
            // runs on the core's thread. `Foundation.Locale`: the generated `Locale` protocol shares the name.
            .replacing(PortImplAdapter(portId: UndraIds.Ports.Locale.portId, impl: localePortImpl(hello: {
                Foundation.Locale.current.language.languageCode?.identifier == "fr" ? "Bonjour" : "Hello"
            })))
        #if DEBUG
        if let url = ProcessInfo.processInfo.environment["UNDRA_DEV_URL"], !url.isEmpty {
            devURL = url
            core = try UndraPlaygroundCore.load(.remote(
                url: url,
                adapters: adapters,
                onError: onError,
                // The runtime reconnects by itself; when it finds a new core instead of its own, it says so.
                onConnectionChange: { state in
                    Task { @MainActor in connectionChanged(state) }
                },
                // What the dev server says about a reload ("Reloaded, state kept"), for the status bar.
                onDevNotice: { message in
                    Task { @MainActor in DevNotice.shared.show(message) }
                },
                onPanic: onPanic
            ))
            configureRemote(RemoteConfig(baseUrl: serverURL))
            return
        }
        #endif
        core = try UndraPlaygroundCore.load(.inproc(adapters: adapters, onError: onError, onPanic: onPanic))
        // Tell the core where the server is, before anything observes the remote list.
        configureRemote(RemoteConfig(baseUrl: serverURL))
    }
}
