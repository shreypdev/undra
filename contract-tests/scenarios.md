# Contract scenarios

This is the definition of "the platforms agree" (SPEC section 14, blueprint section 13): thirty-five
scenarios (S01 to S35) against the **real playground core** (`examples/playground/core`, the same Rust crate the
apps run), through the real boundary. S01 to S20 and S23 to S35 run on every platform; S21 and S22 are about
the web host (worker mode and crash recovery, ADR-049) and run on TypeScript only. S23 to S25 are the opt-in
ports of ADR-047 and ADR-048; S26 is ADR-044's, S27 ADR-040's and S28 ADR-041's; S29 and S30 are ADR-046's
(panic reports and background runs); S31 is ADR-042's (newtypes, generic instantiations, leaf types), S32 and
S33 are ADR-043's (paged queries and lazy lists, polling), S34 is ADR-058's (generic functions, objects and stores)
and S35 is ADR-059's (query handles across a restore):

| Platform | Runner | Boundary under test |
|---|---|---|
| TypeScript | `contract-tests/ts` (vitest) | `@undra/runtime` `WasmMainTransport` over the real `playground_core.wasm` |
| Kotlin | `contract-tests/kotlin` (kotlinc + JVM) | `dev.undra.runtime` `UndraCore` over JNI and the real `libplayground_core` |
| Swift | `contract-tests/swift` (XCTest) | `UndraRuntime` `UndraCore` over the C ABI table of the real core |

Every runner prints one line per scenario, `SCENARIO S07 PASS|FAIL|SKIP <title>`, and
`contract-tests/check.sh` fails unless every id of the platform is `PASS` (S01 to S20 and S23 to S35, plus
S21 and S22 on TypeScript; a `SKIP` needs its reason here, in the platform notes of the scenario). That is 101
cells: 33 on Swift, 33 on Kotlin, 35 on TypeScript. The React Native column (`runtimes/rn`, the TypeScript scenarios
over its model of the native module) runs S34 and S35 too.

## The harness (the same on every platform)

The core is built by the `undra` CLI (`undra build --platform web|host|ios`) and its bindings by
`undra bindgen` (`examples/playground/generated/`). A runner uses the **generated bindings** for
everything a UI would use and the runtime's own API (`UndraCore`) for what bindings do not expose
(raw signal updates, statistics, snapshots, cancellation, schema checks).

* **One playground core** per process on the native platforms (a core's `init` is once per core, and
  the runner loads the playground's once), so scenarios create their own stores and objects, and the state they share (the query cache, the offline queue,
  the Clock) is isolated by **list names** (`s12`, `s13`, `s14`) and left clean.
