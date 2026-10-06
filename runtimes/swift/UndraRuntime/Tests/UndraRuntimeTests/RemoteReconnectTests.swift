import Foundation
import XCTest
@testable import UndraRuntime

// MARK: - A scripted dev server behind the socket seam

/// One connection of the fake server: what the client sent is decoded, what the server says is pushed to the
/// client's `receive`, on a queue of another thread (as URLSession does).
final class FakeSocket: SocketConnection, @unchecked Sendable {
    private struct State {
        var handler: (@Sendable (Result<SocketMessage, any Error>) -> Void)? = nil
        var pending: [Result<SocketMessage, any Error>] = []
        var sent: [Envelope.Decoded] = []
        var cancelled = false
        var close: (code: Int, reason: String)? = nil
        var serverSeq: UInt32 = 0
    }

    let url: URL
    private unowned let server: FakeSocketServer
    private let state = Guarded<State>(State())

    init(url: URL, server: FakeSocketServer) {
        self.url = url
        self.server = server
    }

    // SocketConnection

    func resume() {
        server.connected(self)
    }

    func send(_ frame: [UInt8], completion: @escaping @Sendable ((any Error)?) -> Void) {
        let decoded = try? decodeEnvelope(frame)
        if let decoded = decoded {
            state.withLock { (current: inout State) -> Void in
                current.sent.append(decoded)
            }
        }
        completion(nil)
        if decoded?.kind == .hello {
            server.helloArrived(self)
        }
    }

    func receive(_ handler: @escaping @Sendable (Result<SocketMessage, any Error>) -> Void) {
        let ready = state.withLock { (current: inout State) -> Result<SocketMessage, any Error>? in
            if current.pending.isEmpty {
                current.handler = handler
                return nil
            }
            return current.pending.removeFirst()
        }
        if let ready = ready {
            DispatchQueue.global().async {
                handler(ready)
            }
        }
    }

    func cancel() {
        state.withLock { (current: inout State) -> Void in
            current.cancelled = true
            current.handler = nil
        }
    }

    var peerClose: (code: Int, reason: String)? {
        return state.withLock { (current: inout State) -> (code: Int, reason: String)? in
            return current.close
        }
    }

    // The server's side

    var envelopes: [Envelope.Decoded] {
        return state.withLock { (current: inout State) -> [Envelope.Decoded] in
            return current.sent
        }
    }

    func sent(_ kind: Envelope.Kind) -> [Envelope.Decoded] {
        return envelopes.filter { $0.kind == kind }
    }

    var wasCancelled: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.cancelled
        }
    }

    func push(_ result: Result<SocketMessage, any Error>) {
        let handler = state.withLock { (current: inout State) -> (@Sendable (Result<SocketMessage, any Error>) -> Void)? in
            if let handler = current.handler {
                current.handler = nil
                return handler
            }
            current.pending.append(result)
            return nil
        }
        if let handler = handler {
            DispatchQueue.global().async {
                handler(result)
            }
        }
    }

    func deliver(_ kind: Envelope.Kind, payload: [UInt8], schema: UInt64) {
        let seq = state.withLock { (current: inout State) -> UInt32 in
            let seq = current.serverSeq
            current.serverSeq += 1
            return seq
        }
        push(.success(.binary(encodeEnvelope(kind: kind, seq: seq, schemaHash: schema, payload: payload))))
    }

    func sendHello(schema: UInt64) {
        let hello = Wire.Hello(undraVersion: "test", schemaHash: schema, platform: "rust", mode: "dev")
        deliver(.hello, payload: hello.encode(), schema: schema)
    }

    /// The server closes the connection (or the network drops it): the next `receive` fails.
    func serverClose(code: Int?, reason: String = "") {
        state.withLock { (current: inout State) -> Void in
            if let code = code {
                current.close = (code, reason)
            }
        }
        push(.failure(URLError(.networkConnectionLost)))
    }

    var query: [String: String] {
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        var result: [String: String] = [:]
        for item in items {
            result[item.name] = item.value ?? ""
        }
        return result
    }
}

/// A dev server for the transport to connect to.
final class FakeSocketServer: SocketConnector, @unchecked Sendable {
    enum Behaviour {
        /// Opens and answers the client's `Hello`.
        case answer
        /// Fails the connection before it opens.
        case refuse
        /// Opens but never answers.
        case silent
    }

    private struct State {
        var behaviour = Behaviour.answer
        var schemaHash: UInt64 = 0x77
        var sockets: [FakeSocket] = []
        var afterHello: (@Sendable (FakeSocket, Int) -> Void)? = nil
    }

