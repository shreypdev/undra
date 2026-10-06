import { describe, expect, test, vi } from "vitest";
import {
  CallTarget,
  ChangeOp,
  type Handle,
  Kind,
  PortStatus,
  ReplyStatus,
  StreamFlag,
  UndraCallError,
  UndraCore,
  UndraReader,
  UndraReplyError,
  UndraRestoreError,
  UndraSchemaMismatchError,
  UndraTransportError,
  UndraWriter,
  codecs,
  decodeValue,
  encodeStreamFailure,
  encodeValue,
} from "@undra/runtime";
import { NativeStartCode, RecordKind, portPlan } from "../src/native.js";
import { NativeTransport } from "../src/transport.js";
import { FakeNative, le } from "./support/fake-native.js";
import { arrived, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

/** A `Reply` payload. */
function reply(callId: number, status: number, body: Uint8Array = new Uint8Array(0)): Uint8Array {
  return new Uint8Array([...le([callId, "u32"], [status, "u8"]), ...body]);
}

/** A `ChangeSet` payload with one full-value `u32` entry. */
function changeSet(txn: bigint, handle: bigint, signalId: number, value: number): Uint8Array {
  const body = le([value, "u32"]);
  return new Uint8Array([
    ...le([txn, "u64"], [1, "u32"], [handle, "u64"], [signalId, "u32"], [ChangeOp.FullValue, "u8"], [body.length, "u32"]),
    ...body,
  ]);
}

/** The call id of a `Call` payload (targets 0 and 1). */
function callIdOf(payload: Uint8Array): number {
  return new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint32(13, true);
}

interface Attached {
  readonly core: UndraCore;
  readonly native: FakeNative;
  readonly transport: NativeTransport;
  readonly errors: unknown[];
  readonly logs: Array<[number, string, string]>;
}

async function attach(native = new FakeNative()): Promise<Attached> {
  const errors: unknown[] = [];
  const logs: Array<[number, string, string]> = [];
  const transport = new NativeTransport({
    namespace: native.namespace,
    native,
    expectedSchemaHash: native.hash,
    platform: "test",
    onError: (e) => errors.push(e),
  });
  const core = await UndraCore.attach(transport, {
    expectedSchemaHash: native.hash,
    shared: false,
    adapters: {
      log: { log: (level, target, message) => logs.push([level, target, message]) },
      http: null,
      connectivity: null,
      lifecycle: null,
    },
    onError: (e) => errors.push(e),
    mirror: { schedule: (fn) => setTimeout(fn, 0) },
  });
  return { core, native, transport, errors, logs };
}

/** A store registered with the mirror that records what it is told. */
function fakeStore(core: UndraCore, handle: Handle): Array<[number, number]> {
  const applied: Array<[number, number]> = [];
  core.mirror.register(handle, (signalId, _op, value) => {
    applied.push([signalId, decodeValue(codecs.u32, value)]);
  });
  return applied;
}

// `arrived` (support/wait.ts) waits until what the drain delivers is there, then the check holds. The delivery is a frame or
// a microtask away, a few milliseconds on an idle machine and more on a loaded one: a fixed wait failed under load (the check
// says what arrives, not when; the deadline only catches a delivery that never comes).

describe("start", () => {
  test("hands the core an encoded RuntimeConfig and the schema's port plan", async () => {
    const { native, core } = await attach();
    const started = native.started;
    expect(started).not.toBeNull();
    const r = new UndraReader(started!.config);
    expect([r.readStr(), r.readStr(), r.readU8(), r.readU8(), r.readU8()]).toEqual(["test", "inproc", 1, 0, 2]);
    r.finish();
    // Event ports are left out; sync methods are listed as (port, method) pairs.
    expect(started!.ports).toEqual([0xcd99c48e, 0x1ebeb908, 0x7e57]);
    expect(started!.syncMethods).toEqual([0xcd99c48e, 0xccc94d90, 0x7e57, 0x7e58]);
    expect(core.mode).toBe("native");
    expect(core.hello.undraVersion).toBe("c-abi-2");
  });

  test("refuses another schema before the core is started", async () => {
    const native = new FakeNative();
    const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: 1n });
    await expect(UndraCore.attach(transport, { expectedSchemaHash: 1n, shared: false })).rejects.toBeInstanceOf(UndraSchemaMismatchError);
    expect(native.log).not.toContain("start");
  });

  test("refuses another C ABI version", async () => {
    const native = new FakeNative();
    native.abi = 1;
    const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash });
    await expect(transport.start({} as never)).rejects.toMatchObject({ reason: "handshake" });
    expect(native.log).not.toContain("start");
  });

  test("refuses the module of another core before anything is called", async () => {
    const native = new FakeNative("other_core");
    const transport = new NativeTransport({ namespace: "fake_core", native, expectedSchemaHash: native.hash });
    const failure = transport.start({} as never);
    await expect(failure).rejects.toMatchObject({ reason: "handshake" });
    await expect(failure).rejects.toThrow(/the core `other_core`, not `fake_core`/);
    expect(native.log).not.toContain("start");
    expect(native.sink).toBeUndefined();
  });

  test("without a module it uses the one installed for its namespace, and says so when there is none", async () => {
    const g = globalThis as { __undraNative?: Record<string, unknown> };
    const native = new FakeNative();
    try {
      const missing = new NativeTransport({ namespace: native.namespace, expectedSchemaHash: native.hash });
      await expect(missing.start({} as never)).rejects.toMatchObject({ reason: "unsupported" });
      expect(() => missing.counters()).toThrow(expect.objectContaining({ reason: "unsupported" }));
      g.__undraNative = { other_core: new FakeNative("other_core"), [native.namespace]: native };
      const transport = new NativeTransport({ namespace: native.namespace, expectedSchemaHash: native.hash });
      expect(transport.namespace).toBe("fake_core");
      const core = await UndraCore.attach(transport, { expectedSchemaHash: native.hash, shared: false, adapters: { http: null } });
      expect(native.started).not.toBeNull();
      expect(transport.counters().records).toBe(0);
      core.close();
      expect(native.shutdowns).toBe(1);
    } finally {
      delete g.__undraNative;
    }
  });

  test("turns a start code into a handshake error and detaches", async () => {
    const native = new FakeNative();
    native.startCode = NativeStartCode.Busy;
    const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash });
    const failure = transport.start({} as never);
    await expect(failure).rejects.toBeInstanceOf(UndraTransportError);
    await expect(failure).rejects.toThrow(/already running in this process/);
    expect(native.sink).toBeUndefined();
    expect(native.portSync).toBeUndefined();
  });
});

