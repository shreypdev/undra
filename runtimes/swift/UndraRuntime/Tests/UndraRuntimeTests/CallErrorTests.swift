import Foundation
import XCTest
@testable import UndraRuntime

// ADR-032: what a generated method throws, and how a command reports. Everything here is the
// runtime's half of the error channel; the generated half (`do { ... } catch { throw
// UndraCallError.mapped(error, domain: E.self) }`) is pinned by the bindgen goldens and the contract
// suite.

// MARK: - Fixtures

/// A hand-written stand-in for a generated error enum. `.code(UInt16)` is variant 0, so the four
/// bytes of an empty `String` read as `.code(0)`: the ambiguity a stream's error item had before
/// ADR-036 gave it a flag of its own, which the stream tests pin as gone. `.note(String)` carries a
/// `String`.
enum WireTestError: UndraError {
    case code(UInt16)
    case note(String)
    case empty

    static func undraDecode(_ r: inout UndraReader) throws -> WireTestError {
        let at = r.position
        let index = try r.readU16()
        switch index {
        case 0:
            return .code(try r.readU16())
        case 1:
            return .note(try r.readString())
        case 2:
            return .empty
        default:
            throw WireError.invalidTag(tag: UInt32(index), at: at, type: "WireTestError")
        }
    }

    func undraEncode(_ w: inout UndraWriter) {
        switch self {
        case .code(let value):
            w.writeU16(0)
            w.writeU16(value)
        case .note(let text):
            w.writeU16(1)
            w.writeString(text)
        case .empty:
            w.writeU16(2)
        }
    }
}

/// The encoding of one `String`: the body of a refused reply.
private func stringBody(_ text: String) -> [UInt8] {
    var writer = UndraWriter()
    writer.writeString(text)
    return writer.finish()
}

/// The body of a panic reply: message, then backtrace.
private func panicBody(_ message: String, _ backtrace: String) -> [UInt8] {
    var writer = UndraWriter()
    writer.writeString(message)
    writer.writeString(backtrace)
    return writer.finish()
}

/// Records what `LoadOptions.onError` receives, from any thread.
private final class ErrorLog: @unchecked Sendable {
    private let items = Guarded<[UndraUnhandledError]>([])

    var all: [UndraUnhandledError] {
        return items.withLock { (current: inout [UndraUnhandledError]) -> [UndraUnhandledError] in
            return current
        }
    }

    func append(_ item: UndraUnhandledError) {
        items.withLock { (current: inout [UndraUnhandledError]) -> Void in
            current.append(item)
        }
    }
}

private func replyError(status: ReplyStatus, body: [UInt8]) -> UndraReplyError {
    return UndraReplyError(status: status, body: body)
}

// MARK: - The mapping

final class CallErrorMappingTests: XCTestCase {
    private func mappedCall(_ error: any Error, file: StaticString = #filePath, line: UInt = #line) -> UndraCallError? {
        let result = UndraCallError.mapped(error)
        guard let call = result as? UndraCallError else {
            XCTFail("expected an UndraCallError, got \(result)", file: file, line: line)
            return nil
        }
        return call
    }

    func testCancellationErrorStaysItself() {
        XCTAssertTrue(UndraCallError.mapped(CancellationError()) is CancellationError)
    }

    func testAnUndraCallErrorIsMappedToItself() {
        for original in [
            UndraCallError.cancelledByCore,
            .panicked(message: "m", backtrace: "b"),
            .refused(reason: "r"),
            .unavailable(.closed),
            .malformed("x"),
        ] {
            XCTAssertEqual(mappedCall(original), original)
            XCTAssertEqual(mappedCall(UndraCallError.mapped(original)), original, "idempotent")
        }
    }

    func testStatusCancelledIsCancelledByTheCoreAndNeverACancellationError() {
        let result = UndraCallError.mapped(replyError(status: .cancelled, body: []))
        XCTAssertFalse(result is CancellationError)
        XCTAssertEqual(result as? UndraCallError, .cancelledByCore)
    }

    func testStatusPanicCarriesMessageAndBacktrace() {
        XCTAssertEqual(
            mappedCall(replyError(status: .panic, body: panicBody("boom", "frame 0\nframe 1"))),
            .panicked(message: "boom", backtrace: "frame 0\nframe 1")
        )
    }

    func testAnUndecodablePanicReportStillMapsToPanicked() {
        XCTAssertEqual(
            mappedCall(replyError(status: .panic, body: [0xFF, 0xFF])),
            .panicked(message: "<undecodable panic report>", backtrace: "")
        )
        XCTAssertEqual(
            mappedCall(replyError(status: .panic, body: [])),
            .panicked(message: "<undecodable panic report>", backtrace: "")
        )
        // A message without a backtrace keeps the message.
        XCTAssertEqual(
            mappedCall(replyError(status: .panic, body: stringBody("only a message"))),
            .panicked(message: "only a message", backtrace: "")
        )
    }

    func testStatusBadRequestIsRefusedWithTheCoresText() {
        XCTAssertEqual(
            mappedCall(replyError(status: .badRequest, body: stringBody("E_REENTRANT: the core is calling out"))),
            .refused(reason: "E_REENTRANT: the core is calling out")
        )
        XCTAssertEqual(
            mappedCall(replyError(status: .badRequest, body: [0xFF])),
            .refused(reason: "<undecodable reason>")
        )
    }

