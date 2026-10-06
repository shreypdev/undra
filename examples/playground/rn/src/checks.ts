import {
  CallTarget,
  DbError,
  FsError,
  HttpError,
  PortIds,
  SseError,
  UndraCallError,
  UndraSchemaMismatchError,
  UndraTransportError,
  UndraWriter,
  WsError,
  type WsMessage,
  codecs,
  decodeValue,
  type UndraCore,
} from '@undra/runtime';
import { OptInPortIds } from '@undra/runtime/realtime';
import { NativeTransport, installNative, nativePlatformDefaults } from '@undra/react-native';
import { Platform } from 'react-native';
import {
  BigList,
  Counter,
  LabError,
  Live,
  Notes,
  Probe,
  Stress,
  UndraIds,
  UndraPlaygroundCore,
  add,
  addLater,
  dbCells,
  dbMigrate,
  dbRun,
  explode,
  fileDelete,
  fileList,
  fileRead,
  fileWrite,
  greet,
  httpGet,
  kvGet,
  kvKeys,
  kvPut,
  kvRemove,
  parseCount,
  secretGet,
  secretKeys,
  secretPut,
  secretRemove,
  sseFollow,
  wsEcho,
} from '@playground/core';
import { PLAYGROUND_HEADER, nativeCounters, type Log, type Playground } from './undra';

/** One self-check's outcome. */
export interface CheckResult {
  readonly id: string;
  readonly title: string;
  readonly pass: boolean;
  readonly detail: string;
}

type Check = (core: UndraCore, playground: Playground) => Promise<string>;

function expect(condition: boolean, what: string): void {
  if (!condition) {
    throw new Error(what);
  }
}

async function rejects(run: () => Promise<unknown>): Promise<unknown> {
  try {
    await run();
  } catch (error) {
    return error;
  }
  throw new Error('expected a rejection');
}

const sleep = (ms: number): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));

const utf8 = (text: string): Uint8Array => new TextEncoder().encode(text);
const fromUtf8 = (bytes: Uint8Array | null): string | null => (bytes === null ? null : new TextDecoder().decode(bytes));

/**
 * The loopback server `scripts/rn-device-checks.sh` runs, `contract-tests/servers/realtime-server.mjs` (the simulator
 * shares the Mac's loopback; Android reaches it through `adb reverse`): HTTP, WebSocket and server-sent events.
 */
const LOOPBACK = 'http://127.0.0.1:8737';
const LOOPBACK_WS = 'ws://127.0.0.1:8737';

const sameBytes = (a: Uint8Array, b: Uint8Array): boolean => a.length === b.length && a.every((v, i) => v === b[i]);

/** The messages as one line, for a check's detail. */
const shown = (messages: readonly WsMessage[]): string =>
  messages.map(m => (m.kind === 'text' ? JSON.stringify(m.value) : `binary(${(m.value as Uint8Array).length})`)).join(', ');

/** Whether the module answers `portId` natively on this device (ADR-038 amendment B). */
function native(portId: number): boolean {
  return nativePlatformDefaults(UndraPlaygroundCore.namespace).ports.includes(portId);
}

/**
 * The deadline of a wait for something that always comes (a hang detector, never a speed assertion: a loaded device takes
 * longer, it does not take forever).
 */
const HANG_DEADLINE_MS = 60_000;

/** What {@link watchWebSocket} saw of one connection. */
interface WatchedSocket {
  /** The messages React Native's `WebSocket` handed to JavaScript. */
  readonly messages: number;
  /** Whether JavaScript closed it (the adapter giving up a connection whose core does not read calls `close`). */
  readonly closedHere: boolean;
  /** Whether it closed (its `close` event). */
  readonly closed: boolean;
  /** Puts React Native's `WebSocket` back. */
  restore(): void;
}

/**
 * Watches the next connection the app opens to a URL that contains `path`, until {@link WatchedSocket.restore}: the core's
 * WebSocket port is React Native's global `WebSocket` (`reactNativeWebSocket` looks the constructor up when it connects),
 * so a subclass in its place sees what the adapter sees. What a check waits on instead of a time.
 */
