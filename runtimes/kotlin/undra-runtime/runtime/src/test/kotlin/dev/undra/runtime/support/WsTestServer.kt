package dev.undra.runtime.support

import dev.undra.runtime.wire.Envelope
import dev.undra.runtime.wire.Payloads
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.IOException
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest
import java.util.Base64
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * A small RFC 6455 WebSocket server on loopback (handshake, masked client frames, fragmentation, ping/pong,
 * close), standing in for `undra dev` so that the real [dev.undra.runtime.RemoteTransport] can be tested over
 * a real socket. It speaks whatever bytes a test hands it, so tests can also misbehave.
 */
class WsTestServer(port: Int = 0) : AutoCloseable {
    private val server = ServerSocket(port, 10, InetAddress.getLoopbackAddress())
    val port: Int get() = server.localPort
    val url: String get() = "ws://127.0.0.1:${server.localPort}"
    val connections = CopyOnWriteArrayList<Conn>()

    /** Called on the connection's reader thread once the handshake is done, before any frame is read. */
    @Volatile var onConnect: (Conn) -> Unit = {}

    /** When set, the upgrade is answered with this status line instead of `101` (for example `HTTP/1.1 403 Forbidden`). */
    @Volatile var rejectWith: String? = null

    /** How many upgrade requests were answered with [rejectWith]. */
    val rejected = AtomicInteger()

    /** When `true` the `Sec-WebSocket-Accept` of the upgrade response is wrong. */
    @Volatile var corruptAccept = false

    @Volatile private var closed = false

    init {
        Thread {
            while (!closed) {
                val socket = try {
                    server.accept()
                } catch (e: IOException) {
                    break
                }
                Thread { Conn(socket).run() }.also { it.isDaemon = true }.start()
            }
        }.also { it.isDaemon = true; it.name = "ws-test-accept" }.start()
    }

