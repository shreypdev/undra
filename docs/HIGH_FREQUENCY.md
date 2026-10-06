# High-frequency data

A socket that pushes prices, a sensor at 1 kHz, a simulation, a list that churns: the core commits
state far faster than a screen can show it. This page says what Undra does with such a firehose, what
you can rely on, and the two tools you have: `#[undra(no_coalesce)]` and `ctx.txn`. The binding rules
are in `docs/SPEC.md` §11.1; the decision and its measurements are ADR-031.

## What happens to a firehose

The core is fast: one observed transaction costs about 80 ns to commit and hand to the platform
(about 12 million per second on one core). Every transaction becomes one **change-set**, delivered
in commit order, and nothing on the wire is merged or dropped.

The platform side is where it would hurt, so the **mirror** (the part of each runtime that applies
change-sets to your stores) merges before it applies:

* **Once per frame.** Change-sets the core produced on its own (a timer, a stream, an event, a port
  completion, a background task) are applied at most once per display frame: on the next
  `requestAnimationFrame` in a browser (a zero-delay task while the tab is hidden, a microtask under
  Node), from a `CADisplayLink` on iOS, through `Choreographer` on Android.
* **Merged per signal.** Within one drain, a full value replaces everything queued before it for
  that signal, and the keyed patches of a list are concatenated into one patch. So each signal is
  applied **at most twice per frame** (its last full value, then one merged patch) however many
  transactions touched it, and a keyed list is copied once per frame instead of once per patch.
* **Exact.** The state after a drain is exactly the state after applying every change-set in commit
  order. Only the intermediate states inside one frame are skipped, and you could not have seen
  them: they would never have been rendered.
* **Bounded.** The backlog holds at most 65,536 entries or 16 MiB (tunable). Past that it is folded in
  place, so a blocked main thread, a backgrounded app or a hidden tab costs memory proportional to the
  number of signals you observe, not to the number of transactions, and catches up in one drain. A
  list whose merged patch grows past 4,096 operations or 1 MiB is dropped and re-observed instead:
  one full value is cheaper than that patch, and memory stays bounded however large the items.

On the web, 1,667 one-row patches per frame on a 10,000-row list (100,000 per second) went from about
4 ms of main-thread work per frame to about 0.4 ms with this merge (ADR-031, "Consequences").

## What is never delayed: your own calls

Frame alignment applies only to what the core does on its own. Your calls see their own writes:

* `await store.method()` (TypeScript, Swift `async`, Kotlin `suspend`): the change-sets the call
  produced are applied before your code after the `await` runs.
* A synchronous method called on the main thread (Swift and Kotlin `callSync`-backed methods, TS
  `callSync` in `wasm-main`): applied before it returns.
* Creating a store: its initial values are there when `create()` / the initializer returns.

```ts
await todos.add("milk");
console.log(todos.remaining.get()); // already counts "milk"
```

## When to opt out: `no_coalesce`

Some signals are about the steps, not the state: a progress bar that should visibly walk from 0 to
100, a counter whose every increment drives an animation. Declare them `no_coalesce`:

```rust
#[undra::store]
pub struct Upload {
    #[undra(no_coalesce)]
    progress: Signal<u32>,   // every value reaches the platform and is applied
    bytes_sent: Signal<u64>, // the last value per frame is enough
}
```

The core then delivers every commit of the signal (even while nothing observes it), the schema
records the flag, and the generated store tells its mirror, which applies every entry of that
signal, in order (TypeScript announces each one to subscribers separately).

Know what you are asking for:

* **The UI framework still decides what it shows.** A Kotlin `StateFlow` conflates by design, and
  SwiftUI and React render once per frame. If you need to *react* to each value in code, subscribe to
  the signal (TS `signal.subscribe`) or, better, make the steps a **stream** (`Stream<T>` return
  type): streams have credit-based backpressure and deliver every item.
* **The backlog bound wins.** If the main thread falls past the backlog bound (65,536 entries or
  16 MiB by default), a `no_coalesce` signal is folded like any other.
* **It costs what coalescing saves.** A `no_coalesce` signal written 100,000 times per second is
  100,000 applies per second on the main thread. Keep it for low-rate step semantics.

## Batch in the core: `ctx.txn`

Coalescing happens on the platform, after the core has encoded and handed over one change-set per
transaction. When **your** code makes many writes in one go, batch them in a transaction: the core
then encodes one change-set, crosses the boundary once, and the platform has nothing to merge.

