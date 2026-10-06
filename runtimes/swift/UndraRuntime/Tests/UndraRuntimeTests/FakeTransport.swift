import Foundation
import XCTest
@testable import UndraRuntime

/// A scripted in-process "core" that speaks the wire through the same `UndraTransport` seam the
/// real transports use. It records everything the host sends and lets a test play the core's
/// side: replies, stream items (with the credit rules of docs/SPEC.md section 3.7), change-sets
/// and port calls.
final class FakeTransport: UndraTransport, @unchecked Sendable {
    /// Something the host sent to the core.
    enum Sent: Equatable {
        case call(Wire.Call)
        case cancel(UInt32)
        case credit(UInt32, UInt32)
        case observe(UInt64, UInt32, Bool)
        case release(UInt64)
        case registerPort(UInt32)
        case portReply(Wire.PortReply)
        case event(UInt32, UInt32, [UInt8])
        case timerFired(UInt32)
    }

    /// How a scripted stream ends once its items are sent. Like the core, the fake sends the last
    /// item without credit (docs/SPEC.md section 3.7).
    enum Ending: Equatable {
        /// Flag 1: the stream ended.
        case end
        /// Flag 2 with this body: the stream's own typed error `E`.
        case error([UInt8])
        /// Flag 3 with this body: the call failed (normally an encoded `Wire.StreamFailure`).
        case failed([UInt8])

        /// Flag 3 carrying `failure`.
        static func failure(_ failure: Wire.StreamFailure) -> Ending {
            return .failed(failure.encode())
        }
    }

    /// One scripted stream: items sent only as far as credit allows, then its ending.
    struct FakeStream {
        var items: [[UInt8]]
        var ending: Ending
        /// Keeps the stream open after the last item (no end marker).
        var holdOpen = false
        var credit: UInt32 = 0
        var totalCredit: UInt32 = 0
        var delivered = 0
        var finished = false
    }

    private enum PumpAction {
        case idle
        case item([UInt8])
        case finish(Ending)
    }

    private struct State {
        var sent: [Sent] = []
        var inbound: (any UndraInbound)? = nil
        var streams: [UInt32: FakeStream] = [:]
        var started = false
        var shutDown = false
        /// `false` while the fake plays a remote core that is unreachable: nothing the host sends gets through.
        var up = true
    }

    private let state = Guarded<State>(State())

    let schemaHash: UInt64
    let directSync: Bool

    /// What the core does with a call: return `false` to reject it (`undra_call` returned 5).
    var onCall: (@Sendable (Wire.Call, FakeTransport) -> Bool)?
    /// The `Wire.Reply` bytes returned by an inline `callSync`.
    var onCallSync: (@Sendable (Wire.Call) -> [UInt8])?
    /// What the core does when the host observes a signal.
    var onObserve: (@Sendable (UndraHandle, UInt32, Bool, FakeTransport) -> Void)?
    var statsDocument: String?
    var snapshotBytes: [UInt8] = []
    var restoreCode: UInt32 = 0
    /// Runs inside `restore`, on the calling thread: where an in-process core delivers the
    /// restored values.
    var onRestore: (@Sendable (FakeTransport) -> Void)?

    init(schemaHash: UInt64 = 0x1234, directSync: Bool = true) {
        self.schemaHash = schemaHash
        self.directSync = directSync
    }

    // MARK: UndraTransport

    var mode: UndraMode {
        return directSync ? .inproc : .remote
    }

    var supportsDirectSync: Bool {
        return directSync
    }

    func start(inbound: any UndraInbound, options: TransportStartOptions) throws -> TransportInfo {
        state.withLock { (current: inout State) -> Void in
            current.inbound = inbound
            current.started = true
        }
        return TransportInfo(schemaHash: schemaHash)
    }

    func send(call payload: [UInt8]) -> Bool {
        if !isUp {
            return false
        }
        guard let call = try? Wire.Call.decode(payload) else {
            return false
        }
        record(.call(call))
        if let handler = onCall {
            return handler(call, self)
        }
        return true
    }

    func callSync(_ payload: [UInt8]) throws -> [UInt8] {
        let call = try Wire.Call.decode(payload)
        record(.call(call))
        if let handler = onCallSync {
            return handler(call)
        }
        return Wire.Reply(callId: call.callId, status: .ok).encode()
    }

