import { afterEach, describe, expect, test, vi } from "vitest";
import {
  HttpError,
  type HttpRequest,
  type KvAdapter,
  PortIds,
  PortStatus,
  UndraReader,
  UndraWriter,
  decodePortReply,
} from "@undra/runtime";
import { isHttpUrl, loadNative, nativeDefaultPorts, nativePlatformDefaults, reactNativeHttp } from "../src/index.js";
import { RecordKind } from "../src/native.js";
import { setAppState, setTurboModule } from "./support/react-native-stub.js";
import { FakeNative, le } from "./support/fake-native.js";
import { hangDeadline, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

/*
 * The default ports of @undra/react-native (ADR-038 amendment B): which ones the module answers natively for which
 * options, the Http adapter over React Native's fetch (with a scripted fetch), and the JavaScript adapters reached
 * through the transport. The native halves are tested where they run: the C++ against the real core
 * (cpp/test/run.sh), the Java on the JVM (android/test/run.sh), both on the devices (scripts/rn-device-checks.sh).
 */

const g = globalThis as { __undraNative?: Record<string, FakeNative>; fetch?: unknown };
const ALL = [PortIds.Kv.portId, PortIds.SecureStore.portId, PortIds.Fs.portId, PortIds.Connectivity.portId];
const originalFetch = g.fetch;

afterEach(() => {
  delete g.__undraNative;
  setTurboModule("UndraNative", undefined);
  setAppState("active");
  g.fetch = originalFetch;
});

const tick = (ms = 0): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));
/**
 * Waits until `ready()`: a reply through a default port waits for the code of the port, which the runtime loads on the port's
 * first call (ADR-052, `import()`; the time it takes is the module loader's, not a number of ticks). A failed test that stops
 * waiting too early leaves its core open and takes the next tests with it (one run in six, before this).
 */
async function until(ready: () => boolean, ms = hangDeadline): Promise<void> {
  for (const start = Date.now(); !ready(); await tick(1)) if (Date.now() - start > ms) throw new Error(`not ready after ${ms} ms`);
}

/** An in-memory `Kv`: the deterministic stand-in an app passes to replace the native default in a test. */
function memoryKv(): KvAdapter & { readonly entries: Map<string, Uint8Array> } {
  const entries = new Map<string, Uint8Array>();
  return {
    entries,
    get: (key) => Promise.resolve(entries.get(key)?.slice() ?? null),
    set: (key, value) => {
      entries.set(key, value.slice());
      return Promise.resolve();
    },
    delete: (key) => {
      entries.delete(key);
      return Promise.resolve();
    },
    list: (prefix) => Promise.resolve([...entries.keys()].filter((k) => k.startsWith(prefix)).sort()),
  };
}

/** A scripted `fetch`: records what it was given and answers with `respond`. */
function scriptedFetch(respond: (url: string, init: RequestInit) => Promise<Response>) {
  const calls: Array<{ url: string; init: RequestInit }> = [];
  const fetchFn = ((url: string, init: RequestInit) => {
    calls.push({ url, init });
    return respond(url, init);
  }) as unknown as typeof fetch;
  return { fetch: fetchFn, calls };
}

function response(status: number, body: string, headers: Record<string, string> = {}): Response {
  return new Response(body, { status, headers });
}

function request(overrides: Partial<HttpRequest> = {}): HttpRequest {
  return { method: "get", url: "http://127.0.0.1:8737/check", headers: [], body: null, timeoutMs: null, ...overrides };
}

/** Links `native` as the app's core of its namespace (ADR-044: one module object per namespace). */
function installFake(native: FakeNative): void {
  setTurboModule("UndraNative", {
    install(namespace: string) {
      if (namespace !== native.namespace) throw new Error(`no Undra core \`${namespace}\` is linked into this app`);
      g.__undraNative = { ...g.__undraNative, [namespace]: native };
      return true;
    },
  });
}

/** The entry `loadNative` takes: the fake's namespace and schema hash, as a generated entry has them. */
const entryOf = (native: FakeNative) => ({ namespace: native.namespace, schemaHash: native.hash });

