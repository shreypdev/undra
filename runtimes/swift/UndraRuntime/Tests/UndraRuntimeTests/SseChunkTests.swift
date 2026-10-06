import Darwin
import Foundation
import XCTest
@testable import UndraRuntime

// The default Sse adapter reads the response body in chunks (ADR-047, "Amendment: the Swift adapter
// delivers chunks, 2026-10-02"): what a chunk boundary may fall in the middle of, and that the read-ahead
// still stops when the core stops pulling. How fast it reads is SseThroughputTests.swift.

// MARK: - Helpers

/// What a stream asked of its request, in order.
final class RecordingControl: SseTaskControl, @unchecked Sendable {
    enum Call: Equatable {
        case suspend
        case resume
        case cancel
    }

    private let log = Locked<[Call]>([])

    var calls: [Call] {
        return log.withLock { $0 }
    }

    func suspend() {
        log.withLock { $0.append(.suspend) }
    }

    func resume() {
        log.withLock { $0.append(.resume) }
    }

    func cancel() {
        log.withLock { $0.append(.cancel) }
    }
}

/// `count` events starting at id `from`, as a server writes them: `id: <i>\ndata: <i>\n\n`.
private func eventBytes(_ range: Range<Int>) -> [UInt8] {
    return Array(range.map { "id: \($0)\ndata: \($0)\n\n" }.joined().utf8)
}

/// The events ``eventBytes(_:)`` is the bytes of.
private func expectedEvents(_ range: Range<Int>) -> [SseEvent] {
    return range.map { SseEvent(id: "\($0)", event: "message", data: "\($0)") }
}

/// A hang detector for a test that waits on a stream: after `seconds` without progress it runs `onHang`, which ends the wait, so the
/// test fails by its assertions (and by `fired`) instead of hanging the suite. The default is several times ``hangDeadline`` (the
/// longest one wait of the test may take), so a test that makes a few waits one after the other cannot be ended by the detector while
/// each wait is still within its own deadline; a test that makes many (one per byte) calls ``kick()`` after each, so the detector
/// measures the time since the last step, never the length of the whole test. Cancel it when the test is done.
final class HangDetector: @unchecked Sendable {
    private let hung = Locked(false)
    private let task = Locked<Task<Void, Never>?>(nil)
    private let seconds: Double
    private let onHang: @Sendable () async -> Void

    init(seconds: Double = 5 * hangDeadline, _ onHang: @escaping @Sendable () async -> Void) {
        self.seconds = seconds
        self.onHang = onHang
        kick()
    }

    /// Restarts the countdown: the test made progress.
    func kick() {
        let hung = self.hung
        let seconds = self.seconds
        let onHang = self.onHang
        let next = Task {
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            if !Task.isCancelled {
                hung.withLock { $0 = true }
                await onHang()
            }
        }
        task.withLock { current in
            current?.cancel()
            current = next
        }
    }

    /// Whether the wait lasted until the detector ended it.
    var fired: Bool {
        return hung.withLock { $0 }
    }

    func cancel() {
        task.withLock { $0?.cancel() }
    }
}

/// Reads a stream to its end: the events, and what ended it (`nil` when the stream finished because it was closed).
private func drain(_ stream: any SseStream) async -> (events: [SseEvent], end: SseError?) {
    var all: [SseEvent] = []
    do {
        for try await event in stream.events {
            all.append(event)
        }
        return (all, nil)
    } catch {
        return (all, error as? SseError)
    }
}

// MARK: - A chunk may end anywhere

/// What the body of an event stream is made of that a chunk boundary must not break: a byte order mark, a comment, CR LF, a
/// lone LF and a lone CR as line ends, multi-byte characters of two, three and four bytes, a `retry` and ids that persist,
/// an event with an empty `data`.
private let awkwardBody: [UInt8] = {
    let bom: [UInt8] = [0xEF, 0xBB, 0xBF]
    let text = ": a comment\r\n"
        + "retry: 1500\r\n"
        + "id: 1\r\n"
        + "data: h\u{E9}llo w\u{F6}rld\r\n"
        + "data: \u{65E5}\u{672C}\u{8A9E} \u{1F600}\r\n"
        + "\r\n"
        + "event: tick\n"
        + "id: 2\n"
        + "data: lf only\n"
        + "\n"
        + "data: cr only\r\r"
        + "id: 3\r"
        + "data:\r\r"
        + "data: last\r\n\r\n"
    return bom + Array(text.utf8)
}()

