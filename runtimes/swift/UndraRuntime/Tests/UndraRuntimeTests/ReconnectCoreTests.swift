import Foundation
import Combine
import Observation
import XCTest
@testable import UndraRuntime

/// Collects the connection states a core reports, from whichever thread reports them.
final class StateLog: @unchecked Sendable {
    private let states = Guarded<[UndraConnectionState]>([])

    var all: [UndraConnectionState] {
        return states.withLock { (list: inout [UndraConnectionState]) -> [UndraConnectionState] in
            return list
        }
    }

    func append(_ state: UndraConnectionState) {
        states.withLock { (list: inout [UndraConnectionState]) -> Void in
            list.append(state)
        }
    }

    /// `connecting`, `connected`, `reconnecting 2`, `closed`: the states in short.
    var names: [String] {
        return all.map { state in
            switch state {
            case .connecting:
                return "connecting"
            case .connected:
                return "connected"
            case .reconnecting(let attempt):
                return "reconnecting \(attempt)"
            case .closed(let reason):
                switch reason {
                case .requested:
                    return "closed:requested"
                case .schemaMismatch:
                    return "closed:schemaMismatch"
                case .sessionLost:
                    return "closed:sessionLost"
                case .failed:
                    return "closed:failed"
                }
            }
        }
    }
}

/// Collects what a core hands to `LoadOptions.onError`, from whichever thread reports it.
private final class ReportedErrors: @unchecked Sendable {
    private let items = Guarded<[UndraUnhandledError]>([])

    var all: [UndraUnhandledError] {
        return items.withLock { (list: inout [UndraUnhandledError]) -> [UndraUnhandledError] in
            return list
        }
    }

    func append(_ item: UndraUnhandledError) {
        items.withLock { (list: inout [UndraUnhandledError]) -> Void in
            list.append(item)
        }
    }
}

/// A remote-mode core over a fake transport that can be dropped and brought back.
@MainActor
private func makeRemoteCore(
    _ transport: FakeTransport,
    log: StateLog,
    onError: (@Sendable (UndraUnhandledError) -> Void)? = nil
) throws -> UndraCore {
    let options = LoadOptions.remote(
        url: "ws://fake",
        adapters: Adapters.none,
        expectedSchemaHash: 0x1234,
        onError: onError,
        onConnectionChange: { log.append($0) }
    )
    return try UndraCore.connect(transport: transport, options: options)
}

/// What a transport that reconnects makes of the core: state, what fails, what is observed again (ADR-051).
@MainActor
final class ReconnectCoreTests: XCTestCase {
    private let store = UndraHandle(index: 2, generation: 1)
    private let other = UndraHandle(index: 3, generation: 1)

    private func value(_ number: UInt32) -> [UInt8] {
        return number.undraEncoded()
    }

    func testACoreReportsConnectingThenConnectedAndADropIsReconnectingUntilTheWayBack() async throws {
        let log = StateLog()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: log)
        XCTAssertEqual(log.names, ["connecting", "connected"])
        XCTAssertEqual(core.connectionState, .connected)

