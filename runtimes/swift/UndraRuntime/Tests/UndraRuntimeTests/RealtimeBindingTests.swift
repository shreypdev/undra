import Foundation
import XCTest
@testable import UndraRuntime

// MARK: - A scripted WebSocket adapter

/// One scripted connection: the test pushes messages and ends; the binding's pump pulls them.
final class ScriptedSocket: WebSocketConnection, @unchecked Sendable {
    private struct State {
        var inbox: [WsMessage] = []
        var end: WsError?
        var finished = false
        var closedWith: (code: UInt16, reason: String)?
        var pulled = 0
        /// When the pump took each message (`DispatchTime.uptimeNanoseconds`), in order.
        var pulledAt: [UInt64] = []
        var sent: [WsMessage] = []
        var waiter: CheckedContinuation<Void, Never>?
        var failSends: WsError?
    }

    let negotiatedProtocol: String
    let url: String
    let headers: [Header]
    private let state = Locked(State())

    init(url: String, protocol negotiated: String, headers: [Header]) {
        self.url = url
        self.negotiatedProtocol = negotiated
        self.headers = headers
    }

    var messages: AsyncThrowingStream<WsMessage, any Error> {
        return AsyncThrowingStream(unfolding: { [self] () async throws -> WsMessage? in
            return try await self.next()
        })
    }

    private func next() async throws -> WsMessage? {
        while true {
            let outcome = state.withLock { (current: inout State) -> Result<WsMessage?, WsError>? in
                if current.closedWith != nil {
                    return .success(nil)
                }
                if !current.inbox.isEmpty {
                    current.pulled += 1
                    current.pulledAt.append(DispatchTime.now().uptimeNanoseconds)
                    return .success(current.inbox.removeFirst())
                }
                if let end = current.end {
                    return .failure(end)
                }
                if current.finished {
                    return .success(nil)
                }
                return nil
            }
            if let outcome = outcome {
                return try outcome.get()
            }
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let ready = state.withLock { (current: inout State) -> Bool in
                    if !current.inbox.isEmpty || current.end != nil || current.finished || current.closedWith != nil {
                        return true
                    }
                    current.waiter = continuation
                    return false
                }
                if ready {
                    continuation.resume()
                }
            }
        }
    }

    private func wake(_ change: (inout State) -> Void) {
        let waiter = state.withLock { (current: inout State) -> CheckedContinuation<Void, Never>? in
            change(&current)
            let taken = current.waiter
            current.waiter = nil
            return taken
        }
        waiter?.resume()
    }

    /// The server sends `messages`.
    func push(_ messages: WsMessage...) {
        wake { $0.inbox.append(contentsOf: messages) }
    }

    /// The stream ends with `error` once the inbox is drained.
    func end(_ error: WsError) {
        wake { $0.end = error }
    }

    /// The stream finishes without an error.
    func finish() {
        wake { $0.finished = true }
    }

    /// Sends fail with `error` from now on.
    func failSends(_ error: WsError) {
        state.withLock { $0.failSends = error }
    }

    /// How many messages the binding's pump took.
    var pulled: Int {
        return state.withLock { $0.pulled }
    }

    /// When the binding's pump took each message, in order (`DispatchTime.uptimeNanoseconds`, read as it took it).
    var pulledAt: [UInt64] {
        return state.withLock { $0.pulledAt }
    }

    /// What the binding sent.
    var sent: [WsMessage] {
        return state.withLock { $0.sent }
    }

    /// How the binding closed it.
    var closedWith: (code: UInt16, reason: String)? {
        return state.withLock { $0.closedWith }
    }

    func send(_ message: WsMessage) async throws(WsError) {
        let failure = state.withLock { (current: inout State) -> WsError? in
            if let failure = current.failSends {
                return failure
            }
            current.sent.append(message)
            return nil
        }
        if let failure = failure {
            throw failure
        }
    }

    func close(code: UInt16, reason: String) async {
        wake { current in
            if current.closedWith == nil {
                current.closedWith = (code, reason)
            }
        }
    }
}

/// Accepts every connection (the first offered subprotocol) unless told to refuse.
final class ScriptedWebSocket: WebSocketAdapter, @unchecked Sendable {
    private let state = Locked<(refusal: WsError?, sockets: [ScriptedSocket])>((nil, []))