    func testATypedErrorOnAMethodWithoutOneIsMalformed() {
        XCTAssertEqual(
            mappedCall(replyError(status: .error, body: [1, 2, 3])),
            .malformed("the core answered with a typed error, but this method has none (3 bytes)")
        )
    }

    func testAStreamWhereAReplyWasExpectedIsMalformed() {
        XCTAssertEqual(
            mappedCall(replyError(status: .streamOpened, body: [])),
            .malformed("the core opened a stream where a single reply was expected")
        )
    }

    func testStatusOkAsAFailureIsMalformedAndTotal() {
        XCTAssertEqual(
            mappedCall(replyError(status: .ok, body: [])),
            .malformed("the core answered ok as a failure")
        )
    }

    func testTransportErrorsAreUnavailable() {
        for error in [
            UndraTransportError.closed,
            .timedOut(operation: "callSync"),
            .connectionLost(reason: "reset"),
        ] {
            XCTAssertEqual(mappedCall(error), .unavailable(error))
        }
    }

    func testASchemaChangeOfTheRemoteCoreIsUnavailable() {
        // What the remote transport fails every call in flight with when `undra dev` comes back
        // with another schema (WebSocketTransport.handleFrame).
        let mismatch = UndraSchemaMismatchError(expected: 1, got: 2)
        XCTAssertEqual(mappedCall(mismatch), .unavailable(.connectionLost(reason: mismatch.description)))
        XCTAssertEqual(
            mappedCall(UndraCallError.mapped(mismatch, domain: WireTestError.self)),
            .unavailable(.connectionLost(reason: mismatch.description))
        )
    }

    func testProtocolErrorsAreMalformed() {
        XCTAssertEqual(
            mappedCall(UndraProtocolError.nullHandle),
            .malformed(UndraProtocolError.nullHandle.description)
        )
        XCTAssertEqual(
            mappedCall(UndraProtocolError.notAStream(callId: 4)),
            .malformed(UndraProtocolError.notAStream(callId: 4).description)
        )
    }

    func testWireErrorsAreMalformed() {
        let wire = WireError.unexpectedEOF(needed: 4, at: 0)
        XCTAssertEqual(mappedCall(wire), .malformed("the reply does not decode: \(wire)"))
    }

    func testAModeErrorIsRefused() {
        let mode = UndraModeError(operation: "snapshot", mode: .remote)
        XCTAssertEqual(mappedCall(mode), .refused(reason: mode.description))
    }

    func testAnyOtherErrorIsReturnedUnchanged() {
        struct Foreign: Error, Equatable {}
        XCTAssertEqual(UndraCallError.mapped(Foreign()) as? Foreign, Foreign())
    }

    // MARK: Domain

    func testATypedReplyBecomesTheDomainError() {
        let body = WireTestError.note("no good").undraEncoded()
        let result = UndraCallError.mapped(replyError(status: .error, body: body), domain: WireTestError.self)
        XCTAssertEqual(result as? WireTestError, .note("no good"))
    }

    func testATypedReplyThatDoesNotDecodeIsMalformed() {
        let result = UndraCallError.mapped(replyError(status: .error, body: [0x63, 0x00]), domain: WireTestError.self)
        XCTAssertEqual(
            result as? UndraCallError,
            .malformed("a WireTestError that does not decode (2 bytes)")
        )
        // Trailing bytes after a valid value do not decode either.
        let trailing = WireTestError.empty.undraEncoded() + [0]
        XCTAssertTrue(
            UndraCallError.mapped(replyError(status: .error, body: trailing), domain: WireTestError.self)
                is UndraCallError
        )
    }

    func testNonTypedStatusesFallThroughWithADomain() {
        XCTAssertTrue(UndraCallError.mapped(CancellationError(), domain: WireTestError.self) is CancellationError)
        XCTAssertEqual(
            UndraCallError.mapped(replyError(status: .cancelled, body: []), domain: WireTestError.self) as? UndraCallError,
            .cancelledByCore
        )
        XCTAssertEqual(
            UndraCallError.mapped(UndraTransportError.closed, domain: WireTestError.self) as? UndraCallError,
            .unavailable(.closed)
        )
        XCTAssertEqual(
            UndraCallError.mapped(replyError(status: .panic, body: panicBody("p", "")), domain: WireTestError.self)
                as? UndraCallError,
            .panicked(message: "p", backtrace: "")
        )
    }

    // MARK: Streams (ADR-036)

    /// A stream's own typed error item (flag 2), as `UndraCore` ends the stream with it.
    private func errorItem(_ body: [UInt8]) -> UndraReplyError {
        return UndraReplyError(status: .error, body: body)
    }

    /// A failed stream item (flag 3), as `UndraCore` ends the stream with it.
    private func failedItem(_ failure: Wire.StreamFailure) -> any Error {
        return UndraCore.streamFailureError(ArraySlice(failure.encode()))
    }

