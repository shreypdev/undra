import Foundation
import SQLite3
import XCTest
@testable import UndraRuntime

// The adversarial review of ports-v2 on Swift (ADR-047, ADR-048): backpressure as credit, the
// burst window's latency, close races, the Db binding's deadlines and shutdown on the real
// SQLite adapter, typed ends of the default adapters, and the bridge's status codes.

// MARK: - Helpers

/// A sleep of `milliseconds`, started now: the instant it ended. What a bound on the binding's own timer is measured against: a machine that is slow or
/// busy runs it as late as the timer of the binding it was armed beside, so how much later than it the binding finished is the binding's, not the machine's.
private func referenceSleep(milliseconds: UInt64) -> Task<UInt64, Never> {
    Task { () async -> UInt64 in
        try? await Task.sleep(nanoseconds: milliseconds * 1_000_000)
        return DispatchTime.now().uptimeNanoseconds
    }
}

/// Wraps a WebSocket adapter and counts how many messages the binding's pump took from each
/// connection's stream (what the platform delivered into the binding's memory).
final class CountingWebSocket: WebSocketAdapter, @unchecked Sendable {
    private let inner: any WebSocketAdapter
    let pumped = Locked<[Int]>([])

    init(_ inner: any WebSocketAdapter) {
        self.inner = inner
    }

    func connect(url: String, protocols: [String], headers: [Header]) async throws(WsError) -> any WebSocketConnection {
        let connection = try await inner.connect(url: url, protocols: protocols, headers: headers)
        let index = pumped.withLock { (all: inout [Int]) -> Int in
            all.append(0)
            return all.count - 1
        }
        return Counted(inner: connection, index: index, counter: pumped)
    }

    final class Counted: WebSocketConnection, @unchecked Sendable {
        let inner: any WebSocketConnection
        let index: Int
        let counter: Locked<[Int]>

        init(inner: any WebSocketConnection, index: Int, counter: Locked<[Int]>) {
            self.inner = inner
            self.index = index
            self.counter = counter
        }

        var negotiatedProtocol: String {
            return inner.negotiatedProtocol
        }

        var messages: AsyncThrowingStream<WsMessage, any Error> {
            let source = inner.messages
            let counter = self.counter
            let index = self.index
            let iterator = Locked(source.makeAsyncIterator())
            return AsyncThrowingStream(unfolding: { () async throws -> WsMessage? in
                var current = iterator.withLock { $0 }
                let next = try await current.next()
                iterator.withLock { $0 = current }
                if next != nil {
                    counter.withLock { $0[index] += 1 }
                }
                return next
            })
        }

        func send(_ message: WsMessage) async throws(WsError) {
            try await inner.send(message)
        }

        func close(code: UInt16, reason: String) async {
            await inner.close(code: code, reason: reason)
        }
    }
}

/// The server, shared with the other realtime suites.
private func sharedServer() throws -> RealtimeServer {
    if RealtimeAdapterServer.current == nil {
        RealtimeAdapterServer.current = try RealtimeServer.start()
    }
    return RealtimeAdapterServer.current!
}

// MARK: - 1. Backpressure, latency, races

final class RealtimeReviewTests: XCTestCase {
    /// 1a: a flood of small messages to a core that does not pull. The binding holds at most the
    /// window, URLSession stops reading, and the server's writer stops before the end of the flood
    /// (``RealtimeServer/waitForTheWriterToStall(_:answers:file:line:)``: counted, not timed); reading
    /// then gets every message, in order, so the stall was the reader's push-back.
    func testAWebSocketFloodOfSmallMessagesStallsTheServerWhileTheCoreDoesNotPull() async throws {
        let server = try sharedServer()
        let counting = CountingWebSocket(URLSessionWebSocketAdapter())
        let binding = WebSocketBinding(adapter: counting)
        defer { binding.detach() }
        let total = 100_000
        let conn = try await binding.connect(url: "\(server.ws)/ws/flood?n=\(total)&size=512", protocols: [], headers: []).conn
        let first = try await binding.receive(conn: conn, max: 16)
        XCTAssertFalse(first.isEmpty)
        let stalled = try await server.waitForTheWriterToStall("/ws/flood")
        let pumped = counting.pumped.withLock { $0[0] }
        XCTAssertLessThan(stalled, total, "the server wrote all \(total) messages to a core that did not pull: nothing pushed back")
        XCTAssertLessThanOrEqual(pumped - first.count, 16, "the binding read ahead more than the window")
        var received = first
        while true {
            do {
                received += try await binding.receive(conn: conn, max: 16)
            } catch {
                XCTAssertEqual(error, .closed(code: 1000, reason: "end"))
                break
            }
        }
        XCTAssertEqual(received.count, total)
        let inOrder = received.enumerated().allSatisfy { (index: Int, message: WsMessage) -> Bool in
            guard case .text(let text) = message else {
                return false
            }
            return text.hasPrefix("\(index).") && text.utf8.count == 512
        }
        XCTAssertTrue(inOrder, "every message arrived, in order")
    }

    /// 1a, SSE: a flood of events to a core that does not pull stops the server's writer before the
    /// end of the flood (the adapter suspends its data task, URLSession stops reading, the kernel's
    /// buffers fill and TCP pushes back; ``RealtimeServer/waitForTheWriterToStall(_:answers:file:line:)``
    /// waits for that by counting, not by time); reading again starts the writer again, and the
    /// events come in order. (Draining all 100,000 takes ~20 s in a debug build, so this reads at
    /// least the first 5,000, until the server wrote more than at the stall, and closes.)
    func testAnSseFloodStallsTheServerWhileTheCoreDoesNotPull() async throws {
        let server = try sharedServer()
        let binding = SseBinding(adapter: URLSessionSseAdapter())
        defer { binding.detach() }
        let total = 100_000
        let stream = try await binding.open(url: "\(server.http)/sse/flood?n=\(total)&size=512", headers: [], lastEventId: nil)
        let first = try await binding.next(stream: stream, max: 16)
        XCTAssertFalse(first.isEmpty)
        let stalled = try await server.waitForTheWriterToStall("/sse/flood")
        XCTAssertLessThan(stalled, total, "the server wrote all \(total) events to a core that did not pull: nothing pushed back")
        // Reading releases it: the stall was the reader's push-back, not a writer that stopped on its own. A reader whose
        // server never writes again would wait for an event forever; the detector closes the stream so the test fails instead.
        let detector = HangDetector { try? await binding.close(stream: stream) }
        defer { detector.cancel() }
        var received = first
        var resumed = false
        var nextLook = 0
        while received.count < 5_000 || !resumed {
            let batch = try await binding.next(stream: stream, max: 16)
            if batch.isEmpty {
                break // closed by the detector
            }
            received += batch
            if !resumed && received.count >= nextLook {
                nextLook = received.count + 500
                resumed = try await server.last("/sse/flood").map { $0.written > stalled } ?? false
            }
        }
        XCTAssertFalse(detector.fired, "the reader waited for an event that never came")
        XCTAssertTrue(resumed, "the server never wrote again after the core read \(received.count) events")
        XCTAssertTrue(received.enumerated().allSatisfy { $0.element.id == "\($0.offset)" }, "in order")
        try await binding.close(stream: stream)
        let left = try await server.waitFor("/sse/flood") { $0.clientClosed }
        XCTAssertEqual(left?.clientClosed, true)
    }