    inner class Conn(private val socket: Socket) {
        /** Every complete binary message the client sent, raw. */
        val messages = LinkedBlockingQueue<ByteArray>()
        val envelopes = CopyOnWriteArrayList<Envelope>()
        val closeCodes = LinkedBlockingQueue<Int>()
        @Volatile var onMessage: (ByteArray) -> Unit = {}
        @Volatile var sawTextFrame = false

        /** The request line of the upgrade request (`GET /?undra_session=... HTTP/1.1`). */
        @Volatile var requestLine: String = ""

        /** The query of the upgrade request's URL, as a map. */
        val query: Map<String, String>
            get() = requestLine.split(' ').getOrNull(1)?.substringAfter('?', "")?.split('&')?.filter { it.isNotEmpty() }
                ?.associate { it.substringBefore('=') to it.substringAfter('=', "") } ?: emptyMap()

        /** When `false` the connection ignores pings, like a peer that is gone but whose socket is still open. */
        @Volatile var answerPings = true

        /** The payloads of the pongs the client sent. */
        val pongs = LinkedBlockingQueue<ByteArray>()

        /** How many pings the client sent. */
        val pingsReceived = AtomicInteger()
        private val out: OutputStream = socket.getOutputStream()
        private val writeLock = Any()
        private val seq = AtomicInteger()

        internal fun run() {
            try {
                handshake()
                connections.add(this)
                onConnect(this)
                readFrames()
            } catch (e: IOException) {
                // the client went away
            } finally {
                try {
                    socket.close()
                } catch (e: IOException) {
                    // already closed
                }
            }
        }

        private fun handshake() {
            val input = socket.getInputStream()
            val header = StringBuilder()
            while (!header.endsWith("\r\n\r\n")) {
                val b = input.read()
                if (b < 0) throw IOException("closed during handshake")
                header.append(b.toChar())
            }
            requestLine = header.lines().first()
            val key = header.lines().first { it.startsWith("Sec-WebSocket-Key:", ignoreCase = true) }.substringAfter(':').trim()
            val accept = Base64.getEncoder().encodeToString(
                MessageDigest.getInstance("SHA-1").digest((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").toByteArray()),
            ).let { if (corruptAccept) "AAAA$it" else it }
            synchronized(writeLock) {
                val status = rejectWith
                if (status != null) {
                    out.write("$status\r\nContent-Length: 0\r\n\r\n".toByteArray())
                    out.flush()
                    rejected.incrementAndGet()
                    throw IOException("rejected the upgrade")
                }
                out.write(
                    ("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: $accept\r\n\r\n")
                        .toByteArray(),
                )
                out.flush()
            }
        }

        private fun readFrames() {
            val input = DataInputStream(socket.getInputStream())
            val message = ByteArrayOutputStream()
            while (true) {
                val b0 = input.read()
                if (b0 < 0) return
                val fin = b0 and 0x80 != 0
                val opcode = b0 and 0x0F
                val b1 = input.readUnsignedByte()
                val masked = b1 and 0x80 != 0
                var len = (b1 and 0x7F).toLong()
                if (len == 126L) len = input.readUnsignedShort().toLong() else if (len == 127L) len = input.readLong()
                val mask = ByteArray(4)
                if (masked) input.readFully(mask)
                val payload = ByteArray(len.toInt())
                input.readFully(payload)
                if (masked) for (i in payload.indices) payload[i] = (payload[i].toInt() xor mask[i % 4].toInt()).toByte()
                when (opcode) {
                    0, 2 -> {
                        message.write(payload)
                        if (fin) {
                            val bytes = message.toByteArray()
                            message.reset()
                            messages.add(bytes)
                            try {
                                envelopes.add(Envelope.decode(bytes))
                            } catch (e: dev.undra.runtime.wire.WireException) {
                                // a test that sends garbage from the client side inspects `messages`
                            }
                            onMessage(bytes)
                        }
                    }
                    1 -> sawTextFrame = true
                    8 -> {
                        closeCodes.add(if (payload.size >= 2) ((payload[0].toInt() and 0xFF) shl 8) or (payload[1].toInt() and 0xFF) else 1005)
                        frame(8, payload)
                        return
                    }
                    9 -> {
                        pingsReceived.incrementAndGet()
                        if (answerPings) frame(10, payload)
                    }
                    10 -> pongs.add(payload)
                    else -> Unit
                }
            }
        }

        private fun frame(opcode: Int, payload: ByteArray, fin: Boolean = true) {
            synchronized(writeLock) {
                out.write((if (fin) 0x80 else 0) or opcode)
                when {
                    payload.size < 126 -> out.write(payload.size)
                    payload.size < 65536 -> {
                        out.write(126)
                        out.write(payload.size shr 8)
                        out.write(payload.size)
                    }
                    else -> {
                        out.write(127)
                        for (shift in 56 downTo 0 step 8) out.write((payload.size.toLong() shr shift).toInt())
                    }
                }
                out.write(payload)
                out.flush()
            }
        }

        /** Sends [bytes] as one binary message. */
        fun sendBinary(bytes: ByteArray) = frame(2, bytes)

        /** Sends [bytes] as a binary message split into frames of at most [chunk] bytes. */
        fun sendFragmented(bytes: ByteArray, chunk: Int) {
            var offset = 0
            var first = true
            while (offset < bytes.size || first) {
                val end = minOf(bytes.size, offset + chunk)
                frame(if (first) 2 else 0, bytes.copyOfRange(offset, end), fin = end >= bytes.size)
                first = false
                offset = end
                if (end >= bytes.size) break
            }
        }

        fun sendText(text: String) = frame(1, text.toByteArray())

        /** Sends a ping with [payload]. */
        fun sendPing(payload: ByteArray = ByteArray(0)) = frame(9, payload)

        /** Sends a close frame with [code] and [reason]. */
        fun sendClose(code: Int, reason: String) = frame(8, byteArrayOf((code shr 8).toByte(), code.toByte()) + reason.toByteArray())

        /** Closes the WebSocket with [code]. */
        fun sendClose(code: Int) = frame(8, byteArrayOf((code shr 8).toByte(), code.toByte()))

        /** Writes [bytes] as they are, framing or not (a server that breaks RFC 6455). */
        fun sendRaw(bytes: ByteArray) {
            synchronized(writeLock) {
                out.write(bytes)
                out.flush()
            }
        }

        /** Drops the TCP connection without a close handshake. */
        fun drop() = socket.close()

        /** Sends an Undra envelope with this connection's next sequence number. */
        fun send(kind: Envelope.Kind, payload: ByteArray, schema: ULong = HASH) =
            sendBinary(Envelope.encode(kind, seq.getAndIncrement().toUInt(), schema, payload))

        fun sendHello(schema: ULong = HASH) =
            send(Envelope.Kind.HELLO, Payloads.Hello("test-core", schema, "rust", "dev").toByteArray(), schema)

        /** Waits for the next envelope of [kind] from the client (in arrival order among all envelopes). */
        fun awaitEnvelope(kind: Envelope.Kind, timeoutMs: Long = 10_000): Envelope {
            val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs)
            while (System.nanoTime() < deadline) {
                envelopes.firstOrNull { it.kind == kind && !consumed.contains(it) }?.let {
                    consumed.add(it)
                    return it
                }
                Thread.sleep(2)
            }
            throw AssertionError("no $kind envelope arrived within ${timeoutMs}ms; got ${envelopes.map { it.kind }}")
        }

        private val consumed = CopyOnWriteArrayList<Envelope>()
    }

    /** Waits for the first client connection. */
    fun awaitConnection(timeoutMs: Long = 10_000): Conn {
        eventually("a client connects", timeoutMs) { connections.isNotEmpty() }
        return connections.first()
    }

    override fun close() {
        closed = true
        server.close()
        for (c in connections) c.drop()
    }
}