    func testAnErrorItemWithADomainReadsAsTheDomainError() {
        for value in [WireTestError.code(7), .note("no good"), .empty] {
            let result = UndraCallError.mapped(streamFailure: errorItem(value.undraEncoded()), domain: WireTestError.self)
            XCTAssertEqual(result as? WireTestError, value)
        }
    }

    func testAnErrorItemThatIsNotTheDomainErrorIsMalformed() {
        let garbage: [UInt8] = [0xFF, 0xFF, 0xFF]
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: errorItem(garbage), domain: WireTestError.self) as? UndraCallError,
            .malformed("a WireTestError that does not decode (3 bytes)")
        )
        // A String body is not read as the core's text any more: the prefix "cancelled: " means
        // nothing, the bytes are just not a `WireTestError`.
        let text = stringBody("cancelled: the runtime shut down")
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: errorItem(text), domain: WireTestError.self) as? UndraCallError,
            .malformed("a WireTestError that does not decode (\(text.count) bytes)")
        )
        // Trailing bytes after a valid value do not decode either.
        let trailing = WireTestError.empty.undraEncoded() + [0]
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: errorItem(trailing), domain: WireTestError.self) as? UndraCallError,
            .malformed("a WireTestError that does not decode (3 bytes)")
        )
    }

    func testAnErrorItemOnAStreamWithoutAnErrorTypeIsMalformed() {
        let bodies: [[UInt8]] = [
            WireTestError.code(7).undraEncoded(),
            stringBody("cancelled: the runtime shut down"),
            stringBody("index out of bounds"),
            [],
        ]
        for body in bodies {
            XCTAssertEqual(
                UndraCallError.mapped(streamFailure: errorItem(body)) as? UndraCallError,
                .malformed("a stream without an error type ended with a typed error item (\(body.count) bytes)")
            )
        }
    }

    func testAnErrorItemIsAlwaysTheDomainErrorNeverAString() {
        // Before ADR-036 an empty message (four zero bytes) also read as `WireTestError.code(0)`, and
        // the runtime had to guess. Flag 2 now only ever carries `E`.
        let body = stringBody("")
        XCTAssertEqual(body, [0, 0, 0, 0])
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: errorItem(body), domain: WireTestError.self) as? WireTestError,
            .code(0)
        )
        let note = WireTestError.note("cancelled: the runtime shut down").undraEncoded()
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: errorItem(note), domain: WireTestError.self) as? WireTestError,
            .note("cancelled: the runtime shut down")
        )
    }

    func testAFailedItemMapsExactlyAsAFailedReplyWithItsStatus() {
        let cases: [(Wire.StreamFailure, UndraReplyError, UndraCallError)] = [
            (
                Wire.StreamFailure(status: .cancelled, message: "the runtime shut down"),
                replyError(status: .cancelled, body: []),
                .cancelledByCore
            ),
            (
                Wire.StreamFailure(status: .cancelled, message: "a restore replaced the receiver"),
                replyError(status: .cancelled, body: []),
                .cancelledByCore
            ),
            (
                Wire.StreamFailure(status: .panic, message: "boom", detail: "frame 0\nframe 1"),
                replyError(status: .panic, body: panicBody("boom", "frame 0\nframe 1")),
                .panicked(message: "boom", backtrace: "frame 0\nframe 1")
            ),
            (
                Wire.StreamFailure(status: .panic, message: "", detail: ""),
                replyError(status: .panic, body: panicBody("", "")),
                .panicked(message: "", backtrace: "")
            ),
            (
                Wire.StreamFailure(status: .badRequest, message: "the object was closed"),
                replyError(status: .badRequest, body: stringBody("the object was closed")),
                .refused(reason: "the object was closed")
            ),
        ]
        for (failure, reply, expected) in cases {
            let error = failedItem(failure)
            XCTAssertEqual(error as? UndraReplyError, reply, "\(failure)")
            XCTAssertEqual(UndraCallError.mapped(streamFailure: error) as? UndraCallError, expected, "\(failure)")
            XCTAssertEqual(
                UndraCallError.mapped(streamFailure: error, domain: WireTestError.self) as? UndraCallError,
                expected,
                "\(failure), with a domain"
            )
            XCTAssertEqual(mappedCall(reply), expected, "\(failure): the same as the reply")
        }
    }

    func testACancellationByTheCoreIsNeverACancellationError() {
        let error = failedItem(Wire.StreamFailure(status: .cancelled, message: "the runtime shut down"))
        XCTAssertFalse(UndraCallError.mapped(streamFailure: error) is CancellationError)
        XCTAssertFalse(UndraCallError.mapped(streamFailure: error, domain: WireTestError.self) is CancellationError)
    }

    func testAFailedItemThatDoesNotDecodeIsMalformed() {
        let cases: [([UInt8], WireError)] = [
            ([1, 0, 0, 0, 0, 0, 0, 0, 0], .invalidTag(tag: 1, at: 0, type: "StreamFailure.status")),
            ([4, 0, 0, 0, 0, 0, 0, 0, 0], .invalidTag(tag: 4, at: 0, type: "StreamFailure.status")),
            ([2, 4, 0, 0, 0, 0x62], .lengthTooLarge(len: 4, at: 1)),
            ([], .unexpectedEOF(needed: 1, at: 0)),
            ([3, 0, 0, 0, 0, 0, 0, 0, 0, 7], .trailingBytes(count: 1)),
        ]
        for (body, wire) in cases {
            let error = UndraCore.streamFailureError(ArraySlice(body))
            let expected = UndraProtocolError.malformedMessage(context: "stream failure", error: wire)
            XCTAssertEqual(error as? UndraProtocolError, expected)
            XCTAssertEqual(UndraCallError.mapped(streamFailure: error) as? UndraCallError, .malformed(expected.description))
            XCTAssertEqual(
                UndraCallError.mapped(streamFailure: error, domain: WireTestError.self) as? UndraCallError,
                .malformed(expected.description)
            )
        }
    }

    func testAStreamFailureThatIsNotAnErrorItemUsesTheOrdinaryMapping() {
        XCTAssertTrue(UndraCallError.mapped(streamFailure: CancellationError()) is CancellationError)
        XCTAssertTrue(UndraCallError.mapped(streamFailure: CancellationError(), domain: WireTestError.self) is CancellationError)
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: UndraTransportError.closed, domain: WireTestError.self)
                as? UndraCallError,
            .unavailable(.closed)
        )
        // A stream whose opening reply failed: the reply statuses map as for a call.
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: replyError(status: .panic, body: panicBody("p", "b"))) as? UndraCallError,
            .panicked(message: "p", backtrace: "b")
        )
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: replyError(status: .badRequest, body: stringBody("stale")))
                as? UndraCallError,
            .refused(reason: "stale")
        )
        XCTAssertEqual(
            UndraCallError.mapped(streamFailure: UndraProtocolError.notAStream(callId: 4)) as? UndraCallError,
            .malformed(UndraProtocolError.notAStream(callId: 4).description)
        )
    }

    // MARK: Descriptions

    func testDescriptionsAreTheDocumentedTexts() {
        let cases: [(UndraCallError, String)] = [
            (.cancelledByCore, "the Undra core cancelled the call (a restore replaced its object, or the core shut down)"),
            (.panicked(message: "boom", backtrace: "frame"), "the Undra core panicked: boom"),
            (.refused(reason: "stale handle"), "the Undra core refused the call: stale handle"),
            (.unavailable(.closed), "the Undra core is unavailable: the Undra core is shut down or disconnected"),
            (.malformed("short read"), "the Undra core sent a reply the bindings cannot read: short read"),
        ]
        for (error, text) in cases {
            XCTAssertEqual(error.description, text)
            XCTAssertEqual(error.errorDescription, text)
            XCTAssertEqual(error.localizedDescription, text)
        }
    }

    func testAnUnhandledErrorNamesTheOperationAndTheReason() {
        let unhandled = UndraUnhandledError(operation: "Todos.toggle", error: .refused(reason: "closed"))
        XCTAssertEqual(unhandled.description, "Todos.toggle failed: the Undra core refused the call: closed")
        XCTAssertEqual(unhandled, UndraUnhandledError(operation: "Todos.toggle", error: .refused(reason: "closed")))
        XCTAssertEqual(unhandled.operation, "Todos.toggle")
        XCTAssertEqual(unhandled.error, .refused(reason: "closed"))
        XCTAssertEqual(unhandled.localizedDescription, unhandled.description)
    }
}