private let awkwardEvents: [SseEvent] = [
    SseEvent(id: "1", event: "message", data: "h\u{E9}llo w\u{F6}rld\n\u{65E5}\u{672C}\u{8A9E} \u{1F600}", retryMs: 1500),
    SseEvent(id: "2", event: "tick", data: "lf only"),
    SseEvent(id: "2", event: "message", data: "cr only"),
    SseEvent(id: "3", event: "message", data: ""),
    SseEvent(id: "3", event: "message", data: "last"),
]

/// The adapter's stream fed chunk by chunk, as URLSession's delegate feeds it: the parser is incremental, and the stream hands
/// it each chunk once, whatever the chunk ends in.
final class SseChunkBoundaryTests: XCTestCase {
    /// `body` delivered as `chunks`, then the end of the body; every event and what ended the stream.
    private func read(_ chunks: [[UInt8]]) async -> (events: [SseEvent], end: SseError?) {
        let stream = URLSessionSseStream(lastEventId: nil, room: 1_000_000, control: RecordingControl())
        for chunk in chunks {
            stream.receive(Data(chunk))
        }
        stream.finish(nil)
        return await drain(stream)
    }

    func testTheBodyAsOneChunkIsTheEventsTheStandardSays() async {
        let (events, end) = await read([awkwardBody])
        XCTAssertEqual(events, awkwardEvents)
        XCTAssertEqual(end, .ended)
    }

    /// Two chunks, cut at every byte (inside the byte order mark, between CR and LF, inside a character of two, three and
    /// four bytes, inside a field name, between the last line's end and the blank line).
    func testACutAtEveryByteParsesAsTheWhole() async {
        for cut in 0 ... awkwardBody.count {
            let (events, end) = await read([Array(awkwardBody[..<cut]), Array(awkwardBody[cut...])])
            XCTAssertEqual(events, awkwardEvents, "cut after byte \(cut)")
            XCTAssertEqual(end, .ended, "cut after byte \(cut)")
        }
    }

    /// One byte to a chunk: what the stream looked like when every byte was its own step.
    func testOneByteAtATimeParsesAsTheWhole() async {
        let (events, end) = await read(awkwardBody.map { [$0] })
        XCTAssertEqual(events, awkwardEvents)
        XCTAssertEqual(end, .ended)
    }

    /// Three chunks, the cuts chosen by a generator with a fixed seed (the same cuts on every run).
    func testSeededCutsIntoThreeChunksParseAsTheWhole() async {
        var seed: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next(_ bound: Int) -> Int {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int((seed >> 33) % UInt64(bound + 1))
        }
        for round in 0 ..< 300 {
            let first = next(awkwardBody.count)
            let second = first + next(awkwardBody.count - first)
            let chunks = [Array(awkwardBody[..<first]), Array(awkwardBody[first ..< second]), Array(awkwardBody[second...])]
            let (events, end) = await read(chunks)
            XCTAssertEqual(events, awkwardEvents, "round \(round): cuts at \(first) and \(second)")
            XCTAssertEqual(end, .ended, "round \(round)")
        }
    }

    /// A body that resumes from an id: the stream's parser starts from `Last-Event-ID`, chunk boundaries or not.
    func testAResumedStreamCarriesTheLastEventIdAcrossChunks() async {
        let stream = URLSessionSseStream(lastEventId: "7", room: 1_000_000, control: RecordingControl())
        for byte in Array("data: a\n\nid: 8\ndata: b\n\n".utf8) {
            stream.receive(Data([byte]))
        }
        stream.finish(nil)
        let (events, end) = await drain(stream)
        XCTAssertEqual(events, [SseEvent(id: "7", event: "message", data: "a"), SseEvent(id: "8", event: "message", data: "b")])
        XCTAssertEqual(end, .ended)
    }

