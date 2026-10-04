import Foundation
import UndraRuntime
import PlaygroundCore
import XCTest

extension ContractScenarios {
    // MARK: S04

    func testS04_asyncCall() async {
        await scenario("S04", "async call") {
            let core = try self.core

            // 1. One call: 42, after at least 45 ms.
            let started = ContinuousClock.now
            let sum = try await addLater(a: 20, b: 22, delayMs: 50, ctx: core)
            let elapsed = ContinuousClock.now - started
            try checkEqual(sum, 42, "add_later(20, 22, 50)")
            try check(elapsed >= .milliseconds(45), "add_later(.., 50) returned after only \(elapsed)")
            // The upper bound is waitLimit, a hang detector: a timer that never fires is what it catches. How long after 50 ms
            // the answer comes is the machine's (CI runners stall); step 2's order is the claim that the delays are honoured.
            try check(elapsed < waitLimit, "add_later(.., 50) took \(elapsed), past the \(waitLimit) wait")

            // 2. Three concurrent calls resolve in the order of their delays, with the right values. The claim is that the core
            // honours each call's delay and does not answer in the order the calls were made (the 400 ms call is issued first).
            // It is a claim about the core only on a trial the machine delivered: the three issued together, and no call
            // answered late enough to pass the next one. A call is put behind the next-slower one only by arriving late by the
            // gap to it, less the 20 ms the issues may be apart: 130 ms for the 50 ms call, 180 ms for the 200 ms call (the
            // 400 ms call is last: late is still last). A hosted runner has answered the 50 ms call 150 ms late, which puts the
            // 200 ms call first and says nothing about the core, so that trial is repeated (as the trickle test does); the
            // values and the lower bound on each call's time hold on every trial.
            let calls: [(id: Int32, delayMs: UInt32, lateLimit: Duration?)] = [(1, 400, nil), (2, 50, .milliseconds(130)), (3, 200, .milliseconds(180))]
            var measured: [String] = []
            var delivered = false
            for trial in 0..<30 {
                let timed = try await withThrowingTaskGroup(of: TimedCall.self) { (group: inout ThrowingTaskGroup<TimedCall, any Error>) -> [TimedCall] in
                    for call in calls {
                        group.addTask {
                            let issued = ContinuousClock.now
                            let value = try await addLater(a: call.id, b: 0, delayMs: call.delayMs, ctx: core)
                            let completed = ContinuousClock.now
                            return TimedCall(id: call.id, delay: .milliseconds(Int(call.delayMs)), lateLimit: call.lateLimit, value: value, issued: issued, completed: completed)
                        }
                    }
                    var all: [TimedCall] = []
                    for try await call in group {
                        all.append(call)
                    }
                    return all.sorted { $0.id < $1.id }
                }
                for call in timed {
                    try checkEqual(call.value, call.id, "the value of add_later(\(call.id), 0, \(call.delay.shown))")
                    try check(call.took >= call.delay - .milliseconds(5), "add_later(\(call.id), 0, \(call.delay.shown)) answered after only \(call.took.shown)")
                }
                let issueSpread = (timed.map(\.issued).max() ?? .now) - (timed.map(\.issued).min() ?? .now)
                measured.append("trial \(trial): issued within \(issueSpread.shown), late by " + timed.map { "\($0.id): \($0.late.shown)" }.joined(separator: ", "))
                if issueSpread >= .milliseconds(20) || timed.contains(where: { call in call.lateLimit.map { call.late >= $0 } ?? false }) {
                    continue // the runner held a call up: its place in the order says nothing about the core
                }
                try checkEqual(timed.sorted { $0.completed < $1.completed }.map(\.id), [2, 3, 1], "completion order of delays 400, 50, 200")
                delivered = true
                break
            }
            try check(delivered, "the runner never delivered three concurrent calls within the margins (issued within 20 ms of each other, the 50 ms call answered under 130 ms late, the 200 ms call under 180 ms late) in 30 trials: \(measured.joined(separator: "; "))")

            // 3. A call on an object.
            let probe = try Probe(ctx: core)
            defer { probe.close() }
            let ten = try await probe.wait(ms: 10)
            try checkEqual(ten, 10, "Probe.wait(10)")

            // 4. Re-entrancy. The continuation of one call starts another ...
            let chained = try await Task {
                let first = try await addLater(a: 1, b: 1, delayMs: 5, ctx: core)
                return try await addLater(a: first, b: 10, delayMs: 5, ctx: core)
            }.value
            try checkEqual(chained, 12, "a call started from another call's completion")

            // ... and a change observer calls into the core, synchronously and asynchronously.
            let counter = try RawStore(core: core, type: UndraIds.Objects.Counter.typeId, method: UndraIds.Objects.Counter.new)
            defer { counter.close() }
            counter.observe()
            counter.clear()
            let observed = Locked<(reentered: Bool, later: Int32?)>((false, nil))
            counter.onEntry = { [unowned counter] _ in
                let first = observed.withLock { (current: inout (reentered: Bool, later: Int32?)) -> Bool in
                    let first = !current.reentered
                    current.reentered = true
                    return first
                }
                guard first else {
                    return
                }
                // A sync write from inside the observer: the mirror applies outside the core's lock.
                _ = try? counter.callSync(UndraIds.Objects.Counter.increment)
                Task {
                    let value = try? await addLater(a: 40, b: 2, delayMs: 10, ctx: core)
                    observed.withLock { (current: inout (reentered: Bool, later: Int32?)) -> Void in current.later = value }
                }
            }
            try counter.callSync(UndraIds.Objects.Counter.add, encoded { (w: inout UndraWriter) in w.writeI32(1) })
            try await waitUntil("the async call started from inside the observer") {
                observed.snapshot.later == 42
            }
            try await waitUntil("the sync write made from inside the observer") {
                counter.entries(of: 0).count >= 2
            }
        }
    }

