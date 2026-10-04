//! The harsh-conditions gate (constitution R9): each sustained scenario runs for a fixed wall
//! time with its producers, consumers and threads, and is checked against its
//! `[stress."name"]` table in `bench/budgets.toml` and against the invariants it asserts about
//! itself (nothing lost, nothing reordered, the host's list equals the core's list, a stream
//! never more than one item ahead of its credit).
//!
//! This is what CI runs next to the budgets test:
//! `cargo test -p undra-bench --test stress --release -- --nocapture`.
//!
//! * The numbers are the **core side** on a host; the gates are regression guards (throughput
//!   floors at a fifth of what an Apple-silicon laptop measures, tail ceilings at 5x and 10x),
//!   not device targets. `bench/RESULTS.md`, "Harsh conditions", says which is which.
//! * Without optimisations the numbers mean nothing, so a debug build (plain `cargo test`) runs
//!   every scenario for 100 ms and at least 100 operations (a slow machine takes longer, it does
//!   not do less) and asserts **invariants only**: the scenarios stay working under
//!   `cargo test --workspace`, and the timing gates are left to `--release`.
//! * A noisy run gets three attempts: a scenario passes if any attempt meets every gate. An
//!   invariant that breaks is never retried; it is not noise.
//! * `UNDRA_BENCH_BASELINE=<name or path>` adds a gate against what one machine class measured
//!   (`bench/baselines/<name>.toml`, or a file recorded earlier in the same CI job): throughput
//!   under 1/1.5 of the baseline's, or a p99 over 2.5x it, fails. `UNDRA_BENCH_RECORD=path` runs
//!   every scenario all three times and records the best of each metric as a baseline.
//! * A baseline recorded minutes earlier on the same machine (CI's, `scripts/bench-record-base.sh`)
//!   names the base's stress binary (`stress_binary`). A scenario that misses **only** that baseline
//!   is then decided by rounds of base and head in turn (base, head, base, head, base, head), the
//!   best of each side compared with the same factors: a slow stretch of a shared runner lowers the
//!   rounds it covers on both sides, and cannot fail a scenario whose code did not change; a real
//!   regression is slower in every round. `UNDRA_BENCH_SCENARIO=name` runs one scenario by its exact
//!   name and `UNDRA_STRESS_ATTEMPTS=n` sets the attempts (what a round asks of the base's binary).
//! * Every scenario runs for a **warm-up** first, results discarded (thread start-up, cold caches,
//!   the first allocations): a tenth of the measured run, at most 200 ms, then the measured run
//!   of `UNDRA_STRESS_SECONDS`. `UNDRA_STRESS_WARMUP_MS=0` measures from the first operation,
//!   `=500` warms up for half a second. Each result says how long its warm-up was.
//! * `UNDRA_BENCH_RESULTS_DIR=dir` writes the JSON behind every number (one file per scenario,
//!   with the command, the machine and the load) for `bench/results/`; see `undra_bench::results`.
//! * Knobs: `UNDRA_STRESS_SECONDS=10` sets the wall time per scenario (default 2; the numbers in
//!   `RESULTS.md` use 10), `UNDRA_BENCH_SCALE=2.5` divides every floor and multiplies every
//!   ceiling (never bytes, RSS, invariants or a baseline), `UNDRA_BENCH_FILTER=churn` runs only
//!   matching scenarios, `UNDRA_BENCH_BUDGETS=path` reads another file, `UNDRA_STRESS_JSON=path`
//!   also writes one JSON row per scenario for the site.
//! * `cargo test -p undra-bench --test stress --release -- --ignored --nocapture stress_baseline`
//!   prints fresh measurements as `[stress."name"]` tables, for setting or re-basing a gate.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use undra::wire::payload::ChangeSetBuilder;
use undra::wire::{Handle, Writer};
use undra_bench::baseline::{Baseline, Selected, StressBaseline};
use undra_bench::budget::{Budgets, StressBudget, StressObserved};
use undra_bench::hostinfo;

#[path = "../common/mod.rs"]
mod common;

use common::host::OrderChecker;
use common::stress::{BYTES_EXACT, Fault, Scenario, StressConfig, StressReport, scenarios};

/// Timing tests must not overlap: a second test thread would be noise in the first.
static SERIAL: Mutex<()> = Mutex::new(());

/// Attempts a scenario gets when it misses a gate (`UNDRA_STRESS_ATTEMPTS` overrides it: the interleaved
/// rounds run the base's binary with 1, one measurement per round).
const ATTEMPTS: usize = 3;

/// Rounds of base and head, in turn, that decide a scenario that missed only the baseline (see
/// `interleave`): three of each, the best of each side compared.
const ROUNDS: usize = 3;

fn attempts() -> usize {
    match std::env::var("UNDRA_STRESS_ATTEMPTS") {
        Ok(text) => match text.parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => panic!("UNDRA_STRESS_ATTEMPTS must be a whole number, at least 1, not `{text}`"),
        },
        Err(_) => ATTEMPTS,
    }
}

/// Operations a debug smoke run does at least, whatever the clock says (`StressConfig::min_ops`):
/// a machine that is slow or busy takes longer; it does not run fewer and report "nothing ran".
const SMOKE_OPS: u64 = 100;

/// Operations a run that injects a fault does at least. The mirror drops its 101st patch; a round
/// of ten operations puts at least four patches on every mirror (the cycle's four updates each move
/// a row in or out of the derived view), so 30 rounds make more than 101 patches wherever they
/// are counted, and 300 completions give the "main thread" a frame with two change-sets in it.
const FAULT_OPS: u64 = 300;