    /// Bytes that are not UTF-8 end the stream after the events before them (a reader that feeds the parser a byte at a time
    /// delivers those events first, and so must one that feeds it a chunk), and the request is cancelled.
    func testBytesThatAreNotUtf8EndTheStreamAfterTheEventsBeforeThem() async {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 1_000_000, control: control)
        stream.receive(Data(Array("data: a\n\ndata: b\n\n".utf8) + [0xFF, 0xFE, 0x0A] + Array("data: never\n\n".utf8)))
        // What URLSession may still hand over before the cancel takes effect is ignored.
        stream.receive(Data(Array("data: late\n\n".utf8)))
        let (events, end) = await drain(stream)
        XCTAssertEqual(events.map(\.data), ["a", "b"])
        XCTAssertEqual(end, .protocol("the event stream is not UTF-8"))
        XCTAssertTrue(control.calls.contains(.cancel), "\(control.calls)")
    }

    /// A character cut short by the end of its line is not UTF-8 either, wherever the chunks were cut.
    func testACharacterThatIsCutShortIsNotUtf8() async {
        for cut in 0 ... 4 {
            let body: [UInt8] = Array("data: ".utf8) + [0xE6, 0x97] + [0x0A]
            let (events, end) = await read([Array(body[..<min(cut + 6, body.count)]), Array(body[min(cut + 6, body.count)...])])
            XCTAssertEqual(events, [])
            XCTAssertEqual(end, .protocol("the event stream is not UTF-8"), "cut \(cut)")
        }
    }

    /// The body's end and a failure come after the events that were queued, not before them.
    func testTheEndsComeAfterTheQueuedEvents() async {
        let ended = URLSessionSseStream(lastEventId: nil, room: 1_000_000, control: RecordingControl())
        ended.receive(Data(eventBytes(0 ..< 3)))
        ended.finish(nil)
        let first = await drain(ended)
        XCTAssertEqual(first.events, expectedEvents(0 ..< 3))
        XCTAssertEqual(first.end, .ended)

        let failed = URLSessionSseStream(lastEventId: nil, room: 1_000_000, control: RecordingControl())
        failed.receive(Data(eventBytes(0 ..< 3)))
        failed.finish(URLError(.networkConnectionLost))
        let second = await drain(failed)
        XCTAssertEqual(second.events, expectedEvents(0 ..< 3))
        XCTAssertEqual(second.end, .network(URLError(.networkConnectionLost).localizedDescription))
    }

    /// The pull that waits for a chunk is answered by the chunk, and by the end.
    func testAPullThatWaitsIsAnsweredByAChunkAndByTheEnd() async throws {
        let stream = URLSessionSseStream(lastEventId: nil, room: 1_000_000, control: RecordingControl())
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        let waiting = Task { () async throws -> SseEvent? in
            var iterator = stream.events.makeAsyncIterator()
            return try await iterator.next()
        }
        stream.receive(Data(eventBytes(5 ..< 6)))
        let first = try await waiting.value
        XCTAssertEqual(first, expectedEvents(5 ..< 6)[0])
        XCTAssertFalse(detector.fired, "the pull was not answered by the chunk")

        let ending = Task { () async -> SseError? in
            var iterator = stream.events.makeAsyncIterator()
            do {
                _ = try await iterator.next()
                return nil
            } catch {
                return error as? SseError
            }
        }
        stream.finish(nil)
        let end = await ending.value
        XCTAssertEqual(end, .ended)
        XCTAssertFalse(detector.fired, "the pull was not answered by the end")
    }
}

// MARK: - How many events a chunk can complete

/// ``SseParser/mostEvents(in:upTo:)``, what the stream decides on whether a chunk is parsed with the task suspended: never fewer
/// than the events the bytes complete, whatever the bytes before them left in the parser.
final class SseParserBoundTests: XCTestCase {
    private func most(_ bytes: [UInt8], upTo limit: Int = Int.max) -> Int {
        return bytes.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Int in
            return SseParser.mostEvents(in: raw, upTo: limit)
        }
    }

    /// Over a seeded run of bodies made of the pieces an event stream has (fields, comments, every line end, unknown fields,
    /// bare text), cut once anywhere: the second piece completes no more events than the bound says, with any limit.
    func testTheBoundIsNeverBelowWhatTheBytesComplete() throws {
        var seed: UInt64 = 0x5EED_0000_2026_1006
        func next(_ bound: Int) -> Int {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int((seed >> 33) % UInt64(bound + 1))
        }
        let pieces = ["data", "data: x", "data:", "id: 1", "event: e", "retry: 9", ":", ": c", "x", "\n", "\n", "\r", "\r\n", "\r\r", "\n\n"]
        for round in 0 ..< 3_000 {
            var body: [UInt8] = []
            for _ in 0 ..< next(40) {
                body += Array(pieces[next(pieces.count - 1)].utf8)
            }
            let cut = next(body.count)
            var parser = SseParser()
            _ = try parser.push(body[..<cut])
            let completed = try parser.push(body[cut...]).count
            let rest = Array(body[cut...])
            XCTAssertGreaterThanOrEqual(most(rest), completed, "round \(round): \(String(decoding: rest, as: UTF8.self).debugDescription)")
            let limit = 1 + next(6)
            XCTAssertGreaterThanOrEqual(most(rest, upTo: limit), min(completed, limit), "round \(round), limit \(limit)")
            XCTAssertLessThanOrEqual(most(rest, upTo: limit), limit, "round \(round), limit \(limit)")
        }
    }

    /// The densest bytes reach the bound: one line end that ends an event the earlier bytes began, then `data` and two line ends
    /// per event, with LF or with CR.
    func testTheDensestBytesReachTheBound() throws {
        for end in ["\n", "\r"] {
            var parser = SseParser()
            _ = try parser.push(Array("data: begun\(end)".utf8))
            let bytes = Array((end + String(repeating: "data\(end)\(end)", count: 5)).utf8)
            XCTAssertEqual(try parser.push(bytes).count, 6)
            XCTAssertEqual(most(bytes), 6)
            XCTAssertEqual(most(bytes, upTo: 4), 4, "the count stops at the limit")
        }
        XCTAssertEqual(most([]), 0)
        XCTAssertEqual(most(Array("data: no line end".utf8)), 0)
        XCTAssertEqual(most(Array("\n".utf8), upTo: 0), 0)
    }
}