    private let state = Guarded<State>(State())

    var behaviour: Behaviour {
        get {
            return state.withLock { (current: inout State) -> Behaviour in return current.behaviour }
        }
        set {
            state.withLock { (current: inout State) -> Void in current.behaviour = newValue }
        }
    }

    var schemaHash: UInt64 {
        get {
            return state.withLock { (current: inout State) -> UInt64 in return current.schemaHash }
        }
        set {
            state.withLock { (current: inout State) -> Void in current.schemaHash = newValue }
        }
    }

    /// Runs after the server answered a `Hello`, with the socket and its number (1 for the first).
    var afterHello: (@Sendable (FakeSocket, Int) -> Void)? {
        get {
            return state.withLock { (current: inout State) -> (@Sendable (FakeSocket, Int) -> Void)? in return current.afterHello }
        }
        set {
            state.withLock { (current: inout State) -> Void in current.afterHello = newValue }
        }
    }

    var sockets: [FakeSocket] {
        return state.withLock { (current: inout State) -> [FakeSocket] in return current.sockets }
    }

    var current: FakeSocket {
        return sockets.last!
    }

    func makeConnection(url: URL) -> any SocketConnection {
        let socket = FakeSocket(url: url, server: self)
        state.withLock { (current: inout State) -> Void in
            current.sockets.append(socket)
        }
        return socket
    }

    func connected(_ socket: FakeSocket) {
        if behaviour == .refuse {
            socket.push(.failure(URLError(.cannotConnectToHost)))
        }
    }

    func helloArrived(_ socket: FakeSocket) {
        guard behaviour == .answer else {
            return
        }
        let (schema, hook, number) = state.withLock { (current: inout State) -> (UInt64, (@Sendable (FakeSocket, Int) -> Void)?, Int) in
            let number = (current.sockets.firstIndex { $0 === socket } ?? 0) + 1
            return (current.schemaHash, current.afterHello, number)
        }
        socket.sendHello(schema: schema)
        hook?(socket, number)
    }
}

/// A scheduler the test fires by hand, recording every wait it was asked for.
final class ManualScheduler: ReconnectScheduler, @unchecked Sendable {
    private final class Entry: ReconnectToken, @unchecked Sendable {
        let delay: Double
        let work: @Sendable () -> Void
        let cancelled = Guarded<Bool>(false)

        init(delay: Double, work: @escaping @Sendable () -> Void) {
            self.delay = delay
            self.work = work
        }

        func cancel() {
            cancelled.withLock { (value: inout Bool) -> Void in value = true }
        }

        var isCancelled: Bool {
            return cancelled.withLock { (value: inout Bool) -> Bool in return value }
        }
    }

    private struct State {
        var delays: [Double] = []
        var pending: [Entry] = []
    }

    private let state = Guarded<State>(State())

    func schedule(after seconds: Double, _ work: @escaping @Sendable () -> Void) -> any ReconnectToken {
        let entry = Entry(delay: seconds, work: work)
        state.withLock { (current: inout State) -> Void in
            current.delays.append(seconds)
            current.pending.append(entry)
        }
        return entry
    }

    /// Every wait asked for so far.
    var delays: [Double] {
        return state.withLock { (current: inout State) -> [Double] in return current.delays }
    }

    /// Whether an attempt is waiting to be fired.
    var hasPending: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.pending.contains { !$0.isCancelled }
        }
    }

    /// Runs the next waiting attempt now, on this thread, and returns once it is over. `false` if none waited.
    @discardableResult
    func fire() -> Bool {
        let entry = state.withLock { (current: inout State) -> Entry? in
            while !current.pending.isEmpty {
                let next = current.pending.removeFirst()
                if !next.isCancelled {
                    return next
                }
            }
            return nil
        }
        guard let entry = entry else {
            return false
        }
        entry.work()
        return true
    }
}

/// What a transport tells its core while it reconnects, in order.
final class ReconnectRecorder: UndraInbound, @unchecked Sendable {
    private let log = Guarded<[String]>([])
    private let errors = Guarded<[String]>([])
    private let disconnects = Guarded<[any Error]>([])
    var holds = false
    /// Runs when the transport asks whether objects are held: at the start of a reconnect attempt.
    var onHoldsObjects: (@Sendable () -> Void)? = nil

    var events: [String] {
        return log.withLock { (list: inout [String]) -> [String] in return list }
    }

    var lastDisconnect: (any Error)? {
        return disconnects.withLock { (list: inout [any Error]) -> (any Error)? in return list.last }
    }