    func cancel(callId: UInt32) {
        record(.cancel(callId))
        state.withLock { (current: inout State) -> Void in
            if var stream = current.streams[callId] {
                stream.finished = true
                current.streams[callId] = stream
            }
        }
    }

    func streamCredit(callId: UInt32, credit: UInt32) {
        record(.credit(callId, credit))
        state.withLock { (current: inout State) -> Void in
            if var stream = current.streams[callId] {
                stream.credit &+= credit
                stream.totalCredit &+= credit
                current.streams[callId] = stream
            }
        }
        pump(callId)
    }

    func observe(handle: UndraHandle, signal: UInt32, on: Bool) {
        if !isUp {
            return
        }
        record(.observe(handle.rawValue, signal, on))
        if let handler = onObserve {
            handler(handle, signal, on, self)
        }
    }

    func release(handle: UndraHandle) {
        if !isUp {
            return
        }
        record(.release(handle.rawValue))
    }

    func registerPort(_ portId: UInt32) {
        record(.registerPort(portId))
    }

    func portReply(_ payload: [UInt8]) {
        if let reply = try? Wire.PortReply.decode(payload) {
            record(.portReply(reply))
        }
    }

    func event(portId: UInt32, methodId: UInt32, payload: [UInt8]) {
        record(.event(portId, methodId, payload))
    }

    func timerFired(_ timerId: UInt32) {
        record(.timerFired(timerId))
    }

    func snapshot() throws -> [UInt8] {
        return snapshotBytes
    }

    func restore(_ payload: [UInt8]) throws {
        if restoreCode != 0 {
            throw UndraRestoreError(code: restoreCode)
        }
        onRestore?(self)
    }

    func statsJSON() -> String? {
        return statsDocument
    }

    /// Forget the core when shut down, as the in-process transport does, so that a core nothing else holds can be
    /// released (the review of ADR-045: a connection a view holds must not depend on the core being alive).
    var releasesInboundOnShutdown = false

    func shutdown() {
        let release = releasesInboundOnShutdown
        state.withLock { (current: inout State) -> Void in
            current.shutDown = true
            if release {
                current.inbound = nil
            }
        }
    }

    // MARK: A remote core that can be dropped (ADR-051)

