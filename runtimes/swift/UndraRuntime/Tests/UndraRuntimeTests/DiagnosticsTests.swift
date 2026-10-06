import Foundation
import XCTest
@testable import UndraRuntime

/// ADR-046, decision 4: the panic report records and their wire layout, the `Diagnostics` port, and what
/// the runtime does with a report (`LoadOptions.onPanic`, the log line without one).

/// Golden vectors shared with `crates/undra-ports/tests/encoding.rs` (`panic_frame_*`, `panic_report_*`,
/// `background_report_*`): the same bytes are asserted there for the Rust types.
private enum Golden {
    static let bareFrame = "2211000000000000 00 00 00"
    static let namedFrame = "0100000000000000 01 01000000 66 01 04000000 612e7273 01 07000000"
    static let report = """
        01000000 6d 01000000 6c 01000000 6f 01000000 74 \
        01000000 0200000000000000 00 00 01 01000000 \
        01000000 6e 03000000 312e30 0201000000000000 02000000 6162
        """
    static let emptyReport = "00000000 00000000 00000000 00000000 00000000 00000000 00000000 0000000000000000 00000000"
    static let background = "01 01000000 02000000 03000000"
}

final class PanicRecordsTests: XCTestCase {
    private func sampleReport() -> UndraPanicReport {
        return UndraPanicReport(
            message: "m",
            location: "l",
            operation: "o",
            thread: "t",
            frames: [UndraPanicFrame(address: 2, symbol: nil, file: nil, line: 1)],
            namespace: "n",
            coreVersion: "1.0",
            schemaHash: 0x0102,
            imageId: "ab"
        )
    }

    // MARK: Wire layout

    func testAPanicFrameIsAddressSymbolFileLine() {
        assertCodec(UndraPanicFrame(address: 0x1122), hex: Golden.bareFrame, "bare frame")
        assertCodec(UndraPanicFrame(address: 1, symbol: "f", file: "a.rs", line: 7), hex: Golden.namedFrame, "named frame")
    }

    func testAPanicReportIsNineFieldsInDeclarationOrder() {
        assertCodec(sampleReport(), hex: Golden.report, "report")
        let empty = UndraPanicReport(
            message: "", location: "", operation: "", thread: "", frames: [], namespace: "", coreVersion: "", schemaHash: 0, imageId: ""
        )
        assertCodec(empty, hex: Golden.emptyReport, "empty report")
    }

    func testABackgroundReportIsABoolAndThreeCounts() {
        assertCodec(
            UndraBackgroundReport(finished: true, replayed: 1, refetched: 2, stillPending: 3),
            hex: Golden.background,
            "background report"
        )
        // A bool is 0 or 1 and nothing else.
        expectWireError(.invalidTag(tag: 2, at: 0, type: "bool")) {
            _ = try UndraBackgroundReport.undraDecoded(from: hexToBytes("02 00000000 00000000 00000000"))
        }
    }

    func testTruncatedInputIsRejectedAtEveryLengthAndTrailingBytesToo() throws {
        func rejectsEveryPrefix<T: UndraCodec>(_ type: T.Type, _ hex: String) throws {
            let bytes = hexToBytes(hex)
            for length in 0..<bytes.count {
                XCTAssertThrowsError(try T.undraDecoded(from: Array(bytes[0..<length])), "\(T.self) cut at \(length) of \(bytes.count)") { error in
                    XCTAssertTrue(error is WireError, "\(error)")
                }
            }
            XCTAssertThrowsError(try T.undraDecoded(from: bytes + [0]), "\(T.self) with a trailing byte") { error in
                XCTAssertEqual(error as? WireError, .trailingBytes(count: 1))
            }
        }
        try rejectsEveryPrefix(UndraPanicFrame.self, Golden.namedFrame)
        try rejectsEveryPrefix(UndraPanicFrame.self, Golden.bareFrame)
        try rejectsEveryPrefix(UndraPanicReport.self, Golden.report)
        try rejectsEveryPrefix(UndraBackgroundReport.self, Golden.background)
    }