    /// A `burstGap` timer on the queue the inbox arms its own on, armed now: the instant its waiter ran again.
    /// What the binding does for a lone message, and nothing else, so the two finish together wherever they run.
    private func referenceTimer() -> Task<UInt64, Never> {
        Task { () async -> UInt64 in
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + burstGap) {
                    continuation.resume()
                }
            }
            return DispatchTime.now().uptimeNanoseconds
        }
    }

    /// 1c: one lone message is answered within about the burst gap of its arrival, whether the
    /// pull was waiting or came after it.
    ///
    /// "About the burst gap" is not a number of milliseconds a test can hold a machine to: the 2 ms timer that
    /// ends a burst fired after about 3 ms on a laptop, and after 5, 10 and 17 ms in three runs on a hosted macOS
    /// runner (within one run, too: a calibration made after the rounds read 5 ms where the rounds had seen 17).
    /// So every round arms a timer of its own beside the binding's, for the same gap on the same queue at the
    /// same moment, and what is measured is how much later than that timer the binding answered. A lone message
    /// held for the 8 ms burst cap, or for a linger, answers milliseconds after it on any machine whose timers
    /// can tell 2 ms from 8; a slow clock moves both and leaves the difference alone.
    func testALoneMessageIsAnsweredWithinAFewMillisecondsOfItsArrival() async throws {
        let adapter = ScriptedWebSocket()
        let binding = WebSocketBinding(adapter: adapter)
        let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
        let socket = try XCTUnwrap(adapter.sockets.first)
        var waited: [Double] = []
        var late: [Double] = []
        // And not before the burst gap, in every round: the binding cannot know a message is alone until the gap passed without another, and a
        // slow machine only makes that later (the TypeScript runtime's clock-driven test asserts the same: nothing at 1 ms, the answer at 2).
        var soonest = UInt64.max
        for round in 0 ..< 40 {
            // The pull waits; the message arrives.
            let pull = Task { () async -> (Result<[WsMessage], WsError>, UInt64) in
                let got = await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 16) }
                return (got, DispatchTime.now().uptimeNanoseconds)
            }
            try await Task.sleep(nanoseconds: 5_000_000)
            var reference = referenceTimer()
            let pushed = DispatchTime.now().uptimeNanoseconds
            socket.push(.text("w\(round)"))
            let (result, at) = await pull.value
            XCTAssertEqual(try result.get(), [.text("w\(round)")])
            waited.append((Double(at) - Double(await reference.value)) / 1e6)
            soonest = min(soonest, at - pushed)
            // The message is there before the pull.
            socket.push(.text("l\(round)"))
            await eventually("the read-ahead") { socket.pulled == 2 * (round + 1) }
            reference = referenceTimer()
            let pulled = DispatchTime.now().uptimeNanoseconds
            let early = try await binding.receive(conn: conn, max: 16)
            let answered = DispatchTime.now().uptimeNanoseconds
            XCTAssertEqual(early, [.text("l\(round)")])
            late.append((Double(answered) - Double(await reference.value)) / 1e6)
            soonest = min(soonest, answered - pulled)
        }
        XCTAssertGreaterThanOrEqual(soonest, 2_000_000, "a lone message was answered \(Double(soonest) / 1e6) ms after it arrived or was asked for: before the 2 ms burst gap")
        waited.sort()
        late.sort()
        XCTAssertLessThan(waited[20], 5, "a lone message was answered \(waited[20]) ms (the median of 40) after a burst-gap timer armed at its arrival")
        XCTAssertLessThan(late[20], 5, "a pull that found one message was answered \(late[20]) ms (the median of 40) after a burst-gap timer armed with it")
        // The tail: all but four rounds of forty (a round in which the machine held up the binding's answer and
        // not the timer beside it says nothing about the binding).
        XCTAssertLessThan(waited[35], 100, "a lone message waited 100 ms or more past its burst-gap timer in five rounds of 40")
        XCTAssertLessThan(late[35], 100, "a pull that found one message waited 100 ms or more past its burst-gap timer in five rounds of 40")
    }

    /// 1c: a steady trickle (one message every 0.5 ms, never a 2 ms gap) still answers within the
    /// burst cap of the pull's first message, not when the trickle stops.
    ///
    /// The stimulus is the point, and a loaded or virtualised machine can fail to deliver it: `usleep(500)` slept
    /// several milliseconds on a hosted macOS runner (the same timer slack that makes the binding's own 2 ms timer
    /// fire after about 10 ms), so two pushes more than `burstGap` apart are not a trickle, and a pull answered
    /// between them is answered correctly. The feeder therefore paces itself by spinning on the clock, records when
    /// each message was pushed, and a trial whose pushes were ever `burstGap` or more apart before the answer is not
    /// a trickle and is repeated, up to forty times; the assertions about the behaviour are made on a trial that was
    /// one. (A machine that never lets the feeder keep a 2 ms cadence for 8 ms fails with that said, not with a
    /// claim about the binding.)
    ///
    /// A trickle the feeder kept is not yet one the binding saw: the binding's pump is a task, and a loaded machine
    /// can hold it up after it took a message, so that the next one reaches the buffer 2 ms later although it was
    /// pushed 0.5 ms later (the likely cause of the one message answered in 1 loaded run of 30). So the times are recorded where the
    /// events happen, not where the test hears of them: the scripted socket records when the pump took each message,
    /// and a trial is judged only if the gap the binding could have seen before its answer was under `burstGap` too
    /// (see `bindingGap` below). The answer's own time, read when the pull's task resumed, is later than the answer
    /// whenever that task was held up, so it serves only as an upper bound.
    func testATrickleIsAnsweredByTheBurstCapNotHeldUntilItStops() async throws {
        let gapNanoseconds: UInt64 = 2_000_000 // burstGap
        var pauses: [Double] = []
        // Forty tries of about 30 ms each: a machine busy enough to stall the feeder in ten of them (a hosted
        // runner did) still trickles in one of forty.
        for _ in 0..<40 {
            let adapter = ScriptedWebSocket()
            let binding = WebSocketBinding(adapter: adapter)
            let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
            let socket = try XCTUnwrap(adapter.sockets.first)
            let pull = Task { () async -> (Result<[WsMessage], WsError>, UInt64) in
                let got = await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 1000) }
                return (got, DispatchTime.now().uptimeNanoseconds)
            }
            try await Task.sleep(nanoseconds: 5_000_000)
            let started = DispatchTime.now().uptimeNanoseconds
            let stop = Locked(false)
            let pushedAt = Locked<[UInt64]>([])
            let feeder = Thread {
                var index = 0
                var due = DispatchTime.now().uptimeNanoseconds
                while !stop.withLock({ $0 }) && index < 20_000 {
                    pushedAt.withLock { $0.append(DispatchTime.now().uptimeNanoseconds) }
                    socket.push(.text("\(index)"))
                    index += 1
                    due += 500_000
                    while DispatchTime.now().uptimeNanoseconds < due, !stop.withLock({ $0 }) {}
                }
            }
            feeder.start()
            let (result, at) = await pull.value
            let got = try result.get()
            stop.withLock { $0 = true }
            binding.detach()
            // The pushes up to the answer, and the one after it: a feeder held up right after its first push
            // leaves a single push before the answer, and its pause only shows in when the next one came (or
            // in the answer's own time, when none did). Without that push the pause went unmeasured, and a
            // one-message answer, correct for a feeder that stalled, was judged as if it had trickled.
            let all = pushedAt.withLock { $0 }
            let before = all.filter { $0 <= at }
            let pushes = before + [all.first(where: { $0 > at }) ?? at]
            let widest = zip(pushes, pushes.dropFirst()).map { $1 - $0 }.max() ?? 0
            // What the binding saw. Message k reached the binding's buffer after the pump took it (`taken[k]`) and before
            // the pump took the next one (`taken[k + 1]`: the pump asks for the next message only once it delivered this
            // one). An answer of `got.count` messages by the burst gap came at least `burstGap` after the last of them
            // reached the buffer, before the one after it did, and no later than `at`; so it can only have happened if
            // `bindingGap`, from the pump taking the last message answered to that bound, is `burstGap` or more. A trial
            // whose `bindingGap` is shorter was answered by the cap or `max`, never by a quiet period.
            let taken = socket.pulledAt
            let last = got.count - 1
            let bound = min(at, taken.count > got.count + 1 ? taken[got.count + 1] : at)
            let bindingGap = last < taken.count && bound > taken[last] ? bound - taken[last] : UInt64.max
            if widest >= gapNanoseconds || bindingGap >= gapNanoseconds {
                pauses.append(Double(max(widest, bindingGap == UInt64.max ? 0 : bindingGap)) / 1e6)
                continue // the feeder or the binding's pump was held up: not a trickle, so nothing to assert about one
            }
            let elapsed = Double(at - started) / 1e6
            XCTAssertGreaterThan(got.count, 1, "the trickle was one burst")
            // The cap answers on the first push 8 ms after the pull's first message: about 17 messages at one per
            // 0.5 ms, counted where the binding answered and so the same on any machine. (A pull held until the
            // trickle stops is answered by `max`, with 1,000.)
            XCTAssertLessThanOrEqual(got.count, 64, "a trickle held the pull for \(got.count) messages (\(elapsed) ms)")
            return
        }
        XCTFail("no trial of 40 was a trickle: the feeder was held up for \(pauses.map { String(format: "%.1f", $0) }) ms between two pushes")
    }

    /// 1b: a second concurrent receive on a URLSession connection is the typed error, and the
    /// first still gets its message.
    func testASecondConcurrentReceiveOnARealConnectionIsTyped() async throws {
        let server = try sharedServer()
        let binding = WebSocketBinding(adapter: URLSessionWebSocketAdapter())
        defer { binding.detach() }
        let conn = try await binding.connect(url: "\(server.ws)/ws/echo", protocols: [], headers: []).conn
        let (refused, first) = await firstOfTwo { await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 16) } }
        XCTAssertEqual(refused, .failure(.protocol("a receive is already pending on connection \(conn)")), "the second receive is refused at once")
        try await binding.send(conn: conn, message: .text("one"))
        let answered = try await first.value.get()
        XCTAssertEqual(answered, [.text("one")])
    }

    /// 1d: `close` racing a pending receive and the platform's terminal error: the receive is
    /// answered exactly once, with `[]` or the terminal, never twice and never left hanging.
    func testCloseRacingTheTerminalAnswersThePendingReceiveExactlyOnce() async throws {
        for round in 0 ..< 300 {
            let adapter = ScriptedWebSocket()
            let binding = WebSocketBinding(adapter: adapter)
            let conn = try await binding.connect(url: "ws://x.test", protocols: [], headers: []).conn
            let socket = adapter.sockets[0]
            let pull = Task { await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 16) } }
            if round % 3 == 0 {
                try await Task.sleep(nanoseconds: 100_000)
            }
            async let closing: Void = { () async throws -> Void in try await binding.close(conn: conn, code: 1000, reason: "") }()
            async let ending: Void = { socket.end(.closed(code: 4000, reason: "peer")) }()
            async let pushing: Void = { if round % 2 == 0 { socket.push(.text("x")) } }()
            _ = try await (closing, ending, pushing)
            let outcome = await pull.value
            switch outcome {
            case .success(let messages):
                XCTAssertTrue(messages == [] || messages == [.text("x")], "round \(round): \(messages)")
            case .failure(let error):
                XCTAssertEqual(error, .closed(code: 4000, reason: "peer"), "round \(round)")
            }
            let after = try await binding.receive(conn: conn, max: 16)
            XCTAssertEqual(after, [], "round \(round): after the core's close the stream ends cleanly")
        }
    }

    /// 1d: `detach` (core shutdown) during a pending receive answers it `[]` once.
    func testShutdownDuringAPendingReceiveAnswersItCleanly() async throws {
        let server = try sharedServer()
        let binding = WebSocketBinding(adapter: URLSessionWebSocketAdapter())
        let conn = try await binding.connect(url: "\(server.ws)/ws/stall", protocols: [], headers: []).conn
        let pull = await running { await capture { () async throws(WsError) -> [WsMessage] in try await binding.receive(conn: conn, max: 16) } }
        binding.detach()
        let answered = try await pull.value.get()
        XCTAssertEqual(answered, [])
        try await server.waitForTheClientsClose("/ws/stall", code: 1001, reason: nil)
    }

    /// 1e: SSE resume sends Last-Event-ID, retry surfaces as retryMs, the body's end is Ended.
    func testSseResumeRetryAndEnd() async throws {
        let server = try sharedServer()
        let binding = SseBinding(adapter: URLSessionSseAdapter())
        defer { binding.detach() }
        let stream = try await binding.open(url: "\(server.http)/sse/feed", headers: [], lastEventId: nil)
        var events: [SseEvent] = []
        var end: SseError?
        while end == nil {
            do {
                events += try await binding.next(stream: stream, max: 16)
            } catch {
                end = error
            }
        }
        XCTAssertEqual(events.first?.retryMs, 1500)
        XCTAssertEqual(end, .ended)
        let resumed = try await binding.open(url: "\(server.http)/sse/feed", headers: [], lastEventId: "1")
        let rest = try await binding.next(stream: resumed, max: 16)
        XCTAssertEqual(rest.first?.id, "2")
        let seen = try await server.last("/sse/feed")
        XCTAssertEqual(seen?.headers["last-event-id"], "1")
    }
}