// MARK: - Backpressure with a recorded task

/// What the stream asks of its task while the core does and does not pull: the rule is the Kotlin adapter's, the socket is read
/// only while fewer events wait than the binding's buffer has room for.
final class SseChunkBackpressureTests: XCTestCase {
    func testTheTaskIsSuspendedWhenTheQueueFillsAndResumedWhenThePullTakesItBelowTheMark() async throws {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        var iterator = stream.events.makeAsyncIterator()

        // Ten events wait, the room is four: the task stays suspended (it was suspended while the chunk was parsed).
        stream.receive(Data(eventBytes(0 ..< 10)))
        XCTAssertEqual(control.calls, [.suspend])
        XCTAssertTrue(stream.isSuspended)

        // Six pulls leave four waiting, which is not below the mark: still suspended.
        for index in 0 ..< 6 {
            let event = try await iterator.next()
            XCTAssertEqual(event?.id, "\(index)")
        }
        XCTAssertEqual(control.calls, [.suspend])
        XCTAssertTrue(stream.isSuspended)

        // The seventh leaves three: below the mark, and the task is resumed once.
        let seventh = try await iterator.next()
        XCTAssertEqual(seventh?.id, "6")
        XCTAssertEqual(control.calls, [.suspend, .resume])
        XCTAssertFalse(stream.isSuspended)
    }

    /// With the core not pulling, what URLSession had already read still arrives; it is queued, and the task is not suspended again.
    func testChunksInFlightAfterTheSuspendAreQueuedAndSuspendOnce() async throws {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        var iterator = stream.events.makeAsyncIterator()
        stream.receive(Data(eventBytes(0 ..< 5)))
        stream.receive(Data(eventBytes(5 ..< 10)))
        stream.receive(Data(eventBytes(10 ..< 15)))
        XCTAssertEqual(control.calls, [.suspend], "one suspend for three chunks")

        var seen: [SseEvent] = []
        // Eleven pulls leave four waiting; the twelfth leaves three.
        for _ in 0 ..< 11 {
            if let event = try await iterator.next() {
                seen.append(event)
            }
        }
        XCTAssertEqual(control.calls, [.suspend])
        if let event = try await iterator.next() {
            seen.append(event)
        }
        XCTAssertEqual(control.calls, [.suspend, .resume], "one resume, when the queue fell below the mark")
        for _ in 0 ..< 3 {
            if let event = try await iterator.next() {
                seen.append(event)
            }
        }
        XCTAssertEqual(seen, expectedEvents(0 ..< 15), "every event, in order")
        XCTAssertEqual(control.calls, [.suspend, .resume])
    }

    /// A chunk with too few line ends to take the queue to the mark is parsed with the task running: it is neither suspended
    /// nor resumed.
    func testAChunkThatCannotReachTheMarkIsParsedWithTheTaskRunning() {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        stream.receive(Data(Array("id: 0\ndata: par".utf8)))
        stream.receive(Data(Array("tial\n\n".utf8)))
        XCTAssertEqual(control.calls, [])
        XCTAssertFalse(stream.isSuspended)
    }

    /// A chunk that may take the queue to the mark is parsed with the task suspended, and when it did not (seven comment lines
    /// could end four events, and end none) the task is resumed at once.
    func testAChunkThatMayReachTheMarkIsSuspendedWhileParsedAndResumedWhenItLeftRoom() {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        stream.receive(Data(Array(String(repeating: ":\n", count: 7).utf8)))
        XCTAssertEqual(control.calls, [.suspend, .resume])
        XCTAssertFalse(stream.isSuspended)
        // Six line ends could end three events, one fewer than the room: no suspend.
        stream.receive(Data(Array(String(repeating: ":\n", count: 6).utf8)))
        XCTAssertEqual(control.calls, [.suspend, .resume])
    }