// MARK: - Through UndraCore

@MainActor
final class CallErrorCoreTests: XCTestCase {
    private let target = CallTarget.freeFunction(methodId: 4)

    private func replyWith(_ status: ReplyStatus, _ body: [UInt8]) -> @Sendable (Wire.Call) -> [UInt8] {
        return { call in
            return Wire.Reply(callId: call.callId, status: status, body: ArraySlice(body)).encode()
        }
    }

    /// What a generated sync method without an error type does, written out once.
    private func syncCall(_ core: UndraCore) throws -> [UInt8] {
        do {
            return try core.callSync(target, method: 4, args: [])
        } catch {
            throw UndraCallError.mapped(error)
        }
    }

    /// What a generated async method with an error type does.
    private func asyncCall(_ core: UndraCore) async throws -> [UInt8] {
        do {
            return try await core.call(target, method: 4, args: [])
        } catch {
            throw UndraCallError.mapped(error, domain: WireTestError.self)
        }
    }

    func testCallSyncStatusesMapThroughTheGeneratedPattern() throws {
        let expectations: [(ReplyStatus, [UInt8], UndraCallError)] = [
            (.panic, panicBody("boom", "bt"), .panicked(message: "boom", backtrace: "bt")),
            (.cancelled, [], .cancelledByCore),
            (.badRequest, stringBody("closed handle"), .refused(reason: "closed handle")),
        ]
        for (status, body, expected) in expectations {
            let transport = FakeTransport()
            transport.onCallSync = replyWith(status, body)
            let core = try makeCore(transport)
            XCTAssertThrowsError(try syncCall(core), "\(status)") { error in
                XCTAssertEqual(error as? UndraCallError, expected, "\(status)")
            }
            XCTAssertEqual(core.stats().hostPendingCalls, 0)
        }
    }

