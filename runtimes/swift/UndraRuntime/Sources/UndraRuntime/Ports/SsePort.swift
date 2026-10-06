// The `Sse` port (ADR-047): the adapter interface an app implements to replace the default
// (`SseAdapter`, `SseStream`), the binding that serves the port from one (`SsePortAdapter`), and
// the one server-sent event parser of this runtime (`SseParser`, the HTML standard's algorithm).
//
//   open(url, headers, last_event_id) -> u32      next(stream, max) -> [SseEvent]      close(stream)

/// Opens server-sent event streams for the `Sse` port (ADR-047).
///
/// Implement it to replace ``URLSessionSseAdapter`` and register it with ``SsePortAdapter``:
///
/// ```swift
/// let adapters = Adapters.platformDefault.replacing(SsePortAdapter(MySse()))
/// ```
public protocol SseAdapter: Sendable {
    /// Requests `url` with `Accept: text/event-stream`, `Cache-Control: no-cache`, `headers` and,
    /// when given, `Last-Event-ID: lastEventId`. Returns once a 2xx `text/event-stream` answer
    /// arrived; another status is ``SseError/refused(status:message:)``, another content type
    /// ``SseError/protocol(_:)``.
    func open(url: String, headers: [Header], lastEventId: String?) async throws(SseError) -> any SseStream
}

/// One open event stream, as an adapter provides it.
public protocol SseStream: Sendable {
    /// The events, pulled: the binding asks for the next one only while the core's window has
    /// room, so the stream should be lazy over the response body. It ends by throwing an
    /// ``SseError`` (`ended` when the body ended, `network` when reading failed, `protocol` for a
    /// body that is not UTF-8), or by finishing after ``close()``. The binding reads it once.
    var events: AsyncThrowingStream<SseEvent, any Error> { get }

    /// Stops the stream and releases the connection.
    func close() async
}

/// Serves the `Sse` port with an ``SseAdapter`` (ADR-047), with the WebSocket binding's
/// discipline: ids from 1 never reused, a pump that reads ahead at most the `max` of the core's
/// latest `next` (16 before the first), `next` answering up to `max` events at once (a burst
/// still arriving is one answer), `[]` after the core's `close`, the stream's end (sticky)
/// otherwise, one `next` pending per stream.
///
/// `open` refuses a URL that is not `http://` or `https://` (`refused(status: nil, "invalid URL:
/// <url>")`) before the adapter is asked. A stream that finishes without an error although the
/// core did not close it ended with the body: ``SseError/ended``. When the core shuts down, every
/// stream still open is closed.
public final class SsePortAdapter: UndraAdapter, @unchecked Sendable {
    private let adapter: any SseAdapter
    private let bindings = BindingSet<SseBinding>()

    /// Serves the port with `adapter`.
    public init(_ adapter: any SseAdapter) {
        self.adapter = adapter
    }

    public var portId: UInt32 {
        return StandardPorts.Sse.portId
    }

    public func makePortImpl(core: UndraCore) -> PortImpl? {
        return bindings.add(SseBinding(adapter: adapter)).portImpl()
    }

    public func detach() {
        bindings.detachAll()
    }
}

/// The state of the `Sse` port of one core.
final class SseBinding: DetachableBinding, @unchecked Sendable {
    /// One open stream: the adapter's and its inbox.
    final class Entry: Sendable {
        let stream: any SseStream
        let inbox: PulledInbox<SseEvent, SseError>

        init(_ stream: any SseStream) {
            self.stream = stream
            self.inbox = PulledInbox(
                source: stream.events,
                mapError: { (error: any Error) -> SseError in
                    return (error as? SseError) ?? .network(String(describing: error))
                },
                finished: .ended
            )
        }
    }

    private struct State {
        var lastId: UInt32 = 0
        var open: [UInt32: Entry] = [:]
        var closed: Set<UInt32> = []
        var detached = false
    }

    private let adapter: any SseAdapter
    private let state = Guarded(State())

    init(adapter: any SseAdapter) {
        self.adapter = adapter
    }

