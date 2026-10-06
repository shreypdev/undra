// Objects as parameters and returns (ADR-040) and host callback interfaces (ADR-041) on the Swift
// runtime: one wrapper per handle, the callback registry, delivery through the mirror's drain and on
// a background queue, cancellation, error mapping, the weak wrappers, and a measured `adopt`.

import Foundation
import XCTest

@testable import UndraRuntime

// MARK: - Shapes the generator writes

/// A plain object, as generated (`init(adopting:core:)`).
final class Box: UndraObject, @unchecked Sendable {
    init(adopting handle: UndraHandle, core: UndraCore) {
        super.init(core: core, handle: handle)
    }
}

/// A store whose signal 0 is a `u32`, as generated.
@MainActor
final class CountStore: UndraStore, @unchecked Sendable {
    var count: UInt32 = 0

    init(adopting handle: UndraHandle, core: UndraCore) {
        super.init(core: core, handle: handle)
    }

    override func apply(signal: UInt32, op: ChangeOp, reader: inout UndraReader) {
        if signal == 0, op == .fullValue, let value = try? reader.readU32() {
            count = value
        }
    }
}

enum AskError: Error, Equatable {
    case no
}

/// A main-thread callback interface, as generated.
@MainActor
protocol Listener: AnyObject, Sendable {
    func progress(done: UInt32)
    func note(line: String)
    func ask(question: String) async throws(AskError) -> Bool
}

@MainActor
final class WeakListener: Listener, UndraWeakMainCallback {
    private(set) weak var target: (any Listener)?

    init(_ target: any Listener) {
        self.target = target
    }

    var undraTarget: AnyObject? {
        return target
    }

    func progress(done: UInt32) {
        target?.progress(done: done)
    }

    func note(line: String) {
        target?.note(line: line)
    }

    func ask(question: String) async throws(AskError) -> Bool {
        guard let target = target else {
            return await UndraCallbacks.targetGone()
        }
        return try await target.ask(question: question)
    }
}

private enum ListenerIds {
    static let portId: UInt32 = 0x0100
    static let progress: UInt32 = 1
    static let note: UInt32 = 2
    static let ask: UInt32 = 3
    static let releaseInstance: UInt32 = 0x0101
    static let cancelCall: UInt32 = 0x0102
}

private let listenerCallbacks = UndraCallbackInterface(
    name: "Listener",
    portId: ListenerIds.portId,
    releaseInstance: ListenerIds.releaseInstance,
    cancelCall: ListenerIds.cancelCall,
    methods: [
        ListenerIds.progress: .notify("progress", coalesce: true) { (listener: any Listener, args: inout UndraReader) in
            let done = try args.readU32()
            try args.finish()
            listener.progress(done: done)
        },
        ListenerIds.note: .notify("note") { (listener: any Listener, args: inout UndraReader) in
            let line = try args.readString()
            try args.finish()
            listener.note(line: line)
        },
        ListenerIds.ask: .call("ask") { (listener: any Listener, args: inout UndraReader) async throws -> [UInt8] in
            let question = try args.readString()
            try args.finish()
            do {
                return try await listener.ask(question: question) ? [1] : [0]
            } catch let error as AskError {
                _ = error
                throw UndraPortError(body: [7])
            }
        },
    ]
)

/// A background callback interface, as generated.
protocol Sink: AnyObject, Sendable {
    func record(value: UInt32)
    func fetch(key: String) async throws -> UInt32
}

private enum SinkIds {
    static let portId: UInt32 = 0x0200
    static let record: UInt32 = 1
    static let fetch: UInt32 = 2
    static let releaseInstance: UInt32 = 0x0201
    static let cancelCall: UInt32 = 0x0202
}

private let sinkCallbacks = UndraCallbackInterface(
    name: "Sink",
    portId: SinkIds.portId,
    releaseInstance: SinkIds.releaseInstance,
    cancelCall: SinkIds.cancelCall,
    methods: [
        SinkIds.record: .notifyInBackground("record") { (sink: any Sink, args: inout UndraReader) in
            let value = try args.readU32()
            try args.finish()
            sink.record(value: value)
        },
        SinkIds.fetch: .callInBackground("fetch") { (sink: any Sink, args: inout UndraReader) async throws -> [UInt8] in
            let key = try args.readString()
            try args.finish()
            return try await sink.fetch(key: key).undraEncoded()
        },
    ]
)

/// Records what the core calls, with the store's count at each call.
@MainActor
final class Recorder: Listener {
    var events: [String] = []
    var store: CountStore?
    var answer: Result<Bool, AskError> = .success(true)
    var failOtherwise = false
    var waitForCancel = false
    var sawCancellation = false
    var started = 0

    func progress(done: UInt32) {
        events.append("progress \(done)")
    }

    func note(line: String) {
        events.append("note \(line) at \(store?.count ?? 0)")
    }

    func ask(question: String) async throws(AskError) -> Bool {
        started += 1
        events.append("ask \(question)")
        if waitForCancel {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(5))
            }
            sawCancellation = true
            return false
        }
        return try answer.get()
    }
}

/// Where a cancellation handler ran: whether on the thread of the core's port callback (the test calls `callPort`
/// from the main thread).
final class InsidePortCallback: @unchecked Sendable {
    private let lock = NSLock()
    private var seen: [Bool] = []

    func record() {
        let onCallbackThread = Thread.isMainThread
        lock.withLock { seen.append(onCallbackThread) }
    }

    var observations: [Bool] {
        return lock.withLock { seen }
    }
}