function watchWebSocket(path: string): WatchedSocket {
  const g = globalThis as unknown as { WebSocket: typeof WebSocket };
  const Original = g.WebSocket;
  const seen = { messages: 0, closedHere: false, closed: false };
  const watched = new WeakSet<object>();
  class Watched extends Original {
    constructor(...args: ConstructorParameters<typeof WebSocket>) {
      super(...args);
      if (String(args[0]).includes(path)) {
        watched.add(this);
        this.addEventListener('message', () => {
          seen.messages++;
        });
        this.addEventListener('close', () => {
          seen.closed = true;
        });
      }
    }
    close(code?: number, reason?: string): void {
      if (watched.has(this)) seen.closedHere = true;
      super.close(code, reason);
    }
  }
  g.WebSocket = Watched;
  return {
    get messages() {
      return seen.messages;
    },
    get closedHere() {
      return seen.closedHere;
    },
    get closed() {
      return seen.closed;
    },
    restore() {
      g.WebSocket = Original;
    },
  };
}

/** Waits up to `ms` for `condition`. */
async function eventually(condition: () => boolean, ms: number): Promise<boolean> {
  const until = Date.now() + ms;
  while (!condition()) {
    if (Date.now() > until) return false;
    await sleep(50);
  }
  return true;
}

/**
 * The boundary behaviours a Node host cannot show for @undra/react-native, checked in the app on the
 * device against the real native core (docs/REACT_NATIVE.md, "What is tested where").
 */