    /// The body of the wire test's short stream (the opening comment, three events, the end) never suspends the task: URLSession
    /// can lose the end of a body when its task is resumed and at once suspended again as the body ends, which a suspend around
    /// every chunk did to this stream (`.10x/decisions/sde/sse-end-race.md`).
    func testAShortStreamThatEndsNeverSuspendsTheTask() async {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, control: control)
        stream.receive(Data(ScriptedEventServer.opening))
        stream.receive(Data(eventBytes(0 ..< 3)))
        stream.finish(nil)
        let (events, end) = await drain(stream)
        XCTAssertEqual(events, expectedEvents(0 ..< 3))
        XCTAssertEqual(end, .ended)
        XCTAssertEqual(control.calls, [], "the task was suspended for chunks that could not fill the queue")
    }

    /// The queue is the mark's, not the window's: a core that pulls as fast as events come keeps the task running.
    func testAPullingCoreKeepsTheTaskRunning() async throws {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        var iterator = stream.events.makeAsyncIterator()
        for round in 0 ..< 20 {
            stream.receive(Data(eventBytes(round * 3 ..< round * 3 + 3)))
            for index in round * 3 ..< round * 3 + 3 {
                let event = try await iterator.next()
                XCTAssertEqual(event?.id, "\(index)")
            }
        }
        XCTAssertFalse(stream.isSuspended)
        XCTAssertEqual(control.calls.count, 40, "a suspend and a resume per chunk, nothing left suspended")
        XCTAssertEqual(control.calls.last, .resume)
    }

    /// A `URLSessionTask` counts its suspends (each needs its own resume), so the stream must never suspend a task it suspended
    /// already nor resume one it did not suspend. Over a seeded run of chunks of any size (some ending inside an event) and pulls
    /// of any number, the calls alternate suspend, resume, suspend; after each step the task is suspended exactly when as many
    /// events wait as the room allows; every event arrives in order; and `close` leaves as many resumes as suspends.
    func testSuspendsAndResumesAlternateAndBalanceOverASeededRun() async throws {
        var seed: UInt64 = 0x5EED_0000_2026_1002
        func next(_ bound: Int) -> Int {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int((seed >> 33) % UInt64(bound + 1))
        }
        let room = 4
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: room, control: control)
        var iterator = stream.events.makeAsyncIterator()
        let body = eventBytes(0 ..< 600)
        var sent = 0
        var completed = 0
        var seen: [SseEvent] = []
        while sent < body.count {
            if next(2) > 0 {
                // A chunk of 0 to 120 bytes: from nothing to several events, cut anywhere.
                let end = min(body.count, sent + next(120))
                stream.receive(Data(body[sent ..< end]))
                sent = end
                completed = String(decoding: body[..<sent], as: UTF8.self).components(separatedBy: "\n\n").count - 1
            } else {
                // Pull some of what waits (never more, so no pull waits).
                for _ in 0 ..< next(completed - seen.count) {
                    if let event = try await iterator.next() {
                        seen.append(event)
                    }
                }
            }
            let waiting = completed - seen.count
            XCTAssertEqual(stream.isSuspended, waiting >= room, "after \(sent) bytes and \(seen.count) pulls, \(waiting) waiting")
            for (index, call) in control.calls.enumerated() {
                XCTAssertEqual(call, index % 2 == 0 ? .suspend : .resume, "call \(index) of \(control.calls)")
            }
        }
        while seen.count < completed {
            if let event = try await iterator.next() {
                seen.append(event)
            }
        }
        XCTAssertEqual(seen, expectedEvents(0 ..< 600), "every event, in order")
        XCTAssertFalse(stream.isSuspended)
        stream.receive(Data(eventBytes(600 ..< 610)))
        XCTAssertTrue(stream.isSuspended)
        await stream.close()
        let calls = control.calls
        XCTAssertEqual(calls.filter { $0 == .suspend }.count, calls.filter { $0 == .resume }.count, "\(calls.suffix(4))")
        XCTAssertEqual(calls.suffix(3), [.suspend, .cancel, .resume])
    }

    /// Closing a suspended stream cancels the request and resumes it, so a cancelled task that stays suspended cannot hold the
    /// connection (the server must see the client leave), and wakes the pull that waits.
    func testCloseCancelsAndResumesASuspendedTaskAndWakesThePull() async throws {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 2, control: control)
        stream.receive(Data(eventBytes(0 ..< 5)))
        XCTAssertEqual(control.calls, [.suspend])
        await stream.close()
        XCTAssertEqual(control.calls, [.suspend, .cancel, .resume])
        XCTAssertFalse(stream.isSuspended)
        var iterator = stream.events.makeAsyncIterator()
        let after = try await iterator.next()
        XCTAssertNil(after, "a closed stream finishes")

        let waiting = URLSessionSseStream(lastEventId: nil, room: 2, control: RecordingControl())
        let detector = HangDetector { await waiting.close() }
        defer { detector.cancel() }
        let pull = Task { () async throws -> SseEvent? in
            var iterator = waiting.events.makeAsyncIterator()
            return try await iterator.next()
        }
        await waiting.close()
        let answered = try await pull.value
        XCTAssertNil(answered)
        XCTAssertFalse(detector.fired, "the pull was not answered by the close")
    }

    /// Cancelling the task whose pull waits for an event ends the stream as a cancelled read of `URLSession.AsyncBytes` ended it
    /// (what this adapter was before it read chunks): the pull throws `network("cancelled")` and the request is cancelled, so the
    /// server sees the client leave. A pull that waited until a chunk or the close came would hold the request open.
    func testCancellingAWaitingPullEndsTheStreamAndCancelsTheRequest() async throws {
        let control = RecordingControl()
        let stream = URLSessionSseStream(lastEventId: nil, room: 4, control: control)
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        let pull = Task { () async -> Result<SseEvent?, SseError> in
            var iterator = stream.events.makeAsyncIterator()
            do {
                return .success(try await iterator.next())
            } catch {
                return .failure(error as? SseError ?? .network("not an SseError: \(error)"))
            }
        }
        await eventually("the pull to wait") { stream.hasWaitingPull }
        pull.cancel()
        let result = await pull.value
        XCTAssertEqual(result, .failure(.network("cancelled")))
        XCTAssertFalse(detector.fired, "cancelling the task did not end the pull")
        XCTAssertEqual(control.calls, [.cancel], "the request is cancelled")

        // The stream is over: a chunk that comes after is not parsed and the task is not suspended again, and a pull finishes.
        stream.receive(Data(eventBytes(0 ..< 3)))
        XCTAssertEqual(control.calls, [.cancel])
        var iterator = stream.events.makeAsyncIterator()
        let after = try await iterator.next()
        XCTAssertNil(after)
    }
}