// MARK: - 2. Db integrity on the real SQLite adapter

/// A raw SQLite connection to a file, outside the adapter (what another process would see).
final class RawSQLite {
    private var db: OpaquePointer?

    init(path: String) throws {
        guard sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK else {
            throw TestFailure(description: "cannot open \(path)")
        }
    }

    deinit {
        close()
    }

    /// Runs `sql` and returns SQLite's result code.
    func exec(_ sql: String) -> Int32 {
        return sqlite3_exec(db, sql, nil, nil, nil)
    }

    /// The first column of the first row of `sql` as an integer.
    func integer(_ sql: String) throws -> Int64 {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else {
            throw TestFailure(description: String(cString: sqlite3_errmsg(db)))
        }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else {
            throw TestFailure(description: "no row for \(sql)")
        }
        return sqlite3_column_int64(statement, 0)
    }

    func close() {
        if let db = db {
            sqlite3_close_v2(db)
            self.db = nil
        }
    }
}

/// The paths of this process's open file descriptors that start with `prefix` (the database
/// file, its `-wal` and `-shm`).
func openDescriptors(under prefix: String) -> [String] {
    // `realpath` of the directory: F_GETPATH answers `/private/var/...` for `/var/...`.
    let directory = (prefix as NSString).deletingLastPathComponent
    let resolved = realpath(directory, nil).map { (pointer: UnsafeMutablePointer<CChar>) -> String in
        defer { free(pointer) }
        return String(cString: pointer) + "/" + (prefix as NSString).lastPathComponent
    } ?? prefix
    let size = proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, nil, 0)
    guard size > 0 else {
        return []
    }
    let stride = MemoryLayout<proc_fdinfo>.stride
    var infos = [proc_fdinfo](repeating: proc_fdinfo(), count: Int(size) / stride + 16)
    let filled = infos.withUnsafeMutableBytes { (raw: UnsafeMutableRawBufferPointer) -> Int32 in
        return proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, raw.baseAddress, Int32(raw.count))
    }
    var found: [String] = []
    for info in infos.prefix(Int(filled) / stride) where info.proc_fdtype == UInt32(PROX_FDTYPE_VNODE) {
        var buffer = [CChar](repeating: 0, count: Int(MAXPATHLEN))
        if fcntl(info.proc_fd, F_GETPATH, &buffer) == 0 {
            let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
            let path = String(decoding: bytes, as: UTF8.self)
            if path.hasPrefix(prefix) || path.hasPrefix(resolved) {
                found.append(path)
            }
        }
    }
    return found
}

