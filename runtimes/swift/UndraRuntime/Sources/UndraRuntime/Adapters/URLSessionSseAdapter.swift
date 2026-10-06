// Server-sent events (ADR-047) on a URLSession data task and the runtime's SseParser.

import Foundation

/// `Sse` on a `URLSession` data task, parsed by ``SseParser`` (the HTML standard's algorithm).
///
/// The request carries `Accept: text/event-stream`, `Cache-Control: no-cache`, the core's headers
/// and, when resuming, `Last-Event-ID`. `open` returns once the answer's head arrived: a status
/// other than 2xx is ``SseError/refused(status:message:)`` with it (a 204 means "stop"), a content
/// type other than `text/event-stream` is ``SseError/protocol(_:)``. The body arrives in the chunks
/// URLSession hands over (a task delegate receives them), each chunk is parsed at once, and the
/// events wait for the binding to pull them; its end is ``SseError/ended``, a failure while
/// reading ``SseError/network(_:)``, bytes that are not UTF-8 ``SseError/protocol(_:)``. `close`
/// cancels the request, so the server sees the client leave.
///
/// The read-ahead follows the core's pulls: while enough events wait for the binding, the data task
/// is suspended, so URLSession stops reading the socket and TCP pushes back on the server; it is
/// resumed when the binding takes the queue below the mark again.
///
/// It is also the `Sse` adapter of ``Adapters/platformDefault`` (served by the binding of
/// ``SsePortAdapter``).
///
/// Each stream is its data task's own delegate (`URLSessionTask.delegate`) for the response, the
/// body and the end only; URLSession forwards every other callback to the `session`'s delegate, so
/// an app's session keeps its certificate pinning (`urlSession(_:didReceive:completionHandler:)`
/// or the task-level challenge), its HTTP authentication, its redirects and its metrics on event
/// streams (`SseSessionDelegateTests`).
///
/// The chunks are parsed on the `session`'s delegate queue, which must be serial, as Apple asks of
/// any delegate queue and as URLSession makes the queue it creates itself (the default session's,
/// and any session made without one). Give the adapter a session whose queue is not the main one,
/// or the main thread parses the event streams. A background session takes no task delegate, so
/// `open` refuses it (``SseError/refused(status:message:)`` with no status).
public final class URLSessionSseAdapter: SseAdapter, UndraAdapter, @unchecked Sendable {
    private let session: URLSession
    private let bindings = BindingSet<SseBinding>()

    /// Streams over `session`. The default one never times out an idle stream (an event stream
    /// may stay quiet for minutes) and does not cache.
    public init(session: URLSession = URLSessionSseAdapter.makeDefaultSession()) {
        self.session = session
    }

    /// The session used when none is passed: ephemeral, no request timeout while the stream is
    /// quiet, no waiting for connectivity, and no practical limit on connections per host (an
    /// open stream holds its HTTP/1.1 connection, so URLSession's default of 6 would leave the
    /// seventh stream to one host waiting forever in `open`).
    public static func makeDefaultSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = 24 * 60 * 60
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpMaximumConnectionsPerHost = maximumStreamsPerHost
        return URLSession(configuration: configuration)
    }

    /// How many streams the default session keeps open to one host at once.
    static let maximumStreamsPerHost = 1_024

    /// Requests `url` and returns once a 2xx `text/event-stream` answer's head arrived.
    public func open(url: String, headers: [Header], lastEventId: String?) async throws(SseError) -> any SseStream {
        guard let target = URL(string: url), let scheme = target.scheme?.lowercased(),
              scheme == "http" || scheme == "https", target.host != nil
        else {
            throw SseError.refused(status: nil, message: "invalid URL: \(url)")
        }
        // A background session (the only kind with an identifier) takes no task delegate: setting one raises an Objective-C
        // exception that would abort the app, so the stream is refused before a task exists.
        guard session.configuration.identifier == nil else {
            throw SseError.refused(
                status: nil,
                message: "a background URLSession cannot carry an event stream (it takes no task delegate): "
                    + "give URLSessionSseAdapter a default or ephemeral session"
            )
        }
        var request = URLRequest(url: target)
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        request.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
        for header in headers {
            request.addValue(header.value, forHTTPHeaderField: header.name)
        }
        if let lastEventId = lastEventId {
            request.setValue(lastEventId, forHTTPHeaderField: "Last-Event-ID")
        }
        let stream = URLSessionSseStream(lastEventId: lastEventId)
        switch await stream.start(request, on: session) {
        case .accepted:
            return stream
        case .failed(let error):
            throw error
        }
    }

    /// Why the answer's head is not an event stream, or `nil` when it is one.
    static func refusal(of response: URLResponse) -> SseError? {
        guard let http = response as? HTTPURLResponse else {
            return SseError.protocol("the answer is not HTTP")
        }
        guard (200 ..< 300).contains(http.statusCode), http.statusCode != 204 else {
            let status = http.statusCode
            return SseError.refused(
                status: UInt16(clamping: status),
                message: "the server answered HTTP \(status) (\(HTTPURLResponse.localizedString(forStatusCode: status)))"
            )
        }
        let contentType = http.value(forHTTPHeaderField: "Content-Type") ?? ""
        let mediaType = contentType.split(separator: ";", maxSplits: 1).first
            .map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
        guard mediaType == "text/event-stream" else {
            return SseError.protocol("expected text/event-stream, got \(contentType.isEmpty ? "no content type" : contentType)")
        }
        return nil
    }

    /// What request failed before it had an answer.
    static func openFailure(_ error: any Error, url: String) -> SseError {
        if let urlError = error as? URLError {
            if urlError.code == .badURL || urlError.code == .unsupportedURL {
                return SseError.refused(status: nil, message: "invalid URL: \(url)")
            }
            return SseError.network(urlError.localizedDescription)
        }
        return SseError.network((error as NSError).localizedDescription)
    }

    // MARK: UndraAdapter

    public var portId: UInt32 {
        return StandardPorts.Sse.portId
    }

    public func makePortImpl(core: UndraCore) -> PortImpl? {
        return bindings.add(SseBinding(adapter: self)).portImpl()
    }

    public func detach() {
        bindings.detachAll()
    }
}

