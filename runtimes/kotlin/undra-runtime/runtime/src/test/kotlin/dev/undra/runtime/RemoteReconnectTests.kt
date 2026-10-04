package dev.undra.runtime

import dev.undra.runtime.support.HASH
import dev.undra.runtime.support.NO_BYTES
import dev.undra.runtime.support.WsTestServer
import dev.undra.runtime.support.changeSet
import dev.undra.runtime.support.eventually
import dev.undra.runtime.support.full
import dev.undra.runtime.testing.Suite
import dev.undra.runtime.testing.assertEq
import dev.undra.runtime.testing.assertThrows
import dev.undra.runtime.testing.assertTrue
import dev.undra.runtime.wire.Codecs
import dev.undra.runtime.wire.Envelope
import dev.undra.runtime.wire.Handle
import dev.undra.runtime.wire.Payloads
import dev.undra.runtime.wire.Payloads.CallTarget
import dev.undra.runtime.wire.Payloads.ReplyStatus
import dev.undra.runtime.wire.encodeToByteArray
import java.net.URI
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Test

private val METHOD = 0x41u
private val TARGET = CallTarget.ObjectMethod(Handle(0x100000002L), METHOD)
private const val STORE = 0x100000002L

/** A sleeper that records each wait instead of waiting; while [hold] is set the reconnect loop stays in its sleep. */
private class RecordingSleeper : Sleeper {
    val waits = CopyOnWriteArrayList<Long>()

    @Volatile var hold: CountDownLatch? = null

    override fun sleep(millis: Long) {
        waits.add(millis)
        hold?.await()
    }
}

/**
 * A server that answers every connection's Hello with [hashFor] of the connection's number (1 for the first),
 * and everything else as [configure] says.
 */
private fun serve(
    server: WsTestServer,
    hashFor: (Int) -> ULong = { HASH },
    configure: (WsTestServer.Conn, Int) -> Unit = { _, _ -> },
) {
    val count = AtomicInteger()
    server.onConnect = { conn ->
        val n = count.incrementAndGet()
        conn.onMessage = { bytes ->
            val env = Envelope.decode(bytes)
            if (env.kind == Envelope.Kind.HELLO) conn.sendHello(hashFor(n))
        }
        configure(conn, n)
    }
}

/** A deadline that only detects a hang: no step waited for under it depends on a timer of its own. */
private const val HANG_MS = 60_000L

private fun policy(max: Int = Int.MAX_VALUE) = ReconnectPolicy(random = { 0.0 }, maxAttempts = max)

private class Loaded(val core: UndraCore, val states: CopyOnWriteArrayList<ConnectionState>) : AutoCloseable {
    override fun close() = core.close()
}

private fun load(
    url: String,
    sleeper: Sleeper,
    reconnect: ReconnectPolicy? = policy(),
    timeout: kotlin.time.Duration = 5.seconds,
    pingAfterMillis: Long = 10_000L,
): Loaded {
    val states = CopyOnWriteArrayList<ConnectionState>()
    val core = UndraCore.attach(
        RemoteTransport(URI(url), timeout, reconnect, session = "tok-test", sleeper = sleeper, pingAfterMillis = pingAfterMillis),
        LoadOptions(
            mode = Mode.REMOTE,
            remoteUrl = url,
            expectedSchemaHash = HASH,
            defaultAdapters = false,
            remoteTimeout = timeout,
            onConnectionChange = { states.add(it) },
        ),
        makeShared = false,
    )
    return Loaded(core, states)
}

private fun short(state: ConnectionState): String = when (state) {
    ConnectionState.Connecting -> "connecting"
    ConnectionState.Connected -> "connected"
    is ConnectionState.Reconnecting -> "reconnecting ${state.attempt}"
    is ConnectionState.Closed -> "closed:${state.reason}"
}