* **Adapters** the runner supplies at load time:
  * `Clock`: a **manual clock**, settable and advanceable by the test, starting at
    `1_700_000_000_000` ms (used by the query cache for staleness; timers are not the clock's).
  * `Http`: an **in-memory server** (`FakeServer`): routes by method and exact URL, records every
    request (method, url, headers, body), can delay a reply by N ms and can answer with a network
    error (`HttpError.Network("offline")`). Default reply for an unknown route: status 404.
  * `Kv`: in-memory, recording every operation (`get`, `set`, `delete`, `list` with the key or
    prefix, in order), with **injectable failures** (ADR-049): the test can make the next `n`
    operations, or every operation until it heals the store, of one kind (or of one key) fail with a
    `StorageError` (`Full`, `Locked`, `Io(..)`, ...), which the adapter answers as the port's typed
    error (reply status 1), exactly like the platform's own adapters do. On Swift and Kotlin the first
    `get` of `undra.query.queue2` fails `Locked` (S20 step 4: what an app launched before the device's
    first unlock reads). `Log`: captures `(level, target, message)`. `Rng`, `Timer`: the platform
    defaults (real timers; a real `setTimeout` / Rust timer thread).
  * `Connectivity`: the test emits events itself (`core.event(Connectivity.changed, online, kind)`;
    each runtime has a helper or the raw call).
* **Server fixtures.** Base URL `https://playground.test`. A list `L` lives at
  `GET /lists/L/todos` (JSON array of `{"id":u32,"title":string,"done":bool}`), `POST
  /lists/L/todos` (body `{"title":..}`, answer 201 with the created object) and `PATCH
  /lists/L/todos/ID` (body `{"done":..}`, answer 200 with the object). `Idempotency-Key` is sent on
  POST.
* **Two builds** (S14 steps 7 to 9, S15 steps 11 to 14, S35 step 10; ADR-037, ADR-059). The runner also builds the
  playground core a second time, as **build B**: `UNDRA_PLAYGROUND_V2=1 undra build ...` (the core's
  `build.rs` turns it into `cfg(playground_v2)`; see `examples/playground/core/src/updates.rs` for what
  changes), and keeps that artefact apart from build A's. Build B has no generated bindings: the runner
  loads it with the hash the core reports (`undra_schema_hash`) and drives it through the raw API with
  the ids it knows (`Profile.new` / `Profile.describe` / `Legacy.new`, the mutation ids of `save_note` and
  `tag_note`, the function `storage_status`, whose `StorageStatus` record has the same layout in both
  builds; for S35 the query handle types and their method ids, `configure_remote`, `Library`'s `books`).
  TypeScript loads build B in the same process; Swift and Kotlin load one core per process, so
  they run the build-B steps in a **second process** (Swift: the same test bundle relaunched with build
  B's library in place of build A's; Kotlin: a second JVM with build B's library), handing over the `Kv`
  contents, the snapshots and the handles in a file. The build-B process prints only `SCENARIO S14 FAIL ...` /
  `SCENARIO S15 FAIL ...` / `SCENARIO S35 FAIL ...` lines (never `PASS`), so `check.sh`, which reads the last line
  of an id, fails the scenario if build B fails and keeps build A's `PASS` otherwise.
* **Waiting.** Anything asynchronous is awaited with a **5 second timeout** (polling every 10 ms or
  using the runtime's own wait), never with a bare sleep, except where a scenario says "for 200 ms
  nothing happens".
* **Signals.** "Observe" means `Store.create()` / `init` (which constructs, observes every signal
  and applies the initial change-set before returning), or for raw checks `core.construct` +
  `core.mirror.register` + `core.observe(handle, ALL_SIGNALS, true)`.
* **Statistics** (`core.stats()`) carry `live_handles`, `active_calls`, `open_streams`,
  `transactions` (== change-sets delivered), `panics` and `crossings.{calls, replies, change_sets,
  stream_items, cancelled, bad_requests}`. A scenario that counts uses **deltas** between two
  readings, since other scenarios share the core.

Ids: a method's id is `fnv1a32("Type.method")`, a free function's `fnv1a32("fn.name")`; the bindings
carry them (`UndraIds`).

## The scenarios

### S01 primitives round-trip

Every primitive the wire has makes the round trip through the language's codec unchanged.

1. `echo_primitives` with the **typical** value: `flag=true, tiny=-8, small=-16000, int=-2000000000,
   long=-9000000000000000000, byte=255, word=65535, dword=4000000000, qword=18000000000000000000,
   single=1.5, double=-2.25e100, text="héllo, wörld ✓", blob=[0,1,2,254,255], span=1.500000123 s,
   at=1700000000123 ms, id=12345678-9abc-def0-0102-030405060708`.
2. `echo_primitives` with the **extremes**: `tiny=-128, small=-32768, int=-2147483648,
   long=-9223372036854775808, qword=18446744073709551615, single=-0.0, double=+infinity,
   text="" , blob=[] `, and with `long=9223372036854775807, tiny=127, small=32767, int=2147483647,
   double=NaN` (compared as NaN), `text` = 10,000 characters of `"ü"`, `blob` = 65,536 bytes
   `i % 251`.
3. `ping()` (no arguments, no result) completes.
4. `Bytes`, `String` and `Uuid` are not aliased: mutating the returned value does not change what was
   sent.

Expected: every field equal to what was sent (64-bit integers exactly: `bigint` in TypeScript, `Long`
/ `ULong` in Kotlin, `Int64` / `UInt64` in Swift).

### S02 records, enums and errors

1. `echo_figure` for `Circle{radius=2.5}`, `Rect{width=2, height=3.5}`, `Label("")`, `Label("héllo")`,
   `Empty`: each returns an equal value of the same variant.
2. `echo_composite` with `name="everything", tags=["a","","ü"], figure=Some(Rect{2,3.5}),
   history=[Circle{1}, Label("x"), Empty], scores={"high":99,"low":-3}, names={7:"seven",1:"one"},
   limit=Some(7)` and again with `figure=None, tags=[], history=[], scores={}, names={}, limit=None`.
3. `area(Rect{2,4})` is `8.0`; `area(Circle{1})` is `pi` (to 1e-12).
4. Errors as values: `area(Label("hat"))` fails with `LabError.Rejected(code=1, reason="`hat` has no
   area")`; `area(Empty)` fails with `LabError.Empty`; `parse_count("")` with `Empty`,
   `parse_count("1234567890")` with `TooLong(max=9)`, `parse_count("4x2")` with `NotANumber("4x2")`;
   `parse_count(" 42 ")` is `42`.
5. The error's message is the core's `Display`: `TooLong(9)` -> `"longer than 9 characters"`.

Expected: typed errors arrive as the language's typed error (Swift `throws(LabError)`, Kotlin sealed
`LabError` exception, TypeScript `LabError` subclass), never as a generic failure.

### S03 sync call

The synchronous path (`callSync` / the `undra_call_sync` ABI; wasm-main in TypeScript) answers without
waiting for an event loop.

1. `add(40, 2)` through the sync path is `42`; `add(2147483647, 1)` is `-2147483648`;
   `greet("Ada")` is `"Hello, Ada, from the playground core"`.
2. The sync path has no suspension: in Swift and Kotlin it is called from a plain (non-async) function;
   in TypeScript `core.callSync` returns bytes (not a promise) in `wasm-main` mode.
3. 10,000 sync `add` calls in a loop return the right sums and `crossings.calls` grew by exactly
   10,000 (the runner measures and prints the mean ns per call; no budget is asserted here).
4. A sync call of an **async** method (`add_later`) is refused: status `bad_request` (TypeScript: the
   reply error with status 5), and the core is unharmed (`add(1,1) == 2` afterwards).

### S04 async call

1. `add_later(20, 22, 50)` resolves to `42` after at least 45 ms, and within the 5 s wait (a hang detector: a timer
   that never fires is what it catches; how long after 50 ms the answer comes is the machine's, and step 2's order is
   the claim that the delays are honoured).
2. Three concurrent calls `add_later(i, 0, d)` with `(i, d) = (1, 400), (2, 50), (3, 200)` resolve
   in delay order `2, 3, 1` and with the right values (the 400 ms call is issued first on purpose: the core
   honours each delay, it does not answer in the order of the calls). Every trial asserts the values and that each
   call took at least its delay minus 5 ms; the order of the recorded completion instants is judged only on a trial the
   runner delivered (the three issued within 20 ms of each other, the 50 ms call answered under 130 ms late and the
   200 ms call under 180 ms late: the gap to the next delay less 20 ms, the most a late answer can move a call), a
   stalled trial is repeated up to 30 times, and thirty stalled trials fail the step and say it was the machine's.
3. `Probe.wait(10)` resolves to `10`.
4. A call made from inside a change observer or another call's completion (re-entrancy on the
   platform side: the continuation of `add_later` calls `add_later` again) works: the second call
   resolves. (Checks the runtimes never invoke callbacks under the core lock.)

### S05 error propagation

1. Sync typed error: `parse_count("x")` fails with `NotANumber("x")` (see S02) and the failure is
   delivered on the same path as a success (TypeScript rejects, Swift throws, Kotlin throws).
2. Async typed error: `fail_later(10, 7)` fails with `Rejected(code=7, reason="on purpose")`.
3. Store errors: `Todos.add("   ")` fails with `TodoError.EmptyTitle` and changes nothing (no change
   set: `transactions` delta 0); `BigList.remove_at(10000)` fails with
   `ListError.OutOfRange(index=10000, len=10000)`; `BigList.insert_at(10001, "x")` fails likewise
   (`len=10000`) and does not consume an id (the next `insert_at(0, "y")` returns id `10001`).
4. Reply statuses: an unknown method id (`0xDEADBEEF` as a free function) fails as **bad request**;
   calling a method on a **released** handle fails as bad request (stale handle); a constructor given
   undecodable arguments (`Probe` has none: use `RemoteTodosQueryHandle` with an empty argument
   buffer) fails as bad request. Each carries a reason string.
5. After all of the above the core still works (`add(1, 2) == 3`) and `bad_requests` grew by 3.
6. Through the generated bindings, on closed objects (ADR-032 and its amendment A: every platform fails a call
   with its own `E`, the caller's cancellation or `UndraCallError`, and a command reports): close a `BigList`,
   then `remove_at(0)` fails as **bad request** (Swift `UndraCallError.refused`, Kotlin
   `UndraCallError.Refused`, TypeScript `UndraCallError.Refused`, `kind` `"refused"`), with a reason. Close a
   `Counter`, then `increment()` (a command): it **returns normally** (TypeScript: the promise resolves) and
   `LoadOptions.onError` received exactly one `UndraUnhandledError` with operation `Counter.increment`
   (the platform's spelling, no keyword escapes) whose error is `Refused`. `bad_requests` grew by exactly 2 more,
   the process is alive and `add(1, 2) == 3`. The runner clears what it recorded, and the TypeScript harness
   treats an `onError` report nobody asserted as a failure of the scenario.

### S06 cancellation

1. `Probe` = new. Start `probe.hang()`; wait until `counters().started == 1`.
2. Cancel it (TypeScript `AbortSignal.abort()`, Kotlin `Job.cancel()`, Swift `Task.cancel()`).
   The platform call ends as cancelled (TypeScript: rejects; Kotlin: `CancellationException`; Swift:
   `CancellationError` or the runtime's cancelled error).
3. Wait until `counters().cancelled == 1`: the **core dropped the future** (the `Probe` counts it in a
   drop guard). `completed` is still `0`, `active_calls` is back to its value before step 1, and
   `crossings.cancelled` grew by 1.
4. Independence: start `probe.wait(100)` and `probe.hang()` together, cancel only the second; the
   first resolves `100`, `completed == 1`, `cancelled == 2`.
5. Cancelling after completion is a no-op: `probe.wait(1)`, await, cancel; `cancelled` stays 2.
6. A cancelled call of a method **with a typed error**: start `fail_later(5000, 1)`, wait 100 ms,
   cancel it. The platform call ends as cancelled exactly as in step 2 (Swift `CancellationError`, not
   a `LabError` and not a stopped process; Kotlin `CancellationException`; TypeScript the signal's
   reason), in under half of the call's own 5 s delay (2.5 s after the cancel; a cancel that waited for the core's
   answer would end 4.9 s after it); `crossings.cancelled` grew by 1; `add(1, 1) == 2` afterwards.

### S07 stream with backpressure

1. `probe.reset()`. Open `probe.ticks(1000)` and read exactly 5 items (`0..4`), then **stop reading**
   without cancelling.
2. For 200 ms nothing more is read; then `counters().produced` is **at least 5 and at most 5 + 64**
   (the credit window: 16 granted at subscribe, topped up as the consumer drains; the core is polled
   at most one item past its credit). In particular it is not 1,000.
3. Resume: read the remaining 995 items; they are `5..999` in order, the stream then ends
   normally, and `produced == 1000`.
4. Early termination: open `probe.ticks(1000000)`, read 3 items, cancel/stop the iteration (TypeScript
   `break` out of `for await`, Kotlin cancel the collector, Swift `break` or cancel the task);
   `open_streams` is back to its earlier value (within the 5 s wait, a hang detector: a stream that was not cancelled
   stays open) and `produced` stays below 200.
5. A short stream ends: `ticks(3)` yields `0,1,2` and completes; `ticks(0)` completes with no items.
6. A typed error part-way (ADR-036: `impl Stream<Item = Result<T, E>>`, flag 2 carries only the
   stream's own `E`): `probe.ticks_then_fail(5, 3, 7)` yields `0,1,2` and then fails with the
   stream's own error, `LabError.Rejected { code: 7, reason: "stopped at 3" }` (Swift
   `LabError.rejected`, Kotlin `LabError.Rejected`, TypeScript a `LabError` of that variant), never a
   wire error; `ticks_then_fail(3, 9, 7)` yields `0,1,2` and completes normally.
7. A stream the core cancels (ADR-036: flag 3, status 3). Take a snapshot; open
   `probe.ticks_then_fail(1000000, 999999, 1)` (a stream **with** an error type) and read 2 items; then
   `restore` the snapshot. The probe is not a store, so the restore invalidates it and ends the stream:
   the iteration fails as **cancelled by the core** (`UndraCallError.cancelledByCore` in Swift,
   `UndraCallError.CancelledByCore` in Kotlin and TypeScript), not as a `LabError` and
   not as a wire decode error (Kotlin `WireException`, TypeScript `WireError`), within the 5 s wait (a hang
   detector: the restore ends the stream while it runs, on no timer, and a stream it did not end would stay open or
   end with its own error); `open_streams` is back to its value before the step. Close the probe.

### S08 store observe: initial change-set

1. Raw: `core.construct(Todos.new)`, register a mirror callback for the handle, then
   `observe(handle, ALL_SIGNALS, on)`. **Before `observe` returns** (native) / resolves (TypeScript)
   the mirror has received exactly one change-set's worth of entries: signals `0 todos`, `1 filter`,
   `2 visible`, `3 remaining`, all as full values (`op = 0`): `[]`, `all`, `[]`, `0`.
2. Through the generated class: `Todos.create()` / `Todos()` has `todos == []`, `filter == .all`,
   `visible == []`, `remaining == 0` immediately after it returns, with no further await.
3. `Counter` initial: `count 0, changes 0, parity even`. `BigList` initial: `items.count == 10000`,
   `items[0] == Item(1,"Item 1",0)`, `items[9999] == Item(10000,"Item 10000",0)`, `count == 10000`.
4. `observe(off)`: after turning observation off, `Todos.add("x")` delivers nothing to the mirror
   (200 ms); `observe(on)` again delivers one change-set with the **current** values (the added item).
5. Releasing a store (`close()` / `release`) drops `live_handles` by one.

### S09 transaction: a single change-set

1. `Counter` observed. Take `transactions` and `change_sets` readings `t0`. `counter.add(5)`: the mirror
   receives **three entries** for `count = 5`, `changes = 1`, `parity = odd`, and `transactions`
   grew by exactly **1**.
2. Three separate calls `increment, increment, decrement` grow it by exactly 3.
3. `counter.reset()` (two writes in one `ctx.txn`) grows it by exactly 1 and delivers `count = 0`,
   `changes = 0`, `parity = even`.
4. `Bench.bench_touch_signals(100)` grows it by exactly 1 and the mirror receives exactly 100 entries
   (signal ids `1..=100`, each a full value `1`); `bench_touch_signals(1)` delivers exactly 1 entry;
   `bench_touch_signals(1000)` delivers all 128.
5. Writes are not delivered while nothing observes the store: `Counter` not observed, `add(1)` grows
   `transactions` by 0; observing it afterwards delivers the current value once.

### S10 keyed patch

1. `BigList` observed through the raw mirror. The initial entry for `items` (signal 0) is a **full
   value** of 10,000 items.
2. `insert_at(5000, "fresh")` returns `10001`; the next change-set has an `items` entry with
   `op = 1` (keyed patch) of exactly **1 operation**, `Insert{index=5000, item=(10001,"fresh",0)}`,
   the entry's value is **under 100 bytes**, and the `count` computed entry is `10001`.
3. `update_at(42, "renamed")`: one `Update{index=42, item=(43,"renamed",1)}`.
4. `move_item(10, 9000)`: one `Move{from=10, to=9000}` (after the insert above the list has 10,001
   items).
5. `remove_at(0)`: one `Remove{index=0}`.
6. The runner applies each patch to its own mirror copy of the list and, after every step, compares
   it with the list of a freshly observed second `BigList`-equivalent: here simply with the expected
   model (a local array mutated by the same operations): equal in full, at every step.
7. `Bench.bench_list_insert(123)` is also a 1-operation patch on a 10,000-row list.
8. `reset()` after removing the first item yields one `Insert{index=0, item=Item(1,..)}` patch.

### S11 computed

1. `Todos` observed. `add("a")`, `add("b")`, `add("c")` (ids counting up); `toggle(b)`.
   `remaining == 2`, `visible == [a, b(done), c]`.
2. `set_filter(Done)`: the **one** change-set carries `filter = done` and `visible = [b]` (computed in
   the core; since ADR-039 `visible` is a derived list, so a raw mirror receives it as the keyed patch
   that leaves `[b]`), `remaining` does not change.
3. `set_filter(Active)`: `visible == [a, c]`. `set_filter(All)` restores all three.
4. `remove(a)`, `clear_done()`: `visible`/`remaining` follow.
5. `Counter.parity`: after `add(3)` odd, after `add(1)` even, after `add(-1)` odd, after `add(0)`
   odd (still); always consistent with `count`.
6. **Reads never cross the boundary**: reading `visible`, `remaining`, `parity`, `items` 1,000 times
   leaves `crossings.calls` unchanged.

### S12 query: fetch, stale, refetch

List `s12`; `configure_remote(base_url="https://playground.test")` once per process. Server `GET
/lists/s12/todos` answers `[{"id":1,"title":"Buy milk","done":false}]`; the runner counts GETs.

1. `RemoteTodosQueryHandle.create("s12")`: status goes `loading -> success` (`idle/fetching` while
   pending); `data == [Buy milk]`, `error == nil`, `fetching == false`, `updatedAt == manual clock now`;
   GET count `1`.
2. Clock `+10 s`. A **second** handle on `s12`: its `data` is there at once, GET count still `1`
   (fresh window is 30 s); both handles show the same data.
3. Clock `+31 s` (41 s since the fetch). A **third** handle: stale, so it fetches: GET count `2`. The
   server now answers with two items; all three handles end with two items and a newer `updatedAt`.
4. `refetch()` on any handle fetches although the data is fresh: GET count `3`.
5. `invalidate()`: marks stale and, being observed, refetches: GET count `4`.
6. The server now answers `503` with body `down`: `refetch()`; after the core's retry (1 retry, about
   1 s of backoff, real timer) `status == error`, `error == RemoteError.Status(code=503)`, `data` still
   holds the last good list.
7. The server answers with `not json`: `refetch()` -> eventually `error == RemoteError.BadBody(..)`.
8. Release all handles: `live_handles` is back to its earlier value.

### S13 optimistic mutation and rollback

List `s13`; the server serves `[Buy milk (id 1)]`; POST and PATCH replies are **delayed by 50 ms**. One
handle observes and **records every value of `data`**.

1. `create_remote_todo("s13", "Walk")` while the server answers `500` to POST: the call fails with
   `RemoteError.Status(code=500)`. Recorded `data`: `[milk]`, then `[milk, Walk(id=4294967295)]` (the
   placeholder, visible during the 50 ms), then `[milk]` again: **rolled back**. Final `status ==
   success`.
2. The server now answers `201 {"id":2,"title":"Walk","done":false}` and lists `[milk, Walk(2)]`
   afterwards: the call returns `RemoteTodo(2,"Walk",false)`; recorded: `..., [milk, placeholder], [milk,
   Walk(2)]` (optimistic, then the refetch after success). The POST carried an `Idempotency-Key` header
   (a UUID).
3. `set_remote_done("s13", 1, true)` while PATCH answers `500`: fails with `Status(500)`; recorded:
   `milk` with `done=true` (optimistic), then `done=false` (rollback).
4. `set_remote_done("s13", 1, true)` with PATCH `200`: succeeds; final list has `done=true` after the
   refetch (the server now lists it so).

### S14 offline queue replay

List `s14`; the server serves `[]`. A handle observes it. Before step 1 the runner waits until
`storage_status().queue_readable` is `true` (on Swift and Kotlin the harness failed the first read of the
queue, S20 step 4; the client reads it again after a backoff of about a second).

1. The test emits `Connectivity.changed(online=false, kind=None)` and waits 50 ms.
2. POST `/lists/s14/todos` is scripted to fail with `HttpError.Network("offline")`.
   `create_remote_todo("s14", "Offline item")` is started and **does not finish**: for 200 ms it is
   still pending; the server saw exactly 1 POST; the handle's `data` shows the placeholder.
3. A **non-idempotent** mutation does not queue: `set_remote_done("s14", 1, true)` with the PATCH route
   failing the same way fails **at once** with `RemoteError.Http(HttpError.Network("offline"))`: the server saw one
   PATCH for it, so the error is the first attempt's and not a retry's after its backoff (counted, not timed: the time
   is bounded only by the 5 s wait, a hang detector; a queued mutation never fails while offline, as step 2 shows).
4. POST is now scripted to answer `201 {"id":9,"title":"Offline item","done":false}` and the list
   route to `[{"id":9,...}]`. The test emits `Connectivity.changed(online=true, kind=Wifi)`.
5. The pending `create_remote_todo` **resolves** to `RemoteTodo(9,"Offline item",false)`; the server saw
   exactly 2 POSTs; both carry the **same** `Idempotency-Key`; the handle ends with `data == [id 9]`.
6. The Kv port saw a write of the key `undra.query.queue2` while offline (the queue is persisted, format 2
   of ADR-037: `format u16 = 2, schema_hash u64, count u32, items`, each item with its mutation's
   fingerprint) and the queue was emptied after the replay (the last operation on the key is a `delete`, or
   a `set` of a queue whose count, the `u32` at offset 10, is 0). A key `undra.types.<16 hex digits>` holds
   the description of `create`'s input (written before the queue that needs it).
7. **Build A queues what build B changes.** Offline again; POST `/lists/s14m/notes` fails with
   `HttpError.Network("offline")`. `save_note("s14m", "a")` and `tag_note("s14m", 7)` are started and stay
   pending (`storage_status().pending == 2`). The runner keeps the `Kv` contents (every key and value) and the
   `Idempotency-Key` header of the failed `save_note` POST, and stops build A (Swift and Kotlin: at the end of
   the run, see "Two builds").
8. **Build B reads it.** A core of build B is loaded with a `Kv` holding exactly those contents, offline: the
   runner emits `Connectivity.changed(false, None)` and calls `configure_remote` **before the queue is read**
   (configuration is not persisted, and a replay that started first would fail on an unconfigured endpoint;
   the TypeScript runner holds build B's first `get` of `undra.query.queue2` until then). Within 5 s `storage_status()` says
   `queue_readable`, `pending == 1` (`save_note` gained `pinned: Option<bool>`: migrated by parameter name),
   `migrated == 1`, `dead_lettered == 1`, and `dead_letters == ["tag_note: <reason>"]` with a reason that says
   its input does not migrate (`id` became a `String`); nothing was replayed (no request reached the server).
9. **Replay in build B.** POST `/lists/s14m/notes` answers 201; online: within 5 s the server saw exactly one
   POST with body `save:a` (build B's `save_note` with `pinned == None`) carrying the **same**
   `Idempotency-Key` as build A's attempt; `pending == 0`; the dead letter is still there (never lost, never
   replayed) and so is the key `undra.query.queue.dead` in the `Kv`.

### S15 snapshot and restore

1. `Todos` (observed) with `add("a")`, `add("b")`, `toggle(b)`; `Counter` (observed) with `add(5)`;
   `BigList` (observed).
2. `snapshot = core.snapshot()` (TypeScript: `await core.snapshot()`, the public API of SPEC 17.1);
   it is non-empty and opaque.
3. Mutate: `Todos.add("c")`, `toggle(a)`, `set_filter(Done)`; `Counter.add(10)`; `BigList.remove_at(0)`.
4. `core.restore(snapshot)` (TypeScript: `await core.restore(snapshot)`, which resolves after the restored
   values reached the stores, so the checks that follow need no waiting). The **same handles** still work:
   each store's mirror returns to the snapshot values (`todos == [a, b(done)]`, `filter == all`, `visible`
   likewise, `remaining == 1`, `count == 5`, `changes == 1`, `parity == odd`, `items.count == 10000` and
   `items[0].id == 1`), delivered as change-sets for the observed signals.
5. Identities continue: `Todos.add("d")` returns an id above `b`'s (no collision with `c`'s earlier id is
   required, none with `a` or `b`).
6. A **rejected snapshot** (16 random bytes) fails (`restore` throws / returns non-zero / rejects) and
   leaves every store as it was (TypeScript and Swift: `UndraRestoreError`, whose code is the core's, 5 for
   a malformed snapshot; the core is not closed and the next call works).
7. A handle released **before** the snapshot is not resurrected.
8. Stats: `live_handles` after the restore equals the count of surviving stores, plus the query handles
   that were live (a restore re-issues none that the snapshot does not name, and creates no handle of its
   own; S35 is the scenario about those).
9. A call in flight across a restore. `Probe` = new; start `probe.hang()`; wait until
   `counters().started == 1`; take a snapshot; `restore` it. The probe is not a store, so the restore
   invalidates it and cancels its call: `hang()` fails as **cancelled by the core** (Swift
   `UndraCallError.cancelledByCore`, Kotlin `UndraCallError.CancelledByCore`, TypeScript `UndraCallError.CancelledByCore`,
   `kind` `"cancelledByCore"`), not as a platform cancellation. Then, through the generated bindings,
   `probe.counters()` fails as bad request (`Refused`, stale handle) and `probe.reset()` (a command) returns
   normally and `onError` received `Probe.reset` with `Refused`. Close the probe.
10. A stream in flight across a restore. A new `Probe`; open `probe.ticks(1000000)` and read one item; take a
    snapshot and `restore` it. The core ends the stream with an error item carrying its own String
    (`"cancelled: ..."`, SPEC 5.9), and the consumer's iteration ends with **cancelled by the core** (the same
    three spellings as step 9), not with a decoding failure and not as a platform cancellation. (Kotlin and
    TypeScript used to read that String as a typed error; the stream mapping is one function per runtime now.)
    Close the probe.
11. **Build A's snapshots.** `Profile.new("ada")`, `visit()` twice; `describe() == "name=ada;visits=2"`; snapshot
    `P` (no `Legacy` exists yet). Then `Legacy.new(5)`; snapshot `L`. The runner keeps `P`, `L` and the
    `Profile` handle (Swift and Kotlin: for the build-B process). Release the `Legacy`.
12. **Build B restores `P` by name.** In a core of build B, `restore(P)` succeeds; the same `Profile` handle answers
    `describe() == "name=ada;visits=2;theme="`: build B reordered `visits` before `name` and added `theme` with
    `#[undra(default)]`, and the values arrived by name (SPEC 5.9, layout 2). A store type both builds share and
    did not change (the other stores of `P`, if the runner made any) takes the fast path.
13. **Build B refuses `L`.** `restore(L)` fails with the restore error whose code is **7** (`incompatible`:
    Swift `UndraRestoreError` code 7 / `.incompatible`, Kotlin `UndraRestoreException` code 7, TypeScript
    `UndraRestoreError` code 7), because `Legacy.score` changed from `i32` to `String` and no hook converts it;
    the core's Log port received an ERROR record naming `Legacy` and `score`; the core is unchanged (the
    `Profile` still describes as in step 12) and the next call works.