/// An `ask` that waits for its cancellation with a cancellation handler (app code `Task.cancel()` runs).
@MainActor
final class CancelProbe: Listener {
    let flag: InsidePortCallback
    var started = 0

    init(flag: InsidePortCallback) {
        self.flag = flag
    }

    func progress(done: UInt32) {}

    func note(line: String) {}

    func ask(question: String) async throws(AskError) -> Bool {
        started += 1
        let flag = self.flag
        await withTaskCancellationHandler {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(5))
            }
        } onCancel: {
            flag.record()
        }
        return false
    }
}

final class SinkRecorder: Sink, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [UInt32] = []
    private var onMain = false
    private var failure: (any Error)?

    func record(value: UInt32) {
        lock.lock()
        values.append(value)
        if Thread.isMainThread {
            onMain = true
        }
        lock.unlock()
    }

    func fetch(key: String) async throws -> UInt32 {
        if let failure = lock.withLock({ failure }) {
            throw failure
        }
        return UInt32(key.count)
    }

    func failing(with error: any Error) {
        lock.withLock { failure = error }
    }

    var recorded: [UInt32] {
        return lock.withLock { values }
    }

    var ranOnMain: Bool {
        return lock.withLock { onMain }
    }
}

/// An error that is not the method's own.
struct Unexpected: Error {}

// MARK: - Helpers

private func handle(_ index: UInt32, _ generation: UInt64 = 1) -> UndraHandle {
    return UndraHandle(index: index, generation: generation)
}

private func body(_ handles: [UndraHandle]) -> [UInt8] {
    var writer = UndraWriter()
    writer.writeU32(UInt32(handles.count))
    for handle in handles {
        writer.writeU64(handle.rawValue)
    }
    return writer.finish()
}

private func args(_ instance: UInt64, _ write: (inout UndraWriter) -> Void = { _ in }) -> [UInt8] {
    var writer = UndraWriter()
    writer.writeU64(instance)
    write(&writer)
    return writer.finish()
}

@MainActor
private func waitFor(_ what: String, timeout: Double = hangDeadline, _ condition: @MainActor () -> Bool) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        if Date() > deadline {
            XCTFail("timed out waiting for \(what)")
            return
        }
        try? await Task.sleep(for: .milliseconds(5))
    }
}

// MARK: - Objects