/** The remote transport reconnecting over real sockets: the backoff, the session in the URL, what survives, what is final. */
class RemoteReconnectTests : Suite() {
    init {
        case("a dropped connection is reconnected, the stores are observed again and the mirror resyncs") {
            WsTestServer().use { server ->
                var value = 5u
                serve(server) { conn, _ ->
                    val before = conn.onMessage
                    conn.onMessage = { bytes ->
                        before(bytes)
                        val env = Envelope.decode(bytes)
                        if (env.kind == Envelope.Kind.OBSERVE) {
                            val observe = Payloads.Observe.decode(env.payload)
                            conn.send(Envelope.Kind.CHANGE_SET, changeSet(1uL, full(observe.handle.raw, 0u, Codecs.u32.encodeToByteArray(value))))
                        }
                    }
                }
                load(server.url, RecordingSleeper()).use { loaded ->
                    val seen = CopyOnWriteArrayList<UInt>()
                    loaded.core.mirror.register(STORE) { _, _, r -> seen.add(Codecs.u32.decode(r)) }
                    loaded.core.observe(STORE, UInt.MAX_VALUE, true)
                    eventually("the first value") { seen == listOf(5u) }
                    assertEq(listOf("connecting", "connected"), loaded.states.map(::short))

                    value = 50u
                    server.connections.first().drop()
                    eventually("reconnected") { loaded.states.map(::short).lastOrNull() == "connected" && loaded.states.size > 2 }
                    eventually("the mirror converged") { seen == listOf(5u, 50u) }
                    assertEq(listOf("connecting", "connected", "reconnecting 1", "connected"), loaded.states.map(::short))
                    val second = server.connections[1]
                    val observe = Payloads.Observe.decode(second.awaitEnvelope(Envelope.Kind.OBSERVE).payload)
                    assertEq(Payloads.Observe(Handle(STORE), UInt.MAX_VALUE, true), observe)
                }
            }
        }

        case("a lost connection keeps the callbacks the core holds for the session's return, and closing the core drops them") {
            WsTestServer().use { server ->
                serve(server)
                val hold = CountDownLatch(1)
                load(server.url, RecordingSleeper().also { it.hold = hold }).use { loaded ->
                    val listener = Any()
                    val instance = loaded.core.callbacks.lend(listener)
                    assertEq(1, loaded.core.callbacks.count(listener))
                    server.connections.first().drop()
                    eventually("the connection is lost") { loaded.states.map(::short).lastOrNull() == "reconnecting 1" }
                    // The server keeps the session's objects, and the proxies of the instances lent, for its return
                    // (ADR-051): what the core calls over the next connection must find them.
                    assertEq(1, loaded.core.callbacks.liveCount, "kept while reconnecting")
                    assertEq(instance, loaded.core.callbacks.instanceOf(listener))
                    hold.countDown()
                    eventually("reconnected") { loaded.states.map(::short).lastOrNull() == "connected" && loaded.states.size > 2 }
                    assertEq(1, loaded.core.callbacks.count(listener), "and still there once reconnected")
                    loaded.core.close()
                    assertEq(0, loaded.core.callbacks.liveCount, "dropped with the core")
                }
            }
        }

        case("every connection carries the same session token, and asks to resume only when the core holds objects") {
            WsTestServer().use { server ->
                serve(server) { conn, n ->
                    val before = conn.onMessage
                    conn.onMessage = { bytes ->
                        before(bytes)
                        val env = Envelope.decode(bytes)
                        if (env.kind == Envelope.Kind.CALL) {
                            val call = Payloads.Call.decode(env.payload)
                            conn.send(Envelope.Kind.REPLY, Payloads.Reply(call.callId, ReplyStatus.OK, Codecs.handle.encodeToByteArray(0x200000003L)).toByteArray())
                        }
                    }
                }
                load(server.url, RecordingSleeper()).use { loaded ->
                    val first = server.awaitConnection()
                    assertEq("tok-test", first.query["undra_session"])
                    assertEq(null, first.query["undra_resume"])

                    server.connections.first().drop()
                    eventually("reconnected") { server.connections.size == 2 }
                    val second = server.connections[1]
                    assertEq("tok-test", second.query["undra_session"])
                    assertEq(null, second.query["undra_resume"], "it constructed nothing: nothing to resume")
                    eventually("connected") { loaded.states.last() == ConnectionState.Connected }

                    loaded.core.construct(1u, 2u, NO_BYTES)
                    second.drop()
                    eventually("reconnected again") { server.connections.size == 3 }
                    assertEq("tok-test", server.connections[2].query["undra_session"])
                    assertEq("1", server.connections[2].query["undra_resume"])
                }
            }
        }

        case("the backoff doubles from 250 ms to 5 s while the server is down, then the first attempt that finds it wins") {
            val probe = WsTestServer()
            val port = probe.port
            serve(probe)
            val sleeper = RecordingSleeper()
            load(probe.url, sleeper).use { loaded ->
                probe.close() // the server goes away: every connect is refused now
                eventually("seven waits") { sleeper.waits.size >= 7 }
                assertEq(listOf(250L, 500L, 1000L, 2000L, 4000L, 5000L, 5000L), sleeper.waits.take(7))
                assertTrue(loaded.states.map(::short).containsAll(listOf("reconnecting 1", "reconnecting 2", "reconnecting 7")), loaded.states.map(::short).toString())
                WsTestServer(port).use { back ->
                    serve(back)
                    eventually("connected again") { loaded.states.last() == ConnectionState.Connected }
                    assertTrue(back.connections.isNotEmpty())
                }
            }
        }

        case("a call in flight when the connection drops fails with UndraTransportException (CONNECTION_LOST), Unavailable once mapped; so does every call until it is back, and a command is not reported") {
            WsTestServer().use { server ->
                serve(server)
                val sleeper = RecordingSleeper().also { it.hold = CountDownLatch(1) }
                val reports = CopyOnWriteArrayList<UndraUnhandledError>()
                val states = CopyOnWriteArrayList<ConnectionState>()
                val core = UndraCore.attach(
                    RemoteTransport(URI(server.url), 5.seconds, policy(), session = "tok-test", sleeper = sleeper),
                    LoadOptions(
                        mode = Mode.REMOTE,
                        remoteUrl = server.url,
                        expectedSchemaHash = HASH,
                        defaultAdapters = false,
                        onConnectionChange = { states.add(it) },
                        onError = { reports.add(it) },
                    ),
                    makeShared = false,
                )
                core.use {
                    val inFlight = CopyOnWriteArrayList<Throwable>()
                    val caller = Thread {
                        try {
                            runBlocking { core.call(TARGET, METHOD, NO_BYTES) }
                        } catch (e: Throwable) {
                            inFlight.add(e)
                        }
                    }.also { it.isDaemon = true; it.start() }
                    server.awaitConnection().awaitEnvelope(Envelope.Kind.CALL)
                    server.connections.first().drop()
                    caller.join(10_000)
                    val failure = inFlight.single() as UndraTransportException
                    assertEq(UndraTransportException.Reason.CONNECTION_LOST, failure.reason)
                    assertTrue(UndraCallError.mapped(failure) is UndraCallError.Unavailable)
                    eventually("reconnecting") { states.lastOrNull() is ConnectionState.Reconnecting }

                    // While it is down: the same type at once, and a command that fails with it is only logged.
                    val tapped = assertThrows<UndraTransportException> { core.callSync(TARGET, METHOD, NO_BYTES) }
                    assertEq(UndraTransportException.Reason.CONNECTION_LOST, tapped.reason)
                    core.report(tapped, "Todos.toggle")
                    assertEq(emptyList<UndraUnhandledError>(), reports.toList(), "the connection state already says it")
                    sleeper.hold?.countDown()
                    eventually("connected again") { states.lastOrNull() == ConnectionState.Connected }
                }
            }
        }

        case("the policy gives up after maxAttempts and closes the core as failed") {
            // The server stays up and refuses every reconnect as soon as it is asked (a 503 to the upgrade), so each attempt
            // ends when it is answered. (It used to close the server: an attempt on a closed port is refused at once only
            // while no other socket holds the port, which came from the ephemeral range; one that is not answered waits out
            // its 5 s, and two of those overran the 10 s wait.) Each event is waited for on its own, with a deadline that
            // only detects a hang.
            WsTestServer().use { probe ->
                serve(probe)
                val sleeper = RecordingSleeper()
                load(probe.url, sleeper, reconnect = policy(max = 3)).use { loaded ->
                    probe.rejectWith = "HTTP/1.1 503 Service Unavailable"
                    probe.connections.first().drop()
                    for (n in 1..3) {
                        eventually("reconnect attempt $n", HANG_MS) { "reconnecting $n" in loaded.states.map(::short) }
                    }
                    eventually("the core is closed", HANG_MS) { loaded.states.last() is ConnectionState.Closed }
                    assertEq(ClosedReason.FAILED, (loaded.states.last() as ConnectionState.Closed).reason)
                    assertEq(3, probe.rejected.get(), "the reconnects the server refused")
                    // A core that was lost for good answers every call as unreachable, not as one the app closed.
                    val afterwards = assertThrows<UndraTransportException> { loaded.core.callSync(TARGET, METHOD, NO_BYTES) }
                    assertEq(UndraTransportException.Reason.CONNECTION_LOST, afterwards.reason)
                    assertEq(listOf(250L, 500L, 1000L), sleeper.waits.toList())
                    assertEq(listOf("connecting", "connected", "reconnecting 1", "reconnecting 2", "reconnecting 3", "closed:FAILED"), loaded.states.map(::short))
                }
            }
        }

        case("a schema change on reconnect is the mismatch error, once, with no loop") {
            WsTestServer().use { server ->
                serve(server, hashFor = { n -> if (n == 1) HASH else 0x77uL })
                val sleeper = RecordingSleeper()
                load(server.url, sleeper).use { loaded ->
                    server.awaitConnection().drop()
                    eventually("closed") { loaded.states.last() is ConnectionState.Closed }
                    val closed = loaded.states.last() as ConnectionState.Closed
                    assertEq(ClosedReason.SCHEMA_MISMATCH, closed.reason)
                    val mismatch = closed.cause as UndraSchemaMismatchException
                    assertEq(HASH, mismatch.expected)
                    assertEq(0x77uL, mismatch.got)
                    Thread.sleep(300)
                    assertEq(1, loaded.states.count { it is ConnectionState.Closed }, "reported once")
                    assertEq(1, sleeper.waits.size, "one attempt, not a loop")
                    assertEq(2, server.connections.size)
                    val e = assertThrows<UndraTransportException> { loaded.core.callSync(TARGET, METHOD, NO_BYTES) }
                    assertEq(UndraTransportException.Reason.CONNECTION_LOST, e.reason)
                    assertTrue(UndraCallError.mapped(e) is UndraCallError.Unavailable)
                }
            }
        }

        case("a close with code 4001 right behind the Hello is a lost session, final and reported once") {
            WsTestServer().use { server ->
                serve(server) { conn, n -> if (n >= 2) conn.sendClose(4001, "session lost: no session tok-test") }
                load(server.url, RecordingSleeper()).use { loaded ->
                    server.awaitConnection().drop()
                    eventually("closed") { loaded.states.last() is ConnectionState.Closed }
                    val closed = loaded.states.last() as ConnectionState.Closed
                    assertEq(ClosedReason.SESSION_LOST, closed.reason)
                    assertTrue((closed.cause as UndraSessionLostException).message!!.contains("session lost"))
                    // The close usually beats the announcement of the connection; if it does not, the core was told
                    // `connected` for an instant before `closed`. Never a loop either way.
                    assertTrue(loaded.states.count { it == ConnectionState.Connected } <= 2, loaded.states.map(::short).toString())
                    assertEq(1, loaded.states.count { it is ConnectionState.Closed })
                }
            }
        }

        case("a close from the server (a rebuild: 1001) is reconnected like a drop") {
            WsTestServer().use { server ->
                serve(server)
                load(server.url, RecordingSleeper()).use { loaded ->
                    server.awaitConnection().sendClose(1001)
                    eventually("reconnected") { server.connections.size == 2 && loaded.states.last() == ConnectionState.Connected }
                    assertTrue(loaded.states.map(::short).contains("reconnecting 1"))
                }
            }
        }

        case("a text frame is a protocol error: final, not retried") {
            WsTestServer().use { server ->
                serve(server)
                val sleeper = RecordingSleeper()
                load(server.url, sleeper).use { loaded ->
                    server.awaitConnection().sendText("hello")
                    eventually("closed") { loaded.states.last() is ConnectionState.Closed }
                    assertEq(ClosedReason.FAILED, (loaded.states.last() as ConnectionState.Closed).reason)
                    assertTrue(loaded.states.last().let { (it as ConnectionState.Closed).cause!!.message!!.contains("protocol error") })
                    assertEq(0, sleeper.waits.size)
                }
            }
        }

        case("a server that breaks RFC 6455 (a masked frame) is a protocol error: final, not retried") {
            WsTestServer().use { server ->
                serve(server)
                val sleeper = RecordingSleeper()
                load(server.url, sleeper).use { loaded ->
                    server.awaitConnection().sendRaw(byteArrayOf(0x82.toByte(), 0x81.toByte(), 1, 2, 3, 4, 5))
                    eventually("closed") { loaded.states.last() is ConnectionState.Closed }
                    val closed = loaded.states.last() as ConnectionState.Closed
                    assertEq(ClosedReason.FAILED, closed.reason)
                    val why = closed.cause!!.message!!
                    assertTrue(why.contains("masked frame"), why)
                    assertEq(0, sleeper.waits.size, "no reconnect loop against a broken server")
                    assertEq(1, server.connections.size)
                }
            }
        }

        case("a server that vanished without a FIN is noticed by the client's own ping, and reconnected") {
            WsTestServer().use { server ->
                serve(server)
                load(server.url, RecordingSleeper(), pingAfterMillis = 200).use { loaded ->
                    server.awaitConnection().answerPings = false
                    eventually("the loss is noticed", timeoutMs = 5_000) { loaded.states.map(::short).contains("reconnecting 1") }
                    eventually("and healed") { server.connections.size == 2 && loaded.states.last() == ConnectionState.Connected }
                }
            }
        }

        case("close() during the backoff ends the reconnecting for good") {
            WsTestServer().use { server ->
                serve(server)
                val sleeper = RecordingSleeper().also { it.hold = CountDownLatch(1) }
                val loaded = load(server.url, sleeper)
                server.awaitConnection().drop()
                eventually("it is waiting to retry") { sleeper.waits.size == 1 }
                loaded.core.close()
                Thread.sleep(300)
                sleeper.hold!!.countDown()
                Thread.sleep(300)
                assertEq(1, server.connections.size, "no attempt after close")
                assertEq(ClosedReason.REQUESTED, (loaded.states.last() as ConnectionState.Closed).reason)
                assertEq(1, loaded.states.count { it is ConnectionState.Closed })
            }
        }

        case("with no reconnect policy a drop closes the core as it always did") {
            WsTestServer().use { server ->
                serve(server)
                load(server.url, RecordingSleeper(), reconnect = null).use { loaded ->
                    server.awaitConnection().drop()
                    eventually("closed") { loaded.states.last() is ConnectionState.Closed }
                    assertEq(ClosedReason.FAILED, (loaded.states.last() as ConnectionState.Closed).reason)
                    assertEq(1, server.connections.size)
                }
            }
        }

        case("an initial connection that fails is still an exception from load, not a retry") {
            val port = java.net.ServerSocket(0).use { it.localPort }
            val sleeper = RecordingSleeper()
            val e = assertThrows<UndraTransportException> { load("ws://127.0.0.1:$port", sleeper, timeout = 2.seconds) }
            assertEq(UndraTransportException.Reason.CONNECTION_LOST, e.reason)
            assertTrue(e.message!!.contains("ws://127.0.0.1:$port"), e.message!!)
            assertEq(0, sleeper.waits.size)
        }

        case("load works from a thread that must not touch the network: no I/O on the calling thread") {
            WsTestServer().use { server ->
                serve(server)
                val callers = CopyOnWriteArrayList<String>()
                val threadNames = CopyOnWriteArrayList<String>()
                load(server.url, RecordingSleeper()).use { loaded ->
                    callers.add(Thread.currentThread().name)
                    runBlocking { loaded.core.observe(STORE, UInt.MAX_VALUE, true) }
                    server.awaitConnection().awaitEnvelope(Envelope.Kind.OBSERVE)
                    for (t in Thread.getAllStackTraces().keys) threadNames.add(t.name)
                    assertTrue(threadNames.contains("undra-ws-reader") && threadNames.contains("undra-ws-writer"), threadNames.toString())
                }
            }
        }
    }

    @Test
    fun allCases() = assertPassed()
}