    func testCallSyncWithAGarbageReplyIsMalformed() throws {
        let transport = FakeTransport()
        transport.onCallSync = { _ in return [0xFF] }
        let core = try makeCore(transport)
        XCTAssertThrowsError(try syncCall(core)) { error in
            guard case .malformed(let text)? = error as? UndraCallError else {
                return XCTFail("expected .malformed, got \(error)")
            }
            XCTAssertTrue(text.contains("malformed reply"), text)
        }
    }

    func testCallSyncAfterShutdownIsUnavailable() throws {
        let core = try makeCore(FakeTransport())
        core.shutdown()
        XCTAssertThrowsError(try syncCall(core)) { error in
            XCTAssertEqual(error as? UndraCallError, .unavailable(.closed))
        }
    }

    func testCallSyncOverARemoteTransportThatTimesOutIsUnavailable() throws {
        let transport = FakeTransport(directSync: false)
        let core = try makeCore(transport, blockingCallTimeout: 0.05)
        XCTAssertThrowsError(try syncCall(core)) { error in
            XCTAssertEqual(error as? UndraCallError, .unavailable(.timedOut(operation: "callSync")))
        }
    }

    func testCallStatusesMapThroughTheGeneratedPattern() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let statuses: [(ReplyStatus, [UInt8])] = [
            (.panic, panicBody("boom", "bt")),
            (.cancelled, []),
            (.badRequest, stringBody("stale")),
            (.error, WireTestError.note("typed").undraEncoded()),
        ]
        var seen: [any Error] = []
        for (status, body) in statuses {
            transport.onCall = { call, fake in
                fake.deliver(Wire.Reply(callId: call.callId, status: status, body: ArraySlice(body)))
                return true
            }
            if let error = await captureError({ _ = try await self.asyncCall(core) }) {
                seen.append(error)
            }
        }
        XCTAssertEqual(seen.count, 4)
        XCTAssertEqual(seen[0] as? UndraCallError, .panicked(message: "boom", backtrace: "bt"))
        XCTAssertEqual(seen[1] as? UndraCallError, .cancelledByCore)
        XCTAssertEqual(seen[2] as? UndraCallError, .refused(reason: "stale"))
        XCTAssertEqual(seen[3] as? WireTestError, .note("typed"))
    }

    func testTheCoreRefusingACallWithoutAReplyIsRefused() async throws {
        let transport = FakeTransport()
        transport.onCall = { _, _ in return false }
        let core = try makeCore(transport)
        let error = await captureError { _ = try await self.asyncCall(core) }
        guard case .refused? = error as? UndraCallError else {
            return XCTFail("expected .refused, got \(String(describing: error))")
        }
    }

    func testACallTheRemoteTransportCannotSendIsUnavailableNotRefused() async throws {
        // The remote transport refuses to send only once its connection is closed.
        let transport = FakeTransport(directSync: false)
        transport.onCall = { _, _ in return false }
        let core = try makeCore(transport)
        let error = await captureError { _ = try await self.asyncCall(core) }
        XCTAssertEqual(error as? UndraCallError, .unavailable(.closed))
        XCTAssertThrowsError(try syncCall(core)) { error in
            XCTAssertEqual(error as? UndraCallError, .unavailable(.closed))
        }
        let stream = core.stream(
            .freeFunction(methodId: 8),
            method: 8,
            args: [],
            decode: { (body: [UInt8]) throws -> UInt8 in return body[0] },
            mapError: { UndraCallError.mapped(streamFailure: $0) }
        )
        let streamError = await captureError {
            for try await _ in stream {}
        }
        XCTAssertEqual(streamError as? UndraCallError, .unavailable(.closed))
    }

    func testACallCancelledWhileWaitingThrowsCancellationErrorAndNotCancelledByCore() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let task = Task { () -> [UInt8] in
            return try await self.asyncCall(core)
        }
        let sent = await waitUntil { !transport.calls.isEmpty }
        XCTAssertTrue(sent)
        task.cancel()
        let result = await task.result
        guard case .failure(let error) = result else {
            return XCTFail("a cancelled call must not succeed")
        }
        XCTAssertTrue(error is CancellationError, "\(error)")
        XCTAssertNil(error as? UndraCallError)
        // The core's status 3 for the same call arrives late and finds nobody waiting.
        transport.deliver(Wire.Reply(callId: transport.calls[0].callId, status: .cancelled))
    }

    func testACallCancelledBeforeItIsSentThrowsCancellationError() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let task = Task { () -> [UInt8] in
            withUnsafeCurrentTask { (current: UnsafeCurrentTask?) -> Void in
                current?.cancel()
            }
            return try await self.asyncCall(core)
        }
        let result = await task.result
        guard case .failure(let error) = result else {
            return XCTFail("a cancelled call must not succeed")
        }
        XCTAssertTrue(error is CancellationError, "\(error)")
        XCTAssertEqual(transport.calls.count, 0)
    }

    func testACallFailedByShutdownIsUnavailable() async throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let task = Task { () -> [UInt8] in
            return try await self.asyncCall(core)
        }
        let sent = await waitUntil { !transport.calls.isEmpty }
        XCTAssertTrue(sent)
        core.shutdown()
        let result = await task.result
        guard case .failure(let error) = result else {
            return XCTFail("a call in flight at shutdown must fail")
        }
        XCTAssertEqual(error as? UndraCallError, .unavailable(.closed))
        let later = await captureError { _ = try await self.asyncCall(core) }
        XCTAssertEqual(later as? UndraCallError, .unavailable(.closed))
    }

    func testAnUndecodableConstructorHandleIsMalformed() throws {
        let transport = FakeTransport()
        transport.onCallSync = { call in
            return Wire.Reply(callId: call.callId, status: .ok, body: [1, 2]).encode()
        }
        let core = try makeCore(transport)
        do {
            _ = try core.construct(type: 1, method: 2, args: [])
            XCTFail("a short handle must not construct")
        } catch {
            guard case .malformed? = UndraCallError.mapped(error) as? UndraCallError else {
                return XCTFail("expected .malformed, got \(error)")
            }
        }
    }

    // MARK: Streams end to end

    private func failedStream(_ core: UndraCore, domain: Bool) async -> (any Error)? {
        let stream = core.stream(
            .freeFunction(methodId: 8),
            method: 8,
            args: [],
            decode: { (body: [UInt8]) throws -> UInt8 in return body[0] },
            mapError: { (error: any Error) -> any Error in
                if domain {
                    return UndraCallError.mapped(streamFailure: error, domain: WireTestError.self)
                }
                return UndraCallError.mapped(streamFailure: error)
            }
        )
        return await captureError {
            for try await _ in stream {}
        }
    }

    func testStreamsEndWithTheMappedErrorForEveryKindOfFailure() async throws {
        let restore = Wire.StreamFailure(status: .cancelled, message: "a restore replaced the receiver")
        let shutdown = Wire.StreamFailure(status: .cancelled, message: "the runtime shut down")
        let panic = Wire.StreamFailure(status: .panic, message: "stream blew up", detail: "frame 0")
        let refused = Wire.StreamFailure(status: .badRequest, message: "stale handle")
        let cancelledText = stringBody("cancelled: the runtime shut down")
        let endings: [(String, FakeTransport.Ending, Bool, (any Error) -> Bool)] = [
            ("typed", .error(WireTestError.code(9).undraEncoded()), true, { ($0 as? WireTestError) == .code(9) }),
            (
                "typed, without a domain",
                .error(WireTestError.code(9).undraEncoded()),
                false,
                { ($0 as? UndraCallError) == .malformed("a stream without an error type ended with a typed error item (4 bytes)") }
            ),
            (
                "a String error item is not a cancellation",
                .error(cancelledText),
                true,
                { ($0 as? UndraCallError) == .malformed("a WireTestError that does not decode (\(cancelledText.count) bytes)") }
            ),
            ("restore", .failure(restore), true, { ($0 as? UndraCallError) == .cancelledByCore }),
            ("restore, without a domain", .failure(restore), false, { ($0 as? UndraCallError) == .cancelledByCore }),
            ("shutdown", .failure(shutdown), false, { ($0 as? UndraCallError) == .cancelledByCore }),
            ("panic", .failure(panic), false, { ($0 as? UndraCallError) == .panicked(message: "stream blew up", backtrace: "frame 0") }),
            ("panic, with a domain", .failure(panic), true, { ($0 as? UndraCallError) == .panicked(message: "stream blew up", backtrace: "frame 0") }),
            ("refused", .failure(refused), true, { ($0 as? UndraCallError) == .refused(reason: "stale handle") }),
        ]
        for (name, ending, domain, check) in endings {
            let transport = FakeTransport()
            transport.onCall = { call, fake in
                fake.openStream(call.callId, items: [[1]], ending: ending)
                return true
            }
            let core = try makeCore(transport)
            let error = await failedStream(core, domain: domain)
            XCTAssertNotNil(error, name)
            XCTAssertTrue(error.map(check) ?? false, "\(name): \(String(describing: error))")
        }
    }

    func testAStreamOnAShutDownCoreEndsUnavailable() async throws {
        let core = try makeCore(FakeTransport())
        core.shutdown()
        let error = await failedStream(core, domain: true)
        XCTAssertEqual(error as? UndraCallError, .unavailable(.closed))
    }

    func testCancellingAStreamConsumerStillEndsQuietly() async throws {
        let transport = FakeTransport()
        transport.onCall = { call, fake in
            fake.openStream(call.callId, items: [[1]], holdOpen: true)
            return true
        }
        let core = try makeCore(transport)
        let stream = core.stream(
            .freeFunction(methodId: 8),
            method: 8,
            args: [],
            decode: { (body: [UInt8]) throws -> UInt8 in return body[0] },
            mapError: { UndraCallError.mapped(streamFailure: $0) }
        )
        let consumer = Task { () -> Int in
            var count = 0
            for try await _ in stream {
                count += 1
            }
            return count
        }
        let started = await waitUntil { !transport.calls.isEmpty }
        XCTAssertTrue(started)
        consumer.cancel()
        let result = await consumer.result
        switch result {
        case .success:
            break
        case .failure(let error):
            XCTFail("consumer cancellation must end the loop quietly, got \(error)")
        }
    }
}

