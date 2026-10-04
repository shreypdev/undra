// WebSocket (ADR-047) on URLSessionWebSocketTask.

import Foundation

/// `WebSocket` on `URLSessionWebSocketTask`.
///
/// Each connection has a session of its own, whose delegate learns when the handshake succeeded
/// (and which subprotocol the server chose) and the code and reason of the peer's close frame.
/// The subprotocols go out as `Sec-WebSocket-Protocol` on a `URLRequest` that carries the headers.
///
/// **An app that has a network stack of its own passes its `URLSession`** (``init(session:maximumMessageSize:)``,
/// ADR-060): the connections are tasks of that session, so its configuration (`httpAdditionalHeaders`, `protocolClasses`,
/// proxy, cookie storage), its delegate (server trust and the pinning built on it, authentication challenges) and its
/// delegate queue apply to the upgrade request, and the adapter never invalidates it. The adapter is the task's delegate for
/// the handshake and the close frame (`URLSessionTask.delegate`, iOS 15), which leaves the session's delegate to answer
/// everything else.
///
/// Inbound messages are pulled: `messages` calls `receive()` once per message the binding asks
/// for, and the binding asks only while the core's window has room, so a core that stops reading
/// stops the socket and TCP pushes back on the server.
///
/// How a connection ends: the first `receive` or `send` that fails after the peer's close frame is
/// ``WsError/closed(code:reason:)`` with the frame's code and reason; a failed upgrade is
/// ``WsError/refused(status:message:)`` with the HTTP status; a frame that breaks RFC 6455 (a
/// text frame that is not UTF-8) is ``WsError/protocol(_:)``; any other failure is
/// ``WsError/network(_:)``.
///
/// It is also the `WebSocket` adapter of ``Adapters/platformDefault`` (served by the binding of
/// ``WebSocketPortAdapter``).
public final class URLSessionWebSocketAdapter: WebSocketAdapter, UndraAdapter, @unchecked Sendable {
    /// Where a connection's session comes from.
    private enum Source {
        /// A session of its own per connection, made from this configuration.
        case configuration(URLSessionConfiguration)
        /// The app's session, which the adapter does not own.
        case session(URLSession)
    }

    private let source: Source
    private let maximumMessageSize: Int
    private let bindings = BindingSet<WebSocketBinding>()

    /// Opens connections with sessions made from `configuration` (copied), accepting messages up
    /// to `maximumMessageSize` bytes (URLSession's default is 1 MiB).
    ///
    /// A background configuration (`URLSessionConfiguration.background(withIdentifier:)`) cannot run a WebSocket task:
    /// `connect` refuses it with ``WsError/refused(status:message:)`` and no status.
    public init(configuration: URLSessionConfiguration = .default, maximumMessageSize: Int = 16 * 1024 * 1024) {
        self.source = .configuration(configuration.copy() as? URLSessionConfiguration ?? .default)
        self.maximumMessageSize = maximumMessageSize
    }

    /// Opens connections as tasks of `session`, the app's own, accepting messages up to `maximumMessageSize` bytes.
    ///
    /// The session's delegate, configuration and delegate queue apply to every connection (a delegate that pins certificates or
    /// answers authentication challenges sees the upgrade request as it sees any other); the adapter takes the connection's task
    /// delegate for the handshake and the close frame and never invalidates `session`.
    ///
    /// A background session (made from `URLSessionConfiguration.background(withIdentifier:)`) cannot run a WebSocket task
    /// and takes no task delegate; asking it for either raises an Objective-C exception that no Swift code can catch, and the
    /// app aborts. So `connect` refuses such a session before a task exists, with ``WsError/refused(status:message:)`` and no
    /// status. Pass a default or ephemeral session.
    public init(session: URLSession, maximumMessageSize: Int = 16 * 1024 * 1024) {
        self.source = .session(session)
        self.maximumMessageSize = maximumMessageSize
    }

