import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, test, vi } from "vitest";
import { type AttachOptions, type Transport, UndraCallError, UndraCore, UndraTransportError, UndraUnhandledError } from "@undra/runtime";
import { UndraPlaygroundCore, add } from "@playground/core";
import { type NativeCoreEntry, loadNative } from "../src/index.js";
import { lifecycleState } from "../src/adapters.js";
import { RecordKind } from "../src/native.js";
import { setAppState, setTurboModule } from "./support/react-native-stub.js";
import { FakeNative, le } from "./support/fake-native.js";
import { eventually, testTimeout } from "./support/wait.js";

vi.setConfig({ testTimeout });

const g = globalThis as { __undraNative?: Record<string, FakeNative> };
const opened: UndraCore[] = [];

afterEach(() => {
  for (const core of opened.splice(0)) core.close();
  delete g.__undraNative;
  setTurboModule("UndraNative", undefined);
  setAppState("active");
});

/** The TurboModule over fake cores: `install(namespace)` installs that core's fake, or throws as the module does. */
function linkCores(...cores: FakeNative[]): { installs: string[] } {
  const installs: string[] = [];
  setTurboModule("UndraNative", {
    install(namespace: string) {
      installs.push(namespace);
      const native = cores.find((core) => core.namespace === namespace);
      if (native === undefined) {
        throw new Error(`@undra/react-native: no Undra core \`${namespace}\` is linked into this app`);
      }
      g.__undraNative = { ...g.__undraNative, [namespace]: native };
      return true;
    },
  });
  return { installs };
}

/** An entry like a generated one (`Undra<Namespace>`), recording what it attached. */
function entryOf(native: FakeNative): NativeCoreEntry & { attached: number; core: UndraCore | null } {
  return {
    namespace: native.namespace,
    schemaHash: native.hash,
    attached: 0,
    core: null,
    async attach(transport: Transport, options?: Omit<AttachOptions, "expectedSchemaHash">): Promise<UndraCore> {
      this.attached++;
      // As the generated entry's `attach` does: its hash and its namespace.
      this.core = await UndraCore.attach(transport, { ...options, expectedSchemaHash: native.hash, namespace: native.namespace });
      return this.core;
    },
  };
}

function track(core: UndraCore): UndraCore {
  opened.push(core);
  return core;
}

