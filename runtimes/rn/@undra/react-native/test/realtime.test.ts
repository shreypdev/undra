import { fileURLToPath, pathToFileURL } from "node:url";
import { afterAll, afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import {
  HeaderCodec,
  PortIds,
  PortStatus,
  SseError,
  type SseEvent,
  SseEventCodec,
  UndraPortError,
  UndraWriter,
  WsError,
  WsErrorCodec,
  type WsMessage,
  WsMessageCodec,
  WsOpenedCodec,
  codecs,
  decodePortReply,
  decodeValue,
} from "@undra/runtime";
import { OptInPortIds, type PlatformWebSocket, type WebSocketAdapter } from "@undra/runtime/realtime";
import {
  type NativeCoreEntry,
  type ReactNativeWebSocketConstructor,
  loadNative,
  nativeDefaultPorts,
  nativePlatformDefaults,
  reactNativeAdapters,
  reactNativeSse,
  reactNativeWebSocket,
  realtimePorts,
} from "../src/index.js";
import { RecordKind } from "../src/native.js";
import { setTurboModule } from "./support/react-native-stub.js";
import { FakeNative, le } from "./support/fake-native.js";
import { NodeXhr } from "./support/node-xhr.js";
import { eventually, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

/*
 * The opt-in ports of ADR-047 and ADR-048 in @undra/react-native: the `WebSocket` and `Sse` defaults (React Native's
 * `WebSocket` with headers as its third constructor argument; `Sse` over `XMLHttpRequest` progress events, or a
 * streaming `fetch`) against the shared realtime server of the contract tests, their registration by `loadNative`,
 * and `Db` as a native default (its C++ is tested by cpp/test/run.sh, on the devices by scripts/rn-device-checks.sh).
 */

interface Connection {
  readonly path: string;
  readonly headers: Record<string, string | undefined>;
  readonly protocols: string[];
  readonly clientClosed: boolean;
  readonly closeCode: number | null;
  readonly written: number;
}

interface RealtimeServer {
  readonly url: string;
  readonly wsUrl: string;
  stats(): { readonly connections: Connection[] };
  close(): Promise<void>;
}

let server: RealtimeServer;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../../../contract-tests/servers/realtime-server.mjs", import.meta.url));
  const module = (await import(pathToFileURL(path).href)) as { startRealtimeServer(): Promise<RealtimeServer> };
  server = await module.startRealtimeServer();
});

afterAll(async () => {
  await server.close();
});

const g = globalThis as { __undraNative?: Record<string, FakeNative> };

afterEach(() => {
  delete g.__undraNative;
  setTurboModule("UndraNative", undefined);
});

function lastConnection(path: string): Connection {
  const found = server.stats().connections.filter((c) => c.path === path);
  const last = found[found.length - 1];
  if (last === undefined) throw new Error(`no connection to ${path}`);
  return last;
}

/** Reads `iterable` to its end: the items, and what it threw (undefined when it finished). */
async function drain<T>(iterable: AsyncIterable<T>): Promise<{ items: T[]; end: unknown }> {
  const items: T[] = [];
  try {
    for await (const item of iterable) items.push(item);
  } catch (error) {
    return { items, end: error };
  }
  return { items, end: undefined };
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected a rejection");
}

const text = (value: string): WsMessage => ({ kind: "text", value });

// ----- WebSocket ----------------------------------------------------------------------------------

/**
 * React Native's `WebSocket` constructor form, `(url, protocols, { headers })`, played by Node's global `WebSocket`
 * (`new WebSocket(url, { protocols, headers })`); every construction is recorded.
 */
const constructed: Array<{ url: string; protocols: unknown; init: unknown }> = [];
const RnWebSocket = function (url: string, protocols?: string | string[], init?: { readonly headers: Record<string, string> }) {
  constructed.push({ url, protocols, init });
  const NodeWebSocket = (globalThis as unknown as { WebSocket: new (url: string, init: object) => PlatformWebSocket }).WebSocket;
  const offered = typeof protocols === "string" ? [protocols] : (protocols ?? []);
  return new NodeWebSocket(url, init === undefined ? { protocols: offered } : { protocols: offered, headers: init.headers });
} as unknown as ReactNativeWebSocketConstructor;