// MARK: - A real URLSession task against a server that writes what the test says

/// A loopback HTTP server that answers one request with the head of an event stream (or never answers) and then writes only the
/// bytes a test sends, so the test decides where the chunks of the body end: it sends one piece, waits until the stream's
/// delegate has received it, and sends the next, whatever the speed of the machine.
final class ScriptedEventServer: @unchecked Sendable {
    private struct State {
        var client: Int32 = -1
        var sawRequest = false
        var clientLeft = false
    }

    struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    /// The first bytes of the body, written with the head: a comment, which the parser ignores. CFNetwork hands over the
    /// answer's head only once the first byte of the body arrived (with a length, chunked or ended by closing alike), so a
    /// server that wants `open` to return writes something at once, as the shared Node server's `/sse/hang` does.
    static let opening = Array(": open\n\n".utf8)

    let port: UInt16
    private let listener: Int32
    private let respond: Bool
    private let state = Locked(State())

    /// Listens on a loopback port of the system's choosing. With `respond` false the server accepts, reads the request and says nothing.
    init(respond: Bool = true) throws {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw Failure(description: "socket: \(String(cString: strerror(errno)))")
        }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = 0
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let bound = withUnsafePointer(to: &address) { (pointer: UnsafePointer<sockaddr_in>) -> Int32 in
            return pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { (generic: UnsafePointer<sockaddr>) -> Int32 in
                return bind(fd, generic, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 8) == 0 else {
            let why = String(cString: strerror(errno))
            Darwin.close(fd)
            throw Failure(description: "bind or listen: \(why)")
        }
        var assigned = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &assigned) { (pointer: UnsafeMutablePointer<sockaddr_in>) -> Int32 in
            return pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { (generic: UnsafeMutablePointer<sockaddr>) -> Int32 in
                return getsockname(fd, generic, &length)
            }
        }
        self.port = UInt16(bigEndian: assigned.sin_port)
        self.listener = fd
        self.respond = respond
        let thread = Thread { [self] in
            self.serve()
        }
        thread.start()
    }