/// The report of a run that was asked for `cfg.min_ops` operations says it did them: with a floor a
/// slow machine takes longer, so too few means the run could not make progress for `GIVE_UP`.
fn assert_ran(report: &StressReport, cfg: &StressConfig) {
    assert!(
        report.ops >= cfg.min_ops,
        "{}: the machine did not run {} operations in {:?} (it ran {}); a run waits for its floor, \
         so it made no progress",
        report.name,
        cfg.min_ops,
        report.elapsed,
        report.ops
    );
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn budgets_path() -> PathBuf {
    match std::env::var_os("UNDRA_BENCH_BUDGETS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("budgets.toml"),
    }
}

fn load_budgets() -> Budgets {
    match Budgets::load(&budgets_path()) {
        Ok(budgets) => budgets,
        Err(e) => panic!("{e}"),
    }
}

fn scale() -> f64 {
    match std::env::var("UNDRA_BENCH_SCALE") {
        Ok(text) => match text.parse::<f64>() {
            Ok(scale) if scale.is_finite() && scale > 0.0 => scale,
            _ => panic!("UNDRA_BENCH_SCALE must be a positive number, not `{text}`"),
        },
        Err(_) => 1.0,
    }
}

/// The wall time of each scenario in release mode.
fn seconds() -> f64 {
    match std::env::var("UNDRA_STRESS_SECONDS") {
        Ok(text) => match text.parse::<f64>() {
            Ok(s) if s.is_finite() && s >= 0.1 => s,
            _ => panic!(
                "UNDRA_STRESS_SECONDS must be a number of seconds, at least 0.1, not `{text}`"
            ),
        },
        Err(_) => 2.0,
    }
}

/// `UNDRA_STRESS_WARMUP_MS`: the warm-up of every scenario in milliseconds (`None`: the default,
/// a tenth of the run, at most 200 ms).
fn warmup_override() -> Option<Duration> {
    let text = std::env::var("UNDRA_STRESS_WARMUP_MS").ok()?;
    match text.parse::<u64>() {
        Ok(ms) => Some(Duration::from_millis(ms)),
        Err(_) => panic!(
            "UNDRA_STRESS_WARMUP_MS must be a whole number of milliseconds (0 for none), not `{text}`"
        ),
    }
}

fn selected() -> Vec<(&'static str, Scenario)> {
    let all = scenarios();
    // One scenario by its exact name (what an interleaved round asks of the base's binary): a filter
    // matches substrings, and `churn_10k/sustained` is in two names.
    if let Ok(exact) = std::env::var("UNDRA_BENCH_SCENARIO") {
        return all.into_iter().filter(|(name, _)| *name == exact).collect();
    }
    match std::env::var("UNDRA_BENCH_FILTER") {
        Ok(filter) if !filter.is_empty() => all
            .into_iter()
            .filter(|(name, _)| name.contains(&filter))
            .collect(),
        _ => all,
    }
}

fn is_release() -> bool {
    !cfg!(debug_assertions) || std::env::var_os("UNDRA_BENCH_FORCE").is_some()
}

fn observed(report: &StressReport) -> StressObserved {
    StressObserved {
        per_sec: report.per_sec(),
        p99_ns: report.percentile(0.99).map(|n| n as f64),
        p999_ns: report.percentile(0.999).map(|n| n as f64),
        bytes_per_op: Some(report.bytes_per_op()),
        rss: report.rss,
    }
}

fn human_ns(ns: Option<u64>) -> String {
    match ns {
        None => "-".to_owned(),
        Some(ns) if ns >= 1_000_000 => format!("{:.2} ms", ns as f64 / 1e6),
        Some(ns) if ns >= 1_000 => format!("{:.2} us", ns as f64 / 1e3),
        Some(ns) => format!("{ns} ns"),
    }
}

fn human_rate(per_sec: f64) -> String {
    if per_sec >= 1e6 {
        format!("{:.2} M/s", per_sec / 1e6)
    } else if per_sec >= 1e3 {
        format!("{:.1} k/s", per_sec / 1e3)
    } else {
        format!("{per_sec:.0} /s")
    }
}

fn header() {
    eprintln!(
        "{:<28} {:>11} {:>10} {:>10} {:>10} {:>9} {:>8}  verdict",
        "scenario", "per sec", "p50", "p99", "p999", "bytes/op", "RSS"
    );
}

fn row(report: &StressReport, verdict: &str) {
    let rss = report
        .rss
        .map_or_else(|| "-".to_owned(), |g| format!("{:+.2}%", g.growth_pct));
    eprintln!(
        "{:<28} {:>11} {:>10} {:>10} {:>10} {:>9.1} {:>8}  {verdict}",
        report.name,
        human_rate(report.per_sec()),
        human_ns(report.percentile(0.5)),
        human_ns(report.percentile(0.99)),
        human_ns(report.percentile(0.999)),
        report.bytes_per_op(),
        rss,
    );
    for note in &report.notes {
        eprintln!("    {note}");
    }
}

#[test]
fn stress_table_covers_every_scenario() {
    let _serial = serial();
    let budgets = load_budgets();
    let mut expected: BTreeSet<String> = scenarios().iter().map(|(n, _)| (*n).to_owned()).collect();
    // The soak binary reads its RSS gate from here.
    expected.insert("soak/mixed".to_owned());
    let budgeted: BTreeSet<String> = budgets.stress.keys().cloned().collect();
    let unbudgeted: Vec<_> = expected.difference(&budgeted).collect();
    let stale: Vec<_> = budgeted.difference(&expected).collect();
    assert!(
        unbudgeted.is_empty(),
        "these scenarios have no [stress.\"...\"] table in budgets.toml (run `stress_baseline` and add them): {unbudgeted:?}"
    );
    assert!(
        stale.is_empty(),
        "budgets.toml has stress tables for scenarios that do not exist: {stale:?}"
    );
    for (name, table) in &budgets.stress {
        let problems = table.self_check();
        assert!(problems.is_empty(), "{name}: {problems:?}");
    }
}

#[test]
fn stress() {
    let _serial = serial();
    let scenarios = selected();
    assert!(!scenarios.is_empty(), "UNDRA_BENCH_FILTER matched nothing");

    if !is_release() {
        // Timings from an unoptimised build say nothing; prove the scenarios still work and
        // that every invariant holds.
        let cfg = StressConfig {
            duration: Duration::from_millis(100),
            rss: false,
            fault: Fault::None,
            warmup: None,
            min_ops: SMOKE_OPS,
        };
        for (name, run) in &scenarios {
            let report = run(&cfg);
            assert_eq!(report.name, *name);
            assert_ran(&report, &cfg);
            let broken = report.broken();
            assert!(
                broken.is_empty(),
                "{name}: {}",
                broken
                    .iter()
                    .map(|i| i.what.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        eprintln!(
            "stress: {} scenarios smoke-run, invariants only; timing is asserted only with \
             --release (UNDRA_BENCH_FORCE=1 to time a debug build anyway)",
            scenarios.len()
        );
        return;
    }

    let budgets = load_budgets();
    let scale = scale();
    let baseline = match Selected::from_env() {
        Ok(selected) => selected,
        Err(e) => panic!("{e}"),
    };
    let recording = std::env::var_os("UNDRA_BENCH_RECORD")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let load_before = hostinfo::load_average();
    let cfg = StressConfig {
        warmup: warmup_override(),
        ..StressConfig::new(Duration::from_secs_f64(seconds()))
    };
    let mut failures = Vec::new();
    let mut retried: Vec<String> = Vec::new();
    let mut final_reports: Vec<Finished> = Vec::new();
    let mut recorded = Baseline::default();
    eprintln!(
        "{} s per scenario after a warm-up of {} ms (UNDRA_STRESS_WARMUP_MS), scale {scale}; RSS is sampled {}",
        seconds(),
        cfg.warmup().as_millis(),
        if undra_bench::rss::resident_bytes().is_some() {
            "from the process"
        } else {
            "NOWHERE on this platform (the RSS gates are skipped)"
        }
    );
    if let Some(note) = undra_bench::rss::rss_caveat() {
        eprintln!("note: {note}");
    }
    match &baseline {
        Some(b) => eprintln!("{}", b.describe()),
        None => eprintln!(
            "no baseline selected (UNDRA_BENCH_BASELINE): only the absolute budgets gate this run"
        ),
    }
    let attempts = attempts();
    // The base's own binary, when the baseline was recorded by one that is still here (CI: the same job,
    // scripts/bench-record-base.sh): a scenario that misses only the baseline is decided by rounds of base
    // and head in turn, not by a number recorded minutes earlier.
    let base_binary = baseline.as_ref().and_then(base_stress_binary);
    if let Some(binary) = &base_binary {
        eprintln!(
            "a scenario that misses only the baseline is measured again, base and head in turn ({ROUNDS} rounds, \
             the best of each compared), with the base's binary {}",
            binary.display()
        );
    }
    header();
    for (name, run) in &scenarios {
        let Some(budget) = budgets.stress.get(*name) else {
            failures.push(format!(
                "{name}: no [stress.\"{name}\"] table in budgets.toml"
            ));
            continue;
        };
        // What a recording keeps: the best of each metric over the attempts.
        let mut best: Option<StressBaseline> = None;
        let mut passed = false;
        for attempt in 1..=attempts {
            let report = run(&cfg);
            let broken = report.broken();
            if !broken.is_empty() {
                row(&report, "INVARIANT BROKEN");
                let whats: Vec<String> = broken.iter().map(|i| i.what.clone()).collect();
                for what in &whats {
                    eprintln!("    x {what}");
                    failures.push(format!("{name}: {what}"));
                }
                final_reports.push(Finished {
                    report,
                    attempt,
                    failures: whats,
                });
                break;
            }
            let observed = observed(&report);
            let mut verdict = budget.check(&observed, scale);
            // Timing comparisons a scenario makes about itself are gates, not invariants.
            verdict.failures.extend(
                report
                    .timing_failures()
                    .iter()
                    .map(|check| check.what.clone()),
            );
            let others = verdict.failures.len();
            // What a machine class measured earlier: a regression on it fails whatever the budgets say.
            match baseline.as_ref().map(|b| {
                (
                    b,
                    b.stress_failures_with(name, &observed, budget.baseline_tolerance),
                )
            }) {
                Some((b, Some(over))) => verdict.failures.extend(
                    over.into_iter()
                        .map(|f| format!("{f}, against the baseline in {}", b.path.display())),
                ),
                Some((b, None)) => verdict.notices.push(format!(
                    "{name} has no table in {}: not gated against the baseline (record it again)",
                    b.path.display()
                )),
                None => {}
            }
            for notice in &verdict.notices {
                eprintln!("    notice: {notice}");
            }
            // Only the baseline was missed, and the base can be measured again: base and head in turn decide.
            if let (Some(b), Some(binary), None, false, 0) = (
                baseline.as_ref(),
                base_binary.as_deref(),
                recording.as_ref(),
                verdict.passed(),
                others,
            ) {
                row(
                    &report,
                    &format!("OVER the baseline (attempt {attempt}): base and head in turn"),
                );
                for problem in &verdict.failures {
                    eprintln!("    x {problem}");
                }
                match interleave(name, *run, &cfg, binary) {
                    Err(broken) => {
                        for what in &broken {
                            eprintln!("    x {what}");
                            failures.push(format!("{name}: {what}"));
                        }
                        final_reports.push(Finished {
                            report,
                            attempt,
                            failures: broken,
                        });
                        break;
                    }
                    Ok(None) => {
                        eprintln!(
                            "    notice: the base could not be measured again (see above): the scenario keeps its \
                             attempts against the recorded baseline"
                        );
                    }
                    Ok(Some((base, head, last))) => {
                        let over = b
                            .interleaved_stress_failures(
                                name,
                                &base,
                                &head,
                                budget.baseline_tolerance,
                            )
                            .unwrap_or_default();
                        let rates = |runs: &[StressObserved]| {
                            runs.iter()
                                .map(|r| human_rate(r.per_sec))
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        eprintln!(
                            "    base {} / head {}: {}",
                            rates(&base),
                            rates(&head),
                            if over.is_empty() {
                                "ok, best against best"
                            } else {
                                "OVER, best against best"
                            }
                        );
                        let over: Vec<String> = over
                            .into_iter()
                            .map(|f| {
                                format!("{f}, the best of {ROUNDS} rounds of base and head in turn")
                            })
                            .collect();
                        for problem in &over {
                            eprintln!("    x {problem}");
                            failures.push(format!("{name}: {problem}"));
                        }
                        final_reports.push(Finished {
                            report: last,
                            attempt,
                            failures: over,
                        });
                        break;
                    }
                }
            }
            if recording.is_some() {
                let mine = StressBaseline {
                    per_sec: report.per_sec(),
                    p99_ns: report.percentile(0.99).map(|n| n as f64),
                    p999_ns: report.percentile(0.999).map(|n| n as f64),
                    tolerance: None,
                };
                best = Some(match best.take() {
                    None => mine,
                    Some(b) => StressBaseline {
                        per_sec: b.per_sec.max(mine.per_sec),
                        p99_ns: b.p99_ns.zip(mine.p99_ns).map(|(a, c)| a.min(c)),
                        p999_ns: b.p999_ns.zip(mine.p999_ns).map(|(a, c)| a.min(c)),
                        tolerance: None,
                    },
                });
            }
            if verdict.passed() {
                let tag = if attempt == 1 || recording.is_some() {
                    "ok".to_owned()
                } else {
                    retried.push(format!("{name} (attempt {attempt} of {attempts})"));
                    format!("ok (attempt {attempt})")
                };
                row(&report, &tag);
                if !passed {
                    final_reports.push(Finished {
                        report,
                        attempt,
                        failures: Vec::new(),
                    });
                }
                passed = true;
                if recording.is_none() {
                    break;
                }
                continue;
            }
            row(
                &report,
                &format!("OVER a gate (attempt {attempt} of {attempts})"),
            );
            for problem in &verdict.failures {
                eprintln!("    x {problem}");
            }
            if attempt == attempts && !passed {
                for problem in &verdict.failures {
                    failures.push(format!("{name}: {problem}"));
                }
                // The result file of a scenario that failed says so, and why: it is evidence too.
                final_reports.push(Finished {
                    report,
                    attempt,
                    failures: verdict.failures,
                });
            }
        }
        if let Some(best) = best {
            recorded.stress.insert((*name).to_owned(), best);
        }
    }
    if !retried.is_empty() {
        eprintln!(
            "passed only on a retry (a noisy gate failed first; see the rows above): {}",
            retried.join(", ")
        );
    }
    if let Some(path) = std::env::var_os("UNDRA_STRESS_JSON") {
        let reports: Vec<&StressReport> = final_reports
            .iter()
            .filter(|f| f.failures.is_empty())
            .map(|f| &f.report)
            .collect();
        write_json(&PathBuf::from(path), &reports, &budgets);
    }
    if let Some(dir) = undra_bench::results::dir_from_env() {
        write_results(
            &dir,
            &final_reports,
            &budgets,
            baseline.as_ref(),
            (scale, load_before, hostinfo::load_average()),
        );
    }
    if let Some(path) = &recording {
        record(path, recorded, load_before);
    }
    assert!(
        failures.is_empty(),
        "{} stress gate(s) failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// A scenario's last report, which attempt it was, and the gates it missed (none: it passed).
struct Finished {
    report: StressReport,
    attempt: usize,
    failures: Vec<String>,
}

/// Writes the JSON behind each scenario's numbers: one file per scenario in `dir`, named after the
/// date, the optional `UNDRA_BENCH_RESULTS_TAG` and the scenario.
fn write_results(
    dir: &std::path::Path,
    reports: &[Finished],
    budgets: &Budgets,
    baseline: Option<&Selected>,
    (scale, load_before, load_after): (f64, Option<f64>, Option<f64>),
) {
    use undra_bench::results::{
        command, json_number, json_opt, json_string, machine_members, path_for, slug, tag_from_env,
        write,
    };
    let (date, tag) = (hostinfo::date(), tag_from_env());
    let how = command(
        "cargo test -p undra-bench --test stress --release -- --nocapture",
        &[
            "UNDRA_BENCH_RESULTS_TAG",
            "UNDRA_STRESS_SECONDS",
            "UNDRA_STRESS_WARMUP_MS",
            "UNDRA_BENCH_SCALE",
            "UNDRA_BENCH_FILTER",
            "UNDRA_BENCH_BASELINE",
            "UNDRA_BENCH_BASELINE_TOLERANCE",
        ],
    );
    for Finished {
        report,
        attempt,
        failures,
    } in reports
    {
        let at = |p: f64| report.percentile(p).map(|n| n as f64);
        let strings = |items: Vec<String>| items.join(", ");
        let invariants = strings(
            report
                .invariants
                .iter()
                .map(|i| {
                    format!(
                        "{{\"what\": {}, \"holds\": {}, \"timing\": {}}}",
                        json_string(&i.what),
                        i.holds,
                        i.timing
                    )
                })
                .collect(),
        );
        let notes = strings(report.notes.iter().map(|n| json_string(n)).collect());
        let gate = budgets
            .stress
            .get(report.name)
            .map(|b| gate_text(b, "ops"))
            .unwrap_or_default();
        let text = format!(
            "{{\n  \"scenario\": {},\n  \"kind\": \"sustained\",\n  \"date\": {},\n  \"tag\": {},\n  \"command\": {},\n  {},\n  \
             \"run\": {{\"seconds\": {}, \"warmup_ms\": {}, \"elapsed_s\": {}, \"attempt\": {}, \"attempts\": {}, \"scale\": {}, \"baseline\": {}}},\n  \
             \"result\": {{\"ops\": {}, \"per_sec\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"p999_ns\": {}, \"max_ns\": {}, \"bytes_per_op\": {}, \"rss_growth_pct\": {}, \"rss_baseline_bytes\": {}, \"rss_final_bytes\": {}}},\n  \
             \"gate\": {{\"budget\": {}, \"passed\": {}, \"failures\": [{}]}},\n  \"invariants\": [{}],\n  \"notes\": [{}]\n}}\n",
            json_string(report.name),
            json_string(&date),
            tag.as_deref()
                .map_or_else(|| "null".to_owned(), json_string),
            json_string(&how),
            machine_members(load_before, load_after),
            json_number(seconds()),
            report.warmup.as_millis(),
            json_number(report.elapsed.as_secs_f64()),
            attempt,
            attempts(),
            json_number(scale),
            baseline.map_or_else(
                || "null".to_owned(),
                |b| json_string(&b.path.display().to_string())
            ),
            report.ops,
            json_number(report.per_sec()),
            json_opt(at(0.5)),
            json_opt(at(0.99)),
            json_opt(at(0.999)),
            json_opt((report.latency.count() > 0).then(|| report.latency.max() as f64)),
            json_number(report.bytes_per_op()),
            json_opt(report.rss.map(|g| g.growth_pct)),
            json_opt(report.rss.map(|g| g.baseline_bytes as f64)),
            json_opt(report.rss.map(|g| g.final_bytes as f64)),
            json_string(&gate),
            failures.is_empty(),
            strings(failures.iter().map(|f| json_string(f)).collect()),
            invariants,
            notes,
        );
        write(
            &path_for(dir, &date, tag.as_deref(), &slug(report.name)),
            &text,
        );
    }
    eprintln!("wrote {} result files to {}", reports.len(), dir.display());
}

/// Writes what this run measured as a baseline, next to the facts about the machine.
fn record(path: &std::path::Path, mut baseline: Baseline, load_before: Option<f64>) {
    let load = |l: Option<f64>| l.map_or_else(|| "?".to_owned(), |l| format!("{l:.2}"));
    for (key, value) in [
        ("cpu", hostinfo::cpu()),
        ("cores", hostinfo::cores().to_string()),
        ("os", hostinfo::os()),
        ("rustc", hostinfo::rustc()),
        ("date", hostinfo::date()),
        ("git", hostinfo::git_revision()),
        ("stress_seconds", seconds().to_string()),
        (
            "command_layer_b",
            undra_bench::results::command(
                "cargo test -p undra-bench --test stress --release",
                &[
                    "UNDRA_BENCH_RECORD",
                    "UNDRA_BENCH_RECORD_BEST",
                    "UNDRA_STRESS_SECONDS",
                    "UNDRA_STRESS_WARMUP_MS",
                    "UNDRA_BENCH_FILTER",
                ],
            ),
        ),
        (
            "load_layer_b",
            format!(
                "{} before, {} after (one-minute load average)",
                load(load_before),
                load(hostinfo::load_average())
            ),
        ),
    ] {
        baseline.meta.entry(key.to_owned()).or_insert(value);
    }
    // The binary that measured this, for a head gated against it to measure it again in turn
    // (scripts/bench-record-base.sh asks for it; a committed baseline carries no local path).
    let asked =
        std::env::var_os("UNDRA_BENCH_RECORD_BINARY").is_some_and(|v| !v.is_empty() && v != "0");
    if let (true, Ok(me)) = (asked, std::env::current_exe()) {
        baseline
            .meta
            .insert("stress_binary".to_owned(), me.display().to_string());
    }
    let rows = baseline.stress.len();
    if let Err(e) = baseline.record_into(path, undra_bench::baseline::record_keeps_best()) {
        panic!("cannot write UNDRA_BENCH_RECORD to {}: {e}", path.display());
    }
    eprintln!("recorded {rows} scenarios to {}", path.display());
}

/// The base's stress binary named by the baseline (`stress_binary` in its `[meta]`), when it is a file
/// here and not this binary.
fn base_stress_binary(baseline: &Selected) -> Option<PathBuf> {
    let path = PathBuf::from(baseline.baseline.meta.get("stress_binary")?);
    let me = std::env::current_exe().ok();
    (path.is_file() && me.as_deref() != Some(path.as_path())).then_some(path)
}

/// Measures `name` in [`ROUNDS`] rounds of base and head in turn on this machine: the base's binary
/// runs the scenario once (`UNDRA_STRESS_ATTEMPTS=1`, recording into a file of its own), then this
/// binary runs it once. Returns the base's runs, the head's runs and the head's last report; `Ok(None)`
/// when the base's binary did not record the scenario (it then cannot be compared in turn); `Err` with
/// what broke when an invariant of the head broke (never noise).
#[allow(clippy::type_complexity)]
fn interleave(
    name: &str,
    run: Scenario,
    cfg: &StressConfig,
    base_binary: &std::path::Path,
) -> Result<Option<(Vec<StressObserved>, Vec<StressObserved>, StressReport)>, Vec<String>> {
    let mut base = Vec::new();
    let mut head = Vec::new();
    let mut last = None;
    for round in 1..=ROUNDS {
        let out = std::env::temp_dir().join(format!(
            "undra-stress-base-{}-{}-{round}.toml",
            std::process::id(),
            name.replace('/', "-")
        ));
        let _ = std::fs::remove_file(&out);
        let child = std::process::Command::new(base_binary)
            .args(["stress", "--exact", "--nocapture", "--test-threads=1"])
            .env("UNDRA_BENCH_SCENARIO", name)
            .env("UNDRA_BENCH_RECORD", &out)
            .env("UNDRA_STRESS_ATTEMPTS", "1")
            .env("UNDRA_BENCH_BUDGETS", budgets_path())
            .env_remove("UNDRA_BENCH_BASELINE")
            .env_remove("UNDRA_BENCH_RECORD_BEST")
            .env_remove("UNDRA_BENCH_RECORD_BINARY")
            .env_remove("UNDRA_BENCH_FILTER")
            .env_remove("UNDRA_STRESS_JSON")
            .env_remove("UNDRA_BENCH_RESULTS_DIR")
            .output();
        let measured = Baseline::load(&out)
            .ok()
            .and_then(|b| b.stress.get(name).cloned());
        let _ = std::fs::remove_file(&out);
        let Some(recorded) = measured else {
            let why = match child {
                Ok(o) => String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | "),
                Err(e) => e.to_string(),
            };
            eprintln!("    the base's binary recorded nothing for {name} in round {round}: {why}");
            return Ok(None);
        };
        base.push(StressObserved {
            per_sec: recorded.per_sec,
            p99_ns: recorded.p99_ns,
            p999_ns: recorded.p999_ns,
            ..StressObserved::default()
        });
        let report = run(cfg);
        let broken: Vec<String> = report.broken().iter().map(|i| i.what.clone()).collect();
        if !broken.is_empty() {
            row(&report, "INVARIANT BROKEN");
            return Err(broken);
        }
        head.push(observed(&report));
        last = Some(report);
    }
    Ok(last.map(|last| (base, head, last)))
}

/// The words of every invariant that broke when `scenario` ran with `fault`: a run of no set time
/// that goes on until it has done `FAULT_OPS` operations, so the fault always has the operations
/// it needs to fire (a fixed time did fewer than 101 on a slow machine, and the fault never
/// fired).
fn broken_by(scenario: fn(&StressConfig) -> StressReport, fault: Fault) -> Vec<String> {
    let cfg = StressConfig {
        duration: Duration::ZERO,
        rss: false,
        fault,
        warmup: None,
        min_ops: FAULT_OPS,
    };
    let report = scenario(&cfg);
    assert_ran(&report, &cfg);
    report.broken().iter().map(|i| i.what.clone()).collect()
}

#[test]
fn a_skipped_patch_fails_the_equality_invariant() {
    let _serial = serial();
    let broken = broken_by(common::stress::keyed_churn, Fault::SkipPatches);
    assert!(
        broken
            .iter()
            .any(|what| what.contains("the host list equals the core list")),
        "a host that drops a patch must fail the equality invariant: {broken:?}"
    );
}

#[test]
fn a_skipped_view_patch_fails_the_view_invariant() {
    // ADR-039: the host's derived view must equal filter + stable sort of the core's rows.
    let _serial = serial();
    let broken = broken_by(common::stress::derived_churn, Fault::SkipPatches);
    assert!(
        broken
            .iter()
            .any(|what| what.contains("the host's view equals filter + stable sort")),
        "a host that drops a view patch must fail the view invariant: {broken:?}"
    );
    assert!(
        broken.iter().any(|what| what
            .contains("every view entry after the first full value was a patch, applied")),
        "and the patch-count invariant: {broken:?}"
    );
}

#[test]
fn a_single_dropped_update_fails_the_mirror_invariants() {
    // One lost `Update` keeps every id in place, and a later update of the same row repairs
    // the content: only counting the patches applied sees it for certain.
    let _serial = serial();
    let broken = broken_by(common::stress::keyed_churn, Fault::DropOneUpdate);
    assert!(
        broken
            .iter()
            .any(|what| what.contains("every operation was applied on the host as one keyed patch")),
        "a host that drops one patch must fail the patch-count invariant: {broken:?}"
    );
}

#[test]
fn the_default_warm_up_is_a_tenth_of_the_run_capped_at_200_ms() {
    use common::stress::default_warmup;
    assert_eq!(
        default_warmup(Duration::from_secs(10)),
        Duration::from_millis(200)
    );
    assert_eq!(
        default_warmup(Duration::from_secs(2)),
        Duration::from_millis(200)
    );
    assert_eq!(
        default_warmup(Duration::from_millis(500)),
        Duration::from_millis(50)
    );
    assert_eq!(
        default_warmup(Duration::from_millis(100)),
        Duration::from_millis(10)
    );
    // Configured wins, and zero means none.
    let cfg = StressConfig {
        warmup: Some(Duration::from_millis(7)),
        ..StressConfig::new(Duration::from_secs(10))
    };
    assert_eq!(cfg.warmup(), Duration::from_millis(7));
    assert_eq!(
        cfg.warmup_for(Duration::from_secs(1)),
        Duration::from_millis(7)
    );
    assert_eq!(
        StressConfig::new(Duration::from_secs(1)).warmup(),
        Duration::from_millis(100)
    );
}

#[test]
fn a_warm_up_runs_first_and_is_not_measured() {
    // Every scenario that has one reports it, runs for the measured duration after it, and counts
    // only that run: the "one change-set per transaction (N for N)" invariants hold only if the
    // counters were read after the warm-up, whatever it did before.
    let _serial = serial();
    for (scenario, name) in [
        (
            common::stress::firehose as fn(&StressConfig) -> StressReport,
            "firehose",
        ),
        (common::stress::event_firehose, "event"),
        (common::stress::keyed_churn, "keyed_churn"),
        (common::stress::fanout_stores, "fanout_stores"),
        (common::stress::completions, "completions"),
        (common::stress::completions_contended, "contended"),
    ] {
        let cfg = StressConfig {
            duration: Duration::from_millis(100),
            rss: false,
            fault: Fault::None,
            warmup: Some(Duration::from_millis(60)),
            min_ops: SMOKE_OPS,
        };
        let report = scenario(&cfg);
        assert_eq!(report.warmup, Duration::from_millis(60), "{name}");
        assert!(report.broken().is_empty(), "{name}: {:?}", report.broken());
        assert_ran(&report, &cfg);
        // The measured run is at least its 100 ms. No upper bound: how much longer than that a
        // run takes (a round, the calls in flight, the floor of operations) is the machine's, and
        // a run that cannot finish is the watchdog's (`GIVE_UP`).
        assert!(
            report.elapsed >= cfg.duration,
            "{name}: {:?}",
            report.elapsed
        );
    }
    // And a scenario with none measures from the first operation.
    let none = StressConfig {
        warmup: Some(Duration::ZERO),
        ..StressConfig::new(Duration::from_millis(50))
    };
    let report = common::stress::firehose(&StressConfig { rss: false, ..none });
    assert_eq!(report.warmup, Duration::ZERO);
    assert!(report.broken().is_empty());
}

#[test]
fn the_completions_scenario_stops_near_its_deadline() {
    // The issuer once looked at the clock only when its window of calls in flight filled; when the
    // completer threads kept pace it never did, and a run of 200 ms lasted up to 154 s (debug,
    // a loaded machine). A run is its duration plus the time to answer what is in flight.
    let _serial = serial();
    let cfg = StressConfig {
        duration: Duration::from_millis(100),
        rss: false,
        fault: Fault::None,
        warmup: None,
        min_ops: 0,
    };
    for _ in 0..4 {
        // On a thread with a watchdog: the old loop did not return at all (over 150 s, twice, in
        // review), and a test that hangs fails only when the CI job times out.
        let (done, report) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(common::stress::completions(&cfg));
        });
        let report = report
            .recv_timeout(Duration::from_secs(60))
            .expect("a 100 ms run did not end in 60 s: the issuer ignored its deadline");
        assert!(
            report.elapsed < Duration::from_secs(5),
            "a 100 ms run took {:?}: the issuer ignored its deadline",
            report.elapsed
        );
        assert!(report.broken().is_empty());
    }
}

#[test]
fn swapped_change_sets_fail_the_order_invariants() {
    let _serial = serial();
    for scenario in [
        common::stress::completions,
        common::stress::completions_contended,
    ] {
        let broken = broken_by(scenario, Fault::SwapChangeSets);
        assert!(
            broken
                .iter()
                .any(|what| what.contains("nothing arrived out of order")),
            "a main thread that swaps two change-sets must fail the order invariant: {broken:?}"
        );
        assert!(
            broken.iter().any(|what| what.contains("exact count")),
            "...and the total must stop arriving as a count: {broken:?}"
        );
        // Only those: nothing was lost and every call was answered.
        assert_eq!(broken.len(), 2, "{broken:?}");
    }
}

#[test]
fn a_change_set_lost_on_the_way_to_the_ui_fails_the_loss_invariants() {
    let _serial = serial();
    for scenario in [
        common::stress::completions,
        common::stress::completions_contended,
    ] {
        let broken = broken_by(scenario, Fault::DropChangeSet);
        assert!(
            broken
                .iter()
                .any(|what| what.contains("the main thread saw every change-set")),
            "{broken:?}"
        );
        assert!(
            broken.iter().any(|what| what.contains("exact count")),
            "{broken:?}"
        );
        // The core delivered and answered everything: only the UI side lost one.
        assert_eq!(broken.len(), 2, "{broken:?}");
    }
}

#[test]
fn the_contended_scenario_really_has_two_threads_writing_one_store() {
    // The point of the scenario: besides the completions that commit on the core thread, a host
    // thread's `call_sync` writes land in the same store, and every one of them is accounted for.
    let _serial = serial();
    let cfg = StressConfig {
        duration: Duration::from_millis(300),
        rss: false,
        fault: Fault::None,
        warmup: None,
        min_ops: SMOKE_OPS,
    };
    let report = common::stress::completions_contended(&cfg);
    assert_ran(&report, &cfg);
    assert!(report.broken().is_empty(), "{:?}", report.broken());
    let writer = report
        .notes
        .iter()
        .find(|n| n.starts_with("writer thread:"))
        .expect("the scenario reports its writer");
    assert!(
        !writer.contains(": 0 call_sync"),
        "the writer never ran: {writer}"
    );
    let writes: u64 = report
        .invariants
        .iter()
        .find_map(|i| {
            i.what
                .strip_prefix("every write from the host thread succeeded (")
        })
        .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
        .expect("the writes invariant says how many");
    assert!(writes > 0);
    // The measured run's store writes are among those of the whole run (the warm-up is not in it).
    let replies: u64 = report
        .invariants
        .iter()
        .find_map(|i| i.what.strip_prefix("every call was answered ("))
        .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
        .expect("the answered invariant says how many");
    assert!(
        report.ops > 0 && report.ops <= replies + writes,
        "{} of {}",
        report.ops,
        replies + writes
    );
}

/// A change-set of one entry for store `handle`, transaction `txn_id`.
fn change_set(txn_id: u64, handles: &[u32]) -> Vec<u8> {
    let mut w = Writer::new();
    let mut b = ChangeSetBuilder::new(&mut w, txn_id);
    for handle in handles {
        b.push(
            Handle::new(*handle, 1),
            0,
            undra::wire::payload::ChangeOp::Full,
            &[0; 8],
        );
    }
    b.finish();
    w.into_vec()
}

#[test]
fn the_order_checker_accepts_increasing_ids_per_store_and_rejects_anything_else() {
    let mut ok = OrderChecker::default();
    // Two stores interleave freely: only each store's own sequence matters.
    for (txn, stores) in [(1, &[1][..]), (2, &[2]), (3, &[1, 2]), (9, &[1])] {
        assert!(ok.check(&change_set(txn, stores)), "txn {txn}");
    }
    // A repeat, a step back, and a swapped pair all fail.
    assert!(!ok.check(&change_set(9, &[1])), "the same id twice");
    assert!(
        !ok.check(&change_set(2, &[2])),
        "a step back for store 2 (it was at 3)"
    );
    let mut swapped = OrderChecker::default();
    assert!(
        swapped.check(&change_set(6, &[7])),
        "the first change-set of a store has nothing to follow"
    );
    assert!(!swapped.check(&change_set(5, &[7])), "a swapped pair");
    let mut garbage = OrderChecker::default();
    assert!(
        !garbage.check(&[1, 2, 3]),
        "a malformed change-set is not in order"
    );
}

// ---------------------------------------------------------------------------------------------
// Baseline and the site's JSON
// ---------------------------------------------------------------------------------------------

/// Two significant figures, rounded up: 1,337 becomes 1,400.
fn round_up(value: f64) -> u64 {
    let n = value.max(1.0).ceil() as u64;
    let digits = n.ilog10();
    if digits < 2 {
        return n.div_ceil(10) * 10;
    }
    let step = 10_u64.pow(digits - 1);
    n.div_ceil(step) * step
}

/// Two significant figures, rounded down: 5,799,209 becomes 5,700,000.
fn round_down(value: f64) -> u64 {
    let n = value.max(1.0).floor() as u64;
    let digits = n.ilog10();
    if digits < 2 {
        return n;
    }
    let step = 10_u64.pow(digits - 1);
    n / step * step
}

/// Not a test: prints measurements as `budgets.toml` tables.
#[test]
#[ignore = "prints a baseline; run it explicitly with --ignored --nocapture"]
fn stress_baseline() {
    let _serial = serial();
    let cfg = StressConfig::new(Duration::from_secs_f64(seconds()));
    let factor: f64 = std::env::var("UNDRA_BENCH_FACTOR")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(5.0);
    for (name, run) in selected() {
        let report = run(&cfg);
        let per_sec = report.per_sec();
        println!("[stress.\"{name}\"]");
        println!("min_per_sec = {}", round_down(per_sec / factor));
        if let (Some(p99), Some(p999)) = (report.percentile(0.99), report.percentile(0.999)) {
            println!("p99_ns = {}", round_up(p99 as f64 * factor));
            println!("p999_ns = {}", round_up(p999 as f64 * factor * 2.0));
        }
        let bytes = report.bytes_per_op();
        if name != "stream/backpressure" {
            if BYTES_EXACT.contains(&name) {
                println!("bytes_per_op = {}", bytes.ceil() as u64);
            } else {
                println!(
                    "bytes_per_op = {}  # 1.1x the measured {bytes:.1}",
                    (bytes * 1.1).ceil() as u64
                );
            }
        }
        if let Some(growth) = report.rss {
            println!("rss_growth_pct = 1");
            println!(
                "# RSS {:+.2}% after warm-up ({} to {} bytes)",
                growth.growth_pct, growth.baseline_bytes, growth.final_bytes
            );
        }
        println!("measured_per_sec = {}", per_sec.round() as u64);
        if let (Some(p99), Some(p999)) = (report.percentile(0.99), report.percentile(0.999)) {
            println!("measured_p99_ns = {p99}");
            println!("measured_p999_ns = {p999}");
        }
        if name != "stream/backpressure" {
            println!("measured_bytes_per_op = {bytes:.1}");
        }
        for note in &report.notes {
            println!("# {note}");
        }
        for invariant in report.broken() {
            println!("# INVARIANT BROKEN: {}", invariant.what);
        }
        for check in report.timing_failures() {
            println!("# TIMING CHECK FAILED: {}", check.what);
        }
        println!();
    }
}

/// What the site shows for a scenario: its id, label, the unit of one operation and the step that
/// is timed.
const SITE_ROWS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "firehose/sustained",
        "stress/firehose",
        "Firehose: one signal, one transaction per update",
        "txn",
        "transaction (commit + deliver)",
    ),
    (
        "event/sustained",
        "stress/event",
        "Event firehose: host events written into a signal",
        "events",
        "event (commit + deliver)",
    ),
    (
        "keyed_churn_10k/sustained",
        "stress/keyed_churn",
        "Keyed churn: a 10,000-row list, one list operation per transaction",
        "ops",
        "operation (commit + deliver + host apply)",
    ),
    (
        "derived_churn_10k/sustained",
        "stress/derived_churn",
        "Derived churn: a 10,000-row list and a sorted view of it, one list operation per transaction",
        "ops",
        "operation (commit of the list and its view + deliver + both host applies)",
    ),
    (
        "fanout/sustained",
        "stress/fanout",
        "Fan-out: 1,000 of 100,000 observed signals changed in one transaction",
        "txn",
        "transaction",
    ),
    (
        "fanout_stores/sustained",
        "stress/fanout_stores",
        "Fan-out across 1,000 stores in one transaction",
        "txn",
        "transaction of 1,000 change-sets",
    ),
    (
        "stream/backpressure",
        "stress/stream",
        "Stream backpressure: never more than one item ahead of the consumer",
        "items",
        "",
    ),
    (
        "completions/8_threads",
        "stress/completions",
        "Concurrent completions: 8 threads, nothing lost",
        "completions",
        "call to reply",
    ),
    (
        "completions/contended",
        "stress/completions_contended",
        "Contended completions: a host thread writing the same store, nothing lost or reordered",
        "writes",
        "completion call to reply",
    ),
];

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn human_count(per_sec: f64, unit: &str) -> String {
    if per_sec >= 1e6 {
        format!("{:.1} M {unit}/s", per_sec / 1e6)
    } else if per_sec >= 1e3 {
        format!("{:.0} k {unit}/s", per_sec / 1e3)
    } else {
        format!("{per_sec:.0} {unit}/s")
    }
}

fn gate_text(budget: &StressBudget, unit: &str) -> String {
    let mut parts = Vec::new();
    if let Some(floor) = budget.min_per_sec {
        parts.push(format!("at least {}", human_count(floor, unit)));
    }
    if let Some(p99) = budget.p99_ns {
        parts.push(format!("p99 at most {}", human_ns(Some(p99 as u64))));
    }
    if let Some(pct) = budget.rss_growth_pct {
        parts.push(format!("RSS growth at most {pct}%"));
    }
    format!("CI: {}", parts.join(", "))
}

/// Writes one JSON row per scenario in the shape of `site/data/bench.json`'s "harsh" group.
fn write_json(path: &PathBuf, reports: &[&StressReport], budgets: &Budgets) {
    let machine = budgets.meta.get("machine").cloned().unwrap_or_default();
    let mut rows = Vec::new();
    for report in reports.iter().copied() {
        let Some((_, id, label, unit, step)) = SITE_ROWS.iter().find(|r| r.0 == report.name) else {
            continue;
        };
        let detail = match (report.percentile(0.99), report.name) {
            (Some(p99), _) if !step.is_empty() => format!(
                "p99 {} per {step}, {:.0} bytes each; every invariant held",
                human_ns(Some(p99)),
                report.bytes_per_op()
            ),
            _ => report.notes.first().cloned().unwrap_or_default(),
        };
        let gate = budgets
            .stress
            .get(report.name)
            .map(|b| gate_text(b, unit))
            .unwrap_or_default();
        rows.push(format!(
            "  {{\n    \"group\": \"harsh\",\n    \"id\": {},\n    \"label\": {},\n    \"value\": {},\n    \"detail\": {},\n    \"gate\": {},\n    \"where\": {},\n    \"source\": \"bench/RESULTS.md#harsh-conditions\"\n  }}",
            json_string(id),
            json_string(label),
            json_string(&human_count(report.per_sec(), unit)),
            json_string(&detail),
            json_string(&gate),
            json_string(&format!("{machine}, core only (host)")),
        ));
    }
    let text = format!("[\n{}\n]\n", rows.join(",\n"));
    if let Err(e) = std::fs::write(path, text) {
        panic!("cannot write UNDRA_STRESS_JSON to {}: {e}", path.display());
    }
    eprintln!("wrote {} rows to {}", rows.len(), path.display());
}