        transport.drop()
        XCTAssertEqual(core.connectionState, .reconnecting(attempt: 1))
        transport.retry(attempt: 2)
        transport.retry(attempt: 3)
        transport.reconnect()
        let back = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(back)
        XCTAssertEqual(log.names, ["connecting", "connected", "reconnecting 1", "reconnecting 2", "reconnecting 3", "connected"])
    }

    func testADropFailsWhatIsInFlightAtOnceWithTheTypedOutcomeAndTheCoreStaysOpen() async throws {
        let log = StateLog()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: log)

        let suspended = Task { @MainActor () -> (any Error)? in
            do {
                _ = try await core.call(.freeFunction(methodId: 3), method: 3, args: [])
                return nil
            } catch {
                return error
            }
        }
        let blocked = Guarded<(any Error)?>(nil)
        let blockedDone = expectation(description: "the blocking call ended")
        DispatchQueue.global().async {
            do {
                _ = try core.callSync(.freeFunction(methodId: 4), method: 4, args: [])
            } catch {
                blocked.withLock { (value: inout (any Error)?) -> Void in
                    value = error
                }
            }
            blockedDone.fulfill()
        }
        let sent = await waitUntil { transport.calls.count == 2 }
        XCTAssertTrue(sent)

        transport.drop(UndraTransportError.connectionLost(reason: "socket reset"))
        let suspendedError = await suspended.value
        await fulfillment(of: [blockedDone], timeout: hangDeadline)
        for error in [suspendedError, blocked.withLock { (value: inout (any Error)?) -> (any Error)? in return value }] {
            guard case UndraTransportError.connectionLost(let reason)? = error as? UndraTransportError else {
                return XCTFail("expected the typed outcome, got \(String(describing: error))")
            }
            XCTAssertTrue(reason.contains("socket reset") && reason.contains("reconnecting"), reason)
            // What a generated method makes of it:
            guard case UndraCallError.unavailable(.connectionLost)? = UndraCallError.mapped(error!) as? UndraCallError else {
                return XCTFail("generated methods throw .unavailable")
            }
        }
        XCTAssertEqual(core.stats().hostPendingCalls, 0)
        XCTAssertFalse(core.isShutDown)
    }

    func testCallsMadeWhileReconnectingFailAtOnceWithTheSameOutcome() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        transport.drop()
        XCTAssertThrowsError(try core.callSync(.freeFunction(methodId: 3), method: 3, args: [])) { error in
            guard case UndraTransportError.connectionLost(let reason)? = error as? UndraTransportError else {
                return XCTFail("\(error)")
            }
            XCTAssertTrue(reason.contains("reconnecting"), reason)
        }
        do {
            _ = try await core.call(.freeFunction(methodId: 3), method: 3, args: [])
            XCTFail("a call while reconnecting must fail")
        } catch {
            XCTAssertTrue(error is UndraTransportError, "\(error)")
        }
        XCTAssertThrowsError(try core.construct(type: 1, method: 2, args: []))
        XCTAssertEqual(transport.calls.count, 0, "nothing reached the transport")
        XCTAssertEqual(core.stats().hostPendingCalls, 0)
    }

    func testACommandThatFailsBecauseTheConnectionIsDownIsNotHandedToOnErrorButAnotherFailureIs() async throws {
        let log = StateLog()
        let reports = ReportedErrors()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: log, onError: { reports.append($0) })

        // Connected: a failure that is not the connection reaches the handler.
        core.report(UndraReplyError(status: .cancelled, body: []), operation: "Todos.toggle")
        XCTAssertEqual(reports.all.map { $0.operation }, ["Todos.toggle"])

        // Reconnecting: the connection state already says so, so a command tapped meanwhile is only logged.
        transport.drop()
        do {
            _ = try core.callSync(.freeFunction(methodId: 3), method: 3, args: [])
            XCTFail("a call while reconnecting must fail")
        } catch {
            core.report(error, operation: "Todos.toggle")
        }
        XCTAssertEqual(reports.all.count, 1, "not reported while reconnecting")

        // The way back: reported again.
        transport.reconnect()
        let back = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(back)
        core.report(UndraReplyError(status: .cancelled, body: []), operation: "Todos.toggle")
        XCTAssertEqual(reports.all.count, 2)

        // Lost for good (the dev server restarted): still the connection's news, though the calls after it say `.closed`.
        transport.drop()
        transport.disconnect(UndraSessionLostError(reason: "session lost: no session"))
        XCTAssertEqual(core.connectionState, .closed(.sessionLost))
        core.report(UndraTransportError.closed, operation: "Todos.toggle")
        core.report(UndraSessionLostError(reason: "x"), operation: "Todos.toggle")
        XCTAssertEqual(reports.all.count, 2, "not reported after the session was lost")
    }

    func testACallOnACoreTheAppShutDownIsAProgrammingErrorAndIsReported() throws {
        let reports = ReportedErrors()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog(), onError: { reports.append($0) })
        core.shutdown()
        XCTAssertEqual(core.connectionState, .closed(.requested))
        core.report(UndraTransportError.closed, operation: "Todos.toggle")
        XCTAssertEqual(reports.all, [UndraUnhandledError(operation: "Todos.toggle", error: .unavailable(.closed))])
        // A timeout is not the connection state's news either.
        core.report(UndraTransportError.timedOut(operation: "callSync"), operation: "Todos.toggle")
        XCTAssertEqual(reports.all.count, 2)
    }

    func testAfterTheWayBackCallsWorkAgain() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        transport.drop()
        transport.reconnect()
        let back = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(back)
        transport.onCall = { call, fake in
            fake.replyOk(call.callId, [7])
            return true
        }
        let body = try await core.call(.freeFunction(methodId: 3), method: 3, args: [])
        XCTAssertEqual(body, [7])
    }

    func testEveryStoreTheAppObservesIsObservedAgainAndTheMirrorConvergesOnTheAnswer() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        let current = Guarded<UInt32>(5)
        transport.onObserve = { handle, signal, on, fake in
            if on {
                let number = current.withLock { (value: inout UInt32) -> UInt32 in return value }
                fake.deliverChangeSet(makeChangeSet([(handle, 0, number.undraEncoded())]))
            }
        }
        let log = EventLog()
        core.mirror.register(store) { signal, op, reader in
            log.events.append("\((try? reader.readU32()) ?? 0)")
        }
        core.mirror.register(other) { _, _, _ in }
        core.observe(store, signal: Observe.allSignals, on: true)
        core.observe(other, signal: 0, on: true)
        core.observe(other, signal: 1, on: true)
        XCTAssertEqual(log.events, ["5"])

        transport.forgetSent()
        current.withLock { (value: inout UInt32) -> Void in value = 50 } // changed while the client was away
        transport.drop()
        transport.reconnect()
        let converged = await waitUntil { log.events == ["5", "50"] }
        XCTAssertTrue(converged, "\(log.events)")
        let observed = Set(transport.sent.map { "\($0)" })
        let expected: [FakeTransport.Sent] = [
            .observe(store.rawValue, UInt32.max, true), .observe(other.rawValue, 0, true), .observe(other.rawValue, 1, true),
        ]
        XCTAssertEqual(observed, Set(expected.map { "\($0)" }))
        XCTAssertEqual(transport.sent.count, 3)
        let connected = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(connected)
    }

    func testWhatTheAppStoppedObservingOrReleasedIsNotObservedAgain() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        let third = UndraHandle(index: 4, generation: 1)
        core.observe(store, signal: Observe.allSignals, on: true)
        core.observe(other, signal: 0, on: true)
        core.observe(other, signal: 0, on: false)
        core.observe(third, signal: Observe.allSignals, on: true)
        core.release(third)
        transport.forgetSent()
        transport.drop()
        transport.reconnect()
        let connected = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(connected)
        XCTAssertEqual(transport.sent, [.observe(store.rawValue, UInt32.max, true)], "and nothing is released twice")
    }

    func testAnObjectReleasedWhileTheConnectionIsDownIsReleasedAtTheServerOnceItIsBack() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        core.observe(store, signal: Observe.allSignals, on: true)
        transport.drop()
        core.release(store)
        transport.forgetSent()
        transport.reconnect()
        let connected = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(connected)
        XCTAssertEqual(transport.sent, [.release(store.rawValue)], "released, and not observed again")
    }

    func testTheCoreAsksTheServerToResumeOnlyWhenItHoldsObjectsItsConstructorsMade() throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        XCTAssertFalse(core.holdsObjects())
        transport.onCall = { call, fake in
            fake.replyOk(call.callId, UndraHandle(index: 9, generation: 1).undraEncoded())
            return true
        }
        let handle = try core.construct(type: 5, method: 6, args: [])
        XCTAssertTrue(core.holdsObjects())
        core.release(handle)
        XCTAssertFalse(core.holdsObjects())
    }

    func testASchemaMismatchIsFinalClosedWithItsReasonOnceAndEveryCallFails() async throws {
        let log = StateLog()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: log)
        transport.drop()
        transport.disconnect(UndraSchemaMismatchError(expected: 0x1234, got: 0x77))
        XCTAssertEqual(core.connectionState, .closed(.schemaMismatch(expected: 0x1234, got: 0x77)))
        XCTAssertTrue(core.isShutDown)
        XCTAssertTrue(transport.wasShutDown)
        XCTAssertEqual(log.names, ["connecting", "connected", "reconnecting 1", "closed:schemaMismatch"])
        XCTAssertThrowsError(try core.callSync(.freeFunction(methodId: 3), method: 3, args: []))
        // A queued notice from before the end cannot reopen it.
        transport.retry(attempt: 2)
        XCTAssertEqual(log.names.count, 4)
    }

    func testALostSessionIsFinalWithItsOwnReasonAndCallsOnTheCoreAreUnavailable() async throws {
        let log = StateLog()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: log)
        transport.drop()
        transport.disconnect(UndraSessionLostError(reason: "session lost: no session"))
        XCTAssertEqual(core.connectionState, .closed(.sessionLost))
        XCTAssertEqual(log.names.last, "closed:sessionLost")
        let mapped = UndraCallError.mapped(UndraSessionLostError(reason: "x"))
        guard case UndraCallError.unavailable(.connectionLost(let reason))? = mapped as? UndraCallError else {
            return XCTFail("\(mapped)")
        }
        XCTAssertTrue(reason.contains("load a new core"), reason)
    }

    func testATransportThatGivesUpForAnotherReasonClosesTheCoreAsFailedAndShutdownIsRequested() throws {
        let failing = StateLog()
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: failing)
        transport.disconnect(UndraTransportError.connectionLost(reason: "boom"))
        guard case .closed(.failed(let why)) = core.connectionState else {
            return XCTFail("\(core.connectionState)")
        }
        XCTAssertTrue(why.contains("boom"), why)

        let requested = StateLog()
        let second = try makeRemoteCore(FakeTransport(directSync: false), log: requested)
        second.shutdown()
        second.shutdown()
        XCTAssertEqual(requested.names, ["connecting", "connected", "closed:requested"], "shutting down twice is not a second change")
    }

    func testALossThatHappensWhileTheStoresAreBeingObservedAgainDoesNotAnnounceAConnection() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        core.observe(store, signal: Observe.allSignals, on: true)
        let replaying = DispatchSemaphore(value: 0)
        let fired = Guarded<Bool>(false)
        // The connection drops again in the middle of the replay: the second loss is the newer news.
        transport.onObserve = { _, _, on, fake in
            let first = fired.withLock { (done: inout Bool) -> Bool in
                if done {
                    return false
                }
                done = true
                return true
            }
            if on && first {
                fake.drop()
                replaying.signal()
            }
        }
        transport.drop()
        transport.reconnect()
        XCTAssertEqual(replaying.wait(timeout: .now() + hangDeadline), .success)
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(core.connectionState, .reconnecting(attempt: 1), "still reconnecting")
        transport.onObserve = nil
        transport.reconnect()
        let connected = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(connected)
    }

    func testTheStateStreamStartsWithTheCurrentStateFollowsEveryChangeAndEndsAfterClosed() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        let seen = Guarded<[UndraConnectionState]>([])
        let finished = expectation(description: "the stream ended")
        let stream = core.connectionStates()
        let reader = Task {
            for await state in stream {
                seen.withLock { (list: inout [UndraConnectionState]) -> Void in
                    list.append(state)
                }
            }
            finished.fulfill()
        }
        let first = await waitUntil { seen.withLock { (list: inout [UndraConnectionState]) -> Int in return list.count } == 1 }
        XCTAssertTrue(first)
        transport.drop()
        transport.reconnect()
        let back = await waitUntil { core.connectionState == .connected }
        XCTAssertTrue(back)
        core.shutdown()
        await fulfillment(of: [finished], timeout: hangDeadline)
        reader.cancel()
        let states = seen.withLock { (list: inout [UndraConnectionState]) -> [UndraConnectionState] in return list }
        XCTAssertEqual(states, [.connected, .reconnecting(attempt: 1), .connected, .closed(.requested)])
    }

    func testTheObservableConnectionTrailsTheStateOnTheMainActor() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        let connected = await waitUntil { core.connection.state == .connected }
        XCTAssertTrue(connected)
        transport.drop()
        let reconnecting = await waitUntil { core.connection.state == .reconnecting(attempt: 1) }
        XCTAssertTrue(reconnecting)
    }

    /// ADR-045: the twin for apps below iOS 17, which cannot use the `@Observable` `core.connection`.
    func testTheConnectionObjectIsTheObservableObjectTwinOfTheObservableConnection() async throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        var published: [UndraConnectionState] = []
        let sink = core.connectionObject.$state.sink { published.append($0) }
        let connected = await waitUntil { core.connectionObject.state == .connected }
        XCTAssertTrue(connected)
        transport.drop()
        let reconnecting = await waitUntil { core.connectionObject.state == .reconnecting(attempt: 1) }
        XCTAssertTrue(reconnecting)
        XCTAssertEqual(published.last, .reconnecting(attempt: 1))
        XCTAssertTrue(published.contains(.connected))
        // The two follow the same state, one hop behind it.
        if #available(iOS 17, macOS 14, *) {
            let same = await waitUntil { core.connection.state == .reconnecting(attempt: 1) }
            XCTAssertTrue(same)
        }
        sink.cancel()
    }

    /// ADR-045 review: the two observables are one fact. Every state the core goes through reaches both, and
    /// `core.connection` is the `@Observable` object that SwiftUI on iOS 17 observes (not the floor's twin, and not a
    /// fallback made on a miss): a change to the state fires an Observation tracker on it.
    func testBothConnectionObservablesFollowEveryStateAndTheObservationPathIsTheOneChosenOnIOS17() async throws {
        guard #available(iOS 17, macOS 14, *) else {
            return
        }
        let transport = FakeTransport(directSync: false)
        let core = try makeRemoteCore(transport, log: StateLog())
        XCTAssertTrue(core.connection === core.connection, "one object per core")
        func agree(on state: UndraConnectionState) async -> Bool {
            return await waitUntil { core.connectionObject.state == state && core.connection.state == state }
        }
        let connected = await agree(on: .connected)
        XCTAssertTrue(connected)
        let tracked = Guarded<Bool>(false)
        withObservationTracking({ _ = core.connection.state }, onChange: { tracked.withLock { (flag: inout Bool) -> Void in flag = true } })
        transport.drop()
        let reconnecting = await agree(on: .reconnecting(attempt: 1))
        XCTAssertTrue(reconnecting)
        XCTAssertTrue(tracked.withLock { (flag: inout Bool) -> Bool in return flag }, "Observation told the tracker")
        transport.reconnect()
        let again = await agree(on: .connected)
        XCTAssertTrue(again)
        core.shutdown()
        let closed = await agree(on: .closed(.requested))
        XCTAssertTrue(closed)
    }

    /// ADR-045 review: `onConnectionChange` runs before the hop to the main queue, so an app that drops the core
    /// from there (it loads a new one on `.closed(.sessionLost)`) must not strand a view that holds the connection.
    func testAConnectionAViewHoldsHearsTheFinalStateEvenWhenTheCoreIsReleasedAtOnce() async throws {
        weak var released: UndraCore?
        var object: UndraConnectionObject?
        var observable: AnyObject?
        do {
            let transport = FakeTransport(directSync: false)
            transport.releasesInboundOnShutdown = true
            let core = try makeRemoteCore(transport, log: StateLog())
            released = core
            object = core.connectionObject
            if #available(iOS 17, macOS 14, *) {
                observable = core.connection
            }
            core.shutdown()
        }
        XCTAssertNil(released, "nothing else keeps the core alive, so the hop must not rely on it")
        let objectHeard = await waitUntil { object?.state == .closed(.requested) }
        XCTAssertTrue(objectHeard)
        if #available(iOS 17, macOS 14, *) {
            let observableHeard = await waitUntil { (observable as? UndraConnection)?.state == .closed(.requested) }
            XCTAssertTrue(observableHeard, "the @Observable connection is told too, as the ObservableObject one is")
        }
    }

    func testAnInProcessCoreIsConnectedUntilShutdown() throws {
        let log = StateLog()
        var options = LoadOptions.inproc(adapters: Adapters.none, expectedSchemaHash: 0x1234)
        options.onConnectionChange = { log.append($0) }
        let core = try UndraCore.connect(transport: FakeTransport(), options: options)
        XCTAssertEqual(core.connectionState, .connected)
        core.shutdown()
        XCTAssertEqual(log.names, ["connecting", "connected", "closed:requested"])
    }

    func testADevNoticeFromUndraDevReachesOnDevNoticeOnAQueueOfTheRuntimesOwn() async throws {
        let notices = Guarded<[String]>([])
        let transport = FakeTransport(directSync: false)
        let options = LoadOptions.remote(
            url: "ws://fake",
            adapters: Adapters.none,
            expectedSchemaHash: 0x1234,
            onDevNotice: { message in
                notices.withLock { (list: inout [String]) -> Void in
                    list.append(message)
                }
            }
        )
        let core = try UndraCore.connect(transport: transport, options: options)
        transport.deliverLog(level: 2, target: "undra::dev", message: "Reloaded, state kept")
        transport.deliverLog(level: 2, target: "app", message: "something else")
        transport.deliverLog(level: 2, target: "undra::dev", message: "Reloaded, state reset: schema changed")
        let arrived = await waitUntil { notices.withLock { (list: inout [String]) -> Bool in list.count == 2 } }
        XCTAssertTrue(arrived)
        XCTAssertEqual(
            notices.withLock { (list: inout [String]) -> [String] in list },
            ["Reloaded, state kept", "Reloaded, state reset: schema changed"]
        )
        XCTAssertEqual(core.connectionState, .connected)
    }

    func testAnInProcessCoreNeverFiresOnDevNoticeWhateverItsLogSays() async throws {
        let notices = Guarded<[String]>([])
        var options = LoadOptions.inproc(adapters: Adapters.none, expectedSchemaHash: 0x1234)
        options.onDevNotice = { message in
            notices.withLock { (list: inout [String]) -> Void in
                list.append(message)
            }
        }
        let transport = FakeTransport()
        _ = try UndraCore.connect(transport: transport, options: options)
        transport.deliverLog(level: 2, target: "undra::dev", message: "Reloaded, state kept")
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(notices.withLock { (list: inout [String]) -> [String] in list }, [])
    }

    func testAPolicyNeverWaitsNothingAndALongOutageStaysAtTheCap() {
        let zero = UndraReconnectPolicy(initialDelay: 0, maxDelay: 0, jitter: 0)
        XCTAssertEqual(zero.delay(forAttempt: 1), 0.001, accuracy: 1e-9)
        XCTAssertEqual(UndraReconnectPolicy(jitter: 0).delay(forAttempt: 5_000), 5, accuracy: 1e-9)
    }

    func testTheReconnectPolicySchedule() {
        let longest = UndraReconnectPolicy(random: { 0 })
        XCTAssertEqual((1...8).map { longest.delay(forAttempt: $0) }, [0.25, 0.5, 1, 2, 4, 5, 5, 5])
        let shortest = UndraReconnectPolicy(random: { 0.999999 })
        let waits = (1...7).map { shortest.delay(forAttempt: $0) }
        for (wait, expected) in zip(waits, [0.125, 0.25, 0.5, 1, 2, 2.5, 2.5]) {
            XCTAssertEqual(wait, expected, accuracy: 0.001)
        }
        for attempt in 1...12 {
            let wait = UndraReconnectPolicy.default.delay(forAttempt: attempt)
            let top = min(5, 0.25 * pow(2, Double(attempt - 1)))
            XCTAssertGreaterThanOrEqual(wait, top / 2 - 0.000_001)
            XCTAssertLessThanOrEqual(wait, top)
        }
        let tuned = UndraReconnectPolicy(initialDelay: 0.1, maxDelay: 0.3, jitter: 0, random: { 0.7 })
        XCTAssertEqual((1...4).map { tuned.delay(forAttempt: $0) }, [0.1, 0.2, 0.3, 0.3])
        XCTAssertEqual(UndraReconnectPolicy(jitter: 5).jitter, 1)
    }
}