    // MARK: S05

    func testS05_errorPropagation() async {
        await scenario("S05", "error propagation") {
            let core = try self.core
            let badRequestsBefore = core.stat("crossings.bad_requests")

            // 1. A sync typed error is thrown, on the same path as a success would have returned.
            try checkThrows({ try parseCount(text: "x", ctx: core) },
                            LabError.notANumber("x"), "parse_count(\"x\")")

            // 2. An async typed error.
            try await checkThrows({ try await failLater(delayMs: 10, code: 7, ctx: core) },
                                  LabError.rejected(code: 7, reason: "on purpose"), "fail_later(10, 7)")

            // 3. Store errors. A refused add changes nothing: no change-set.
            let todos = try Todos(ctx: core)
            defer { todos.close() }
            let transactionsBefore = core.stat("transactions")
            try await checkThrows({ try await todos.add(title: "   ") },
                                  TodoError.emptyTitle, "Todos.add of blanks")
            try await quietFor(milliseconds: 100)
            try checkEqual(core.stat("transactions") - transactionsBefore, 0, "change-sets after a refused add")
            try checkEqual(todos.todos.count, 0, "todos after a refused add")

            let list = try BigList(ctx: core)
            defer { list.close() }
            try checkThrows({ try list.removeAt(index: 10_000) },
                            ListError.outOfRange(index: 10_000, len: 10_000), "BigList.remove_at(10000)")
            try checkThrows({ try list.insertAt(index: 10_001, label: "x") },
                            ListError.outOfRange(index: 10_001, len: 10_000), "BigList.insert_at(10001, ..)")
            // The refused insert did not consume an identity.
            try checkEqual(try list.insertAt(index: 0, label: "y"), 10_001, "the id of the next insert")

            // 4. Reply statuses that are not typed errors: each is a bad request with a reason.
            try self.checkBadRequest("an unknown method id") {
                _ = try core.callSync(.freeFunction(methodId: 0xDEAD_BEEF), method: 0xDEAD_BEEF, args: [])
            }
            let stale = try core.construct(type: UndraIds.Objects.Probe.typeId, method: UndraIds.Objects.Probe.new, args: [])
            core.release(stale)
            try self.checkBadRequest("a call on a released handle") {
                _ = try core.callSync(
                    .objectMethod(handle: stale, methodId: UndraIds.Objects.Probe.counters),
                    method: UndraIds.Objects.Probe.counters,
                    args: []
                )
            }
            try self.checkBadRequest("a constructor with undecodable arguments") {
                _ = try core.construct(
                    type: UndraIds.Objects.RemoteTodosQueryHandle.typeId,
                    method: UndraIds.Objects.RemoteTodosQueryHandle.new,
                    args: []
                )
            }

            // 5. The core still works, and exactly those three were bad requests.
            try checkEqual(try PlaygroundCore.add(a: 1, b: 2, ctx: core), 3, "add(1, 2) afterwards")
            try checkEqual(core.stat("crossings.bad_requests") - badRequestsBefore, 3, "bad_requests delta")

            // 6. Through the generated bindings, on closed objects (ADR-032): a call that can throw
            // fails as bad request (`UndraCallError.refused`); a command returns and `onError` hears of it.
            let badRequestsBeforeClosed = core.stat("crossings.bad_requests")
            let reportsBefore = Fixture.shared.unhandled.snapshot.count
            let closedList = try BigList(ctx: core)
            closedList.close()
            let refusal = try callError("BigList.remove_at(0) on a closed list") { try closedList.removeAt(index: 0) }
            guard case .refused(let reason) = refusal else {
                throw ScenarioFailure(description: "BigList.remove_at on a closed list failed with \(refusal), not .refused")
            }
            try check(!reason.isEmpty, "the refusal carries no reason")
            let closedCounter = try Counter(ctx: core)
            closedCounter.close()
            closedCounter.increment()
            let reports = Array(Fixture.shared.unhandled.snapshot.dropFirst(reportsBefore))
            try checkEqual(reports.count, 1, "reports to onError for Counter.increment on a closed counter")
            let report = try require(reports.first, "the report of the closed counter's increment()")
            try checkEqual(report.operation, "Counter.increment", "the operation of the report")
            guard case .refused(let why) = report.error, !why.isEmpty else {
                throw ScenarioFailure(description: "Counter.increment on a closed counter reported \(report.error), not .refused")
            }
            try checkEqual(core.stat("crossings.bad_requests") - badRequestsBeforeClosed, 2, "bad_requests delta of the two closed calls")
            try checkEqual(try PlaygroundCore.add(a: 1, b: 2, ctx: core), 3, "add(1, 2) after the closed calls")
        }
    }