/// What the stream asks of its request: a `URLSessionTask`, or a recorder in the tests.
protocol SseTaskControl: AnyObject, Sendable {
    /// Stops reading the socket (what is already on its way may still arrive).
    func suspend()
    /// Reads again.
    func resume()
    /// Ends the request, so the server sees the client leave.
    func cancel()
}

extension URLSessionTask: SseTaskControl {}

/// One response body: a task delegate that parses each chunk as it arrives and queues the events
/// for the binding.
///
/// URLSession hands the body over in chunks of whatever size the socket gave it; the parser is
/// incremental, so a chunk is fed to it whole and may end anywhere (inside an event, a multi-byte
/// character, a CR LF pair). Reading it one byte at a time through `URLSession.AsyncBytes` made
/// every byte an asynchronous hop, which is what capped the throughput.
///
/// Backpressure is the Kotlin adapter's rule (`SseStreamReader`): the socket is read only while
/// fewer events wait than the binding's buffer has `room` for. A delegate has no pull, so the data
/// task is suspended when `waiting` reaches `room` and resumed when the binding takes `waiting`
/// below it. A chunk may hold several events, so the read-ahead is `room` plus at most one chunk
/// (plus the few chunks already in flight when the task was suspended), and it ends in the
/// kernel's socket buffers, where TCP pushes back on the server.
///
/// The task is suspended only when that bound needs it, never for a chunk that cannot take
/// `waiting` to `room`: URLSession can lose the end of a body (no `didCompleteWithError`, ever,
/// and the task stays `.running`) when its task is resumed and at once suspended again while the
/// body ends, which a suspend around every chunk risked at the end of every stream
/// (`.10x/decisions/sde/sse-end-race.md`).
final class URLSessionSseStream: NSObject, SseStream, URLSessionDataDelegate, @unchecked Sendable {
    /// How the request's head came out: the stream is open, or why it is not.
    enum Head: Sendable {
        case accepted
        case failed(SseError)
    }

    /// How many events may wait for the binding before the task is suspended: the room of the
    /// binding's buffer, which is its window (16 before the core's first pull, SPEC 3.7).
    static let room = initialReadAhead

    private enum Step {
        case event(SseEvent)
        case end(SseError)
        case finished
        case wait
    }

    private struct State {
        var control: (any SseTaskControl)?
        var url = ""
        /// `open`, waiting for the head.
        var head: CheckedContinuation<Head, Never>?
        var cancelledWhileOpening = false
        /// The events parsed and not yet taken by the binding: `queue[first...]`.
        var queue: [SseEvent] = []
        var first = 0
        /// How the body ended; handed over once the queued events are taken.
        var end: SseError?
        /// The end was handed over, or the core closed the stream.
        var over = false
        /// The binding's pump, waiting for an event.
        var waiter: CheckedContinuation<Void, Never>?
        var suspended = false
        /// Chunks being parsed now (the task stays suspended until the last of them is queued).
        var parsing = 0
        var chunks = 0
        var bytes = 0

        /// Events that wait for the binding.
        var waiting: Int {
            return queue.count - first
        }