    /// Whether the request arrived.
    var sawRequest: Bool {
        return state.withLock { $0.sawRequest }
    }

    /// Whether the client closed its end of the connection.
    var clientLeft: Bool {
        return state.withLock { $0.clientLeft }
    }

    /// `http://127.0.0.1:<port>/`.
    var url: String {
        return "http://127.0.0.1:\(port)/"
    }

    /// Writes `bytes` to the client (they are the body: the server answers without a length and ends the body by closing).
    func send(_ bytes: [UInt8]) {
        let fd = state.withLock { $0.client }
        guard fd >= 0 else {
            return
        }
        var offset = 0
        while offset < bytes.count {
            let written = bytes[offset...].withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Int in
                return Darwin.send(fd, raw.baseAddress, raw.count, 0)
            }
            if written <= 0 {
                return
            }
            offset += written
        }
    }

    /// Answers the request: the head of an event stream and the opening comment (what a server made with `respond` does at once).
    func answer() {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        send(Array(head.utf8) + ScriptedEventServer.opening)
    }

    /// Ends the body: the server closes its sending side.
    func finish() {
        let fd = state.withLock { $0.client }
        if fd >= 0 {
            shutdown(fd, SHUT_WR)
        }
    }

    private func serve() {
        let client = accept(listener, nil, nil)
        // One request per server: the listener is not needed again.
        Darwin.close(listener)
        guard client >= 0 else {
            return
        }
        var one: Int32 = 1
        setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(client, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))
        var request: [UInt8] = []
        var buffer = [UInt8](repeating: 0, count: 4_096)
        let end: [UInt8] = [13, 10, 13, 10]
        while !(request.count >= 4 && Array(request.suffix(4)) == end) {
            let count = recv(client, &buffer, buffer.count, 0)
            if count <= 0 {
                Darwin.close(client)
                return
            }
            request += buffer[0 ..< count]
        }
        state.withLock {
            $0.client = client
            $0.sawRequest = true
        }
        if respond {
            answer()
        }
        // From here the client is only listened to, to notice it leaving.
        while recv(client, &buffer, buffer.count, 0) > 0 {}
        state.withLock {
            $0.clientLeft = true
            $0.client = -1
        }
        Darwin.close(client)
    }
}

/// The default adapter and a real URLSession task, with the server deciding where each chunk ends.
final class SseChunkWireTests: XCTestCase {
    private func open(_ server: ScriptedEventServer) async throws -> URLSessionSseStream {
        let stream = try await URLSessionSseAdapter().open(url: server.url, headers: [], lastEventId: nil)
        return try XCTUnwrap(stream as? URLSessionSseStream)
    }

    /// The whole awkward body, one byte to a chunk, through URLSession: the server waits until the delegate has each byte before it
    /// sends the next, so every cut the body has is a chunk boundary of the real task.
    func testEveryByteOfTheBodyArrivingAsItsOwnChunkParsesAsTheWhole() async throws {
        let server = try ScriptedEventServer()
        let stream = try await open(server)
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        let reading = Task { await drain(stream) }
        var sent = ScriptedEventServer.opening.count
        var allArrived = true
        for byte in awkwardBody {
            server.send([byte])
            sent += 1
            // Each byte is a real socket round trip: how long it takes is the machine's business, so the wait has the hang deadline and
            // the detector is restarted at every step (it is armed for the time since the last byte, not for the whole body).
            guard await eventually("byte \(sent) of the body", { stream.received.bytes == sent }) else {
                allArrived = false
                break
            }
            detector.kick()
        }
        if allArrived {
            XCTAssertGreaterThanOrEqual(stream.received.chunks, awkwardBody.count, "each byte was a chunk of its own")
        }
        server.finish()
        let (events, end) = await reading.value
        XCTAssertEqual(events, awkwardEvents)
        XCTAssertEqual(end, .ended)
        XCTAssertFalse(detector.fired, "the stream did not end")
    }