@MainActor
final class ObjectIdentityTests: XCTestCase {
    func testAReturnedHandleHasOneWrapperAndTheDuplicateReferenceGoesBack() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let first = core.adopt(handle(1)) { Box(adopting: $0, core: $1) }
        let again = core.adopt(handle(1)) { Box(adopting: $0, core: $1) }
        XCTAssertTrue(first === again)
        XCTAssertEqual(transport.releases, [handle(1).rawValue], "the duplicate reference is given back at once")
        XCTAssertEqual(core.identities.count, 1)
        first.close()
        again.close()
        XCTAssertEqual(transport.releases, [handle(1).rawValue, handle(1).rawValue], "the wrapper gives its own back once")
        XCTAssertEqual(core.identities.count, 0, "nothing is left in the map")
    }

    func testAClosedWrapperIsReplacedAndAConstructedOneIsFound() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let constructed = Box(adopting: handle(2), core: core)
        XCTAssertTrue(core.adopt(handle(2)) { Box(adopting: $0, core: $1) } === constructed, "a constructor's wrapper is in the map")
        constructed.close()
        let fresh = core.adopt(handle(2)) { Box(adopting: $0, core: $1) }
        XCTAssertFalse(fresh === constructed)
        XCTAssertFalse(fresh.isClosed)
    }

    func testTheFinalizerReleasesOnceAndAGoneWrapperLeavesNothing() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        weak var gone: Box?
        do {
            let box = core.adopt(handle(3)) { Box(adopting: $0, core: $1) }
            gone = box
            box.close()
            box.close()
        }
        XCTAssertNil(gone)
        XCTAssertEqual(transport.releases, [handle(3).rawValue])
        do {
            _ = core.adopt(handle(4)) { Box(adopting: $0, core: $1) }
        }
        XCTAssertEqual(transport.releases, [handle(3).rawValue, handle(4).rawValue], "deinit released the dropped wrapper")
        XCTAssertEqual(core.identities.count, 0)
        XCTAssertEqual(core.stats().hostLiveHandles, 0)
    }

    func testListsAndOptionalsAreAdoptedAsTheyAreRead() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport)
        let boxes = try core.adoptList(body([handle(1), handle(2), handle(1)])) { Box(adopting: $0, core: $1) }
        XCTAssertEqual(boxes.count, 3)
        XCTAssertTrue(boxes[0] === boxes[2])
        XCTAssertEqual(Set(boxes).count, 2, "Hashable by identity")
        XCTAssertEqual(transport.releases, [handle(1).rawValue])
        XCTAssertNil(try core.adoptOptional([0]) { Box(adopting: $0, core: $1) })
        var some = UndraWriter()
        some.writeU8(1)
        some.writeU64(handle(2).rawValue)
        XCTAssertTrue(try core.adoptOptional(some.finish()) { Box(adopting: $0, core: $1) } === boxes[1])
        XCTAssertThrowsError(try core.adoptObject(UndraHandle.null.undraEncoded()) { Box(adopting: $0, core: $1) }) { error in
            XCTAssertEqual(UndraCallError.mapped(error) as? UndraCallError, .malformed(UndraProtocolError.nullHandle.description))
        }
        XCTAssertThrowsError(try core.adoptObject(handle(5).undraEncoded() + [0]) { Box(adopting: $0, core: $1) })
        XCTAssertThrowsError(try core.adoptList([9, 0, 0, 0]) { Box(adopting: $0, core: $1) }, "a count the body cannot hold")
    }

    func testAnObjectOfAnotherCoreIsRefusedBeforeAnythingIsSent() throws {
        let transportA = FakeTransport()
        let transportB = FakeTransport()
        let coreA = try makeCore(transportA)
        let coreB = try makeCore(transportB)
        // The same handle number in both cores: only the core says which object it is.
        let mine = coreA.adopt(handle(1)) { Box(adopting: $0, core: $1) }
        let theirs = coreB.adopt(handle(1)) { Box(adopting: $0, core: $1) }
        XCTAssertNoThrow(try coreA.requireOwn(mine))
        XCTAssertNoThrow(try coreA.requireOwn(nil as Box?))
        XCTAssertThrowsError(try coreA.requireOwn([mine, theirs])) { error in
            guard case .refused(let reason)? = error as? UndraCallError else {
                return XCTFail("\(error)")
            }
            XCTAssertTrue(reason.contains("Box") && reason.contains("another Undra core"), reason)
        }
        XCTAssertTrue(transportA.calls.isEmpty && transportB.calls.isEmpty)
    }

    func testAStoreReturnedTwiceIsMirroredOnce() throws {
        let transport = FakeTransport()
        let frames = ManualFrameScheduler()
        let core = try makeCore(transport, frames: frames)
        let store = core.adopt(handle(6)) { CountStore(adopting: $0, core: $1) }
        let again = core.adopt(handle(6)) { CountStore(adopting: $0, core: $1) }
        XCTAssertTrue(store === again)
        XCTAssertEqual(core.mirror.registeredCount, 1)
        transport.deliverChangeSet(makeChangeSet([(handle(6), 0, UInt32(4).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(store.count, 4)
        XCTAssertEqual(core.mirror.stats().entriesApplied, 1, "one store applied the entry once")
    }

    /// Objects-followups O2(a): an `Arc<Self>` `new` of a store gives a second wrapper of an object the host
    /// already wraps (a Swift initializer cannot return the first). It must not take the first one's mirror
    /// registration over: both keep receiving the changes, and closing either leaves the other routed.
    func testASecondWrapperOfAStoreDoesNotTakeTheFirstOnesRoutingOver() throws {
        let transport = FakeTransport()
        let frames = ManualFrameScheduler()
        let core = try makeCore(transport, frames: frames)
        let first = CountStore(adopting: handle(7), core: core)
        let second = CountStore(adopting: handle(7), core: core)
        XCTAssertEqual(core.mirror.registeredCount, 1, "one handle, one registration")
        transport.deliverChangeSet(makeChangeSet([(handle(7), 0, UInt32(5).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(first.count, 5, "the first wrapper keeps updating")
        XCTAssertEqual(second.count, 5, "and so does the second")

        first.close()
        transport.deliverChangeSet(makeChangeSet([(handle(7), 0, UInt32(6).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(second.count, 6, "closing one wrapper leaves the other routed")
        XCTAssertEqual(first.count, 5, "the closed one is not updated")
        XCTAssertEqual(core.mirror.registeredCount, 1)

        second.close()
        XCTAssertEqual(core.mirror.registeredCount, 0, "the registration goes with the last wrapper")
        XCTAssertEqual(transport.releases, [handle(7).rawValue, handle(7).rawValue], "each wrapper gave back its own reference")
    }

    /// Objects-followups O2(b): a `close()` racing an `adopt` of the same handle. The other thread adopts at the
    /// moment the closing wrapper is marked closed and has not left the identity map: its routing and its observed
    /// signals must survive the close (they were unregistered and cleared after `forget`, not atomically with it).
    func testACloseRacingAnAdoptOfTheSameHandleLeavesTheNewWrapperRoutedAndObserved() throws {
        let transport = FakeTransport()
        let frames = ManualFrameScheduler()
        let core = try makeCore(transport, frames: frames)
        let old = core.adopt(handle(8)) { CountStore(adopting: $0, core: $1) }
        core.observe(handle(8), signal: Observe.allSignals, on: true)
        XCTAssertTrue(core.isObserved(handle(8)))

        var adopted: CountStore?
        UndraObject.testHookAfterMarkedClosed = { closing in
            guard closing === old else {
                return
            }
            UndraObject.testHookAfterMarkedClosed = nil
            // The core answered another call with the same handle: one more reference, a new wrapper.
            let made = core.adopt(handle(8)) { CountStore(adopting: $0, core: $1) }
            core.observe(handle(8), signal: Observe.allSignals, on: true)
            adopted = made
        }
        defer {
            UndraObject.testHookAfterMarkedClosed = nil
        }
        old.close()
        let new = try XCTUnwrap(adopted)
        XCTAssertFalse(new === old)
        XCTAssertEqual(core.mirror.registeredCount, 1, "the new wrapper is still registered")
        transport.deliverChangeSet(makeChangeSet([(handle(8), 0, UInt32(9).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(new.count, 9, "and still receives the changes")
        XCTAssertTrue(core.isObserved(handle(8)), "its observed signals were not cleared by the old wrapper's close")
        new.close()
        XCTAssertFalse(core.isObserved(handle(8)), "the last wrapper's close clears them")
        XCTAssertEqual(core.mirror.registeredCount, 0)
    }

    /// Adopting on the main thread while other threads close what was adopted, of one handle, leaves the last wrapper
    /// routed, observed and counted exactly once afterwards (no lost registration, no deadlock between the identity
    /// map's lock and the mirror's or the core's).
    func testAdoptingWhileOtherThreadsCloseTheSameHandleIsConsistent() throws {
        let transport = FakeTransport()
        let frames = ManualFrameScheduler()
        let core = try makeCore(transport, frames: frames)
        let closing = DispatchGroup()
        for _ in 0 ..< 600 {
            let wrapper = core.adopt(handle(9)) { CountStore(adopting: $0, core: $1) }
            core.observe(handle(9), signal: Observe.allSignals, on: true)
            DispatchQueue.global().async(group: closing) {
                wrapper.close()
            }
        }
        XCTAssertEqual(closing.wait(timeout: .now() + hangDeadline), .success)
        let last = core.adopt(handle(9)) { CountStore(adopting: $0, core: $1) }
        core.observe(handle(9), signal: Observe.allSignals, on: true)
        XCTAssertEqual(core.mirror.registeredCount, 1, "only the last wrapper is registered")
        transport.deliverChangeSet(makeChangeSet([(handle(9), 0, UInt32(3).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(last.count, 3)
        last.close()
        XCTAssertEqual(core.mirror.registeredCount, 0)
        XCTAssertEqual(core.identities.count, 0)
        XCTAssertFalse(core.isObserved(handle(9)))
    }

    /// Review of O2: eight threads adopt and close wrappers of one handle at the same time, and wrappers are shared
    /// between them (an adopt of a live wrapper returns it, so several threads close the same object). Whatever the
    /// interleaving, every reference the core issued goes back exactly once (the interned adopt's extra at once, the
    /// wrapper's own with its close), the identity map holds nothing afterwards, and it is not wedged: the next adopt
    /// makes a fresh wrapper of the handle.
    func testEightThreadsAdoptingAndClosingOneHandleLoseNoReference() throws {
        let transport = FakeTransport()
        let core = try makeCore(transport, frames: ManualFrameScheduler())
        let threads = 8
        let rounds = 2_000
        // A deadlock is a failure, not a hung run: the storm runs off this thread and is waited for.
        let finished = DispatchGroup()
        DispatchQueue.global().async(group: finished) {
            DispatchQueue.concurrentPerform(iterations: threads) { _ in
                for _ in 0 ..< rounds {
                    // The second adopt's wrapper (the live one, or a duplicate that is discarded) is dropped
                    // without `close()` at the end of the statement: its `deinit` closes it, on this thread,
                    // while the others adopt and close the same handle.
                    let wrapper = core.adopt(handle(21)) { Box(adopting: $0, core: $1) }
                    _ = core.adopt(handle(21)) { Box(adopting: $0, core: $1) }
                    wrapper.close()
                }
            }
        }
        guard finished.wait(timeout: .now() + 60) == .success else {
            XCTFail("eight threads adopting and closing one handle deadlocked (the identity map's lock is not re-entered by a wrapper's deinit)")
            return
        }
        XCTAssertEqual(transport.releases.count, threads * rounds * 2, "one release per reference the core issued")
        XCTAssertEqual(Set(transport.releases), [handle(21).rawValue])
        XCTAssertEqual(core.identities.count, 0, "no entry outlives its wrappers")
        let fresh = core.adopt(handle(21)) { Box(adopting: $0, core: $1) }
        XCTAssertEqual(core.identities.count, 1)
        fresh.close()
        XCTAssertEqual(core.identities.count, 0)
        XCTAssertEqual(transport.releases.count, threads * rounds * 2 + 1)
    }

    /// The same race with stores, which also own a routing in the mirror and an observed set: wrappers are made on
    /// the main actor (as stores are), closed on seven other threads, and afterwards the last one is routed and
    /// observed and its close clears both.
    func testAStoreAdoptedOnTheMainThreadWhileSevenThreadsCloseItsOlderWrappersEndsRoutedAndObserved() throws {
        let transport = FakeTransport()
        let frames = ManualFrameScheduler()
        let core = try makeCore(transport, frames: frames)
        let closers = DispatchQueue(label: "closers", attributes: .concurrent)
        let closing = DispatchGroup()
        var kept: [CountStore] = []
        for round in 0 ..< 2_000 {
            let wrapper = core.adopt(handle(22)) { CountStore(adopting: $0, core: $1) }
            core.observe(handle(22), signal: Observe.allSignals, on: true)
            if round % 7 == 6 {
                kept.append(wrapper)
            }
            for _ in 0 ..< 3 {
                closers.async(group: closing) {
                    wrapper.close()
                }
            }
        }
        XCTAssertEqual(closing.wait(timeout: .now() + hangDeadline), .success)
        kept.removeAll()
        let last = core.adopt(handle(22)) { CountStore(adopting: $0, core: $1) }
        core.observe(handle(22), signal: Observe.allSignals, on: true)
        XCTAssertEqual(core.mirror.registeredCount, 1)
        transport.deliverChangeSet(makeChangeSet([(handle(22), 0, UInt32(5).undraEncoded())]))
        frames.fire()
        XCTAssertEqual(last.count, 5)
        last.close()
        XCTAssertEqual(core.mirror.registeredCount, 0)
        XCTAssertEqual(core.identities.count, 0)
        XCTAssertFalse(core.isObserved(handle(22)))
    }

    func testStatisticsReadHostReferencesTolerantly() {
        XCTAssertEqual(UndraStats(json: "{\"live_handles\":2,\"host_refs\":3}").hostRefs, 3)
        XCTAssertEqual(UndraStats(json: "{\"live_handles\":2}").hostRefs, 0)
    }

    /// Host-side `adopt` (ADR-040 implementation brief 8): a first adoption (make and register) and an
    /// adoption of a handle that has a live wrapper (the interned path, which gives the extra
    /// reference back). Printed for bench/RESULTS.md.
    func testAdoptCostIsMeasured() throws {
        let transport = NullReleaseTransport()
        let core = try UndraCore.connect(
            transport: transport,
            options: .inproc(expectedSchemaHash: 0x1234),
            frameScheduler: ManualFrameScheduler()
        )
        let rounds = 20_000
        var kept: [Box] = []
        kept.reserveCapacity(rounds)
        let clock = ContinuousClock()
        let first = clock.measure {
            for index in 1 ... rounds {
                kept.append(core.adopt(handle(UInt32(index))) { Box(adopting: $0, core: $1) })
            }
        }
        let interned = clock.measure {
            for index in 1 ... rounds {
                _ = core.adopt(handle(UInt32(index))) { Box(adopting: $0, core: $1) }
            }
        }
        func nanoseconds(_ duration: Duration) -> Double {
            let parts = duration.components
            return (Double(parts.seconds) * 1e9 + Double(parts.attoseconds) / 1e9) / Double(rounds)
        }
        print(String(format: "BENCH swift adopt/first %.0f ns/op", nanoseconds(first)))
        print(String(format: "BENCH swift adopt/interned %.0f ns/op", nanoseconds(interned)))
        XCTAssertEqual(core.identities.count, rounds)
        kept.removeAll()
        XCTAssertEqual(core.identities.count, 0)
    }
}

/// A transport whose release costs nothing, for the measurement.
private final class NullReleaseTransport: UndraTransport, @unchecked Sendable {
    var mode: UndraMode { .inproc }
    var supportsDirectSync: Bool { true }
    func start(inbound: any UndraInbound, options: TransportStartOptions) throws -> TransportInfo {
        return TransportInfo(schemaHash: 0x1234)
    }
    func send(call payload: [UInt8]) -> Bool { false }
    func callSync(_ payload: [UInt8]) throws -> [UInt8] { throw UndraTransportError.closed }
    func cancel(callId: UInt32) {}
    func streamCredit(callId: UInt32, credit: UInt32) {}
    func observe(handle: UndraHandle, signal: UInt32, on: Bool) {}
    func release(handle: UndraHandle) {}
    func registerPort(_ portId: UInt32) {}
    func portReply(_ payload: [UInt8]) {}
    func event(portId: UInt32, methodId: UInt32, payload: [UInt8]) {}
    func timerFired(_ timerId: UInt32) {}
    func snapshot() throws -> [UInt8] { [] }
    func restore(_ payload: [UInt8]) throws {}
    func statsJSON() -> String? { nil }
    func shutdown() {}
}

// MARK: - Callbacks

@MainActor
final class CallbackTests: XCTestCase {
    private var transport = FakeTransport()
    private var frames = ManualFrameScheduler()
    private var core: UndraCore!
    private let reports = ReportSink()

    override func setUp() async throws {
        transport = FakeTransport()
        frames = ManualFrameScheduler()
        var options = LoadOptions.inproc(expectedSchemaHash: 0x1234)
        let sink = reports
        options.onError = { report in sink.add(report) }
        core = try UndraCore.connect(transport: transport, options: options, frameScheduler: frames)
        core.callbacks.install([listenerCallbacks, sinkCallbacks])
    }

    override func tearDown() async throws {
        core.shutdown()
    }

    private func call(_ method: UInt32, port: UInt32 = ListenerIds.portId, id: UInt32 = 0, _ arguments: [UInt8]) -> PortCallOutcome {
        return transport.callPort(portId: port, methodId: method, portCallId: id, args: arguments)
    }

    private func note(_ instance: UInt64, _ line: String) {
        XCTAssertEqual(call(ListenerIds.note, args(instance) { $0.writeString(line) }), .async, "the port callback only queues")
    }

    func testTheRegistryInternsCountsAndNeverFreesEarly() {
        let a = Recorder()
        let b = Recorder()
        let instanceA = core.callbacks.lend(a)
        XCTAssertEqual(instanceA, 1, "a per-core counter starting at 1")
        XCTAssertEqual(core.callbacks.lend(a), instanceA, "the same implementation is the same instance")
        let instanceB = core.callbacks.lend(b)
        XCTAssertEqual(instanceB, 2)
        XCTAssertEqual(core.callbacks.count(of: a), 2)
        XCTAssertEqual(core.callbacks.liveCount, 2)
        core.callbacks.release(instanceA)
        core.callbacks.giveBack(instanceA)
        XCTAssertEqual(core.callbacks.count(of: a), 0)
        core.callbacks.release(instanceA) // over-release: logged, nothing else
        XCTAssertEqual(core.callbacks.liveCount, 1)
        XCTAssertEqual(core.callbacks.lend(a), 3, "an instance handle is never reused")
        XCTAssertTrue(transport.sent.contains { if case .registerPort(let id) = $0 { return id == ListenerIds.portId } else { return false } })
    }

    func testAnImplementationIsHeldStronglyUntilTheCoreLetsGo() {
        weak var gone: Recorder?
        var instance: UInt64 = 0
        do {
            let inline = Recorder()
            gone = inline
            instance = core.callbacks.lend(inline)
        }
        XCTAssertNotNil(gone, "the registry holds it")
        XCTAssertEqual(call(ListenerIds.releaseInstance, args(instance)), .async)
        XCTAssertNotNil(gone, "a release takes effect when the drain reaches it")
        frames.fire()
        XCTAssertNil(gone)
        XCTAssertEqual(core.callbacks.liveCount, 0)
    }

    /// Objects-followups O5: a lost connection keeps what the core holds for the session's return (the server keeps
    /// the objects, and with them the proxies of the instances lent, ADR-051); only closing the core drops it.
    func testALostConnectionKeepsTheRegistryAndClosingTheCoreDropsIt() async throws {
        let rep = Recorder()
        rep.waitForCancel = true
        let instance = core.callbacks.lend(rep)
        // A call the lost connection asked for is running: it is cancelled (the server answered it unavailable).
        _ = call(ListenerIds.ask, id: 9, args(instance) { $0.writeString("running") })
        frames.fire()
        await waitFor("the call to start") { rep.started == 1 }
        transport.drop()
        await waitFor("the running call to see its cancellation") { rep.sawCancellation }
        XCTAssertEqual(core.callbacks.liveCount, 1, "kept while reconnecting")
        XCTAssertEqual(core.callbacks.count(of: rep), 1)
        transport.reconnect()
        XCTAssertEqual(core.callbacks.count(of: rep), 1, "and still there once the connection is back")
        XCTAssertEqual(call(ListenerIds.releaseInstance, args(instance)), .async, "the session's proxy gives its reference back")
        frames.fire()
        XCTAssertEqual(core.callbacks.liveCount, 0)
        let kept = Recorder()
        _ = core.callbacks.lend(kept)
        core.shutdown()
        XCTAssertEqual(core.callbacks.liveCount, 0, "dropped with the core")
    }

    func testCallsRunAtTheDrainInOrderAfterTheChangeSetsBeforeThem() {
        let store = core.adopt(handle(1)) { CountStore(adopting: $0, core: $1) }
        let rep = Recorder()
        rep.store = store
        let instance = core.callbacks.lend(rep)
        transport.deliverChangeSet(makeChangeSet([(handle(1), 0, UInt32(1).undraEncoded())]))
        note(instance, "one")
        transport.deliverChangeSet(makeChangeSet([(handle(1), 0, UInt32(2).undraEncoded())]))
        note(instance, "two")
        transport.deliverChangeSet(makeChangeSet([(handle(1), 0, UInt32(3).undraEncoded())]))
        XCTAssertEqual(rep.events, [], "nothing ran inside the port callback")
        frames.fire()
        XCTAssertEqual(rep.events, ["note one at 1", "note two at 2"])
        XCTAssertEqual(store.count, 3)
        XCTAssertEqual(core.mirror.stats().callbacksDelivered, 2)
    }

    func testACoalescingMethodDeliversOnlyTheNewestPerDrain() {
        let rep = Recorder()
        let instance = core.callbacks.lend(rep)
        for step: UInt32 in 1 ... 50 {
            XCTAssertEqual(call(ListenerIds.progress, args(instance) { $0.writeU32(step) }), .async)
            note(instance, "\(step)")
        }
        frames.fire()
        XCTAssertEqual(rep.events.filter { $0.hasPrefix("progress") }, ["progress 50"])
        XCTAssertEqual(rep.events.filter { $0.hasPrefix("note") }.count, 50)
        XCTAssertEqual(rep.events.last, "note 50 at 0")
        XCTAssertEqual(rep.events[rep.events.count - 2], "progress 50", "delivered at its newest place")
    }

    func testAReleaseWaitsForTheCallsQueuedBeforeIt() {
        let rep = Recorder()
        let instance = core.callbacks.lend(rep)
        note(instance, "last")
        _ = call(ListenerIds.releaseInstance, args(instance))
        XCTAssertEqual(core.callbacks.count(of: rep), 1)
        frames.fire()
        XCTAssertEqual(rep.events, ["note last at 0"])
        XCTAssertEqual(core.callbacks.count(of: rep), 0)
        XCTAssertEqual(call(ListenerIds.ask, id: 9, args(instance) { $0.writeString("?") }), .unavailable, "a released instance answers unavailable")
    }

    func testAnAsyncCallAnswersItsValueItsErrorOrUnavailable() async {
        let rep = Recorder()
        let instance = core.callbacks.lend(rep)
        _ = call(ListenerIds.ask, id: 1, args(instance) { $0.writeString("go?") })
        frames.fire()
        await waitFor("the reply") { self.transport.portReplies.count == 1 }
        XCTAssertEqual(transport.portReplies.last, Wire.PortReply(portCallId: 1, status: .ok, body: [1]))
        rep.answer = .failure(.no)
        _ = call(ListenerIds.ask, id: 2, args(instance) { $0.writeString("go?") })
        frames.fire()
        await waitFor("the typed error") { self.transport.portReplies.count == 2 }
        XCTAssertEqual(transport.portReplies.last, Wire.PortReply(portCallId: 2, status: .error, body: [7]))
        // Arguments that do not decode are another failure: reported, answered unavailable.
        _ = call(ListenerIds.ask, id: 3, args(instance))
        frames.fire()
        await waitFor("unavailable") { self.transport.portReplies.count == 3 }
        XCTAssertEqual(transport.portReplies.last, Wire.PortReply(portCallId: 3, status: .unavailable))
        XCTAssertEqual(reports.operations, ["Listener.ask"])
        // A fire-and-forget failure is only reported.
        _ = call(ListenerIds.note, args(instance))
        frames.fire()
        XCTAssertEqual(reports.operations, ["Listener.ask", "Listener.note"])
        XCTAssertEqual(transport.portReplies.count, 3)
    }

    func testACancelDropsAQueuedCallAndCancelsARunningOne() async {
        let rep = Recorder()
        rep.waitForCancel = true
        let instance = core.callbacks.lend(rep)
        _ = call(ListenerIds.ask, id: 5, args(instance) { $0.writeString("queued") })
        _ = call(ListenerIds.cancelCall, args(instance) { $0.writeU32(5) })
        frames.fire()
        try? await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(rep.started, 0, "a call cancelled before the drain never starts")
        _ = call(ListenerIds.ask, id: 6, args(instance) { $0.writeString("running") })
        frames.fire()
        await waitFor("the call to start") { rep.started == 1 }
        XCTAssertEqual(call(ListenerIds.cancelCall, args(instance) { $0.writeU32(6) }), .async)
        await waitFor("the task to see its cancellation") { rep.sawCancellation }
        try? await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(transport.portReplies.isEmpty, "a late answer is not sent")
        XCTAssertTrue(reports.operations.isEmpty)
    }

    func testACancelNeverRunsTheImplementationsCancellationHandlerInsideTheCoresCallback() async {
        let flag = InsidePortCallback()
        let probe = CancelProbe(flag: flag)
        let instance = core.callbacks.lend(probe)
        _ = call(ListenerIds.ask, id: 7, args(instance) { $0.writeString("running") })
        frames.fire()
        await waitFor("the call to start") { probe.started == 1 }
        XCTAssertTrue(Thread.isMainThread, "the port callback below runs on this thread")
        XCTAssertEqual(call(ListenerIds.cancelCall, args(instance) { $0.writeU32(7) }), .async)
        await waitFor("the cancellation handler") { !flag.observations.isEmpty }
        XCTAssertEqual(flag.observations, [false], "app code (onCancel) ran inside the core's port callback")
    }

    func testAWeakWrapperForwardsWhileItsTargetLivesAndThenAnswersUnavailable() async {
        var target: Recorder? = Recorder()
        weak var gone = target
        let wrapper = WeakListener(target!)
        let instance = core.callbacks.lend(wrapper)
        note(instance, "alive")
        frames.fire()
        XCTAssertEqual(target?.events, ["note alive at 0"])
        target = nil
        XCTAssertNil(gone)
        note(instance, "gone")
        _ = call(ListenerIds.ask, id: 8, args(instance) { $0.writeString("?") })
        frames.fire()
        await waitFor("the answer") { !self.transport.portReplies.isEmpty }
        XCTAssertEqual(transport.portReplies, [Wire.PortReply(portCallId: 8, status: .unavailable)])
        XCTAssertTrue(reports.operations.isEmpty, "a gone target is not a failure to report")
    }

    func testABackgroundInterfaceRunsInCallOrderOffTheMainThreadWithoutAFrame() async {
        let sink = SinkRecorder()
        let instance = core.callbacks.lend(sink)
        DispatchQueue.global().sync {
            for value: UInt32 in 1 ... 200 {
                _ = transport.callPort(portId: SinkIds.portId, methodId: SinkIds.record, portCallId: 0, args: args(instance) { $0.writeU32(value) })
            }
        }
        await waitFor("every call") { sink.recorded.count == 200 }
        XCTAssertEqual(sink.recorded, Array(1 ... 200))
        XCTAssertFalse(sink.ranOnMain)
        XCTAssertEqual(frames.requests, 0, "no frame was needed")
        _ = transport.callPort(portId: SinkIds.portId, methodId: SinkIds.fetch, portCallId: 4, args: args(instance) { $0.writeString("four") })
        await waitFor("the reply") { !self.transport.portReplies.isEmpty }
        XCTAssertEqual(transport.portReplies.last, Wire.PortReply(portCallId: 4, status: .ok, body: ArraySlice(UInt32(4).undraEncoded())))
        sink.failing(with: Unexpected())
        _ = transport.callPort(portId: SinkIds.portId, methodId: SinkIds.fetch, portCallId: 5, args: args(instance) { $0.writeString("x") })
        await waitFor("the second reply") { self.transport.portReplies.count == 2 }
        XCTAssertEqual(transport.portReplies.last, Wire.PortReply(portCallId: 5, status: .unavailable))
        XCTAssertEqual(reports.operations, ["Sink.fetch"])
        _ = transport.callPort(portId: SinkIds.portId, methodId: SinkIds.releaseInstance, portCallId: 0, args: args(instance))
        await waitFor("the release") { self.core.callbacks.liveCount == 0 }
    }

    func testARefusedOrUnsentCallGivesItsCallbacksBack() throws {
        let rep = Recorder()
        transport.onCallSync = { call in
            Wire.Reply(callId: call.callId, status: .badRequest, body: ArraySlice(UndraWriter.encodedString("stale handle"))).encode()
        }
        let refused = core.callbacks.lend(rep)
        XCTAssertThrowsError(try core.callSync(.freeFunction(methodId: 1), method: 1, args: args(refused), lending: [refused]))
        XCTAssertEqual(core.callbacks.count(of: rep), 0, "a refusal transfers nothing")
        transport.onCallSync = { call in Wire.Reply(callId: call.callId, status: .ok, body: []).encode() }
        let kept = core.callbacks.lend(rep)
        _ = try core.callSync(.freeFunction(methodId: 1), method: 1, args: args(kept), lending: [kept, nil])
        XCTAssertEqual(core.callbacks.count(of: rep), 1, "the core owns what it received")
        core.shutdown()
        XCTAssertEqual(core.callbacks.liveCount, 0, "a core that is gone holds nothing")
        let unsent = core.callbacks.lend(rep)
        XCTAssertThrowsError(try core.callSync(.freeFunction(methodId: 1), method: 1, args: args(unsent), lending: [unsent]))
        XCTAssertEqual(core.callbacks.count(of: rep), 0, "a call that never reached the core transfers nothing")
    }

    /// Objects-followups O1: a stream that lends a callback gives it back when the core refuses it (status 5), or
    /// when the call never reaches the core, and keeps it for everything else.
    func testAStreamTheCoreRefusesOrNeverReceivedGivesItsCallbacksBack() async throws {
        let rep = Recorder()
        // Refused: the open reply is status 5.
        transport.onCall = { call, fake in
            fake.deliver(Wire.Reply(callId: call.callId, status: .badRequest, body: ArraySlice(UndraWriter.encodedString("stale handle"))))
            return true
        }
        let refused = core.callbacks.lend(rep)
        do {
            for try await _ in core.stream(.freeFunction(methodId: 1), method: 1, args: args(refused), lending: [refused], decode: { $0 }) {}
            XCTFail("a refused stream did not fail")
        } catch let error as UndraReplyError {
            XCTAssertEqual(error.status, .badRequest)
        }
        XCTAssertEqual(core.callbacks.count(of: rep), 0, "a refusal transfers nothing")

        // Opened and served: the core owns the reference, whether the stream ends or the consumer stops.
        transport.onCall = { call, fake in
            fake.openStream(call.callId, items: [[1], [2]])
            return true
        }
        let served = core.callbacks.lend(rep)
        var items: [[UInt8]] = []
        for try await item in core.stream(.freeFunction(methodId: 1), method: 1, args: args(served), lending: [served], decode: { $0 }) {
            items.append(item)
        }
        XCTAssertEqual(items, [[1], [2]])
        XCTAssertEqual(core.callbacks.count(of: rep), 1, "the core owns what it opened a stream with")

        // A failure the core produced after it read the arguments keeps the reference too.
        transport.onCall = { call, fake in
            fake.openStream(call.callId, items: [], ending: .failure(Wire.StreamFailure(status: .cancelled, message: "restore", detail: "")))
            return true
        }
        let cancelled = core.callbacks.lend(rep)
        do {
            for try await _ in core.stream(.freeFunction(methodId: 1), method: 1, args: args(cancelled), lending: [cancelled], decode: { $0 }) {}
            XCTFail("a cancelled stream did not fail")
        } catch {
            // expected
        }
        XCTAssertEqual(core.callbacks.count(of: rep), 2)

        // A consumer whose decode fails ends a stream the core served: not a refusal.
        transport.onCall = { call, fake in
            fake.openStream(call.callId, items: [[9]])
            return true
        }
        let decoded = core.callbacks.lend(rep)
        do {
            for try await _ in core.stream(.freeFunction(methodId: 1), method: 1, args: args(decoded), lending: [decoded], decode: { (_: [UInt8]) throws -> UInt8 in throw Unexpected() }) {}
            XCTFail("a failing decode did not fail the stream")
        } catch {
            // expected
        }
        XCTAssertEqual(core.callbacks.count(of: rep), 3, "an item the core sent that does not decode leaves the reference with it")

        // Never sent: the core is gone.
        core.shutdown()
        XCTAssertEqual(core.callbacks.liveCount, 0)
        let unsent = core.callbacks.lend(rep)
        do {
            for try await _ in core.stream(.freeFunction(methodId: 1), method: 1, args: args(unsent), lending: [unsent], decode: { $0 }) {}
            XCTFail("a stream on a closed core did not fail")
        } catch {
            // expected
        }
        XCTAssertEqual(core.callbacks.count(of: rep), 0, "a stream that never reached the core transfers nothing")
    }

    /// A stream whose call could not be made (an object of another core) fails at its first element.
    func testAFailedStreamThrowsTheMappedErrorAtItsFirstElement() async throws {
        let stream: AsyncThrowingStream<UInt8, Error> = core.failedStream(
            UndraCallError.refused(reason: "another core"),
            mapError: { UndraCallError.mapped(streamFailure: $0) }
        )
        do {
            for try await _ in stream {
                XCTFail("a failed stream yielded")
            }
            XCTFail("a failed stream ended without failing")
        } catch let error as UndraCallError {
            XCTAssertEqual(error, .refused(reason: "another core"))
        }
        XCTAssertTrue(transport.calls.isEmpty, "nothing was sent")
    }
}

/// What `LoadOptions.onError` received.
final class ReportSink: @unchecked Sendable {
    private let lock = NSLock()
    private var reports: [UndraUnhandledError] = []

    func add(_ report: UndraUnhandledError) {
        lock.withLock { reports.append(report) }
    }

    var operations: [String] {
        return lock.withLock { reports.map(\.operation) }
    }
}

extension UndraWriter {
    /// A string, encoded on its own.
    static func encodedString(_ text: String) -> [UInt8] {
        var writer = UndraWriter()
        writer.writeString(text)
        return writer.finish()
    }
}