        mutating func take() -> SseEvent? {
            guard first < queue.count else {
                return nil
            }
            let event = queue[first]
            first += 1
            if first == queue.count {
                queue.removeAll(keepingCapacity: true)
                first = 0
            } else if first >= 1_024 && first * 2 >= queue.count {
                queue.removeFirst(first)
                first = 0
            }
            return event
        }
    }

    private let state = Guarded(State())
    /// Touched by `receive`, which URLSession calls one chunk at a time.
    private let parser: Guarded<SseParser>
    private let room: Int

    /// A stream whose request is `control` (a test's recorder), or the one `start` makes.
    init(lastEventId: String?, room: Int = URLSessionSseStream.room, control: (any SseTaskControl)? = nil) {
        self.parser = Guarded(SseParser(lastEventId: lastEventId))
        self.room = max(1, room)
        super.init()
        state.withLock { (current: inout State) -> Void in
            current.control = control
        }
    }

    /// Chunks the body has arrived in so far, and their bytes (what the tests wait on).
    var received: (chunks: Int, bytes: Int) {
        return state.withLock { (current: inout State) -> (chunks: Int, bytes: Int) in
            return (current.chunks, current.bytes)
        }
    }

    /// Whether the task is suspended now (what the tests look at).
    var isSuspended: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.suspended
        }
    }

    /// Whether a pull waits for an event now (what the tests wait on).
    var hasWaitingPull: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.waiter != nil
        }
    }

    // MARK: Opening

    /// Starts `request` on `session` and waits for the answer's head. Cancelling the caller
    /// cancels the request.
    func start(_ request: URLRequest, on session: URLSession) async -> Head {
        let task = session.dataTask(with: request)
        task.delegate = self
        state.withLock { (current: inout State) -> Void in
            current.control = task
            current.url = request.url?.absoluteString ?? ""
        }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Head, Never>) in
                let cancelled = state.withLock { (current: inout State) -> Bool in
                    if current.cancelledWhileOpening {
                        return true
                    }
                    current.head = continuation
                    return false
                }
                if cancelled {
                    continuation.resume(returning: .failed(.network("cancelled")))
                } else {
                    task.resume()
                }
            }
        } onCancel: {
            let head = state.withLock { (current: inout State) -> CheckedContinuation<Head, Never>? in
                current.cancelledWhileOpening = true
                let taken = current.head
                current.head = nil
                return taken
            }
            task.cancel()
            head?.resume(returning: .failed(.network("cancelled")))
        }
    }

    // MARK: SseStream

    var events: AsyncThrowingStream<SseEvent, any Error> {
        return AsyncThrowingStream(unfolding: { [self] () async throws -> SseEvent? in
            return try await self.nextEvent()
        })
    }

    func close() async {
        let (waiter, control, wasSuspended) = state.withLock { (current: inout State) -> (CheckedContinuation<Void, Never>?, (any SseTaskControl)?, Bool) in
            current.over = true
            current.queue = []
            current.first = 0
            let waiter = current.waiter
            current.waiter = nil
            let suspended = current.suspended
            current.suspended = false
            return (waiter, current.control, suspended)
        }
        control?.cancel()
        if wasSuspended {
            // A cancelled task that stays suspended may never report, and the server must see the client leave.
            control?.resume()
        }
        waiter?.resume()
    }

    // MARK: Reading

    /// The next event: the first queued one, else what ended the body, else wait for a chunk.
    private func nextEvent() async throws -> SseEvent? {
        while true {
            let step = state.withLock { (current: inout State) -> Step in
                if current.over {
                    return .finished
                }
                if let event = current.take() {
                    if current.suspended && current.parsing == 0 && current.waiting < room {
                        current.suspended = false
                        current.control?.resume()
                    }
                    return .event(event)
                }
                if let end = current.end {
                    current.over = true
                    return .end(end)
                }
                return .wait
            }
            switch step {
            case .event(let event):
                return event
            case .end(let error):
                throw error
            case .finished:
                return nil
            case .wait:
                await park()
            }
        }
    }

    /// Waits until a chunk queued an event, the body ended, the stream was closed, or the waiting
    /// task was cancelled.
    private func park() async {
        await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let ready = state.withLock { (current: inout State) -> Bool in
                    if current.over || current.end != nil || current.waiting > 0 {
                        return true
                    }
                    current.waiter = continuation
                    return false
                }
                if ready {
                    continuation.resume()
                }
            }
        } onCancel: {
            cancelRead()
        }
    }

    /// The task that waits for an event was cancelled: the stream ends as a cancelled read of
    /// `URLSession.AsyncBytes` ended it (what this adapter read before it took chunks), with
    /// ``SseError/network(_:)`` "cancelled" after the events already queued, and the request is
    /// cancelled, so the server sees the client leave. A pull that waited on for a chunk or a close
    /// would hold the request open for a reader that is gone.
    private func cancelRead() {
        let taken = state.withLock { (current: inout State) -> (CheckedContinuation<Void, Never>?, (any SseTaskControl)?, Bool)? in
            if current.over || current.end != nil {
                return nil
            }
            current.end = SseError.network("cancelled")
            let waiter = current.waiter
            current.waiter = nil
            let suspended = current.suspended
            current.suspended = false
            return (waiter, current.control, suspended)
        }
        guard let (waiter, control, wasSuspended) = taken else {
            return
        }
        control?.cancel()
        if wasSuspended {
            // As in `close`: a cancelled task that stays suspended may never report.
            control?.resume()
        }
        waiter?.resume()
    }

    /// One chunk of the body: parsed whole, its events queued. The parser keeps what is left of an
    /// event the chunk ended in; bytes that are not UTF-8 end the stream after the events before them.
    ///
    /// A chunk that may take `waiting` to `room` is parsed with the task suspended, not suspended
    /// after: parsing takes time (a chunk is up to megabytes), and URLSession goes on reading the
    /// socket meanwhile and hands the lot over in one piece, so a suspend that came after the parse
    /// would have nothing left to stop. It is resumed once the events are queued, unless `waiting`
    /// has reached `room`: then the binding's next pull below the mark resumes it. A chunk with too
    /// few line ends to reach the mark (``SseParser/mostEvents(in:upTo:)``) is parsed with the task
    /// running, as it would be resumed right after anyway.
    func receive(_ chunk: Data) {
        let most = chunk.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) -> Int in
            return SseParser.mostEvents(in: bytes, upTo: room)
        }
        let proceed = state.withLock { (current: inout State) -> Bool in
            current.chunks += 1
            current.bytes += chunk.count
            if current.over || current.end != nil {
                return false
            }
            current.parsing += 1
            if !current.suspended && current.waiting + most >= room {
                current.suspended = true
                current.control?.suspend()
            }
            return true
        }
        guard proceed else {
            return
        }
        var events: [SseEvent] = []
        var failure: SseError?
        parser.withLock { (parser: inout SseParser) -> Void in
            chunk.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) -> Void in
                do throws(SseError) {
                    try parser.push(buffer, into: &events)
                } catch {
                    failure = error
                }
            }
        }
        let (waiter, control, resume) = state.withLock { (current: inout State) -> (CheckedContinuation<Void, Never>?, (any SseTaskControl)?, Bool) in
            current.parsing -= 1
            if current.over {
                return (nil, nil, false)
            }
            current.queue.append(contentsOf: events)
            var resume = false
            if let failure = failure {
                // Bytes that are not UTF-8 end the stream: the request has nothing more to say.
                current.end = failure
                resume = current.suspended
            } else if current.parsing == 0 && current.waiting < room {
                resume = current.suspended
            }
            if resume {
                current.suspended = false
            }
            var waiter: CheckedContinuation<Void, Never>?
            if !events.isEmpty || failure != nil {
                waiter = current.waiter
                current.waiter = nil
            }
            return (waiter, current.control, resume)
        }
        if failure != nil {
            control?.cancel()
        }
        if resume {
            control?.resume()
        }
        waiter?.resume()
    }

    /// The request is over: the body ended (`error == nil`), failed, or never had an answer.
    func finish(_ error: (any Error)?) {
        let (head, url, waiter) = state.withLock { (current: inout State) -> (CheckedContinuation<Head, Never>?, String, CheckedContinuation<Void, Never>?) in
            let head = current.head
            current.head = nil
            if head == nil && current.end == nil && !current.over {
                if let error = error {
                    current.end = SseError.network((error as NSError).localizedDescription)
                } else {
                    current.end = .ended
                }
            }
            let waiter = current.waiter
            current.waiter = nil
            return (head, current.url, waiter)
        }
        if let head = head {
            if let error = error {
                head.resume(returning: .failed(URLSessionSseAdapter.openFailure(error, url: url)))
            } else {
                head.resume(returning: .failed(.network("the connection ended before the answer")))
            }
        }
        waiter?.resume()
    }

    // MARK: URLSessionDataDelegate

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping @Sendable (URLSession.ResponseDisposition) -> Void
    ) {
        let refusal = URLSessionSseAdapter.refusal(of: response)
        let head = state.withLock { (current: inout State) -> CheckedContinuation<Head, Never>? in
            let taken = current.head
            current.head = nil
            return taken
        }
        if let refusal = refusal {
            completionHandler(.cancel)
            head?.resume(returning: .failed(refusal))
        } else {
            completionHandler(.allow)
            head?.resume(returning: .accepted)
        }
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        receive(data)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
        finish(error)
    }
}
