import { describe, expect, test, vi } from "vitest";
import {
  PortStatus,
  ReplyStatus,
  UndraCallError,
  UndraCore,
  UndraReader,
  UndraWriter,
  adopt,
  adoptObject,
  callbacks,
  codecs,
  decodeValue,
  encodeValue,
} from "@undra/runtime";
import { type Reporter, Shelf, UndraIds, Watch, Workshop } from "@playground/core";
import { RecordKind, portPlan } from "../src/native.js";
import { NativeTransport } from "../src/transport.js";
import { FakeNative, le } from "./support/fake-native.js";
import { eventually, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

// Objects (ADR-040) and host callbacks (ADR-041) through @undra/react-native's JavaScript, over the stand-in of the
// native module: handles are opaque to the module (two 32-bit halves, whatever the 24/40 layout), and a callback
// port is registered like any other non-event port, its calls queued for JavaScript (the `main` delivery path),
// fire-and-forget ones (port call id 0) never answered.

const reporter = UndraIds.Callbacks.Reporter;

/** A `Reply` payload. */
function reply(callId: number, status: number, body: Uint8Array = new Uint8Array(0)): Uint8Array {
  return new Uint8Array([...le([callId, "u32"], [status, "u8"]), ...body]);
}

/** The call id and arguments of a `Call` payload to an object method (target 1). */
function methodCall(payload: Uint8Array): { callId: number; methodId: number; handle: bigint; args: Uint8Array } {
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  return {
    handle: view.getBigUint64(1, true),
    methodId: view.getUint32(9, true),
    callId: view.getUint32(13, true),
    args: payload.subarray(17),
  };
}

async function attach(native = new FakeNative()): Promise<{ core: UndraCore; native: FakeNative; errors: unknown[] }> {
  native.schema = {
    ports: [
      { port_id: reporter.portId, kind: "callback", methods: [
        { method_id: reporter.confirm, is_async: true },
        { method_id: reporter.note, is_async: false },
        { method_id: reporter.progress, is_async: false },
      ] },
    ],
  };
  const errors: unknown[] = [];
  const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash, platform: "test" });
  const core = await UndraCore.attach(transport, {
    expectedSchemaHash: native.hash,
    shared: false,
    adapters: { log: { log() {} }, http: null, connectivity: null, lifecycle: null },
    onError: (e) => errors.push(e),
    mirror: { schedule: (fn) => setTimeout(fn, 0) },
  });
  return { core, native, errors };
}

const handle = (index: number, generation: number): bigint => (BigInt(generation) << 24n) | BigInt(index);

describe("objects through the native module", () => {
  test("a returned object is one wrapper per handle; the duplicate reference goes back; parameters carry the handle", async () => {
    const { core, native } = await attach();
    const workshop = adopt(core, handle(1, 1), Workshop);
    const shelf = handle(2, 0x12_3456_789a); // a 40-bit generation: the module splits it, opaque
    const calls: Array<ReturnType<typeof methodCall>> = [];
    native.onCall = (payload) => {
      const call = methodCall(payload);
      calls.push(call);
      const body = call.methodId === UndraIds.Objects.Workshop.find ? encodeValue(codecs.option(codecs.u64), shelf) : new Uint8Array(0);
      native.queue(RecordKind.Reply, reply(call.callId, ReplyStatus.Ok, body), "js");
      return 0;
    };
    native.onObserve = (h) => {
      // The initial values of the shelf: label and items.
      const w = new UndraWriter();
      w.writeU64(1n);
      w.writeU32(2);
      for (const [signalId, value] of [[0, encodeValue(codecs.string, "a")], [1, encodeValue(codecs.u32, 3)]] as const) {
        w.writeU64(h);
        w.writeU32(signalId);
        w.writeU8(0);
        w.writeBytes(value);
      }
      native.queue(RecordKind.ChangeSet, w.finish(), "js");
    };
    const found = await workshop.find("a");
    expect(found).toBeInstanceOf(Shelf);
    expect(found?.handle).toBe(shelf);
    expect(found?.label.peek()).toBe("a");
    expect(await workshop.find("a")).toBe(found);
    const half = (h: bigint): string => `release ${Number(h >> 32n)}:${Number(h & 0xffff_ffffn)}`;
    expect(native.log.filter((l) => l.startsWith("release")), "the duplicate's reference, by its two halves").toEqual([half(shelf)]);
    await workshop.merge(found as Shelf, found as Shelf);
    const merge = calls.at(-1);
    expect(merge?.methodId).toBe(UndraIds.Objects.Workshop.merge);
    const r = new UndraReader(merge?.args ?? new Uint8Array(0));
    expect([r.readU64(), r.readU64()]).toEqual([shelf, shelf]);
  });

  test("a call aborted after the core answered gives the reply's reference back (objects-followups O7)", async () => {
    const { core, native } = await attach();
    const workshop = adopt(core, handle(1, 1), Workshop);
    const shelf = handle(2, 0x12_3456_789a);
    const half = (h: bigint): string => `release ${Number(h >> 32n)}:${Number(h & 0xffff_ffffn)}`;
    let answer: (() => void) | undefined;
    native.onCall = (payload) => {
      const call = methodCall(payload);
      // The module answers later: by the time the abort reaches it, the success reply is already on its way.
      answer = () => native.queue(RecordKind.Reply, reply(call.callId, ReplyStatus.Ok, encodeValue(codecs.u64, shelf)), "core");
      return 0;
    };
    const controller = new AbortController();
    const opening = workshop.open("x", 0, controller.signal);
    const aborted = expect(opening).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    await aborted;
    expect(native.log.filter((l) => l.startsWith("cancel"))).toHaveLength(1);
    expect(native.log.filter((l) => l.startsWith("release"))).toEqual([]);
    answer?.();
    // The reply comes from a core thread: a drain away. Waited for by its effect, then counted.
    const releases = (): string[] => native.log.filter((l) => l.startsWith("release"));
    await eventually(() => releases().length > 0, "the release of the abandoned reply's reference");
    expect(releases(), "the abandoned reply's reference went back, once").toEqual([half(shelf)]);
  });

  test("an object of another core is refused before anything reaches the module", async () => {
    const one = await attach(new FakeNative("core_one"));
    const two = await attach(new FakeNative("core_two"));
    const workshop = adopt(one.core, handle(1, 1), Workshop);
    const foreign = await adoptObject(two.core, encodeValue(codecs.u64, handle(1, 1)), Watch);
    let sent = 0;
    one.native.onCall = () => {
      sent++;
      return 0;
    };
    const theirs = adopt(two.core, handle(3, 1), Shelf);
    await expect(workshop.total([theirs])).rejects.toBeInstanceOf(UndraCallError.Refused);
    await expect(workshop.describe(theirs)).rejects.toThrow(/Shelf belongs to another core/);
    expect(sent).toBe(0);
    expect(foreign.core).toBe(two.core);
  });
});

