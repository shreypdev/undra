import Foundation
import XCTest
@testable import UndraRuntime

// The Sse adapter's stream is the delegate of its own data task (`URLSessionTask.delegate`; ADR-047, "Amendment: the Swift
// adapter delivers chunks, 2026-10-02"). A task delegate takes the callbacks it implements and URLSession forwards the rest to
// the session's delegate. The stream implements only the response, the data and the completion, so an app that gives the
// adapter its session (ADR-060) keeps its certificate pinning, its authentication and its metrics on event streams: these
// tests show each reaching the session's delegate over TLS.

/// An HTTPS server for event streams on a certificate made for this run (`openssl`, a throwaway key in a temporary directory),
/// run with Node: `/sse` answers two events and ends, `/auth` asks for HTTP Basic credentials first.
final class TlsEventServer: @unchecked Sendable {
    let port: Int
    /// The server's certificate, DER: what a pinning delegate compares the presented one with.
    let certificate: Data
    private let process: Process
    private let stdin: Pipe
    private let directory: URL

    static let user = "undra"
    static let password = "pinned"

    private static let script = """
    import { createServer } from "node:https";
    import { readFileSync } from "node:fs";
    const [key, cert] = process.argv.slice(2);
    const expected = "Basic " + Buffer.from("\(user):\(password)").toString("base64");
    const server = createServer({ key: readFileSync(key), cert: readFileSync(cert) }, (req, res) => {
      if (req.url.startsWith("/auth") && req.headers.authorization !== expected) {
        res.writeHead(401, { "www-authenticate": 'Basic realm="undra"', "content-length": "0" });
        res.end();
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      res.end(": open\\n\\nid: 1\\ndata: a\\n\\nid: 2\\ndata: b\\n\\n");
    });
    server.listen(0, "127.0.0.1", () => console.log(`READY ${server.address().port}`));
    process.stdin.on("end", () => process.exit(0));
    process.stdin.resume();
    """

    /// `https://127.0.0.1:<port>`.
    var https: String {
        return "https://127.0.0.1:\(port)"
    }

    private init(port: Int, certificate: Data, process: Process, stdin: Pipe, directory: URL) {
        self.port = port
        self.certificate = certificate
        self.process = process
        self.stdin = stdin
        self.directory = directory
    }

    /// `openssl`: the system's, else the first on the PATH.
    private static func openssl() -> URL? {
        var candidates = [URL(fileURLWithPath: "/usr/bin/openssl")]
        candidates += (ProcessInfo.processInfo.environment["PATH"] ?? "")
            .split(separator: ":")
            .map { URL(fileURLWithPath: String($0)).appendingPathComponent("openssl") }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    private static func run(_ tool: URL, _ arguments: [String]) throws {
        let process = Process()
        process.executableURL = tool
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw RealtimeServer.ServerError(description: "\(tool.lastPathComponent) \(arguments.joined(separator: " ")) exited with \(process.terminationStatus)")
        }
    }

    /// Makes the certificate and starts the server. Without Node or `openssl` the test is skipped, unless
    /// `UNDRA_REQUIRE_TOOLCHAINS=1` makes a missing toolchain a failure.
    static func start() throws -> TlsEventServer {
        guard let node = RealtimeServer.node(), let openssl = openssl() else {
            let why = "no node or no openssl: the TLS event stream suite needs both"
            if ProcessInfo.processInfo.environment["UNDRA_REQUIRE_TOOLCHAINS"] == "1" {
                throw RealtimeServer.ServerError(description: why)
            }
            throw XCTSkip(why)
        }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("undra-tls-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let key = directory.appendingPathComponent("key.pem").path
        let cert = directory.appendingPathComponent("cert.pem").path
        let der = directory.appendingPathComponent("cert.der").path
        let script = directory.appendingPathComponent("server.mjs")
        try run(openssl, [
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert, "-days", "2",
            "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1",
        ])
        try run(openssl, ["x509", "-in", cert, "-outform", "DER", "-out", der])
        try Data(TlsEventServer.script.utf8).write(to: script)
        let process = Process()
        process.executableURL = node
        process.arguments = [script.path, key, cert]
        let stdin = Pipe()
        let stdout = Pipe()
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = FileHandle.standardError
        try process.run()
        let ready = DispatchSemaphore(value: 0)
        let line = Locked("")
        let reader = Thread {
            var text = ""
            while !text.contains("\n") {
                let data = stdout.fileHandleForReading.availableData
                if data.isEmpty {
                    break
                }
                text += String(decoding: data, as: UTF8.self)
            }
            line.withLock { $0 = text }
            ready.signal()
        }
        reader.start()
        guard ready.wait(timeout: .now() + hangDeadline) == .success,
              let first = line.withLock({ $0 }).split(separator: "\n").first,
              first.hasPrefix("READY "), let port = Int(first.dropFirst("READY ".count))
        else {
            process.terminate()
            throw RealtimeServer.ServerError(description: "the TLS event server did not print READY <port>: \(line.withLock { $0 })")
        }
        return TlsEventServer(
            port: port, certificate: try Data(contentsOf: URL(fileURLWithPath: der)), process: process, stdin: stdin, directory: directory
        )
    }

    /// Ends the server and removes the certificate and its key.
    func stop() {
        try? stdin.fileHandleForWriting.close()
        process.terminate()
        process.waitUntilExit()
        try? FileManager.default.removeItem(at: directory)
    }
}

/// What an app's session delegate saw, and how it answers: server trust by comparing the presented certificate with a pinned
/// one (`.useCredential` when they are the same, `.cancelAuthenticationChallenge` when not), HTTP Basic with the test server's
/// credentials.
class RecordingSessionDelegate: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let pinned: Data
    private let log = Locked<[String]>([])