14. **A snapshot from before layout 2 is refused** (R7: what an older host kept is checked when it is
    loaded): the 8 bytes `00000000 00000000` (ADR-022's empty snapshot, `count, floor`) fail with code **5**
    (malformed) in either build, and the core is unchanged. (Live peers with different schema hashes still
    refuse each other at load: S16.)

### S16 schema mismatch rejection

1. `UndraCore.load` with `expectedSchemaHash = generatedHash ^ 1` fails **before the core is initialised**
   with the runtime's schema-mismatch error carrying `expected` and `got` (`got == generatedHash`)
   and a message that names both in hex (TypeScript `UndraSchemaMismatchError`, Kotlin
   `UndraSchemaMismatch`, Swift `UndraSchemaMismatchError`).
2. A subsequent load with `UndraIds.schemaHash` succeeds (the failed attempt did not leave the process
   half-initialised).
3. The hash in the bindings equals the hash the core reports (`stats().schema_hash`) and the hash of the
   schema the core exports (`undra_schema_hash`).
4. (TypeScript and Kotlin) the core's JSON schema (`undra_schema_json`) lists the playground's types
   (`Todos`, `Counter`, `BigList`, `Bench`, `Probe`, the queries) and the standard ports.
5. **No core loaded.** (Kotlin and Swift: first, before the load that sticks; the failed load of step 1 left
   nothing behind. TypeScript: the harness never makes a core shared, so it holds throughout.) `UndraCore.current`
   is nil/null; `UndraCore.shared` does not throw on access but is a closed placeholder; a generated call with
   the default core (`add(1, 2)`) and a generated constructor with the default core (`Counter` create) fail as
   **unavailable** (Swift `UndraCallError.unavailable(.closed)`, Kotlin `UndraCallError.Unavailable` whose
   transport reason is `CLOSED`, TypeScript `UndraCallError.Unavailable`, `transport.reason == "closed"`), with a
   message that says to load a core; a failure reported on the placeholder (`UndraCore.shared`'s `report`) only
   logs; `current` is still nil/null afterwards.

### S17 panic containment

**Native (Kotlin over JNI, Swift over the C ABI).** A panic unwinds to the boundary:

1. `explode("kaboom")` fails with reply status **panic** and a message containing `kaboom` (plus a
   backtrace string). The process is alive. Swift and Kotlin call the generated `explode` (Swift:
   `UndraCallError.panicked`, Kotlin: `UndraCallError.Panicked`, whose `panicMessage` contains `kaboom`).
2. `explode_later(10, "later")` (an async call) fails the same way (Swift `UndraCallError.panicked`, Kotlin
   `UndraCallError.Panicked`).
3. The core keeps working: `add(1, 2) == 3`; a store constructed before still updates;
   `stats().panics` grew by 2.
4. The Log port received a record with level >= 4 (error/fatal) and target `undra::panic` for each.
5. Re-entry is refused, not deadlocked or aborted: while the core is logging the panic of
   `explode("reenter")`, the runner's Log adapter (a synchronous port, called on the thread that holds
   the core lock) calls the generated `add(1, 1)` once. That call is **refused**, as a bad request whose reason
   contains `E_REENTRANT` (Swift `UndraCallError.refused`, the core's own refusal; Kotlin `UndraCallError.Refused`,
   raised by the runtime's guard, which answers with the same reply before the call reaches the core);
   `explode` itself fails as in step 1; afterwards `add(1, 2) == 3`.
6. Shutdown with a typed call in flight (it ends the core). Start
   `fail_later(5000, 1)`, wait 100 ms, shut the core down: the call fails as **closed** (Swift
   `UndraCallError.unavailable(.closed)`, Kotlin `UndraCallError.Unavailable` with transport reason `CLOSED`)
   in under half of the call's own 5 s delay (2.5 s after the shutdown; a shutdown that left the call to its timer
   would fail it 4.9 s later, or never). Then, on the shut-down core, the generated `add(1, 2)` fails the same
   way and `Counter.increment()` on a store of that core returns (`onError` received `Unavailable`, closed). The
   shut-down core is no longer the shared one: `UndraCore.current` is nil/null, and a generated constructor
   with the default core fails as in S16.5. The process is alive.
7. Closing ends the core's work, and a new load starts fresh (ADR-034; the last step of the run). Before
   step 6's shutdown a timer-paced core task is running (`Stress.start`); for 200 ms after the shutdown
   no port call reaches the runner's adapters (the runner counts the calls its Clock, Log, Http and Kv
   adapters receive). Then `UndraCore.load` with the same options succeeds again in the same process
   (Kotlin: `close()` reached the JNI `shutdown` of the bindings' `UndraCoreNative`; Swift: the table's `shutdown`), `stats()` of the
   new core reports no live handles, the generated `add(1, 2) == 3` runs on it, for 200 ms its Clock
   adapter receives no call (the new core runs no timer-paced task), and it closes cleanly. After each
   close, the native core reports no thread of its own still running: with no core loaded,
   the core's `stats_json` (Kotlin `UndraCoreNative.statsJson()`) says `runtime_threads == 0`. That is the check
   that sees a task which survived the shutdown: its port calls never reach the runner's adapters (Kotlin
   detaches the transport first; the native shutdown retires the port registrations and the Swift
   adapters are detached), so the windows alone cannot.