// MARK: - report

final class ReportTests: XCTestCase {
    private func core(onError: (@Sendable (UndraUnhandledError) -> Void)?) throws -> UndraCore {
        var options = LoadOptions.inproc(adapters: Adapters.none, expectedSchemaHash: 0x1234)
        options.onError = onError
        return try UndraCore.connect(transport: FakeTransport(), options: options)
    }

    func testReportCallsTheHandlerOnceWithTheOperationAndTheMappedError() throws {
        let log = ErrorLog()
        let core = try core(onError: { log.append($0) })
        core.report(replyError(status: .badRequest, body: stringBody("closed")), operation: "Todos.toggle")
        XCTAssertEqual(log.all, [UndraUnhandledError(operation: "Todos.toggle", error: .refused(reason: "closed"))])
    }

    func testReportMapsEveryRuntimeError() throws {
        let log = ErrorLog()
        let core = try core(onError: { log.append($0) })
        core.report(UndraTransportError.closed, operation: "a")
        core.report(replyError(status: .panic, body: panicBody("p", "b")), operation: "b")
        core.report(replyError(status: .cancelled, body: []), operation: "c")
        core.report(WireError.badMagic, operation: "d")
        XCTAssertEqual(log.all.map { $0.operation }, ["a", "b", "c", "d"])
        XCTAssertEqual(log.all[0].error, .unavailable(.closed))
        XCTAssertEqual(log.all[1].error, .panicked(message: "p", backtrace: "b"))
        XCTAssertEqual(log.all[2].error, .cancelledByCore)
        guard case .malformed = log.all[3].error else {
            return XCTFail("expected .malformed")
        }
    }