const hasWebSocket = typeof (globalThis as { WebSocket?: unknown }).WebSocket === "function";
if (!hasWebSocket) console.log("# skipped the WebSocket checks: this Node has no global WebSocket (Node 22+)");

describe.skipIf(!hasWebSocket)("reactNativeWebSocket against the realtime server", () => {
  const adapter = reactNativeWebSocket({ WebSocket: RnWebSocket });

  test("echoes text and binary, with the subprotocol the server chose", async () => {
    const conn = await adapter.connect(`${server.wsUrl}/ws/echo`, ["v1", "v2"], []);
    expect(conn.protocol).toBe("v1");
    const inbound = conn.messages()[Symbol.asyncIterator]();
    await conn.send(text("hello"));
    await conn.send({ kind: "binary", value: new Uint8Array([1, 2, 0, 255]) });
    expect((await inbound.next()).value).toEqual(text("hello"));
    const binary = (await inbound.next()).value as WsMessage;
    expect(binary.kind).toBe("binary");
    expect([...(binary.value as Uint8Array)]).toEqual([1, 2, 0, 255]);
    await conn.close(1000, "done");
    expect((await inbound.next()).done).toBe(true);
    await eventually(() => lastConnection("/ws/echo").closeCode === 1000);
  });

  test("headers go to React Native's constructor as its third argument, { headers }", async () => {
    const conn = await adapter.connect(`${server.wsUrl}/ws/headers`, [], [{ name: "x-undra-token", value: "t1" }]);
    // Under Node `browserWebSocket` would pick Node's form; the React Native adapter always hands over its own.
    expect(constructed[constructed.length - 1]).toEqual({ url: `${server.wsUrl}/ws/headers`, protocols: [], init: { headers: { "x-undra-token": "t1" } } });
    const first = (await conn.messages()[Symbol.asyncIterator]().next()).value as WsMessage;
    expect(JSON.parse(first.value as string)["x-undra-token"]).toBe("t1");
    await conn.close(1000, "");
  });

  test("the peer's close frame ends the stream with Closed { code, reason }; a drop is Network", async () => {
    const closing = await adapter.connect(`${server.wsUrl}/ws/close?code=4000&reason=bye`, [], []);
    const closed = await drain(closing.messages());
    expect(closed.items).toEqual([text("hello")]);
    expect(closed.end).toBeInstanceOf(WsError.Closed);
    expect(closed.end).toMatchObject({ code: 4000, reason: "bye" });
    const dropping = await adapter.connect(`${server.wsUrl}/ws/drop`, [], []);
    const dropped = await drain(dropping.messages());
    expect(dropped.items).toEqual([text("hello")]);
    expect(dropped.end).toBeInstanceOf(WsError.Network);
  });

  test("a refused upgrade is Refused (React Native does not report the status)", async () => {
    const refused = await rejection(adapter.connect(`${server.wsUrl}/ws/deny?status=401`, [], []));
    expect(refused).toBeInstanceOf(WsError.Refused);
    expect((refused as InstanceType<typeof WsError.Refused>).status).toBeNull();
  });

  test("a stalled reader: React Native cannot pause, so past the limit the stream ends with Closed(1008)", async () => {
    const small = reactNativeWebSocket({ WebSocket: RnWebSocket, maxBufferedMessages: 50 });
    const conn = await small.connect(`${server.wsUrl}/ws/flood?n=200&size=16`, [], []);
    // The flood has arrived when this side closed the connection: the adapter gave up at the 51st message (Node's
    // `WebSocket` dispatches the messages before it answers the server's close frame, so the close is never earlier than
    // the give-up), or, were the limit not honoured, after the whole flood. Reading before then would keep up with it.
    await eventually(() => lastConnection("/ws/flood").closeCode !== null, "the client's close of /ws/flood");
    const flooded = await drain(conn.messages());
    expect(flooded.items.length).toBe(50);
    expect(flooded.end).toBeInstanceOf(WsError.Closed);
    expect(flooded.end).toMatchObject({ code: 1008, reason: "the core did not keep up" });
  });

  test("a stalled reader past the byte limit (16 MiB by default) also ends with Closed(1008)", async () => {
    const small = reactNativeWebSocket({ WebSocket: RnWebSocket, maxBufferedBytes: 64 * 1024 });
    const conn = await small.connect(`${server.wsUrl}/ws/flood?n=200&size=4096`, [], []);
    // The connection was given up: closed from this side (with 1008 on React Native; Node's `WebSocket`, which plays
    // it here, refuses 1008 from a script and sends a close frame without a code). As above, that is when the flood has arrived.
    await eventually(() => lastConnection("/ws/flood").closeCode !== null, "the client's close of /ws/flood");
    const flooded = await drain(conn.messages());
    // 16 messages of 4 KiB fit in 64 KiB; the 17th is past it.
    expect(flooded.items.length).toBe(16);
    expect(flooded.end).toBeInstanceOf(WsError.Closed);
    expect(flooded.end).toMatchObject({ code: 1008, reason: "the core did not keep up" });
  });

  test("the binding: one receive at a time (a second is Protocol), a lone message answered within milliseconds, close during a receive answers []", async () => {
    // "Within milliseconds" is not a number of them that a machine can be held to (a loopback round trip and the 2 ms timer that ends a burst take as long as
    // the machine lets them). So the adapter is wrapped to arm a timer of the binding's quiet period, 2 ms (`QUIET_MS` of the runtime's realtime lines, which
    // the package does not export), as it hands each message to the binding, where the binding arms its own, and the answer is measured against that timer:
    // a message held for a linger answers milliseconds after it on a machine whose timers can tell 2 ms from 8; a slow machine moves both and leaves the difference.
    let reference: Promise<number> = Promise.resolve(0);
    const tapped: WebSocketAdapter = {
      async connect(url, protocols, headers) {
        const connection = await adapter.connect(url, protocols, headers);
        return {
          protocol: connection.protocol,
          send: (message) => connection.send(message),
          close: (code, reason) => connection.close(code, reason),
          messages: () => ({
            [Symbol.asyncIterator]() {
              const iterator = connection.messages()[Symbol.asyncIterator]();
              return {
                async next() {
                  const step = await iterator.next();
                  if (step.done !== true) reference = new Promise((resolve) => setTimeout(() => resolve(performance.now()), 2));
                  return step;
                },
                return: (value) => iterator.return?.(value) ?? Promise.resolve({ done: true, value: undefined }),
              };
            },
          }),
        };
      },
    };
    const ports = realtimePorts({ webSocket: tapped, sse: reactNativeSse({ transport: "xhr", XMLHttpRequest: NodeXhr }) });
    const port = ports[OptInPortIds.WebSocket.portId]!;
    const ids = OptInPortIds.WebSocket;
    const w = new UndraWriter(64);
    w.writeStr(`${server.wsUrl}/ws/echo`);
    codecs.vec(codecs.string).encode(w, []);
    codecs.vec(HeaderCodec).encode(w, []);
    const { conn } = decodeValue(WsOpenedCodec, await port.methods[ids.connect]!(w.finish()));
    const receiveArgs = (max: number): Uint8Array => {
      const r = new UndraWriter(8);
      r.writeU32(conn);
      r.writeU32(max);
      return r.finish();
    };
    const sendText = async (value: string): Promise<void> => {
      const s = new UndraWriter(16);
      s.writeU32(conn);
      WsMessageCodec.encode(s, text(value));
      await port.methods[ids.send]!(s.finish());
    };
    const portError = async (pending: Promise<Uint8Array> | Uint8Array): Promise<WsError> => {
      const error = await rejection(Promise.resolve(pending));
      expect(error).toBeInstanceOf(UndraPortError);
      return decodeValue(WsErrorCodec, (error as UndraPortError).body);
    };
    const receive = (max: number) => Promise.resolve(port.methods[ids.receive]!(receiveArgs(max)));

    // A lone message: the pull answers it once it has been quiet for 2 ms, not after waiting for 16.
    const lone: number[] = [];
    for (let i = 0; i < 20; i++) {
      const pending = receive(16);
      await sendText(`lone ${i}`);
      expect(decodeValue(codecs.vec(WsMessageCodec), await pending)).toEqual([text(`lone ${i}`)]);
      lone.push(performance.now() - (await reference));
    }
    lone.sort((a, b) => a - b);
    expect(lone[10], `a lone message was answered ${lone[10]} ms (the median of 20) after a quiet-period timer armed as it arrived`).toBeLessThan(4);
    expect(lone[17], "a lone message waited 100 ms or more past its quiet-period timer in three rounds of 20").toBeLessThan(100);

    // A second receive while one waits is the typed Protocol error; the first still answers.
    const first = receive(16);
    const second = await portError(port.methods[ids.receive]!(receiveArgs(16)));
    expect(second).toBeInstanceOf(WsError.Protocol);
    expect(second.message).toContain(`a receive is already pending on connection ${conn}`);

    // The core's close answers the waiting receive with [] (once), and every later one with [] too.
    const close = new UndraWriter(16);
    close.writeU32(conn);
    close.writeU16(1000);
    close.writeStr("bye");
    await port.methods[ids.close]!(close.finish());
    expect(decodeValue(codecs.vec(WsMessageCodec), await first)).toEqual([]);
    expect(decodeValue(codecs.vec(WsMessageCodec), await receive(16))).toEqual([]);
    // A send after the core's close is Closed with that close's code and reason.
    const late = new UndraWriter(16);
    late.writeU32(conn);
    WsMessageCodec.encode(late, text("late"));
    const closed = await portError(port.methods[ids.send]!(late.finish()));
    expect(closed).toBeInstanceOf(WsError.Closed);
    expect(closed).toMatchObject({ code: 1000, reason: "bye" });
    await eventually(() => lastConnection("/ws/echo").closeCode === 1000);
    port.dispose?.();
    ports[OptInPortIds.Sse.portId]!.dispose?.();
  });

  test("through the binding loadNative registers: connect, send, receive, close", async () => {
    const ports = realtimePorts({ webSocket: adapter, sse: reactNativeSse({ transport: "xhr", XMLHttpRequest: NodeXhr }) });
    const port = ports[OptInPortIds.WebSocket.portId]!;
    const ids = OptInPortIds.WebSocket;
    const w = new UndraWriter(64);
    w.writeStr(`${server.wsUrl}/ws/echo`);
    codecs.vec(codecs.string).encode(w, ["p"]);
    codecs.vec(HeaderCodec).encode(w, []);
    const opened = decodeValue(WsOpenedCodec, await port.methods[ids.connect]!(w.finish()));
    expect(opened).toEqual({ conn: 1, protocol: "p" });
    const send = new UndraWriter(16);
    send.writeU32(opened.conn);
    WsMessageCodec.encode(send, text("ping"));
    await port.methods[ids.send]!(send.finish());
    const receive = new UndraWriter(8);
    receive.writeU32(opened.conn);
    receive.writeU32(16);
    expect(decodeValue(codecs.vec(WsMessageCodec), await port.methods[ids.receive]!(receive.finish()))).toEqual([text("ping")]);
    const close = new UndraWriter(16);
    close.writeU32(opened.conn);
    close.writeU16(1000);
    close.writeStr("bye");
    await port.methods[ids.close]!(close.finish());
    await eventually(() => lastConnection("/ws/echo").closeCode === 1000);
    port.dispose?.();
    ports[OptInPortIds.Sse.portId]!.dispose?.();
  });
});