**wasm (TypeScript).** The shipped wasm profile aborts on panic (SPEC section 7), so containment means
the host survives and recovers:

1. Before the panic: `snapshot = undra_snapshot()` of a core with a `Todos` holding two items.
2. `explode("kaboom")` makes the call **fail** (it does not hang or crash the test process) with an
   `UndraError` whose message says the core trapped; `core.closed` is true and `onClose` fired; the Log
   adapter received a **fatal** (5) record containing `kaboom` before the trap.
3. The page can **restart**: a fresh `UndraCore.load` of the same module succeeds and the snapshot restores:
   `Todos` (re-created from the restored handle) shows the two items.
4. A second core loaded in the same process before the panic was not affected.
5. On the trapped core, calls through the generated bindings reject with `UndraCallError.Unavailable`
   (its `transport.reason` is `trap` or `closed`) and none hangs: an async call (`add_later`) and a typed one
   (`parse_count`); a store command (`Counter.increment`) resolves and `onError` received `Unavailable`.
6. Worker mode: boot the playground core with `mode: "wasm-worker"` (an in-thread worker over a
   `MessageChannel` is enough in the vitest harness, because the harness cannot load TypeScript in a real
   worker thread; the real thread is covered by the undra-ffi acceptance test,
   `crates/undra-ffi/tests/wasm/ts-runtime.test.mjs`). Observe a `RemoteTodosQueryHandle` (it reads the Clock
   on every observe) against the harness `FakeServer` (Http is an async port: it crosses to the main thread)
   and call `create_remote_todo` (it generates an idempotency key through the Rng). The core does not trap:
   `core.closed` stays false, the data arrives (the list, then the created item), and the core's own log
   records reach the harness Log adapter. (A worker that answered every port call "async" left the first
   clock read without an answer, and the core trapped.)

### S18 coalesced burst

The core commits one change-set per transaction; the platform mirror merges what arrived before it
drains (ADR-031, SPEC section 11). `Stress.burst(mode, n)` commits `n` transactions in a tight loop,
one write each, without `ctx.txn`. The calls are made **from the main thread** (Swift: the main
actor; Kotlin: `UndraDispatchers.main`), where a synchronous call drains the mirror before it
returns; TypeScript awaits them.

1. Raw: `Stress` constructed and observed through a raw mirror callback (registered without
   `no_coalesce` ids). Take a `transactions` reading. `burst(Firehose, 1000)`: when the call returns
   (resolves), **with no further wait or flush**, the raw callback has run **exactly once**, with one
   full value (`op = 0`) for signal `0 value` equal to `1000`; `transactions` grew by exactly
   **1000**; the mirror's counters say `changeSetsReceived` grew by 1000 and `entriesApplied` by 1.
2. Through the generated class: `Stress.create()` / `Stress()`, `burst(Firehose, 1000)`: `value ==
   1000` as soon as the call returns (read-your-writes).