describe("which ports the module answers natively", () => {
  test("every port the platform offers, when the app overrides none", () => {
    expect(nativeDefaultPorts({ ports: ALL }, {})).toEqual(ALL);
    expect(nativeDefaultPorts({ ports: [PortIds.Kv.portId] }, {})).toEqual([PortIds.Kv.portId]);
    expect(nativeDefaultPorts({ ports: [] }, {})).toEqual([]);
  });

  test("an adapter, a null or a port implementation in the options keeps that port JavaScript's", () => {
    const chosen = nativeDefaultPorts(
      { ports: ALL },
      {
        adapters: { kv: memoryKv(), secureStore: null, connectivity: { subscribe: () => () => {} } },
        ports: { [PortIds.Fs.portId]: { sync: false, methods: {} } },
      },
    );
    expect(chosen).toEqual([]);
    expect(nativeDefaultPorts({ ports: ALL }, { adapters: { kv: memoryKv() } })).toEqual([
      PortIds.SecureStore.portId,
      PortIds.Fs.portId,
      PortIds.Connectivity.portId,
    ]);
    // An absent key is "not overridden", as in UndraCore.attach.
    expect(nativeDefaultPorts({ ports: ALL }, { adapters: {} })).toEqual(ALL);
  });
});

describe("loadNative and the native defaults", () => {
  test("passes the native defaults to start, and the module's report is readable", async () => {
    const native = new FakeNative();
    native.defaults = {
      ports: ALL,
      kv: "/data/undra/playground_core/kv",
      fs: "/data/undra/playground_core/fs",
      secureStore: "Keychain service playground_core.dev.undra.securestore",
    };
    installFake(native);
    expect(nativePlatformDefaults(native.namespace)).toEqual(native.defaults);
    const core = await loadNative(entryOf(native));
    expect(native.started?.nativePorts).toEqual(ALL);
    core.close();
  });

  test("an overridden port is not native, and its JavaScript adapter answers the core", async () => {
    const native = new FakeNative();
    native.defaults = { ports: ALL };
    native.schema = { ports: [{ port_id: PortIds.Kv.portId, kind: "async", methods: [{ method_id: PortIds.Kv.get, is_async: true }] }] };
    installFake(native);
    const kv = memoryKv();
    kv.entries.set("k", new Uint8Array([7]));
    const core = await loadNative(entryOf(native), { adapters: { kv } });
    expect(native.started?.nativePorts).toEqual([PortIds.SecureStore.portId, PortIds.Fs.portId, PortIds.Connectivity.portId]);
    expect(native.started?.ports).toContain(PortIds.Kv.portId);
    // The core asks Kv.get("k") from one of its threads: the override answers Some([7]).
    const args = new UndraWriter(8);
    args.writeStr("k");
    native.queue(RecordKind.PortCall, new Uint8Array([...le([PortIds.Kv.portId, "u32"], [PortIds.Kv.get, "u32"], [31, "u32"]), ...args.finish()]), "core");
    await until(() => native.portReplies.length > 0);
    const reply = decodePortReply(native.portReplies[0]!);
    expect(reply.portCallId).toBe(31);
    expect(reply.status).toBe(PortStatus.Ok);
    expect([...reply.body]).toEqual([1, 1, 0, 0, 0, 7]);
    core.close();
  });

  test("a platform without defaults is logged once and every port stays JavaScript's", async () => {
    const native = new FakeNative();
    native.defaults = { ports: [], error: "the Android library of @undra/react-native is not in the app" };
    installFake(native);
    const lines: string[] = [];
    const core = await loadNative(entryOf(native), { adapters: { log: { log: (level, _t, m) => lines.push(`${level} ${m}`) } } });
    expect(native.started?.nativePorts).toEqual([]);
    expect(lines.filter((l) => l.includes("native default ports are off"))).toHaveLength(1);
    core.close();
  });

  test("the default Http is React Native's fetch, reached from a core thread", async () => {
    const native = new FakeNative();
    native.schema = { ports: [{ port_id: PortIds.Http.portId, kind: "async", methods: [{ method_id: PortIds.Http.request, is_async: true }] }] };
    installFake(native);
    const scripted = scriptedFetch(() => Promise.resolve(response(200, "pong", { "x-echo": "1" })));
    g.fetch = scripted.fetch;
    const core = await loadNative(entryOf(native));
    const w = new UndraWriter(64);
    w.writeU16(0); // get
    w.writeStr("http://127.0.0.1:8737/ping");
    w.writeLen(0); // headers
    w.writeU8(0); // no body
    w.writeU8(0); // no timeout
    native.queue(RecordKind.PortCall, new Uint8Array([...le([PortIds.Http.portId, "u32"], [PortIds.Http.request, "u32"], [5, "u32"]), ...w.finish()]), "core");
    await until(() => native.portReplies.length > 0);
    expect(scripted.calls.map((c) => c.url)).toEqual(["http://127.0.0.1:8737/ping"]);
    const reply = decodePortReply(native.portReplies[0]!);
    expect(reply.status).toBe(PortStatus.Ok);
    const r = new UndraReader(reply.body);
    expect(r.readU16()).toBe(200);
    core.close();
  });

  test("a native default is the only one: a JavaScript adapter the web defaults would add is dropped", async () => {
    // Review (2026-10-02): `UndraCore.attach` merges `browserAdapters()` under the options, and an app that polyfills
    // `navigator.onLine` and a global `addEventListener` gets `browserConnectivity()` from it: a second Connectivity
    // source next to the native one, reporting `online` whatever the device says.
    const target = globalThis as { navigator?: unknown; addEventListener?: unknown; removeEventListener?: unknown };
    const navigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    Object.defineProperty(globalThis, "navigator", { value: { onLine: true }, configurable: true, writable: true });
    target.addEventListener = () => {};
    target.removeEventListener = () => {};
    try {
      const native = new FakeNative();
      native.defaults = { ports: ALL };
      installFake(native);
      const core = await loadNative(entryOf(native));
      await tick(5);
      expect(native.started?.nativePorts).toEqual(ALL);
      expect(native.log.filter((l) => l.startsWith(`event ${PortIds.Connectivity.portId} `))).toEqual([]);
      core.close();

      // Overridden, the app's own source is the one that reports.
      delete g.__undraNative;
      const again = new FakeNative();
      again.defaults = { ports: ALL };
      installFake(again);
      const reports: boolean[] = [];
      const second = await loadNative(entryOf(again), {
        adapters: {
          connectivity: {
            subscribe: (emit) => {
              queueMicrotask(() => {
                reports.push(true);
                emit(false, "none");
              });
              return () => {};
            },
          },
        },
      });
      // The app's source reports from a microtask after it subscribed; the event reaches the module after that.
      const connectivityEvents = (): string[] => again.log.filter((l) => l.startsWith(`event ${PortIds.Connectivity.portId} `));
      await until(() => connectivityEvents().length > 0);
      expect(again.started?.nativePorts).not.toContain(PortIds.Connectivity.portId);
      expect(connectivityEvents()).toHaveLength(1);
      expect(reports).toEqual([true]);
      second.close();
    } finally {
      delete target.addEventListener;
      delete target.removeEventListener;
      if (navigator !== undefined) Object.defineProperty(globalThis, "navigator", navigator);
      else delete target.navigator;
    }
  });

  test("the default Lifecycle reports AppState to the core", async () => {
    const native = new FakeNative();
    installFake(native);
    const core = await loadNative(entryOf(native));
    const events = (): string[] => native.log.filter((l) => l.startsWith(`event ${PortIds.Lifecycle.portId} `));
    await until(() => events().length > 0); // the current state, reported once the adapter subscribed
    setAppState("background");
    setAppState("background"); // the same state again is not a report
    setAppState("unknown"); // nor one that maps to the same state
    expect(events()).toHaveLength(2); // the current state first, then the change
    setAppState("active");
    expect(events()).toHaveLength(3);
    core.close();
  });
});