    /// Opens a `URLSessionWebSocketTask` to `url` with `headers` and `protocols` and returns once
    /// the handshake succeeded (the delegate's `didOpenWithProtocol`).
    public func connect(url: String, protocols: [String], headers: [Header]) async throws(WsError) -> any WebSocketConnection {
        guard let target = URL(string: url), let scheme = target.scheme?.lowercased(), scheme == "ws" || scheme == "wss",
              target.host != nil
        else {
            throw WsError.refused(status: nil, message: "invalid URL: \(url)")
        }
        // A background session (the only kind with an identifier) runs no WebSocket task and takes no task delegate: asking
        // for either raises an Objective-C exception that would abort the app, so the connection is refused before a task exists.
        let identifier: String?
        switch source {
        case .configuration(let configuration):
            identifier = configuration.identifier
        case .session(let session):
            identifier = session.configuration.identifier
        }
        guard identifier == nil else {
            throw WsError.refused(
                status: nil,
                message: "a background URLSession cannot carry a WebSocket (it runs no WebSocket task): "
                    + "give URLSessionWebSocketAdapter a default or ephemeral session"
            )
        }
        var request = URLRequest(url: target)
        for header in headers {
            request.addValue(header.value, forHTTPHeaderField: header.name)
        }
        if !protocols.isEmpty {
            request.setValue(protocols.joined(separator: ", "), forHTTPHeaderField: "Sec-WebSocket-Protocol")
        }
        let connection: URLSessionWebSocketConnection
        switch source {
        case .configuration(let configuration):
            connection = URLSessionWebSocketConnection(
                request: request,
                configuration: configuration.copy() as? URLSessionConfiguration ?? .default,
                maximumMessageSize: maximumMessageSize
            )
        case .session(let session):
            connection = URLSessionWebSocketConnection(request: request, session: session, maximumMessageSize: maximumMessageSize)
        }
        try await connection.open()
        return connection
    }

    // MARK: UndraAdapter

    public var portId: UInt32 {
        return StandardPorts.WebSocket.portId
    }

    public func makePortImpl(core: UndraCore) -> PortImpl? {
        return bindings.add(WebSocketBinding(adapter: self)).portImpl()
    }

    public func detach() {
        bindings.detachAll()
    }
}

/// One `URLSessionWebSocketTask` and its delegate: of its own session, or of the app's.
final class URLSessionWebSocketConnection: NSObject, WebSocketConnection, URLSessionWebSocketDelegate, @unchecked Sendable {
    private struct State {
        var opening: CheckedContinuation<Result<String, WsError>, Never>?
        var opened = false
        var negotiated = ""
        /// The peer's close frame, as the delegate reported it.
        var peerClose: (code: UInt16, reason: String)?
        /// The task completed (the delegate's last word).
        var completed = false
        var closing = false
    }

    private let state = Guarded(State())
    private var session: URLSession!
    private var task: URLSessionWebSocketTask!

    /// Whether `session` is this connection's own, to be invalidated when the connection ends. The app's never is.
    private let ownsSession: Bool

    init(request: URLRequest, configuration: URLSessionConfiguration, maximumMessageSize: Int) {
        ownsSession = true
        super.init()
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        // The session keeps its delegate (this object) until it is invalidated, which `close`
        // and the end of the task do.
        session = URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
        task = session.webSocketTask(with: request)
        task.maximumMessageSize = maximumMessageSize
    }

    /// A connection that is a task of the app's `session` (ADR-060): the session's delegate keeps answering challenges and the
    /// like, this object is the task's delegate for the handshake and the close frame.
    init(request: URLRequest, session: URLSession, maximumMessageSize: Int) {
        ownsSession = false
        super.init()
        self.session = session
        task = session.webSocketTask(with: request)
        task.maximumMessageSize = maximumMessageSize
        task.delegate = self
    }

    /// Lets go of what the connection holds after it ended: its own session (the app's is left alone).
    private func release(cancelling: Bool = false) {
        if ownsSession {
            if cancelling {
                session.invalidateAndCancel()
            } else {
                session.finishTasksAndInvalidate()
            }
        } else if cancelling {
            task.cancel()
        }
    }