// ----- Sse ----------------------------------------------------------------------------------------

/** What /sse/feed sends, as events (contract-tests/servers/realtime-server.mjs, FEED). */
const FEED: SseEvent[] = [
  { id: "1", event: "message", data: "one", retryMs: 1500 },
  { id: "2", event: "tick", data: "two\nlines", retryMs: null },
  { id: "2", event: "message", data: "three", retryMs: null },
  { id: "4", event: "message", data: "four", retryMs: null },
];

describe("reactNativeSse over XMLHttpRequest progress events", () => {
  const sse = reactNativeSse({ transport: "xhr", XMLHttpRequest: NodeXhr });

  test("the feed parses as the HTML standard says (ids persist, multi-line data, CRLF, retry, comments); the end is Ended", async () => {
    const stream = await sse.open(`${server.url}/sse/feed`, [{ name: "x-undra-token", value: "s1" }], null);
    const read = await drain(stream.events());
    expect(read.items).toEqual(FEED);
    expect(read.end).toBeInstanceOf(SseError.Ended);
    const request = NodeXhr.made[NodeXhr.made.length - 1]!;
    expect(request.headers).toEqual({ Accept: "text/event-stream", "Cache-Control": "no-cache", "x-undra-token": "s1" });
    expect(lastConnection("/sse/feed").headers["x-undra-token"]).toBe("s1");
  });

  test("resumes after Last-Event-ID", async () => {
    const stream = await sse.open(`${server.url}/sse/feed`, [], "2");
    const read = await drain(stream.events());
    expect(read.items).toEqual(FEED.slice(2));
    expect(lastConnection("/sse/feed").headers["last-event-id"]).toBe("2");
  });

  test("a status other than 2xx (204 included) is Refused { status }; another type is Protocol", async () => {
    for (const code of [500, 204, 401]) {
      const refused = await rejection(sse.open(`${server.url}/sse/status?code=${code}`, [], null));
      expect(refused).toBeInstanceOf(SseError.Refused);
      expect((refused as InstanceType<typeof SseError.Refused>).status).toBe(code);
    }
    const html = await rejection(sse.open(`${server.url}/sse/html`, [], null));
    expect(html).toBeInstanceOf(SseError.Protocol);
    expect((html as Error).message).toContain("expected text/event-stream, got text/html");
  });

  test("no answer is Network", async () => {
    const failed = await rejection(sse.open("http://127.0.0.1:1/sse/feed", [], null));
    expect(failed).toBeInstanceOf(SseError.Network);
  });

  test("close ends the request: the server sees the client leave, and the events finish", async () => {
    const stream = await sse.open(`${server.url}/sse/hang`, [], null);
    const events = stream.events()[Symbol.asyncIterator]();
    const pending = events.next();
    await stream.close();
    expect((await pending).done).toBe(true);
    await eventually(() => lastConnection("/sse/hang").clientClosed);
  });

  test("events wait for the core up to the limit, then the stream ends with Network (an XMLHttpRequest cannot pause)", async () => {
    const small = reactNativeSse({ transport: "xhr", XMLHttpRequest: NodeXhr, maxBufferedEvents: 2 });
    const stream = await small.open(`${server.url}/sse/feed`, [], null);
    // The request is over once the adapter gave up at the third event and aborted it (or the feed ended first): the events
    // and the end are settled in the same turn, so that is when to read.
    const request = NodeXhr.made[NodeXhr.made.length - 1];
    await eventually(() => request?.readyState === 4, "the end of the /sse/feed request");
    const read = await drain(stream.events());
    expect(read.items).toEqual(FEED.slice(0, 2));
    expect(read.end).toBeInstanceOf(SseError.Network);
    expect((read.end as Error).message).toContain("did not keep up");
  });

  test("a flood under a stalled reader stays bounded at the default limit (4,096 events), then Network", async () => {
    const stream = await sse.open(`${server.url}/sse/flood?n=6000&size=8`, [], null);
    // The flood has arrived when the request is over: the adapter gave up at the 4,097th event and aborted it, or the whole
    // body came first. (Not when the server has written 6,000 events: whether Node's HTTP write path accepts all of them at
    // once or pauses for `drain`, and so whether the client's abort comes before the last write, depends on the Node and the
    // OS, and the reader must still be stalled when it is over: reading earlier would keep up with the flood.)
    const request = NodeXhr.made[NodeXhr.made.length - 1];
    await eventually(() => request?.readyState === 4, "the end of the /sse/flood request");
    const read = await drain(stream.events());
    expect(read.items.length).toBe(4096);
    expect(read.items[4095]?.id).toBe("4095");
    expect(read.end).toBeInstanceOf(SseError.Network);
    expect((read.end as Error).message).toContain("did not keep up");
  });

  test("through the binding loadNative registers: open, next, close", async () => {
    const port = realtimePorts({ webSocket: reactNativeWebSocket({ WebSocket: RnWebSocket }), sse })[OptInPortIds.Sse.portId]!;
    const ids = OptInPortIds.Sse;
    const open = new UndraWriter(64);
    open.writeStr(`${server.url}/sse/feed`);
    codecs.vec(HeaderCodec).encode(open, []);
    codecs.option(codecs.string).encode(open, null);
    const stream = decodeValue(codecs.u32, await port.methods[ids.open]!(open.finish()));
    const next = new UndraWriter(8);
    next.writeU32(stream);
    next.writeU32(16);
    const nextArgs = next.finish();
    const events: SseEvent[] = [];
    while (events.length < FEED.length) events.push(...decodeValue(codecs.vec(SseEventCodec), await port.methods[ids.next]!(nextArgs)));
    expect(events).toEqual(FEED);
    const close = new UndraWriter(4);
    close.writeU32(stream);
    await port.methods[ids.close]!(close.finish());
    port.dispose?.();
  });
});