    var disconnectCount: Int {
        return disconnects.withLock { (list: inout [any Error]) -> Int in return list.count }
    }

    var errorsSeen: [String] {
        return errors.withLock { (list: inout [String]) -> [String] in return list }
    }

    private func note(_ event: String) {
        log.withLock { (list: inout [String]) -> Void in list.append(event) }
    }

    func onReply(callId: UInt32, payload: [UInt8]) {}
    func onChangeSet(_ payload: [UInt8]) {}
    func onStreamItem(callId: UInt32, payload: [UInt8]) {}
    func onPortCall(portId: UInt32, methodId: UInt32, portCallId: UInt32, args: [UInt8]) -> PortCallOutcome {
        return .unavailable
    }
    func onLog(level: UInt8, target: String, message: String) {}

    func onDisconnect(_ error: any Error) {
        disconnects.withLock { (list: inout [any Error]) -> Void in list.append(error) }
        note("closed")
    }

    func onReconnecting(attempt: Int, error: any Error) {
        errors.withLock { (list: inout [String]) -> Void in list.append(String(describing: error)) }
        note("reconnecting \(attempt)")
    }

    func onReconnected() {
        note("reconnected")
    }

    func holdsObjects() -> Bool {
        onHoldsObjects?()
        return holds
    }
}

// MARK: - The tests

/// The remote transport reconnecting: the backoff, the session in the URL, what is final.
final class RemoteReconnectTests: XCTestCase {
    private let expected: UInt64 = 0x77

    private func options(timeout: Double = 2) -> TransportStartOptions {
        return TransportStartOptions(platform: "ios", logLevel: 2, connectTimeout: timeout, expectedSchemaHash: expected)
    }

    private struct Rig {
        let server: FakeSocketServer
        let scheduler: ManualScheduler
        let inbound: ReconnectRecorder
        let transport: WebSocketTransport
    }

    /// A transport started against a fake server, with the jitter fixed so the schedule is exact.
    private func started(
        reconnect: UndraReconnectPolicy? = UndraReconnectPolicy(random: { 0 }),
        timeout: Double = 2,
        session: String? = "tok-test",
        url: String = "ws://127.0.0.1:7443"
    ) throws -> Rig {
        let server = FakeSocketServer()
        let scheduler = ManualScheduler()
        let inbound = ReconnectRecorder()
        let transport = WebSocketTransport(
            url: URL(string: url)!,
            connector: server,
            reconnect: reconnect,
            session: session,
            scheduler: scheduler
        )
        _ = try transport.start(inbound: inbound, options: options(timeout: timeout))
        return Rig(server: server, scheduler: scheduler, inbound: inbound, transport: transport)
    }