3. Through the generated class, `no_coalesce`: `burst(Progress, 10)`: `progress == 10` as soon as the
   call returns, and the mirror applied all 10 entries (`entriesApplied` grew by 10), because the
   generated store registered `progress` as `no_coalesce`. (TypeScript also checks that a subscriber
   heard `1, 2, ..., 10`; a Kotlin `StateFlow` conflates and SwiftUI renders once per frame, so they
   check the mirror's counter only.)

### S19 derived keyed list

`Todos.visible` is a derived list (ADR-039): it reaches the platform as keyed patches, never as a whole
list after the first; a derived list's patches are ordinary SPEC 3.8 patches, so every runtime's decoder,
applier and mirror (ADR-031) apply them unchanged.

1. `Todos` observed through a raw mirror: the initial `visible` (signal 2) entry is a full value
   (`op = 0`), `[]`.
2. `add("a")`, `add("b")`, `add("c")`: each change-set's `visible` entry is `op = 1` with exactly one
   `Insert` at index 0, 1, 2; `remaining` is 1, 2, 3.
3. `toggle(b)` with filter `All`: `visible` is one `Update{index=1}` (b, done); `remaining == 2`.
4. `set_filter(Active)`: the change-set's `visible` entry is one `Remove{index=1}`; `set_filter(All)`: one
   `Insert{index=1, b}`.
5. Under `Active`, `toggle(a)`: one `Remove{index=0}`; `toggle(a)` again: one `Insert{index=0, a}`.
6. `fill(10000)` (one full value or one patch for `visible`: either is allowed), then `toggle` of a visible
   item: the `visible` entry is `op = 1`, one op, **under 100 bytes**.
7. Through the generated class (`Todos.create()` / `Todos()`), the same steps and `fill`, `remove` and
   `clear_done`: `visible` equals the model (a local list mutated by the same steps) after every step,
   read-your-writes as in S18.
8. Reads never cross: reading `visible` 1,000 times leaves `crossings.calls` unchanged.
9. **Recorded patches.** `contract-tests/derived-vectors.sh` writes (with
   `cargo run -p undra-signals --example derived_vectors`) a recording of 60,000 seeded operations over
   three derived views (a filter; a parameter filter and a sort with heavy ties; a map and a sort on a
   `String`), which the Rust side checks against `filter + stable sort` after every operation: the
   change-sets a host received (about 24,000) and each view's FNV-1a 64 hash after each of them. The
   runner decodes and applies every change-set with its runtime's own change-set decoder, `decodePatch`
   and `applyPatch`, and after **every** change-set the hash of each view's encoding equals the recorded
   one. (TypeScript also feeds the change-sets through a `Mirror`, drained at seeded points, and checks
   at every drain: what ADR-031 merges per drain applies to the same views.)

### S20 storage failures are typed (ADR-049)

The storage ports have an error channel: an adapter that cannot store answers `StorageError`, and the core
neither panics nor traps (a wasm core would have trapped before ADR-049). List `s20`; the server serves one item.

1. The harness `Kv` fails every `set` with `StorageError.Full`. `remote_todos("s20")` is observed; it shows the
   item. After 300 ms (the write is debounced 250 ms) no key `undra.query.cache2.<query id>.*` of the `s20`
   entry is in the `Kv`; `storage_status().write_failed` grew by at least 1; the `Log` port received a WARN
   record of target `undra::query` whose message contains `storage is full`, exactly once however many writes
   failed; `stats().panics` did not grow and the core answers the next call. (In a core that has not yet
   stored the description of the query's type, the first failing write is the key `undra.types.<fingerprint>`,
   written before the entry, and the entry's own write is not attempted: either way no entry key is stored.)
2. The `Kv` heals. `invalidate()` on the handle refetches; within 5 s the `Kv` holds the `s20` entry, a value
   whose first two bytes are `02 00` (format 2) and whose fingerprint (bytes 10 to 18) is the one of the key
   `undra.types.<fingerprint>` it also holds. The data still shows.
3. A failed read of a cache entry starts it empty: (TypeScript) a fresh core whose `Kv` holds that entry and
   fails one `get` with `StorageError.Io("busy")` shows no data for `s20` until it fetched, and the entry's key
   is still in the `Kv` afterwards; (Swift, Kotlin) not run, the core is not reloaded.
4. An unreadable queue is never overwritten (ADR-049 decision 1.4).
   * TypeScript: a fresh core is loaded with a `Kv` whose `get` of `undra.query.queue2` fails `Locked`, and is
     told it is offline. `storage_status().queue_readable == false`. `save_note("s20", "late")` (POST failing
     with `HttpError.Network("offline")`) stays pending in memory: for 500 ms the `Kv` sees **no** `set` of
     `undra.query.queue2` and `pending == 1`. The `Kv` heals; `Lifecycle.changed(Active)`: within 5 s
     `queue_readable`, a `set` of `undra.query.queue2` (count 1); online: counted from the online event, the note
     replays exactly once (body `save:late`; its first attempt, made while offline, is not counted) and
     `pending == 0`.
   * Swift and Kotlin: the harness failed the first `get` of `undra.query.queue2` with `Locked` at load. Now
     `queue_readable == true`, the `Kv`'s operation log shows the failed `get`, then a successful `get` of the
     key, and no `set` of the key between the two.
5. No step of this scenario panicked or trapped (`stats().panics` unchanged), and the core is the one loaded
   at the start (TypeScript: the fresh cores of steps 3 and 4 are closed).

### S21 worker mode answers synchronous ports in the worker (ADR-049; TypeScript only)

The playground core in `wasm-worker` mode, loaded with `worker.ports` pointing at a module that registers the
playground's `Locale` port (`hello()` answers `"Hola"`).

1. `remote_todos("s21")` observed: it shows the server's data and `updated_at` is within a minute of `Date.now()`
   (the core read the `Clock` in the worker); `create_remote_todo("s21", "x")` reaches the server with an
   `Idempotency-Key` that is a UUID v4 made from the worker's `crypto.getRandomValues` (two calls carry
   different keys); a log record the core wrote reaches the main thread's `Log` adapter. Nothing trapped.
2. `localized_greeting("Ada") == "Hola, Ada"`: the app's synchronous port answered in the worker.
3. A second load in `wasm-worker` mode that registers `Locale` on the **main thread** (`adapters` or
   `registerPort` before load) fails at load with an error that names `Locale` (a generated adapter carries its
   port's name; a hand-written `PortImpl` without one is named by its id) and says to register it in
   `worker.ports`; no core is left running.
4. The worker protocol is version 3: the `init` message carries `asyncPorts` (the host's asynchronous ports, `Http`
   and `Kv` among them) and the `portsModule` URL.

### S22 a trapped web core restarts from its last snapshot (ADR-049; TypeScript only)

`wasm-main`, loaded with `recovery: { snapshotEveryMs: 50, maxRestarts: 3, perMs: 60_000 }`, `onCoreRestarted`,
`onError` and `onClose` recorded.

1. `Counter` observed, `add(5)`; `remote_todos("s22")` observed through its generated handle, showing the server's
   one item. Wait until the `s22` entry is persisted in the `Kv` (its write is debounced 250 ms; the re-created
   handle of step 4 reads it back) and a snapshot was taken.
2. A call is started and left in flight (`add_later(1, 2)` with a delay, or a `Probe.hang()`), then
   `explode("kaboom")` is called: it rejects as a failed call, the in-flight call rejects with
   `UndraTransportError("restarted")` (through generated code: `UndraCallError.Unavailable` whose transport reason is
   `"restarted"`), never retried.
3. Within 5 s `onCoreRestarted` was called once with the panic report (message containing `kaboom`), a
   `restoredFromAgeMs` that is not null and under the 5 s wait (a hang detector: the age is the runner's own time
   since step 1's last change, about half a second, so a tighter bound would be about the machine),
   `rejectedCalls >= 1` and the stale objects; `onError` received an `UndraCoreRestarted` with the same.
4. The same `Counter` wrapper shows `count == 5` (restored, same handle) and `add(1)` makes it 6; the
   `remote_todos("s22")` handle is **the handle it was before the trap** (the snapshot kept what it is made
   of and the core re-issued it, ADR-059; the runtime runs no code of its own for it): its wrapper still shows
   the item and, after the runner calls `configure_remote` again, since configuration is core state outside
   stores and is lost with the instance that trapped, `invalidate()` refetches through it; the `Probe` (not a
   store) is stale and fails with a typed refusal.
5. Three more `explode` calls within the minute: the third restart is the last; the fourth trap leaves the core
   dead: `onClose` reports the trap, `onCoreRestarted` was called 3 times in all, and calls fail as unavailable
   (`closed`).

### S23 WebSocket (ADR-047)

The opt-in `WebSocket` port through the **platform's default adapter** (Swift `URLSessionWebSocketTask`, Kotlin
the runtime's own client, TypeScript `nodeWebSocket()`: Node's `http` upgrade with the runtime's own framing, because
Node's global `WebSocket` hides a refusal's status, `ts/NOTES.md`) against the shared local server
`contract-tests/servers/realtime-server.mjs` (started once per run; `WS` is its `ws://127.0.0.1:<port>`). The
core's side is `ws_echo` and the `Live` object of `examples/playground/core/src/live.rs`.

1. Echo: `wsEcho("WS/ws/echo", [Text "a", Binary [1, 2, 3], Text "é"])` returns the same three messages in
   order; the server's `/stats` shows that connection closed by the client with `(1000, "done")`.
2. Subprotocol and headers: `live.connect("WS/ws/headers", ["v2", "v1"], [Header("X-Token", "t")])` returns
   `"v2"`; `live.read(1)` is one text message, the JSON of the upgrade's headers, with `"x-token": "t"`;
   `live.send(Text "ping")`, `live.read(1)` is `[Text "ping"]`; `live.disconnect(4000, "bye")`: the server
   saw `(4000, "bye")`.
3. Credit (SPEC 3.7, ADR-047 §3): `live.connect("WS/ws/flood?n=1000", [], [])`; `live.read(5)` is `"0"`..`"4"`;
   for 200 ms nothing is read; then `live.pulls()` is **at most 2** (one pull of 16 answered what the core holds;
   a core that stopped reading pulls no more). `live.read(995)` is `"5"`..`"999"` in order, and `live.read(1)`
   fails with `WsError.Closed(code 1000, reason "end")`.
4. Typed ends: `wsEcho("WS/ws/deny?status=401", [Text "x"])` fails with `WsError.Refused` (status 401 where the
   platform reports it: Swift, Kotlin and Node do); `live.connect("WS/ws/close?code=4001&reason=kicked")`,
   `read(1)` is `["hello"]`, `read(1)` fails with `Closed(4001, "kicked")`; `live.connect("WS/ws/drop")`,
   `read(1)` is `["hello"]`, `read(1)` fails with `WsError.Network`.
5. A connection nobody closes: `live.connect("WS/ws/stall")`, `live.abandon()`: the server saw the client close
   with **1001** (going away), within the 5 s wait (a hang detector: the port closes a dropped connection fire and
   forget, on no timer, so one that did not would leave it open).

### S24 server-sent events (ADR-047)

The opt-in `Sse` port through the platform's default adapter (Swift `URLSession.bytes`, Kotlin
`java.net.http` on the JVM, TypeScript `fetch` with a body stream) against the same server (`HTTP` is its
`http://127.0.0.1:<port>`); the core's side is `sse_follow`.

1. `sseFollow("HTTP/sse/feed", null, 10)` returns `ended = true` and four events, parsed as the HTML standard
   says: `{id "1", event "message", data "one", retry 1500}`, `{id "2", event "tick", data "two\nlines"}`,
   `{id "2", event "message", data "three"}` (an event without `id` keeps the last one), `{id "4", event
   "message", data "four"}` (CRLF line ends); the comment and the event without data are not events. The
   server saw no `Last-Event-ID`.
2. Resume: `sseFollow("HTTP/sse/feed", "2", 10)` returns `ended = true` and the events after id 2 (`three` with
   id `"2"`, `four` with id `"4"`); the server saw `Last-Event-ID: 2`.
3. A reader that stops: `sseFollow("HTTP/sse/feed", null, 2)` returns two events and `ended = false`;
   `sseFollow("HTTP/sse/hang", null, 0)` returns no event and `ended = false`, and the server saw the client leave,
   within the 5 s wait (a hang detector: the server never ends `/sse/hang`, and the port leaves it when the core
   closes the subscription, on no timer).
4. Typed failures: `HTTP/sse/status?code=204` and `?code=500` fail with `SseError.Refused` with that status;
   `HTTP/sse/html` fails with `SseError.Protocol`.

### S25 Db (ADR-048)

The opt-in `Db` port through the platform's **real SQLite adapter** (Swift the SQLite3 C API, Kotlin JDBC on the
JVM, TypeScript `node:sqlite`), each rooted in a fresh temporary directory; the core's side is `Notes`,
`db_cells`, `db_run` and `db_migrate` of `examples/playground/core/src/notes.rs`.

1. `Notes.create()`; `notes.open("contract-s25")` returns 2 (two migrations ran); `notes` is empty.
2. `add("milk")`, `add("eggs")` return ids 1 and 2 and the mirror holds both (the second arrives as a keyed
   patch; the first, onto an empty list, as a full value: no key overlaps, SPEC 3.8);
   `toggle(1)`: note 1 is done; `count()` is 2.