describe("one module, several transports", () => {
  test("a second start that fails leaves the running core's sink and sync port in place", async () => {
    const { core, native } = await attach();
    const sink = native.sink;
    const portSync = native.portSync;
    native.startCode = NativeStartCode.AlreadyStarted;
    const second = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash });
    await expect(second.start({} as never)).rejects.toThrow(/already started/);
    second.close();
    expect(native.sink).toBe(sink);
    expect(native.portSync).toBe(portSync);
    expect(native.shutdowns).toBe(0);
    // The running core still hears what its core says.
    const applied = fakeStore(core, 9n);
    native.queue(RecordKind.ChangeSet, changeSet(1n, 9n, 0, 7), "core");
    await arrived(() => expect(applied).toEqual([[0, 7]]));
  });

  test("a sync call or a snapshot after this runtime's core was stopped under it is a typed 'closed' error", async () => {
    const { native, transport } = await attach();
    native.coreGone = true;
    expect(() => transport.callSync(new Uint8Array(17))).toThrow(expect.objectContaining({ reason: "closed" }));
    await expect(transport.snapshot()).rejects.toBeInstanceOf(UndraTransportError);
    await expect(transport.restore(new Uint8Array(8))).rejects.toBeInstanceOf(UndraRestoreError);
  });
});