describe("reactNativeHttp", () => {
  test("a GET: status, headers and body; the method, headers and URL as given", async () => {
    const scripted = scriptedFetch(() => Promise.resolve(response(200, "hello", { "content-type": "text/plain", "x-two": "2" })));
    const http = reactNativeHttp({ fetch: scripted.fetch });
    const res = await http.request(request({ headers: [{ name: "x-a", value: "1" }, { name: "x-a", value: "2" }] }));
    expect(res.status).toBe(200);
    expect(new TextDecoder().decode(res.body)).toBe("hello");
    expect(res.headers).toContainEqual({ name: "x-two", value: "2" });
    expect(scripted.calls[0]?.url).toBe("http://127.0.0.1:8737/check");
    expect(scripted.calls[0]?.init.method).toBe("GET");
    expect(scripted.calls[0]?.init.headers).toEqual([
      ["x-a", "1"],
      ["x-a", "2"],
    ]);
  });

  test("a body is sent as exactly its bytes, even from a view into a larger buffer", async () => {
    const scripted = scriptedFetch(() => Promise.resolve(response(201, "")));
    const big = new Uint8Array([9, 9, 1, 2, 3, 9]);
    await reactNativeHttp({ fetch: scripted.fetch }).request(request({ method: "post", body: big.subarray(2, 5) }));
    const sent = scripted.calls[0]?.init.body as Uint8Array;
    expect([...sent]).toEqual([1, 2, 3]);
    expect(sent.buffer.byteLength).toBe(3);
  });

  test("an error status is a response, not an error", async () => {
    const res = await reactNativeHttp({ fetch: scriptedFetch(() => Promise.resolve(response(503, "busy"))).fetch }).request(request());
    expect(res.status).toBe(503);
  });

  test("no connection is HttpError.Network (an offline device is never 'unavailable')", async () => {
    const offline = scriptedFetch(() => Promise.reject(new TypeError("Network request failed")));
    await expect(reactNativeHttp({ fetch: offline.fetch }).request(request())).rejects.toSatisfy(
      (e: unknown) => e instanceof HttpError.Network && e.value === "Network request failed",
    );
  });

  test("the timeout covers the whole exchange and is HttpError.Timeout; another abort is Cancelled", async () => {
    const hangs = scriptedFetch(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => reject(Object.assign(new Error("aborted"), { name: "AbortError" })));
        }),
    );
    await expect(reactNativeHttp({ fetch: hangs.fetch }).request(request({ timeoutMs: 10 }))).rejects.toBeInstanceOf(HttpError.Timeout);
    const aborted = scriptedFetch(() => Promise.reject(Object.assign(new Error("aborted"), { name: "AbortError" })));
    await expect(reactNativeHttp({ fetch: aborted.fetch }).request(request())).rejects.toBeInstanceOf(HttpError.Cancelled);
  });

  test("a URL React Native's URL shim would accept but is not http(s)://host is InvalidUrl, before any fetch", async () => {
    const scripted = scriptedFetch(() => Promise.resolve(response(200, "")));
    const http = reactNativeHttp({ fetch: scripted.fetch });
    for (const url of ["not a url", "ftp://host/file", "http://", "http:///path", "http://host name/", "http://host:99999/", "//host/x", "HTTP:host"]) {
      await expect(http.request(request({ url }))).rejects.toSatisfy((e: unknown) => e instanceof HttpError.InvalidUrl && e.value === url);
    }
    expect(scripted.calls).toHaveLength(0);
    for (const url of ["http://127.0.0.1:8737/x?y=1#z", "https://[::1]:443/", "http://user@host/", "HTTPS://Example.com", "http://localhost"]) {
      expect(isHttpUrl(url)).toBe(true);
    }
  });

  test("without fetch it is HttpError.Network", async () => {
    g.fetch = undefined;
    await expect(reactNativeHttp().request(request())).rejects.toBeInstanceOf(HttpError.Network);
  });
});