3. Every storage class there and back: `dbCells(-9007199254740993, 1.5, "é😀", [0, 255, 7], null)` returns the
   same five values (the integer is outside JavaScript's safe range: it crosses as `i64`/`bigint`) and the types
   `["integer", "real", "text", "blob", "null"]`.
4. Constraints, typed: `addWithId(1, "dup")` fails `DbError.Constraint` with kind `Unique`; `addAll(["a", null])`
   fails `Constraint` with kind `NotNull`, and afterwards `count()` is still 2 and the mirror still holds two
   notes (the transaction rolled back); `addAll(["a", "b"])` returns 2 and `count()` is 4.
5. SQL errors, typed: `dbRun(":memory:", "INSERT INTO missing VALUES (1)")` and `dbRun(":memory:", "SELEC 1")`
   fail with `DbError.Sql`.
6. Migrations run in one transaction: `dbMigrate("contract-s25-m", true)` fails with `DbError.Migration` of
   version 2; `dbMigrate("contract-s25-m", false)` then returns 2 (had migration 1 survived the failed open,
   its `CREATE TABLE a` would now fail).
7. Persistence: `notes.closeDatabase()`; a new `Notes` store `open("contract-s25")` returns 2 and its mirror
   holds the four notes, note 1 done.
8. `open("../escape")` on a third store fails with `DbError.Unavailable`.

### S26 two cores

ADR-044: a process can hold several cores, each its own image reached through its own table
(`<namespace>_undra_api`), with nothing shared between them. The core under test is the playground core
built twice under two namespaces, `playground_a` and `playground_b` (`examples/two-cores/a` and `b`: the
same source, two libraries, two generated packages, `UndraPlaygroundA` and `UndraPlaygroundB`), loaded
next to each other (and next to the runner's playground core on the native platforms) with the
harness adapters.

1. Both load through their generated entries (`UndraPlaygroundA.load(..)`, `UndraPlaygroundB.load(..)`),
   with the bindings' schema hash and no `expectedSchemaHash` written by the runner: two open cores, two
   different objects; each entry's `core` is its own; each core's `schema_hash` statistic is the bindings'
   hash, and the bindings say their namespace (`UndraIds` namespace `playground_a` / `playground_b`).
   (Native: the C ABI version is 2, the same image is never loaded twice.)
2. A call on each: `add(2, 3)` is `5` through either core; the generated function's default core is its
   package's own (`add(2, 3)` with no core argument goes to `A` for package A, to `B` for package B,
   which their `calls` counters show).
3. An observed change on each, independent: a `Counter` created in each core (`Counter.create()` /
   `Counter()` with no core: the package's own); `add(2)` on A's, `add(5)` on B's: A's `count` is `2`
   and B's is `5` as soon as the calls return, each core delivered its change-set to its own mirror
   only (A's mirror counted one more change-set, B's did not move for A's write).
4. Independent statistics: A's `live_handles` grew by the counter A made and B's by the one B made;
   each generated object carries its own core; releasing A's counter takes A's count back and leaves
   B's (and B's counter keeps its value). Two cores with the same history issue the **same handle
   numbers** (a handle is a slot and a generation of one core's table), so a handle means something only
   with the core that issued it; the bindings never pass one core's handle to another.
5. One shut down while the other keeps working: close A. B's counter takes `add(1)` (`count == 6`) and
   `add(2, 3)` through B is still `5`; a call through A's (closed) core fails as unavailable
   (`UndraCallError.Unavailable` / `.unavailable`); A's entry's `core` is the closed placeholder.
   (Native: A's image reports no running core and `runtime_threads == 0`, while B's still runs.)
6. A namespace loads once: loading B again while it is loaded is refused (TypeScript: an `UndraError`
   of kind `state`; native: the runtime's in-process claim), and A, closed, loads again and answers
   `add(2, 3) == 5`. Both are closed at the end.

### S27 objects cross (ADR-040)

`Workshop` is a store that hands out child stores, `Shelf`s (`examples/playground/core/src/workshop.rs`).
Every step goes through the **generated** classes: `Workshop.create()` / `Workshop()`, `Shelf`, `Watch`.
`stats().host_refs` is the sum of the references the host owns (the core's view of the live wrappers).

1. **A parent returns a child.** `w.shelf("a")` returns a `Shelf` wrapper (a store: its `label` is `"a"` and
   `items` is `0` after the first drain, observed when the wrapper was made); `stats().live_handles` and
   `host_refs` each grew by one for the shelf (and one for `w`).
2. **One object, one handle, one wrapper.** `w.shelf("a")` again is the same wrapper (`===` / `===` / identity),
   the same handle, and `host_refs` did not move (the duplicate reference was given back at once); `w.find("a")`
   is the same wrapper, `w.find("zz")` is `nil`/`null`; `w.shelves()` lists it once; a store returned twice is
   mirrored once: `shelf.stock(2)` delivers exactly one change-set (`items == 2`), not two.
3. **A child is passed back as a parameter.** `w.shelf("b").stock(3)`; `w.merge(from: a, onto: b)` moves the two
   items (`a.items == 0`, `b.items == 5`: `merge` is one `txn` over two stores, so two change-sets, one per store, that
   one drain applies before `merge` returns); `w.total([a, b])
   == 5`; `w.describe(a) == "a"` and `w.describe(nil) == "none"`. A parameter is borrowed: `host_refs` is
   unchanged by these calls.
4. **Closing releases exactly one reference.** `w.shelf("c")` twice (one wrapper, one reference); `close()` the
   wrapper: `host_refs` is back to where it was before step 4 and its handle is stale: `stock(1)` on the closed
   wrapper fails as `refused` (a command: reported through `onError`, not thrown). `w.shelf("c")` again is a
   **new** wrapper with a new handle (not the closed one) whose `label` is `"c"`.
5. **A call cancelled before it finishes owes nothing.** `w.open("slow", 60_000)` (async, waits on the Timer) is
   started and cancelled before its reply: it fails as cancelled, and `host_refs` and `live_handles` are
   unchanged. `w.open("fast", 10)` returns the shelf (`label == "fast"`); `w.open("", 0)` fails with
   `WorkshopError.NoName`; neither leaks.
6. **A restore makes derived handles stale.** A snapshot of the core taken while `a` (a child) is live holds
   `Workshop` but **not** the shelves (derived handles are transient); after `restore`, the old `Workshop` handle
   is the same (it is a store) and still answers; the old `a` wrapper is stale (its `stock(1)` is refused); and
   `w.shelf("a")` returns a **fresh** shelf wrapper with `items == 0` (the restored workshop rebuilt its private
   state empty, SPEC 5.9).
7. **Releasing is by handle and finalisers.** Drop (do not `close()`) the last strong reference to a wrapper (and
   collect it: Swift releases at `deinit`, Kotlin on GC with `System.gc()` and a bounded wait, TypeScript with
   the `FinalizationRegistry` when `gc` is exposed, else `close()`): `host_refs` goes back down within 5 s.
8. **A foreign object is refused with a typed error, and the cores' handle numbers are not mixed up.** (Native
   runners and TypeScript use the two cores of S26: the generated classes of `UndraPlaygroundA` with
   `ctx: coreA` and `ctx: coreB`.) Both cores issue the same handle numbers for the same history, so a handle
   alone proves nothing: `wA.merge(from: shelfOfB, onto: shelfOfA)` fails with `UndraCallError.refused` (a
   message that names the shelf and says it belongs to another core) **before anything is sent**: neither
   core's `calls` counter moved and both `host_refs` are unchanged.
9. **Statistics.** `stats()` reports `host_refs` (the sum), and a raw call that gives a handle back twice
   over-releases nothing the core still counts (the runtimes release at most once per wrapper).

### S28 host callbacks (ADR-041)

`Workshop` calls the app's `Reporter` back (`progress`, `note`, `confirm`). The runner implements `Reporter`
(generated protocol/interface; on Swift the main-actor protocol) and records what it is called with, in order,
with the stores' state as seen at each call. `UndraCore.callbacks` (the registry) is read for its live count.

1. **Ordered, and after the change-sets committed before them.** `w.watch(rep)` returns a `Watch`;
   `w.announce("one")`, `w.announce("two")`: `rep.note` is called with `"one"` then `"two"`, and when it runs
   the observed `Workshop.notes` is already `1` / `2` (the change-set committed before the call is applied
   first, ADR-041 decision 6). Nothing ran inside the core's callback: the runner's `note` calls back into the
   core (`w.announce` from inside `note` is allowed: it is queued, never `E_REENTRANT`).
2. **An async callback returns a value and throws its typed error.** `await w.run(3, rep)` with `rep.confirm`
   answering `true` returns `3` (the notes `step 1 of 3` to `step 3 of 3` arrive in order, and the progress
   reports arrive in order and end at `3/3`: `progress` is `coalesce`, so a drain that ran late delivers fewer
   than three);
   with `confirm` answering `false` the call fails with `ReportError.Declined`; with `confirm` throwing
   `ReportError.Unavailable("x")` the call fails with that error (`status 1`).
3. **Any other throw is reported, not propagated.** `confirm` throws something that is not a `ReportError`
   (Swift: another `Error`; Kotlin: `IllegalStateException`; TypeScript: a `TypeError`): the host reports it
   through `onError` (operation names the callback) and answers unavailable, so `w.run` fails with
   `ReportError.Unavailable(..)`; a throw from `note` is reported the same way and goes nowhere else.
4. **Cancellation reaches the host task.** `w.run(1, rep)` with a `confirm` that waits until cancelled (it
   records that its task / job / signal was cancelled) is started, and the call is cancelled: the host's
   `confirm` observes cancellation (Swift `Task.isCancelled` / `CancellationError`, Kotlin `Job` cancelled,
   TypeScript the method's `AbortSignal` aborted) within the 5 s wait (a hang detector: `confirm` waits for its
   cancellation and nothing else, so one that never reached it would leave it waiting); a `confirm` answer that
   arrives late is discarded; no `ReportError` leaks; the registry still holds the instance until the proxy is gone.
5. **Interning and the registry.** `w.watch(rep)` twice with the same `rep`: one instance handle
   (`callbacks.lend` returned equal handles), the registry count for `rep` is 2 after the crossings and **1**
   once the core gave the duplicate back (`__release`), and `w.watching() == 2` (two subscriptions, one
   proxy). Closing both `Watch`es: the core dropped the last proxy, the registry is **empty** (within the 5 s wait,
   a hang detector: a proxy the core did not drop holds its reference for good), and
   `rep` is no longer held (a weak reference to it clears after a GC).
6. **`coalesce` delivers only the newest per drain.** `w.burst(50, rep)` called while the runner holds the
   frame (the mirror's drain is not run): when it runs, `rep.progress` is called **once**, with `(50, 50)`,
   and `rep.note` 50 times in order (`burst 1` to `burst 50`); the invocation of `progress` was not folded with
   change-set entries (S18 still holds).
7. **A refused call leaves the registry unchanged.** `w.watch(rep2)` on a closed (stale) `Workshop` wrapper
   fails as `refused`; `rep2` is not in the registry afterwards (the generated code gave its reference back),
   and the core sent no `__release` for it.
8. **Weak by default? No: strong, with the weak wrapper available.** An inline `Reporter` object that nothing
   else references (`w.watch(Inline())`) is still called by `announce` after a GC (the registry holds it
   strongly); `WeakReporter(target)` (Swift) / `Reporter.weak(target)` (Kotlin) / `weakReporter(target)`
   (TypeScript) forwards while the target lives and does nothing (fire-and-forget) or answers unavailable
   (async) once the target is gone.
9. **A `background` callback is not delivered through the drain.** (Runners that have a background-delivery
   interface in the playground only: none yet; the golden case `callbacks` and each runtime's own tests cover
   `background`.)

### S29 panic report (ADR-046)

The playground core's panics, seen by the app's crash reporter. Every platform sets `LoadOptions.onPanic`
(Swift `onPanic`, Kotlin `onPanic`, TypeScript `onPanic`) and records each `UndraPanicReport` (message, location,
operation, thread, frames, namespace, coreVersion, schemaHash, imageId), with a note of the thread it was
delivered on.

**Native (Kotlin over JNI, Swift over the C ABI)**

1. `explode("kaboom")` fails as in S17.1 (status 2). `onPanic` received **exactly one** report: `message`
   contains `kaboom`; `location` contains `lab.rs:` and ends in `:<line>:<column>`; `operation == "explode"`;
   `thread` is not empty; `namespace == "playground_core"`; `coreVersion` is not empty; `schemaHash` is the
   bindings' hash; `imageId` is empty or lowercase hex; `frames` is not empty (the runners load debug builds, which
   name their frames: some frame has a `symbol`), and every frame's `address` is a `UInt64`/`Long`/`bigint`.
2. `explode_later(10, "later")` reports once, `operation == "explode_later"`; a **detached task** that panics
   (`explode_detached("task")`, which returns at once) reports once, `operation == "task"`.
3. `onPanic` is called on the platform's main thread (the Swift main actor's thread, the Kotlin runner's `main`
   executor, the JavaScript thread), once per report and in the order the panics happened; the call that
   panicked still fails as in S17.
4. `stats().panics` and `stats().panicReports` both grew by 3 (the stats report `panic_reports`), and the core keeps
   working: `add(1, 2) == 3`. A reporter that throws changes nothing: the next panic is reported all the same
   (the runner's `onPanic` throws once, on the first report, in a second `load` of the S17 kind: Swift cannot
   throw there, so its `onPanic` is `@Sendable (UndraPanicReport) -> Void` and the step is Kotlin and TypeScript).

**wasm (TypeScript).** The core traps, so the report is built from the core's FATAL record and the trap:

1. `explode("kaboom")` makes the call fail as in S17 (wasm); `onPanic` received **exactly one** report **before**
   the restart of S22 begins (`onCoreRestarted` fires after it): `message` contains `kaboom`, `location` contains
   `lab.rs:`, `operation == "explode"` (the call the core was running, from the FATAL record), `thread == "main"`,
   `namespace == "playground_core"`, `coreVersion` not empty, `schemaHash` the bindings', `frames` not empty (each
   `address` the module offset of a `wasm-function[i]:0x..` line, a `symbol` when the build has names) and
   `imageId` is the SHA-256 of the module's bytes as 64 lowercase hex digits.
2. The same report arrives with recovery on (S22) and with it off, and `onPanic` throwing is reported to `onError`
   and changes nothing.

### S30 background run (ADR-046)

`runInBackground(deadline)` (Swift `core.runInBackground(deadline:)`, Kotlin `core.runInBackground(deadlineMs)`,
TypeScript `core.runInBackground(deadlineMs)`) over the standard function `run_background`. List `s30`; the
server serves `[]`; a handle observes it.

1. **Offline work is pending.** `Connectivity.changed(online=false, kind=None)`; POST `/lists/s30/todos` fails with
   `HttpError.Network("offline")`; `create_remote_todo("s30", "Queued")` is started and stays pending
   (`storage_status().pending == 1`). `stats().background.tasks >= 3` and `stats().background.pending >= 1`: the
   platform can tell that a window is worth asking for. Emitting `Lifecycle.changed(Background)` writes a cache
   entry that waits out its 250 ms persistence debounce at once, not when the debounce fires. Shown against the
   debounce's own clock: once the first fetch's entry has been written (its own debounce waited out), the server
   serves the list with new contents (`[{"id":1,"title":"Server <n>","done":false}]`) and the handle refetches it,
   which makes the entry dirty and arms its debounce after an instant `armed` read just before; after `Background`
   the Kv holds a new write of the list's key (`undra.query.cache2.<query id>.<fnv1a64 of the argument>`) less than
   240 ms after `armed` (10 ms short of the debounce, for the clocks' granularity), which no debounce armed after
   `armed` can have written. A trial in which the machine was slower than that (the key written before
   `Background`, or seen 240 ms or more after `armed`) proves nothing and is repeated with the next `n`, up to five
   times; a core that leaves the entry to its debounce fails all five. Then `Lifecycle.changed(Active)`, and the
   server serves `[]` again.
2. **Still offline, the run says so and does not wait.** `runInBackground(5 s)` returns in under half its deadline
   (2.5 s; a run that waited would return at the deadline less the half second kept for the host, 4.5 s) with
   `finished == false`, `replayed == 0`, `stillPending >= 1`; the item is still queued.
3. **Online, the run drains the queue.** POST answers `201 {"id":9,"title":"Queued","done":false}` after a delay of
   400 ms. `Connectivity.changed(online=true, kind=Wifi)` starts the replay; `runInBackground(10 s)` started
   right after it returns once the replay finished: `finished == true`, `replayed == 1`, `stillPending == 0`; the
   pending `create_remote_todo` resolved; the server saw 2 POSTs with the same `Idempotency-Key`.
4. **A run cut at its deadline leaves the work intact.** Offline again; `create_remote_todo("s30", "Slow")` queued
   (`pending == 1`); POST answers `201 {...}` after 5 s; online. `runInBackground(1 s)` returns in about
   500 ms (the deadline less the half second kept for the host): after more than 300 ms, and **less than 400 ms after
   a 500 ms timer armed beside the call** (a machine that is slow fires both late; a run that kept no half second or
   waited for the POST ends 500 ms or more after it), with
   `finished == false`, `replayed == 0`, `stillPending == 1`: the replay in flight is the client's, not the run's, so
   the item is still queued (`pending == 1`, the Kv queue key still holds it) and **nothing is sent twice**; within
   10 s the POST answers, `pending == 0`, and the server saw exactly one POST for it after the failed one, with the
   same `Idempotency-Key`.
5. **A host that cancels the call.** `runInBackground(30 s)` while a replay is held (offline, then online with a
   POST that answers after 3 s) is cancelled after 100 ms (Swift: the `Task` is cancelled, Kotlin: the coroutine,
   TypeScript: the `AbortSignal`): the call fails with the platform's cancellation, without waiting for the replay in
   flight (Kotlin checks the held creation is still unanswered when the cancel returns), the core keeps working
   (`add(1, 2) == 3`) and the queue is intact (`pending == 1` until the POST answers, then 0).
6. **Counters.** `stats().background.runs == 4`, `.finished == 1`, `.replayed == 1`.

### S31 newtypes, generic instantiations and leaf types (ADR-042)

`ledger` (`examples/playground/core/src/ledger.rs`): free functions (`open_account`, `deposit`, `balances`, `statement`,
`loadable_statement`, `echo_*`, `sample_receipt`) and the `Ledger` store. Every step goes through the **generated** API.

1. **A newtype is its inner value on the wire.** The codec of `AccountId` writes the same 16 bytes as the `Uuid` codec of the same
   value, `Cents(-5)` the same 8 bytes as an `i64`, `Price` the same 17 bytes as a `Decimal`; `echo_account(id)` returns an equal
   `AccountId`, `echo_cents` an equal `Cents`, `echo_price` an equal `Price`. The platform type is a distinct type
   (Swift `struct AccountId: RawRepresentable` with `rawValue: UUID`, Kotlin `@JvmInline value class`, TypeScript a branded
   `string`/`number` that is `===` to its inner value at run time); `Cents` is `Comparable` on Swift and Kotlin
   (`Cents(1) < Cents(2)`).
2. **A newtype is a map key and the key of a keyed list.** After `open_account` twice, `balances()` is a dictionary/map with
   exactly those two `AccountId` keys (Swift `[AccountId: Price]`, Kotlin `Map<AccountId, Price>`, TypeScript
   `Map<AccountId, Price>`). `Ledger` (a store whose `accounts` list is keyed by an `AccountId`): `open("Ada")` delivers one
   change-set whose `accounts` entry is a keyed patch of one `Insert` (value under 64 bytes), `rename` one `Update`,
   `close_account` one `Remove`, and the mirrored list equals the model after each.
3. **Generic instantiations are plain records and enums.** After five `deposit`s of `1.50` into one account: `statement(a, 0, 2)` is an
   `EntrySlice` of 2 items with `next == 2`, `statement(a, 4, 10)` has 1 item and `next == nil`; `loadable_statement(a, false, 3)` is
   `.loading`, `(a, true, 3)` is `.loaded(slice)` with 3 items, an unknown account `.failed("no such account")`.
4. **Decimals are exact.** `deposit(0.1)` then `deposit(0.2)` returns a balance whose text is `0.3`; `echo_decimal` returns the
   same mantissa and scale for `0`, `1.10`, `-1.50`, a scale of 38, the smallest and the largest 128-bit mantissa (Swift compares
   the re-encoded bytes with the sent ones, Kotlin `BigDecimal.equals` and TypeScript `Decimal.equals` are scale-sensitive and hold);
   a wire decimal with a scale of 39, sent through the raw API, is refused by the core as a bad request, never decoded; `deposit` of
   an amount `rust_decimal` cannot hold fails with `LedgerError.OutOfRange`.
5. **Leaf types of other crates cross as wire types.** `sample_receipt()` has `id` equal to the UUID `1ed9e400-0000-0000-0000-000000000007`,
   `issued` the instant `2026-10-01T12:00:00.123Z` (1 790 856 000 123 ms), `valid_for` 90 s, `total` the decimal `19.990` (scale 3), `signature`
   the bytes `[1, 2, 3, 255]`; `echo_receipt` of a receipt the platform builds returns an equal receipt.

### S32 paged queries and lazy lists (ADR-043)

`Library` (`examples/playground/core/src/paging.rs`) holds two `Lazy<Item>` lists: `books` (10,000 rows it owns) and `evens` (a read-only view of
the even rows of `source`, a 20-row `Signal<Vec<Item>>`), and `feed` is an `infinite` query (`FeedQueryHandle`: pages of 50 rows of the big list's
10,000, `even_only` as its parameter, `touch_feed(revision)` makes the even rows show `revision` as their `version` the next time they are fetched).
Every step goes through the **generated** classes; "page calls" are the target-3 calls the runner counts at its transport (Swift and Kotlin wrap the
transport the runner loads, TypeScript counts `Kind.Call` payloads whose first byte is 3).

1. **A lazy list is not sent whole.** `Library()`: after the first drain `books.count == 10_000`, and the store's first change-set carries 20 bytes for
   `books` (a `LazyValue`), not 10,000 rows; `books[0]` is nothing until its page arrives, then `books[0].id == 1`; reading row 9,999 loads the last
   page (`id == 10_000`); an index outside `0..<count` is nothing and requests nothing (the page-call count does not move).
2. **A page is requested once, with one page of prefetch each side.** The first read of row 120 (page size 50) makes 3 page calls (pages 2, 1 and 3);
   reading another row of page 2 makes none (a row of page 1 or 3 asks once for the page beyond it, so scrolling stays one page ahead); the list's
   `version` equals the version in the page reply.
3. **A change is one 12-byte entry and the host re-pages only its window.** `add_rows(1)`: one change-set with one `books` entry, op 2, 12 bytes, carrying
   `len == 10_001` and a higher version; `count == 10_001`; rows already on screen stay readable while the pages the host touched are asked again
   (the page-call count grows by the window, not by 200); `rename(121, "x")`: row 121 reads `"x"` (`version == 1`) after the re-page;
   `remove_at(0)`: `count == 10_000` and `books[0].id == 2`.
4. **A drain folds without losing the page server** (ADR-031, amendment 2026-10-02). With the frame held, `add_rows(1)` three times, then released: one
   op-2 delivery with the last length and version. A second `Library()` whose `add_rows(1)` runs before its first drain ends with `count == 10_001`
   and a readable `books[0]` (the drain held the op 0 that names the page server, then the op 2).
5. **A view pages through the derived index.** `evens.count == 10`, `evens[0].id == 2`, `evens[9].id == 20`; `add_rows(2)` makes `evens.count == 11` and
   `evens[10].id == 10_002`; `drop_source(20)` leaves `evens.count == 1`.
6. **An infinite query grows a page at a time.** `FeedQueryHandle(evenOnly: false)`: after the first fetch `data.count == 50` and `hasNextPage`;
   `fetchNextPage()` makes `fetchingNextPage` true and then false, `data.count == 100`, and the change-set for `data` is a **keyed patch of 50 inserts**
   (under 5 KB), not a full value; a second handle with the same parameter shares the entry (no fetch of its own); after `touch_feed(7)` and `refetch()` the
   even rows' `version == 7`, the odd rows' `0`, and the change-set is a keyed patch of at most 50 updates; `FeedQueryHandle(evenOnly: true)` has the 50 rows
   `2, 4, .., 100`. On Swift `loadMore(ifNeededFor:)` of a row within 5 of the end fetches the next page, of an earlier row it does not.

### S33 polling (ADR-043)

`ticker` (`paging.rs`) is a query with `interval = "1s"` that returns a counter bumped by every fetch (`ticker_fetches()` reads the same counter,
`set_ticker_failing` makes it fail). The runner uses the real Timer port (real time), the `Lifecycle` and `Connectivity` events of the harness, and
5-second waits.

1. **A poll after each fetch.** Observing `TickerQueryHandle`: `data == 1` at once; within 2.5 s `data == 2` and `ticker_fetches() >= 2`; two
   consecutive fetches are at least 0.9 s apart (the interval runs from the end of a fetch).
2. **Background pauses, Active resumes.** After `Lifecycle` `Background`, `ticker_fetches()` does not move for 1.5 s; after `Active`, `data` advances again
   within 2.5 s.
3. **Offline pauses, online resumes.** After `Connectivity` offline nothing is fetched for 1.5 s; after online `data` advances within 2.5 s.
4. **A failure keeps polling and clears on success.** After `set_ticker_failing(true)`: `error == TickError.Failing` within 2.5 s and `ticker_fetches()`
   keeps growing; after `set_ticker_failing(false)`: `error` is nothing and `data` advances.
5. **An observer's override.** `setPollInterval(3 s)` on the only observer: once the next fetch has ended, the following gap is at least 2.9 s; `setPollInterval(nil)`
   returns to the query's own second.
6. **The last observer stops it.** After the handle is closed `ticker_fetches()` does not move for 2.5 s.

### S34 generic functions, objects and stores (ADR-058)

`selection` (`examples/playground/core/src/selection.rs`): the generic functions `newest` and `draft`, each listed for `Todo` and `Note`, the
generic store `Selection<T>` (the aliases `TodoSelection` and `NoteSelection`: two stores, two type ids) and the generic object `Recent<T>` (the
alias `RecentTodos`, returned by `recent_todos`). Every step goes through the **generated** API (the raw API where a step says so). The
platforms spell a generic function as overloads (Swift `newest(rows:)`, `draft(Todo.self, title:)`; Kotlin `newest(rows)`, `draft(Todo::class, title)`;
TypeScript `newest("Todo", rows)`, `draft("Todo", title)`); the rows of the steps have the identities named by their counter (a to-do's `Uuid` holds it in
its first eight bytes, big-endian; a note's `id` is it).

1. **A generic function is one function per type.** `newest` of the to-dos with counters 5, 9 and 7 is the one with counter 9; of the notes with
   counters 2 and 1 it is the one with counter 2 (a `Note`, not a `Todo`); of an empty list of either type it is nothing.
2. **A type that no argument names is named by the caller.** `draft(Todo.self, "first")` and a second `draft` of the same type return rows
   titled as asked, not done, with different ids; `draft(Note.self, "a note")` returns a `Note`; `newest` of the two drafts is the second.
3. **Two instantiations are two stores.** `TodoSelection` and `NoteSelection` have different handles and different type ids
   (`UndraIds.Objects.<Name>.typeId`). Toggling a to-do on the to-do selection delivers one change-set whose `rows` entry is a keyed patch of one
   `Insert` (and whose `count` entry follows), and **nothing** to the note selection; toggling it again delivers one `Remove`; toggling a note
   delivers one `Insert` to the note selection and nothing to the to-do one.
4. **Snapshot and restore keep both.** With two to-dos ticked on a `TodoSelection` and one note on a `NoteSelection`, `snapshot`, then clear the
   first and tick another note, then `restore`: both stores keep their handles and show the snapshot's rows and counts (2 and 1), and keep
   working, each on its own rows.
5. **An id that names no instantiation is refused.** A raw free-function call with `fnv1a32("fn.newest<Draft>")` fails as a bad request (status 5;
   Swift `UndraCallError.refused`, Kotlin `UndraCallError.Refused`; TypeScript `UndraReplyError`), the core is unharmed, and the ids of the
   instantiations that exist are `fnv1a32("fn.newest<Todo>")` and `fnv1a32("fn.newest<Note>")`.
6. **A generic object returned from a function is the alias's class.** `recent_todos([1, 2, 3], 2)` is a `RecentTodos` whose `rows()` are the
   to-dos with counters 3 and 2 and whose `latest()` is the one with 3; `open` of the one with 2 puts it first; a second `recent_todos([], 5)` is another
   wrapper with no rows, and closing it leaves the first working.

### S35 query handles across a restore (ADR-059)

A query handle is a view of the query cache, so a snapshot keeps what it is made of (the query's parameters
and the polling interval its observer set for itself) instead of a store's values, and a restore re-issues the
handle under the **same value**: the app's wrapper keeps working with no code of its own, after a time travel,
an app's own `core.restore`, a dev reload and a web crash restart. The object behind a re-issued handle is built
when the host first uses it. Steps 1 to 9 restore into the runtime that holds the handles, through the generated
bindings; step 10 restores into a **fresh runtime** (build B, "Two builds"; the query `roster` of
`examples/playground/core/src/updates.rs` changes its parameter from `u32` to `String`, `remote_todos` and `ticker`
do not change) through the raw API with the ids it knows. The server serves `GET /lists/s35/todos`.

1. **Opening.** `live_handles` is read first. `remote_todos("s35")` shows the server's one item (milk);
   `TickerQueryHandle()` is observed and has ticked; `FeedQueryHandle(evenOnly: false)` has two pages (100 rows);
   `Library()` has read `books[0]`; `RosterQueryHandle(7)` is observed (the handle whose query build B changes);
   a `Counter` has `add(5)`; a `Probe` exists.
2. **The snapshot, a change and a restore.** `snapshot`; `Counter.add(10)`; the server now answers two items
   (milk and dog); `restore(snapshot)`.
3. **Same wrappers, nothing blinked.** The counter shows 5; the remote handle's wrapper is the same object with
   the same handle value; the entries the mirror applied to the remote and feed wrappers during the restore are
   none (the runner records them: `data` never became absent and nothing was sent for the handle); the GET count
   did not move; the feed still has 100 rows; `live_handles` is what it was before the restore less one (the
   `Probe`, the one object the restore makes stale, step 7): no query handle was dropped and the restore made no
   handle of its own.
4. **`refetch` is accepted** on the same remote wrapper: the GET count grows by 1 and `data` is the two items.
5. **Polling continues.** The ticker advances (it polls every second; each runner waits up to 20 s, so a loaded machine does not fail the step) with no call from the runner (the core's timer was
   not touched).
6. **Pages still load.** `fetchNextPage()` on the same feed wrapper gives 150 rows; `Library.books[0]` is readable
   again (the restore rebuilt the store and its page servers, and a page call through the new server succeeds).
7. **What is not re-creatable stays stale.** The `Probe` is refused (status 5 / `Refused`), as in S15 step 9.
8. **Idempotent.** `restore(snapshot)` again changes nothing more: the checks of step 3 hold (the remote wrapper
   shows the two items it was last given, the feed 150 rows, `live_handles` unchanged, nothing applied to the
   remote and feed wrappers).
9. **Closing** the wrappers returns `live_handles` to its value before step 1.
10. **A fresh runtime** (TypeScript: a second core of build B in the process; Swift and Kotlin: the build-B
    process, the snapshot and the handles of step 1 handed over in a file, on a **new core of build B**: S14's
    and S15's build-B steps leave stores in the core they used, which a restore replaces, so the core is shut
    down and loaded again over an empty `Kv` and a new server first). The runner reads `live_handles`,
    then `restore(snapshot)` succeeds. `live_handles` has grown by the stores of the snapshot (`Counter`,
    `Library` and its two page servers) and the three query handles build B honours: 7 (the handle of `roster` is
    not counted). No GET was made yet. After `configure_remote` (core state outside stores, as S22 says),
    `observe(remote handle)` is answered with `status = fetching` and `data` absent, and then the data; `refetch`
    on it is status 0 and makes one more GET; the ticker's handle delivers ticks (it is observed again like any
    handle after a reload); `Library`'s `books` (observed again) pages through the server its op 0 names (the
    reply says 10,000 rows); the handle of `roster` answers `refetch` with status 5, and the Log port received a
    WARN that names it (`Handle(index=<i>, gen=<g>)`, "is not re-issued", "the types it was made from changed").

## Platform notes

* TypeScript: S03 runs only in `wasm-main` mode (the only one with `callSync`); S17 step 6 is the only
  step that runs in `wasm-worker` mode. S15 uses the public `core.snapshot()` / `core.restore()` (SPEC 17.1),
  which both wasm modes have (a socket has none yet: `UndraModeError`).
* The Kotlin runner runs on the JVM with a single-thread "main" executor (`UndraDispatchers`), the Swift
  runner on the main actor; both load the real native library.
* S17 steps 5, 6 and 7 are native-only (Kotlin and Swift): the wasm core cannot call out of a panic
  into a synchronous port (step 5), and a trapped core has nothing left to shut down or reload (steps 6
  and 7; a fresh `UndraCore.load` after the trap is what wasm step 3 already does); the wasm step 5
  covers the same ground for a core that is gone. Steps 6 and 7 end and reload the core, so they are the
  last steps of the last scenario a native runner runs.
* S07 step 7 restores a snapshot the way S15 does on each platform (TypeScript through the wasm export).
* Every platform reports a **command** (a synchronous method that returns nothing and has no error type)
  through `LoadOptions.onError` / `onError` instead of throwing or rejecting (ADR-032, and its amendment A for
  Kotlin and TypeScript); each runner records those reports (`UndraUnhandledError`: operation, error), and
  S05.6, S15.9 and S17.6 assert them. A generated call fails with its own `E`, the caller's cancellation
  (`CancellationException`, `AbortError`, `CancellationError`) or `UndraCallError`, on all three.
* S19 step 9 reads `examples/playground/build/derived-vectors.bin` (`UNDRA_DERIVED_VECTORS` overrides it),
  which each `run.sh` writes through `contract-tests/derived-vectors.sh` when it is missing or older than the
  signals crate. Only TypeScript's mirror is public; Kotlin and Swift apply change-set by change-set.
* Timing constants (50 ms delays, 200 ms quiet windows) are chosen for a loaded CI machine; do not
  shrink them.
* S14 steps 7 to 9, S15 steps 11 to 14 and S35 step 10 need build B (see "Two builds"). TypeScript loads both wasm
  modules in one process; Swift and Kotlin run build B's steps in a second process after the main run.
* S35 steps 3 and 8 record what the platform's mirror applied to a wrapper during the restore (TypeScript: the
  wrapper's `_apply` through `tapEntries`; Kotlin and Swift: the signal's own change counts or a collector on
  `data`); the ticker is not asserted there (it polls). Its step 5 uses the real Timer port, as S33 does. A
  restore reads no clock and starts no fetch (R12): a GET count that moves during steps 3, 8 or 10 before the
  runner observes is a failure.
* S20 step 4 differs by platform: a fresh TypeScript core can be loaded with a `Kv` whose queue reads fail, so
  the TypeScript column walks the whole "unreadable, then readable on `Active`" path; Swift and Kotlin load one
  core per process, so their harness fails the first read of the queue at load and S20 checks what that did.
* S28 step 3 (a throw that is not the method's own error) cannot be written against Swift's generated protocols
  as the runner is: with typed throws (the default, ADR-032) `confirm` can only throw `ReportError` and `note`
  cannot throw, so Swift's column prints a `NOTE` line for the step and the runtime's own tests
  (`ObjectsCallbacksTests`) cover the mapping (status 2, `onError`, operation `Reporter.confirm`).
* S27 step 7 depends on the garbage collector on Kotlin and TypeScript (`System.gc()` and a bounded wait; the
  `FinalizationRegistry`, which needs Node's `--expose-gc`, else the runner `close()`s and says so): a GC that
  does not run in time is a FAIL on Kotlin and a `close()` on TypeScript, never a SKIP.
* S29 steps 1 to 4 and S30 run on every platform with the playground's debug core; S29's wasm column is the
  trap path (the core cannot call `Diagnostics` before it traps, so the host builds the report from the FATAL
  `undra::panic` record, `message`, `at <file>:<line>:<col>` and `in <operation>` on lines of their own, and the
  trap's stack). S27 and S28 are `objects-callbacks`'; ADR-046's provisional numbers (S27 panic report, S28
  background run) became S29 and S30.
* S30 needs a **quiet cache**: a background run refetches the entries that are observed or persisted and stale or in error, and finishes only when they are fetched, so a scenario that leaves an entry in error (S12's 503 and bad body) must leave it healthy before S30 (the Kotlin runner's S12 does, and S30 runs after S20 there; S13 expects the first optimistic placeholder id of the process, `u32::MAX`, so S30 cannot move before it). `refetched` in step 3 is at least 1: coming back online refetches an observed list and the replay's invalidation does again, and the run waits for both.
* S21 and S22 are TypeScript-only: worker mode and crash recovery are web features (ADR-049; a native core
  contains a panic without trapping, SPEC 5.6). `check.sh` does not expect them from Swift or Kotlin.
* S23 and S24 start `contract-tests/servers/realtime-server.mjs` with Node (every runner's machine has Node:
  the TypeScript runner needs it) and read its `READY <port>` line; S25 roots its adapter in a temporary
  directory the runner deletes afterwards.