    func refuse(with error: WsError?) {
        state.withLock { $0.refusal = error }
    }

    var sockets: [ScriptedSocket] {
        return state.withLock { $0.sockets }
    }

    func connect(url: String, protocols: [String], headers: [Header]) async throws(WsError) -> any WebSocketConnection {
        let refusal = state.withLock { $0.refusal }
        if let refusal = refusal {
            throw refusal
        }
        let socket = ScriptedSocket(url: url, protocol: protocols.first ?? "", headers: headers)
        state.withLock { $0.sockets.append(socket) }
        return socket
    }
}

/// A value behind a lock, for the scripted adapters.
final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value

    init(_ value: Value) {
        self.value = value
    }

    func withLock<Result>(_ body: (inout Value) throws -> Result) rethrows -> Result {
        lock.lock()
        defer { lock.unlock() }
        return try body(&value)
    }
}

/// Polls `condition` every 5 ms until it holds. A hang detector, not a speed assertion: it gives up only after ``hangDeadline``, and
/// returns `false` (after failing the test) when it does, so a caller with more waits to make can stop instead of waiting again.
@discardableResult
func eventually(_ what: String, file: StaticString = #filePath, line: UInt = #line, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(hangDeadline)
    while !condition() {
        if Date() > deadline {
            XCTFail("timed out waiting for \(what)", file: file, line: line)
            return false
        }
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
    return true
}

/// Starts `body` in a task and returns the task once it has begun running `body`. What `body` does before it first suspends has then been
/// done too, short of a preemption in the few instructions between the flag and that: a binding registers a pending pull (`receive`,
/// `next`) or a waiter for a transaction inside the call, before it waits. The 20 to 50 ms sleeps this replaces bet on how soon a machine
/// starts a task; lost, a "second" receive was the first one and waited for a message that never came, or a close came before the call.
func running<Success: Sendable>(_ body: @escaping @Sendable () async -> Success) async -> Task<Success, Never> {
    let begun = Locked(false)
    let task = Task { () async -> Success in
        begun.withLock { $0 = true }
        return await body()
    }
    await eventually("the task to start") { begun.withLock { $0 } }
    return task
}

/// Makes the same call twice at once and returns the outcome of the one that finished first, with the other still
/// running. A binding allows one pending `receive` or `next` per connection, so of two made together the one that
/// registers second is refused at once and the first waits; which is which depends on scheduling (`running` sees a
/// task begin, not the binding register its pull, and a preemption between the two made the "second" call the
/// pending one, waiting for a message that never came). A test asserts on the two outcomes, never on the order.
func firstOfTwo<Value: Sendable, Failure: Error & Sendable>(
    _ body: @escaping @Sendable () async -> Result<Value, Failure>
) async -> (first: Result<Value, Failure>, other: Task<Result<Value, Failure>, Never>) {
    let done = Locked<[Int: Result<Value, Failure>]>([:])
    let tasks = [0, 1].map { index in
        Task { () async -> Result<Value, Failure> in
            let outcome = await body()
            done.withLock { $0[index] = outcome }
            return outcome
        }
    }
    await eventually("one of the two calls to finish") { done.withLock { !$0.isEmpty } }
    let (index, first) = done.withLock { $0.min { $0.key < $1.key }! }
    return (first, tasks[1 - index])
}

/// `running` for a body that throws.
func runningThrowing<Success: Sendable>(_ body: @escaping @Sendable () async throws -> Success) async -> Task<Success, any Error> {
    let begun = Locked(false)
    let task = Task { () async throws -> Success in
        begun.withLock { $0 = true }
        return try await body()
    }
    await eventually("the task to start") { begun.withLock { $0 } }
    return task
}

/// `body`'s outcome as a typed `Result` (a `Task` can only carry `any Error`).
func capture<Value, Failure: Error>(_ body: () async throws(Failure) -> Value) async -> Result<Value, Failure> {
    do {
        return .success(try await body())
    } catch {
        return .failure(error)
    }
}

/// Expects `body` to throw exactly `expected`.
func expectThrows<Failure: Error & Equatable, Value>(
    _ expected: Failure,
    file: StaticString = #filePath,
    line: UInt = #line,
    _ body: () async throws(Failure) -> Value
) async {
    do {
        let value = try await body()
        XCTFail("expected \(expected), got \(value)", file: file, line: line)
    } catch {
        XCTAssertEqual(error, expected, file: file, line: line)
    }
}

// MARK: - The WebSocket binding

final class WebSocketBindingTests: XCTestCase {
    private func texts(_ range: Range<Int>) -> [WsMessage] {
        return range.map { WsMessage.text("\($0)") }
    }

    func testConnectRegistersIdsFromOneAndChecksTheUrlFirst() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let first = try await binding.connect(url: "ws://a.test/x", protocols: ["v2", "v1"], headers: [Header(name: "X-Token", value: "t")])
        XCTAssertEqual(first, WsOpened(conn: 1, protocol: "v2"))
        let second = try await binding.connect(url: "WSS://b.test", protocols: [], headers: [])
        XCTAssertEqual(second, WsOpened(conn: 2, protocol: ""))
        XCTAssertEqual(adapter.sockets.first?.headers, [Header(name: "X-Token", value: "t")])
        await expectThrows(WsError.refused(status: nil, message: "invalid URL: https://c.test")) { () async throws(WsError) -> WsOpened in
            try await binding.connect(url: "https://c.test", protocols: [], headers: [])
        }
        XCTAssertEqual(adapter.sockets.count, 2, "the adapter was not asked for a URL that is not ws:// or wss://")
        adapter.refuse(with: .refused(status: 401, message: "no"))
        await expectThrows(WsError.refused(status: 401, message: "no")) { () async throws(WsError) -> WsOpened in
            try await binding.connect(url: "ws://d.test", protocols: [], headers: [])
        }
        adapter.refuse(with: nil)
        let third = try await binding.connect(url: "ws://e.test", protocols: [], headers: [])
        XCTAssertEqual(third.conn, 3, "a refused connect takes no id")
    }

    func testThePumpReadsAheadOnlyTheWindow() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        for message in texts(0 ..< 100) {
            socket.push(message)
        }
        // 16 before the first receive, and no more while nobody pulls.
        await eventually("the initial read-ahead") { socket.pulled == 16 }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(socket.pulled, 16)

        // A receive of 4 takes 4 of the buffered 16; 12 are still more than the new window (4).
        let four = try await binding.receive(conn: conn, max: 4)
        XCTAssertEqual(four, texts(0 ..< 4))
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(socket.pulled, 16)
        // Draining the buffer lets the pump refill it up to the window, and no further.
        for start in stride(from: 4, to: 16, by: 4) {
            let batch = try await binding.receive(conn: conn, max: 4)
            XCTAssertEqual(batch, texts(start ..< start + 4))
        }
        await eventually("the refill") { socket.pulled == 20 }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(socket.pulled, 20)
        // A larger receive widens the window and waits for the burst still arriving: one answer of 50.
        let next = try await binding.receive(conn: conn, max: 50)
        XCTAssertEqual(next, texts(16 ..< 66))
        await eventually("the wider window") { socket.pulled == 100 }
        // The last 34 are all there will be for now: answered once the burst is over.
        let rest = try await binding.receive(conn: conn, max: 50)
        XCTAssertEqual(rest, texts(66 ..< 100))
    }

    func testABurstIsOneAnswerAndALoneMessageIsNotHeldBack() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        // A pull waiting when a burst starts gets the burst, not its first message.
        let waiting = await runningThrowing { try await binding.receive(conn: conn, max: 16) }
        for message in texts(0 ..< 10) {
            socket.push(message)
        }
        let burst = try await waiting.value
        XCTAssertEqual(burst, texts(0 ..< 10))
        // A lone message is answered once the burst gap passed. How long the answer took is the machine's business, so what is asserted is
        // what answers it: with fewer than `max` messages, no end and no message after it, the only thing that can answer the pull is the
        // burst timer (the cap is checked when a message arrives, and none does), so the answer coming at all proves the timer answered it,
        // and the timer's length is the constant below (a gap raised to 2 s would hold every lone message for 2 s: it fails this assertion).
        // (That the timer fires when armed, against a timer of the same length armed beside it, is
        // `RealtimeReviewTests.testALoneMessageIsAnsweredWithinAFewMillisecondsOfItsArrival`.)
        XCTAssertEqual(burstGap, .milliseconds(2), "a lone message waits the burst gap: 2 ms (ADR-047 section 3)")
        XCTAssertEqual(burstCap, 8_000_000, "a burst still arriving is answered at the latest 8 ms after its first message")
        let lone = await runningThrowing { try await binding.receive(conn: conn, max: 16) }
        socket.push(.text("lone"))
        let answer = try await lone.value
        XCTAssertEqual(answer, [.text("lone")])
    }

    func testAReceiveWaitsForTheFirstMessageAndOnlyOneMayBePending() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        let (refused, waiting) = await firstOfTwo { await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 16) } }
        XCTAssertEqual(refused, .failure(.protocol("a receive is already pending on connection \(conn)")), "the second receive is refused at once")
        socket.push(.text("a"))
        let first = try await waiting.value.get()
        XCTAssertEqual(first, [.text("a")], "a pending receive answers as soon as one message is there")
    }

    func testTheEndComesAfterTheBufferedMessagesAndSticks() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        socket.push(.text("a"), .binary([1, 2]))
        socket.end(.closed(code: 4001, reason: "kicked"))
        await eventually("the end") { binding.isEnded(conn) }
        let messages = try await binding.receive(conn: conn, max: 16)
        XCTAssertEqual(messages, [.text("a"), .binary([1, 2])])
        for _ in 0 ..< 2 {
            await expectThrows(WsError.closed(code: 4001, reason: "kicked")) { () async throws(WsError) -> [WsMessage] in
                try await binding.receive(conn: conn, max: 16)
            }
        }
        await expectThrows(WsError.closed(code: 4001, reason: "kicked")) { () async throws(WsError) -> Void in
            try await binding.send(conn: conn, message: .text("late"))
        }
        // Closing after the end is still allowed, and then the stream ends cleanly.
        try await binding.close(conn: conn, code: 1000, reason: "done")
        let after = try await binding.receive(conn: conn, max: 16)
        XCTAssertEqual(after, [])
    }

    func testAPendingReceiveHearsTheEndAndAFinishedStreamIsANetworkEnd() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let dropped = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let waiting = await running { await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: dropped, max: 16) } }
        adapter.sockets[0].end(.network("reset"))
        await expectThrows(WsError.network("reset")) { () async throws(WsError) -> [WsMessage] in try await waiting.value.get() }

        let finished = try await binding.connect(url: "ws://y.test", protocols: [], headers: []).conn
        adapter.sockets[1].finish()
        await expectThrows(WsError.network("the connection ended")) { () async throws(WsError) -> [WsMessage] in
            try await binding.receive(conn: finished, max: 16)
        }
    }

    func testCloseAnswersAPendingReceiveDropsTheBufferAndRefusesSends() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        try await binding.send(conn: conn, message: .text("hi"))
        XCTAssertEqual(socket.sent, [.text("hi")])
        let waiting = await runningThrowing { try await binding.receive(conn: conn, max: 16) }
        try await binding.close(conn: conn, code: 4000, reason: "bye")
        let answered = try await waiting.value
        XCTAssertEqual(answered, [], "the core's close ends its pending receive cleanly")
        XCTAssertEqual(socket.closedWith?.code, 4000)
        XCTAssertEqual(socket.closedWith?.reason, "bye")
        socket.push(.text("late"))
        let after = try await binding.receive(conn: conn, max: 16)
        XCTAssertEqual(after, [])
        await expectThrows(WsError.closed(code: 4000, reason: "bye")) { () async throws(WsError) -> Void in
            try await binding.send(conn: conn, message: .text("x"))
        }
        try await binding.close(conn: conn, code: 1000, reason: "again")
        XCTAssertEqual(socket.closedWith?.code, 4000, "a second close is Ok and changes nothing")
    }

    func testUnknownIdsAreNetworkErrorsAndAFailedSendIsTyped() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        await expectThrows(WsError.network("no WebSocket connection 9")) { () async throws(WsError) -> [WsMessage] in
            try await binding.receive(conn: 9, max: 1)
        }
        await expectThrows(WsError.network("no WebSocket connection 9")) { () async throws(WsError) -> Void in
            try await binding.send(conn: 9, message: .text("x"))
        }
        await expectThrows(WsError.network("no WebSocket connection 9")) { () async throws(WsError) -> Void in
            try await binding.close(conn: 9, code: 1000, reason: "")
        }
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        adapter.sockets[0].failSends(.network("broken pipe"))
        await expectThrows(WsError.network("broken pipe")) { () async throws(WsError) -> Void in
            try await binding.send(conn: conn, message: .binary([1]))
        }
    }

    func testDetachClosesEveryOpenConnectionGoingAway() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        _ = try await binding.connect(url: "ws://a.test", protocols: [], headers: [])
        let closed = try await binding.connect(url: "ws://b.test", protocols: [], headers: []).conn
        try await binding.close(conn: closed, code: 1000, reason: "done")
        binding.detach()
        await eventually("the 1001 close") { adapter.sockets[0].closedWith != nil }
        XCTAssertEqual(adapter.sockets[0].closedWith?.code, 1001)
        XCTAssertEqual(adapter.sockets[0].closedWith?.reason, "")
        XCTAssertEqual(adapter.sockets[1].closedWith?.code, 1000, "a connection the core closed is not closed again")
        await expectThrows(WsError.network("the core shut down")) { () async throws(WsError) -> WsOpened in
            try await binding.connect(url: "ws://c.test", protocols: [], headers: [])
        }
        XCTAssertEqual(adapter.sockets[2].closedWith?.code, 1001, "a connection made after the shutdown is closed at once")
    }

    func testThePortImplDecodesArgumentsAndAnswersTypedErrors() async throws {
        let adapter = ScriptedWebSocket()
        let port = WebSocketPortAdapter(adapter)
        XCTAssertEqual(port.portId, 0x7388_b95f)
        let core = try makeCore(FakeTransport())
        let impl = port.makePortImpl(core: core)
        let connectArgs = encodeArgs { (w: inout UndraWriter) -> Void in
            w.writeString("ws://x.test")
            ["chat"].undraEncode(&w)
            [Header(name: "a", value: "b")].undraEncode(&w)
        }
        let opened = try WsOpened.undraDecoded(from: await PortCaller.callAsync(impl, StandardPorts.WebSocket.connect, connectArgs))
        XCTAssertEqual(opened, WsOpened(conn: 1, protocol: "chat"))
        adapter.sockets[0].push(.text("m"))
        let sendArgs = encodeArgs { (w: inout UndraWriter) -> Void in
            w.writeU32(1)
            WsMessage.binary([3]).undraEncode(&w)
        }
        let sentReply = try await PortCaller.callAsync(impl, StandardPorts.WebSocket.send, sendArgs)
        XCTAssertEqual(sentReply, [])
        XCTAssertEqual(adapter.sockets[0].sent, [.binary([3])])
        let receiveArgs = encodeArgs { (w: inout UndraWriter) -> Void in
            w.writeU32(1)
            w.writeU32(16)
        }
        let received = try [WsMessage].undraDecoded(from: await PortCaller.callAsync(impl, StandardPorts.WebSocket.receive, receiveArgs))
        XCTAssertEqual(received, [.text("m")])
        let closeArgs = encodeArgs { (w: inout UndraWriter) -> Void in
            w.writeU32(7)
            w.writeU16(1000)
            w.writeString("")
        }
        do {
            _ = try await PortCaller.callAsync(impl, StandardPorts.WebSocket.close, closeArgs)
            XCTFail("an unknown connection is an error")
        } catch let error as UndraPortError {
            XCTAssertEqual(try WsError.undraDecoded(from: error.body), .network("no WebSocket connection 7"))
        }
        port.detach()
        await eventually("the shutdown close") { adapter.sockets[0].closedWith?.code == 1001 }
        core.shutdown()
    }
}

extension WebSocketBinding {
    /// Whether connection `conn`'s stream ended on the platform side.
    func isEnded(_ conn: UInt32) -> Bool {
        return terminal(of: conn) != nil
    }
}