final class DbReviewTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        directory = try temporaryDirectory(self)
    }

    private func binding(busyTimeoutMs: UInt32 = 5_000) -> DbBinding {
        return DbBinding(adapter: SQLiteDbAdapter(directory: directory), busyTimeoutMs: busyTimeoutMs)
    }

    private func path(_ name: String) -> String {
        return SQLiteDbAdapter(directory: directory).fileURL(forDatabase: name).path
    }

    /// 2a: an outer statement during a transaction fails `busy` at the busy timeout (not much
    /// earlier, not much later), and the transaction goes on meanwhile (that its statements are not
    /// queued behind the waiting one is the next test's, by order).
    func testAnOuterStatementIsBusyAtTheDeadlineAndTheTransactionKeepsGoing() async throws {
        let db = binding(busyTimeoutMs: 400)
        let id = try await db.open(name: "deadline", migrations: notesMigrations).db
        let tx = try await db.begin(id)
        let started = DispatchTime.now().uptimeNanoseconds
        let reference = referenceSleep(milliseconds: 400) // armed as the statement is: the busy timeout's own clock
        let outer = Task { () async -> (Result<DbExecuted, DbError>, UInt64) in
            let result = await capture { () async throws(DbError) -> DbExecuted in
                try await db.execute(id, "INSERT INTO notes (title) VALUES ('outer')", [])
            }
            return (result, DispatchTime.now().uptimeNanoseconds)
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        for index in 0 ..< 20 {
            _ = try await db.execute(tx, "INSERT INTO notes (title) VALUES (?)", [.text("in tx \(index)")])
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        let (result, ended) = await outer.value
        let waited = Double(ended - started) / 1e6
        guard case .failure(.busy) = result else {
            return XCTFail("expected busy, got \(result)")
        }
        XCTAssertGreaterThanOrEqual(waited, 395, "busy came before the timeout")
        // "Well after the timeout" is not 200 ms of wall clock (a stalled machine made it 600 and more): it is more than half the timeout after a sleep of
        // the timeout's own length that was started beside the statement, which a slow machine ends as late as it ends the binding's.
        let late = (Double(ended) - Double(await reference.value)) / 1e6
        XCTAssertLessThan(late, 200, "busy came \(late) ms after a sleep of the timeout's length started beside the statement")
        try await db.finish(tx, commit: true)
        let rows = try await db.query(id, "SELECT COUNT(*) FROM notes", [])
        XCTAssertEqual(rows.rows, [[.integer(20)]], "the outer statement never ran")
    }

    /// 2a: the transaction's own statements are not queued behind an outer statement that waits for
    /// the transaction. Shown by order, with a busy timeout that never elapses here (60 s, a hang
    /// detector): the outer statement is still waiting after each of them, on any machine. Queued
    /// behind it, the first would wait for it and it for the transaction, until its timeout. (The
    /// test above bounded the slowest of them by 200 ms of wall clock, which a machine that stalled
    /// that long failed; the TypeScript runtime's test of the same claim is by order too.)
    func testTheTransactionsOwnStatementsAreNotQueuedBehindAWaitingOuterStatement() async throws {
        let db = binding(busyTimeoutMs: 60_000)
        let id = try await db.open(name: "queue", migrations: notesMigrations).db
        let tx = try await db.begin(id)
        let started = Locked(false)
        let settled = Locked(false)
        let outer = Task { () async -> Result<DbExecuted, DbError> in
            started.withLock { $0 = true }
            let result = await capture { () async throws(DbError) -> DbExecuted in
                try await db.execute(id, "INSERT INTO notes (title) VALUES ('outer')", [])
            }
            settled.withLock { $0 = true }
            return result
        }
        // The outer statement's task registers it as a waiter as soon as it runs, without suspending first. Waited for, so that
        // the statements below come after it (a task that had not run would make them prove nothing, never fail).
        await eventually("the outer statement's task to run") { started.withLock { $0 } }
        for index in 0 ..< 5 {
            _ = try await db.execute(tx, "INSERT INTO notes (title) VALUES (?)", [.text("in tx \(index)")])
            XCTAssertFalse(settled.withLock { $0 }, "statement \(index) of the transaction waited for the outer statement")
        }
        try await db.finish(tx, commit: true)
        let executed = try await outer.value.get()
        XCTAssertEqual(executed.lastInsertId, 6, "the outer statement ran once the transaction ended")
        let rows = try await db.query(id, "SELECT COUNT(*) FROM notes", [])
        XCTAssertEqual(rows.rows, [[.integer(6)]])
    }

    /// 2b: a transaction left open when the core shuts down (the binding detaches) is rolled back
    /// and the connection closed: another connection can take the write lock, the uncommitted rows
    /// are gone and no file descriptor of the database stays open.
    func testShutdownWithAnOpenTransactionLeavesNothingDangling() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport, adapters: Adapters([SQLiteDbAdapter(directory: directory)]))
        func call(_ method: UInt32, _ args: [UInt8]) async throws -> Wire.PortReply {
            let before = transport.portReplies.count
            _ = transport.callPort(portId: StandardPorts.Db.portId, methodId: method, portCallId: UInt32(before + 1), args: args)
            guard await waitUntil({ transport.portReplies.count == before + 1 }) else {
                throw TestFailure(description: "no reply")
            }
            return transport.portReplies[before]
        }
        let opened = try DbOpened.undraDecoded(from: Array(try await call(StandardPorts.Db.open, encodeArgs { w in
            w.writeString("dangle")
            notesMigrations.undraEncode(&w)
        }).body))
        _ = try await call(StandardPorts.Db.execute, encodeArgs { w in
            w.writeU32(opened.db)
            w.writeString("INSERT INTO notes (title) VALUES ('committed')")
            [DbValue]().undraEncode(&w)
        })
        let tx = try UInt32.undraDecoded(from: Array(try await call(StandardPorts.Db.begin, encodeArgs { w in w.writeU32(opened.db) }).body))
        for index in 0 ..< 3 {
            let reply = try await call(StandardPorts.Db.execute, encodeArgs { w in
                w.writeU32(tx)
                w.writeString("INSERT INTO notes (title) VALUES (?)")
                [DbValue.text("uncommitted \(index)")].undraEncode(&w)
            })
            XCTAssertEqual(reply.status, .ok)
        }
        core.shutdown()
        // Another connection (busy timeout 0) gets the write lock once the rollback ran.
        let raw = try RawSQLite(path: path("dangle"))
        var code = raw.exec("BEGIN IMMEDIATE")
        var attempts = 0
        while code == SQLITE_BUSY && attempts < 200 {
            attempts += 1
            usleep(5_000)
            code = raw.exec("BEGIN IMMEDIATE")
        }
        XCTAssertEqual(code, SQLITE_OK, "the transaction was left open")
        XCTAssertEqual(try raw.integer("SELECT COUNT(*) FROM notes"), 1, "only the committed row survives")
        XCTAssertEqual(raw.exec("ROLLBACK"), SQLITE_OK)
        XCTAssertEqual(try raw.integer("PRAGMA user_version"), 2)
        // The adapter's connection is closed: no descriptor of this process still names the file
        // (Apple's SQLite keeps the WAL file after the last close, so its absence proves nothing).
        let file = path("dangle")
        XCTAssertFalse(openDescriptors(under: file).isEmpty, "the check sees an open connection")
        raw.close()
        await eventually("the adapter's connection to close") { openDescriptors(under: file).isEmpty }
        XCTAssertEqual(openDescriptors(under: file), [], "the adapter's connection was left open")
        // A fresh adapter opens it and migrates nothing.
        let fresh = binding(busyTimeoutMs: 100)
        let again = try await fresh.open(name: "dangle", migrations: notesMigrations)
        XCTAssertEqual(again.version, 2)
        let tx2 = try await fresh.begin(again.db)
        try await fresh.finish(tx2, commit: true)
    }

    /// 2c: a failing migration on a database that already has data leaves `user_version`, the
    /// schema and the rows exactly as they were.
    func testAFailingMigrationOnAnExistingDatabaseChangesNothing() async throws {
        let db = binding()
        let first = try await db.open(name: "existing", migrations: notesMigrations).db
        _ = try await db.execute(first, "INSERT INTO notes (title) VALUES ('kept')", [])
        try await db.close(first)
        let broken = notesMigrations + [
            DbMigration(version: 3, sql: "CREATE TABLE extra (x); ALTER TABLE notes ADD COLUMN body TEXT"),
            DbMigration(version: 4, sql: "INSERT INTO notes (title) VALUES (NULL)"),
        ]
        do {
            _ = try await db.open(name: "existing", migrations: broken)
            XCTFail("migration 4 fails")
        } catch {
            guard case .migration(let version, _) = error else {
                return XCTFail("\(error)")
            }
            XCTAssertEqual(version, 4)
        }
        let raw = try RawSQLite(path: path("existing"))
        XCTAssertEqual(try raw.integer("PRAGMA user_version"), 2)
        XCTAssertEqual(try raw.integer("SELECT COUNT(*) FROM sqlite_master WHERE name = 'extra'"), 0)
        XCTAssertEqual(try raw.integer("SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name = 'body'"), 0)
        XCTAssertEqual(try raw.integer("SELECT COUNT(*) FROM notes"), 1)
        raw.close()
        await expectThrows(DbError.migration(version: 2, message: "the database is at version 2, newer than the newest migration (1)")) { () async throws(DbError) -> DbOpened in
            try await db.open(name: "existing", migrations: [notesMigrations[0]])
        }
    }

    /// 2c: a migration version above SQLite's `user_version` range (a signed 32-bit integer,
    /// which records a larger value as 0) is refused before anything runs; the largest one that
    /// fits is kept exactly.
    func testAVersionBeyondUserVersionsRangeIsRefusedAndTheLargestFittingOneIsKept() async throws {
        let db = binding()
        let wide = [DbMigration(version: 1, sql: "CREATE TABLE a (x)"), DbMigration(version: 3_000_000_000, sql: "CREATE TABLE b (y)")]
        await expectThrows(DbError.migration(
            version: 3_000_000_000,
            message: "migration versions must be at most 2147483647: SQLite keeps the version in a signed 32-bit integer (PRAGMA user_version)"
        )) { () async throws(DbError) -> DbOpened in
            try await db.open(name: "wide", migrations: wide)
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: path("wide")), "nothing was opened or migrated")
        let largest = [DbMigration(version: 1, sql: "CREATE TABLE a (x)"), DbMigration(version: 2_147_483_647, sql: "CREATE TABLE b (y)")]
        let first = try await db.open(name: "wide", migrations: largest)
        XCTAssertEqual(first.version, 2_147_483_647)
        try await db.close(first.db)
        let again = try await db.open(name: "wide", migrations: largest)
        XCTAssertEqual(again.version, 2_147_483_647, "the version read back is the version written")
    }

    /// 2c: opens of one new database at the same time (two stores of a core, an app and its
    /// extension) all succeed and migrate it once: the version is read again under the write lock,
    /// and the switch to WAL (which SQLite answers BUSY at once, without its busy handler, while
    /// another connection makes it) waits like any other lock.
    func testConcurrentOpensOfOneNewDatabaseAllSucceedAndMigrateOnce() async throws {
        let slow = notesMigrations + [DbMigration(version: 3, sql: "CREATE TABLE extra (x); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) INSERT INTO extra SELECT i FROM n")]
        for round in 0 ..< 10 {
            let name = "shared\(round)"
            let one = binding()
            let other = binding()
            async let a = capture { () async throws(DbError) -> DbOpened in try await one.open(name: name, migrations: slow) }
            async let b = capture { () async throws(DbError) -> DbOpened in try await other.open(name: name, migrations: slow) }
            async let c = capture { () async throws(DbError) -> DbOpened in try await one.open(name: name, migrations: slow) }
            for result in await [a, b, c] {
                guard case .success(let opened) = result else {
                    return XCTFail("round \(round): a concurrent open failed: \(result)")
                }
                XCTAssertEqual(opened.version, 3)
            }
            let raw = try RawSQLite(path: path(name))
            XCTAssertEqual(try raw.integer("SELECT COUNT(*) FROM extra"), 100_000, "round \(round): migration 3 ran once")
            raw.close()
        }
    }

    /// 2a: the switch to WAL is retried while SQLite answers BUSY, until the busy timeout.
    func testTheSwitchToWalWaitsForTheBusyTimeoutThenIsBusy() async throws {
        let recorder = RecordingDb(SQLiteDbAdapter(directory: directory))
        let db = DbBinding(adapter: recorder, busyTimeoutMs: 300)
        recorder.fail("PRAGMA journal_mode = WAL", with: .busy)
        Task {
            try await Task.sleep(nanoseconds: 60_000_000)
            recorder.fail("PRAGMA journal_mode = WAL", with: nil)
        }
        let started = Date()
        let opened = try await db.open(name: "wal", migrations: notesMigrations)
        XCTAssertEqual(opened.version, 2)
        XCTAssertGreaterThanOrEqual(Date().timeIntervalSince(started), 0.055, "the open waited for the lock")
        recorder.fail("PRAGMA journal_mode = WAL", with: .busy)
        let again = Date()
        let reference = referenceSleep(milliseconds: 300) // the busy timeout's own length, armed as the open is
        await expectThrows(DbError.busy) { () async throws(DbError) -> DbOpened in
            try await db.open(name: "wal", migrations: notesMigrations)
        }
        let ended = DispatchTime.now().uptimeNanoseconds
        let waited = Date().timeIntervalSince(again)
        XCTAssertGreaterThanOrEqual(waited, 0.29, "busy before the busy timeout")
        // As above: not a wall-clock ceiling, but half the timeout after a sleep of its length started beside the open.
        let late = (Double(ended) - Double(await reference.value)) / 1e6
        XCTAssertLessThan(late, 150, "busy came \(late) ms after a sleep of the timeout's length started beside the open")
        XCTAssertEqual(recorder.ran(2).last, "close", "the connection of a failed open is closed")
    }

    /// 2d: parameters are bound, never interpolated: SQL in a value is data.
    func testSqlInAParameterIsData() async throws {
        let db = binding()
        let id = try await db.open(name: "inject", migrations: [DbMigration(version: 1, sql: "CREATE TABLE t (v TEXT)")]).db
        let payloads = ["'; DROP TABLE t; --", "x'); DROP TABLE t; --", "\"; DELETE FROM t; --", "?", "?1", ":name", "\\'"]
        for payload in payloads {
            _ = try await db.execute(id, "INSERT INTO t (v) VALUES (?)", [.text(payload)])
        }
        let rows = try await db.query(id, "SELECT v FROM t WHERE v = ? OR 1 = 0 ORDER BY rowid", [.text(payloads[0])])
        XCTAssertEqual(rows.rows, [[.text(payloads[0])]])
        let all = try await db.query(id, "SELECT v FROM t ORDER BY rowid", [])
        XCTAssertEqual(all.rows, payloads.map { [DbValue.text($0)] })
        let tables = try await db.query(id, "SELECT name FROM sqlite_master WHERE type = 'table'", [])
        XCTAssertEqual(tables.rows, [[.text("t")]])
    }

    /// 2e: every value round-trips: text with U+0000 inside, blobs of NUL bytes, an empty blob
    /// (not NULL), an empty text (not NULL), i64 bounds, infinities, and a 2 MB blob row.
    func testEveryValueRoundTripsIncludingNulsAndA2MBRow() async throws {
        let db = binding()
        let id = try await db.open(name: "cells", migrations: [DbMigration(version: 1, sql: "CREATE TABLE c (v)")]).db
        var big = [UInt8](repeating: 0, count: 2 * 1024 * 1024)
        for index in big.indices where index % 7 == 0 {
            big[index] = UInt8(index % 251)
        }
        let values: [DbValue] = [
            .text("\u{0}"), .text("\u{0}\u{0}a\u{0}"), .text("tail\u{0}"), .text(""),
            .blob([0]), .blob([0, 0, 0]), .blob([]), .null,
            .integer(.min), .integer(.max), .integer(-1),
            .real(.infinity), .real(-.infinity), .real(.leastNonzeroMagnitude), .real(.greatestFiniteMagnitude),
            .blob(big), .text(String(repeating: "é\u{0}😀", count: 100_000)),
        ]
        for value in values {
            _ = try await db.execute(id, "INSERT INTO c (v) VALUES (?)", [value])
        }
        let rows = try await db.query(id, "SELECT v, typeof(v), length(CAST(v AS BLOB)) FROM c ORDER BY rowid", [])
        XCTAssertEqual(rows.rows.count, values.count)
        for (index, value) in values.enumerated() {
            XCTAssertEqual(rows.rows[index][0], value, "value \(index)")
            XCTAssertEqual(rows.rows[index][1], .text(value.storageClass), "storage class of value \(index)")
        }
        // The empty blob is a zero-length blob, not NULL; the empty text likewise.
        let empty = try await db.query(id, "SELECT COUNT(*) FROM c WHERE v IS NULL", [])
        XCTAssertEqual(empty.rows, [[.integer(1)]])
        // A blob of NULs is found by an equal blob parameter.
        let found = try await db.query(id, "SELECT COUNT(*) FROM c WHERE v = ?", [.blob([0, 0, 0])])
        XCTAssertEqual(found.rows, [[.integer(1)]])
        let foundText = try await db.query(id, "SELECT COUNT(*) FROM c WHERE v = ?", [.text("\u{0}\u{0}a\u{0}")])
        XCTAssertEqual(foundText.rows, [[.integer(1)]])
    }

    /// 2f: the error is chosen by the result code, never the text: a trigger that raises a
    /// message reading like a UNIQUE failure is `other`; text that reads "database is locked" in a
    /// CHECK is `check`.
    func testErrorsAreTypedByCodeNotByTheirText() async throws {
        let db = binding()
        let id = try await db.open(name: "codes", migrations: [DbMigration(version: 1, sql: """
            CREATE TABLE t (v INTEGER CONSTRAINT "database is locked" CHECK (v > 0));
            CREATE TRIGGER fake BEFORE INSERT ON t WHEN NEW.v = 7
                BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: t.v'); END;
            CREATE TRIGGER disk BEFORE INSERT ON t WHEN NEW.v = 8
                BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;
            """)]).db
        do {
            _ = try await db.execute(id, "INSERT INTO t (v) VALUES (7)", [])
            XCTFail("the trigger aborts")
        } catch {
            guard case .constraint(.other, _) = error else {
                return XCTFail("a RAISE that reads like UNIQUE is still SQLITE_CONSTRAINT_TRIGGER: \(error)")
            }
        }
        do {
            _ = try await db.execute(id, "INSERT INTO t (v) VALUES (8)", [])
            XCTFail("the trigger aborts")
        } catch {
            guard case .constraint(.other, _) = error else {
                return XCTFail("\(error)")
            }
        }
        do {
            _ = try await db.execute(id, "INSERT INTO t (v) VALUES (0)", [])
            XCTFail("the check fails")
        } catch {
            guard case .constraint(.check, _) = error else {
                return XCTFail("\(error)")
            }
        }
        // The mapping itself, with messages that lie.
        XCTAssertEqual(SQLiteConnection.error(code: 2067, message: "NOT NULL constraint failed"), .constraint(kind: .unique, message: "NOT NULL constraint failed"))
        XCTAssertEqual(SQLiteConnection.error(code: 1, message: "database is locked"), .sql(message: "database is locked"))
        XCTAssertEqual(SQLiteConnection.error(code: 517, message: "x"), .busy) // SQLITE_BUSY_SNAPSHOT
        XCTAssertEqual(SQLiteConnection.error(code: 262, message: "x"), .busy) // SQLITE_LOCKED_SHAREDCACHE
        XCTAssertEqual(SQLiteConnection.error(code: 13, message: "x"), .full)
        XCTAssertEqual(SQLiteConnection.error(code: 266, message: "disk I/O error"), .unavailable("disk I/O error")) // IOERR_READ
    }
}