    /// The task is suspended while events wait and is not lost: what the server sent meanwhile arrives after the pull, in order,
    /// and the end follows.
    func testASuspendedTaskResumesWhenTheCorePullsAndTheEventsAreAllThereInOrder() async throws {
        let server = try ScriptedEventServer()
        let stream = try await open(server)
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        let first = eventBytes(0 ..< 40)
        let second = eventBytes(40 ..< 100)
        let third = eventBytes(100 ..< 130)
        let opening = ScriptedEventServer.opening.count
        server.send(first)
        await eventually("the first piece") { stream.received.bytes == opening + first.count }
        // Forty events wait and the room is sixteen, and nobody pulls: the task is, and stays, suspended.
        await eventually("the task suspended") { stream.isSuspended }
        server.send(second)
        server.send(third)
        var iterator = stream.events.makeAsyncIterator()
        var seen: [SseEvent] = []
        // Twenty-five pulls leave fifteen waiting: below the room.
        for _ in 0 ..< 25 {
            if let event = try await iterator.next() {
                seen.append(event)
            }
        }
        // What was sent while the task was suspended arrives (it would never, if the task were not resumed).
        await eventually("the pieces sent while suspended") { stream.received.bytes == opening + first.count + second.count + third.count }
        server.finish()
        while true {
            do {
                guard let event = try await iterator.next() else {
                    break
                }
                seen.append(event)
            } catch {
                XCTAssertEqual(error as? SseError, .ended)
                break
            }
        }
        XCTAssertEqual(seen, expectedEvents(0 ..< 130))
        XCTAssertFalse(detector.fired, "the stream did not deliver the events the server sent while the task was suspended")
    }

    /// Closing a stream whose task is suspended makes the server see the client leave.
    func testClosingASuspendedStreamLetsTheServerSeeTheClientLeave() async throws {
        let server = try ScriptedEventServer()
        let stream = try await open(server)
        let piece = eventBytes(0 ..< 40)
        server.send(piece)
        await eventually("the piece") { stream.received.bytes == ScriptedEventServer.opening.count + piece.count }
        await eventually("the task suspended") { stream.isSuspended }
        XCTAssertFalse(server.clientLeft)
        await stream.close()
        await eventually("the server seeing the client leave") { server.clientLeft }
    }

    /// Cancelling the caller of `open` cancels the request: it fails as the cancellation URLSession reports, and the server sees
    /// the client leave, however long the server would have taken to answer.
    func testCancellingOpenCancelsTheRequest() async throws {
        let server = try ScriptedEventServer(respond: false)
        let adapter = URLSessionSseAdapter()
        let url = server.url
        let opening = Task { await capture { () async throws(SseError) -> UInt32 in
            _ = try await adapter.open(url: url, headers: [], lastEventId: nil)
            return 0
        } }
        // If cancelling did not end the request, the server answering it ends the wait (with a stream, which is not a cancellation).
        let detector = HangDetector { server.answer() }
        defer { detector.cancel() }
        await eventually("the request") { server.sawRequest }
        opening.cancel()
        let result = await opening.value
        XCTAssertEqual(result, .failure(.network("cancelled")))
        XCTAssertFalse(detector.fired, "cancelling open did not cancel the request")
        await eventually("the server seeing the client leave") { server.clientLeft }
    }

    /// Cancelling the task that waits for the next event cancels the request: the pull throws `network("cancelled")`, as a
    /// cancelled `URLSession.AsyncBytes` read did, and the server sees the client leave.
    func testCancellingTheTaskThatWaitsForAnEventCancelsTheRequest() async throws {
        let server = try ScriptedEventServer()
        let stream = try await open(server)
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        server.send(eventBytes(0 ..< 1))
        let tookFirst = Locked(false)
        let pull = Task { () async -> (first: SseEvent?, end: Result<SseEvent?, SseError>) in
            var iterator = stream.events.makeAsyncIterator()
            let first = try? await iterator.next()
            tookFirst.withLock { $0 = true }
            do {
                return (first, .success(try await iterator.next()))
            } catch {
                return (first, .failure(error as? SseError ?? .network("not an SseError: \(error)")))
            }
        }
        await eventually("the first event taken and the next pull waiting") { tookFirst.withLock { $0 } && stream.hasWaitingPull }
        pull.cancel()
        let (first, end) = await pull.value
        XCTAssertEqual(first, expectedEvents(0 ..< 1)[0])
        XCTAssertEqual(end, .failure(.network("cancelled")))
        XCTAssertFalse(detector.fired, "cancelling the task did not end the pull")
        await eventually("the server seeing the client leave") { server.clientLeft }
    }

    /// A body that ends cleanly is `ended`, after the events that were in it.
    func testABodyThatEndsIsEndedAfterItsEvents() async throws {
        let server = try ScriptedEventServer()
        let stream = try await open(server)
        let detector = HangDetector { await stream.close() }
        defer { detector.cancel() }
        server.send(eventBytes(0 ..< 3))
        server.finish()
        let (events, end) = await drain(stream)
        XCTAssertEqual(events, expectedEvents(0 ..< 3))
        XCTAssertEqual(end, .ended)
        XCTAssertFalse(detector.fired, "the body's end was not seen")
    }
}