describe("calls", () => {
  test("a reply queued on the JS thread settles the call, after the call's change-sets are applied", async () => {
    const { core, native } = await attach();
    const applied = fakeStore(core, 7n);
    native.onCall = (payload) => {
      native.queue(RecordKind.ChangeSet, changeSet(1n, 7n, 0, 41), "js");
      native.queue(RecordKind.Reply, reply(callIdOf(payload), ReplyStatus.Ok, le([42, "u32"])), "js");
      return 0;
    };
    const body = await core.call({ target: CallTarget.ObjectMethod, handle: 7n }, 0x99, new Uint8Array(0));
    expect(decodeValue(codecs.u32, body)).toBe(42);
    // Read-your-writes: the mirror applied the change-set before the continuation ran.
    expect(applied).toEqual([[0, 41]]);
  });

  test("callSync returns the reply and applies the change-sets before it returns", async () => {
    const { core, native } = await attach();
    const applied = fakeStore(core, 9n);
    native.onCallSync = (payload) => {
      native.queue(RecordKind.ChangeSet, changeSet(1n, 9n, 2, 5), "js");
      return reply(callIdOf(payload), ReplyStatus.Ok, le([3, "u32"]));
    };
    const body = core.callSync(CallTarget.FreeFunction, 0x1, new Uint8Array(0));
    expect(decodeValue(codecs.u32, body)).toBe(3);
    expect(applied).toEqual([[2, 5]]);
  });

  test("a refused call rejects with status 5", async () => {
    const { core, native } = await attach();
    native.onCall = () => 5;
    await expect(core.call(CallTarget.FreeFunction, 1, new Uint8Array(0))).rejects.toMatchObject({ status: ReplyStatus.BadRequest });
  });

  test("a reply from a core thread arrives in a later batch", async () => {
    const { core, native } = await attach();
    let id = 0;
    native.onCall = (payload) => {
      id = callIdOf(payload);
      return 0;
    };
    const pending = core.call(CallTarget.FreeFunction, 1, new Uint8Array(0));
    native.queue(RecordKind.Reply, reply(id, ReplyStatus.Error, le([7, "u32"])), "core");
    await expect(pending).rejects.toBeInstanceOf(UndraReplyError);
    expect(native.hostCounters().wakes).toBe(1);
  });

  test("observe resolves with the initial change-set already applied", async () => {
    const { core, native } = await attach();
    const applied = fakeStore(core, 0x1_0000_0002n);
    native.onObserve = (handle, signalId, on) => {
      expect([handle, signalId, on]).toEqual([0x1_0000_0002n, 0xffffffff, true]);
      native.queue(RecordKind.ChangeSet, changeSet(3n, handle, 1, 77), "js");
    };
    const observed = core.observe(0x1_0000_0002n, 0xffffffff, true);
    expect(applied).toEqual([[1, 77]]);
    await observed;
  });

  test("one inbox keeps commit order across threads", async () => {
    const { core, native } = await attach();
    const applied = fakeStore(core, 5n);
    // txn 1 committed on a core thread is still queued when a JS call commits txn 2.
    native.queue(RecordKind.ChangeSet, changeSet(1n, 5n, 0, 100), "core");
    native.onCall = (payload) => {
      native.queue(RecordKind.ChangeSet, changeSet(2n, 5n, 0, 200), "js");
      native.queue(RecordKind.Reply, reply(callIdOf(payload), ReplyStatus.Ok), "js");
      return 0;
    };
    await core.call(CallTarget.FreeFunction, 1, new Uint8Array(0));
    // Merged per drain (docs/SPEC.md section 11.1): the last value of the commit order wins.
    expect(applied.at(-1)).toEqual([0, 200]);
    // The drain txn 1's core thread posted runs later and finds nothing left: the order still holds once it has run.
    await arrived(() => expect(native.drainPosted).toBe(false));
    expect(applied.at(-1)).toEqual([0, 200]);
  });

  test("cancel, credit, release, events and timers go to their entries", async () => {
    const { core, native } = await attach();
    const abort = new AbortController();
    let id = 0;
    native.onCall = (payload) => {
      id = callIdOf(payload);
      return 0;
    };
    const pending = core.call(CallTarget.FreeFunction, 1, new Uint8Array(0), abort.signal);
    abort.abort();
    await expect(pending).rejects.toThrow();
    core.release(0x2_0000_0001n);
    core.event(10, 11, new Uint8Array([1, 2, 3]));
    core.timerFired(4);
    expect(native.log).toEqual(["start", `cancel ${id}`, "release 2:1", "event 10 11 3", "timer 4"]);
  });
});

