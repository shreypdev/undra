import type { NativeHostCounters, NativePlatformDefaults, UndraNativeModule } from "../../src/native.js";
import { RecordKind } from "../../src/native.js";

/** The namespace of a {@link FakeNative} unless it is given another. */
export const FAKE_NAMESPACE = "fake_core";

/** Where a fake record comes from: the JS thread inside a host function, or a core thread. */
export type Thread = "js" | "core";

/** A fake `__undraNative` with the module's inbox rules (ADR-038, decision 4) and a scripted core. */
export class FakeNative implements UndraNativeModule {
  sink?: ((batch: ArrayBuffer) => void) | undefined;
  portSync?: ((portId: number, methodId: number, portCallId: number, args: ArrayBuffer) => ArrayBuffer | number) | undefined;
  frame?: (() => void) | undefined;

  /** The core's namespace (a real module reports its table's). */
  namespace: string;
  abi = 2;
  hash = 0x1234_5678_9abc_def0n;
  schema: unknown = {
    ports: [
      { port_id: 0xcd99c48e, kind: "sync", methods: [{ method_id: 0xccc94d90, is_async: false }] },
      { port_id: 0x1ebeb908, kind: "async", methods: [{ method_id: 0x1ab1bc95, is_async: true }] },
      { port_id: 0x1feff6ff, kind: "event", methods: [{ method_id: 1, is_async: false }] },
      { port_id: 0x7e57, kind: "sync", methods: [{ method_id: 0x7e58, is_async: false }] },
    ],
  };
  startCode = 0;
  frames = true;
  /** As the module answers when this runtime's core was stopped under it (a reload's takeover). */
  coreGone = false;
  /** What `restore` answers while this runtime's core runs: 0, or a `restore_code` (5 malformed, 2 a store panicked). */
  restoreCode = 0;
  /** The bytes the last `restore` was given. */
  restored: Uint8Array | undefined;
  /** What `platformDefaults()` reports: the standard ports this fake platform answers natively (none by default). */
  defaults: NativePlatformDefaults = { ports: [] };

  /** What the scripted core does with a `Call` payload; returns the `undra_call` code. */
  onCall: (payload: Uint8Array) => number = () => 0;
  /** What it answers to `callSync`. */
  onCallSync: (payload: Uint8Array) => Uint8Array = () => new Uint8Array([0, 0, 0, 0, 5]);
  /** What it does on `observe`. */
  onObserve: (handle: bigint, signalId: number, on: boolean) => void = () => {};

  readonly log: string[] = [];
  readonly portReplies: Uint8Array[] = [];
  started: { config: Uint8Array; ports: number[]; syncMethods: number[]; nativePorts: number[] } | null = null;
  shutdowns = 0;
  frameRequests = 0;
  batches = 0;

  #inbox: Array<{ kind: number; payload: Uint8Array }> = [];
  #depth = 0;

  /** @param namespace The core's namespace; default {@link FAKE_NAMESPACE}. */
  constructor(namespace: string = FAKE_NAMESPACE) {
    this.namespace = namespace;
  }
  #draining = false;
  #drainPending = false;
  #records = 0;
  #wakes = 0;

  /** Queues a record the way a core callback does; one from a core thread posts a drain. */
  queue(kind: number, payload: Uint8Array, thread: Thread): void {
    this.#inbox.push({ kind, payload: payload.slice() });
    this.#records++;
    if (thread === "core" && !this.#drainPending) {
      this.#drainPending = true;
      this.#wakes++;
      setTimeout(() => {
        this.#drain();
      }, 0);
    }
  }

  /** Runs `fn` as a host function that enters the core, then drains as the module does. */
  #enter<T>(fn: () => T): T {
    this.#depth++;
    try {
      return fn();
    } finally {
      this.#depth--;
      if (this.#depth === 0) this.#drain();
    }
  }

  get insideHostFunction(): boolean {
    return this.#depth > 0;
  }

  /** Whether a drain posted by a record from a core thread has yet to run (what a test waits on before it reads the result). */
  get drainPosted(): boolean {
    return this.#drainPending;
  }

  #drain(): void {
    this.#drainPending = false;
    if (this.#draining || this.#depth > 0) return;
    this.#draining = true;
    try {
      while (this.#inbox.length > 0) {
        const records = this.#inbox.splice(0);
        let size = 0;
        for (const r of records) size += 5 + r.payload.length;
        const batch = new Uint8Array(size);
        const view = new DataView(batch.buffer);
        let at = 0;
        for (const r of records) {
          batch[at] = r.kind;
          view.setUint32(at + 1, r.payload.length, true);
          batch.set(r.payload, at + 5);
          at += 5 + r.payload.length;
        }
        if (this.sink === undefined) return;
        this.batches++;
        this.sink(batch.buffer);
      }
    } finally {
      this.#draining = false;
    }
  }

