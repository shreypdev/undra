//! The leaderboard recipe's contention check (`site/docs/cookbook/leaderboard.html`): a caller that
//! only needs the core lock, while snapshots of 100,000 players are applied one after another.
//!
//! * `the_probe_sees_both_designs_run` is a smoke test: both designs apply snapshots and the probe
//!   gets its calls through, for a tenth of a second. It asserts no speed, only that the scenario
//!   works (it runs under `cargo test --workspace` in a debug build, on 2,000 players).
//! * `stall_report` prints what the probe waited, per design, for `UNDRA_STRESS_SECONDS` (default 5):
//!   `cargo test -p undra-bench --test leaderboard --release -- --ignored --nocapture stall_report`.
//!   Those are the numbers on the recipe's page and in `bench/RESULTS.md` (finding 9). The gates
//!   on the lock hold itself are the `leaderboard/*` rows of `budgets.toml`.

use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;

use common::leaderboard::{PLAYERS, Stall, stall};

#[test]
fn the_probe_sees_both_designs_run() {
    for naive in [false, true] {
        let seen = stall(naive, Duration::from_millis(100));
        assert!(seen.snapshots > 0, "naive={naive}: no snapshot was applied");
        assert!(seen.probes > 0, "naive={naive}: the probe never got in");
        assert!(seen.max_ns >= seen.p50_ns);
    }
}

fn human(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1} us", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

fn print(name: &str, s: &Stall) {
    eprintln!(
        "{name:<34} {:>6} snapshots {:>8} probes   wait p50 {:>9}  p99 {:>9}  p99.9 {:>9}  max {:>9}",
        s.snapshots,
        s.probes,
        human(s.p50_ns),
        human(s.p99_ns),
        human(s.p999_ns),
        human(s.max_ns),
    );
}

#[test]
#[ignore = "a measurement, not a test: run it with --ignored --nocapture"]
fn stall_report() {
    let seconds: u64 = std::env::var("UNDRA_STRESS_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    eprintln!(
        "{PLAYERS} players, a snapshot every ~9 ms (4 ms of work, then 5 ms of sleep), {seconds} s per design"
    );
    let run = Duration::from_secs(seconds);
    print("recommended (pool, then publish)", &stall(false, run));
    print("snapshot applied inside a call", &stall(true, run));
}
