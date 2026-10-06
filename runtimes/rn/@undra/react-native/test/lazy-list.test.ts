import { describe, expect, test, vi } from "vitest";
import {
  CallTarget,
  ChangeOp,
  LazyList,
  ReplyStatus,
  UndraCore,
  UndraReader,
  codecs,
  decodeCall,
  encodeChangeSet,
  encodeLazyInvalidated,
  encodeLazyPage,
  encodeLazyValue,
  encodeReply,
} from "@undra/runtime";
import { RecordKind } from "../src/native.js";
import { NativeTransport } from "../src/transport.js";
import { FakeNative } from "./support/fake-native.js";

/*
 * What a `Lazy<T>` list needs of React Native (ADR-043 decision 3.5): nothing of its own. The page call is an
 * ordinary `Call` payload (target 3), `NativeTransport` is `synchronous`, so `UndraCore.callSync` hands the payload to
 * the module's `callSync` (`undra_call_sync`, which does not look at the target) and the runtime's `LazyList` answers
 * from the reply: the same class, the same calls as on `wasm-main`. The change-sets (the value, op 2) arrive through
 * the inbox like any other. These tests drive a `LazyList` through `NativeTransport` and the module's fake.
 */

const HANDLE = 0x0001_0000_0007n;
const STORE = 9n;
const SIGNAL = 3;

/** A scripted core: 1,000 rows `10 * i`, a version, and the page calls it has answered. */
class PageServer {
  version = 1n;
  rows: number[] = Array.from({ length: 1000 }, (_, i) => i * 10);
  readonly calls: Array<{ handle: bigint; offset: number; limit: number }> = [];

  constructor(readonly native: FakeNative) {
    native.onCallSync = (payload) => {
      const call = decodeCall(payload);
      if (call.target !== CallTarget.LazyListPage) return encodeReply({ callId: call.callId, status: ReplyStatus.BadRequest, reason: "not a page call" });
      this.calls.push({ handle: call.handle, offset: call.offset, limit: call.limit });
      const items = this.rows.slice(call.offset, call.offset + call.limit);
      const body = encodeLazyPage(codecs.i32, { version: this.version, total: this.rows.length, items });
      return encodeReply({ callId: call.callId, status: ReplyStatus.Ok, body });
    };
  }

  /** A change-set with one entry of the list's signal. */
  send(op: ChangeOp, value: Uint8Array, thread: "js" | "core"): void {
    const payload = encodeChangeSet({ txnId: this.version, entries: [{ handle: STORE, signalId: SIGNAL, op, value }] });
    this.native.queue(RecordKind.ChangeSet, payload, thread);
  }

  value(): Uint8Array {
    return encodeLazyValue({ handle: HANDLE, len: this.rows.length, version: this.version });
  }

  /** The core commits an edit of row `at` and tells the host with op 2, from a core thread. */
  edit(at: number, value: number): void {
    this.rows[at] = value;
    this.version++;
    this.send(ChangeOp.LazyInvalidated, encodeLazyInvalidated({ len: this.rows.length, version: this.version }), "core");
  }
}

async function attach(): Promise<{ core: UndraCore; native: FakeNative; server: PageServer; list: LazyList<number>; errors: unknown[] }> {
  const native = new FakeNative();
  const errors: unknown[] = [];
  const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: native.hash, onError: (e) => errors.push(e) });
  const core = await UndraCore.attach(transport, {
    expectedSchemaHash: native.hash,
    shared: false,
    adapters: { log: { log() {} }, http: null, connectivity: null, lifecycle: null },
    onError: (e) => errors.push(e),
    mirror: { schedule: (fn) => setTimeout(fn, 0) },
  });
  const server = new PageServer(native);
  const list = new LazyList<number>(core, codecs.i32);
  // What a generated store's `_apply` does for the signal.
  core.mirror.register(STORE, (signalId, op, value) => {
    if (signalId !== SIGNAL) return;
    if (op === ChangeOp.FullValue) list.applyFull(new UndraReader(value));
    else if (op === ChangeOp.LazyInvalidated) list.applyInvalidated(new UndraReader(value));
  });
  // The observation answers with the signal's value, as the core does.
  native.onObserve = () => server.send(ChangeOp.FullValue, server.value(), "js");
  await core.observe(STORE, 0xffff_ffff, true);
  return { core, native, server, list, errors };
}

