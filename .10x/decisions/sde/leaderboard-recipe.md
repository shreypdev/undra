# SDE: the live leaderboard recipe (wt/leaderboard-recipe, 2026-10-05)

A cookbook recipe for high-frequency data with a large structure: a ranking of 100,000 players fed by a snapshot (1,377,798
bytes of JSON) and a stream of score deltas. No wire, ABI, schema-format, runtime-model or generated-shape change, so no ADR: it
adds one module to the cookbook core (new records, one store, one error) and a measurement. The lesson it teaches, and measures:
a big structure belongs behind the app's own lock on `spawn_blocking`, and the core publishes a small derived window in one
transaction, so the core lock is held for the publish and not for the parse.

## Design

| Piece | What it is |
|---|---|
| `examples/cookbook/core/src/standings.rs` | Plain Rust, no Undra types: every player twice (sorted by id, and in rank order). `from_snapshot` parses and sorts and takes no lock; `apply` changes the scores in place, takes the touched players out of the ranking and merges them back in one pass; `view` is `O(top + around + log n)`; `ranking` is the whole list (the wrong design's input, and the tests' oracle). Competition ranks (ties share one). A seeded SplitMix generator makes the synthetic snapshot and deltas (a function of its seed: R12). |
| `examples/cookbook/core/src/leaderboard.rs` | The store: `top`, `around`, `summary` signals and a handle on `Mutex<Inner>` (the structure, and who is followed). `load_snapshot(path)` fetches over `Http`; `apply_deltas`; `follow`; `start_demo` / `stop_demo` run a seeded feed on the `Timer` and `Rng` ports. Every mutation goes `spawn_blocking(parse, swap under the lock, view)` then `publish` on the core: three signals in one `txn`, unchanged ones skipped. Errors are typed (`LeaderboardError`: `Net`, `BadSnapshot`, `Closed`); a bad snapshot leaves the ranking as it was. A restored store clears its window (the players are not in a snapshot). |
| `standings_tests.rs` | The structure's tests live in their own file because the benchmark harness compiles `standings.rs` by path and must not compile them. |
| Snippets | SwiftUI, Compose (StateFlow) and React lines under `snippets/{swiftui,kotlin,ts}`, regions `leaderboard-*`; `check.sh` compiles all three. Bindings regenerated (`undra bindgen -C examples/cookbook --docs`): a new schema hash, so every generated file's header changed. |
| `bench/common/leaderboard.rs` | The rows, with the recipe's `standings.rs` compiled in by path (one source of truth), two fixture stores (`Standing`: the recommended publish; `Roster`: a signal of every player), and the `stall` probe. |
| `site/docs/cookbook/leaderboard.html` | The recipe page (problem, the two wrong designs, the recipe, the numbers, what to watch); in the cookbook index, the docs nav (`docs.json`), the search index and `llms*.txt`. `docs/HIGH_FREQUENCY.md` has one sentence and a link. |

## What was measured

A row times one `Runtime::call_sync`: a call holds the core lock for as long as it runs, so the call's time is the lock hold of one
snapshot. The host copies every change-set under the lock (`CopyingHost`). Best of three runs, Apple M5 Pro, rustc 1.99.0, load
average 13 (other agents were building):

| Row | p50 | p99 | Change-set |
|---|---|---|---|
| `leaderboard/lock_hold/recommended` (the publish) | 375 ns | 458 ns | 771 bytes |
| `leaderboard/lock_hold/in_core_call` (parse, sort, swap and publish inside the call) | 2.56 ms | 2.94 ms | 771 bytes |
| `leaderboard/lock_hold/signal_of_rows` (inside the call, a `Signal<Vec<Place>>` of every player) | 2.94 ms | 3.26 ms | 1,200,033 bytes |
| `leaderboard/ingest/off_core` (the pool's work, no core lock) | 2.56 ms | 2.98 ms | none |

About 6,800 times shorter at p50 and 6,400 times at p99. A second probe (`cargo test -p undra-bench --test leaderboard --release --
--ignored --nocapture stall_report`: a thread calling a no-op method in a loop while a snapshot is applied every ~9 ms), best of
four 8 s runs: with the recipe the worst call waited 132 us (p99.9 7.8 us); with the snapshot applied inside a call the p99.9 was
3.47 ms and the max 7.04 ms. An open-loop probe was tried first and dropped: on a machine at load 20 to 60 the probe thread's own
descheduling swamped the result (22 ms p99 for the recommended design, at load 27), so the committed probe is closed-loop and says so.

## Bench machinery changed

* `Stats` has `p99_ns`; the budgets test prints it for a row that gates it, writes it into the results JSON, and the `baseline`
  helper prints it as a comment.
* A `[bench]` table may carry `p99_ns` (an absolute ceiling) and `measured_p99_ns`; the budgets test judges the best p99 of the
  attempts, and `UNDRA_BENCH_SCALE` multiplies it (parser, a unit test, the self-consistency check). Only
  `leaderboard/lock_hold/recommended` has one (4,600 ns: 10x the measured p99, the rule of the sustained scenarios' p999; header of
  `budgets.toml` says so).
* Budgets: p50 at 5x the best measured (1.9 us, 13 ms, 15 ms, 13 ms) and the ratio `leaderboard_publish_vs_in_core_apply`
  (recommended over in-core, max 0.05, measured 0.00015): the machine's speed cancels, and a publish that grew with the players
  would put it near 1. No test depends on machine speed beyond the existing gate; the probe's test only checks that both designs
  apply snapshots and the probe gets calls through (debug, 2,000 players, no timing assertion); the report is `--ignored`.
* The harness registers 1.8 KB more canonical schema (42,689 bytes now), about 2.7 us on `Runtime::new` at finding 8's price; the
  `apple-m5-pro` baseline is not re-recorded (the cold-start rows sat at 1.18x of it).
* `bench/benches/leaderboard.rs` is the criterion bench; `bench/RESULTS.md` finding 9 has the numbers and the method.

## How it was verified

* `cargo test -p cookbook` (54 tests and one doctest; 63 tests with `--features realtime`), `cargo clippy` on `cookbook` and
  `undra-bench` with `-D warnings`, `cargo doc` with `-D warnings`, `cargo fmt --check`, `cargo check -p cookbook` for wasm32 and
  iOS (the CI wasm32 loop builds `cookbook`).
* `undra bindgen -C examples/cookbook --check --docs`; `bash examples/cookbook/snippets/check.sh` (swift, kotlin, ts).
* `cargo test -p undra-bench` (debug: every operation smoke-runs and every build-time assertion holds on 2,000 players) and
  `cargo test -p undra-bench --test budgets --release` (the whole file, the new rows and the ratio inside their gates).
* `node site/scripts/build-all.mjs` (idempotent) and `node site/scripts/check-links.mjs` (55 pages OK);
  `scripts/check-no-em-dash.sh`.
* Not run here: `scripts/ci-local.sh` (the machine was at load 20 to 60 from other builds), a Gate run on a pushed head, and the
  recipe on a device.

## Left for later

* The recipe's `start_demo` is a demo feed; a real app would call `apply_deltas` from its socket reader (the `realtime` recipe).
* No device numbers: the phone's cost of the 1.2 MB change-set the second wrong design sends is the platform's, and unmeasured.