```rust
pub fn apply_quotes(&self, quotes: Vec<Quote>, at: Timestamp) {
    self.ctx.txn(|| {
        for q in &quotes {
            self.board.update_at(q.row as usize, |row| row.cents = q.cents);
        }
        self.updated_at.set(at);
    });
}
```

Without `ctx.txn`, every write outside a transaction is its own transaction (SPEC §5.5): a loop of
1,000 writes is 1,000 change-sets. Coalescing makes that cheap on the platform; `ctx.txn` makes it
free. Use it whenever one logical change touches many signals or many rows, and in loops that ingest
batches from a socket or a port. It also keeps a multi-signal change atomic for the UI: a transaction
that touches one store is one change-set, never split across frames.

Two more rules for producers inside the core:

* A push source you own (a channel fed by a port, a socket reader) is your buffer: bound it, or
  expose it as a stream so the consumer's credit paces it.
* A transaction that touches several stores arrives as several change-sets; a frame can fall between
  them. Keep state that must change together in one store.

A big structure fed by a snapshot and a stream (a ranking of 100,000 players) is ingested on `spawn_blocking`
into a structure you own and only a small window of it is published, in one transaction, which holds the core lock
for 375 ns where applying the snapshot inside a call holds it for 2.56 ms: see the
[live leaderboard recipe](https://shreypdev.github.io/undra/docs/cookbook/leaderboard.html) (`bench/RESULTS.md`, finding 9).

A view over a churning list costs the change too. A `Computed<Vec<T>>` that filters or sorts the
board is recomputed, re-sent and re-decoded whole on every write of any row (at 10,000 rows, 177 µs and
353 KB per change in the core, 1.1 ms to apply in a browser); a `DerivedList<T>`
(`board.derive().filter(..).sort_by_key(..).build()`, ADR-039) replays each recorded row operation on
its own index and ships at most two patch ops for it, nothing when the row is outside the view: about
0.3 µs and 158 bytes per change, and its patches merge per drain like any keyed list's. Write the
board with the recorded operations (`update_at`, `insert`, `remove`, ..); a raw `set` or `update`
rebuilds every view of it and sends them whole. `bench/RESULTS.md` (finding 5) has the measurements, and
the `derived_churn_10k/sustained` scenario holds a sorted view of a 10,000-row list at 122,000
operations a second with the host's copy equal to the core's.

## Measuring it

Every runtime counts what its mirror did: change-sets and entries received, entries applied after
merging, drains, compactions and resyncs. `UndraCore.stats()` reports them, and a drain listener
reports each drain as it happens.

```ts
const stop = core.mirror.addDrainListener(({ changeSets, entries, appliedEntries, durationMs }) => {
  console.log(`${changeSets} change-sets, ${entries} entries, applied ${appliedEntries} in ${durationMs} ms`);
});
const { mirror } = await core.stats(); // changeSetsReceived, entriesApplied, drains, compactions, ...
```

```swift
let registration = core.mirror.addDrainListener { stats in
    print(stats.changeSets, stats.appliedEntries, stats.duration)
}
let counters = core.stats().mirror
```

```kotlin
val handle = core.mirror.addDrainListener { stats -> log(stats.changeSets, stats.appliedEntries, stats.duration) }
val counters = core.stats().mirror
```

`entriesApplied` far below `entriesReceived` means coalescing is doing its job; `compactions` above
zero means the main thread fell behind the bound; `resyncs` above zero means a list was re-observed
instead of patched.

## See it yourself: the playground stress screen

The playground's **Stress** tab (web: `playground/?screen=stress`) lets you watch all of the above on your own
machine. The core generates the updates **itself**, the way a socket or a sensor would: `Stress.start(mode,
perSecond)` spawns a task that sleeps 10 ms on the `Timer` port, reads the `Clock` port each tick and commits
`rate x elapsed` updates, **each its own transaction**, carrying the remainder, so the achieved rate tracks the
target even when the host's timer fires late (a tick never commits more than 100 ms of work: a hidden tab does
not burst when it comes back). The updates go to `value` (firehose) or to the `no_coalesce` `progress` (progress),
and the `generated` signal reports how many the generator really committed. A host loop calling a method would
measure the read-your-writes path instead, which is never delayed; only what the core produces on its own is.

The screen shows, from the runtime's own numbers (`mirror.stats()`, `mirror.addDrainListener`), refreshed every
500 ms over a rolling two seconds:

