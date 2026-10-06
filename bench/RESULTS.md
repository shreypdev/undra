# Undra benchmark results

Host-measured numbers for every row of the blueprint's section 14 budget table that a host can
measure, the full criterion tables behind them, and what is still waiting for devices.

* Reproduce: `cargo bench -p undra-bench` (criterion, for humans) and
  `cargo bench -p undra-ffi --bench boundary` (the C ABI).
* The CI gate is different and cheaper: `cargo test -p undra-bench --test budgets --release`
  times the same operations with plain `Instant` and fails over `bench/budgets.toml` (see
  [The CI gate](#the-ci-gate)).
* Harsh conditions (sustained load, tails, memory, invariants) have their own gate and a soak:
  `cargo test -p undra-bench --test stress --release` and
  `cargo run -p undra-bench --release --bin soak` (see [Harsh conditions](#harsh-conditions)).
* Medians are criterion's; the bracket is its 95% confidence interval on the median.

## Machine

| | |
|---|---|
| CPU | Apple M5 Pro, 18 cores (Apple silicon, arm64) |
| OS | macOS 26.5 (25F71) |
| Rust | rustc 1.98.1 (48a229cea 2026-09-01), criterion 0.5 |
| Profile | `bench` inheriting `release`: opt-level 3, `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"` |
| Runtime | no `undra-core` thread unless a row says so: the host drives the executor, as the wasm shell does |
| Load | **shared**: other builds ran on this machine during the measurements (load average from 5 to several hundred), so a thread-wake-dependent number can swing; two full runs and the gate's own timings agreed within about 5% for everything single-threaded |

## Section 14 rows, measured on the host

The blueprint's targets are per device (iOS A15-class, Android mid-range 2022, Chromium). An M5 Pro
core is substantially faster than an A15 core, so **"within target" here is necessary, not sufficient**,
and a miss here is a stronger miss on the device. "Host / target" is this host's median divided by the
iOS target.

| Row | Operation measured here | Median | iOS target | Host / target | Verdict |
|---|---|---|---|---|---|
| Handle method call, primitive args and return | `dispatch/call_sync/add` | 43.9 ns | ≤ 60 ns | 0.73x | within on the host, **open for the device** (Finding 2) |
| 1 KB record, round trip (codec) | `wire/record1k/roundtrip` | 228.0 ns | ≤ 3 µs | 0.08x | within |
| 1 KB record, round trip (through a call) | `dispatch/call_sync/echo_record1k` | 139.5 ns | ≤ 3 µs | 0.05x | within |
| Change-set, 100 dirty signals (core side: write, build, deliver) | `signals/changeset_100/runtime` | 2.30 µs | ≤ 100 µs | 0.02x | within |
| Keyed patch on 10,000 items, one insert (recorded list operation) | `signals/keyed_10k/insert` | 6.31 µs | ≤ 20 µs | 0.32x | within |
| Core cold start, 100 KB snapshot restore | `snapshot/cold_start_restore_100kb` | 80.12 µs | ≤ 3 ms | 0.03x | within (the budgets test's p50 on 2026-10-02; three quarters of it is `Runtime::new` hashing this binary's schema, not the restore: Finding 8) |
| Core cold start, including the `undra-core` thread | `snapshot/cold_start_restore_100kb_core_thread` | 88.12 µs | ≤ 3 ms | 0.03x | within (as above) |
| Web crash recovery, 1 MB state (restore) | `snapshot/restore_1mb` | 286.54 µs | ≤ 100 ms | 0.003x | within |
| Restore of a 100 KB snapshot an older build wrote (every store migrated by name, ADR-037) | `snapshot/restore_100kb_migrated` | 88.84 µs | ≤ 10x the fast path (`snapshot/restore_100kb`, 30.33 µs) | 2.9x the fast path | within |

Two more rows have a host proxy, which is a data point and not a verdict:

| Row | Host proxy | Value | Target | Note |
|---|---|---|---|---|
| Hello-world size added to the app | `undra-ffi` cdylib on `aarch64-apple-darwin`, release, LTO fat, stripped, no app code | 734 KB (316 KB gzipped) | ≤ 900 KB (iOS, arm64) | 82% of the budget before any app code; a macOS dylib carries Mach-O overhead an iOS static link does not. The `undra-ffi` test fixture core (runtime, JNI glue and a test core) is 978 KB stripped. A real measurement needs `aarch64-apple-ios` |
| Runtime memory at idle | resident-set growth of a test process after `Runtime::new` with an `undra-core` thread | about 0.45 MB for the first runtime, about 70 KB for each further one (no core thread) | ≤ 2 MB | `ps` RSS, page-granular, so indicative only; a heap counter needs `unsafe` (R2) |

Also measured, not a row: the same handle method call through the C ABI (`undra_call_sync`,
`boundary/call_sync/add`) is 49.8 ns (79.3 ns before ADR-028), so the ABI itself adds about 6 ns (the
`Runtime::global()` lookup and the `UndraBuf` hand-off). Without the one allocation that hand-off owes, the
same call is 31.5 ns (`dispatch/call_sync_with/add`).

## Harsh conditions

The question a demanding app team asks is not "how fast is one call" but "what happens at a hundred times
any UI's rate, for minutes, with several threads, and does it stay correct and flat in memory". These
scenarios answer it for **the core side** of the pipeline, on a host: a transaction committed, its
change-set built and handed to the host's callback, the host's list updated, a stream and a port call
completed. What a platform does with a change-set afterwards (copy it, decode it, apply it on the main
thread, render) is **not** measured here: it is measured in the browser (the playground's stress screen), and on the
iOS simulator, the Android emulator and Chromium by the device bench (see [Device numbers](#device-numbers-ios-android-web)
and ADR-031, which is about exactly that platform half); the rows from the blueprint's devices wait for
hardware. A host number says "the core is not the bottleneck"; it does not say "the app is smooth".

The numbers below are the files in `bench/results/`: `2026-09-30-<scenario>.json` for each sustained scenario (a 10 s
measured run after a 200 ms warm-up, the first attempt that passed every gate, which was attempt 1 in all eight),
`2026-09-30-layer-a.json` for the layer A rows and `2026-09-30-soak-60s.json` for the soak, all from commit `acc4a22`
on the machine above with `UNDRA_BENCH_BASELINE=apple-m5-pro` selected (the baseline gate passed too). Each file says
what it measured, the command, the machine and the load average before and after: **2.1 to 2.4** for these runs (the
machine is shared, and 2 is as quiet as it gets while another agent builds; an earlier session saw 3 to 8). The
cache-bound tails move by up to 4x between runs on a busier machine (fan-out p99 82 to 295 us), which is why the gates
are 5x to 10x the best run set at the time (`measured_*` in `budgets.toml`, the best of three 10 s runs on that loaded
machine) and why no number was tuned to pass. Two consequences to keep in mind: the **absolute** gates catch an
operation that became several times slower, not a 2x one (in review, a commit path made 1.8x slower on purpose passed
every layer A row and every sustained gate here; the ratio gates and baselines of [Gates that catch a 2x
regression](#gates-that-catch-a-2x-regression) are what catch that); and the files of the experiments that
show it are in the same directory, tagged `commit-control`, `commit-spin` and `commit-ramp`.

| # | Scenario | What it proves | Measured (10 s run) | CI gate | Verdict |
|---|---|---|---|---|---|
| a | **Firehose**: one observed `Signal<u64>`, one transaction per update, a host calling `call_sync` | each transaction is O(1) in the core at 100x any UI rate: one change-set of exactly 37 bytes | **6.2 M transactions/s**; p50 125 ns, p99 211 ns, p999 295 ns per call (commit + delivery + the copy every FFI callback makes); a core-side burst commits at 76 ns each, 13.1 M/s | at least 1.1 M/s, p99 at most 1.1 us, p999 at most 3 us, 37 bytes; layer A: 1,000-burst 400 us, call 630 ns | within, 5x margin |
| g | **Event firehose**: `Runtime::event` on an event port whose subscriber writes the signal | the path a WebSocket or sensor feed takes into the core | **7.5 M events/s**; p50 125 ns, p99 167 ns, p999 251 ns; 37 bytes | at least 1.4 M/s, p99 840 ns, p999 2.6 us; layer A 580 ns | within |
| b | **Keyed churn**: 10,000 rows, a fixed cycle (4 update, 2 insert, 2 remove, 2 move) at seeded random positions, one operation per transaction, a host list applying every patch | recorded list operations stay O(change) under sustained churn and the host copy never desynchronises | **182 k operations/s**; p50 3.8 us, p99 16.9 us, p999 22.0 us per operation (commit + delivery + host apply); **61 bytes** each; host list equals core list field for field, every operation applied as exactly one patch | at least 33 k/s, p99 90 us, p999 270 us, 61 bytes; layer A 30 ms per 1,000 | within |
| b' | **Derived churn** (ADR-039): scenario b's list and cycle with a derived view of it (the rows not done, by title), both observed and mirrored by the host; every update of the cycle moves a row into or out of the view | a view over a churning list costs the change: each source op is at most two view ops, the host's view never desynchronises and nothing is ever re-sent whole | **122 k operations/s** (169 k for b in the same run, load 10); p50 5.8 us, p99 24.6 us, p999 100 us per operation (both commits, delivery, two host applies); 93.0 bytes each; the host's view equals filter + stable sort of the core's rows field for field, 896,219 view patches applied of 896,219, one rebuild (the observe) | at least 22 k/s, p99 170 us, p999 890 us, at most 103 bytes (a ceiling: the view's bytes depend on which ops fall inside it); layer A 39 ms per 1,000 (`bench/results/2026-10-01-derived-lists-*.json`, 2026-10-01) | within |
| c | **Fan-out**: 1,000 of 100,000 observed signals written per transaction, then the same 1,000 over 10,000 observed (signal-table layer, no runtime) | commit time and bytes follow the dirty count, not the observed count | **27.9 k transactions/s**; p50 35 us, p99 59 us; one change-set of **21,012 bytes** either way; the 10,000-observed run has p50 25.6 us (ratio 1.4, gated at 4) | at least 4.6 k/s, p99 410 us, p999 1.3 ms, 21,012 bytes; layer A 190 us and 130 us | within |
| c' | **Fan-out across stores**: 1,000 stores of 100 signals, one written in each, one transaction | the per-store overhead when one transaction touches many stores: 1,000 change-sets sharing a transaction | **9.9 k transactions/s** (9.9 M change-sets/s); p50 98 us, p99 147 us; 33,000 bytes | at least 1.6 k/s, p99 1.3 ms, 33,000 bytes; layer A 580 us | within |
| d | **Stream backpressure**: an always-ready producer, a consumer granting 16 credits a round, then 100,000 | Undra buffers at most one item beyond the consumer's credit, so memory is bounded whatever the producer does | **30.6 M items/s** with credit; produced minus delivered never above **1** in 8.0 M rounds; RSS **+0.00%** over 10 s | at least 5.7 M/s, RSS at most 1% (or 64 KiB); layer A 180 us per 1,000 items | within |
| e | **Concurrent completions**: 8 host threads answering async port calls, an `undra-core` thread, a 60 Hz "main thread" drain, 256 calls in flight | port calls completed from 8 threads at once are never lost: each wakes its task, commits once and is delivered once, and the drain sees the store's change-sets in transaction order and its total arriving as an exact count (the commits themselves run on the one `undra-core` thread, so this does not contend the per-store delivery lock: that is e') | **341 k completions/s**; call to reply p50 172 us, p99 573 us, p999 819 us; every call answered, every completion one change-set of 37 bytes, **0 lost, 0 out of order**, final total exact | at least 69 k/s, p99 3.7 ms, p999 9.7 ms | within |
| e' | **Contended completions**: scenario e plus a host thread writing the **same store** through `call_sync` as fast as it gets the core lock | ordering when two threads commit to one store (the `undra-core` thread running the completions, and the host thread): every completion and every write adds one to one signal, and the main thread must see that total arrive as exactly `0, 1, 2, .. N` in transaction order, so nothing is lost, repeated or reordered whichever thread committed it. The core lock serialises the two committers, so this tests the hand-off between them and the delivery inside it | **345 k writes/s** (about 302 k completions and 43 k host writes a second); completion call to reply p50 147 us, p99 336 us, p999 475 us; the host thread's `call_sync` p50 1.2 us, p99 152 us, max 0.49 ms; every call answered, every write one 37-byte change-set, **0 lost, 0 out of order, 0 steps that were not +1**, final total exact | at least 70 k/s, p99 1.8 ms, p999 6.6 ms, 37 bytes | within |
| f | **Soak**: firehose 100 k/s + churn 20 k ops/s + completions 50 k/s + a stream at 1 M items/s + a 60 Hz drain, together | no leak, and no second whose tail is far off the others' | 60 s: all four loads at 98% to 100% of target in every second (see below); RSS **+0.14%** over the second half; the p99 trend **-26%** of its median; every invariant held | RSS at most 1% (or 64 KiB), the p99's trend over the second half rising by at most 50% of its median, worst second's p99 at most 3x the median, invariants; CI runs 10 s | within |

The completions scenarios (e and e') run in one of **two regimes per process** on this host, which the scheduler
picks when the threads start and which lasts the whole run: call to reply p50 near 170 us at about 340 k/s (the
published files), or near 560 us at about 260 k/s (the final gate runs of 2026-09-30, and the first baseline
recording). Both pass the gates, the baseline holds the best of three recordings, and the contended scenario is the
steadier of the two (its p50 moved from 147 to 245 us across the same runs).

Allocations: one observed single-signal commit allocates **exactly 3 times** (the two vectors of
`group_by_store` and the `claimed` vector), an unobserved one **0**; `crates/undra-ffi/tests/commit_alloc.rs`
holds the first at "at most 3" and the second at 0 with a counting allocator (only that crate may count,
R2). At 100,000 commits a second those 3 are 300,000 `malloc`/`free` pairs a second, which is the next thing
to remove (a roadmap line, not a budget here).

Keyed churn is 182 k operations/s here against the design's 280 k probe (`.10x/specs/2026-09-30-stress-bench-design.md`,
section 3) because the scenario does more per operation, not because the mix is harder: the probe's random
20/20/20/40 mix has the same proportions as the fixed cycle, but the probe used a host that only counted, while
the scenario's host decodes every patch and applies it to its own 10,000-row list inside the timed step. Measured
side by side in review on the same build (load average about 9): 243 k operations/s with the counting host,
151 k/s with the applying one.

### The soak, 60 s

| | Target | Achieved (mean, slowest second) |
|---|---|---|
| firehose, `Ticker.set` through `call_sync` | 100,000/s | 99,996/s, 99,760/s |
| keyed churn, 20 operations per call on 10,000 rows | 20,000 ops/s | 20,000/s, 19,959/s |
| completions, 128 in flight, 8 completer threads | 50,000/s | 49,985/s, 49,210/s |
| stream, the consumer granting 1,000 credits a millisecond | 1,000,000 items/s | 999,989/s, 998,397/s |

6.0 M firehose writes, 1.2 M churn operations, 3.0 M completions, 60.0 M stream items and 10.2 M change-sets
were delivered; the drain thread (60 Hz) found at most 6,633 change-sets in one frame. Every invariant held:
nothing out of order, no completion lost, the store's total equals the number of completions, arrives as the exact
count `0, 1, 2, ..` and is the last value the main thread applied, the host's 10,000-row list equals the core's after
1.2 M patches, the stream at most one item ahead of its credit at the end (scenario d checks it after every round),
no warning logged. **RSS** (`ps`, sampled once a second; see the macOS caveat in Method): 10.69 MB at 1 s, 11.00 MB
at 6 s, 11.06 MB from 20 s to 55 s, 11.08 MB at the end, so +0.14% after the half-run warm-up. The firehose's own
p99 inside the mix is 96 us (median over the seconds after the warm-up, worst 121 us), not the 211 ns it has alone: it
is the wait for the core lock behind a 20-operation churn call or a completion burst, which is what mixed load costs;
the number itself is not gated: the soak's latency gates compare the post-warm-up seconds with each other (below).
The p99 trend over those seconds was **-819 ns a second, -26% of the median**, against a limit of +50%.

**The drift gate.** Until this commit the soak's latency gate was a spike gate (the worst post-warm-up second's p99 at
most 3x the median second's), which a steady climb passes: a p99 that doubles over the second half has a worst/median of
1.33. It now has two parts. The spike gate stays. Next to it, a straight line is fitted through the per-second p99s
after the warm-up (the second half), and the line may not rise by more than **half of the median p99** across them
(`undra_bench::stats::drift`). The fit is Theil-Sen, the median of the slopes between every pair of seconds, not least
squares: one bad second at the end of the window tilts a least-squares line (a flat 40 us series with one 400 us
second at the end rises by 40% of its median under least squares and by 0 under Theil-Sen), and that second is the spike
gate's business. A trend is fitted to at least six seconds, which a 10 s run has exactly. Evidence, all in the unit tests
of `bench/src/stats.rs` and on the real soak:

* a synthetic p99 that climbs steadily, 40 us to 100 us over 30 seconds with 8% jitter, rises by 80% to 140% of its
  median and **fails**; one that doubles steadily (40 us to 80 us, the review's case) **fails** and passes the old gate;
  a flat series with 25% jitter, a 20% climb with 10% jitter and a falling series pass; two bad seconds at the end do
  not tilt it;
* the second half (30 to 60 s) of a real 60 s soak on this host (31 seconds, 57 to 125 us) fits at -5% and **passes**
  (the series is `REAL_SOAK_SECOND_HALF` in the tests); the published 60 s soak above passed at -26% of the median
  (`2026-09-30-soak-60s.json`). Of nine 60 s runs of the final gate on the unmodified core, eight passed, between
  -101% and +9% of the median, and **one failed at +52% against a limit of +50%**, at a load average of 5.3 while another
  build ran: on a shared machine a gradual rise in load is indistinguishable from a rise in latency, so the gate has a
  false-positive rate here (about one run in nine at 60 s, none in five 10 s runs) and CI's `--attempts 2`, each
  attempt a fresh process, is what absorbs it;
* a commit made to slow down steadily while the soak runs (a spin of 60 iterations per second of uptime added to every
  commit, never committed: firehose p50 251 ns at 1 s, 847 ns at 10 s, 1.79 us at 30 s, 3.52 us at 60 s, the p99 from
  57 us to 143 us) is `2026-09-30-commit-ramp-soak-60s.json`. Every other gate passes it (the spike gate at 1.7x the
  median, RSS +0.00%, every rate at 100%, every invariant); the drift gate fails it: "a rise of +95% of the median across
  31 seconds after warm-up"; and with `--attempts 2` both fresh-process attempts of a 30 s version failed the same way
  (+90% and +82%).

The limit is the host, not the gate: this machine's scheduler moves the firehose thread between fast and slow cores, and
the p99 flips between a regime around 40 us and one around 110 us, in one 60 s run at 7, 11, 15 and 27 s. A step that
falls inside the second half and does not return reads as a climb (tested: 40 us then 110 us fails). That is why CI runs
the soak with `--attempts 2`, each attempt a fresh process that lands in its own regime, and why the gate reads the
second half only.

RSS on macOS climbs in page-sized steps early under this load (allocator magazines and thread stacks
settling): 10.39 MB at 1 s, 10.52 MB at 6 s, 10.58 MB from 11 s on in the run above, and in two of the four
60 s runs that counted only the first 20% as warm-up it took further steps at 17 to 33 s and ended at +0.88% and
+2.05%. It is flat afterwards, so the soak's warm-up is the first half of the run; both 60 s runs with that rule
ended at +0.00%, and the 10 s CI configuration (5 s of warm-up) passed five runs in a row, after one of an
earlier six ended at +1.44% and one missed its completion rate while another build ran. That is the host
allocator, not the core, and it is why CI runs the soak with `--attempts 2`: a run that fails only a noisy gate
is repeated, a broken invariant is not.

### Method

* **Warm-up** is excluded and configurable. Every sustained scenario first runs for a warm-up, results
  discarded (thread start-up, cold caches, the first allocations): a tenth of the measured run, at most 200 ms, so a
  10 s run warms up for 200 ms and a 2 s CI run for 200 ms. `UNDRA_STRESS_WARMUP_MS` sets it (`0` measures from the
  first operation); each result file records the warm-up it used. The measured run is then exactly
  `UNDRA_STRESS_SECONDS`; throughput, percentiles and bytes are of that run only, while the invariants cover both (a
  patch lost during the warm-up still fails). The stream has no warm-up of its own: its slow half and the 20% RSS
  warm-up do that job. Before this change no scenario excluded one, which at CI's 2 s put thread start-up and the
  first cold calls into the completions histogram. The soak's warm-up is `--warmup PCT` (default half the run: the RSS
  and drift gates look only at what follows; the rate gate counts every second).
* **Sustained throughput** is operations divided by the wall time of the measured run, not the sum of
  per-operation samples. **Latencies** time every operation with one `Instant` pair into a fixed log-linear
  histogram (1/32 relative error, no allocation per sample), so p999 is of millions of samples; a percentile is
  the upper bound of its bucket. `Instant` ticks at 41.67 ns on this host, so a 125 ns p50 is three ticks, and
  the sub-microsecond percentiles are whole ticks (a 211 ns p99 is five, give or take one). Timing every
  operation costs about 35 ns of wall time per sample here (an empty timed step runs at 28 M/s), which the
  throughput includes: the firehose's 6.2 M/s is the rate with that clock (and the host's copy) in the loop,
  against 7.6 M/s for the same call timed in batches with a host that only counts (layer A, 131 ns).
* **Bytes** are what the host's callback was handed (`payload.len()`) and are deterministic: every load is a fixed
  cycle at fixed widths. The gate in `budgets.toml` (`bytes_per_op`) is a **ceiling**, not an equality: a run that
  ships fewer bytes passes it. What makes it exact is each scenario's own invariant in code, which asserts the exact
  count (37 bytes per change-set for the firehose, event and both completions scenarios, 61 per operation for
  churn, 21,012 and 33,000 per transaction for the fan-outs); a scenario that shipped fewer bytes fails there.
* **Memory** is resident set size (`/proc/self/status` on Linux, `ps -o rss=` on macOS, nothing else), sampled
  outside the timed region and page-granular (16 KiB here), so the gate is "at most 1% **or** 64 KiB" from the
  first sample after the warm-up to the last. A platform that cannot be sampled reports the gate as skipped, never
  as passed. **A flat RSS is weaker evidence on macOS than on Linux**: `ps`'s `rss` leaves out the pages the system has
  compressed, so under memory pressure a leak that was written once can be compressed away and the line stays flat.
  `phys_footprint` is the right metric there, and it needs `proc_pid_rusage` (FFI), which R2 keeps out of this crate.
  Both the stress test and the soak print this next to their RSS verdict; trust the 60 s soak over the 10 s one, and
  prefer a Linux run (CI) for a verdict on small leaks. The exact check is an allocation-balance test in `undra-ffi`
  (review, "A stricter memory check"), which is not built.
* **Invariants** are asserted in code and are not scaled or retried: a fast core that loses or reorders a
  change-set fails. They have teeth: tests make the host drop every 101st patch, drop one single patch (an
  update that a later write of the same row repairs, so only the count of applied patches sees it), and make
  the main-thread model swap two change-sets or lose one on its way to the UI (in scenarios e and e'), and
  require the equality, patch-count, order, loss and exact-count invariants to fail. In e and e' the store's
  total is a signal every write adds one to, so the main thread checks that it arrives as `0, 1, 2, ..` with no
  step that is not +1, which catches a lost, repeated or reordered change-set in one test. The one timing comparison a scenario makes about itself (fan-out over 100,000 observed at most 4x the
  10,000 case) is a gate, retried like the others, not an invariant.
* **Host stand-ins**: a copy of every change-set (what each FFI callback does), a host list that decodes and
  applies each keyed patch, and a "main thread" that drains once a frame and checks per-store order. They stand
  in for the platform's mailbox and list state; they cost far less than the platform's own work does.
* **Where a number comes from.** The harness writes the JSON behind a run when `UNDRA_BENCH_RESULTS_DIR` names a
  directory: one file per sustained scenario (`<date>-<scenario>.json`), one for the layer A rows and ratio gates
  (`<date>-layer-a.json`) and one for a soak (`<date>-soak-<N>s.json`), each with the numbers, the command, the machine
  (CPU, cores, OS, compiler, commit) and the one-minute load average before and after. The published numbers
  are those files, in `bench/results/`, and the sections above name them; CI uploads the same files of every run as an
  artifact. `UNDRA_BENCH_RESULTS_TAG=name` puts a tag in the file names for a run that is evidence and not a published
  number. The `measured_*` values in `budgets.toml` are different: the best of three runs at the time a gate was set,
  kept for humans and checked to be inside their gates.
* **Budgets** follow the file's rule: floor = measured / 5, p99 ceiling 5x, p999 ceiling 10x, bytes exact,
  RSS 1%; each layer A row 5x its p50. Reproduce: `UNDRA_STRESS_SECONDS=10 cargo test -p undra-bench --test
  stress --release -- --nocapture`, `cargo run -p undra-bench --release --bin soak -- --seconds 60`,
  `cargo bench -p undra-bench --bench stress` (criterion, for humans).

### Gates that catch a 2x regression

The absolute budgets are 5x what the reference host measures (CI doubles that with `UNDRA_BENCH_SCALE=2`), so that a
slower runner passes. That is also why they cannot see a regression of 2x. The bench review (M1) made the commit path
slower on purpose, with a spin in `undra_signals::txn::commit` (never committed), and every gate passed. Repeated on
the reference host (load average 3), against a control run of the same binary with the spin off; the files are
`2026-09-30-commit-control-*.json` and `2026-09-30-commit-spin-*.json` in `bench/results/`:

| Row | Control | With the commit slowed (about +71 ns per commit) | Absolute budget | Margin left |
|---|---|---|---|---|
| `stress/firehose/txn_x1000` (a commit and its delivery, 1,000 times) | 77.5 us | **148.5 us** (1.92x) | 400 us | 2.7x |
| `stress/firehose/call_set` | 125.5 ns | 223.9 ns (1.78x) | 630 ns | 2.8x |
| `stress/firehose/event` | 117.6 ns | 205.6 ns (1.75x) | 580 ns | 2.8x |
| every other layer A row | | unchanged | | 4.0x or more |
| `firehose/sustained` (2 s) | 5.78 M/s, p99 211 ns | 4.01 M/s, p99 335 ns | floor 1.1 M/s, p99 1.1 us | 3.6x, 3.3x |

Two gates that do not depend on how fast the machine is close that, and a third mechanism makes the second usable on
CI.

**Ratio gates** (`[ratio."..."]` tables in `budgets.toml`) compare two layer A rows measured **in the same run**, so
the machine's speed cancels. Each `max` is 1.15x the largest ratio seen on any healthy sample (the reference host's
runs and runs of the Bench workflow on a GitHub runner, Oct 2026), rounded up to 0.1. They were set from six runner
runs; six more came after and are the out-of-sample check (the review of these gates read them): four ratios stayed
inside what the first six showed, and the fan-out ratio did not (2.37 against its first maximum of 2.3, on a docs-only
commit), so its maximum is now the bound of what it guards (below).

| Gate | Rows | Healthy, host and twelve runner runs | `max` | Control | Commit slowed |
|---|---|---|---|---|---|
| `commit_vs_bare_call` | `txn_x1000` per transaction / `dispatch/call_sync/add` | 1.24 to 1.94 | 2.3 | 1.74 | **3.37 (fails)** |
| `set_vs_bare_call` | `call_set` / `dispatch/call_sync/add` | 2.25 to 3.05 | 3.6 | 2.82 | **5.08 (fails)** |
| `event_vs_set` | `event` / `call_set` | 0.76 to 0.93 | 1.1 | 0.94 | 0.92 (the commit is in both) |
| `fanout_100k_vs_10k_observed` | the two fan-out rows | 0.99 to 2.37 | 4 (not 1.15x: cache-bound; commit cost following the observed count would read about 10) | 1.39 | 1.37 (commit cost follows the dirty count) |
| `keyed_churn_vs_set` | `keyed_churn_10k` per operation / `call_set` | 15 to 47 | 54 | 43.6 | 25.5 |

A ratio can only see a shift larger than **its own spread across hardware**, and the runner runs show how large
that is. The ubuntu-latest pool is not one machine class: the same row measured up to 2.2x apart between runs
(`stress/firehose/txn_x1000` 123 to 275 us, `signals/changeset_100/cell` 1.56 to 4.44 us), in two clusters. Rows of one
family (a commit against a bare call) keep their ratio within 1.2x to 1.5x; rows of different families (a memmove-bound
list operation against an atomics-bound call: the runner runs the list 1.2x slower than the host and the call 3x slower)
within 2x to 3x, which is why `keyed_churn_vs_set` is wide and guards an order-of-magnitude shift on a runner, not a 2x
one. Only the two commit-versus-call ratios are tight enough to see a commit that is 2x slower, and a uniform slowdown
of the whole machine is, by construction, invisible to a ratio. `ratios_hold_on_every_recorded_sample` in
`bench/tests/budgets.rs` holds the recorded samples (the runs are named in its comment) and checks that every gate
passes on every one; the next two tests model a commit `S` times slower on the same samples (a transaction costs `S`
times as much, and `call_set` and `event` pay the difference once). At 2x, `commit_vs_bare_call` fails on all fourteen
samples; at 1.8x on eleven, and the three it misses are fast-class runner runs whose commit is the cheapest against their
call (2.23 to 2.28 against a maximum of 2.3). There the baseline below is the gate: `txn_x1000` alone reads 1.8x its base.

**Baselines** (`bench/baselines/<name>.toml`, `UNDRA_BENCH_BASELINE=<name or path>`) record what one machine class
measured for every layer A row and sustained scenario, and fail a run on that class that is more than 1.5x worse
(p50 1.5x, or 20 ns more where that is more for rows of a few tens of nanoseconds; a throughput under 1/1.5; a p99 over
2.5x). The cache-bound fan-out rows get 2x and the two fan-out scenarios 3x (`baseline_tolerance` in their
`budgets.toml` tables: their tails moved 82 to 295 us between runs on a busy host, and a commit regression shows in the
firehose rows, not in these). `UNDRA_BENCH_RECORD=path` records one (best of three attempts, `UNDRA_BENCH_RECORD_BEST=1`
keeps the better of what the file holds), with the CPU, the commands, the load average and the commit next to the
numbers.
`bench/baselines/apple-m5-pro.toml` is the reference host's (three recordings at load 2.1 to 2.4, best of each row).
Against it, with the spin back on (a weaker one that run, `2026-09-30-commit-spin-vs-host-baseline-*.json`): `txn_x1000`
at **1.61x** its baseline, `call_set` 1.70x, `event` 1.72x, both ratio gates failing (2.85 and 4.92), and the sustained
scenarios at the edge: `event/sustained` 4.83 to 4.93 M/s against a floor of 5.02 M/s on all three attempts (fails),
`firehose/sustained` 4.07 M/s then 4.19 M/s against 4.18 M/s (passes on its second attempt, by 0.2%). A sustained
scenario dilutes a commit regression with the call, the clock and the host's copy, so **the layer A rows are the sharper
instrument and the sustained gates catch a commit regression of about 1.6x and up**. `UNDRA_BENCH_SCALE` does not apply
to a baseline.

**On CI the baseline is the base commit, measured in the same job.** A baseline recorded on one machine is worth
nothing on another, and the runner pool is at least two machines, so `bench.yml` records the baseline from the
**base commit on the same VM** with `scripts/bench-record-base.sh` (this tree's harness against the base's crates, in
a target directory of its own, best of three attempts per row, 2 s per scenario), then gates the head against it. For
a pull request the base is the first parent of the merge ref; for a push it is the commit the push replaced, which is
why no push's run is cancelled by the next push's (a cancelled run would leave that push compared with nothing, and the
next one compared against it). The same machine, minutes apart: its speed cancels by construction. Run end to end on the reference host (the baseline is
`bench/results/2026-09-30-commit-experiment-base.toml`, recorded about two minutes before the runs it gates):

| | Layer A rows against the base | Ratio gates | `firehose/sustained`, `event/sustained` |
|---|---|---|---|
| the head unchanged (`commit-control`) | `txn_x1000` 0.99x, `call_set` 0.98x, `event` 1.01x; no row over 1.4x | pass (1.74, 2.82) | 5.78 M/s, 7.03 M/s: pass |
| the head with the commit slowed (`commit-spin`) | `txn_x1000` **1.90x**, `call_set` **1.75x**, `event` **1.77x** | **fail** (3.37, 5.08) | 4.01 M/s, 4.60 M/s against floors of 4.20 and 4.97 on all three attempts: **fail** |

A commit that slows down **while the process runs** is the soak's: a spin of 60 iterations per second of uptime added to
every commit leaves the firehose p50 at 251 ns, 847 ns and 3.52 us at 1 s, 10 s and 60 s, and the soak's p99 trend gate
fails it (`2026-09-30-commit-ramp-soak-60s.json`, below).

What none of this sees: a regression under 1.5x (the baseline) or under a ratio's own spread, a regression in a row the
base does not have, and anything when the base does not build against the head's harness (the run then carries a
warning saying so, and the gates are the absolute budgets and the ratios). Noise is the other side of 1.5x: in the spin run a row the spin cannot
touch, `signals/keyed_100/insert` (333 ns), read 1.43x its baseline on a machine at load 3, and it passed because the
best of three attempts counts; the first CI run of this workflow is the first time `bench-record-base.sh` runs on a
runner, and it is an A/A run by construction (this change touches no crate, so the base and the head build the same
core): a row that proves too noisy there gets `baseline_tolerance` in its own `budgets.toml` table, the only place a
per-row factor survives a baseline recorded afresh in every job.

### What these numbers are not

They are a host's core side, not a device's, and not the platform half. The same firehose reaches the UI
as 100,000 change-sets a second to copy, decode and apply (and a keyed patch copies the whole list on
TypeScript and Kotlin), which is what ADR-031 changes; its before and after on the platforms, measured
on the simulator, the emulator and Chromium, is in [Device numbers](#device-numbers-ios-android-web)
(the drain table) and in the ADR's consequences. Until a device is attached the only claim these rows support is
the one they measure: **the core commits, delivers and keeps order and memory at rates 100x any UI's.**

## Findings

### 1. A keyed patch was O(list), not O(change); with recorded list operations it is O(change) (resolved, ADR-027)

The blueprint row says lists must be "O(change), not O(list)". It was not: the cost was linear in the list and
independent of the change, and the 10,000-row insert missed its row by ~27x. Before and after, the same
operation through the runtime (`signals/keyed_*`, criterion medians):

| Rows | One insert | One update | One move | Per row (insert) |
|---|---|---|---|---|
| 100, before | 6.28 µs | | | 63 ns |
| 1,000, before | 55.74 µs | | | 56 ns |
| 10,000, before | 536.16 µs | 532.32 µs | 702.95 µs | 54 ns |
| 100, recorded | 359.1 ns | | | |
| 1,000, recorded | 769.7 ns | | | |
| **10,000, recorded** | **6.31 µs** | **272.3 ns** | **9.74 µs** | 0.6 ns |

**The cause** was the design that `StoreCell`'s docs described ("each commit costs O(n) to compute the patch"): at every
commit `KeyedList::diff` (`crates/undra-signals/src/store.rs`) called `KeyedPatch::diff`
(`crates/undra-wire/src/patch.rs`) over the old and the new list, and that diff hashes every key of both lists into two
`HashMap`s and builds position tables before it looks at what changed. Measured on its own, with a plain `u64` key and
`PartialEq` (`wire/keyed_patch_10k/diff`), it takes 456.79 µs: **85% of the 536.16 µs**; the rest was the generated key
function, the encoded comparison of surviving rows and the baseline replay.

**The fix** (ADR-027) records the operation instead of rediscovering it. `Signal<Vec<T>>` has `push`, `insert`, `remove`,
`update_at`, `move_item` and `clear`, which mutate the list and append the SPEC 3.8 op they performed to a log; a commit of
a list written that way sends the log as the patch and replays it on the baseline, O(ops), with no key hashing and no item
comparison. The wire is unchanged. The benchmark fixture's store methods now use them, which is what generated store code
does. What remains at 10,000 rows is the `memmove` of the vector's tail, which the core's own `Vec::insert` needs and which
is paid twice (the list and the baseline): 0.6 ns per row, so an insert into the middle of 10,000 rows is 6.3 µs, and an
update, which moves nothing, is 272 ns (dispatch, argument decode, the write and the patch). The host side is unchanged
and cheap: decoding a one-op patch and replaying it on a 10,000-row list is 2.57 µs (`wire/keyed_patch_10k/apply`).

**The fallback** is unchanged on purpose. A list written with `set`, `update` or `replace` (a whole-list refresh, or an
edit no recorded operation can express) is still found by diffing it against what the host has: the same one-row edit
through the raw `update` is `signals/keyed_10k/raw_update_diff`, 528.7 µs in the gate harness (534.7 µs before, same
machine and load), and a transaction that mixes recorded operations with a raw write is diffed as a whole. Its budget is
guarded at the old level; the two paths are different rows so neither can hide a regression in the other.

### 2. The handle method call: the allocator was 60% of it, ADR-028 took it out, the row is within target on the host

The row was the second miss: `Runtime::call_sync` of `add(i64, i64)` was 77.8 ns when first measured (1.30x the 60 ns
target) on a core faster than the iOS device's. A sampling profile of the call (macOS `sample`, ~4,900 samples of the
loop) put **about 60% of it in the allocator**: `malloc` and `free`, plus a `mach_absolute_time` read that this OS's
`libmalloc` does inside every allocation (the top symbol, 23% of the samples). The call allocated at least three times:
the dispatcher's encoded return value (`encode_to_vec`), its `Box<dyn Any>` outcome, and the `Reply` payload `Vec`.

ADR-028 removes all three. A generated dispatcher answers a synchronous method with `rt.sync_ok(&value, Encode::encode)`
(`sync_err` for a typed error): when `call_sync` has armed the thread's reply slot, that encodes the whole `Reply` payload
(`call_id`, status, value) into one reusable thread-local buffer and returns a zero-sized outcome (boxing a zero-sized
value does not allocate); `Runtime::call_sync_with` lends the buffer to a closure. `Runtime::call_sync` copies it into the
`Vec` it returns, which is the one allocation left and the one the C ABI owes (`undra_call_sync` hands that `Vec` over as
the `UndraBuf` the caller frees). Whatever the buffer cannot serve (`undra_call`, dispatch layers, a call nested in another,
a second runtime) takes the old allocating path, byte-identical on the wire. `crates/undra-ffi/tests/sync_alloc.rs` counts
allocations with a global allocator and holds the path to exactly 0 per `call_sync_with`, 1 per `call_sync` and 1 per
`undra_call_sync`. Four smaller costs on the same path went with it: SipHash on the `u32` dispatch ids, a linear scan of
the object's method list per call, an `Arc` reference taken twice per receiver lookup, and several thread-local accesses
in the panic guard.

The parent commit was built in a second worktree and measured back to back with the new build (criterion medians, best of
three interleaved rounds each; the machine was shared, load average 5 to 20, and the single-threaded rows agreed within
about 3% between rounds when it was not saturated):

| Benchmark | Before | After |
|---|---|---|
| `dispatch/call_sync/add` (returns a `Vec`) | 73.8 ns | 43.9 ns |
| `dispatch/call_sync_with/add` (new: no allocation at all) | n/a | 31.5 ns |
| `dispatch/call_sync/function` | 65.4 ns | 35.8 ns |
| `dispatch/call_sync/echo_record1k` | 268.4 ns | 139.5 ns |
| `boundary/call_sync/add` (C ABI, one `UndraBuf`) | 79.3 ns | 49.8 ns |
| `boundary/call_sync/unknown` (status 5: formats a reason, allocates by nature) | 106.8 ns | 107.3 ns |
| `boundary/call/add` (the async entry; the slot is not armed there) | 102.3 ns | 99.2 ns |

What is left of the 31.5 ns, from a sampling profile of `call_sync_with`: about a quarter is entering and leaving the core
lock (re-entrancy check, the lock, the current-runtime scope with its `Weak::upgrade`), about a sixth the two receiver
lookups (the object table's reader lock and an `Arc` reference, once to route and once for the dispatcher), a tenth the
panic guard, and the rest payload decode, the dispatcher and the reply encode. The cheapest next steps are resolving the
receiver once per call instead of twice, and a cheaper re-entrancy check (one thread-local read instead of two);
neither changes a contract.

**The row is not closed.** 43.9 ns against 60 ns is a pass on a core faster than an A15, which is necessary and not
sufficient: the row passes on the device only if an A15 core runs this path within 1.37x of this core's time (60 / 43.9),
and this note has no A15 to measure. The verdict belongs to the device phase. `undra_call` (the async entry) still
allocates its reply; it is not on the synchronous row's path.

### 3. `Runtime::new` + drop, without `shutdown()`, leaks the runtime and two threads

Found while building the cold-start benchmark: a `Runtime` created with `Runtime::new` and merely dropped is never
freed, and its `undra-core` thread and a second thread stay alive (100 create/drop cycles: +200 threads; after
about 1,500 cycles thread creation on this host degrades from ~100 µs to several ms). `shutdown()` before the
drop fixes it (+0 threads, runtime freed). The cause is a reference cycle through the `undra-query.hydrate`
init hook: it spawns a task that holds a `Ctx` and parks on the unavailable `Kv` port, so the executor owns a
task that owns the runtime. `TestRuntime` and `undra-transport`'s server both call `shutdown()`, so nothing in
the tree trips over it, and `Runtime::shutdown` documents that surviving `Ctx`s keep a runtime alive; but an
embedder that relies on drop leaks silently. The bench harness wraps its runtimes so they are shut down.

### 4. Expensive wire types

Everything scalar, string, option, enum and record round-trips in tens to a few hundred ns, and the 1 KB record in 228.0 ns.
The two that stand out are maps: a 100-entry `HashMap<String, u32>` is 6.46 µs and `HashMap<u32, u32>` is 3.26 µs round trip, dominated by
the encode (3.30 µs and 2.48 µs): maps encode their entries sorted by the encoded key bytes, so every encode encodes each key
into a scratch buffer and sorts. `Vec<u32>` of 1,000 is 1.12 µs (0.63 ns per element to decode): it goes element by element rather than as one copy. None is
near a section 14 row; they are the first places to look if a large-collection command ever shows up in a profile.

### 5. A computed list over a keyed list cost the list again; a derived list costs the change (ADR-039)

ADR-027 made a keyed list cost the change, but the first `Computed<Vec<T>>` over it (the playground's `visible`, the
shape every app has) cost the list again: recomputed at every commit with a clone of every passing row, sent as a full
value, decoded whole on every platform. ADR-039's `DerivedList<T>` (`list.derive().filter(..).sort_by_key(..).build()`)
is kept from the list's recorded operations on an index of two order-statistic trees, O(log n) per changed row, and
reaches the host as at most two keyed-patch ops per source op. The same change before and after
(`bench/tests/derived_before_after.rs`, `bench/results/2026-10-01-derived-lists-before-after.json`): one `update_at` of a
visible row's title, `visible` = the rows not done (75%), `undra-signals` directly with both slots observed and a sink
that copies each change-set, median per change, best of three; titles of 34 characters, load 6-9:

| Rows | Before: `Computed<Vec<_>>` | Change-set | After: `DerivedList` | Change-set | The keyed list alone |
|---|---|---|---|---|---|
| 1,000 | 21.0 µs | 35.4 KB | 292 ns | 158 bytes | 209 ns, 85 bytes |
| 10,000 | 176.5 µs | 352.6 KB | **333 ns** | **158 bytes** | 250 ns, 85 bytes |
| 100,000 | 2.24 ms | 3.53 MB | **375 ns** | **158 bytes** | 292 ns, 85 bytes |

The view adds 80 ns and one 73-byte entry to the change it follows, whatever the list's length. On the platforms
(`bench/results/2026-10-01-derived-lists-platform-apply.json`: each runtime's own reader, generated `Todo` codec and
`applyPatch`, throwaway probes like ADR-039's, best of three medians), applying `visible`'s entry:

| Rows (visible) | TypeScript, Node 24: full / one op | Kotlin, JVM 17: full / one op | Swift 6 `-O`: full / one op in place / while a view holds the array |
|---|---|---|---|
| 1,000 (750) | 104.5 µs / 0.46 µs | 15.2 µs / 0.21 µs | 81.2 µs / 0.38 µs / 4.9 µs |
| 10,000 (7,500) | 1.07 ms / 1.62 µs | 152.2 µs / 1.13 µs | 832.2 µs / 0.38 µs / 46.3 µs |
| 100,000 (75,000) | 11.27 ms / 47.3 µs | 1.50 ms / 7.2 µs | 8.97 ms / 0.42 µs / 513 µs |

What a platform still pays at 100,000 rows is its own list copy (TypeScript's and Kotlin's `applyPatch` return a new
list; Swift copies on write when SwiftUI holds the array), once per drain (ADR-031), not the core.

**The gate rows** (`signals/derived_*`, through the runtime as the keyed rows are; each checks the shape of what it
ships before it is timed; `bench/results/2026-10-01-derived-lists-layer-a.json`, best of three p50s on a host at load
6-10) and ADR-039's targets, every one met:

| Row | p50 | ADR-039 target | Budget (CI) |
|---|---|---|---|
| `signals/derived_10k/update_visible` (`Update`, `Update`) | 392 ns | ≤ 1 µs | 2 µs |
| `signals/derived_10k/toggle_membership` (`Remove` / `Insert`) | 379 ns | ≤ 1.5 µs | 1.9 µs |
| `signals/derived_10k/sort_key_change` (`Move` + `Update`) | 555 ns | ≤ 2 µs | 2.8 µs |
| `signals/derived_100k/update_visible` | 378 ns | ≤ 2 µs | 1.9 µs |
| `signals/derived_100k/sort_key_change` | 607 ns | ≤ 4 µs | 3.1 µs |
| `signals/derived_10k/insert_sorted` | 6.50 µs | ≤ 1.5x `keyed_10k/insert` (6.17 µs): 1.05x | 33 µs |
| `signals/derived_10k/param_flip` (2,500 rows move: the full value) | 217.6 µs | ≤ 300 µs | 1.1 ms |
| `signals/derived_10k/rebuild_after_replace` | 174.6 µs | ≤ 1 ms | 880 µs |
| `signals/derived_10k/count_toggle` (`count()` as a `Computed<u32>`) | 388 ns | ≤ 1 µs | 2 µs |

The ratios: `derived_sort_scaling` (100,000 against 10,000 rows) is 1.04-1.12 (max 4: an O(n) step would be near 10),
`derived_vs_keyed_update` 1.36-1.65 (max 2.5). The first measurement missed two targets (`param_flip` 1.43 ms,
`rebuild_after_replace` 1.37 ms, on a loaded host): the parameter walk computed a rank for every row that stayed, every
full value was materialised (a clone of each row) before it was encoded, and the rebuild row's fixture formatted 10,000
titles inside the timed call. The walk now skips rows whose membership and key did not change, ranks are computed only
when an op is kept for the host (an unobserved list and a `count()` never compute one), a full value is encoded straight
from the source rows, and the fixture swaps in a prebuilt list.

**Sustained** (`derived_churn_10k/sustained`, the keyed churn cycle on a list with a sorted view of it, both mirrored by
the host; `bench/results/2026-10-01-derived-lists-derived_churn_10k-sustained.json`): **122,000 operations a second**,
p50 5.8 µs, p99 24.6 µs, p999 100 µs, 93.0 bytes per operation, against 169,300 a second for the keyed churn alone in
the same run (ADR-039's floor: half of it). Over 1.24 million operations the host's view equalled `filter + stable sort`
of the core's rows field for field, 896,219 view patches were applied of 896,219 delivered, and the core rebuilt the
index once (the observe).

### 6. Objects and callbacks cross at the price of a call (ADR-040, ADR-041)

An object passed to the core is one `u64` in the call and one table lookup in the dispatch; an object handed to the host
is a table entry (or one more reference on the entry the host already holds) and the same `u64` in the reply. A
callback method is a port call with the instance first in its arguments. None of it is allowed to cost more than a
call plus the thing it does, and the gate rows say what that is (the budgets test's own timings, best of three p50s on a
shared host at load; `with_reset` rows are timed per iteration with the release that gives the reference back outside
the clock, so their resolution is the 41.7 ns tick of the macOS timer):

| Row | p50 | What it is | Budget (CI) |
|---|---|---|---|
| `dispatch/call_sync/add` (the yardstick) | 49.4 ns | a call with two integers | 250 ns |
| `dispatch/call_sync/object_param` | 48.7 ns | `Calculator.dock_slots(&Dock)`: the handle resolved and downcast | 250 ns; ratio to `add` at most 2.0 (1.03 measured) |
| `dispatch/call_sync/return_object` | 84.0 ns | a method returns an object the host does not hold: a new entry, one host reference | 420 ns |
| `dispatch/call_sync/return_interned_object` | 84.0 ns | the object the host already holds: the same handle, one more reference | 420 ns |
| `boundary/port_call/notify` | 24.3 ns | a fire-and-forget port call (the floor of any callback method that answers nothing) | 250 ns |
| `boundary/callback/notify` | 43.2 ns | the same through a `CallbackHandle` (the instance first, the weak proxy upgraded) | 250 ns |
| `boundary/callback/async_roundtrip` | 182.7 ns | an `async` callback method: the call, the host's `port_reply`, the task woken and resumed (one thread, the host drives) | 920 ns |

Returning an object costs about one extra call's worth over the bare call (the table insert and the reference count
under the table's lock); passing one costs nothing measurable over the integer it replaces. Both gates sit far below
anything a UI sees. What a platform adds (a wrapper object, the identity map's lookup, the registry's entry) is measured
on the platform, in the contract scenarios' `adopt` and `lend` timings recorded in the piece's decision record, not
here.

### 7. A lazy list costs the window the host shows and the 12 bytes that say it changed (ADR-043)

A 100,000-row table is not a value the host should mirror. `Lazy<T>` keeps the items typed in the core; the host is
told the length and a version (change-set op 0 on observe: handle, length, version, 20 bytes), asks for the 50 rows
it shows with a page call, and is told that something changed by one 12-byte op-2 entry (`LazyInvalidated`: the new
length and version), whatever the change was, after which it asks for its window again. `Lazy::over` a derived list
pages through the derived index (the `k`-th row in O(log n), a page in O(log n + limit)). The gate rows (`lazy/*`,
through the runtime as the keyed rows are; each checks what it ships before it is timed: the 50 rows decode and equal
the model, the page header's version is the one `observe` announced, and the invalidation is one change-set of one
op-2 entry of exactly 12 bytes carrying the new length and version;
`bench/results/2026-10-01-lazy-lists-layer-a.json`, best of three p50s on a host at load 20-35) and ADR-043's targets:

| Row | p50 | ADR-043 target | Budget (CI) |
|---|---|---|---|
| `lazy/page_50_of_100k` (50 rows from the middle: dispatch, the page server, the encoding, the reply) | 290 ns | core half <= 20 us | 1.5 us |
| `lazy/page_50_of_10k` | 288 ns | (the same row on 10,000) | 1.5 us |
| `lazy/view_page_50_of_100k` (the rows not done, by title, of a 100,000-row view: found in the derived index) | 3.20 us | (no target; O(log n + limit)) | 16 us |
| `lazy/invalidate` (one `update_at` of an observed 100,000-row list: the write, the commit, 12 bytes) | 178 ns | <= 1 us, 12 bytes | 900 ns |
| `lazy/invalidate_10k` | 171 ns | (the same row on 10,000) | 860 ns |

The ratios: `lazy_page_scaling` (100,000 against 10,000 rows) is 1.00-1.01 and `lazy_invalidate_scaling` 0.99-1.16 (max 2
each: an O(n) step, the list cloned or encoded on the way, would put either near 10). Nothing grows with the list: the
allocation gate (`crates/undra-ffi/tests/lazy_alloc.rs`) counts the commit of an observed `Lazy` at one buffer more than
the same write to a plain observed counter (the 12-byte entry), the same number at 10 and at 100,000 items, and zero
allocations to encode a page of 50 rows into a buffer that is big enough. Against what a `Signal<Vec<T>>` of the same
rows would send at a change (a keyed patch of about 85 bytes, or the full value, 3.7 MB at 100,000 rows of this shape), the
invalidation is 12 bytes and the host's cost moves to the page it re-asks for. The view's page is dominated by the
pipeline running on the 50 rows it serves (the sort-key closure clones a title per row); the index lookup is
logarithmic.

### 8. A cold start is the schema hash: `Runtime::new` costs per byte of schema, and 40% less of it now

On 2026-10-02 the two cold-start rows failed the `apple-m5-pro` baseline: 123.83 µs and 133.79 µs against 68.04 µs and
80.38 µs recorded two days earlier (1.82x and 1.66x; the gate is 1.5x). The baseline's commit rebuilt with the same
compiler (rustc 1.99.0) still measured 72.96 µs, so it was the code. A bisect over the 973 commits in between found no
commit to blame but a ramp, and a probe run at each step (the budgets row, best of three p50s, and its parts timed
alone; the machine at load 3 to 7) says why:

| Commit | What it added to this binary's schema | Canonical schema | `Runtime::new` | Restore into it | The row |
|---|---|---|---|---|---|
| `8004a21` (the baseline) | | 22,684 B | 51.5 µs | 19.7 µs | 72.9 µs |
| `ea81c3c` | persistence-v2 with main merged: the typed storage errors, the derived-list fixtures (`Views`, `ChurnViews`) | 26,726 B | 65.5 µs | 21.7 µs | 83.8 µs |
| `8516136` | ports-v2 alone: `Db`, `Sse` and `WebSocket`, linked for the `ports/*` and `db/*` rows | 31,876 B | 80.6 µs | 20.0 µs | 98.3 µs |
| `5413a5c` | both | 35,918 B | 92.9 µs | 22.2 µs | 112.0 µs |
| `b1038fe` | prod-ops: the `Diagnostics` port, the panic and background reports | 37,604 B | 96.1 µs | 21.9 µs | 118.5 µs |
| `a309e9f` (main) | objects, callbacks and lazy lists: their fixtures (`Shelf`, `PushFeed`, `Dock`, the `Pinger` port) | 40,598 B | 103.8 µs | 21.3 µs | 125.8 µs |
| this change | | 40,598 B | 61.1 µs | 21.0 µs | 80.1 µs |

**The restore never moved** (19.7 to 21.3 µs for the 100 KB; the store fingerprint ADR-037 added is about 2 µs of it).
What grew is `Runtime::new`, which collects the schema from the registrations and hashes it, both linear in the schema:
this binary's canonical schema went from 22.7 KB to 40.6 KB (1.79x, the row's 1.8x) as five pieces registered their
ports, records and fixture stores in it. An app pays for the schema it has. Of the 17.9 KB, 9.2 KB are the three opt-in
ports and their types (in the cores that enable them), 5.6 KB are this harness's own fixtures, and 3.1 KB are in every
core: persistence-v2's typed storage errors (1.4 KB) and prod-ops' `Diagnostics` port with the panic and background
reports (1.7 KB). So the regression was the price of what shipped, at an unchanged price per byte (2.3 to 2.6 µs per KB
of canonical schema at every step). That price was the finding:

| Part of `Runtime::new`, 40.6 KB canonical schema | main | this change |
|---|---|---|
| `collect_schema` (the owned definitions from the registrations) | 22.0 µs | 22.5 µs |
| The canonical JSON | 49.1 µs (a clone of the schema with docs cleared and lists sorted, about 20 µs; `serde_json` on it, about 29 µs) | 10.0 µs |
| `fnv1a64` over its bytes (4 cycles a byte; the definition of the hash, SPEC 1.1) | about 38 µs | about 35 µs |
| `Schema::hash` | 88.4 µs | 45.3 µs |
| `Runtime::new` | 103.8 µs | 61.1 µs |

Every start-up copied the whole schema, doc comments included, to drop the docs and sort a few lists, and handed the
copy to `serde`. `undra-meta`'s `schema_json` module now writes the canonical form straight from the schema, in
canonical order (the unordered lists through references in sorted order, `sort::order_by`), as `closure_json` already
did for closures (ADR-052): the same bytes, so no schema hash moved (the previous path is the tests' oracle, on the
fixtures and on generated schemas; every golden with a hash in it is unchanged). `Schema::to_json` (the C ABI's
`undra_schema_json`) is the same walk with labels and docs, so `serde`'s serializer and the derived `Serialize` impls
are no longer in a shipped core: the hello-world web core is 111,355 bytes gzipped where main's is 116,224 with the same
compiler (rustc 1.99.0; `scripts/wasm-size.sh`, the record of 116,690 is 1.98.1's and is left for the pinned toolchain to
re-record).

The rows after it, the budgets test on a host at load 2.4 to 2.8, three runs: `snapshot/cold_start_restore_100kb`
80.12 to 81.29 µs (1.18x the baseline, was 1.82x) and `..._core_thread` 88.12 to 90.00 µs (1.10x, was 1.66x). The
baseline is **not** re-recorded: what is left over it (about 12 µs) is `collect_schema` and `fnv1a64` on the 17.9 KB
this binary's schema gained, and the next piece that adds fixture types to the harness will move these two rows by
about 1.5 µs per KB of canonical schema (it was 2.6). These are quiet-machine numbers: at load 90 and more the
review measured the same rows at 1.41x and 1.20x to 1.37x of the baseline (within the gate, with less room). The hash
has a row of its own now, `snapshot/cold_start_schema_hash` (47.9 µs; the previous path measures 88 µs, 1.84x, which
fails the baseline gates by itself), so the canonical form going back through a clone or `serde` is caught where it
happens. Two things this did not change, both read in the code and not
yet measured on a device: a native core computes all of this twice per launch (`UndraApi::new` hashes the schema for
the table before `init`, then `Runtime::new` collects and hashes it again), and `fnv1a64` is now three quarters of the
hash.

### 9. A snapshot of 100,000 players holds the core lock for what the core does with it, not for what it costs (the leaderboard recipe)

The cookbook's leaderboard (`site/docs/cookbook/leaderboard.html`, `examples/cookbook/core/src/leaderboard.rs`) takes a
snapshot of 100,000 players (1,377,798 bytes of JSON: `[[id, score], ..]`) and a stream of score deltas. The design
the recipe teaches ingests on `spawn_blocking` into a structure the app owns behind its own lock
(`standings.rs`), and publishes a small window of it, the top 50, the 7 rows around one player and four numbers, in one
transaction on the core. The rows below time one `Runtime::call_sync` on the bench thread, and a call holds the core lock
for as long as it runs, so the time of the call is the core-lock hold of one snapshot. The host copies every change-set
under the lock, as every platform's callback does (`CopyingHost`). `standings.rs` is the recipe's own file, compiled into
the harness by path, so these are the numbers of the code the recipe shows. Budgets-test p50 and p99, the best of runs
on the reference host at a load average of 13 (other builds ran), 2026-10-05, rustc 1.99.0:

| Row (`leaderboard/..`) | What the call does under the core lock | p50 | p99 | Change-set |
|---|---|---|---|---|
| `lock_hold/recommended` | publishes the window the blocking pool prepared: three signals, one transaction | **375 ns** | **458 ns** | 780 bytes |
| `lock_hold/in_core_call` | parses, sorts and swaps the snapshot inside the call, then publishes the same window | 2.56 ms | 2.94 ms | 780 bytes |
| `lock_hold/signal_of_rows` | parses and sorts inside the call and sets a `Signal<Vec<Place>>` of every player | 2.94 ms | 3.26 ms | 1,200,033 bytes |
| `ingest/off_core` | (no core lock) what the pool does for the recommended design: parse, sort, swap, window | 2.56 ms | 2.98 ms | none |

The lock is held about **6,800 times** shorter with the recommended design at the median, and (2.94 ms against 458 ns)
6,400 times at the 99th percentile: the cost of the snapshot did not go away, it moved to a thread that does not hold the lock (the
`ingest/off_core` row: the snapshot is on screen about 2.6 ms after it arrives either way). Gated by
`[bench."leaderboard/lock_hold/recommended"]` (p50 1.9 us and a `p99_ns` ceiling of 4.6 us: the first row of the file whose
claim is its tail, so `Stats` has a p99 now and a `[bench]` table may carry `p99_ns`), and by the ratio
`leaderboard_publish_vs_in_core_apply` (the publish is at most 0.05 of the in-call apply; 0.00015 measured): a publish that
grew with the players, the standings read or sorted on the core or every row in a signal, would put it near 1, whatever the
machine. The second wrong design is only 15% more under the lock on a host (the 1.2 MB change-set is encoded and copied
in half a millisecond), but it is what the platform then has to decode and apply every time, which no host-side row
shows; the recommended window is 780 bytes whatever the number of players. (The fixture's window mirrors the
recipe's, the followed row and the version check included; re-measured after the review made it so, the best p50 and
p99 were 375 ns and 459 ns, the same within the 42 ns tick of this host's clock.)

What a caller sees. `cargo test -p undra-bench --test leaderboard --release -- --ignored --nocapture stall_report`
applies a snapshot every ~9 ms (4 ms of work, then 5 ms of sleep) for 8 s per design while another thread makes a call
that does nothing (`ping`: it only needs the core lock) in a loop, and records how long each call took. Best of four runs
at a load average of 13 to 16 (a busy machine adds descheduling noise to the probe itself, so the worst call of a run is the noisiest number here):

| Design | Calls | Wait p50 | p99 | p99.9 | Max |
|---|---|---|---|---|---|
| recommended (pool, then publish) | 269,276 | 125 ns | 2.0 us | 7.8 us | 131.7 us |
| snapshot applied inside a call | 171,176 | 125 ns | 2.9 us | 3.47 ms | 7.04 ms |

A call that arrives during an in-call apply waits out the parse: the p99.9 and the max are that wait (about 4 ms a
snapshot with the probe running; the max adds the probe's own descheduling on a busy machine). A
closed-loop probe sees one such wait per snapshot, about 1 call in 200, so the p99 does not show it; the p99.9 does. With
the recommended design nobody waits more than the publish: the worst call of the best run is 132 us. These are the
numbers of one host; the structure is the claim, the budgets test holds the ratio.

One limit of the same-job baseline for these rows: `scripts/bench-record-base.sh` gives the base worktree the head's
`standings.rs` along with the head's harness (the file is compiled into the harness by path, and a base without it would
have no baseline at all), so both sides of a `leaderboard/*` row run the same `standings.rs`. A change that slows that
file does not show against the baseline; the absolute budgets and the ratio above are what catch it. The core crates,
which are what the baseline gate is for, differ between the two sides as everywhere else.

The harness grew two fixture stores and two records for this: 1.8 KB of the 42,689 bytes of canonical schema it
registers now, so about 2.7 us of `Runtime::new` at finding 8's price of 1.5 us a KB (3% of the two cold-start rows,
which sit at 1.18x their baseline's); the baseline is not re-recorded.

## Full tables

### Wire: encode, decode and round trip per type

`encode` writes into a reused buffer (the codec alone); `decode` reads a fixed byte string into an owned
value; `roundtrip` is what a call argument or return value pays: `encode_to_vec` (one allocation) and
`decode_exact`.

| Type | encode | decode | round trip |
|---|---|---|---|
| `bool` | 0.95 ns | 1.00 ns | 14.7 ns |
| `u8` | 0.95 ns | 0.95 ns | 15.5 ns |
| `u32` | 0.95 ns | 0.97 ns | 15.5 ns |
| `i64` | 1.16 ns | 0.72 ns | 15.6 ns |
| `f64` | 1.96 ns | 0.74 ns | 16.2 ns |
| `string_short` | 3.05 ns | 20.5 ns | 36.1 ns |
| `string_1kb` | 11.1 ns | 49.1 ns | 79.5 ns |
| `bytes_1kb` | 11.0 ns | 28.6 ns | 56.8 ns |
| `option_some` | 1.56 ns | 0.88 ns | 16.5 ns |
| `option_none` | 1.43 ns | 1.07 ns | 15.8 ns |
| `vec_u32_1k` | 258.2 ns | 630.1 ns | 1.12 µs |
| `map100_string_u32` | 3.30 µs | 2.81 µs | 6.46 µs |
| `map100_u32_u32` | 2.48 µs | 770.4 ns | 3.26 µs |
| `duration` | 1.17 ns | 1.04 ns | 16.9 ns |
| `timestamp` | 1.14 ns | 0.77 ns | 15.5 ns |
| `uuid` | 0.96 ns | 1.33 ns | 16.2 ns |
| `record5` | 4.50 ns | 22.3 ns | 96.7 ns |
| `record1k` | 13.6 ns | 68.7 ns | 228.0 ns |
| `enum_rect` | 1.84 ns | 3.02 ns | 56.3 ns |
| `enum_label` | 4.77 ns | 19.9 ns | 53.0 ns |
| `result_ok` | 1.99 ns | 2.71 ns | 16.9 ns |
| `result_err` | 3.43 ns | 23.5 ns | 68.5 ns |

### Wire: keyed patch (undra-wire)

The patch algorithm and its host-side replay on their own, with a cheap key and `PartialEq`. `diff` is the O(list) fallback that a raw write (`set`, `update`, `replace`) takes since ADR-027; a recorded list operation does not run it. Before ADR-027 it was 85% of the keyed signals rows (Finding 1).

| Benchmark | Median | 95% CI |
|---|---|---|
| `wire/keyed_patch_10k/apply` | 2.57 µs | 2.51 µs .. 2.63 µs |
| `wire/keyed_patch_10k/diff` | 456.79 µs | 450.62 µs .. 464.15 µs |
| `wire/keyed_patch_10k/roundtrip` | 136.7 ns | 134.4 ns .. 139.3 ns |

### Dispatch

`Runtime::call_sync` / `Runtime::call` with a prebuilt payload and a host that only counts: `undra_call_sync` without the C ABI. `call_sync` returns the reply as a `Vec` (the one allocation the C ABI owes the host); `call_sync_with` lends the reply buffer instead and allocates nothing (ADR-028). `call_async` includes building the `Call` payload (a host must) and running the executor (`run_pending`) on this thread; there is no thread hop.

| Benchmark | Median | 95% CI |
|---|---|---|
| `dispatch/call_sync/add` | 43.9 ns | 43.8 ns .. 44.0 ns |
| `dispatch/call_sync_with/add` | 31.5 ns | 31.4 ns .. 31.6 ns |
| `dispatch/call_sync/function` | 35.8 ns | 35.6 ns .. 36.0 ns |
| `dispatch/call_sync/echo_record1k` | 139.5 ns | 139.2 ns .. 139.8 ns |
| `dispatch/call_async/ready_add` | 220.6 ns | 217.6 ns .. 224.8 ns |

An instantiation of a generic function (ADR-058, `add_one_for<Record5>`: the body of `add_one`, listed for one type) is a function like any other: `dispatch/call_sync/generic_fn` measured 0.89x to 1.04x of `dispatch/call_sync/function` over six gate runs on the reference host (36 to 48 ns each, the machine shared), and `generic_fn_vs_function` gates the ratio at 1.2.

### Signals and stores

`cell` is the signals crate alone (100 `Signal<u32>` attached to a `StoreCell`, one transaction, a counting sink). `runtime` is the same 100 writes as one method call on a macro-generated store through the runtime. `decode` is a host validating and walking that change-set (borrowed). Keyed rows are one call through the runtime on an observed `Signal<Vec<Item>>` with `#[undra(key = "id")]`, written with the recorded list operations (`insert`, `update_at`, `move_item`); `raw_update_diff` is the same one-row edit through the raw `update`, which takes the diff path. Insert runs against a list that is restored outside the timed region. The wide interval on `raw_update_diff` is machine load (the gate harness measured 528.7 µs p50).

| Benchmark | Median | 95% CI |
|---|---|---|
| `signals/changeset_100/cell` | 760.9 ns | 749.6 ns .. 771.4 ns |
| `signals/changeset_100/runtime` | 2.30 µs | 2.27 µs .. 2.33 µs |
| `signals/changeset_100/decode` | 253.9 ns | 252.6 ns .. 255.1 ns |
| `signals/observe_100_initial` | 2.82 µs | 2.80 µs .. 2.86 µs |
| `signals/keyed_10k/insert` | 6.31 µs | 6.25 µs .. 6.35 µs |
| `signals/keyed_10k/update` | 272.3 ns | 271.5 ns .. 273.5 ns |
| `signals/keyed_10k/move` | 9.74 µs | 9.62 µs .. 9.86 µs |
| `signals/keyed_10k/raw_update_diff` | 579.7 µs | 547.9 µs .. 630.8 µs |
| `signals/keyed_1k/insert` | 769.7 ns | 745.8 ns .. 793.2 ns |
| `signals/keyed_100/insert` | 359.1 ns | 352.6 ns .. 366.0 ns |
| `signals/derived_10k/update_visible` | 391.9 ns | gate harness p50, best of three (finding 5) |
| `signals/derived_10k/toggle_membership` | 378.5 ns | gate harness |
| `signals/derived_10k/sort_key_change` | 554.8 ns | gate harness |
| `signals/derived_100k/update_visible` | 378.4 ns | gate harness |
| `signals/derived_100k/sort_key_change` | 606.8 ns | gate harness |
| `signals/derived_10k/insert_sorted` | 6.50 µs | gate harness |
| `signals/derived_10k/param_flip` | 217.6 µs | gate harness |
| `signals/derived_10k/rebuild_after_replace` | 174.6 µs | gate harness |
| `signals/derived_10k/count_toggle` | 387.9 ns | gate harness |
| `signals/computed/recompute_1` | 42.1 ns | 41.8 ns .. 42.4 ns |
| `signals/computed/recompute_chain_10` | 253.1 ns | 251.0 ns .. 256.3 ns |

### Lazy lists (`bench/benches/lazy.rs`, ADR-043)

`Shelf` is a macro-generated store with an owned `Lazy<Item>` of 100,000 rows (24-character titles, every fourth done)
and a `Lazy::over` view of a derived list of the same rows (the rows not done, by title). `page_*` is one `LazyPage` call
for 50 rows from the middle through `call_sync_with` (the reply is lent, not copied); `invalidate*` is one method call
that `update_at`s a row of the observed list and commits the 12-byte entry. The gate harness measured 290 ns, 288 ns,
3.20 us, 178 ns and 171 ns p50 (the criterion medians below are lower: quieter moments of a shared host).

| Benchmark | Median | 95% CI |
|---|---|---|
| `lazy/page_50_of_100k` | 263.8 ns | 261.5 ns .. 266.5 ns |
| `lazy/page_50_of_10k` | 262.6 ns | 260.5 ns .. 265.0 ns |
| `lazy/view_page_50_of_100k` | 2.834 µs | 2.817 µs .. 2.851 µs |
| `lazy/invalidate` | 151.6 ns | 151.2 ns .. 152.0 ns |
| `lazy/invalidate_10k` | 151.2 ns | 150.5 ns .. 152.1 ns |

### Snapshot and restore

100 KB is four stores of 250 rows of 100 bytes; 1 MB is forty. `cold_start` builds a runtime and restores; the runtime it made is shut down outside the timed region. `restore_100kb_migrated` restores the same 100 KB as an older build wrote it (`Item.id` a `u32`): every store's fingerprint differs, so each one is decoded by name and migrated structurally, streamed (ADR-037). Measured on 2026-10-01 with snapshot layout 2 (ADR-037), the machine shared with other builds; the restore rows read about 10% slower than the earlier run on the same code paths, which is the load, and the cold start pays one fingerprint computation per new runtime (about 2 µs; the 14 µs this file first put on it were the schema this binary hashes at start, which had grown by persistence-v2's fixtures: Finding 8, which also has the two cold-start rows as they are now, 80.12 µs and 88.12 µs).

| Benchmark | Median | 95% CI |
|---|---|---|
| `snapshot/encode_100kb` | 13.12 µs | 12.95 µs .. 13.32 µs |
| `snapshot/restore_100kb` | 30.33 µs | 29.67 µs .. 30.89 µs |
| `snapshot/restore_1mb` | 286.54 µs | 281.57 µs .. 292.14 µs |
| `snapshot/restore_100kb_migrated` | 88.84 µs | 86.40 µs .. 93.03 µs |
| `snapshot/cold_start_restore_100kb` | 84.68 µs | 83.95 µs .. 85.53 µs |
| `snapshot/cold_start_restore_100kb_core_thread` | 90.27 µs | 88.78 µs .. 91.94 µs |

After the hello-world size work at the end of persistence-v2 (the closure reader reads the canonical form only, the
streamer checks lengths only when it copies), the budgets test's p50s on a loaded host: `restore_100kb` 27.97 µs,
`restore_100kb_migrated` 76.48 µs (2.7x the fast path; the ratio gate's bound is 10).

### Web recovery (TypeScript, ADR-049)

`cd examples/playground/web && UNDRA_BENCH_RECOVERY_RUNS=5 npm run bench:recovery` (after `undra build -C
examples/playground --platform web`): headless Chromium 153.0.8010.12 (Playwright), cross-origin isolated (a 5 µs
clock), wasm-main, on the machine above (Apple M5 Pro, macOS 26.5), measured at the end of persistence-v2 with
recovery as `crashRecovery()`, a layer over the transport (load average about 13: other builds were running). The workload is the playground core with one
`Todos` store of 1,000 to-dos and 16 `Counter` stores, 52 signals: a 101,446-byte snapshot.
`ts/snapshot_take_100kb` is what the snapshot keeper pays to keep one (the core's `snapshot` plus the copy kept, 50
batches of 10 per run, so its p99 is over batch means); `ts/recovery_restart_100kb` is a trap until `onCoreRestarted`
(the panic report, failing what was in flight, a new instance of the same compiled module, the restore, re-observing
every store (a query handle the core re-issues is observed again like any store, ADR-059, so the replay of the query handles this row once included is gone); 30 samples per run, so its p99 is the maximum). The ranges are over five
runs in fresh pages. `bench/recovery.spec.ts` fails a run whose p50 is over its budget (R9).

| Benchmark | p50 | p99 | Budget (desktop Chromium) | Verdict |
|---|---|---|---|---|
| `ts/snapshot_take_100kb` | 0.063 ms .. 0.066 ms | 0.077 ms .. 0.094 ms | 2 ms | within |
| `ts/recovery_restart_100kb` | 3.12 ms .. 3.22 ms | 5.33 ms .. 9.59 ms | 50 ms | within |

The first measurement, with recovery built into `UndraCore` (before ADR-052's 26 KB gate on the hello-world runtime
made it an import), read 0.041 ms .. 0.043 ms and 1.55 ms .. 1.64 ms on a quieter host.

### C ABI (`crates/undra-ffi/benches/boundary.rs`)

`call/ready_add` is the only row that includes a real `undra-core` thread hop (spawn, wake, poll, reply on the core thread) and is the one most sensitive to machine load.

| Benchmark | Median | 95% CI |
|---|---|---|
| `boundary/call_sync/add` | 49.8 ns | 49.6 ns .. 50.0 ns |
| `boundary/call_sync/unknown` | 107.3 ns | 106.7 ns .. 108.1 ns |
| `boundary/call/add` | 99.2 ns | 98.7 ns .. 100.1 ns |
| `boundary/call/ready_add` | 5.64 µs | 5.50 µs .. 5.80 µs |
| `boundary/write_observed` | 129.7 ns | 129.3 ns .. 130.2 ns |

### Query client (`bench/benches/query.rs`)

No row of its own in section 14; kept so regressions in the hot paths are visible.

| Benchmark | Median | 95% CI |
|---|---|---|
| `query/observe_cached_and_release` | 516.2 ns | 512.0 ns .. 521.1 ns |
| `query/refetch_published_to_100_observers` | 6.90 µs | 6.82 µs .. 6.98 µs |
| `query/platform_construct_and_release` | 1.19 µs | 1.18 µs .. 1.22 µs |
| `query/platform_refetch_call` | 586.1 ns | 585.0 ns .. 587.4 ns |
| `query/infinite_append_page_50` | 7.29 µs | 7.07 µs .. 7.59 µs |
| `query/keyed_push_50` | 5.41 µs | 5.32 µs .. 5.51 µs |

The last two are budgets-test rows (ADR-043, `bench/common/query_rows.rs`; 2026-10-01, the reference host while other pieces
built, so the ratio is the number to read). `query/infinite_append_page_50` is a platform's `fetch_next_page()` call on an infinite
query whose list holds 10,000 rows: the call, the fetch task, 50 rows appended with the recorded `push`, and the change-set
(asserted, when the row is built, to be a keyed patch of exactly those 50 rows) delivered to the host; every run appends a page, so
the list keeps growing and the cost does not. `query/keyed_push_50` is the same 50 rows appended to a keyed list of 10,000 by a store
method (`call_sync`, one transaction): the "50-op keyed patch" ADR-043 compares with. Budgets 38 µs and 28 µs (5x the best budgets-test
p50, 7.4 µs and 5.5 µs); `[ratio."infinite_append_vs_keyed_push_50"]` holds the first at no more than twice the second (1.34 to 1.46
measured, ADR-043 requires at most 2): the fetch machinery on top of a 50-op keyed patch, however long the list.

### Opt-in ports (`bench/benches/ports.rs`, ADR-047, ADR-048)

The core's side of a call through a port proxy: `ports/*` answered by `FakeWebSocket`, `db/*` by `MemDb`, a real
in-memory SQLite (rusqlite, bundled), so the `db` rows include SQLite's own work but no disk. Budgets-test p50s
(best of three, 2026-10-01, the reference host while other pieces built); budgets are 5x.

| Row | p50 | Budget | What |
|---|---|---|---|
| `ports/ws_roundtrip` | 417 ns | 2.1 µs | one 64-byte text message sent and its echo received (`send`, then `receive` with the credit of 16) |
| `db/insert_1k` | 1.29 ms | 6.5 ms | one transaction of 1,000 bound three-parameter `INSERT`s (`begin`, 1,000 `execute`, `commit`) |
| `db/query_10k` | 1.78 ms | 9.0 ms | one `query` of 10,000 rows of three cells, encoded and decoded into `DbRows` |

Web: the browser `Db` adapter is an opt-in worker bundle outside the hello world (ADR-048 §7): its worker script is
80,932 bytes (26,172 gzipped) and wa-sqlite's sync wasm 558,343 bytes (272,993 gzipped), **299,165 bytes gzipped**
in all (zlib level 9, the playground's Vite production build). The hello world is unchanged: an app pays this
only when it imports `@undra/runtime/db`.

### Production operations (ADR-046)

Nothing on the hot path moved: reads never cross, a write crosses once, and the panic report is built only when a
panic is contained, so no budgeted row changed. `sync_alloc`, `commit_alloc` and `derived_alloc` still hold their
exact counts, and the budgets test passes at its committed numbers (2026-10-01, the merged tree).

| What | Number | How |
|---|---|---|
| A contained panic answered with status 2, and the `PanicReport` encoded and handed to `Diagnostics` | p50 34.6 µs with the report, 32.5 µs with the port unbound: the report is about 2 µs of it | `cargo test --release -p undra-ports --test diagnostics -- --ignored --nocapture panic_cost` (p50 of 400; an ignored measurement, not a gated row); a `TestRuntime` has no frame source, so `undra-ffi`'s stack walk is not in it |
| Hello-world web core, gzipped (the wasm alone) | 119,055 (`main` with objects-callbacks) to 119,654 (+599); budget 120,000 | `scripts/wasm-size.sh`; the standard surface (one port, three records, `run_background`) is in every schema and the FATAL record carries location and operation. Merged as the piece first landed it measured 121,164 (over the gate); the review made `guarded` `Ok(f())` on wasm and `run_background` idle-synchronous (ADR-052, the prod-ops review note) |
| Hello-world JavaScript runtime up front, gzipped | 21,672 (`main` with objects-callbacks) to 22,005 (+333); budget 22,100 (ADR-052, the prod-ops review note; the review made `runInBackground` and the `Diagnostics` registration lazy) | `scripts/wasm-size.sh`; the report builder (853 bytes), `runInBackground` and the `Diagnostics` port are lazy chunks |
| Shipped artefacts, built with `--no-symbols` (the profile before ADR-046) and with the symbol work, same core | Android `.so` arm64-v8a 2,355,912 to 2,355,848, x86_64 2,515,840 to 2,515,776; iOS app, linked and stripped, byte-identical (2,190,456); host dylib +16; web wasm 879,095 to 875,686 raw (353,111 to 351,605 gzipped) | `cargo test -p undra-cli --test symbols` (`shipped_artefacts_do_not_grow_and_no_symbols_writes_none`, which fails if any grew); the playground core |

## The CI gate

`bench/budgets.toml` holds a host budget for each of the 56 operations the gate runs (the wire round trips,
dispatch, signals, snapshot and the eight per-operation rows of the harsh-conditions scenarios). Each is about **5x** what this machine measures (with a 250 ns floor and two
significant figures), which is what makes a shared CI runner pass while an operation that became several
times slower fails. The budgets guard against **regressions on a host**; they are not the section 14 device
targets, and none of them sits above its device target today. (The keyed-patch rows were one miss until
ADR-027: the insert row's gate is 31 µs, 5x what it measures, and what it measures, 6.2 µs, is under the
20 µs device target. The handle method call was the other until ADR-028: its budget now guards the 43.9 ns
it measures, not the allocator.) The test takes the best p50 of up to three attempts, runs in `--release` only (a debug build
just smoke-runs every operation, so `cargo test --workspace` stays green and fast), and supports
`UNDRA_BENCH_SCALE` for a slower runner. `.github/workflows/bench.yml` runs it on every PR and on main.

Two more steps follow it in the same job. `cargo test -p undra-bench --test stress --release` runs the eight
sustained scenarios of [Harsh conditions](#harsh-conditions) for 2 s each and fails over their
`[stress."..."]` tables in `budgets.toml` (a throughput floor, p99 and p999 ceilings, a change-set bytes ceiling,
RSS growth) or on a broken invariant (nothing lost, nothing reordered, the exact byte count, the host's list equals
the core's, a stream never more than one item ahead); a noisy run gets three attempts, an invariant none. Then
`cargo run -p undra-bench --release --bin soak -- --seconds 10 --attempts 2` runs the mixed paced load and fails
on RSS growth, drift in the firehose's p99, a broken invariant or a host that could not carry the load. The
debug build of the stress test checks invariants only (500 churn rows, 100 ms per scenario), so
`cargo test --workspace` stays green and about 4 s slower.

Before those steps `bench.yml` records a baseline from the base commit on the same VM, and the budgets and stress
steps gate against it as well (see [Gates that catch a 2x regression](#gates-that-catch-a-2x-regression)); the
ratio tables of `budgets.toml` run in every budgets step on every machine.

Every run uploads the JSON behind its numbers (`UNDRA_BENCH_RESULTS_DIR`, see Method) as the `bench-results`
artifact; the soak runs each attempt in a fresh process.

## Device numbers (iOS, Android, Web)

The host rows above are the core's half: a core call on a fast desktop core, not what a Swift, Kotlin or TypeScript app
pays. This section is the platform's half, **measured through the generated binding and the platform's mirror, in the
playground app itself**, by one command per target:

```sh
scripts/bench-device.sh --device ios                   # the iPhone 17 Pro simulator (boots it if needed)
scripts/bench-device.sh --device android               # the one device or emulator adb sees; else boots the `undra` AVD headless
scripts/bench-device.sh --device web                   # headless Chromium (Playwright)
```

Each run builds the playground core (`undra build --release`) and app for the target, drives the app's bench hooks
(`Bench`, ADR-031's drain) with the harness each platform's smoke test already uses (an XCUITest, an instrumented test,
Playwright), writes `bench/results/device/<date>-<target>.json` (schema `undra-device-bench/1`: device model, OS version,
build type, commit, host load, the timer's step, every row's p50/p99/min/max, the drain experiment, the cold-start
launches) and renders this section's tables from those files (`node scripts/bench-device-report.mjs render`; `node --test
scripts/bench-device-report.test.mjs` fails when the tables are not what the files say). **To get a real-device row** (the command that matters
the day hardware is attached):

```sh
# an iPhone (USB, trusted, Developer Mode on; the team id is the one in Xcode > Settings > Accounts)
UNDRA_IOS_TEAM=<team id> scripts/bench-device.sh --device ios --target <udid from `xcrun xctrace list devices`> --runs 3
# an Android phone (USB debugging on; `adb devices` shows the serial)
scripts/bench-device.sh --device android --target <serial> --runs 3
```

A row from a physical device is the only one that gets a verdict against the blueprint's target. `--runs 3` makes three
files and the reproducibility lines compare them. Keep the phone charged, cool, unlocked and out of low-power mode (the
file records the thermal state and the mode).

### How the rows are timed

* **End to end, as the app experiences it.** Every row calls the generated store method (`bench.benchAdd(..)`,
  `benchEchoBytes`, `benchListInsert`, `benchTouchSignals`), on the main thread (the main actor, the main looper, the page's
  thread), and the clock stops when the mirror holds the result: the call returns only after the main-thread drain
  (ADR-031 decision 2), and **the runner compares the mirror's state with what the operations did** (the sum of the
  replies, the length of the list and the id at the insert position, the hundredth counter) after every operation on iOS
  and Android and after every batch on the web, where the checks are cumulative (a batch in which one apply was missing
  leaves the length or the counter short), and aborts the run, reporting no number, when they differ.
* **Release builds.** The core is `undra build --release` (LTO fat); the iOS app is the Release configuration, the Android
  app the `benchmark` build type (release, not debuggable, signed with the debug key) with the code compiled ahead of time
  (`cmd package compile -m speed`), the web page the production build. The file records all of it.
* **The web build target decides what a web row costs, and the rows below are the `es2022` build.** The runtime is
  ES2022. At Vite 6's default target (es2020, which is what the `undra init` template built at before ADR-056) its class
  fields became `WeakMap`/`WeakSet` helper calls, and the same page measured 3.2 to 3.5 us for the handle call, 3.5 to
  3.9 us for `callSync` and 88 to 96 us for the 100-signal change-set (the files of 2026-10-01; the review's rerun at
  `es2022`: 1.8, 1.6, 31 us). The playground page is now built with `build.target: "es2022"` in its `vite.config.ts`, as
  the Undra Vite plugin builds an app that sets no target of its own, and the call path's classes no longer use
  `#private` at all (ADR-056): at es2022 the handle call is 0.44 to 0.68 us, and at Vite 6's default target the same tree
  measured 1.08 us, `callSync` 0.70 us, the 1 KB round trip 2.94 us, the keyed insert 23.0 us, the change-set 47.9 us and
  the merged frame 2.25 ms (one run at host load 28 to 31, before the last two levers), not 5x worse. The cause
  analysis is in `.10x/reviews/2026-10-01-device-bench-review.md`.
* **Clocks.** `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)` (41.67 ns tick), `System.nanoTime` and `performance.now`, which a
  browser rounds to 100 us, or to 5 us when the page is cross-origin isolated (the bench page is). The handle call is
  tens to hundreds of nanoseconds, so it is timed as **batches of calls divided by the batch** (1,000 on a phone) and
  its p50/p99 are over batches; the other rows are timed **one operation at a time** on iOS and Android (2,000 samples
  after a warm-up) and as batches on the web (the batch size is in each file and in the "Timed as" column), where the
  clock cannot resolve one. A pair of clock reads costs 10 to 30 ns on a phone (in the file).
* **The rows are not the host rows.** The 1 KB row echoes a `Bytes` of 1,024 (the playground's `bench_echo_bytes`; a byte
  string is a `memcpy`, a record of the same size is more work), so it is a floor for the blueprint's record. The keyed
  insert is the **whole call** (encode, core insert, patch, apply) on a list of about 10,000 rows (it is put back to
  10,000 outside the timed region), where the host row is the core's part. The 100-signal row is one call that dirties
  100 signals and the apply of that change-set to 100 observable properties. On the web the generated call is
  asynchronous, so there are two handle-call rows: the generated binding, and the runtime's `callSync` (the blueprint's
  "in-thread" row, which generated code does not use).
* **Cold start** is the first `UndraCore.load` of a fresh process plus the restore of a 100 KB snapshot (1,000 to-dos of 80
  characters; the host row restores four stores of 250 rows of 100 bytes), over ten launches (XCUITest launches, `am
  instrument` runs, Playwright contexts); iOS also reloads the core in process thirty times. An in-process core cannot be
  loaded twice on Android, so its in-process figure is the restore alone; the generated TypeScript has no `snapshot()` or
  `restore()` (see item 5 of the first rows' notes below), so the web row is the load after the module is compiled (the blueprint's own wording) and
  says it has no restore.
* **The drain (ADR-031)**: 1,667 one-update keyed patches on the 10,000-row list per frame, 100,000 a second at 60 Hz. The
  core commits them in a burst (`bench_list_update_burst`, one transaction per update, each its own change-set) from a
  thread of its own on iOS and Android, so the main thread meets them only as the drain at the next frame, through the
  real frame source (`CADisplayLink`, the Choreographer); on the web the page's own thread commits them and the whole
  burst is main-thread time. The tables report the main thread's cost of a frame with the runtime as it is (merged), and
  **estimate the unmerged cost, not by reverting the runtime**: the drain of a single entry (a call on the main thread
  drains before it returns, so the drain listener times a drain of exactly one change-set applied on its own; on the web, a
  call that commits one update minus a call that commits none) times 1,667.

<!-- device-bench:begin (generated by scripts/bench-device-report.mjs render; edit nothing between the markers) -->

**Every row below was measured by `scripts/bench-device.sh` and is the file named under its heading** (`bench/results/device/`). The blueprint targets are beside the numbers for reference. **A row from a simulator, an emulator or a desktop browser makes no claim about a device target: the verdict column says so, and only a row from a physical device gets one** (on its p50; the blueprint gives no percentile). A row timed in batches has its p50 and p99 over batch means (a batch's time divided by its size), so its p99 is not the tail of a single call.

#### iPhone 17 Pro simulator (iOS 26.5, arm64 on Apple M5 Pro)

**A simulator, not a device.** `2026-10-01-ios-simulator-iphone-17-pro-run3.json`, 2026-10-01. model iPhone18,1 (simulated), iOS 26.5, arm64, 18 cores, runtime: inproc (C ABI, linked static library). Build (release): core release (LTO fat), static XCFramework slice; app Release (-O, whole-module); UI test launches in benchmark mode; commit `65bf971`. Timer: clock_gettime_nsec_np(CLOCK_UPTIME_RAW), step 41 ns, a pair of reads 7.71 ns. Host: Apple M5 Pro, macOS 26.5 (25F71); load average 7.22 before the run, 7.04 after.

| Row | p50 | p99 | Timed as | Blueprint target | Verdict |
|---|---|---|---|---|---|
| Handle method call, primitive args and return | 295 ns | 437 ns | batches of 1000, 200 samples | ≤ 60 ns | none (not a device) |
| 1 KB record, round trip | 1.04 µs | 4.71 µs | each op, 2000 samples | ≤ 3 µs | none (not a device) |
| Keyed patch on a 10,000-item list, one insert | 8.79 µs | 11.1 µs | each op, 2000 samples | ≤ 20 µs | none (not a device) |
| Change-set with 100 dirty signals, applied on the main thread | 27.9 µs | 35 µs | each op, 2000 samples | ≤ 100 µs | none (not a device) |
| Core cold start with 100 KB snapshot restore | 2.67 ms | 2.86 ms | fresh process, 10 launches | ≤ 3 ms | none (not a device) |

Cold start: fresh launches (10): load p50 2.6 ms, min 2.2 ms, max 2.79 ms; restore p50 63.7 µs; in-process reloads (warm): load p50 337 µs, restore p50 51.1 µs; snapshot 101,046 bytes.

<details><summary>What each row timed</summary>

* Handle method call: `try bench.benchAdd(a:b:)` on the main actor: encode, `callSync` through the C ABI, decode, through the generated binding.
* 1 KB round trip: `try bench.benchEchoBytes(data:)` with 1,024 bytes: the payload crosses the boundary twice (a byte string, which is cheaper than a record of the same size).
* Keyed insert, 10k rows: `bench.benchListInsert(i: 5000)` on about 10,000 rows: the call, the core's recorded insert, the one-operation patch, applied to the mirror's list before the call returns.
* 100-signal change-set: `bench.benchTouchSignals(k: 100)`: one call, one change-set of 100 entries, applied to 100 `@Observable` properties of the mirror before the call returns.
* Cold start: a 100 KB snapshot is 1000 to-dos of 80 characters (the host row restores four stores of 250 rows of 100 bytes); load = `UndraCore.load` of the linked core, which starts the runtime and its core thread; the C library is statically linked, so there is no dlopen.

</details>

#### Android emulator "Google sdk_gphone64_arm64" (Android 15 (API 35), arm64-v8a, hardware-virtualized on Apple M5 Pro)

**An emulator, not a device.** `2026-10-01-android-emulator-arm64-v8a-run2.json`, 2026-10-01. model Google sdk_gphone64_arm64, Android 15 (API 35), arm64-v8a, 4 cores, runtime: inproc (JNI, libundra_core.so). Build (release): core release (LTO fat), libundra_core.so; app benchmark build type (release, not debuggable); ART compile filter speed; instrumented test; commit `65bf971`. Timer: System.nanoTime, step 41 ns, a pair of reads 21.9 ns. Host: Apple M5 Pro, macOS 26.5 (25F71); load average 3.8 before the run, 3.37 after.

| Row | p50 | p99 | Timed as | Blueprint target | Verdict |
|---|---|---|---|---|---|
| Handle method call, primitive args and return | 251 ns | 561 ns | batches of 1000, 200 samples | ≤ 250 ns | none (not a device) |
| 1 KB record, round trip | 1.38 µs | 8.71 µs | each op, 2000 samples | ≤ 8 µs | none (not a device) |
| Keyed patch on a 10,000-item list, one insert | 23.2 µs | 191 µs | each op, 2000 samples | ≤ 40 µs | none (not a device) |
| Change-set with 100 dirty signals, applied on the main thread | 19.6 µs | 98.7 µs | each op, 2000 samples | ≤ 150 µs | none (not a device) |
| Core cold start with 100 KB snapshot restore | 1.35 ms | 1.53 ms | fresh process, 10 launches | ≤ 5 ms | none (not a device) |

Cold start: fresh launches (10): load p50 1.09 ms, min 1 ms, max 1.2 ms; restore p50 250 µs; snapshot 101,046 bytes.

<details><summary>What each row timed</summary>

* Handle method call: `bench.benchAdd(a, b)` on the main thread: encode, `callSync` through JNI, decode, through the generated binding.
* 1 KB round trip: `bench.benchEchoBytes(1,024 bytes)`: the payload crosses the boundary twice (a byte string, which is cheaper than a record of the same size).
* Keyed insert, 10k rows: `bench.benchListInsert(5000)` on about 10,000 rows: the call, the core's recorded insert, the one-operation patch, applied to the mirror's list before the call returns.
* 100-signal change-set: `bench.benchTouchSignals(100)`: one call, one change-set of 100 entries, applied to 100 `StateFlow`s of the mirror before the call returns.
* Cold start: a 100 KB snapshot is 1000 to-dos of 80 characters (the host row restores four stores of 250 rows of 100 bytes); load = the first `UndraCore.load` of the process, which includes `System.loadLibrary`; an in-process core cannot be loaded twice, so the in-process row is restores only, and the restore of a cold launch includes the main-thread drain.

</details>

#### Chromium 153.0.8010.12 headless (Playwright, wasm-main) on Apple M5 Pro

**A desktop browser, not a device.** `2026-10-02-web-chromium-headless-run3.json`, 2026-10-02. model chromium (headless, Playwright), arm64, 18 cores, runtime: wasm-main. Build (release): core release-wasm, wasm-opt -Oz; app vite production build with build.target es2022 (the playground's vite.config.ts, as the Undra Vite plugin sets it for an app that has none; Vite 6's own default, es2020, lowers the runtime's class fields to helpers), cross-origin isolated (5 µs clock); commit `83a5f85`. Timer: performance.now, step 5 µs, a pair of reads 82 ns. Host: Apple M5 Pro, macOS 26.5 (25F71); load average 24.24 before the run, 22.78 after.

| Row | p50 | p99 | Timed as | Blueprint target | Verdict |
|---|---|---|---|---|---|
| Handle method call, primitive args and return | 475 ns | 715 ns | batches of 1000, 100 samples | n/a | none (no reference machine) |
| Handle method call, in-thread: the runtime's `callSync` (web only; generated TypeScript calls are asynchronous) | 322 ns | 342 ns | batches of 5000, 100 samples | ≤ 80 ns (in-thread; at most one frame in Worker mode) | none (no reference machine) |
| 1 KB record, round trip | 1.6 µs | 2.27 µs | batches of 500, 100 samples | ≤ 4 µs | none (no reference machine) |
| Keyed patch on a 10,000-item list, one insert | 16.7 µs | 19.4 µs | batches of 200, 100 samples | ≤ 30 µs | none (no reference machine) |
| Change-set with 100 dirty signals, applied on the main thread | 19.7 µs | 22.8 µs | batches of 100, 100 samples | ≤ 120 µs | none (no reference machine) |
| Core cold start with 100 KB snapshot restore (**load only: no restore, see the notes**) | 5.4 ms | 5.72 ms | fresh process, 10 launches | ≤ 8 ms (after wasm compile) | none (no reference machine) |

Cold start: fresh launches (10): load p50 5.4 ms, min 5.25 ms, max 5.72 ms; wasm compile p50 875 µs (not in the row, as the blueprint says); in-page reloads (warm): load p50 1.06 ms.

<details><summary>What each row timed</summary>

* Handle method call: `await bench.benchAdd(a, b)`: encode, the call, the reply, decode, through the generated asynchronous binding.
* Handle call, `callSync`: `UndraCore.callSync`: the runtime's synchronous entry (no promise), which the generated TypeScript does not use; the blueprint's in-thread row.
* 1 KB round trip: `await bench.benchEchoBytes(1,024 bytes)`: the payload crosses the boundary twice (a byte string, which is cheaper than a record of the same size).
* Keyed insert, 10k rows: `await bench.benchListInsert(5000)` on about 10,000 rows: the call, the core's recorded insert, the one-operation patch, applied to the mirror's list before the call returns.
* 100-signal change-set: `await bench.benchTouchSignals(100)`: one call, one change-set of 100 entries, applied to 100 signals of the mirror before the call returns.

</details>

#### The ADR-031 drain: 1,667 one-update keyed patches on 10,000 rows per frame (100,000 a second at 60 Hz)

| Target | Main-thread cost of a frame, merged (p50 / p99) | Entries received → applied | One entry applied on its own (median / mean) | Unmerged frame, estimated (from the median and the mean entry, lower first) | Merging saves | Share of a 60 Hz frame, merged → unmerged |
|---|---|---|---|---|---|---|
| iPhone 17 Pro simulator (iOS 26.5, arm64 on Apple M5 Pro) | 787 µs / 1.41 ms | 1,667 → 1 (1 drain) | 2.04 µs / 2.37 µs | 3.4 ms to 3.94 ms | 4.3x to 5.0x | 4.7% → 20.4% to 23.7% |
| Android emulator "Google sdk_gphone64_arm64" (Android 15 (API 35), arm64-v8a, hardware-virtualized on Apple M5 Pro) | 167 µs / 285 µs | 1,667 → 1 (1 drain) | 12.5 µs / 13.7 µs | 20.9 ms to 22.9 ms | 124.9x to 136.8x | 1.0% → 125.4% to 137.4% |
| Chromium 153.0.8010.12 headless (Playwright, wasm-main) on Apple M5 Pro | 1.46 ms / 1.81 ms | 1,667 → 1 (1 drain) | 2.75 µs / 2.71 µs | 4.52 ms to 4.58 ms | 3.1x | 8.8% → 27.1% to 27.5% |

* **iPhone 17 Pro simulator (iOS 26.5, arm64 on Apple M5 Pro).** A thread of its own commits the burst (the core's side of a socket or a timer): the main thread's cost of a frame is the drain at the next frame, as `DrainStats.duration` times it (`ContinuousClock`). Merged frame = the drain(s) that consumed one burst, summed: decode and apply of the merged patch to the `@Observable` store. Unmerged estimate: the drain of a single entry (a call on the main thread drains before it returns, so the drain listener times a drain of exactly one change-set of one entry applied on its own), 8 after each frame of the experiment so that both are measured in the same minutes (1920 in all); times 1667, by the median entry and by the mean entry. The runtime was not reverted: this is what applying every entry on its own would cost, from the entry cost measured here.
* **Android emulator "Google sdk_gphone64_arm64" (Android 15 (API 35), arm64-v8a, hardware-virtualized on Apple M5 Pro).** A thread of its own commits the burst (the core's side of a socket or a timer): the main thread's cost of a frame is the drain at the next frame (`ChoreographerFramePacer`), as `DrainStats.duration` times it (`System.nanoTime`). Merged frame = the drain(s) that consumed one burst, summed: decode and apply of the merged patch to the `StateFlow` store (the list is copied once per drain). Unmerged estimate: the drain of a single entry (a call on the main thread drains before it returns, so the drain listener times a drain of exactly one change-set of one entry applied on its own), 8 after each frame of the experiment so that both are measured in the same minutes (1920 in all); times 1667, by the median entry and by the mean entry. The runtime was not reverted: this is what applying every entry on its own would cost, from the entry cost measured here.
* **Chromium 153.0.8010.12 headless (Playwright, wasm-main) on Apple M5 Pro.** The page's own thread: one awaited call per frame commits the burst (wasm-main), so the whole burst is main-thread time. Merged frame = the awaited burst call of 1,667 transactions plus the drain it ends with, as the page experiences it. Unmerged estimate: a call that commits one update (one transaction, one drain of one entry, the list copy) minus a call that commits none, as two batches of 20 after each frame of the experiment so that both are measured in the same minutes (240 batches); times 1667, by the median batch and by the mean batch. The runtime was not reverted: this is what applying every entry on its own would cost, from the entry cost measured here.

#### Reproducibility

* **iPhone 17 Pro simulator (iOS 26.5, arm64 on Apple M5 Pro)**, 3 runs (2026-10-01-ios-simulator-iphone-17-pro.json, 2026-10-01-ios-simulator-iphone-17-pro-run2.json, 2026-10-01-ios-simulator-iphone-17-pro-run3.json): median per row, run by run, with the spread: Handle method call: 302 ns / 294 ns / 295 ns (1.03x); 1 KB round trip: 1.13 µs / 458 ns / 1.04 µs (2.46x); Keyed insert, 10k rows: 8.29 µs / 8 µs / 8.79 µs (1.10x); 100-signal change-set: 26.9 µs / 26.2 µs / 27.9 µs (1.07x); merged frame 815 µs / 820 µs / 787 µs (1.04x).
* **Android emulator "Google sdk_gphone64_arm64" (Android 15 (API 35), arm64-v8a, hardware-virtualized on Apple M5 Pro)**, 2 runs (2026-10-01-android-emulator-arm64-v8a.json, 2026-10-01-android-emulator-arm64-v8a-run2.json): median per row, run by run, with the spread: Handle method call: 266 ns / 251 ns (1.06x); 1 KB round trip: 1.33 µs / 1.38 µs (1.03x); Keyed insert, 10k rows: 23 µs / 23.2 µs (1.01x); 100-signal change-set: 20.3 µs / 19.6 µs (1.04x); merged frame 182 µs / 167 µs (1.09x).
* **Chromium 153.0.8010.12 headless (Playwright, wasm-main) on Apple M5 Pro**, 3 runs (2026-10-02-web-chromium-headless.json, 2026-10-02-web-chromium-headless-run2.json, 2026-10-02-web-chromium-headless-run3.json): median per row, run by run, with the spread: Handle method call: 605 ns / 680 ns / 475 ns (1.43x); Handle call, `callSync`: 428 ns / 442 ns / 322 ns (1.37x); 1 KB round trip: 1.92 µs / 2.02 µs / 1.6 µs (1.26x); Keyed insert, 10k rows: 20.5 µs / 22.4 µs / 16.7 µs (1.34x); 100-signal change-set: 25.4 µs / 27.4 µs / 19.7 µs (1.38x); merged frame 2.08 ms / 2.06 ms / 1.46 ms (1.42x).

#### Pending hardware

| Platform | Reference device | What is needed |
|---|---|---|
| iOS | iPhone with an A15-class chip | an iPhone with an A15-class chip, attached over USB, trusted, in Developer Mode, and `UNDRA_IOS_TEAM` set to a development team id; `scripts/bench-device.sh --device ios --target <id>` |
| Android | mid-range Android phone, 2022 | a 2022 mid-range phone with USB debugging enabled; `scripts/bench-device.sh --device android --target <id>` |

<!-- device-bench:end -->

### What the first device-phase rows say (2026-10-01)

Nothing below is a device. The machine is an Apple M5 Pro shared with other builds (load average 2.3 to 10.6 across the
runs, in each file), so a row moves by 5% to 30% between runs (the reproducibility lines above) and the tails (p99,
max) are the host's scheduler as much as the platform's. At a higher load the whole table moves: the review reran the web
twice at load 19 to 22 and iOS once at load 94 to 48, and every row but the iOS drain came out 1.2x to 2.0x the medians
below (the iOS cold start at 3.18 ms, over its 3 ms reference; the iOS merged frame at 0.61 ms, lower), with the same
shape (the order of the rows, the web drain's merged-to-unmerged ratio). Compare runs only at a similar load, which each
file records. The simulator and the emulator run on the M5 Pro's own cores
(the emulator is arm64, hardware-virtualized, not a translation), which are faster than an A15's and than a 2022
mid-range phone's: **read a row under its target as encouraging, not as met, and a row over its target as a stronger
miss on the device.** Ranges are over the runs in the files (three on iOS and the web, two on Android).

1. **The handle call through the generated binding is not the core's 44 ns.** 294 to 302 ns on iOS, 251 to 266 ns on
   Android, 0.47 to 0.68 us on the web (0.32 to 0.44 us for the runtime's own `callSync`; 0.44 to 0.48 and 0.29 to 0.32 on
   the quietest of the runs, load 10, and 3.2 to 3.5 and 3.5 to 3.9 before E4), against the blueprint's 60, 250
   and 80 ns and the host's core-side 44 ns (`dispatch/call_sync/add`): most of a platform call is the generated binding
   (encode the arguments into a writer, the crossing, the reply, decode), not the core. On iOS it is 5x its target on a
   core faster than an A15's, and on Android it sits on its target on one faster than a 2022 mid-range phone's: this
   is the row to watch on hardware. On the web it is 4 to 6x over for the in-thread row on a desktop browser (it was 44
   to 49x), and the rest of what is above the wasm export's own 130 to 200 ns is two wasm calls, the writer, the reply
   copy and the `await`. The review measured where it went (`.10x/reviews/2026-10-01-device-bench-review.md`, E4: the
   build target, then per-call allocation and the `BigInt` handle) and ADR-056 took it apart lever by lever: the
   `es2022` target (the plugin and the playground), no `#private` on the call path, a writer and reader with no
   `DataView` or 256-byte buffer per call, a call payload in one allocation, a direct call that builds no promise, and a
   transport that copies the header and the arguments apart. The budgets of the rows are tests since then
   (`[web."id"]` in `bench/budgets.toml`, R9).
2. **The other rows have room on this hardware**: 1 KB round trip 0.46 to 1.1 us (iOS), 1.3 to 1.4 us (Android), 1.6 to
   2.0 us (web; 4.6 to 5.3 before E4, the one row that was over its target) against 3, 8 and 4 us; keyed insert 8.0 to 8.8 us, 23 us and
   16.7 to 22.4 us against 20, 40 and 30 us; the 100-signal change-set 26 to 28 us, 19.6 to 20.3 us and 19.7 to 27.4 us
   against 100, 150 and 120 us. The iOS 1 KB row was 458 ns in one of three runs and 1.04 to 1.13 us in the other two,
   with nothing different in the harness (the process landing on another kind of core is the likely cause), which is why a
   file records every run.
3. **Cold start is mostly the platform's load, not the restore**: a fresh iOS process takes 2.2 to 2.7 ms to load the core
   and restore 100 KB (load 2.2 to 2.65 ms, restore 54 to 64 us), Android 1.4 ms (load 1.1 to 1.2 ms, which includes
   `System.loadLibrary`, restore 250 us), the web 4.2 to 4.5 ms after the module is compiled at the load of 2 to 4 of the first files (0.67 to 0.70 ms to
   compile; no restore, below), 5.3 to 5.6 ms at the load of 10 to 26 of the files after E4, with the warm reload in the page
   1.0 to 1.1 ms instead of 0.7 ms and the compile 0.84 to 0.92 ms: the host, not the runtime (a Node loop of
   `UndraCore.load` on the same module takes 950 us before E4 and after), against the host's 70 to 80 us for the core alone (`snapshot/cold_start_restore_100kb`) and the
   targets of 3, 5 and 8 ms. The iOS median is already 0.74 to 0.90 of its target on a core faster than an A15's.
4. **ADR-031's drain, measured on Swift and Kotlin for the first time**: 1,667 one-update keyed patches on 10,000 rows a
   frame arrive as one drain that applies one merged patch. On iOS the main thread spends 0.79 to 0.82 ms a frame
   (5% of a 60 Hz frame) where applying every entry alone would cost an estimated 3.4 to 4.1 ms (20 to 25%), 4.2 to 5.0x
   more; on Android 0.17 to 0.18 ms (1%) against an estimated 20.9 to 24.7 ms, **more than a whole frame**, 122 to 137x
   more, because every Kotlin patch copies the 10,000-row list; on the web the page's own thread pays 1.5 to 2.1 ms (4.0 to 4.3 before E4), of
   which the core's own 1,667 transactions are most (wasm-main runs the core on the main thread), against 4.5 to 4.6 ms
   (11.3 to 12.1 before), 3.1x. The ADR's Node probe leaves the core out and saw 3.7 to 4.4 ms a frame before and 0.33 to 0.45 ms after;
   Chromium's drain alone is 0.16 to 0.23 ms here (0.76 to 0.81 before E4) against that probe's 0.18 to 0.26 ms (the
   probe ran the runtime's own build in Node, not a Vite production bundle).
5. **Gaps the harness ran into**: the generated TypeScript has no `snapshot()` or `restore()` (SPEC 17.1; the API gaps in
   `.10x/decisions/sde/playground.md`), so the web cold-start row is the load alone and says so; an in-process Kotlin core
   cannot be loaded twice, so Android has no in-process reload row, only the restore (the iOS core can be shut down and
   loaded again, thirty times in the file); and the 1 KB row echoes a `Bytes` (the playground's hook), not a 1 KB record,
   so it is a floor for the blueprint's record.
6. **Two harness costs in the committed cold-start rows, removed in review** (both make a row slower, never faster): the
   iOS load included emptying the bench's key-value directory (`FileManager.removeItem`, about 10 us for an absent
   directory: under 1% of a cold load, about 3% of a warm in-process reload), and the Android restore included the
   instrumentation thread's hop to the main looper and back (`runOnMainSync`). The runners now time both inside; the
   files above predate that.

### Still waiting for

| Row | Waiting for |
|---|---|
| Every row above, with a verdict | an iPhone with an A15-class chip, a 2022 mid-range Android phone (the commands are at the top of this section); Chromium needs a named reference machine |
| Hello-world size added to the app | release builds for `aarch64-apple-ios`, the Android ABIs and `wasm32-unknown-unknown` on a hello-world core (the playground's sizes are in `.10x/decisions/sde/playground.md`; the host proxy above is thin against 900 KB) |
| Runtime memory at idle | a device memory profile (Instruments, Android Studio); the host proxy above is an RSS delta, and an exact heap counter needs a custom global allocator, which is `unsafe` and outside `undra-ffi` (R2) |
| Incremental core rebuild in `undra dev`, 20k-line core | a 20k-line core, which the playground does not yet have |
| Web crash recovery, 1 MB | a public `restore()` in the TypeScript runtime; the restore itself is measured above |
| Comparison with UniFFI and KMP baselines | the playground phase; the blueprint publishes these per release |