describe("host callbacks through the native module", () => {
  test("the port plan registers the callback port and answers none of its methods synchronously", () => {
    const plan = portPlan(
      JSON.stringify({ ports: [{ port_id: 9, kind: "callback", methods: [{ method_id: 1, is_async: false }, { method_id: 2, is_async: true }] }] }),
    );
    expect(plan).toEqual({ ports: [9], syncMethods: [] });
  });

  test("a callback round trip: lent, called back from a core thread, answered, released", async () => {
    const { core, native, errors } = await attach();
    expect(native.started?.ports).toContain(reporter.portId);
    expect(native.started?.syncMethods).toEqual([]);
    const workshop = adopt(core, handle(1, 1), Workshop);
    let instance = 0n;
    native.onCall = (payload) => {
      const call = methodCall(payload);
      instance = new UndraReader(call.args).readU64();
      native.queue(RecordKind.Reply, reply(call.callId, ReplyStatus.Ok, encodeValue(codecs.u64, handle(5, 1))), "js");
      return 0;
    };
    const heard: string[] = [];
    const rep: Reporter = {
      progress: () => {},
      note: (line) => heard.push(line),
      confirm: (question) => Promise.resolve(question === "go on?"),
    };
    const watch = await workshop.watch(rep);
    expect(watch).toBeInstanceOf(Watch);
    expect(instance).toBeGreaterThan(0n);
    expect(callbacks(core).count(rep)).toBe(1);

    const portCall = (methodId: number, portCallId: number, write: (w: UndraWriter) => void): Uint8Array => {
      const w = new UndraWriter();
      w.writeU64(instance);
      write(w);
      return new Uint8Array([...le([reporter.portId, "u32"], [methodId, "u32"], [portCallId, "u32"]), ...w.finish()]);
    };
    native.queue(RecordKind.PortCall, portCall(reporter.note, 0, (w) => w.writeStr("hello")), "core");
    native.queue(RecordKind.PortCall, portCall(reporter.confirm, 41, (w) => w.writeStr("go on?")), "core");
    // Both calls are handled once the second one's answer is there (they are drained in order).
    await eventually(() => native.portReplies.length > 0, "the answer to confirm");
    expect(heard).toEqual(["hello"]);
    expect(native.portReplies.length, "one answer: the async call's, never the fire-and-forget one's").toBe(1);
    const answer = new UndraReader(native.portReplies[0] ?? new Uint8Array(0));
    expect([answer.readU32(), answer.readU8()]).toEqual([41, PortStatus.Ok]);
    expect(decodeValue(codecs.bool, (native.portReplies[0] ?? new Uint8Array(0)).subarray(5))).toBe(true);

    native.queue(RecordKind.PortCall, portCall(reporter.releaseInstance, 0, () => {}), "core");
    await eventually(() => callbacks(core).count(rep) === 0, "the release of the lent callback");
    expect(errors).toEqual([]);
  });
});