| Tile | Where it comes from |
|---|---|
| Generated / s | the core's `generated` counter |
| Received / s | `changeSetsReceived`: one per update, plus the generator's own write of `generated` once per 10 ms tick (100 a second, which is why it reads a little above Generated) |
| Applied / s | `entriesApplied`, after the mirror merged what arrived between two frames, and the **merge ratio** (applied over received). In firehose mode this is about 120 a second whatever the rate: `value` and `generated` once per frame each. Nothing is lost: each apply carries the latest value of everything merged into it |
| Drains / s, drain p50 / p99 | the drain listener's `durationMs`: how long the mirror held the main thread each time it ran |
| Per change-set | all drain time over all change-sets: the average that survives the clock rounding below |
| Dropped frames | a `requestAnimationFrame` loop: a gap over 1.5 frame intervals drops `round(gap / interval) - 1` frames |
| JS heap | `performance.memory`, Chrome only, and only the JS heap (the core's wasm memory is not in it) |

Two limits are on the screen too. Browsers round `performance.now()` (Chrome to 0.1 ms, Firefox and Safari to
1 ms, unless the page is cross-origin isolated), so one drain shorter than the step reads "under 0.1 ms"; the
per-change-set average is exact. And everything is measured in **your** browser, on the page's main thread:
nothing on the page is a recording. The `value` and `progress` tiles show the difference coalescing makes: after a
second of 10,000 updates a second, `value` has been applied about 60 times (once per frame) and `progress`
10,000 times (every update), the cost of the opt-out.

**Run it.** Choose firehose or progress, 1k to 100k updates a second, Start, and "Burst 1,000" for a thousand
transactions at once. The page takes `?screen=stress&rate=100000&mode=firehose&autostart=1` (`rate` is a number
or a count of thousands, `100k`, up to the core's maximum of 1,000,000), which is what the landing page's "Push it"
button opens in its iframe (at 10,000). Above what the main thread can carry the screen stays honest rather than
pretty: at `rate=1000000` on the reference machine the generator reaches about 675,000 a second (each 10 ms tick
commits at most 100 ms of work, and the 100,000 wasm-to-JS crossings of one tick take longer than that), the
Generated tile shows the shortfall against its target, Dropped frames climbs by about 40 a second, and the Received
tile reports how often the mirror folded its backlog before a frame came (ADR-031 decision 3). Embedded
(`embed=1`) it posts what it shows to the parent as the `undra-stats` message, the base fields of the list demo
plus `generatedPerSec`, `entriesReceivedPerSec`, `entriesAppliedPerSec`, `mergeRatio`, `drainsPerSec`,
`applyNsPerChangeSet`, `droppedFrames`, `longestFrameMs` and a few more (`examples/playground/web/src/embed-stats.ts`).

On the reference machine (Apple M5 Pro, headless Chromium, `wasm-main`, the numbers the embedded page posts: the
median of the last eight messages, four seconds, of a ten-second run; the drain p99 is quantised by the 0.1 ms clock):

| Updates a second | Generated | Received | Applied | Drain p99 | Per change-set | Dropped frames |
|---|---|---|---|---|---|---|
| 10,000, firehose | 10,002 | 10,086 | 120 | 0.1 ms | 55 ns | 0 |
| 50,000, firehose | 49,992 | 50,100 | 119 | 1.2 ms | 123 ns | 0 |
| 100,000, firehose | 100,055 | 100,112 | 120 | 1.0 ms | 50 ns | 0 |
| 100,000, progress (`no_coalesce`) | 100,025 | 100,089 | 100,085 | 3.3 ms | 1,052 ns | 0 |

The merge is what the firehose rows show: ten times the updates, the same 60 drains and about 120 applies a
second (the merged signal and the generator's own counter, once per frame each). A 60-second run at 100,000 a
second (6 million updates) logged no error, dropped no frame and held 99,400 to 100,500 generated a second. The
last row is the `no_coalesce` cost from "When to opt out": 100,000 applies a second, 3.3 ms in the worst drain, and
still no dropped frame. These are one browser on one machine, not a guarantee; the numbers you see are the ones
that count.

## Tuning

The defaults suit an app; change them only with a measurement in hand.

| | TypeScript | Swift | Kotlin |
|---|---|---|---|
| Frame source | `UndraCore.load({ mirror: { schedule } })` (default `scheduleFrame`) | automatic (`CADisplayLink` on iOS, tvOS, visionOS) | `LoadOptions(mirror = MirrorOptions(framePacer = ChoreographerFramePacer()))` from `android-adapters` |
| Backlog bound | `mirror: { maxPendingEntries, maxPendingBytes }` | `LoadOptions.maxPendingEntries`, `.maxPendingBytes` | `MirrorOptions(maxPendingEntries, maxPendingBytes)` |