    var isUp: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.up
        }
    }

    /// The connection drops and the (pretend) transport starts reconnecting: attempt 1 is the loss.
    func drop(_ error: any Error = UndraTransportError.connectionLost(reason: "the fake connection was lost")) {
        state.withLock { (current: inout State) -> Void in
            current.up = false
        }
        currentInbound()?.onReconnecting(attempt: 1, error: error)
    }

    /// A reconnect attempt failed; the transport tries again.
    func retry(attempt: Int, _ error: any Error = UndraTransportError.connectionLost(reason: "the fake server is still down")) {
        currentInbound()?.onReconnecting(attempt: attempt, error: error)
    }

    /// The connection is back.
    func reconnect() {
        state.withLock { (current: inout State) -> Void in
            current.up = true
        }
        currentInbound()?.onReconnected()
    }

    // MARK: Observations

    var wasStarted: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.started
        }
    }

    var wasShutDown: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.shutDown
        }
    }

    var sent: [Sent] {
        return state.withLock { (current: inout State) -> [Sent] in
            return current.sent
        }
    }

    /// The observations the host made, in order.
    var observes: [Sent] {
        return sent.filter { item in
            if case .observe = item {
                return true
            }
            return false
        }
    }

    /// Forgets what the host sent so far.
    func forgetSent() {
        state.withLock { (current: inout State) -> Void in
            current.sent = []
        }
    }

    var calls: [Wire.Call] {
        var result: [Wire.Call] = []
        for item in sent {
            if case .call(let call) = item {
                result.append(call)
            }
        }
        return result
    }

    var cancels: [UInt32] {
        var result: [UInt32] = []
        for item in sent {
            if case .cancel(let id) = item {
                result.append(id)
            }
        }
        return result
    }

    var releases: [UInt64] {
        var result: [UInt64] = []
        for item in sent {
            if case .release(let handle) = item {
                result.append(handle)
            }
        }
        return result
    }

    var portReplies: [Wire.PortReply] {
        var result: [Wire.PortReply] = []
        for item in sent {
            if case .portReply(let reply) = item {
                result.append(reply)
            }
        }
        return result
    }

    /// The credit grants for `callId`, in order.
    func credits(for callId: UInt32) -> [UInt32] {
        var result: [UInt32] = []
        for item in sent {
            if case .credit(let id, let credit) = item, id == callId {
                result.append(credit)
            }
        }
        return result
    }

    /// How many items of the scripted stream `callId` the core has sent.
    func deliveredCount(_ callId: UInt32) -> Int {
        return state.withLock { (current: inout State) -> Int in
            return current.streams[callId]?.delivered ?? 0
        }
    }

    /// The credit granted in total to the scripted stream `callId`.
    func totalCredit(_ callId: UInt32) -> UInt32 {
        return state.withLock { (current: inout State) -> UInt32 in
            return current.streams[callId]?.totalCredit ?? 0
        }
    }

    private func record(_ item: Sent) {
        state.withLock { (current: inout State) -> Void in
            current.sent.append(item)
        }
    }

    // MARK: Playing the core

    private func currentInbound() -> (any UndraInbound)? {
        return state.withLock { (current: inout State) -> (any UndraInbound)? in
            return current.inbound
        }
    }

    func deliver(_ reply: Wire.Reply) {
        currentInbound()?.onReply(callId: reply.callId, payload: reply.encode())
    }

    func replyOk(_ callId: UInt32, _ body: [UInt8] = []) {
        deliver(Wire.Reply(callId: callId, status: .ok, body: ArraySlice(body)))
    }

    func replyError(_ callId: UInt32, _ body: [UInt8]) {
        deliver(Wire.Reply(callId: callId, status: .error, body: ArraySlice(body)))
    }

    func deliverRawReply(_ callId: UInt32, _ payload: [UInt8]) {
        currentInbound()?.onReply(callId: callId, payload: payload)
    }

    func deliverChangeSet(_ changeSet: Wire.ChangeSet) {
        currentInbound()?.onChangeSet(changeSet.encode())
    }

    func deliverRawChangeSet(_ payload: [UInt8]) {
        currentInbound()?.onChangeSet(payload)
    }

    func deliverStreamItem(_ item: Wire.StreamItem) {
        currentInbound()?.onStreamItem(callId: item.callId, payload: item.encode())
    }

    func deliverRawStreamItem(_ callId: UInt32, _ payload: [UInt8]) {
        currentInbound()?.onStreamItem(callId: callId, payload: payload)
    }

    func deliverLog(level: UInt8, target: String, message: String) {
        currentInbound()?.onLog(level: level, target: target, message: message)
    }

    func disconnect(_ error: any Error) {
        currentInbound()?.onDisconnect(error)
    }

    /// The core calls the host's port.
    func callPort(portId: UInt32, methodId: UInt32, portCallId: UInt32, args: [UInt8]) -> PortCallOutcome {
        guard let inbound = currentInbound() else {
            return .unavailable
        }
        return inbound.onPortCall(portId: portId, methodId: methodId, portCallId: portCallId, args: args)
    }

    /// Opens a scripted stream for `callId`: replies "stream opened" and then delivers items as
    /// credit arrives, followed by `ending`.
    func openStream(_ callId: UInt32, items: [[UInt8]], ending: Ending = .end, holdOpen: Bool = false) {
        state.withLock { (current: inout State) -> Void in
            current.streams[callId] = FakeStream(items: items, ending: ending, holdOpen: holdOpen)
        }
        deliver(Wire.Reply(callId: callId, status: .streamOpened))
        pump(callId)
    }

    private func pump(_ callId: UInt32) {
        while true {
            let action = state.withLock { (current: inout State) -> PumpAction in
                guard var stream = current.streams[callId] else {
                    return .idle
                }
                if stream.finished {
                    return .idle
                }
                if stream.delivered < stream.items.count {
                    if stream.credit == 0 {
                        return .idle
                    }
                    let item = stream.items[stream.delivered]
                    stream.delivered += 1
                    stream.credit -= 1
                    current.streams[callId] = stream
                    return .item(item)
                }
                if stream.holdOpen {
                    return .idle
                }
                stream.finished = true
                current.streams[callId] = stream
                return .finish(stream.ending)
            }
            switch action {
            case .idle:
                return
            case .item(let item):
                deliverStreamItem(Wire.StreamItem(callId: callId, flag: .item, body: ArraySlice(item)))
            case .finish(.end):
                deliverStreamItem(Wire.StreamItem(callId: callId, flag: .end))
                return
            case .finish(.error(let body)):
                deliverStreamItem(Wire.StreamItem(callId: callId, flag: .error, body: ArraySlice(body)))
                return
            case .finish(.failed(let body)):
                deliverStreamItem(Wire.StreamItem(callId: callId, flag: .failed, body: ArraySlice(body)))
                return
            }
        }
    }
}