    /// The method table of the port.
    func portImpl() -> PortImpl {
        return .async([
            StandardPorts.Sse.open: { [self] (args: [UInt8]) async throws -> [UInt8] in
                var reader = UndraReader(args)
                let url = try reader.readString()
                let headers = try [Header].undraDecode(&reader)
                let lastEventId = try Optional<String>.undraDecode(&reader)
                try reader.finish()
                return try await answer { () async throws(SseError) -> UInt32 in
                    try await self.open(url: url, headers: headers, lastEventId: lastEventId)
                }
            },
            StandardPorts.Sse.next: { [self] (args: [UInt8]) async throws -> [UInt8] in
                var reader = UndraReader(args)
                let stream = try reader.readU32()
                let max = try reader.readU32()
                try reader.finish()
                return try await answer { () async throws(SseError) -> [SseEvent] in
                    try await self.next(stream: stream, max: max)
                }
            },
            StandardPorts.Sse.close: { [self] (args: [UInt8]) async throws -> [UInt8] in
                var reader = UndraReader(args)
                let stream = try reader.readU32()
                try reader.finish()
                return try await answerUnit { () async throws(SseError) -> Void in
                    try await self.close(stream: stream)
                }
            },
        ])
    }

    // MARK: The port's methods

    func open(url: String, headers: [Header], lastEventId: String?) async throws(SseError) -> UInt32 {
        let lowered = url.lowercased()
        guard lowered.hasPrefix("http://") || lowered.hasPrefix("https://") else {
            throw SseError.refused(status: nil, message: "invalid URL: \(url)")
        }
        let stream = try await adapter.open(url: url, headers: headers, lastEventId: lastEventId)
        let entry = Entry(stream)
        let id = state.withLock { (current: inout State) -> UInt32? in
            if current.detached {
                return nil
            }
            current.lastId += 1
            current.open[current.lastId] = entry
            return current.lastId
        }
        guard let id = id else {
            entry.inbox.close()
            await stream.close()
            throw SseError.network("the core shut down")
        }
        return id
    }

    func next(stream: UInt32, max: UInt32) async throws(SseError) -> [SseEvent] {
        let found = state.withLock { (current: inout State) -> Entry?? in
            if let entry = current.open[stream] {
                return .some(entry)
            }
            return current.closed.contains(stream) ? .some(nil) : nil
        }
        switch found {
        case nil:
            throw SseError.network("no event stream \(stream)")
        case .some(nil):
            return []
        case .some(let entry?):
            let pending = SseError.protocol("a next is already pending on stream \(stream)")
            return try await entry.inbox.pull(max: max, alreadyPending: pending).get()
        }
    }

    func close(stream: UInt32) async throws(SseError) {
        let found = state.withLock { (current: inout State) -> Entry?? in
            if let entry = current.open.removeValue(forKey: stream) {
                current.closed.insert(stream)
                return .some(entry)
            }
            return current.closed.contains(stream) ? .some(nil) : nil
        }
        switch found {
        case nil:
            throw SseError.network("no event stream \(stream)")
        case .some(nil):
            return
        case .some(let entry?):
            entry.inbox.close()
            await entry.stream.close()
        }
    }

    /// The core shut down: every stream still open is closed.
    func detach() {
        let entries = state.withLock { (current: inout State) -> [Entry] in
            current.detached = true
            let open = Array(current.open.values)
            current.closed.formUnion(current.open.keys)
            current.open = [:]
            return open
        }
        for entry in entries {
            entry.inbox.close()
            Task {
                await entry.stream.close()
            }
        }
    }
}

// MARK: - The parser

/// The server-sent event parser of the HTML standard ("Interpreting an event stream"), fed bytes
/// as they arrive.
///
/// Lines end with CRLF, LF or CR (a CR at the end of one chunk and an LF at the start of the next
/// are one line end). A line starting with `:` is a comment; `field: value` drops one space after
/// the colon, and a line without a colon is a field with an empty value. `event` sets the type,
/// `data` appends its value and an LF, `id` sets the last event id (unless the value contains
/// NUL; the id persists across events), `retry` with only ASCII digits sets the next event's
/// `retryMs`. A blank line dispatches: nothing when no data arrived (the type is reset), else an
/// event with the data minus its last LF, the type or `"message"`, the last event id (`nil` while
/// empty) and the retry. A UTF-8 byte order mark at the very start is skipped; a line that is not
/// UTF-8 is ``SseError/protocol(_:)``. At the end of the body an event without its blank line is
/// dropped, as the standard says.
///
/// Lines are split, and the colon, the space and NUL found, on bytes (the standard's code points),
/// not on `Character`s, which would join a combining mark to the colon before it. Bytes in contiguous
/// storage (an array, a slice, `Data`) are taken a run at a time between line ends.
///
/// ```swift
/// var parser = SseParser()
/// let events = try parser.push(Array("id: 1\ndata: hello\n\n".utf8))
/// // [SseEvent(id: "1", event: "message", data: "hello", retryMs: nil)]
/// ```
public struct SseParser: Sendable {
    private var line: [UInt8] = []
    private var afterCR = false
    private var atStart = true
    private var bomBytes: [UInt8] = []
    /// The event's data so far, as the UTF-8 of the lines' values (each line was checked whole).
    private var data: [UInt8] = []
    private var hasData = false
    private var eventType = ""
    private var lastEventId = ""
    private var retryMs: UInt32?

