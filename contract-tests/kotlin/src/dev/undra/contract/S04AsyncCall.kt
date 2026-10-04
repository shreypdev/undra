package dev.undra.contract

import dev.undra.playground.core.UndraIds
import dev.undra.playground.core.Probe
import dev.undra.playground.core.addLater
import dev.undra.runtime.wire.Handle
import dev.undra.runtime.wire.Payloads.CallTarget
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking

/** S04: async calls wait on the core's timer and resume the caller off the core's threads. */
fun s04AsyncCall(w: World) {
    // 1. One call: the right value, after roughly the delay.
    val started = System.nanoTime()
    val sum = runBlocking { addLater(20, 22, 50u) }
    val elapsedMs = (System.nanoTime() - started) / 1_000_000L
    expectEq("add_later(20, 22, 50)", 42, sum)
    check(elapsedMs >= 45) { "add_later(.., 50 ms) answered after $elapsedMs ms" }
    // The upper bound is WAIT_MS, a hang detector: a timer that never fires is what it catches. How long after 50 ms the
    // answer comes is the machine's (CI runners stall); step 2's order is the claim that the delays are honoured.
    check(elapsedMs < WAIT_MS) { "add_later(.., 50 ms) took $elapsedMs ms, past the $WAIT_MS ms wait" }

    // 2. Three at once resolve in the order of their delays, each with its own value. The claim is that the core honours
    // each call's delay and does not answer in the order the calls were made (the 400 ms call is issued first). It is a
    // claim about the core only on a trial the machine delivered: the three issued together, and no call answered late
    // enough to pass the next one. A call is put behind the next-slower one only by arriving late by the gap to it, less
    // the 20 ms the issues may be apart: 130 ms for the 50 ms call, 180 ms for the 200 ms call (the 400 ms call is last:
    // late is still last). A hosted runner has answered the 50 ms call 150 ms late, which puts the 200 ms call first and
    // says nothing about the core, so that trial is repeated; the values and the lower bound on each call's time hold on
    // every trial.
    val measured = mutableListOf<String>()
    var delivered = false
    for (trial in 0 until 30) {
        val timed = runBlocking(Dispatchers.Default) {
            listOf(Triple(1, 400u, null), Triple(2, 50u, 130.0), Triple(3, 200u, 180.0))
                .map { (i, delayMs, lateLimitMs) ->
                    async {
                        val issuedNs = System.nanoTime()
                        val value = addLater(i, 0, delayMs)
                        TimedCall(i, delayMs.toLong(), lateLimitMs, value, issuedNs, System.nanoTime())
                    }
                }
                .awaitAll()
        }
        for (call in timed) {
            expectEq("the value of add_later(${call.id}, 0, ${call.delayMs})", call.id, call.value)
            check(call.tookMs >= call.delayMs - 5) { "add_later(${call.id}, 0, ${call.delayMs}) answered after only ${call.tookMs} ms" }
        }
        val issueSpreadMs = (timed.maxOf { it.issuedNs } - timed.minOf { it.issuedNs }) / 1e6
        measured += "trial $trial: issued within $issueSpreadMs ms, late by " + timed.joinToString(", ") { "${it.id}: ${it.lateMs} ms" }
        if (issueSpreadMs >= 20 || timed.any { it.lateLimitMs != null && it.lateMs >= it.lateLimitMs }) continue // the runner held a call up: its place in the order says nothing about the core
        expectEq("the order the three calls resolved in", listOf(2, 3, 1), timed.sortedBy { it.completedNs }.map { it.id })
        delivered = true
        break
    }
    check(delivered) {
        "the runner never delivered three concurrent calls within the margins (issued within 20 ms of each other, the 50 ms call " +
            "answered under 130 ms late, the 200 ms call under 180 ms late) in 30 trials: ${measured.joinToString("; ")}"
    }

    // 3. A method of an object, asynchronously.
    Probe.create().use { probe -> expectEq("Probe.wait(10)", 10u, runBlocking { probe.wait(10u) }) }

    // 4a. The continuation of a call calls again. The runtime must not resume it on the core's thread under the
    // core's lock, or the second call would wait for a lock its own thread holds.
    val twice = runBlocking(Dispatchers.Unconfined) { addLater(1, 1, 10u) to addLater(2, 2, 10u) }
    expectEq("two add_later calls, the second made where the first resumed", 2 to 4, twice)

    // 4b. A change observer calls the core again: a counter observed through the raw mirror, whose callback
    // (which runs on the main thread, never under the core's lock) increments it once more and starts an
    // async call, both from inside the callback.
    val counter = w.core.construct(UndraIds.Objects.Counter.TYPE_ID, UndraIds.Objects.Counter.NEW, ByteArray(0))
    val secondCall = CompletableFuture<Int>()
    val reachedTwo = CompletableFuture<Unit>()
    val incremented = AtomicBoolean(false)
    w.core.mirror.register(counter) { signalId, _, reader ->
        if (signalId == 0u) {
            val count = reader.readI32()
            if (count == 1 && incremented.compareAndSet(false, true)) {
                w.core.callSync(CallTarget.ObjectMethod(Handle(counter), UndraIds.Objects.Counter.INCREMENT), UndraIds.Objects.Counter.INCREMENT, ByteArray(0))
                secondCall.complete(runBlocking { addLater(3, 4, 10u) })
            }
            if (count == 2) reachedTwo.complete(Unit)
        }
    }
    w.core.observe(counter, RawStore.ALL_SIGNALS, true)
    w.core.callSync(CallTarget.ObjectMethod(Handle(counter), UndraIds.Objects.Counter.INCREMENT), UndraIds.Objects.Counter.INCREMENT, ByteArray(0))
    expectEq("an async call started inside a change observer", 7, secondCall.get(WAIT_MS, TimeUnit.MILLISECONDS))
    reachedTwo.get(WAIT_MS, TimeUnit.MILLISECONDS)
    w.core.release(counter)
}

/**
 * One of step 2's three concurrent calls: what it returned and the two instants (from `System.nanoTime`) that place it in
 * time. `lateLimitMs` is how late it may be answered on a trial that says something about the order (`null` for the slowest).
 */
private class TimedCall(val id: Int, val delayMs: Long, val lateLimitMs: Double?, val value: Int, val issuedNs: Long, val completedNs: Long) {
    /** How long the call took, from just before it was made to just after its result was back. */
    val tookMs: Double get() = (completedNs - issuedNs) / 1e6

    /** How long after its delay the result was back. */
    val lateMs: Double get() = tookMs - delayMs
}