    func testAnErrorThatIsNotOneOfTheRuntimesIsReportedAsMalformedNotTrapped() throws {
        struct Foreign: Error {}
        let log = ErrorLog()
        let core = try core(onError: { log.append($0) })
        core.report(Foreign(), operation: "x")
        core.report(CancellationError(), operation: "y")
        XCTAssertEqual(log.all.count, 2)
        for item in log.all {
            guard case .malformed = item.error else {
                return XCTFail("expected .malformed, got \(item.error)")
            }
        }
    }

    func testNoHandlerIsFine() throws {
        let core = try core(onError: nil)
        core.report(UndraTransportError.closed, operation: "Todos.toggle")
    }

    func testAHandlerThatReportsAgainDoesNotRecurse() throws {
        let count = Guarded<Int>(0)
        let holder = Guarded<UndraCore?>(nil)
        let core = try core(onError: { _ in
            count.withLock { (value: inout Int) -> Void in
                value += 1
            }
            let inner = holder.withLock { (value: inout UndraCore?) -> UndraCore? in
                return value
            }
            inner?.report(UndraTransportError.closed, operation: "from the handler")
        })
        holder.withLock { (value: inout UndraCore?) -> Void in
            value = core
        }
        core.report(UndraTransportError.closed, operation: "outer")
        XCTAssertEqual(count.withLock { (value: inout Int) -> Int in return value }, 1)
        // The flag is cleared again afterwards.
        core.report(UndraTransportError.closed, operation: "outer again")
        XCTAssertEqual(count.withLock { (value: inout Int) -> Int in return value }, 2)
    }

    func testATaskStartedFromTheHandlerInheritsTheGuardButADetachedOneDoesNot() async throws {
        // Documented on `LoadOptions.onError`: a `Task` started inside the handler inherits the
        // task-local "reporting" flag, so what it reports is only logged; `Task.detached` does not.
        let log = ErrorLog()
        let holder = Guarded<UndraCore?>(nil)
        let children = Guarded<[Task<Void, Never>]>([])
        let core = try core(onError: { unhandled in
            log.append(unhandled)
            guard unhandled.operation == "outer" else {
                return
            }
            let inner = holder.withLock { (value: inout UndraCore?) -> UndraCore? in
                return value
            }
            let child = Task { () -> Void in
                inner?.report(UndraTransportError.closed, operation: "from a child task")
            }
            let detached = Task.detached { () -> Void in
                inner?.report(UndraTransportError.closed, operation: "from a detached task")
            }
            children.withLock { (value: inout [Task<Void, Never>]) -> Void in
                value.append(contentsOf: [child, detached])
            }
        })
        holder.withLock { (value: inout UndraCore?) -> Void in
            value = core
        }
        core.report(UndraTransportError.closed, operation: "outer")
        let started = children.withLock { (value: inout [Task<Void, Never>]) -> [Task<Void, Never>] in
            return value
        }
        XCTAssertEqual(started.count, 2)
        for task in started {
            await task.value
        }
        XCTAssertEqual(log.all.map { $0.operation }, ["outer", "from a detached task"])
    }

    func testTheHandlerRunsOnTheCallingThread() throws {
        let seen = Guarded<[Bool]>([])
        let core = try core(onError: { _ in
            seen.withLock { (value: inout [Bool]) -> Void in
                value.append(Thread.isMainThread)
            }
        })
        core.report(UndraTransportError.closed, operation: "here")
        let done = expectation(description: "background report")
        Thread.detachNewThread {
            core.report(UndraTransportError.closed, operation: "there")
            done.fulfill()
        }
        wait(for: [done], timeout: hangDeadline)
        XCTAssertEqual(seen.withLock { (value: inout [Bool]) -> [Bool] in return value }, [Thread.isMainThread, false])
    }