describe("loadNative", () => {
  test("installs the core's module, attaches it through its entry and makes it shared", async () => {
    const native = new FakeNative();
    const { installs } = linkCores(native);
    const entry = entryOf(native);
    const core = track(await loadNative(entry, { adapters: { http: null } }));
    expect(installs).toEqual(["fake_core"]);
    expect(entry.attached).toBe(1);
    expect(entry.core).toBe(core);
    expect(core.mode).toBe("native");
    expect(UndraCore.shared).toBe(core);
    const r = native.started!.config;
    expect(new TextDecoder().decode(r.subarray(4, 4 + "react-native-test".length))).toBe("react-native-test");
    // A second load of the namespace while it is open is the same core; after close, a new one.
    expect(await loadNative(entry)).toBe(core);
    expect(entry.attached).toBe(1);
    core.close();
    const again = track(await loadNative(entry));
    expect(again).not.toBe(core);
    expect(entry.attached).toBe(2);
    expect(installs).toEqual(["fake_core"]);
  });

  test("the core knows its namespace, the entry's, which the default stores are kept under (ADR-044 amendment A)", async () => {
    const native = new FakeNative("core_ns");
    linkCores(native);
    // An entry without `attach`: loadNative attaches the transport itself, with the entry's namespace.
    const bare = track(await loadNative({ namespace: native.namespace, schemaHash: native.hash }, { adapters: { http: null } }));
    expect(bare.namespace).toBe("core_ns");
    bare.close();
    // An entry like a generated one fills its namespace in (`UndraIds.namespace`).
    const generated = track(await loadNative(entryOf(native), { adapters: { http: null } }));
    expect(generated.namespace).toBe("core_ns");
  });

  test("through the generated entry, the core is the bindings' default core", async () => {
    const native = new FakeNative(UndraPlaygroundCore.namespace);
    native.hash = UndraPlaygroundCore.schemaHash;
    // The generated `add` calls UndraPlaygroundCore.core; the fake core answers 42 on the JS thread.
    native.onCall = (payload) => {
      const callId = new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint32(13, true);
      native.queue(RecordKind.Reply, le([callId, "u32"], [0, "u8"], [42, "u32"]), "js");
      return 0;
    };
    linkCores(native);
    expect(UndraPlaygroundCore.core).toBe(UndraCore.unloaded);
    const core = track(await loadNative(UndraPlaygroundCore, { adapters: { http: null } }));
    expect(UndraPlaygroundCore.core).toBe(core);
    expect(await add(40, 2)).toBe(42);
    core.close();
    expect(UndraPlaygroundCore.core).toBe(UndraCore.unloaded);
  });

  test("two cores, two namespaces: each its own module, transport and core", async () => {
    const a = new FakeNative("core_a");
    const b = new FakeNative("core_b");
    b.hash = 0x0bn;
    const { installs } = linkCores(a, b);
    const [coreA, coreB] = await Promise.all([
      loadNative(entryOf(a), { adapters: { http: null } }),
      loadNative({ namespace: "core_b", schemaHash: 0x0bn }, { adapters: { http: null }, shared: false }),
    ]);
    track(coreA);
    track(coreB);
    expect(coreA).not.toBe(coreB);
    expect(installs.sort()).toEqual(["core_a", "core_b"]);
    expect(Object.keys(g.__undraNative ?? {}).sort()).toEqual(["core_a", "core_b"]);
    expect(a.started).not.toBeNull();
    expect(b.started).not.toBeNull();
    coreA.close();
    expect(a.shutdowns).toBe(1);
    expect(b.shutdowns).toBe(0);
    expect(coreB.closed).toBe(false);
    expect(await loadNative({ namespace: "core_b", schemaHash: 0x0bn })).toBe(coreB);
  });

  test("a core the app does not have is an UndraTransportError that says which", async () => {
    linkCores(new FakeNative());
    await expect(loadNative({ namespace: "no_such_core", schemaHash: 1n })).rejects.toSatisfy(
      (e: unknown) => e instanceof UndraTransportError && e.reason === "unsupported" && /`no_such_core`/.test(e.message),
    );
  });

  test("a failure of the native module that no caller can see reaches onError as an UndraUnhandledError", async () => {
    const native = new FakeNative();
    linkCores(native);
    const reported: UndraUnhandledError[] = [];
    const core = track(
      await loadNative(entryOf(native), {
        adapters: { http: null },
        onError: (error) => reported.push(error),
      }),
    );
    // An inbox record of a kind nobody sends: the transport cannot hand it to a caller.
    native.queue(99 as (typeof RecordKind)[keyof typeof RecordKind], new Uint8Array(0), "core");
    // Until the report arrives, under a deadline that only detects a hang (a fixed 10 ms was a bet on how soon the inbox is read).
    await eventually(() => reported.length > 0, "the report of the unknown record");
    expect(reported).toHaveLength(1);
    expect(reported[0]).toBeInstanceOf(UndraUnhandledError);
    expect(reported[0]?.operation).toBe("the native module");
    expect(reported[0]?.error).toBeInstanceOf(UndraCallError.Malformed);
    core.close();
  });

  test("rejects with a clear error when the TurboModule is not linked", async () => {
    await expect(loadNative({ namespace: "fake_core", schemaHash: 1n })).rejects.toSatisfy(
      (e: unknown) => e instanceof UndraTransportError && e.reason === "unsupported" && /not linked/.test(e.message),
    );
  });

  test("the Lifecycle adapter follows AppState", () => {
    expect(lifecycleState("active")).toBe("active");
    expect(lifecycleState("inactive")).toBe("inactive");
    expect(lifecycleState("background")).toBe("background");
    expect(lifecycleState("unknown")).toBe("background");
  });
});

test("cpp/undra.h is byte-identical to the Swift runtime's (the single source of truth of the C ABI)", () => {
  const ours = readFileSync(fileURLToPath(new URL("../cpp/undra.h", import.meta.url)));
  const swift = readFileSync(
    fileURLToPath(new URL("../../../../swift/UndraRuntime/Sources/UndraFFI/include/undra.h", import.meta.url)),
  );
  expect(ours.equals(swift)).toBe(true);
});