    private func eventually(_ what: String, timeout: Double = hangDeadline, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline {
                return XCTFail("timed out waiting for: \(what)")
            }
            Thread.sleep(forTimeInterval: 0.005)
        }
    }

    func testADroppedConnectionIsAnnouncedAtOnceAndRetriedAfterTheFirstBackoff() throws {
        let rig = try started()
        rig.server.current.serverClose(code: nil)
        eventually("the loss is announced") { rig.inbound.events == ["reconnecting 1"] }
        eventually("an attempt is scheduled") { rig.scheduler.hasPending }
        XCTAssertEqual(rig.scheduler.delays, [0.25])
        XCTAssertEqual(rig.server.sockets.count, 1, "no attempt before the backoff passed")
        XCTAssertTrue(rig.scheduler.fire())
        eventually("reconnected") { rig.inbound.events == ["reconnecting 1", "reconnected"] }
        XCTAssertEqual(rig.server.sockets.count, 2)
    }

    func testTheBackoffDoublesFrom250MsToFiveSecondsWhileTheServerIsDownThenTheFirstAttemptThatFindsItWins() throws {
        let rig = try started()
        rig.server.behaviour = .refuse
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        for round in 1...7 {
            XCTAssertTrue(rig.scheduler.fire(), "attempt \(round)")
            eventually("the next attempt is scheduled") { rig.scheduler.delays.count == round + 1 }
        }
        XCTAssertEqual(rig.scheduler.delays, [0.25, 0.5, 1, 2, 4, 5, 5, 5])
        eventually("every attempt was announced") { rig.inbound.events.count == 8 }
        XCTAssertEqual(rig.inbound.events, (1...8).map { "reconnecting \($0)" })
        rig.server.behaviour = .answer
        XCTAssertTrue(rig.scheduler.fire())
        eventually("reconnected") { rig.inbound.events.last == "reconnected" }
        XCTAssertFalse(rig.inbound.events.contains("closed"))
    }

    func testEveryConnectionCarriesTheSameSessionTokenAndAsksToResumeOnlyWhenTheCoreHoldsObjects() throws {
        let rig = try started()
        XCTAssertEqual(rig.server.sockets[0].query["undra_session"], "tok-test")
        XCTAssertNil(rig.server.sockets[0].query["undra_resume"])

        rig.inbound.holds = false
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("reconnected") { rig.inbound.events.last == "reconnected" }
        XCTAssertEqual(rig.server.sockets[1].query["undra_session"], "tok-test")
        XCTAssertNil(rig.server.sockets[1].query["undra_resume"], "nothing to resume")

        rig.inbound.holds = true
        rig.server.current.serverClose(code: nil)
        eventually("scheduled again") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("reconnected again") { rig.server.sockets.count == 3 && rig.inbound.events.last == "reconnected" }
        XCTAssertEqual(rig.server.sockets[2].query["undra_session"], "tok-test")
        XCTAssertEqual(rig.server.sockets[2].query["undra_resume"], "1")
    }

    func testTheRestOfTheURLIsKeptAndNoTokenIsSentWhenSessionIsOff() throws {
        let rig = try started(url: "ws://127.0.0.1:7443/dev?x=1")
        XCTAssertEqual(rig.server.sockets[0].url.absoluteString, "ws://127.0.0.1:7443/dev?x=1&undra_session=tok-test")
        let quiet = try started(session: nil)
        XCTAssertEqual(quiet.server.sockets[0].url.absoluteString, "ws://127.0.0.1:7443")
        let bare = try started()
        XCTAssertEqual(bare.server.sockets[0].url.absoluteString, "ws://127.0.0.1:7443/?undra_session=tok-test")
    }

    func testTheSequenceNumbersRestartOnEveryConnection() throws {
        let rig = try started()
        rig.transport.cancel(callId: 1)
        XCTAssertEqual(rig.server.sockets[0].envelopes.map { $0.seq }, [1, 2], "Swift counts from 1")
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("reconnected") { rig.inbound.events.last == "reconnected" }
        XCTAssertEqual(rig.server.sockets[1].envelopes.map { $0.seq }, [1])
    }

    func testSendsAreRefusedWhileReconnectingAndWorkAgainAfter() throws {
        let rig = try started()
        rig.server.current.serverClose(code: nil)
        eventually("announced") { rig.inbound.events == ["reconnecting 1"] }
        XCTAssertFalse(rig.transport.send(call: [1, 2, 3]))
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("reconnected") { rig.inbound.events.last == "reconnected" }
        XCTAssertTrue(rig.transport.send(call: [1, 2, 3]))
    }

    func testACoreWithAnotherSchemaHashIsFinalTheErrorComesOnceAndNothingRetries() throws {
        let rig = try started()
        rig.server.schemaHash = 0x99 // `undra dev` rebuilt the core with another schema
        rig.server.current.serverClose(code: 1001)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("closed") { rig.inbound.disconnectCount == 1 }
        XCTAssertEqual(rig.inbound.lastDisconnect as? UndraSchemaMismatchError, UndraSchemaMismatchError(expected: 0x77, got: 0x99))
        XCTAssertEqual(rig.inbound.events, ["reconnecting 1", "closed"])
        XCTAssertFalse(rig.scheduler.hasPending, "no loop")
        XCTAssertTrue(rig.server.current.wasCancelled, "the attempt's socket was let go")
    }

    func testASessionTheServerLostIsFinalWhetherTheCodeOrOnlyTheReasonSaysSo() throws {
        for (code, reason) in [(4001, "session lost: this core has no session tok-test"), (1011, "session lost: restarted")] {
            let rig = try started()
            rig.inbound.holds = true
            rig.server.afterHello = { socket, number in
                if number == 2 {
                    socket.serverClose(code: code, reason: reason)
                }
            }
            rig.server.current.serverClose(code: 1001)
            eventually("scheduled") { rig.scheduler.hasPending }
            rig.scheduler.fire()
            eventually("closed") { rig.inbound.disconnectCount == 1 }
            XCTAssertTrue(rig.inbound.lastDisconnect is UndraSessionLostError, "\(String(describing: rig.inbound.lastDisconnect))")
            XCTAssertTrue((rig.inbound.lastDisconnect as? UndraSessionLostError)?.reason.contains("session lost") == true)
            XCTAssertFalse(rig.scheduler.hasPending)
        }
    }

    func testAnAttemptThatGetsNoHelloTimesOutAndIsRetried() throws {
        let rig = try started(timeout: 0.2)
        rig.server.behaviour = .silent
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire() // blocks for the handshake timeout
        eventually("announced as attempt 2") { rig.inbound.events == ["reconnecting 1", "reconnecting 2"] }
        XCTAssertTrue(rig.inbound.errorsSeen[1].contains("handshakeTimedOut") || rig.inbound.errorsSeen[1].contains("did not answer"), rig.inbound.errorsSeen[1])
        XCTAssertTrue(rig.server.current.wasCancelled, "the silent socket was let go")
        eventually("the next attempt is scheduled") { rig.scheduler.delays.count == 2 }
    }

    func testThePolicyGivesUpAfterMaxAttemptsAndSaysWhy() throws {
        let rig = try started(reconnect: UndraReconnectPolicy(maxAttempts: 2, random: { 0 }))
        rig.server.behaviour = .refuse
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.scheduler.fire()
        eventually("attempt 2 scheduled") { rig.scheduler.delays.count == 2 }
        rig.scheduler.fire()
        eventually("closed") { rig.inbound.disconnectCount == 1 }
        XCTAssertEqual(rig.inbound.events, ["reconnecting 1", "reconnecting 2", "closed"])
        guard case UndraTransportError.connectionLost(let reason)? = rig.inbound.lastDisconnect as? UndraTransportError else {
            return XCTFail("\(String(describing: rig.inbound.lastDisconnect))")
        }
        XCTAssertTrue(reason.contains("gave up"), reason)
    }

    func testWithoutAReconnectPolicyALostConnectionIsFinalAtOnce() throws {
        let rig = try started(reconnect: nil)
        rig.server.current.serverClose(code: nil)
        eventually("closed") { rig.inbound.disconnectCount == 1 }
        XCTAssertTrue(rig.inbound.lastDisconnect is UndraTransportError)
        XCTAssertFalse(rig.scheduler.hasPending)
        XCTAssertEqual(rig.server.sockets.count, 1)
    }

    func testShutdownDuringTheBackoffCancelsTheRetryAndNobodyIsTold() throws {
        let rig = try started()
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        rig.transport.shutdown()
        XCTAssertFalse(rig.scheduler.hasPending, "the pending attempt was cancelled")
        XCTAssertFalse(rig.scheduler.fire())
        XCTAssertEqual(rig.server.sockets.count, 1)
        XCTAssertEqual(rig.inbound.disconnectCount, 0)
    }

    func testShutdownThatRacesAStartingAttemptDoesNotBringTheTransportBack() throws {
        // `shutdown()` lands after the attempt checked that the transport is live and before it connects: the
        // attempt must not reopen a transport that was shut down (a socket nobody closes would hold the dev
        // server's one client slot).
        let rig = try started()
        rig.server.current.serverClose(code: nil)
        eventually("scheduled") { rig.scheduler.hasPending }
        let transport = rig.transport
        rig.inbound.onHoldsObjects = { transport.shutdown() }
        XCTAssertTrue(rig.scheduler.fire())
        XCTAssertEqual(rig.server.sockets.count, 1, "no new connection after shutdown()")
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertFalse(rig.inbound.events.contains("reconnected"), "\(rig.inbound.events)")
        XCTAssertFalse(rig.scheduler.hasPending, "and no further attempt")
        XCTAssertFalse(transport.sendFrame(kind: .call, payload: []), "still shut down")
    }

    func testAnInitialConnectionThatFailsStillThrowsFromStartAndIsNotRetried() throws {
        let server = FakeSocketServer()
        server.behaviour = .refuse
        let scheduler = ManualScheduler()
        let transport = WebSocketTransport(
            url: URL(string: "ws://127.0.0.1:7443")!,
            connector: server,
            reconnect: UndraReconnectPolicy(),
            session: "tok",
            scheduler: scheduler
        )
        XCTAssertThrowsError(try transport.start(inbound: ReconnectRecorder(), options: options())) { error in
            guard case UndraLoadError.connectionFailed? = error as? UndraLoadError else {
                return XCTFail("\(error)")
            }
        }
        XCTAssertFalse(scheduler.hasPending)
    }

    func testAMalformedServerMessageIsNotALostConnection() throws {
        let rig = try started()
        rig.server.current.push(.success(.text))
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(rig.inbound.events, [], "a text frame is logged, as it always was")
        XCTAssertTrue(rig.transport.send(call: [1]))
    }
}
