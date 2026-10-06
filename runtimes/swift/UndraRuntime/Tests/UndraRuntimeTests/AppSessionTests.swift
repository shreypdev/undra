import Foundation
import XCTest
@testable import UndraRuntime

/// An app's own `URLSession` behind the three network adapters (ADR-060): `HttpAdapter(session:)`, `URLSessionSseAdapter(session:)` and
/// `URLSessionWebSocketAdapter(session:)` (whose suite on an app's session is `URLSessionWebSocketOnAppSessionTests`). The session has a
/// configuration of its own and a delegate that records the tasks it ran, the way a delegate that pins certificates or answers
/// authentication challenges sits beside the tasks: every request the core makes goes through it, with nothing Undra-specific configured.
final class AppSessionTests: XCTestCase {
    private var server: RealtimeServer!
    private var delegate: MetricsRecordingSessionDelegate!
    private var session: URLSession!

    override func setUpWithError() throws {
        if RealtimeAdapterServer.current == nil {
            RealtimeAdapterServer.current = try RealtimeServer.start()
        }
        server = RealtimeAdapterServer.current
        delegate = MetricsRecordingSessionDelegate()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpAdditionalHeaders = ["X-Traced": "yes"]
        session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
    }

    override func tearDown() {
        session?.invalidateAndCancel()
    }

    /// Waits until the session's delegate was told of the task for `path`.
    private func delegateSaw(_ path: String, file: StaticString = #filePath, line: UInt = #line) async throws {
        let deadline = Date().addingTimeInterval(hangDeadline)
        while !delegate.paths.contains(path), Date() < deadline {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertTrue(delegate.paths.contains(path), "the session's delegate saw \(delegate.paths)", file: file, line: line)
    }

    func testHttpGoesThroughTheAppsSessionConfigurationAndDelegate() async throws {
        let request = HttpRequest(
            method: .get,
            url: "\(server.http)/undra-rn-check",
            headers: [Header(name: "X-Core", value: "1")],
            body: nil,
            timeoutMs: 5000
        )
        let reply = try await HttpAdapter.perform(request, on: session)
        let response = try HttpResponse.undraDecoded(from: reply)
        XCTAssertEqual(response.status, 200)
        let echoed = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(response.body)) as? [String: Any])
        let headers = try XCTUnwrap(echoed["headers"] as? [String: Any])
        // The session's configuration put its header on the core's request, beside the core's own.
        XCTAssertEqual(headers["x-traced"] as? String, "yes")
        XCTAssertEqual(headers["x-core"] as? String, "1")
        try await delegateSaw("/undra-rn-check")
    }

    func testTheEventStreamGoesThroughTheAppsSessionConfigurationAndDelegate() async throws {
        let binding = SseBinding(adapter: URLSessionSseAdapter(session: session))
        defer {
            binding.detach()
        }
        let stream = try await binding.open(url: "\(server.http)/sse/feed", headers: [Header(name: "X-Token", value: "t")], lastEventId: nil)
        var events: [SseEvent] = []
        while true {
            do {
                events += try await binding.next(stream: stream, max: 16)
            } catch {
                XCTAssertEqual(error, .ended)
                break
            }
        }
        XCTAssertEqual(events, SseParserTests.feedEvents)
        let seen = try await server.last("/sse/feed")
        XCTAssertEqual(seen?.headers["x-traced"], "yes")
        XCTAssertEqual(seen?.headers["x-token"], "t")
        try await delegateSaw("/sse/feed")
    }

    func testAWebSocketGoesThroughItToo() async throws {
        let binding = WebSocketBinding(adapter: URLSessionWebSocketAdapter(session: session))
        defer {
            binding.detach()
        }
        let conn = try await binding.connect(url: "\(server.ws)/ws/echo", protocols: [], headers: []).conn
        try await binding.send(conn: conn, message: .text("hello"))
        let echoed = try await binding.receive(conn: conn, max: 1)
        XCTAssertEqual(echoed, [.text("hello")])
        let seen = try await server.last("/ws/echo")
        XCTAssertEqual(seen?.headers["x-traced"], "yes")
        try await binding.close(conn: conn, code: 1000, reason: "")
        try await delegateSaw("/ws/echo")
    }
}
