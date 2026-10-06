import { afterEach, describe, expect, test, vi } from "vitest";
import {
  BackgroundReportCodec,
  CallTarget,
  PanicReportCodec,
  PortIds,
  ReplyStatus,
  type AppState,
  UndraCallError,
  UndraCore,
  type UndraPanicReport,
  UndraReplyError,
  UndraUnhandledError,
  encodeValue,
  codecs,
  decodeValue,
} from "@undra/runtime";
import { loadNative } from "../src/index.js";
import { RecordKind } from "../src/native.js";
import { NativeTransport } from "../src/transport.js";
import { FakeNative, le } from "./support/fake-native.js";
import { setTurboModule } from "./support/react-native-stub.js";
import { arrived, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

/*
 * ADR-046 on React Native: the native host answers the core's `Diagnostics.panicked` itself and queues each report for the JS
 * thread as a `PortCall` record of that port (cpp/UndraHost.cpp); the transport hands it to the runtime, which calls `onPanic`
 * once per report, in order, and never throws into the core. `runInBackground` and the stats go through the same transport as
 * any call.
 */

const RUN_BACKGROUND = 0x0e5b14ff;

const report = (n: number): UndraPanicReport => ({
  message: `kaboom ${n}`,
  location: "core/src/lab.rs:42:9",
  operation: "explode",
  thread: "undra-core",
  frames: [{ address: 0x1f2cn, symbol: "undra_core::lab::explode", file: "lab.rs", line: 42 }],
  namespace: "fake_core",
  coreVersion: "1.2.3",
  schemaHash: 0x1234_5678_9abc_def0n,
  imageId: "0123456789abcdef0123456789abcdef",
});

/** The `PortCall` record the host queues for a panic report: `port_id, method_id, port_call_id 0`, then the encoded report. */
function panicRecord(r: UndraPanicReport): Uint8Array {
  return new Uint8Array([...le([PortIds.Diagnostics.portId, "u32"], [PortIds.Diagnostics.panicked, "u32"], [0, "u32"]), ...encodeValue(PanicReportCodec, r)]);
}

async function attach(options: { onPanic?: (report: UndraPanicReport) => void; native?: FakeNative } = {}) {
  const native = options.native ?? new FakeNative();
  const errors: unknown[] = [];
  const logs: Array<[number, string, string]> = [];
  const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash, platform: "test", onError: (e) => errors.push(e) });
  const core = await UndraCore.attach(transport, {
    expectedSchemaHash: native.hash,
    shared: false,
    adapters: { log: { log: (level, target, message) => logs.push([level, target, message]) }, http: null, connectivity: null, lifecycle: null },
    onError: (e) => errors.push(e),
    ...(options.onPanic && { onPanic: options.onPanic }),
    mirror: { schedule: (fn) => setTimeout(fn, 0) },
  });
  return { core, native, errors, logs };
}

const opened: UndraCore[] = [];
afterEach(() => {
  for (const core of opened.splice(0)) core.close();
});

describe("a panic report of the native core", () => {
  test("reaches onPanic once, whole, from a core thread, and is not answered (call id 0)", async () => {
    const seen: UndraPanicReport[] = [];
    const { core, native } = await attach({ onPanic: (r) => seen.push(r) });
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(1)), "core");
    await arrived(() => expect(seen).toHaveLength(1));
    expect(seen).toEqual([report(1)]);
    expect(native.portReplies, "nothing waits for an answer to the core's own report: none is sent").toEqual([]);
    expect(typeof seen[0]?.schemaHash).toBe("bigint");
    expect(seen[0]?.frames[0]).toEqual({ address: 0x1f2cn, symbol: "undra_core::lab::explode", file: "lab.rs", line: 42 });
  });

  test("from several threads, in the order the panics happened, each once", async () => {
    const seen: string[] = [];
    const { core, native } = await attach({ onPanic: (r) => seen.push(r.message) });
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(1)), "core");
    native.queue(RecordKind.PortCall, panicRecord(report(2)), "core");
    native.queue(RecordKind.PortCall, panicRecord(report(3)), "js");
    await arrived(() => expect(seen.length).toBeGreaterThanOrEqual(3));
    expect(seen).toEqual(["kaboom 1", "kaboom 2", "kaboom 3"]);
  });

  test("a panic during a synchronous call is delivered on the JS thread before the call fails", async () => {
    const seen: string[] = [];
    const { core, native } = await attach({ onPanic: (r) => seen.push(r.message) });
    opened.push(core);
    let failed = false;
    native.onCallSync = () => {
      // The core panics inside the call: the host queues the report on the JS thread, and the call is answered status 2.
      native.queue(RecordKind.PortCall, panicRecord(report(7)), "js");
      return new Uint8Array([...le([1, "u32"], [ReplyStatus.Panic, "u8"])]);
    };
    try {
      core.callSync(CallTarget.FreeFunction, 5, new Uint8Array(0));
    } catch (error) {
      failed = true;
      expect(error).toBeInstanceOf(UndraReplyError);
      expect(seen, "the report arrived with the call, before it was thrown").toEqual(["kaboom 7"]);
    }
    expect(failed).toBe(true);
  });

  test("a handler that throws is reported to onError and changes nothing: the next report is delivered", async () => {
    const seen: string[] = [];
    const { core, native, errors } = await attach({
      onPanic: (r) => {
        seen.push(r.message);
        if (r.message === "kaboom 1") throw new Error("the reporter is down");
      },
    });
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(1)), "core");
    native.queue(RecordKind.PortCall, panicRecord(report(2)), "core");
    await arrived(() => expect(seen.length).toBeGreaterThanOrEqual(2));
    expect(seen).toEqual(["kaboom 1", "kaboom 2"]);
    const unhandled = errors.filter((e): e is UndraUnhandledError => e instanceof UndraUnhandledError);
    expect(unhandled).toHaveLength(1);
    expect(unhandled[0]?.operation).toBe("onPanic");
    expect(String(unhandled[0]?.message)).toContain("the reporter is down");
  });

  test("without onPanic it is logged at error level, one line, so a panic is never silent", async () => {
    const { core, native, logs } = await attach();
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(1)), "core");
    await arrived(() => expect(logs.some(([, target]) => target === "undra::panic")).toBe(true));
    expect(logs.filter(([, target]) => target === "undra::panic")).toEqual([[4, "undra::panic", "explode: kaboom 1 (core/src/lab.rs:42:9)"]]);
  });

  test("a report that does not decode is reported once and never reaches onPanic", async () => {
    const seen: UndraPanicReport[] = [];
    const { core, native, errors } = await attach({ onPanic: (r) => seen.push(r) });
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(1)).subarray(0, 20), "core");
    await arrived(() => expect(errors.length).toBeGreaterThan(0)); // the report of the record that does not decode
    expect(seen).toEqual([]);
    expect(errors).toHaveLength(1);
  });
});