const tick = (ms = 0): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));
/**
 * Waits until what a change-set from a core thread causes has happened, then the check holds. That delivery is a chain of
 * timers (the fake's drain, then the mirror's `schedule`), each a timeout the event loop may run late under load, and a
 * fixed wait of 5 ms ran out before the chain did (an empty `server.calls`). The check says what arrives, not when; 10 s
 * only catches a delivery that never comes (the same rule as `arrived` in transport.test.ts).
 */
const arrived = (check: () => void): Promise<void> => vi.waitFor(check, { timeout: 10_000, interval: 5 });

describe("a LazyList over the native transport", () => {
  test("takes its length from the observed value and reads rows through module.callSync, in one batch", async () => {
    const { core, list, server, errors } = await attach();
    expect(core.mode).toBe("native");
    expect(list.length.peek()).toBe(1000);
    expect(list.get(75)).toBeUndefined();
    await tick();
    expect(server.calls).toEqual([
      { handle: HANDLE, offset: 0, limit: 50 },
      { handle: HANDLE, offset: 50, limit: 50 },
      { handle: HANDLE, offset: 100, limit: 50 },
    ]);
    expect(list.get(75)).toBe(750);
    expect(list.get(0)).toBe(0);
    expect(list.get(149)).toBe(1490);
    expect(errors).toEqual([]);
  });

  test("an op 2 from a core thread takes the length at once and re-pages only the window", async () => {
    const { list, server } = await attach();
    list.get(75);
    await tick();
    server.calls.length = 0;
    const before = list.revision.peek(); // armed beside the edit: the revision changes when the re-paged window arrives
    server.edit(75, -1);
    await arrived(() => expect(list.revision.peek()).toBeGreaterThan(before));
    expect(server.calls).toEqual([{ handle: HANDLE, offset: 50, limit: 50 }]);
    expect(list.get(75)).toBe(-1);
    expect(list.get(76)).toBe(760);
  });

  test("a page call the module refuses (its page server is gone) is quiet; one that panics is reported; neither is thrown into get", async () => {
    const { list, native, errors } = await attach();
    native.onCallSync = (payload) => encodeReply({ callId: decodeCall(payload).callId, status: ReplyStatus.BadRequest, reason: "stale handle" });
    expect(() => list.get(0)).not.toThrow();
    await tick();
    expect(errors).toEqual([]);
    expect(list.get(0)).toBeUndefined();
    native.onCallSync = (payload) => encodeReply({ callId: decodeCall(payload).callId, status: ReplyStatus.Panic, message: "boom", backtrace: "" });
    list.applyFull(new UndraReader(encodeLazyValue({ handle: HANDLE, len: 1000, version: 9n }))); // a value revives the list
    expect(() => list.get(0)).not.toThrow();
    await tick();
    expect(errors.length).toBeGreaterThan(0);
    expect(list.get(0)).toBeUndefined();
  });

  test("a core that was stopped under this runtime reports 'closed' instead of throwing", async () => {
    const { list, native, errors } = await attach();
    native.coreGone = true;
    expect(() => list.get(0)).not.toThrow();
    await tick();
    expect(errors.length).toBeGreaterThan(0);
  });

  test("the page call is the 21-byte target-3 payload", async () => {
    const { list, native } = await attach();
    const seen: Uint8Array[] = [];
    const answer = native.onCallSync;
    native.onCallSync = (payload) => {
      seen.push(payload.slice());
      return answer(payload);
    };
    list.get(0);
    await tick();
    expect(seen.length).toBe(2);
    for (const payload of seen) {
      expect(payload.length).toBe(21);
      expect(payload[0]).toBe(CallTarget.LazyListPage);
    }
  });
});