describe("streams (ADR-036)", () => {
  /** A `StreamItem` payload: call id, flag, body. */
  function streamItem(callId: number, flag: number, body: Uint8Array = new Uint8Array(0)): Uint8Array {
    return new Uint8Array([...le([callId, "u32"], [flag, "u8"]), ...body]);
  }

  /** Opens a stream whose core sends one item and then `last`; returns what the loop read and how it ended. */
  async function run(last: (callId: number) => Uint8Array): Promise<{ items: number[]; error: unknown }> {
    const { core, native } = await attach();
    native.onCall = (payload) => {
      const id = callIdOf(payload);
      native.queue(RecordKind.Reply, reply(id, ReplyStatus.StreamOpened), "core");
      native.queue(RecordKind.StreamItem, streamItem(id, StreamFlag.Item, le([1, "u32"])), "core");
      native.queue(RecordKind.StreamItem, last(id), "core");
      return 0;
    };
    const items: number[] = [];
    try {
      for await (const body of core.stream(CallTarget.FreeFunction, 1, new Uint8Array(0))) items.push(decodeValue(codecs.u32, body));
    } catch (error) {
      return { items, error };
    }
    return { items, error: undefined };
  }

  test("an end item finishes the loop", async () => {
    expect(await run((id) => streamItem(id, StreamFlag.End))).toEqual({ items: [1], error: undefined });
  });

  test("flag 2 carries the stream's own E, untouched", async () => {
    const e = new Uint8Array([9, 8, 7]);
    const { items, error } = await run((id) => streamItem(id, StreamFlag.Error, e));
    expect(items).toEqual([1]);
    expect(error).toBeInstanceOf(UndraReplyError);
    expect((error as UndraReplyError).status).toBe(ReplyStatus.Error);
    expect((error as UndraReplyError).body).toEqual(e);
  });

  test("flag 3 ends the loop as the failed reply of its status, mapped to the same UndraCallError as a call", async () => {
    const failed = (status: ReplyStatus.Panic | ReplyStatus.Cancelled | ReplyStatus.BadRequest, message: string, detail = ""): ((id: number) => Uint8Array) =>
      (id) => streamItem(id, StreamFlag.Failed, encodeStreamFailure({ status, message, detail }));
    const cancelled = await run(failed(ReplyStatus.Cancelled, "the runtime shut down"));
    expect((cancelled.error as UndraReplyError).status).toBe(ReplyStatus.Cancelled);
    expect(UndraCallError.mappedStream(cancelled.error)).toBeInstanceOf(UndraCallError.CancelledByCore);

    const panicked = await run(failed(ReplyStatus.Panic, "boom", "at core.rs:1"));
    const panic = UndraCallError.mappedStream(panicked.error) as UndraCallError.Panicked;
    expect(panic).toBeInstanceOf(UndraCallError.Panicked);
    expect([panic.panicMessage, panic.backtrace]).toEqual(["boom", "at core.rs:1"]);

    const refused = await run(failed(ReplyStatus.BadRequest, "stale handle"));
    expect((refused.error as UndraReplyError).status).toBe(ReplyStatus.BadRequest);
    const refusal = UndraCallError.mappedStream(refused.error) as UndraCallError.Refused;
    expect(refusal).toBeInstanceOf(UndraCallError.Refused);
    expect(refusal.reason).toBe("stale handle");
    expect(refused.items).toEqual([1]);
  });

  test("a flag-3 body that does not decode is a protocol error, mapped to Malformed", async () => {
    const { error } = await run((id) => streamItem(id, StreamFlag.Failed, new Uint8Array([1])));
    expect(error).toBeInstanceOf(UndraTransportError);
    expect((error as UndraTransportError).reason).toBe("protocol");
    expect(UndraCallError.mappedStream(error)).toBeInstanceOf(UndraCallError.Malformed);
  });
});

describe("ports", () => {
  test("an async port call is answered through the registered implementation", async () => {
    const { core, native } = await attach();
    core.registerPort(0x1ebeb908, { sync: false, methods: { 0x1ab1bc95: async (args) => new Uint8Array([args.length]) } });
    native.queue(RecordKind.PortCall, new Uint8Array([...le([0x1ebeb908, "u32"], [0x1ab1bc95, "u32"], [12, "u32"]), 9, 9]), "core");
    await arrived(() => expect(native.portReplies).toEqual([new Uint8Array([...le([12, "u32"], [PortStatus.Ok, "u8"]), 2])]));
  });

  test("a port nobody implements is answered unavailable", async () => {
    const { native } = await attach();
    native.queue(RecordKind.PortCall, le([0xbad, "u32"], [1, "u32"], [13, "u32"]), "core");
    await arrived(() => expect(native.portReplies).toEqual([le([13, "u32"], [PortStatus.Unavailable, "u8"])]));
  });

  test("a JS sync port answers on the JS thread with a complete PortReply", async () => {
    const { core, native } = await attach();
    core.registerPort(0x7e57, { sync: true, methods: { 0x7e58: () => new Uint8Array([4, 5]) } });
    const answer = native.callJsSyncPort(0x7e57, 0x7e58, 21, new Uint8Array(0));
    expect(answer).toBeInstanceOf(ArrayBuffer);
    expect(new Uint8Array(answer as ArrayBuffer)).toEqual(new Uint8Array([...le([21, "u32"], [0, "u8"]), 4, 5]));
    expect(native.callJsSyncPort(0x7e57, 0x1, 22, new Uint8Array(0))).toBe(2);
  });

  test("log records reach the Log adapter", async () => {
    const { native, logs } = await attach();
    const w = new UndraWriter(32);
    w.writeU8(3);
    w.writeStr("undra::react-native");
    w.writeStr("hello");
    native.queue(RecordKind.Log, w.finish(), "core");
    await arrived(() => expect(logs).toEqual([[3, "undra::react-native", "hello"]]));
  });
});