describe("reactNativeSse over a streaming fetch", () => {
  test("a fetch with body streams (here Node's) is used when there is one: the same feed, then Ended", async () => {
    const stream = await reactNativeSse().open(`${server.url}/sse/feed`, [], null);
    const read = await drain(stream.events());
    expect(read.items).toEqual(FEED);
    expect(read.end).toBeInstanceOf(SseError.Ended);
  });
});

// ----- what loadNative registers --------------------------------------------------------------------

function installFake(native: FakeNative): void {
  setTurboModule("UndraNative", {
    install(namespace: string) {
      g.__undraNative = { ...g.__undraNative, [namespace]: native };
      return true;
    },
  });
}

/** The core entry of a fake, as a generated `Undra<Namespace>` would be. */
function entryOf(native: FakeNative): NativeCoreEntry {
  return { namespace: native.namespace, schemaHash: native.hash };
}

describe("loadNative and the opt-in ports", () => {
  test("reactNativeAdapters has the WebSocket and Sse adapters", () => {
    const adapters = reactNativeAdapters();
    expect(typeof adapters.webSocket.connect).toBe("function");
    expect(typeof adapters.sse.open).toBe("function");
  });

  test("the core's WebSocket and Sse calls reach the default bindings (a URL that is not ws:// is refused before any socket)", async () => {
    const native = new FakeNative();
    native.schema = {
      ports: [
        { port_id: OptInPortIds.WebSocket.portId, kind: "async", methods: [{ method_id: OptInPortIds.WebSocket.connect, is_async: true }] },
        { port_id: OptInPortIds.Sse.portId, kind: "async", methods: [{ method_id: OptInPortIds.Sse.next, is_async: true }] },
      ],
    };
    installFake(native);
    const core = await loadNative(entryOf(native));
    expect(native.started?.ports).toEqual([OptInPortIds.WebSocket.portId, OptInPortIds.Sse.portId]);
    const connect = new UndraWriter(32);
    connect.writeStr("http://not-a-websocket");
    codecs.vec(codecs.string).encode(connect, []);
    codecs.vec(HeaderCodec).encode(connect, []);
    native.queue(RecordKind.PortCall, new Uint8Array([...le([OptInPortIds.WebSocket.portId, "u32"], [OptInPortIds.WebSocket.connect, "u32"], [7, "u32"]), ...connect.finish()]), "core");
    const next = new UndraWriter(8);
    next.writeU32(42);
    next.writeU32(16);
    native.queue(RecordKind.PortCall, new Uint8Array([...le([OptInPortIds.Sse.portId, "u32"], [OptInPortIds.Sse.next, "u32"], [8, "u32"]), ...next.finish()]), "core");
    await eventually(() => native.portReplies.length === 2);
    const replies = native.portReplies.map((bytes) => decodePortReply(bytes));
    const ws = replies.find((r) => r.portCallId === 7)!;
    expect(ws.status).toBe(PortStatus.Error);
    const refused = decodeValue(WsErrorCodec, ws.body);
    expect(refused).toBeInstanceOf(WsError.Refused);
    expect(refused.message).toContain("invalid URL: http://not-a-websocket");
    const sse = replies.find((r) => r.portCallId === 8)!;
    expect(sse.status).toBe(PortStatus.Error);
    core.close();
  });

  test("an app's own WebSocket port in `ports` replaces the default", async () => {
    const native = new FakeNative();
    native.schema = { ports: [{ port_id: OptInPortIds.WebSocket.portId, kind: "async", methods: [{ method_id: OptInPortIds.WebSocket.connect, is_async: true }] }] };
    installFake(native);
    const seen: number[] = [];
    const core = await loadNative(entryOf(native), {
      ports: {
        [OptInPortIds.WebSocket.portId]: {
          sync: false,
          methods: {
            [OptInPortIds.WebSocket.connect]: () => {
              seen.push(1);
              return new Uint8Array([1, 0, 0, 0, 0, 0, 0, 0]);
            },
          },
        },
      },
    });
    native.queue(RecordKind.PortCall, new Uint8Array([...le([OptInPortIds.WebSocket.portId, "u32"], [OptInPortIds.WebSocket.connect, "u32"], [9, "u32"])]), "core");
    await eventually(() => native.portReplies.length === 1);
    expect(seen).toEqual([1]);
    expect(decodePortReply(native.portReplies[0]!).status).toBe(PortStatus.Ok);
    core.close();
  });

  test("Db is a native default (port 0x559eda82) unless `ports` replaces it", async () => {
    expect(OptInPortIds.Db.portId).toBe(0x559eda82);
    const offered = { ports: [PortIds.Kv.portId, OptInPortIds.Db.portId] };
    expect(nativeDefaultPorts(offered, {})).toEqual([PortIds.Kv.portId, OptInPortIds.Db.portId]);
    expect(nativeDefaultPorts(offered, { ports: { [OptInPortIds.Db.portId]: { sync: false, methods: {} } } })).toEqual([PortIds.Kv.portId]);
    expect(nativeDefaultPorts(offered, { adapters: { kv: null } })).toEqual([OptInPortIds.Db.portId]);

    const native = new FakeNative();
    native.defaults = { ports: [OptInPortIds.Db.portId], db: "/data/user/0/app/databases/undra-<name>.sqlite" };
    native.schema = { ports: [{ port_id: OptInPortIds.Db.portId, kind: "async", methods: [{ method_id: OptInPortIds.Db.open, is_async: true }] }] };
    installFake(native);
    expect(nativePlatformDefaults(native.namespace).db).toBe("/data/user/0/app/databases/undra-<name>.sqlite");
    const core = await loadNative(entryOf(native));
    expect(native.started?.nativePorts).toEqual([OptInPortIds.Db.portId]);
    core.close();
  });
});