    func testAFrameCountLargerThanTheInputIsRejectedNotAllocated() {
        // The frame count claims four billion frames in 8 bytes.
        let bytes = hexToBytes("00000000 00000000 00000000 00000000 ffffffff")
        XCTAssertThrowsError(try UndraPanicReport.undraDecoded(from: bytes)) { error in
            XCTAssertEqual(error as? WireError, .lengthTooLarge(len: UInt32.max, at: 16))
        }
    }

    func testTheRecordsRoundTripWhatTheCoreCanSend() throws {
        let report = UndraPanicReport(
            message: "boom: caf\u{E9}\nsecond line",
            location: "crates/lab/src/lab.rs:42:9",
            operation: "computed Todos.visible",
            thread: "undra-core",
            frames: [
                UndraPanicFrame(address: UInt64.max, symbol: "playground_core::lab::explode", file: "lab.rs", line: UInt32.max),
                UndraPanicFrame(address: 0),
            ],
            namespace: "playground_core",
            coreVersion: "0.9.1",
            schemaHash: 0x5324_1303_b2d0_8c5e,
            imageId: "0123456789abcdef0123456789abcdef"
        )
        XCTAssertEqual(try UndraPanicReport.undraDecoded(from: report.undraEncoded()), report)
        let json = try JSONEncoder().encode(report)
        XCTAssertEqual(try JSONDecoder().decode(UndraPanicReport.self, from: json), report)
        let background = UndraBackgroundReport(finished: false, replayed: UInt32.max, refetched: 0, stillPending: 7)
        XCTAssertEqual(try UndraBackgroundReport.undraDecoded(from: background.undraEncoded()), background)
        XCTAssertEqual(try JSONDecoder().decode(UndraBackgroundReport.self, from: JSONEncoder().encode(background)), background)
    }

    // MARK: Ids

    func testTheDiagnosticsAndBackgroundIdsAreThePinnedOnes() {
        // The same values `crates/undra-ports/tests/ids.rs` pins for the other runtimes.
        XCTAssertEqual(StandardPorts.Diagnostics.portId, 0xab68_cd7c)
        XCTAssertEqual(StandardPorts.Diagnostics.panicked, 0xbd14_7e2e)
        XCTAssertEqual(StandardFunctions.runBackground, 0x0e5b_14ff)
        XCTAssertEqual(StandardPorts.Diagnostics.portId, fnv1a32("port.Diagnostics"))
        XCTAssertEqual(StandardPorts.describe(portId: StandardPorts.Diagnostics.portId), "Diagnostics")
        XCTAssertEqual(StandardPorts.describe(methodId: StandardPorts.Diagnostics.panicked), "Diagnostics.panicked")
    }
}

// MARK: - The Diagnostics port

@MainActor
final class DiagnosticsAdapterTests: XCTestCase {
    private func report(_ operation: String, message: String = "boom", location: String = "lab.rs:1:1") -> UndraPanicReport {
        return UndraPanicReport(
            message: message,
            location: location,
            operation: operation,
            thread: "undra-core",
            frames: [UndraPanicFrame(address: 0x10, symbol: "f", file: "lab.rs", line: 1)],
            namespace: "playground_core",
            coreVersion: "1.0.0",
            schemaHash: 0x1234,
            imageId: "ab12"
        )
    }

    /// A core over `transport` with the default Diagnostics adapter and `onPanic`.
    private func load(
        _ transport: FakeTransport,
        onPanic: (@Sendable (UndraPanicReport) -> Void)?
    ) throws -> UndraCore {
        let options = LoadOptions.inproc(
            adapters: Adapters([DiagnosticsAdapter()]),
            expectedSchemaHash: 0x1234,
            onPanic: onPanic
        )
        return try UndraCore.connect(transport: transport, options: options)
    }

    private func panicCall(_ transport: FakeTransport, _ report: UndraPanicReport, callId: UInt32 = 0) -> PortCallOutcome {
        return transport.callPort(
            portId: StandardPorts.Diagnostics.portId,
            methodId: StandardPorts.Diagnostics.panicked,
            portCallId: callId,
            args: report.undraEncoded()
        )
    }