    private static let lineFeed: UInt8 = 0x0A
    private static let carriageReturn: UInt8 = 0x0D
    private static let colon: UInt8 = 0x3A
    private static let space: UInt8 = 0x20

    /// A parser at the start of a stream. `lastEventId` is the id the request resumed from
    /// (`Last-Event-ID`): events without an `id` field carry it until the stream sets another, as
    /// the standard's last-event-id buffer does across a reconnection.
    public init(lastEventId: String? = nil) {
        self.lastEventId = lastEventId ?? ""
    }

    /// The last event id the stream set, `nil` while there is none: what to resume from.
    public var lastId: String? {
        return lastEventId.isEmpty ? nil : lastEventId
    }

    /// The most events `bytes` can complete, whatever the parser holds from the bytes before them, counted up to `limit`
    /// (the count stops there, so a chunk with many line ends is not read to its end).
    ///
    /// An event is dispatched only at a line end (the blank line after it). The first one `bytes` complete may take just
    /// that line end, the earlier bytes having begun it; every one after it takes at least two (a `data` line's end, then
    /// the blank line's). A CR LF pair or a comment line counts more line ends than it ends events, so the count may be
    /// higher than what the bytes complete, never lower.
    static func mostEvents(in bytes: UnsafeRawBufferPointer, upTo limit: Int) -> Int {
        guard limit > 0 else {
            return 0
        }
        let enough = limit > Int.max / 2 ? Int.max : 2 * limit - 1
        var lineEnds = 0
        for byte in bytes where byte == lineFeed || byte == carriageReturn {
            lineEnds += 1
            if lineEnds == enough {
                break
            }
        }
        return lineEnds == 0 ? 0 : 1 + (lineEnds - 1) / 2
    }

    /// Parses `bytes` and returns the events they completed, in order.
    public mutating func push<Bytes: Sequence>(_ bytes: Bytes) throws(SseError) -> [SseEvent] where Bytes.Element == UInt8 {
        var events: [SseEvent] = []
        try push(bytes, into: &events)
        return events
    }

    /// ``push(_:)`` that appends to `events`, so what the bytes completed before a line that is not UTF-8 is not lost
    /// with the error: a reader that feeds one chunk at a time hands those events over, then the error, as a reader that
    /// feeds one byte at a time does.
    mutating func push<Bytes: Sequence>(_ bytes: Bytes, into events: inout [SseEvent]) throws(SseError) where Bytes.Element == UInt8 {
        var failure: SseError?
        let contiguous: Void? = bytes.withContiguousStorageIfAvailable { (buffer: UnsafeBufferPointer<UInt8>) -> Void in
            do throws(SseError) {
                try push(buffer: buffer, into: &events)
            } catch {
                failure = error
            }
        }
        if let failure = failure {
            throw failure
        }
        if contiguous != nil {
            return
        }
        for byte in bytes {
            try push(byte: byte, into: &events)
        }
    }

    /// Bytes in contiguous storage: a run of bytes that are not line ends is appended to the line at once.
    private mutating func push(buffer: UnsafeBufferPointer<UInt8>, into events: inout [SseEvent]) throws(SseError) {
        var index = buffer.startIndex
        let end = buffer.endIndex
        while index < end && atStart {
            try push(byte: buffer[index], into: &events)
            index += 1
        }
        while index < end {
            let byte = buffer[index]
            if byte == SseParser.lineFeed || byte == SseParser.carriageReturn {
                try consume(byte, into: &events)
                index += 1
                continue
            }
            afterCR = false
            let start = index
            index += 1
            while index < end {
                let next = buffer[index]
                if next == SseParser.lineFeed || next == SseParser.carriageReturn {
                    break
                }
                index += 1
            }
            line.append(contentsOf: UnsafeBufferPointer(rebasing: buffer[start ..< index]))
        }
    }