    /// Starts the handshake and waits for its outcome.
    func open() async throws(WsError) {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<String, WsError>, Never>) in
            state.withLock { (current: inout State) -> Void in
                current.opening = continuation
            }
            task.resume()
        }
        switch outcome {
        case .success:
            return
        case .failure(let error):
            release(cancelling: true)
            throw error
        }
    }

    // MARK: WebSocketConnection

    var negotiatedProtocol: String {
        return state.withLock { (current: inout State) -> String in
            return current.negotiated
        }
    }

    var messages: AsyncThrowingStream<WsMessage, any Error> {
        return AsyncThrowingStream(unfolding: { [self] () async throws -> WsMessage? in
            return try await self.receiveNext()
        })
    }

    func send(_ message: WsMessage) async throws(WsError) {
        let frame: URLSessionWebSocketTask.Message
        switch message {
        case .text(let text):
            frame = .string(text)
        case .binary(let bytes):
            frame = .data(Data(bytes))
        }
        do {
            try await task.send(frame)
        } catch {
            throw await failure(after: error)
        }
    }

    func close(code: UInt16, reason: String) async {
        let first = state.withLock { (current: inout State) -> Bool in
            if current.closing {
                return false
            }
            current.closing = true
            return true
        }
        guard first else {
            return
        }
        let closeCode = URLSessionWebSocketTask.CloseCode(rawValue: Int(code)) ?? .normalClosure
        task.cancel(with: closeCode, reason: reason.isEmpty ? nil : Data(reason.utf8))
        // Lets the close frame go out, then releases the delegate.
        release()
    }

    // MARK: Receiving

    /// The next message, `nil` after `close`, or the typed end.
    private func receiveNext() async throws -> WsMessage? {
        if isClosing {
            return nil
        }
        let message: URLSessionWebSocketTask.Message
        do {
            message = try await task.receive()
        } catch {
            if isClosing {
                return nil
            }
            throw await failure(after: error)
        }
        switch message {
        case .string(let text):
            return .text(text)
        case .data(let data):
            return .binary([UInt8](data))
        @unknown default:
            throw WsError.protocol("an unknown kind of WebSocket message")
        }
    }

    private var isClosing: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.closing
        }
    }

    /// What a failed `receive` or `send` means. The delegate may hear of the peer's close frame
    /// just after the call failed, so this waits (briefly) for its word first.
    private func failure(after error: any Error) async -> WsError {
        for _ in 0 ..< 100 {
            let settled = state.withLock { (current: inout State) -> Bool in
                return current.peerClose != nil || current.completed
            }
            if settled {
                break
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        if let close = state.withLock({ (current: inout State) -> (code: UInt16, reason: String)? in current.peerClose }) {
            return .closed(code: close.code, reason: close.reason)
        }
        let code = task.closeCode.rawValue
        if code != 0 {
            let reason = task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? ""
            return .closed(code: UInt16(clamping: code), reason: reason)
        }
        let nsError = error as NSError
        if nsError.domain == NSPOSIXErrorDomain && nsError.code == Int(EPROTO) {
            // Network.framework's verdict on a frame that breaks RFC 6455, a text frame that is
            // not UTF-8 among them.
            return .protocol("the server sent a frame that breaks RFC 6455 (\(nsError.localizedDescription))")
        }
        return .network(nsError.localizedDescription)
    }

    // MARK: URLSessionWebSocketDelegate

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol negotiated: String?) {
        let opening = state.withLock { (current: inout State) -> CheckedContinuation<Result<String, WsError>, Never>? in
            current.opened = true
            current.negotiated = negotiated ?? ""
            let taken = current.opening
            current.opening = nil
            return taken
        }
        opening?.resume(returning: .success(negotiated ?? ""))
    }

    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didCloseWith closeCode: URLSessionWebSocketTask.CloseCode,
        reason: Data?
    ) {
        state.withLock { (current: inout State) -> Void in
            if current.peerClose == nil {
                current.peerClose = (
                    UInt16(clamping: closeCode.rawValue),
                    reason.map { String(decoding: $0, as: UTF8.self) } ?? ""
                )
            }
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
        let opening = state.withLock { (current: inout State) -> CheckedContinuation<Result<String, WsError>, Never>? in
            current.completed = true
            let taken = current.opening
            current.opening = nil
            return taken
        }
        if let opening = opening {
            opening.resume(returning: .failure(URLSessionWebSocketConnection.refusal(task: task, error: error)))
        }
        // The task is over: let the session (and with it this delegate) go.
        release()
    }

    /// Why a handshake failed: the HTTP status of a refused upgrade, else a network failure.
    static func refusal(task: URLSessionTask, error: (any Error)?) -> WsError {
        if let response = task.response as? HTTPURLResponse, response.statusCode != 101 {
            let status = response.statusCode
            let text = HTTPURLResponse.localizedString(forStatusCode: status)
            return .refused(status: UInt16(clamping: status), message: "the server answered the upgrade with HTTP \(status) (\(text))")
        }
        if let urlError = error as? URLError, urlError.code == .badURL || urlError.code == .unsupportedURL {
            return .refused(status: nil, message: "invalid URL: \(task.originalRequest?.url?.absoluteString ?? "")")
        }
        if let error = error {
            return .network((error as NSError).localizedDescription)
        }
        return .network("the connection ended before the handshake")
    }
}