  abiVersion(): number {
    return this.abi;
  }
  schemaHash(): bigint {
    return this.hash;
  }
  schemaJson(): string {
    return JSON.stringify(this.schema);
  }
  start(
    config: ArrayBuffer,
    byteOffset: number,
    byteLength: number,
    ports: readonly number[],
    syncMethods: readonly number[],
    nativePorts: readonly number[] = [],
  ): number {
    this.log.push("start");
    if (this.startCode !== 0) return this.startCode;
    this.started = {
      config: new Uint8Array(config, byteOffset, byteLength).slice(),
      ports: [...ports],
      syncMethods: [...syncMethods],
      nativePorts: [...nativePorts],
    };
    return 0;
  }
  platformDefaults(): NativePlatformDefaults {
    return this.defaults;
  }
  shutdown(): void {
    this.shutdowns++;
    this.log.push("shutdown");
  }
  call(buffer: ArrayBuffer, byteOffset: number, byteLength: number): number {
    return this.#enter(() => this.onCall(new Uint8Array(buffer, byteOffset, byteLength)));
  }
  callSync(buffer: ArrayBuffer, byteOffset: number, byteLength: number): ArrayBuffer | undefined {
    if (this.coreGone) return undefined;
    return this.#enter(() => this.onCallSync(new Uint8Array(buffer, byteOffset, byteLength)).slice().buffer);
  }
  cancel(callId: number): void {
    this.#enter(() => this.log.push(`cancel ${callId}`));
  }
  streamCredit(callId: number, credit: number): void {
    this.#enter(() => this.log.push(`credit ${callId} ${credit}`));
  }
  observe(handleLo: number, handleHi: number, signalId: number, on: boolean): void {
    this.#enter(() => {
      this.onObserve((BigInt(handleHi) << 32n) | BigInt(handleLo), signalId, on);
    });
  }
  release(handleLo: number, handleHi: number): void {
    this.#enter(() => this.log.push(`release ${handleHi}:${handleLo}`));
  }
  portReply(buffer: ArrayBuffer, byteOffset: number, byteLength: number): void {
    this.#enter(() => this.portReplies.push(new Uint8Array(buffer, byteOffset, byteLength).slice()));
  }
  event(portId: number, methodId: number, buffer: ArrayBuffer, byteOffset: number, byteLength: number): void {
    this.#enter(() => this.log.push(`event ${portId} ${methodId} ${byteLength}`));
    void buffer;
    void byteOffset;
  }
  timerFired(timerId: number): void {
    this.#enter(() => this.log.push(`timer ${timerId}`));
  }
  snapshot(): ArrayBuffer | undefined {
    if (this.coreGone) return undefined;
    return new Uint8Array([0, 0, 0, 0, 0, 0, 0, 0]).buffer;
  }
  restore(buffer: ArrayBuffer, byteOffset: number, byteLength: number): number {
    if (this.coreGone) return 6;
    this.restored = new Uint8Array(buffer, byteOffset, byteLength).slice();
    return this.#enter(() => this.restoreCode);
  }
  statsJson(): string {
    return JSON.stringify({ live_handles: 3 });
  }
  hostCounters(): NativeHostCounters {
    return {
      records: this.#records,
      bytes: 0,
      wakes: this.#wakes,
      dropped: 0,
      nativePortCalls: 0,
      jsSyncPortCalls: 0,
      unavailableSyncPortCalls: 0,
    };
  }
  requestFrame(): boolean {
    this.frameRequests++;
    return this.frames;
  }

  /** Calls the transport's sync port function as the module does, inside a host function. */
  callJsSyncPort(portId: number, methodId: number, portCallId: number, args: Uint8Array): ArrayBuffer | number {
    return this.#enter(() => (this.portSync === undefined ? 2 : this.portSync(portId, methodId, portCallId, args.slice().buffer)));
  }
}

export { RecordKind };

/** Little-endian bytes. */
export function le(...parts: Array<[number, "u8" | "u32"] | [bigint, "u64"]>): Uint8Array {
  const out: number[] = [];
  for (const [value, kind] of parts) {
    const width = kind === "u8" ? 1 : kind === "u32" ? 4 : 8;
    let v = BigInt(value);
    for (let i = 0; i < width; i++) {
      out.push(Number(v & 0xffn));
      v >>= 8n;
    }
  }
  return new Uint8Array(out);
}