    /// One byte, with the byte order mark check at the start of the stream.
    private mutating func push(byte: UInt8, into events: inout [SseEvent]) throws(SseError) {
        if atStart {
            // The standard skips one BOM (EF BB BF) at the start of the stream.
            let bom: [UInt8] = [0xEF, 0xBB, 0xBF]
            if byte == bom[bomBytes.count] {
                bomBytes.append(byte)
                if bomBytes.count == bom.count {
                    atStart = false
                    bomBytes = []
                }
                return
            }
            atStart = false
            let partial = bomBytes
            bomBytes = []
            for earlier in partial {
                try consume(earlier, into: &events)
            }
        }
        try consume(byte, into: &events)
    }

    /// One byte after the BOM check.
    private mutating func consume(_ byte: UInt8, into events: inout [SseEvent]) throws(SseError) {
        switch byte {
        case SseParser.lineFeed:
            if afterCR {
                afterCR = false
                return
            }
            try endLine(into: &events)
        case SseParser.carriageReturn:
            afterCR = true
            try endLine(into: &events)
        default:
            afterCR = false
            line.append(byte)
        }
    }

    /// A complete line, in `line`; `line` is empty after it (its storage is kept for the next one).
    private mutating func endLine(into events: inout [SseEvent]) throws(SseError) {
        var complete: [UInt8] = []
        swap(&complete, &line)
        defer {
            complete.removeAll(keepingCapacity: true)
            line = complete
        }
        try apply(complete, into: &events)
    }

    /// What a complete line says.
    private mutating func apply(_ line: [UInt8], into events: inout [SseEvent]) throws(SseError) {
        if line.isEmpty {
            dispatch(into: &events)
            return
        }
        guard SseParser.isUTF8(line) else {
            throw SseError.protocol("the event stream is not UTF-8")
        }
        if line[0] == SseParser.colon {
            return
        }
        let colon = line.firstIndex(of: SseParser.colon)
        let field = line[..<(colon ?? line.endIndex)]
        var valueStart = colon.map { $0 + 1 } ?? line.endIndex
        if valueStart < line.endIndex && line[valueStart] == SseParser.space {
            valueStart += 1
        }
        let value = line[valueStart...]
        // The field and the value are split at ASCII bytes of a line that is UTF-8, so each is UTF-8.
        if field.elementsEqual("data".utf8) {
            data.append(contentsOf: value)
            data.append(SseParser.lineFeed)
            hasData = true
        } else if field.elementsEqual("event".utf8) {
            eventType = String(decoding: value, as: UTF8.self)
        } else if field.elementsEqual("id".utf8) {
            if !value.contains(0) {
                lastEventId = String(decoding: value, as: UTF8.self)
            }
        } else if field.elementsEqual("retry".utf8) {
            if !value.isEmpty, value.allSatisfy({ $0 >= 0x30 && $0 <= 0x39 }), let ms = UInt32(String(decoding: value, as: UTF8.self)) {
                retryMs = ms
            }
        }
    }

    /// Whether `bytes` are well-formed UTF-8 (a line of ASCII, the usual one, is told by one pass).
    static func isUTF8(_ bytes: [UInt8]) -> Bool {
        var any: UInt8 = 0
        for byte in bytes {
            any |= byte
        }
        if any < 0x80 {
            return true
        }
        return utf8(bytes) != nil
    }

    /// `bytes` as a string, or `nil` when they are not well-formed UTF-8.
    static func utf8(_ bytes: [UInt8]) -> String? {
        var decoder = UTF8()
        var iterator = bytes.makeIterator()
        while true {
            switch decoder.decode(&iterator) {
            case .scalarValue:
                continue
            case .emptyInput:
                return String(decoding: bytes, as: UTF8.self)
            case .error:
                return nil
            }
        }
    }

    /// A blank line: the event so far, if it has data.
    private mutating func dispatch(into events: inout [SseEvent]) {
        guard hasData else {
            data.removeAll(keepingCapacity: true)
            eventType = ""
            return
        }
        // Without the LF the last data line appended.
        let payload = String(decoding: data[..<(data.endIndex - 1)], as: UTF8.self)
        events.append(SseEvent(
            id: lastEventId.isEmpty ? nil : lastEventId,
            event: eventType.isEmpty ? "message" : eventType,
            data: payload,
            retryMs: retryMs
        ))
        data.removeAll(keepingCapacity: true)
        hasData = false
        eventType = ""
        retryMs = nil
    }
}