// MARK: - 3. Typed ends of the default adapters

final class RealtimeEndsReviewTests: XCTestCase {
    func testWebSocketEnds() async throws {
        let server = try sharedServer()
        let binding = WebSocketBinding(adapter: URLSessionWebSocketAdapter())
        defer { binding.detach() }
        do {
            _ = try await binding.connect(url: "\(server.ws)/ws/deny?status=401", protocols: [], headers: [])
            XCTFail("refused")
        } catch {
            guard case .refused(let status, _) = error else {
                return XCTFail("\(error)")
            }
            XCTAssertEqual(status, 401)
        }
        do {
            _ = try await binding.connect(url: "ws://nonexistent.invalid/", protocols: [], headers: [])
            XCTFail("no such host")
        } catch {
            guard case .network = error else {
                return XCTFail("a DNS failure is a network error (ADR-047 §5, the brief's URLError rule): \(error)")
            }
        }
        let conn = try await binding.connect(url: "\(server.ws)/ws/close?code=1000&reason=", protocols: [], headers: []).conn
        let hello = try await binding.receive(conn: conn, max: 16)
        XCTAssertEqual(hello, [.text("hello")])
        await expectThrows(WsError.closed(code: 1000, reason: "")) { () async throws(WsError) -> [WsMessage] in
            try await binding.receive(conn: conn, max: 16)
        }
        let dropped = try await binding.connect(url: "\(server.ws)/ws/drop", protocols: [], headers: []).conn
        _ = try await binding.receive(conn: dropped, max: 16)
        do {
            _ = try await binding.receive(conn: dropped, max: 16)
            XCTFail("dropped")
        } catch {
            guard case .network = error else {
                return XCTFail("\(error)")
            }
        }
        let bad = try await binding.connect(url: "\(server.ws)/ws/bad-utf8", protocols: [], headers: []).conn
        do {
            _ = try await binding.receive(conn: bad, max: 16)
            XCTFail("bad UTF-8")
        } catch {
            guard case .protocol = error else {
                return XCTFail("\(error)")
            }
        }
    }