describe("loadNative and the panic report", () => {
  test("onPanic passes through the options to the core it attaches", async () => {
    const native = new FakeNative();
    const g = globalThis as { __undraNative?: Record<string, FakeNative> };
    setTurboModule("UndraNative", {
      install(namespace: string) {
        g.__undraNative = { ...g.__undraNative, [namespace]: native };
        return true;
      },
    });
    const seen: UndraPanicReport[] = [];
    const core = await loadNative({ namespace: native.namespace, schemaHash: native.hash }, { adapters: { http: null }, onPanic: (r) => seen.push(r) });
    opened.push(core);
    native.queue(RecordKind.PortCall, panicRecord(report(4)), "core");
    await arrived(() => expect(seen).toHaveLength(1));
    expect(seen.map((r) => r.message)).toEqual(["kaboom 4"]);
    delete g.__undraNative;
    setTurboModule("UndraNative", undefined);
  });
});

describe("runInBackground and the stats through the native transport", () => {
  test("runInBackground is an ordinary call of the standard function; the report comes back decoded", async () => {
    const { core, native } = await attach();
    opened.push(core);
    const calls: Uint8Array[] = [];
    native.onCall = (payload) => {
      calls.push(payload);
      const callId = new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint32(13, true);
      native.queue(
        RecordKind.Reply,
        new Uint8Array([...le([callId, "u32"], [ReplyStatus.Ok, "u8"]), ...encodeValue(BackgroundReportCodec, { finished: true, replayed: 2, refetched: 0, stillPending: 0 })]),
        "core",
      );
      return 0;
    };
    const result = await core.runInBackground(25_000);
    expect(result).toEqual({ finished: true, replayed: 2, refetched: 0, stillPending: 0 });
    const call = calls[0] as Uint8Array;
    expect(new DataView(call.buffer, call.byteOffset, call.byteLength).getUint32(9, true)).toBe(RUN_BACKGROUND);
    expect(decodeValue(codecs.u64, call.subarray(17))).toBe(25_000n);
  });

  test("aborting the signal cancels the call in the native core", async () => {
    const { core, native } = await attach();
    opened.push(core);
    let sent = 0;
    native.onCall = () => {
      sent += 1;
      return 0; // never answered
    };
    const abort = new AbortController();
    const run = core.runInBackground(30_000, { signal: abort.signal }).then(
      () => undefined,
      (e: unknown) => e,
    );
    // `runInBackground` loads on demand (ADR-052, prod-ops review): the call reaches the core a few ticks later.
    await arrived(() => expect(sent).toBe(1));
    abort.abort();
    const error = await run;
    expect((error as Error).name).toBe("AbortError");
    expect(native.log.some((entry) => entry.startsWith("cancel "))).toBe(true);
  });

  test("a closed core fails with UndraCallError, never a raw error", async () => {
    const { core } = await attach();
    core.close();
    await expect(core.runInBackground(1000)).rejects.toBeInstanceOf(UndraCallError.Unavailable);
  });

  test("the stats carry panicReports and the background counters of the native core's stats_json", async () => {
    const native = new FakeNative();
    native.statsJson = () => JSON.stringify({ live_handles: 3, panic_reports: 2, background: { tasks: 3, pending: 1, runs: 4, finished: 2, replayed: 5, refetched: 6 } });
    const { core } = await attach({ native });
    opened.push(core);
    const stats = await core.stats();
    expect(stats.panicReports).toBe(2);
    expect(stats.background).toEqual({ tasks: 3, pending: 1, runs: 4, finished: 2, replayed: 5, refetched: 6 });
  });

  test("no page, no automatic window: going to the background reports Lifecycle.Background and runs nothing by itself", async () => {
    const native = new FakeNative();
    native.statsJson = () => JSON.stringify({ background: { pending: 2 } });
    let emit: ((state: AppState) => void) | undefined;
    const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash, platform: "test" });
    const core = await UndraCore.attach(transport, {
      expectedSchemaHash: native.hash,
      shared: false,
      adapters: {
        log: { log() {} },
        http: null,
        connectivity: null,
        lifecycle: {
          subscribe(fn) {
            emit = fn;
            return () => {};
          },
        },
      },
    });
    opened.push(core);
    let calls = 0;
    native.onCall = () => {
      calls++;
      return 0;
    };
    (emit as (state: AppState) => void)("background");
    await arrived(() => expect(native.log.some((entry) => entry.startsWith("event "))).toBe(true));
    expect(calls, "the OS grants windows on a phone (ADR-046); a page's own window is the browser's").toBe(0);
  });
});