    private func assertAnsweredOk(_ outcome: PortCallOutcome, file: StaticString = #filePath, line: UInt = #line) throws {
        guard case .sync(let bytes) = outcome else {
            return XCTFail("a sync port answers inline, got \(outcome)", file: file, line: line)
        }
        let reply = try Wire.PortReply.decode(bytes)
        XCTAssertEqual(reply.status, .ok, file: file, line: line)
        XCTAssertEqual(Array(reply.body), [], file: file, line: line)
    }

    func testTheAdapterIsTheDiagnosticsPortAndIsRegisteredWithTheCore() throws {
        let transport = FakeTransport()
        let core = try load(transport, onPanic: nil)
        XCTAssertEqual(DiagnosticsAdapter().portId, 0xab68_cd7c)
        XCTAssertEqual(transport.sent, [.registerPort(StandardPorts.Diagnostics.portId)])
        core.shutdown()
    }

    func testAReportIsAnsweredAtOnceAndHandedToOnPanicOnTheMainThreadLater() throws {
        let transport = FakeTransport()
        let got = Guarded<[(UndraPanicReport, Bool)]>([])
        let delivered = expectation(description: "onPanic ran")
        let core = try load(transport, onPanic: { (report: UndraPanicReport) -> Void in
            got.withLock { (all: inout [(UndraPanicReport, Bool)]) -> Void in
                all.append((report, Thread.isMainThread))
            }
            delivered.fulfill()
        })
        let sent = report("Todos.add")
        // The port call is answered before the handler runs: the core may hold its lock here.
        try assertAnsweredOk(panicCall(transport, sent))
        XCTAssertEqual(got.withLock { $0.count }, 0, "the handler never runs on the thread that called the port")
        wait(for: [delivered], timeout: hangDeadline)
        let all = got.withLock { $0 }
        XCTAssertEqual(all.count, 1)
        XCTAssertEqual(all.first?.0, sent)
        XCTAssertEqual(all.first?.1, true, "the handler runs on the main thread")
        core.shutdown()
    }

    func testReportsFromAnyThreadArriveOnceEachAndInOrder() throws {
        let transport = FakeTransport()
        let seen = Guarded<[String]>([])
        let all = expectation(description: "five reports")
        all.expectedFulfillmentCount = 5
        let core = try load(transport, onPanic: { (report: UndraPanicReport) -> Void in
            XCTAssertTrue(Thread.isMainThread)
            seen.withLock { (names: inout [String]) -> Void in
                names.append(report.operation)
            }
            all.fulfill()
        })
        // The core panics on its own thread (here: a background queue), one report after the other.
        let done = expectation(description: "sent")
        DispatchQueue.global().async {
            for name in ["a", "b", "c", "d", "e"] {
                let outcome = transport.callPort(
                    portId: StandardPorts.Diagnostics.portId,
                    methodId: StandardPorts.Diagnostics.panicked,
                    portCallId: 0,
                    args: UndraPanicReport(
                        message: "m", location: "l", operation: name, thread: "t", frames: [],
                        namespace: "n", coreVersion: "v", schemaHash: 1, imageId: ""
                    ).undraEncoded()
                )
                XCTAssertEqual(outcome == .unavailable, false)
            }
            done.fulfill()
        }
        wait(for: [done, all], timeout: hangDeadline)
        XCTAssertEqual(seen.withLock { $0 }, ["a", "b", "c", "d", "e"])
        core.shutdown()
    }

    func testWithoutOnPanicTheRuntimeLogsOneErrorLineAndStillAnswersOk() throws {
        let recorder = LogRecorder(self)
        let transport = FakeTransport()
        let core = try load(transport, onPanic: nil)
        try assertAnsweredOk(panicCall(transport, report("Todos.add", message: "index out of bounds:\nthe len is 0", location: "todos.rs:42:9")))
        let lines = recorder.errors(containing: "Todos.add")
        XCTAssertEqual(lines.count, 1, "\(lines)")
        let line = try XCTUnwrap(lines.first)
        XCTAssertTrue(line.contains("index out of bounds: the len is 0"), line)
        XCTAssertTrue(line.contains("todos.rs:42:9"), line)
        XCTAssertFalse(line.contains("\n"), "one line")
        core.shutdown()
    }