    init(pinned: Data) {
        self.pinned = pinned
    }

    /// What reached the delegate, in order: `session <method>`, `task <method>`, `metrics <path>`.
    var seen: [String] {
        return log.withLock { $0 }
    }

    func note(_ entry: String) {
        log.withLock { $0.append(entry) }
    }

    func answer(
        _ challenge: URLAuthenticationChallenge,
        _ completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let space = challenge.protectionSpace
        switch space.authenticationMethod {
        case NSURLAuthenticationMethodServerTrust:
            guard let trust = space.serverTrust,
                  let leaf = (SecTrustCopyCertificateChain(trust) as? [SecCertificate])?.first,
                  SecCertificateCopyData(leaf) as Data == pinned
            else {
                completionHandler(.cancelAuthenticationChallenge, nil)
                return
            }
            completionHandler(.useCredential, URLCredential(trust: trust))
        case NSURLAuthenticationMethodHTTPBasic:
            completionHandler(.useCredential, URLCredential(user: TlsEventServer.user, password: TlsEventServer.password, persistence: .forSession))
        default:
            completionHandler(.performDefaultHandling, nil)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        note("metrics \(task.originalRequest?.url?.path ?? "?")")
    }
}

/// Pinning where most apps put it: the session-level challenge (`urlSession(_:didReceive:completionHandler:)`), which
/// URLSession asks for server trust.
final class SessionLevelPinningDelegate: RecordingSessionDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        note("session \(challenge.protectionSpace.authenticationMethod)")
        answer(challenge, completionHandler)
    }
}

/// Every challenge in the task-level method (`urlSession(_:task:didReceive:completionHandler:)`), which URLSession asks for
/// server trust when the session-level one is not implemented, and always for HTTP authentication.
final class TaskLevelChallengeDelegate: RecordingSessionDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        note("task \(challenge.protectionSpace.authenticationMethod)")
        answer(challenge, completionHandler)
    }
}

final class SseSessionDelegateTests: XCTestCase {
    nonisolated(unsafe) private static var server: TlsEventServer?

    private func sharedServer() throws -> TlsEventServer {
        if SseSessionDelegateTests.server == nil {
            SseSessionDelegateTests.server = try TlsEventServer.start()
        }
        return SseSessionDelegateTests.server!
    }

    override class func tearDown() {
        server?.stop()
        server = nil
        super.tearDown()
    }

    /// An app's session (ephemeral, so no credential or connection outlives the test) with `delegate`.
    private func session(_ delegate: RecordingSessionDelegate) -> URLSession {
        return URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
    }

    /// Opens `url` through the default adapter on `session` and reads the stream to its end.
    private func read(_ url: String, on session: URLSession) async -> Result<(events: [String], end: SseError?), SseError> {
        let adapter = URLSessionSseAdapter(session: session)
        do {
            let stream = try await adapter.open(url: url, headers: [], lastEventId: nil)
            var events: [String] = []
            do {
                for try await event in stream.events {
                    events.append(event.data)
                }
                return .success((events, nil))
            } catch {
                return .success((events, error as? SseError))
            }
        } catch {
            return .failure(error)
        }
    }