    func testReportsFromManyThreadsAllArrive() throws {
        let log = ErrorLog()
        let core = try core(onError: { log.append($0) })
        let group = DispatchGroup()
        for index in 0 ..< 8 {
            group.enter()
            DispatchQueue.global().async {
                for _ in 0 ..< 25 {
                    core.report(UndraTransportError.closed, operation: "thread \(index)")
                }
                group.leave()
            }
        }
        XCTAssertEqual(group.wait(timeout: .now() + hangDeadline), .success)
        XCTAssertEqual(log.all.count, 200)
    }

    func testLoadOptionsCarryTheHandlerThroughEveryInitializer() {
        let handler: @Sendable (UndraUnhandledError) -> Void = { _ in }
        XCTAssertNil(LoadOptions.inproc(expectedSchemaHash: 1).onError)
        XCTAssertNil(LoadOptions.remote(url: "ws://127.0.0.1:1", expectedSchemaHash: 1).onError)
        XCTAssertNotNil(LoadOptions.inproc(expectedSchemaHash: 1, onError: handler).onError)
        XCTAssertNotNil(LoadOptions.remote(url: "ws://127.0.0.1:1", expectedSchemaHash: 1, onError: handler).onError)
        XCTAssertNotNil(LoadOptions(mode: .inproc, expectedSchemaHash: 1, onError: handler).onError)
    }
}

// MARK: - UndraCore.shared without a loaded core

/// ADR-032, decision 7. No test in this target loads a real core (`UndraCore.connect` never sets the
/// shared slot), so nothing is loaded here and `shared` is the placeholder.
@MainActor
final class SharedPlaceholderTests: XCTestCase {
    func testSharedWithNothingLoadedIsAShutDownPlaceholderAndCurrentStaysNil() {
        XCTAssertNil(UndraCore.current)
        let shared = UndraCore.shared
        XCTAssertTrue(shared.isShutDown)
        XCTAssertEqual(shared.mode, .inproc)
        XCTAssertNil(UndraCore.current, "the placeholder never becomes the loaded core")
        XCTAssertTrue(UndraCore.shared === shared, "one placeholder, not one per use")
    }

    func testSyncCallsOnThePlaceholderThrowClosed() {
        let shared = UndraCore.shared
        XCTAssertThrowsError(try shared.callSync(.freeFunction(methodId: 1), method: 1, args: [])) { error in
            XCTAssertEqual(error as? UndraTransportError, .closed)
            XCTAssertEqual(UndraCallError.mapped(error) as? UndraCallError, .unavailable(.closed))
        }
        XCTAssertThrowsError(try shared.construct(type: 1, method: 2, args: [])) { error in
            XCTAssertEqual(UndraCallError.mapped(error) as? UndraCallError, .unavailable(.closed))
        }
        XCTAssertThrowsError(try shared.snapshot()) { error in
            XCTAssertEqual(error as? UndraTransportError, .closed)
        }
        XCTAssertThrowsError(try shared.restore([1])) { error in
            XCTAssertEqual(error as? UndraTransportError, .closed)
        }
    }

    func testAsyncCallsAndStreamsOnThePlaceholderFailClosed() async {
        let shared = UndraCore.shared
        let error = await captureError {
            _ = try await shared.call(.freeFunction(methodId: 1), method: 1, args: [])
        }
        guard let failure = error else {
            return XCTFail("a call on the placeholder must fail")
        }
        XCTAssertEqual(UndraCallError.mapped(failure) as? UndraCallError, .unavailable(.closed))
        let stream = shared.stream(
            .freeFunction(methodId: 1),
            method: 1,
            args: [],
            decode: { (body: [UInt8]) throws -> UInt8 in return body[0] },
            mapError: { UndraCallError.mapped(streamFailure: $0) }
        )
        let streamError = await captureError {
            for try await _ in stream {}
        }
        XCTAssertEqual(streamError as? UndraCallError, .unavailable(.closed))
    }

    func testEverythingElseOnThePlaceholderIsAHarmlessNoOp() {
        let shared = UndraCore.shared
        shared.report(UndraTransportError.closed, operation: "Todos.toggle")
        shared.release(UndraHandle(rawValue: 5))
        shared.event(port: 1, method: 2, payload: [3])
        shared.timerFired(4)
        shared.observe(UndraHandle(rawValue: 5), signal: 0, on: true)
        shared.registerPort(0xAB, .sync([:]))
        shared.shutdown()
        XCTAssertEqual(shared.stats().hostRegisteredPorts, 0, "nothing registers on the placeholder")
        XCTAssertEqual(shared.stats().hostPendingCalls, 0)
        XCTAssertEqual(shared.stats().hostLiveHandles, 0)
        XCTAssertEqual(shared.stats().hostMirroredStores, 0)
        XCTAssertNil(UndraCore.current)
    }

    func testTheLifecycleReporterDefaultsToTheSharedCoreAndDoesNotTrap() {
        UndraLifecycle().changed(.active)
    }

    func testARealShutDownCoreIgnoresPortRegistration() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        core.shutdown()
        core.registerPort(0xCD, .sync([:]))
        XCTAssertEqual(core.stats().hostRegisteredPorts, 0)
        XCTAssertEqual(transport.sent, [])
    }
}