// MARK: - Helpers

/// The deadline of every wait in this test target, in seconds. A wait is a hang detector, never a speed assertion: the condition
/// it waits for always arrives, however busy the machine is, so the deadline is far longer than any machine needs and only a
/// genuine hang reaches it (a passing wait returns the moment its condition holds, so the length costs nothing). A test that
/// supervises waits (``HangDetector``) is armed for longer still.
let hangDeadline: Double = 60

/// Polls `condition` until it holds or `timeout` seconds pass (``hangDeadline``); yields the main actor between polls.
@MainActor
func waitUntil(timeout: Double = hangDeadline, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() {
            return true
        }
        try? await Task.sleep(nanoseconds: 2_000_000)
    }
    return condition()
}

/// Runs `body` and returns the error it threw, or `nil`.
@MainActor
func captureError(_ body: @MainActor () async throws -> Void) async -> (any Error)? {
    do {
        try await body()
        return nil
    } catch {
        return error
    }
}

/// A core over `transport` with no adapters, for tests. `frames` replaces the platform's frame
/// scheduler (the main actor's next turn on macOS).
func makeCore(
    _ transport: FakeTransport,
    adapters: Adapters = Adapters.none,
    expectedSchemaHash: UInt64 = 0x1234,
    blockingCallTimeout: Double = 30,
    frames: (any FrameScheduler)? = nil,
    maxPendingEntries: Int = 65_536,
    maxPendingBytes: Int = 16 * 1024 * 1024,
    namespace: String? = nil
) throws -> UndraCore {
    var options = LoadOptions.inproc(adapters: adapters, expectedSchemaHash: expectedSchemaHash)
    options.namespace = namespace
    options.blockingCallTimeout = blockingCallTimeout
    options.maxPendingEntries = maxPendingEntries
    options.maxPendingBytes = maxPendingBytes
    return try UndraCore.connect(transport: transport, options: options, frameScheduler: frames)
}

/// A frame scheduler driven by hand: a frame comes only when the test calls `fire()`, so a
/// change-set the core produced on its own stays queued until then.
final class ManualFrameScheduler: FrameScheduler, @unchecked Sendable {
    private struct State {
        var ticks: [@MainActor @Sendable () -> Void] = []
        var requests = 0
        var invalidated = false
    }

    private let state = Guarded<State>(State())

    func requestFrame(_ tick: @escaping @MainActor @Sendable () -> Void) {
        state.withLock { (current: inout State) -> Void in
            current.ticks.append(tick)
            current.requests += 1
        }
    }

    func invalidate() {
        state.withLock { (current: inout State) -> Void in
            current.invalidated = true
        }
    }

    /// Frames requested so far.
    var requests: Int {
        return state.withLock { (current: inout State) -> Int in
            return current.requests
        }
    }

    /// Frames requested and not fired yet.
    var pending: Int {
        return state.withLock { (current: inout State) -> Int in
            return current.ticks.count
        }
    }

    var wasInvalidated: Bool {
        return state.withLock { (current: inout State) -> Bool in
            return current.invalidated
        }
    }

    /// Runs the frames requested so far.
    @MainActor
    func fire() {
        let due = state.withLock { (current: inout State) -> [@MainActor @Sendable () -> Void] in
            let ticks = current.ticks
            current.ticks = []
            return ticks
        }
        for tick in due {
            tick()
        }
    }
}

/// A change-set with the given `(handle, signal, value)` full-value entries.
func makeChangeSet(txn: UInt64 = 1, _ entries: [(UndraHandle, UInt32, [UInt8])]) -> Wire.ChangeSet {
    var list: [Wire.ChangeEntry] = []
    for entry in entries {
        list.append(Wire.ChangeEntry(handle: entry.0, signalId: entry.1, op: .fullValue, value: ArraySlice(entry.2)))
    }
    return Wire.ChangeSet(txnId: txn, entries: list)
}