    func testWithOnPanicTheRuntimeLogsNothingItself() throws {
        let recorder = LogRecorder(self)
        let transport = FakeTransport()
        let delivered = expectation(description: "onPanic ran")
        let core = try load(transport, onPanic: { _ in delivered.fulfill() })
        try assertAnsweredOk(panicCall(transport, report("Todos.add")))
        wait(for: [delivered], timeout: hangDeadline)
        XCTAssertEqual(recorder.errors(containing: "Todos.add"), [])
        core.shutdown()
    }

    func testAMalformedArgumentIsLoggedAndAnsweredOkNeverThrown() throws {
        let recorder = LogRecorder(self)
        let transport = FakeTransport()
        let calls = Guarded<Int>(0)
        let core = try load(transport, onPanic: { _ in
            calls.withLock { (count: inout Int) -> Void in count += 1 }
        })
        for args: [UInt8] in [[], [1, 2, 3], Array(report("x").undraEncoded().dropLast()), report("x").undraEncoded() + [9]] {
            let outcome = transport.callPort(
                portId: StandardPorts.Diagnostics.portId,
                methodId: StandardPorts.Diagnostics.panicked,
                portCallId: 0,
                args: args
            )
            try assertAnsweredOk(outcome)
        }
        XCTAssertEqual(recorder.errors(containing: "Diagnostics.panicked", "does not decode").count, 4)
        // Let the main queue run: nothing was queued for the handler.
        let drained = expectation(description: "main queue drained")
        DispatchQueue.main.async { drained.fulfill() }
        wait(for: [drained], timeout: hangDeadline)
        XCTAssertEqual(calls.withLock { $0 }, 0)
        core.shutdown()
    }

    func testAnUnknownMethodOfThePortIsUnavailable() throws {
        let transport = FakeTransport()
        let core = try load(transport, onPanic: nil)
        let outcome = transport.callPort(portId: StandardPorts.Diagnostics.portId, methodId: 1, portCallId: 0, args: [])
        XCTAssertEqual(outcome, .unavailable)
        core.shutdown()
    }
}

// MARK: - Stats

final class BackgroundStatsTests: XCTestCase {
    func testTheStatsDocumentCarriesPanicReportsAndTheBackgroundCounters() {
        let stats = UndraStats(json: """
            {"panics": 3, "panic_reports": 3, "tasks": 5,
             "background": {"tasks": 3, "pending": 2, "runs": 4, "finished": 1, "replayed": 1, "refetched": 6}}
            """)
        XCTAssertEqual(stats.corePanics, 3)
        XCTAssertEqual(stats.panicReports, 3)
        XCTAssertEqual(stats.coreTasks, 5, "the executor's tasks are not the background tasks")
        XCTAssertEqual(
            stats.background,
            UndraBackgroundStats(tasks: 3, pending: 2, runs: 4, finished: 1, replayed: 1, refetched: 6)
        )
        XCTAssertEqual(stats.values["background.pending"], 2)
    }

    func testACoreWithoutThemReportsZeros() {
        let stats = UndraStats(json: "{\"panics\": 1, \"tasks\": 2}")
        XCTAssertEqual(stats.panicReports, 0)
        XCTAssertEqual(stats.background, UndraBackgroundStats())
        XCTAssertEqual(UndraStats().background.pending, 0)
        let partial = UndraStats(json: "{\"background\": {\"pending\": 4}}")
        XCTAssertEqual(partial.background.pending, 4)
        XCTAssertEqual(partial.background.tasks, 0)
        // A document that is not JSON at all still yields zeros.
        XCTAssertEqual(UndraStats(json: "not json").background, UndraBackgroundStats())
    }

    @MainActor
    func testStatsOfALoadedCoreReadTheDocument() throws {
        let transport = FakeTransport()
        transport.statsDocument = "{\"panic_reports\": 2, \"background\": {\"pending\": 1, \"tasks\": 3}}"
        let core = try makeCore(transport)
        XCTAssertEqual(core.stats().panicReports, 2)
        XCTAssertEqual(core.stats().background.pending, 1)
        XCTAssertEqual(core.pendingBackgroundWork(), 1)
        core.shutdown()
    }
}