    /// `body` must fail with a bad-request reply that carries a reason string.
    private func checkBadRequest(_ what: String, _ body: () throws -> Void) throws {
        do {
            try body()
            throw ScenarioFailure(description: "\(what) was accepted")
        } catch let error as UndraReplyError {
            try checkEqual(error.status, .badRequest, "status of \(what)")
            try check(!(error.message ?? "").isEmpty, "\(what) carries no reason")
        }
    }

    // MARK: S06

    func testS06_cancellation() async {
        await scenario("S06", "cancellation") {
            let core = try self.core
            let probe = try Probe(ctx: core)
            defer { probe.close() }

            // 1. A call that never ends is running in the core.
            let activeBefore = core.stat("active_calls")
            let cancelledBefore = core.stat("crossings.cancelled")
            let hanging = Task { try await probe.hang() }
            try await waitUntil("the hanging call to start") { try probe.counters().started == 1 }

            // 2. Cancelling the task ends the platform call as cancelled.
            hanging.cancel()
            switch await hanging.result {
            case .success(let value):
                throw ScenarioFailure(description: "a cancelled hang() returned \(value)")
            case .failure(let error):
                try check(error is CancellationError, "a cancelled hang() failed with \(error), not CancellationError")
            }

            // 3. The core dropped the future.
            try await waitUntil("the core to drop the hanging future") { try probe.counters().cancelled == 1 }
            try checkEqual(try probe.counters().completed, 0, "completed after the cancel")
            try await waitUntil("active_calls to settle") { core.stat("active_calls") == activeBefore }
            try checkEqual(core.stat("crossings.cancelled") - cancelledBefore, 1, "crossings.cancelled delta")

            // 4. Independence: only the cancelled call is cancelled.
            let waiting = Task { try await probe.wait(ms: 100) }
            let second = Task { try await probe.hang() }
            try await waitUntil("both calls to start") { try probe.counters().started == 3 }
            second.cancel()
            let hundred = try await waiting.value
            try checkEqual(hundred, 100, "the call that was not cancelled")
            _ = await second.result
            try await waitUntil("the second drop") { try probe.counters().cancelled == 2 }
            try checkEqual(try probe.counters().completed, 1, "completed after the independent pair")

            // 5. Cancelling after completion is a no-op.
            let finished = Task { try await probe.wait(ms: 1) }
            _ = try await finished.value
            finished.cancel()
            try await quietFor(milliseconds: 100)
            try checkEqual(try probe.counters().cancelled, 2, "cancelled after a late cancel")

            // 6. A cancelled call of a method with a typed error ends as cancelled too: not as the
            // method's `LabError`, and not as a stopped process (ADR-032).
            let cancelledBeforeTyped = core.stat("crossings.cancelled")
            let typed = Task { try await failLater(delayMs: 5_000, code: 1, ctx: core) }
            try await quietFor(milliseconds: 100)
            let cancelRequested = ContinuousClock.now
            typed.cancel()
            switch await typed.result {
            case .success(let value):
                throw ScenarioFailure(description: "a cancelled fail_later returned \(value)")
            case .failure(let error):
                try check(error is CancellationError, "a cancelled fail_later failed with \(error), not CancellationError")
            }
            // Relative to the call's own 5 s delay, not to a second of wall clock: a cancel that waited for the core's answer
            // would end 4.9 s after it, so under half the delay (2.5 s) tells the two apart on a machine that stalls.
            let cancelTook = ContinuousClock.now - cancelRequested
            try check(cancelTook < .milliseconds(2_500), "the cancelled fail_later took \(cancelTook) to end, not under half of its own 5 s delay")
            try await waitUntil("crossings.cancelled to grow by the typed call") {
                core.stat("crossings.cancelled") - cancelledBeforeTyped == 1
            }
            try checkEqual(try PlaygroundCore.add(a: 1, b: 1, ctx: core), 2, "add(1, 1) after the cancelled typed call")
        }
    }
}

/// One of step 2's three concurrent calls: what it returned and the two instants that place it in time.
private struct TimedCall: Sendable {
    let id: Int32
    let delay: Duration
    /// How late the call may be answered, on a trial that says something about the order; `nil` for the slowest call.
    let lateLimit: Duration?
    let value: Int32
    let issued: ContinuousClock.Instant
    let completed: ContinuousClock.Instant

    /// How long the call took, from just before it was made to just after its result was back.
    var took: Duration { completed - issued }

    /// How long after its delay the result was back.
    var late: Duration { took - delay }
}

private extension Duration {
    /// This duration as milliseconds, for a message ("401.3 ms").
    var shown: String {
        let parts = components
        return String(format: "%.1f ms", Double(parts.seconds) * 1e3 + Double(parts.attoseconds) / 1e15)
    }
}
