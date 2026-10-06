# Review: leaderboard-recipe (the live leaderboard cookbook recipe), PR #35

Adversarial review of `wt/leaderboard-recipe` at `485eda8` (+3,399 -131, 97 files), 2026-10-05; the fixes land on
`01e98d8`, `847e661`, `a7f49c1` and `12a2540`. Everything below was run on this Mac (Apple M5 Pro, Xcode, JDK 17,
Node 24, rustc 1.99.0, load average 3 to 21 from other agents' builds) unless it says otherwise; the implementer's
record was not taken as evidence. The Gate's first run on `485eda8` (37402498681) is read for the runner's numbers.

## Verdict

**CLEAN after the fixes below, which are on the branch.** The design holds and is measured honestly: the parse and
sort of a 1.4 MB snapshot run on the blocking pool behind the app's own lock, the core holds its lock only for a
780-byte publish, and the budgets test holds both the absolute rows and a machine-independent ratio. The four Medium
findings (a stale window that could be published last, a page claim no test made, a Bench run with no baseline gate,
a timing-dependent smoke test) are fixed with a regression test, a repro, or a CI log that failed first. Nothing in
the piece names an outside party or a market or money domain (players, scores, ranks only).

## Findings

| # | Sev | Finding | Status |
|---|---|---|---|
| M1 | Medium | **An older window could be published last.** `Board::load`/`apply`/`follow` read the window under the app's lock on the pool and publish it when the awaiting task resumes on the core; nothing orders the publishes. Two updates whose tasks resume in the other order than they took the lock leave the older window on screen until the next update. Repro (scratch test in the module, then the regression test): eight `apply_deltas(+1)` for one followed player awaited together (polled in index order, as any `join` does) on `TestRuntime`, whose pool is real: **1,811 of 2,000 trials** ended showing a score below the structure's. With eight separately spawned tasks the window is the gap between a worker releasing the lock and waking its task (0 of 2,000 here): THEORY on a real runtime, `leaderboard.rs` `Board::apply` then `publish`. | Fixed (`01e98d8`): `Inner::window` bumps a version under the lock; `publish` skips a window older than the newest written (`AtomicU64::fetch_max`). Test `updates_that_resume_out_of_order_never_leave_an_older_window_on_screen` fails without the check (round 0, three runs) and passed 30 of 30 with it |
| M2 | Medium | **The page claimed a test the recipe does not have.** "What the tests prove" said the platform receives "one change-set of 771 bytes, decoded from the wire"; the recipe's test asserted `< 3_000` raw bytes and decoded nothing, and the recipe's store sends 663 bytes (no player followed) or 780 (followed). 771 was the bench fixture's window, whose `Tally` (`my_rank: u32`) differs from the recipe's `Summary` (`me: Option<Row>`): the "This recipe" row of the page's table was not the recipe's store. | Fixed (`01e98d8`, `847e661`, `12a2540`): the recipe's test follows a player, asserts exactly 780 bytes, decodes three entries of the store and the top's 50 and the 7 rows around; the fixture mirrors `Summary` and the version check (780, asserted at build); the page, `RESULTS.md` and the record say 780. Re-measured: best p50 375 ns, best p99 459 ns, unchanged within the 42 ns tick of this host's clock, so the recorded numbers stand |
| M3 | Medium | **This PR's Bench ran with no baseline gate for any row.** `bench/common/leaderboard.rs` compiles `examples/cookbook/core/src/standings.rs` by `#[path]`, and `scripts/bench-record-base.sh` copies only `bench/` into the base's worktree. Gate run 37402498681: `error: couldn't find file .../examples/cookbook/core/src/standings.rs`, then "the base HEAD^1 could not be measured with this harness ... gated by the absolute budgets and the ratios only". Every later PR that changes `standings.rs`'s API would lose the baseline the same way. | Fixed (`a7f49c1`): the script copies the harness's by-path file from the tree under test; `cargo check -p undra-bench --tests` on a worktree of `origin/main` prepared as the script does builds |
| M4 | Medium | **A smoke test that depends on scheduling.** `the_probe_sees_both_designs_run` (debug, under `cargo test --workspace`) asserted `snapshots > 0` after a 100 ms probe loop; the applying thread only checks `done` after it is scheduled, so a runner that does not schedule it within 100 ms fails the test with nothing wrong (AGENT_WORKFLOW: wait for the exact condition you assert). | Fixed (`847e661`): the probe runs until `run` is over **and** one snapshot was applied |
| L1 | Low | The `baseline` helper printed "p99_ns, 5x this" where the file's header (and the rows) use 10x. | Fixed (`847e661`) |
| L2 | Low | `a_bad_snapshot_is_a_typed_error_and_leaves_the_ranking_alone` checked the signals only, not the structure. | Fixed (`01e98d8`): an empty batch re-reads the window from the structure after the refused snapshot and it is the same |
| L3 | Low | `RESULTS.md` finding 9 said the probe's p99.9 and max "are the 4 ms the lock was held"; the max is 7.04 ms. | Fixed (`12a2540`): the max adds the probe's own descheduling |
| L4 | Low | The demo generates each batch of deltas on the core (`run_demo`, `synthetic_deltas` after an await): O(per_tick), 100 deltas at the default, so microseconds; at a million a second it would be 100,000 a tick on the core. And `running` is not reset when a demo task ends by itself (only when the runtime is closing). | Open, noted: a demo feed, and the page says a real app calls `apply_deltas` from its reader |

Checked and found sound:

* **R12.** `standings.rs` and `leaderboard.rs` read no clock, no randomness and start no thread. The SplitMix generator is
  a pure function of its seed; the demo's seed is 8 bytes of the `Rng` port, its ticks are `WeakCtx::sleep` on the `Timer`
  port (the test moves them with the fake clock and gets the same window twice), and every heavy step (snapshot, merge,
  window, synthetic snapshot) is in `spawn_blocking`. The bench's `std::thread`/`Instant` are the harness's, not the core's.
* **Ranks and windows.** Competition ranks by `partition_point` on score (ties share, the next skips: 1, 1, 3, 4, 4);
  the "around" window clips at the first and last player (tested at both ends and in the middle, with ties); a player
  not in the ranking and an empty ranking give no window and zeros; `view`'s position search uses the same total order
  as the ranking. The merge in `apply` removes each touched player once at its pre-batch entry (stable sort plus
  `dedup_by_key` keeps the oldest) and re-inserts the post-batch entries in one pass; the 40-batch test against a full
  re-sort covers players touched several times, saturation at both ends and unknown ids.
* **Errors.** A snapshot that fails to parse, repeats an id or exceeds the maximum fails before the lock is taken, so the
  swap never happens (now tested through the structure, L2); a fetch failure is `LeaderboardError::Net`.
* **The exact byte counts** (780, and `PLAYERS * 12 + 33` for the roster) belong to the recipe's own store and the
  fixture's: they move only with the change-set wire format, which is a major version and an ADR (R7), and the
  arithmetic is in the assertion's message.
* **The harness change.** `p99_ns`/`measured_p99_ns` parse like the other numbers (positive, a syntax error otherwise,
  unit test); the consistency test checks ceiling >= measured and no `measured_p99_ns` without a gate, the same level
  of check `measured_ns` gets; `UNDRA_BENCH_SCALE` multiplies the ceiling; the ratio and `baseline` paths pass `None`;
  no existing row has a `p99_ns`, so `best_of` behaves as before for them (its early stop needs `p99_limit` only when
  there is one). The results JSON gains a `p99_ns` member; nothing in the repo parses those files by schema.
* **`stall_report`** is `#[ignore]` and asserts nothing; the smoke test asserts no speed.
* **The page against the bench.** Every number on the page is one the budgets test or `stall_report` prints (the ratio
  0.015% is 375 / 2,563,917; 6,800x and 6,400x are 2.56 ms / 375 ns and 2.94 ms / 458 ns; 15% is 2.94 / 2.56; 1.6 MB is
  two 8-byte entries per player); the wasm note is right (`Ctx::spawn_blocking` runs inline on wasm and the pool has no
  threads there); E0065 is the off-core write refusal (SPEC §12); SPEC §5.1 is the threading model. The
  `HIGH_FREQUENCY.md` sentence matches the rows.
* **Snippets.** SwiftUI (Swift 6 language mode, strict concurrency), Kotlin (`-Werror`) and TSX (strict) compile with
  `snippets/check.sh`; they observe the three signals and call `follow` / `loadSnapshot` as a platform engineer would.

## The budgets against CI

The repo's rules: an absolute budget is 5x the reference host's best p50 (CI doubles it with `UNDRA_BENCH_SCALE=2`
because runner classes measured 3.5x to 6.2x slower), a tail ceiling 10x its measurement, and the regression gate is
the same-job baseline at 1.5x; a row gets `baseline_tolerance` only once it proves noisy against its base on the
runner, and a gate is never loosened to make a row pass.

What the runner did on the branch's first Gate run (37402498681, `ubuntu-latest`), against what the rows allow there:

| Row | Runner p50 | CI budget (scale 2) | Margin | Runner p99 | CI ceiling | Margin |
|---|---|---|---|---|---|---|
| `lock_hold/recommended` | 842 ns | 3.8 us | 4.5x | 882 ns | 9.2 us | 10.4x |
| `lock_hold/in_core_call` | 6.11 ms | 26 ms | 4.3x | | | |
| `lock_hold/signal_of_rows` | 7.25 ms | 30 ms | 4.1x | | | |
| `ingest/off_core` | 6.13 ms | 26 ms | 4.2x | | | |
| ratio `leaderboard_publish_vs_in_core_apply` | 0.00014 | 0.05 | 363x | | | |

The same run put `signals/changeset_100/cell` at 3.6x and `stress/firehose/call_set` at 3.4x of their budgets, so the
new rows have more room than rows that already pass every run. That runner was of the faster class
(`changeset_100/cell` 1.98 us; RESULTS.md has the pool at 1.56 to 4.44 us for that row): on the slowest runner seen, the
new rows would sit near 2x of their budgets, against 1.6x for `changeset_100/cell`. The p99 is the tightest of the
tails on Linux (882 ns against a p50 of 842: the runner's clock has nanosecond resolution, where this Mac's ticks at
41.67 ns), so the 10x ceiling is not close.

**Decision: no change to the budgets, the p99 ceiling or the scale, and no `baseline_tolerance` yet.** The absolute
budgets already pass on the runner with the margin the file's rule intends, and widening them would only blunt the
outer guard. Whether a row needs a baseline tolerance is an empirical question the header answers per row once it
has been measured against a base on the runner, and these rows had no base until M3's fix: the Gate run on the fixed head
(37403599656, Budgets job 112078567425, an AMD EPYC 9V74 runner) recorded the base on the same VM, and since this
piece changes no core crate, that was an A/A run for these rows: `recommended` 771 ns at **0.97x** its base (p99 861 ns
of 9.2 us), `in_core_call` 6.71 ms at **1.01x**, `signal_of_rows` 7.75 ms at **1.01x**, `off_core` 6.67 ms at **1.01x**,
margins 3.9x to 4.9x to the absolute budgets, while the widest row of the whole file in that run read 1.29x. A row
1% from its base on the same VM has nothing to gain from a tolerance above the 1.5x default; if a later run shows one of
them noisy against its base, its own table gets `baseline_tolerance`, as the header says.

## What was run

* `cargo test -p cookbook` (55 tests and the doctest) and `--features realtime` (64); `cargo fmt --check`; `cargo clippy -p
  cookbook -p undra-bench --all-targets -- -D warnings` (and with `realtime`); `RUSTDOCFLAGS=-D warnings cargo doc -p
  cookbook -p undra-bench --no-deps`; `cargo check -p cookbook` for `wasm32-unknown-unknown` and `aarch64-apple-ios`.
* `cargo run -p undra-cli -- bindgen -C examples/cookbook --check --docs`; `bash examples/cookbook/snippets/check.sh`
  (swift, kotlin, ts).
* `cargo test -p undra-bench` (debug: every operation built and smoke-run, the 780-byte assertions included) and
  `cargo test -p undra-bench --test budgets --release` (the whole file: every row, ratio and the new p99 ceiling pass;
  the leaderboard rows five more times with `UNDRA_BENCH_FILTER=leaderboard`).
* `node site/scripts/build-all.mjs` (idempotent after the page edits), `node site/scripts/check-links.mjs` (55 pages),
  `bash scripts/check-no-em-dash.sh`.

## Not verified

* `scripts/ci-local.sh` (the machine was shared with other agents' builds, load 3 to 21); the Gate on the fixed head is
  the check that counts and is the author's/integrator's to see green before merge.
* `bench-record-base.sh` end to end locally (a full release build of the base); its build step was checked with
  `cargo check`, and the Gate run on the fixed head ran it end to end ("baseline from 58e373a6a324 written").
* The rest of that Gate run (CI, Two cores, Site) had not finished when this review was written.
* The recipe on a device, and what the platforms pay for the second wrong design's 1.2 MB change-set (the page says no
  host row shows it).