const CHECKS: ReadonlyArray<readonly [string, string, Check]> = [
  [
    'RN01',
    'sync call through JSI (raw callSync and a generated method)',
    async core => {
      const w = new UndraWriter(8);
      w.writeI32(20);
      w.writeI32(22);
      const body = core.callSync(CallTarget.FreeFunction, UndraIds.Functions.add, w.finish());
      expect(decodeValue(codecs.i32, body) === 42, `callSync add = ${decodeValue(codecs.i32, body)}`);
      expect((await add(2, 3, core)) === 5, 'add(2, 3) === 5');
      // loadNative attached through the generated entry: its core is the bindings' default (ADR-044).
      expect(UndraPlaygroundCore.core === core, 'UndraPlaygroundCore.core is the native core');
      expect((await add(3, 4)) === 7, 'add(3, 4) on the default core === 7');
      expect((await greet('Hermes', core)).includes('Hermes'), 'greet');
      return 'add(20, 22) = 42 synchronously; add(2, 3) = 5; add(3, 4) = 7 on UndraPlaygroundCore.core';
    },
  ],
  [
    'RN02',
    'async call answered from the core thread',
    async core => {
      const before = nativeCounters(core)?.wakes ?? 0;
      const sum = await addLater(40, 2, 20, core);
      expect(sum === 42, `addLater = ${sum}`);
      const wakes = (nativeCounters(core)?.wakes ?? 0) - before;
      expect(wakes >= 1, 'the reply woke the JS thread through invokeAsync');
      return `addLater(40, 2, 20 ms) = 42; ${wakes} wake(s)`;
    },
  ],
  [
    'RN03',
    'typed error',
    async core => {
      const error = await rejects(() => parseCount('twelve', core));
      expect(error instanceof LabError, `LabError, got ${String(error)}`);
      return `parseCount("twelve") rejected with ${(error as LabError).kind}`;
    },
  ],
  [
    'RN04',
    'panic containment: status 2, the panic report, the core keeps working',
    async (core, playground) => {
      const reports = playground.panics.length;
      const error = await rejects(() => explode('on purpose', core));
      // The core answers status 2 on the wire; the generated binding maps it onto the closed set (ADR-032, amendment A).
      expect(error instanceof UndraCallError.Panicked, `UndraCallError.Panicked (status 2), got ${String(error)}`);
      expect((await add(1, 1, core)) === 2, 'the core still answers');
      // ADR-046: the same panic, once, through `onPanic` on the JS thread, with where it happened and in which build.
      await sleep(50);
      expect(playground.panics.length === reports + 1, `one panic report through onPanic, got ${playground.panics.length - reports}`);
      const report = playground.panics[reports]!;
      expect(report.message.includes('on purpose'), `the panic message: ${report.message}`);
      expect(/\.rs:\d+:\d+$/.test(report.location), `the panic location (file:line:column): ${report.location}`);
      expect(report.operation === 'explode', `what was running: ${report.operation}`);
      expect(report.thread !== '' && report.namespace === UndraPlaygroundCore.namespace && report.coreVersion !== '', `thread, namespace and version: ${report.thread} ${report.namespace} ${report.coreVersion}`);
      expect(report.schemaHash === UndraIds.schemaHash, 'the core schema hash');
      expect(report.frames.length > 0, 'frames');
      return `explode() -> UndraCallError.Panicked (status 2); onPanic: ${report.operation} at ${report.location}, ${report.frames.length} frames, thread ${report.thread}; add(1, 1) = 2 afterwards`;
    },
  ],
  [
    'RN05',
    'cancellation of an in-flight call',
    async core => {
      const probe = await Probe.create(core);
      const abort = new AbortController();
      const pending = probe.hang(abort.signal);
      await sleep(20);
      abort.abort();
      const error = await rejects(() => pending);
      expect(error !== undefined, 'hang() rejected');
      const counters = await probe.counters();
      probe.close();
      return `hang() aborted; probe counters ${JSON.stringify(counters, (_k, v) => (typeof v === 'bigint' ? Number(v) : v))}`;
    },
  ],
  [
    'RN06',
    'stream with backpressure (credit 16, top-up)',
    async core => {
      const probe = await Probe.create(core);
      const seen: number[] = [];
      for await (const tick of probe.ticks(100)) {
        seen.push(tick);
      }
      probe.close();
      expect(seen.length === 100, `100 items, got ${seen.length}`);
      expect(seen.every((value, index) => index === 0 || value > (seen[index - 1] as number)), 'in order');
      return `100 items in order (${seen[0]}..${seen[99]})`;
    },
  ],
  [
    'RN07',
    'store observe, transaction, read-your-writes',
    async core => {
      const counter = await Counter.create(core);
      expect(counter.count.get() === 0, 'initial value applied by create()');
      await counter.increment();
      expect(counter.count.get() === 1, 'applied before the await resumed');
      await counter.add(4);
      const parity = counter.parity.get();
      counter.close();
      expect(parity === 'odd', `computed parity, got ${parity}`);
      return 'count 0 -> 1 -> 5, parity odd, each visible right after its await';
    },
  ],
  [
    'RN08',
    'keyed patch on a 10,000-row list',
    async core => {
      const list = await BigList.create(core);
      const before = core.mirror.stats().entriesApplied;
      const id = await list.insertAt(0, 'Inserted by RN08');
      const items = list.items.get();
      expect(items.length === 10001 && items[0]?.label === 'Inserted by RN08', 'row 0 is the new one');
      expect(items[0]?.id === id, 'with the id the call returned');
      expect(list.count.get() === 10001, 'computed count');
      const applied = core.mirror.stats().entriesApplied - before;
      list.close();
      return `insertAt(0) -> 10,001 rows, ${applied} entries applied (a one-op patch + the count)`;
    },
  ],
  [
    'RN09',
    'native Clock and Timer drive the core on its own',
    async core => {
      const stress = await Stress.create(core);
      await stress.start('firehose', 2000);
      await sleep(300);
      await stress.stop();
      const generated = Number(stress.generated.get());
      stress.close();
      expect(generated > 0, 'the generator ran');
      const native = nativeCounters(core);
      return `generated ${generated} updates in 300 ms (Timer + Clock answered natively: ${native?.nativePortCalls ?? '?'} calls)`;
    },
  ],
  [
    'RN10',
    'schema gate refuses another schema before undra_init',
    async core => {
      const native = installNative(UndraPlaygroundCore.namespace);
      expect(native.namespace === UndraPlaygroundCore.namespace, `the module is the core ${native.namespace}`);
      const transport = new NativeTransport({ namespace: native.namespace, native, expectedSchemaHash: 0x1234n });
      const error = await rejects(() =>
        transport.start({
          reply() {},
          changeSet() {},
          streamItem() {},
          portCall: () => ({ kind: 'unavailable' }),
          log() {},
          closed() {},
        }),
      );
      expect(error instanceof UndraSchemaMismatchError, `UndraSchemaMismatchError, got ${String(error)}`);
      expect((await add(3, 4, core)) === 7, 'the running core is untouched');
      // A namespace the app has no core of: the module's lookup (the class on iOS, lib<ns>.so on Android) says so.
      let unknown: unknown;
      try {
        installNative('no_such_core');
      } catch (failure) {
        unknown = failure;
      }
      expect(
        unknown instanceof UndraTransportError && unknown.reason === 'unsupported' && unknown.message.includes('no_such_core'),
        `an unknown core is refused, got ${String(unknown)}`,
      );
      return 'expected 0x1234, refused; the running core still answers; no_such_core refused by the module';
    },
  ],
  [
    'RN11',
    'Kv default: a round trip through the core, answered natively',
    async (core, playground) => {
      expect(native(PortIds.Kv.portId), 'the module answers Kv natively on this device');
      const before = nativeCounters(core)?.nativePortCalls ?? 0;
      const key = `rn.checks.kv.${playground.nonce}`;
      await kvPut(key, utf8(`value ${playground.nonce}`), core);
      expect(fromUtf8(await kvGet(key, core)) === `value ${playground.nonce}`, 'kv_get returns what kv_put stored');
      const keys = await kvKeys('rn.checks.', core);
      expect(keys.includes(key) && keys.includes('rn.checks.restart'), `kv_keys lists it, got ${keys.join(',')}`);
      await kvRemove(key, core);
      expect((await kvGet(key, core)) === null, 'kv_remove removed it');
      const calls = (nativeCounters(core)?.nativePortCalls ?? 0) - before;
      expect(calls >= 5, `the core's Kv calls were answered by the module (${calls})`);
      // A plain-text marker next to the secret of RN12: the device script finds this one in the app's files.
      await kvPut('rn.checks.kvmark', utf8(`UNDRA-KVMARK-${playground.nonce}`), core);
      return `put, get, keys, remove through the core; ${calls} native port calls`;
    },
  ],
  [
    'RN12',
    'SecureStore default: a round trip through the core (the Keychain, the Android Keystore)',
    async (core, playground) => {
      expect(native(PortIds.SecureStore.portId), 'the module answers SecureStore natively on this device');
      const secret = `UNDRA-SECRET-${playground.nonce}`;
      await secretPut('rn.checks.secret', utf8(secret), core);
      expect(fromUtf8(await secretGet('rn.checks.secret', core)) === secret, 'secret_get returns what secret_put stored');
      expect((await secretKeys('rn.checks.', core)).includes('rn.checks.secret'), 'secret_keys lists it');
      expect((await kvGet('rn.checks.secret', core)) === null, 'a secret is not in Kv');
      // A key is any string, U+0000 included: list returns it whole (the Keychain's account, the sealed file's key).
      const nul = 'rn.checks.nul\u0000end';
      await secretPut(nul, utf8('n'), core);
      const listed = await secretKeys('rn.checks.nul', core);
      expect(listed.length === 1 && listed[0] === nul, `secret_keys returns a key with U+0000 whole, got ${JSON.stringify(listed)}`);
      await secretRemove(nul, core);
      expect((await secretGet(nul, core)) === null, 'and secret_remove removes it');
      // Kept: the device script checks that this value is not readable in the app's files.
      return `stored and read back (marker nonce ${playground.nonce}); ${nativePlatformDefaults(UndraPlaygroundCore.namespace).secureStore ?? ''}`;
    },
  ],
  [
    'RN13',
    'Fs default: write, read, list, delete inside the root; a path outside is Denied',
    async (core, playground) => {
      expect(native(PortIds.Fs.portId), 'the module answers Fs natively on this device');
      const body = utf8(`hello ${playground.nonce}`);
      await fileWrite('rn-checks/notes/hello.txt', body, core);
      expect(fromUtf8(await fileRead('rn-checks/notes/hello.txt', core)) === `hello ${playground.nonce}`, 'file_read returns what file_write wrote');
      const names = await fileList('rn-checks/notes', core);
      expect(names.length === 1 && names[0] === 'hello.txt', `file_list, got ${names.join(',')}`);
      const outside = await rejects(() => fileRead('../escape.txt', core));
      expect(outside instanceof FsError.Denied, `reading ../ is FsError.Denied, got ${String(outside)}`);
      const escape = await rejects(() => fileWrite('rn-checks/../../escape.txt', body, core));
      expect(escape instanceof FsError.Denied, `writing outside is FsError.Denied, got ${String(escape)}`);
      await fileDelete('rn-checks', core);
      const gone = await rejects(() => fileRead('rn-checks/notes/hello.txt', core));
      expect(gone instanceof FsError.NotFound, `deleted with its directory: FsError.NotFound, got ${String(gone)}`);
      return `write/read/list/delete under ${nativePlatformDefaults(UndraPlaygroundCore.namespace).fs ?? '?'}; ../ Denied`;
    },
  ],
  [
    'RN14',
    'Http default: a GET through the core to the loopback server; a refused port is HttpError.Network',
    async core => {
      const res = await httpGet(`${LOOPBACK}/undra-rn-check`, 5000, core);
      expect(res.status === 200, `status ${res.status}`);
      const answer = JSON.parse(new TextDecoder().decode(res.body)) as { ok?: boolean; headers?: Record<string, string> };
      expect(answer.ok === true, 'the server answered');
      expect(answer.headers?.[PLAYGROUND_HEADER] === 'rn', "the app's override added its header (the sample of overriding a default)");
      const refused = await rejects(() => httpGet('http://127.0.0.1:1/', 5000, core));
      expect(refused instanceof HttpError.Network, `a refused connection is HttpError.Network (never Unavailable), got ${String(refused)}`);
      return `GET ${LOOPBACK}/undra-rn-check -> 200 with the override's header; 127.0.0.1:1 -> HttpError.Network`;
    },
  ],
  [
    'RN15',
    'Connectivity default: the core received the native source\'s report',
    async (_core, playground) => {
      expect(native(PortIds.Connectivity.portId), 'the module reports Connectivity natively on this device');
      const device = playground.device;
      expect(await eventually(() => device.connectivityReports.get() >= 1, 5000), 'at least one report within 5 s');
      expect(device.online.get(), 'online');
      return `online=${String(device.online.get())} kind=${device.netKind.get()} after ${device.connectivityReports.get()} report(s)`;
    },
  ],
  [
    'RN16',
    'Lifecycle default: the core received AppState',
    async (_core, playground) => {
      const device = playground.device;
      expect(await eventually(() => device.lifecycleReports.get() >= 1, 5000), 'at least one report within 5 s');
      expect(device.appState.get() === 'active', `active, got ${device.appState.get()}`);
      return `state=${device.appState.get()} after ${device.lifecycleReports.get()} report(s)`;
    },
  ],
  [
    'RN17',
    'Db default: Notes in SQLite through the core (open with migrations, add, count, close, reopen), answered natively',
    async (core, playground) => {
      expect(native(OptInPortIds.Db.portId), 'the module answers Db natively on this device');
      const before = nativeCounters(core)?.nativePortCalls ?? 0;
      const notes = await Notes.create(core);
      try {
        expect((await notes.open('rn-checks')) === 2, 'open migrates to version 2');
        const start = await notes.count();
        const added = await notes.add(`note ${playground.nonce}`);
        await notes.add('and another');
        expect((await notes.count()) === start + 2, 'count reads the two new rows from the database');
        expect(notes.notes.get().some(n => n.id === added.id && n.title === `note ${playground.nonce}`), 'the mirror shows the new note');
        const taken = await rejects(() => notes.addWithId(added.id, 'duplicate'));
        expect(taken instanceof DbError.Constraint && taken.message.includes('UNIQUE'), `an id in use is DbError.Constraint (unique), got ${String(taken)}`);
        await notes.closeDatabase();
        const closed = await rejects(() => notes.count());
        expect(closed instanceof DbError.Unavailable, `after close: DbError.Unavailable, got ${String(closed)}`);
        expect((await notes.open('rn-checks')) === 2, 'reopened at version 2');
        expect((await notes.count()) === start + 2, 'the rows are in the file');
        await notes.closeDatabase();
      } finally {
        notes.close();
      }
      const calls = (nativeCounters(core)?.nativePortCalls ?? 0) - before;
      expect(calls >= 10, `the core's Db calls were answered by the module (${calls})`);
      return `notes kept across close and reopen in ${nativePlatformDefaults(UndraPlaygroundCore.namespace).db ?? '?'}; unique id refused; ${calls} native port calls`;
    },
  ],
  [
    'RN18',
    'Db typed cells: every storage class through the platform\'s SQLite (i64 extremes, empty text and blob, NULL, U+0000, a 2 MiB row)',
    async core => {
      const min = -(2n ** 63n);
      const max = 2n ** 63n - 1n;
      const blob = new Uint8Array([0, 1, 254, 255]);
      const a = await dbCells(min, 1.5, 'héllo \u{1F30D}\n', blob, null, core);
      expect(a.int === min && a.real === 1.5 && a.text === 'héllo \u{1F30D}\n' && sameBytes(a.blob, blob) && a.none === null, `round trip 1: ${String(a.int)} ${a.real} ${JSON.stringify(a.text)} ${a.none}`);
      expect(a.types.join(',') === 'integer,real,text,blob,null', `storage classes ${a.types.join(',')}`);
      const b = await dbCells(max, -0.25, '', new Uint8Array(0), 'x', core);
      expect(b.int === max && b.real === -0.25 && b.text === '' && b.blob.length === 0 && b.none === 'x', `round trip 2: ${String(b.int)} ${b.real} ${b.blob.length} ${b.none}`);
      expect(b.types.join(',') === 'integer,real,text,blob,text', `an empty text is text and an empty blob is a blob, got ${b.types.join(',')}`);
      // What JNI and Java strings trip on (Android crosses every value through Java): U+0000, a supplementary
      // character, U+FFFF; and SQL in a value, which stays a value (parameters are bound, never formatted into SQL).
      const tricky = "a\u0000b \u{1F30D} ￿ '; DROP TABLE cells; --";
      const zeros = new Uint8Array([0, 0, 0]);
      const c = await dbCells(0n, 0, tricky, zeros, tricky, core);
      expect(c.text === tricky && c.none === tricky && sameBytes(c.blob, zeros), `U+0000, an emoji and SQL in a value come back as they were, got ${JSON.stringify(c.text)}`);
      // A 2 MiB row: the system SQLite returns it; Android's CursorWindow (2 MiB) cannot hold it, which must be a typed
      // DbError, never a crash.
      const big = new Uint8Array(2 * 1024 * 1024).fill(7);
      let bigRow: string;
      try {
        const d = await dbCells(1n, 1, 'big', big, null, core);
        expect(sameBytes(d.blob, big), 'the 2 MiB blob came back whole');
        bigRow = 'a 2 MiB row round-trips';
      } catch (error) {
        expect(error instanceof DbError.Sql || error instanceof DbError.Full, `a 2 MiB row is a typed DbError (Sql or Full), got ${String(error)}`);
        bigRow = `a 2 MiB row is ${String(error)}`;
      }
      return `i64 ${String(min)}..${String(max)}, reals, text, blobs (empty too) and NULL come back by storage class; U+0000, an emoji and SQL text intact; ${bigRow}`;
    },
  ],
  [
    'RN19',
    'Db migrations and transactions: a failed migration or a refused commit rolls everything back; typed SQL errors',
    async core => {
      const name = 'rn-checks-migrate';
      // From scratch: what an earlier launch left goes (one statement per call; no migrations, so no downgrade check).
      await dbRun(name, 'DROP TABLE IF EXISTS a', core);
      await dbRun(name, 'DROP TABLE IF EXISTS b', core);
      await dbRun(name, 'PRAGMA user_version = 0', core);
      const failed = await rejects(() => dbMigrate(name, true, core));
      expect(failed instanceof DbError.Migration && failed.version === 2 && failed.message.includes('nowhere'), `the broken second migration: ${String(failed)}`);
      const gone = await rejects(() => dbRun(name, 'INSERT INTO a VALUES (1)', core));
      expect(gone instanceof DbError.Sql && gone.message.includes('no such table'), `migration 1 was rolled back with it, got ${String(gone)}`);
      expect((await dbMigrate(name, false, core)) === 2, 'the good pair then runs from version 0 to 2');
      expect((await dbRun(name, 'INSERT INTO a VALUES (1)', core)) === 1n, 'and its table exists');
      const two = await rejects(() => dbRun(name, 'INSERT INTO a VALUES (2); INSERT INTO a VALUES (3)', core));
      expect(two instanceof DbError.Sql && two.message.includes('only one statement'), `two statements in one call, got ${String(two)}`);
      // A commit the database refuses (a deferred foreign key, checked only at COMMIT) rolls the transaction back and
      // ends it: nothing of it is kept, and the next transaction begins and commits.
      const deferred = 'rn-checks-deferred';
      const notes = await Notes.create(core);
      try {
        await notes.open(deferred);
        await dbRun(deferred, 'CREATE TABLE IF NOT EXISTS parent (id INTEGER PRIMARY KEY)', core);
        await dbRun(deferred, 'CREATE TABLE IF NOT EXISTS child (pid INTEGER REFERENCES parent (id) DEFERRABLE INITIALLY DEFERRED)', core);
        await dbRun(deferred, "CREATE TRIGGER IF NOT EXISTS orphan AFTER INSERT ON notes WHEN NEW.title = 'orphan' BEGIN INSERT INTO child VALUES (-1); END", core);
        const before = await notes.count();
        const refused = await rejects(() => notes.addAll(['orphan']));
        expect(refused instanceof DbError.Constraint && refused.kind_ === 'foreignKey', `the refused commit is DbError.Constraint(foreignKey), got ${String(refused)}`);
        const next = await notes.addAll(['kept']).catch((error: unknown) => `failed: ${String(error)}`);
        expect(next === 1, `after the refused commit the next transaction begins and commits, got ${String(next)}`);
        expect((await notes.count()) === before + 1, 'only the second transaction\'s row is there');
        await notes.closeDatabase();
        await notes.open(deferred);
        expect((await notes.count()) === before + 1, 'and it is in the file after a reopen');
        await notes.closeDatabase();
      } finally {
        notes.close();
      }
      return `Migration { version: 2 } and nothing of it kept; then version 2; one statement per call; a commit refused by a deferred foreign key is Constraint(foreignKey) and rolled back, the next transaction commits`;
    },
  ],
  [
    'RN20',
    'WebSocket default: an echo through the core to the loopback server; headers, the peer\'s close, a drop, a stalled flood and a refusal typed',
    async (core, playground) => {
      let floodDetail = '';
      const sent: WsMessage[] = [
        { kind: 'text', value: `hello ${playground.nonce}` },
        { kind: 'binary', value: new Uint8Array([1, 2, 0, 255]) },
      ];
      const back = await wsEcho(`${LOOPBACK_WS}/ws/echo`, sent, core);
      expect(back.length === 2 && back[0]?.kind === 'text' && back[0].value === sent[0]?.value, `the text came back, got ${shown(back)}`);
      expect(back[1]?.kind === 'binary' && sameBytes(back[1].value as Uint8Array, sent[1]?.value as Uint8Array), `the binary came back, got ${shown(back)}`);
      // Headers ride React Native's third constructor argument; the server echoes the upgrade's headers.
      const live = await Live.create(core);
      try {
        await live.connect(`${LOOPBACK_WS}/ws/headers`, [], [{ name: 'x-undra-token', value: playground.nonce }]);
        const [headers] = await live.read(1);
        const seen = headers?.kind === 'text' ? (JSON.parse(headers.value) as Record<string, string>)['x-undra-token'] : undefined;
        expect(seen === playground.nonce, `the upgrade carried the header, the server saw ${String(seen)}`);
        await live.disconnect(1000, 'done');
        await live.connect(`${LOOPBACK_WS}/ws/close?code=4000&reason=bye`, [], []);
        const [hello] = await live.read(1);
        expect(hello?.kind === 'text' && hello.value === 'hello', 'the message before the close frame');
        const closed = await rejects(() => live.read(1));
        expect(closed instanceof WsError.Closed && closed.code === 4000 && closed.reason === 'bye', `the peer's close is WsError.Closed(4000, bye), got ${String(closed)}`);
        // A normal close frame (1000) is Closed too; a connection that drops without one is Network.
        await live.connect(`${LOOPBACK_WS}/ws/close?code=1000&reason=done`, [], []);
        await live.read(1);
        const normal = await rejects(() => live.read(1));
        expect(normal instanceof WsError.Closed && normal.code === 1000 && normal.reason === 'done', `the peer's 1000 is WsError.Closed(1000, done), got ${String(normal)}`);
        await live.connect(`${LOOPBACK_WS}/ws/drop`, [], []);
        await live.read(1);
        const dropped = await rejects(() => live.read(1));
        // React Native forwards no `wasClean`: Android's OkHttp reports a drop as a failure (Network), iOS as the end of
        // its stream (Closed 1001 "Stream end encountered"); both are typed ends, documented in docs/REACT_NATIVE.md.
        expect(
          dropped instanceof WsError.Network || (dropped instanceof WsError.Closed && Platform.OS === 'ios'),
          `a drop is a typed end (Network; on iOS Closed), got ${String(dropped)}`,
        );
        // A flood the core does not read: React Native's WebSocket cannot pause, so what arrives waits in JavaScript up
        // to 4,096 messages (plus the 16 the binding reads ahead), then the connection is given up with 1008. The core
        // reads only once that happened, or once the whole flood arrived without it (when the adapter did not hold to its
        // limit): reading earlier keeps up with the flood. (A fixed 1.5 s was not always enough on a loaded emulator.)
        const watched = watchWebSocket('/ws/flood');
        try {
          await live.connect(`${LOOPBACK_WS}/ws/flood?n=6000&size=16`, [], []);
        } finally {
          watched.restore();
        }
        expect(
          await eventually(() => watched.closedHere || watched.closed, HANG_DEADLINE_MS),
          `the flood neither was given up nor ended in ${HANG_DEADLINE_MS} ms (${watched.messages} messages arrived)`,
        );
        let flooded = 0;
        let floodEnd: unknown = null;
        while (floodEnd === null && flooded < 7000) {
          try {
            flooded += (await live.read(16)).length;
          } catch (error) {
            floodEnd = error;
          }
        }
        expect(
          floodEnd instanceof WsError.Closed && floodEnd.code === 1008 && floodEnd.reason === 'the core did not keep up',
          `a stalled flood ends with WsError.Closed(1008, "the core did not keep up"), got ${String(floodEnd)} after ${flooded}`,
        );
        expect(flooded >= 4096 && flooded <= 4096 + 32, `bounded: ${flooded} of 6000 messages were kept for the core`);
        floodDetail = `a stalled flood of 6000 (${watched.messages} reached JavaScript) kept ${flooded} then Closed(1008)`;
      } finally {
        live.close();
      }
      const refused = await rejects(() => wsEcho(`${LOOPBACK_WS}/ws/deny?status=401`, [], core));
      expect(refused instanceof WsError.Refused, `a refused upgrade is WsError.Refused, got ${String(refused)}`);
      return `echo of ${shown(back)}; a header on the upgrade; Closed(4000, "bye"), Closed(1000, "done"); a drop is Network; ${floodDetail}; /ws/deny is Refused`;
    },
  ],
  [
    'RN21',
    'Sse default: server-sent events through the core from the loopback server (parse, resume, end, refusal)',
    async core => {
      const feed = `${LOOPBACK}/sse/feed`;
      const all = await sseFollow(feed, null, 10, core);
      const summary = all.events.map(e => `${e.id ?? '-'}:${e.event}:${JSON.stringify(e.data)}`).join(' ');
      expect(all.ended, 'the server ended the stream (SseError.Ended)');
      expect(
        summary === '1:message:"one" 2:tick:"two\\nlines" 2:message:"three" 4:message:"four"',
        `the feed parses as the HTML standard says, got ${summary}`,
      );
      expect(all.events[0]?.retryMs === 1500, 'retry: 1500');
      const resumed = await sseFollow(feed, '2', 10, core);
      expect(resumed.events.map(e => e.data).join(',') === 'three,four' && resumed.ended, 'resumed after Last-Event-ID 2');
      const refused = await rejects(() => sseFollow(`${LOOPBACK}/sse/status?code=500`, null, 10, core));
      expect(refused instanceof SseError.Refused && refused.status === 500, `a 500 is SseError.Refused(500), got ${String(refused)}`);
      const html = await rejects(() => sseFollow(`${LOOPBACK}/sse/html`, null, 10, core));
      expect(html instanceof SseError.Protocol, `text/html is SseError.Protocol, got ${String(html)}`);
      return `${all.events.length} events then Ended; resumed after id 2; 500 Refused; text/html Protocol`;
    },
  ],
];

/** Runs every check in order and logs `UNDRA-RN CHECK <id> PASS|FAIL <detail>`. */
export async function runChecks(playground: Playground, log: Log): Promise<CheckResult[]> {
  const results: CheckResult[] = [];
  for (const [id, title, check] of CHECKS) {
    let result: CheckResult;
    try {
      const detail = await check(playground.core, playground);
      result = { id, title, pass: true, detail };
    } catch (error) {
      result = { id, title, pass: false, detail: error instanceof Error ? error.message : String(error) };
    }
    results.push(result);
    log(`UNDRA-RN CHECK ${id} ${result.pass ? 'PASS' : 'FAIL'} ${title}: ${result.detail}`);
  }
  const passed = results.filter(r => r.pass).length;
  log(`UNDRA-RN CHECKS ${passed}/${results.length} passed`);
  return results;
}