    /// The session delegate's pin decides the stream's server trust: the stream opens and reads, and the delegate saw the
    /// challenge and the task's metrics. (`URLSession.bytes(for:)`, the adapter's request before it read chunks, never asked a
    /// session-level delegate: the stream failed on the untrusted certificate, and a pin on a trusted one was not consulted.)
    func testTheSessionDelegatesPinDecidesTheStreamsServerTrust() async throws {
        let server = try sharedServer()
        let delegate = SessionLevelPinningDelegate(pinned: server.certificate)
        let session = session(delegate)
        defer { session.finishTasksAndInvalidate() }
        let result = await read("\(server.https)/sse", on: session)
        guard case .success(let read) = result else {
            return XCTFail("the stream did not open: \(result)")
        }
        XCTAssertEqual(read.events, ["a", "b"])
        XCTAssertEqual(read.end, .ended)
        XCTAssertEqual(delegate.seen.first, "session \(NSURLAuthenticationMethodServerTrust)")
        await eventually("the task's metrics at the session's delegate") { delegate.seen.contains("metrics /sse") }
    }

    /// A pin that does not match refuses the stream: the delegate's answer is what URLSession does, so a stream cannot get
    /// around the app's pinning.
    func testAPinThatDoesNotMatchRefusesTheStream() async throws {
        let server = try sharedServer()
        let delegate = SessionLevelPinningDelegate(pinned: Data("another certificate".utf8))
        let session = session(delegate)
        defer { session.finishTasksAndInvalidate() }
        let result = await read("\(server.https)/sse", on: session)
        guard case .failure(let error) = result, case .network = error else {
            return XCTFail("a stream opened past a pin that does not match: \(result)")
        }
        XCTAssertEqual(delegate.seen.first, "session \(NSURLAuthenticationMethodServerTrust)")
    }

    /// A background session takes no task delegate (setting one raises an Objective-C exception, "Task delegate is not supported
    /// on background session task", which no Swift code can catch: the app would abort). The adapter refuses it at `open`, typed,
    /// before any task exists. Needs no server.
    func testABackgroundSessionIsRefusedAtOpenInsteadOfAborting() async throws {
        let configuration = URLSessionConfiguration.background(withIdentifier: "dev.undra.tests.sse.\(UUID().uuidString)")
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        let adapter = URLSessionSseAdapter(session: session)
        let result = await capture { () async throws(SseError) -> Bool in
            _ = try await adapter.open(url: "http://127.0.0.1:9/sse", headers: [], lastEventId: nil)
            return true
        }
        guard case .failure(.refused(status: nil, message: let message)) = result else {
            return XCTFail("a background session was not refused: \(result)")
        }
        XCTAssertTrue(message.contains("background"), message)
    }

    /// A delegate that answers every challenge at the task level gets the stream's server trust and its HTTP authentication.
    func testATaskLevelDelegateAnswersTheStreamsTrustAndAuthentication() async throws {
        let server = try sharedServer()
        let delegate = TaskLevelChallengeDelegate(pinned: server.certificate)
        let session = session(delegate)
        defer { session.finishTasksAndInvalidate() }
        let result = await read("\(server.https)/auth", on: session)
        guard case .success(let read) = result else {
            return XCTFail("the stream did not open: \(result)")
        }
        XCTAssertEqual(read.events, ["a", "b"])
        XCTAssertEqual(read.end, .ended)
        let challenges = delegate.seen.filter { $0.hasPrefix("task ") }
        // The stream's trust first, then its HTTP authentication once. Whether the authenticated request reuses the connection of the
        // refused one is URLSession's decision (a connection that was closed in between is a new handshake, so a second trust challenge),
        // not the adapter's: any further challenge is a trust one.
        let trust = "task \(NSURLAuthenticationMethodServerTrust)"
        let basic = "task \(NSURLAuthenticationMethodHTTPBasic)"
        XCTAssertEqual(challenges.first, trust)
        XCTAssertEqual(challenges.filter { $0 == basic }.count, 1, "\(challenges)")
        XCTAssertTrue(challenges.allSatisfy { $0 == trust || $0 == basic }, "\(challenges)")
        await eventually("the task's metrics at the session's delegate") { delegate.seen.contains("metrics /auth") }
    }
}