    func testSseEnds() async throws {
        let server = try sharedServer()
        let binding = SseBinding(adapter: URLSessionSseAdapter())
        defer { binding.detach() }
        do {
            _ = try await binding.open(url: "\(server.http)/sse/status?code=401", headers: [], lastEventId: nil)
            XCTFail("refused")
        } catch {
            guard case .refused(let status, _) = error else {
                return XCTFail("\(error)")
            }
            XCTAssertEqual(status, 401)
        }
        do {
            _ = try await binding.open(url: "http://nonexistent.invalid/", headers: [], lastEventId: nil)
            XCTFail("no such host")
        } catch {
            guard case .network = error else {
                return XCTFail("a DNS failure is a network error: \(error)")
            }
        }
    }

    /// More long-lived streams to one host than URLSession's default per-host connection limit
    /// (6 for HTTP/1.1): every one opens, and the server sees each leave when it is closed.
    func testMoreThanSixStreamsToOneHostAllOpen() async throws {
        let server = try sharedServer()
        let binding = SseBinding(adapter: URLSessionSseAdapter())
        defer { binding.detach() }
        var streams: [UInt32] = []
        for index in 0 ..< 12 {
            let opening = Task { await capture { () async throws(SseError) -> UInt32 in
                try await binding.open(url: "\(server.http)/sse/hang?n=\(index)", headers: [], lastEventId: nil)
            } }
            let timeout = Task {
                try await Task.sleep(nanoseconds: UInt64(hangDeadline * 1_000_000_000))
                opening.cancel()
            }
            let result = await opening.value
            timeout.cancel()
            guard case .success(let stream) = result else {
                return XCTFail("stream \(index) did not open: \(result)")
            }
            streams.append(stream)
        }
        XCTAssertEqual(streams.count, 12)
        for stream in streams {
            try await binding.close(stream: stream)
        }
        let deadline = Date().addingTimeInterval(hangDeadline)
        var hanging: [RealtimeServer.Connection] = []
        repeat {
            hanging = Array(try await server.connections().filter { $0.path == "/sse/hang" }.suffix(12))
            if hanging.allSatisfy(\.clientClosed) {
                break
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        } while Date() < deadline
        XCTAssertEqual(hanging.count, 12)
        XCTAssertTrue(hanging.allSatisfy(\.clientClosed), "the server saw every stream leave")
    }
}

// MARK: - 4. The bridge's answers after the persistence merge

final class PortsV2BridgeReviewTests: XCTestCase {
    private func call(_ transport: FakeTransport, _ portId: UInt32, _ method: UInt32, _ args: [UInt8]) async throws -> Wire.PortReply {
        let before = transport.portReplies.count
        let outcome = transport.callPort(portId: portId, methodId: method, portCallId: UInt32(before + 1), args: args)
        XCTAssertEqual(outcome, .async)
        guard await waitUntil({ transport.portReplies.count == before + 1 }) else {
            throw TestFailure(description: "no reply")
        }
        return transport.portReplies[before]
    }

