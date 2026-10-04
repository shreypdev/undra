import Foundation
import XCTest
@testable import UndraRuntime

/// A background `URLSession` given to an adapter. URLSession answers a misuse of a background session (a WebSocket task, a task
/// delegate, a completion handler) with an Objective-C exception, which no Swift code can catch: the process aborts. So each
/// case runs in a child process (this test bundle again, filtered to one `child…` test), and the parent asserts that the child
/// exited normally and printed what the adapter answered. Before the WebSocket and Http adapters refused a background session,
/// each child died of SIGABRT, and this suite carried on and reported it.
final class BackgroundSessionTests: XCTestCase {
    /// Set in the child's environment: the `child…` tests run only there.
    private static let childKey = "UNDRA_BACKGROUND_SESSION_CHILD"
    /// The line a child prints with the adapter's answer.
    private static let answerPrefix = "ANSWER: "

    func testAWebSocketOnABackgroundSessionIsRefusedInsteadOfAborting() throws {
        guard let answer = try runChild("childWebSocketOnABackgroundSession") else { return }
        XCTAssertTrue(answer.hasPrefix("refused(nil) "), "connect answered \(answer), not WsError.refused with no status")
        XCTAssertTrue(answer.contains("background"), answer)
    }

    func testAWebSocketFromABackgroundConfigurationIsRefusedInsteadOfAborting() throws {
        guard let answer = try runChild("childWebSocketFromABackgroundConfiguration") else { return }
        XCTAssertTrue(answer.hasPrefix("refused(nil) "), "connect answered \(answer), not WsError.refused with no status")
    }

    /// The Http adapter had the same defect: `data(for:)` on a background session aborted the process the same way. It now
    /// fails the request with `HttpError.network` before a task exists.
    func testAnHttpRequestOnABackgroundSessionFailsTypedInsteadOfAborting() throws {
        guard let answer = try runChild("childHttpOnABackgroundSession") else { return }
        XCTAssertTrue(answer.hasPrefix("HttpError.network "), "the request answered \(answer), not HttpError.network")
        XCTAssertTrue(answer.contains("background"), answer)
    }

    // MARK: The children

    func testChildWebSocketOnABackgroundSession() async throws {
        try skipUnlessChild()
        let session = URLSession(configuration: .background(withIdentifier: "dev.undra.tests.ws.\(UUID().uuidString)"))
        defer { session.invalidateAndCancel() }
        await printAnswer(of: URLSessionWebSocketAdapter(session: session))
    }

    func testChildWebSocketFromABackgroundConfiguration() async throws {
        try skipUnlessChild()
        let configuration = URLSessionConfiguration.background(withIdentifier: "dev.undra.tests.ws.\(UUID().uuidString)")
        await printAnswer(of: URLSessionWebSocketAdapter(configuration: configuration))
    }

    func testChildHttpOnABackgroundSession() async throws {
        try skipUnlessChild()
        let session = URLSession(configuration: .background(withIdentifier: "dev.undra.tests.http.\(UUID().uuidString)"))
        defer { session.invalidateAndCancel() }
        let request = HttpRequest(method: .get, url: "http://127.0.0.1:9/", headers: [], body: nil, timeoutMs: 2_000)
        do {
            _ = try await HttpAdapter.perform(request, on: session)
            print("\(Self.answerPrefix)a response")
        } catch let error as UndraPortError {
            var reader = UndraReader(error.body)
            switch try? HttpError.undraDecode(&reader) {
            case .network(let message):
                print("\(Self.answerPrefix)HttpError.network \(message)")
            case let other:
                print("\(Self.answerPrefix)HttpError \(other.map { "\($0)" } ?? "undecodable")")
            }
        } catch {
            print("\(Self.answerPrefix)another error \(error)")
        }
    }

    // MARK: Helpers

    private func skipUnlessChild() throws {
        guard ProcessInfo.processInfo.environment[Self.childKey] == "1" else {
            throw XCTSkip("runs in a child process of the test of the same name without `child`")
        }
    }

    private func printAnswer(of adapter: URLSessionWebSocketAdapter) async {
        let result = await capture { () async throws(WsError) -> Bool in
            _ = try await adapter.connect(url: "ws://127.0.0.1:9/ws", protocols: [], headers: [])
            return true
        }
        switch result {
        case .success:
            print("\(Self.answerPrefix)connected")
        case .failure(.refused(let status, let message)):
            print("\(Self.answerPrefix)refused(\(status.map { "\($0)" } ?? "nil")) \(message)")
        case .failure(let other):
            print("\(Self.answerPrefix)\(other)")
        }
    }

    /// Runs `test<name>` (capitalised) of this class in a child process and returns the answer it printed. Fails when the child
    /// did not exit normally (an Objective-C exception aborts it) or printed no answer, and returns nil. The deadline only
    /// detects a hang.
    private func runChild(_ name: String) throws -> String? {
        let test = "test" + name.prefix(1).uppercased() + name.dropFirst()
        let bundle = Bundle(for: BackgroundSessionTests.self).bundleURL
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
        process.arguments = ["xctest", "-XCTest", "UndraRuntimeTests.BackgroundSessionTests/\(test)", bundle.path]
        var environment = ProcessInfo.processInfo.environment
        environment[Self.childKey] = "1"
        process.environment = environment
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        let exited = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in exited.signal() }
        try process.run()
        let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        guard exited.wait(timeout: .now() + 300) == .success else {
            process.terminate()
            XCTFail("the child \(test) did not exit within 300 s (a hang)")
            return nil
        }
        guard process.terminationReason == .exit, process.terminationStatus == 0 else {
            XCTFail("the child \(test) did not exit normally (\(process.terminationReason == .uncaughtSignal ? "signal" : "status") \(process.terminationStatus)): \(text.suffix(1_500))")
            return nil
        }
        guard let line = text.split(separator: "\n").first(where: { $0.hasPrefix(Self.answerPrefix) }) else {
            XCTFail("the child \(test) printed no answer: \(text.suffix(1_500))")
            return nil
        }
        return String(line.dropFirst(Self.answerPrefix.count))
    }
}