describe("robustness", () => {
  test("a malformed batch is reported, never thrown into the module", async () => {
    const { native, errors } = await attach();
    expect(() => native.sink!(new Uint8Array([2, 9, 0, 0, 0, 1]).buffer)).not.toThrow();
    expect(() => native.sink!(new Uint8Array([2, 0]).buffer)).not.toThrow();
    expect(() => native.sink!(new Uint8Array([99, 0, 0, 0, 0]).buffer)).not.toThrow();
    expect(errors).toHaveLength(3);
  });

  test("close shuts the core down once and detaches", async () => {
    const { core, native, transport } = await attach();
    core.close();
    core.close();
    expect(native.shutdowns).toBe(1);
    expect(native.sink).toBeUndefined();
    expect(() => transport.send(0 as never, new Uint8Array(0))).toThrow(UndraTransportError);
    await expect(core.call(CallTarget.FreeFunction, 1, new Uint8Array(0))).rejects.toBeInstanceOf(UndraTransportError);
  });

  test("snapshot and restore are the transport's own (UndraCore.snapshot and restore reach them)", async () => {
    const { core, native, transport } = await attach();
    const bytes = await core.snapshot();
    expect(bytes.length).toBe(8);
    await core.restore(bytes);
    expect(native.restored).toEqual(bytes);
    // A core that refuses the bytes is a typed, coded error, on both paths, as on the wasm transports.
    native.restoreCode = 5;
    await expect(core.restore(bytes)).rejects.toMatchObject({ name: "UndraRestoreError", code: 5 });
    expect(() => transport.send(Kind.Restore, bytes)).toThrow(UndraRestoreError);
    native.restoreCode = 0;
    await core.restore(bytes);
    core.close();
    await expect(transport.snapshot()).rejects.toBeInstanceOf(UndraTransportError);
    await expect(transport.restore(bytes)).rejects.toBeInstanceOf(UndraTransportError);
  });

  test("stats read the core's JSON and the host counters", async () => {
    const { core, transport } = await attach();
    const stats = await core.stats();
    expect(stats.liveHandles).toBe(3);
    expect(transport.counters().records).toBeGreaterThanOrEqual(0);
    expect((await transport.snapshot()).length).toBe(8);
  });

  test("a sink that throws inside a handler keeps delivering the rest of the batch", async () => {
    const { core, native, errors } = await attach();
    const seen: number[] = [];
    core.mirror.register(1n, () => {
      throw new Error("apply failed");
    });
    core.mirror.register(2n, (_s, _op, value) => seen.push(decodeValue(codecs.u32, value)));
    native.queue(RecordKind.ChangeSet, changeSet(1n, 1n, 0, 1), "core");
    native.queue(RecordKind.ChangeSet, changeSet(2n, 2n, 0, 2), "core");
    await arrived(() => {
      expect(seen).toEqual([2]);
      expect(errors.length).toBeGreaterThan(0);
    });
  });
});

describe("portPlan", () => {
  test("reads the ports and their synchronous methods from the schema", () => {
    const plan = portPlan(
      JSON.stringify({
        ports: [
          { port_id: 1, kind: "async", methods: [{ method_id: 10, is_async: true }, { method_id: 11, is_async: false }] },
          { port_id: 2, kind: "event", methods: [{ method_id: 20, is_async: false }] },
        ],
      }),
    );
    expect(plan).toEqual({ ports: [1], syncMethods: [1, 11] });
    expect(portPlan("{}")).toEqual({ ports: [], syncMethods: [] });
  });
});

test("an encoded value round-trips through the fake's callSync", async () => {
  const { core, native } = await attach();
  native.onCallSync = (payload) => {
    const r = new UndraReader(payload.subarray(17));
    return reply(callIdOf(payload), 0, encodeValue(codecs.string, r.readStr().toUpperCase()));
  };
  const out = core.callSync(CallTarget.FreeFunction, 1, encodeValue(codecs.string, "héllo"));
  expect(decodeValue(codecs.string, out)).toBe("HÉLLO");
});