    func testTypedErrorsAnswerStatus1AndMalformedArgumentsStatus2() async throws {
        let log = LogRecorder(self)
        let directory = try temporaryDirectory(self)
        let transport = FakeTransport()
        let core = try makeCore(transport, adapters: Adapters([
            WebSocketPortAdapter(ScriptedWebSocket()),
            SsePortAdapter(URLSessionSseAdapter()),
            SQLiteDbAdapter(directory: directory),
        ]))
        defer { core.shutdown() }
        // Typed: status 1 with the encoded error, no ERROR log.
        let ws = try await call(transport, StandardPorts.WebSocket.portId, StandardPorts.WebSocket.receive, encodeArgs { w in
            w.writeU32(9)
            w.writeU32(16)
        })
        XCTAssertEqual(ws.status, .error)
        XCTAssertEqual(try WsError.undraDecoded(from: Array(ws.body)), .network("no WebSocket connection 9"))
        let sse = try await call(transport, StandardPorts.Sse.portId, StandardPorts.Sse.open, encodeArgs { w in
            w.writeString("ftp://x")
            [Header]().undraEncode(&w)
            Optional<String>.none.undraEncode(&w)
        })
        XCTAssertEqual(sse.status, .error)
        XCTAssertEqual(try SseError.undraDecoded(from: Array(sse.body)), .refused(status: nil, message: "invalid URL: ftp://x"))
        let db = try await call(transport, StandardPorts.Db.portId, StandardPorts.Db.query, encodeArgs { w in
            w.writeU32(5)
            w.writeString("SELECT 1")
            [DbValue]().undraEncode(&w)
        })
        XCTAssertEqual(db.status, .error)
        XCTAssertEqual(try DbError.undraDecoded(from: Array(db.body)), .unavailable("no open database or transaction 5"))
        XCTAssertEqual(log.errors(containing: "WebSocket"), [])
        XCTAssertEqual(log.errors(containing: "Sse"), [])
        XCTAssertEqual(log.errors(containing: "Db"), [])
        // Malformed arguments: status 2 and an ERROR naming the method.
        for (port, method, name) in [
            (StandardPorts.WebSocket.portId, StandardPorts.WebSocket.send, "WebSocket.send"),
            (StandardPorts.Sse.portId, StandardPorts.Sse.next, "Sse.next"),
            (StandardPorts.Db.portId, StandardPorts.Db.execute, "Db.execute"),
        ] {
            let reply = try await call(transport, port, method, [1])
            XCTAssertEqual(reply, Wire.PortReply(portCallId: reply.portCallId, status: .unavailable), name)
            XCTAssertEqual(log.errors(containing: name).count, 1, name)
        }
    }
}
