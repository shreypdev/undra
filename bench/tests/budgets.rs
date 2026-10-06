//! The budget gate (constitution R9): every benchmarked operation runs a few thousand times with
//! plain `Instant` timing and its p50 must be under its budget in `bench/budgets.toml`.
//!
//! This is what CI runs (`cargo test -p undra-bench --test budgets --release`); criterion is for
//! humans and stays out of the critical path.
//!
//! * The budgets are **host** regression guards, about five times what an Apple-silicon laptop
//!   measures, not the blueprint's device targets (see `budgets.toml` and `bench/RESULTS.md`).
//! * Without optimisations the numbers mean nothing, so a debug build (plain `cargo test`) only
//!   smoke-runs every operation a few times: fixtures and operations stay working under
//!   `cargo test --workspace`, and the timing assertion is left to `--release`.
//! * A noisy run gets three attempts; the best p50 counts. One slow neighbour does not fail CI,
//!   a real regression is slow on every attempt.
//! * Two more gates close what a 5x budget lets through (see `bench/RESULTS.md`, "Gates that catch
//!   a 2x regression"): the **ratio** tables of `budgets.toml` compare two rows measured in this
//!   run, so the machine's speed cancels; and `UNDRA_BENCH_BASELINE` selects a **baseline**
//!   (`bench/baselines/<name>.toml`, or a file recorded earlier in the same job) that fails a row
//!   whose p50 is 1.5x its baseline's. `UNDRA_BENCH_RECORD=path` records one.
//! * `UNDRA_BENCH_RESULTS_DIR=dir` writes the JSON behind the run (every row, every ratio, the
//!   command, the machine and its load) as `<date>-layer-a.json`, for `bench/results/`.
//! * Knobs: `UNDRA_BENCH_SCALE=2.5` multiplies every budget (a slower runner; not a baseline or a
//!   ratio), `UNDRA_BENCH_FILTER=keyed` measures only matching names, `UNDRA_BENCH_BUDGETS=path`
//!   reads another file, `UNDRA_BENCH_BASELINE`, `UNDRA_BENCH_BASELINE_TOLERANCE` and
//!   `UNDRA_BENCH_RECORD` as above.
//! * `cargo test -p undra-bench --test budgets --release -- --ignored --nocapture baseline`
//!   prints fresh measurements in `budgets.toml` syntax, for setting or re-basing a budget.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use undra_bench::baseline::{Baseline, BenchBaseline, Selected};
use undra_bench::budget::Budgets;
use undra_bench::hostinfo;
use undra_bench::measure::{MeasureConfig, Stats, measure};
use undra_bench::workload::Workload;

#[path = "../common/mod.rs"]
mod common;

/// Timing tests must not overlap: a second test thread would be noise in the first.
static SERIAL: Mutex<()> = Mutex::new(());

const ATTEMPTS: usize = 3;

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

/// The baseline `UNDRA_BENCH_BASELINE` selects. A name or path that does not resolve is a panic:
/// a typo must not switch the gate off.
fn select_baseline() -> Option<Selected> {
    match Selected::from_env() {
        Ok(selected) => selected,
        Err(e) => panic!("{e}"),
    }
}

/// Where `UNDRA_BENCH_RECORD` says to write the measured baseline.
fn record_path() -> Option<PathBuf> {
    std::env::var_os("UNDRA_BENCH_RECORD")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

fn selected(workloads: Vec<Workload>) -> Vec<Workload> {
    match std::env::var("UNDRA_BENCH_FILTER") {
        Ok(filter) if !filter.is_empty() => workloads
            .into_iter()
            .filter(|w| w.name.contains(&filter))
            .collect(),
        _ => workloads,
    }
}

/// Builds the operation afresh for each attempt and keeps the best p50 (and, separately, the best
/// p99: a row whose claim is its tail is judged on its cleanest tail, as the others are on their
/// cleanest median). The attempts stop at the first one under `budget_ns` and, when the row has
/// one, `p99_limit_ns`.
fn best_of(
    workload: &Workload,
    attempts: usize,
    budget_ns: f64,
    p99_limit_ns: Option<f64>,
) -> Stats {
    let config = MeasureConfig::default();
    let mut best: Option<Stats> = None;
    let mut best_p99 = f64::MAX;
    for _ in 0..attempts {
        let mut op = workload.build();
        let stats = measure(&mut *op, &config);
        best_p99 = best_p99.min(stats.p99_ns);
        if best.is_none_or(|b| stats.p50_ns < b.p50_ns) {
            best = Some(stats);
        }
        let under_p99 = p99_limit_ns.is_none_or(|limit| best_p99 <= limit);
        if stats.p50_ns <= budget_ns && under_p99 {
            break;
        }
    }
    let mut best = best.expect("at least one attempt");
    best.p99_ns = best_p99;
    best
}

fn human(ns: f64) -> String {
    if ns >= 1_000_000.0 {
        format!("{:.2} ms", ns / 1_000_000.0)
    } else if ns >= 1_000.0 {
        format!("{:.2} us", ns / 1_000.0)
    } else {
        format!("{ns:.1} ns")
    }
}

#[test]
fn the_budget_file_covers_every_workload_and_nothing_else() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let budgets = load_budgets();
    let workloads: BTreeSet<String> = common::workloads::all()
        .into_iter()
        .map(|w| w.name)
        .collect();
    let budgeted: BTreeSet<String> = budgets.benches.keys().cloned().collect();
    let unbudgeted: Vec<_> = workloads.difference(&budgeted).collect();
    let stale: Vec<_> = budgeted.difference(&workloads).collect();
    assert!(
        unbudgeted.is_empty(),
        "these operations have no budget in budgets.toml (run the `baseline` test and add them): {unbudgeted:?}"
    );
    assert!(
        stale.is_empty(),
        "budgets.toml names operations that no longer exist: {stale:?}"
    );
    for (name, ratio) in &budgets.ratios {
        for row in [&ratio.num, &ratio.den] {
            assert!(
                workloads.contains(row),
                "[ratio.\"{name}\"] names `{row}`, which is not an operation the gate runs"
            );
        }
        let problems = ratio.self_check();
        assert!(problems.is_empty(), "[ratio.\"{name}\"]: {problems:?}");
    }
    for (name, b) in &budgets.benches {
        assert!(b.budget_ns > 0.0, "{name}: a budget of zero can never pass");
        if let Some(measured) = b.measured_ns {
            assert!(
                b.budget_ns >= measured,
                "{name}: the budget ({}) is below what was measured ({measured})",
                b.budget_ns
            );
        }
        if let (Some(ceiling), Some(measured)) = (b.p99_ns, b.measured_p99_ns) {
            assert!(
                ceiling >= measured,
                "{name}: the p99 ceiling ({ceiling}) is below what was measured ({measured})"
            );
        }
        assert!(
            b.measured_p99_ns.is_none() || b.p99_ns.is_some(),
            "{name}: measured_p99_ns without a p99_ns gate"
        );
    }
}

#[test]
fn committed_baselines_parse_and_name_only_rows_that_exist() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let workloads: BTreeSet<String> = common::workloads::all()
        .into_iter()
        .map(|w| w.name)
        .collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("baselines");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let baseline = Baseline::load(&path).unwrap_or_else(|e| panic!("{e}"));
        let stale: Vec<_> = baseline
            .benches
            .keys()
            .filter(|name| !workloads.contains(*name))
            .collect();
        assert!(
            stale.is_empty(),
            "{} has rows for operations that no longer exist (a renamed row silently leaves the \
             gate; re-record): {stale:?}",
            path.display()
        );
        for (name, row) in &baseline.benches {
            assert!(
                row.p50_ns > 0.0,
                "{}: {name} has a p50 of zero",
                path.display()
            );
        }
        for key in ["cpu", "date"] {
            assert!(
                baseline.meta.contains_key(key),
                "{}: [meta] says nothing about `{key}`: a number without its machine is not one",
                path.display()
            );
        }
    }
}

#[test]
fn budgets() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let workloads = selected(common::workloads::all());
    assert!(!workloads.is_empty(), "UNDRA_BENCH_FILTER matched nothing");

    if cfg!(debug_assertions) && std::env::var_os("UNDRA_BENCH_FORCE").is_none() {
        // Timings from an unoptimised build say nothing; prove the operations still work.
        for workload in &workloads {
            let mut op = workload.build();
            for _ in 0..3 {
                if op.needs_reset() {
                    op.reset();
                }
                op.run();
            }
        }
        eprintln!(
            "budgets: {} operations smoke-run; timing is asserted only with --release \
             (UNDRA_BENCH_FORCE=1 to time a debug build anyway)",
            workloads.len()
        );
        return;
    }

    let budgets = load_budgets();
    let scale = scale();
    let baseline = select_baseline();
    let recording = record_path();
    let load_before = hostinfo::load_average();
    let mut failures = Vec::new();
    let mut p50s: BTreeMap<String, f64> = BTreeMap::new();
    let mut rows: Vec<RowResult> = Vec::new();
    let mut ratio_results: Vec<RatioResult> = Vec::new();
    match &baseline {
        Some(b) => eprintln!("{}", b.describe()),
        None => eprintln!(
            "no baseline selected (UNDRA_BENCH_BASELINE): only the absolute budgets and the \
             ratios gate this run"
        ),
    }
    eprintln!(
        "{:<46} {:>10} {:>10} {:>10} {:>7} {:>8}  blueprint",
        "operation", "p50", "p90", "budget", "margin", "vs base"
    );
    for workload in &workloads {
        let Some(budget) = budgets.benches.get(&workload.name) else {
            failures.push(format!("{}: no budget in budgets.toml", workload.name));
            continue;
        };
        let limit = budget.budget_ns * scale;
        let gate = baseline
            .as_ref()
            .and_then(|b| b.bench_gate_with(&workload.name, budget.baseline_tolerance));
        // The attempts stop at the first one under both gates; a recording keeps the best of all.
        let effective = gate.map_or(limit, |g| limit.min(g.limit_ns));
        let stop_at = if recording.is_some() { 0.0 } else { effective };
        let p99_limit = budget.p99_ns.map(|p99| p99 * scale);
        let stats = best_of(workload, ATTEMPTS, stop_at, p99_limit);
        p50s.insert(workload.name.clone(), stats.p50_ns);
        rows.push(RowResult {
            name: workload.name.clone(),
            p50_ns: stats.p50_ns,
            p90_ns: stats.p90_ns,
            p99_ns: stats.p99_ns,
            budget_ns: limit,
            baseline_ns: gate.map(|g| g.baseline_ns),
            iterations: stats.iterations,
            failed: stats.p50_ns > limit || gate.is_some_and(|g| stats.p50_ns > g.limit_ns),
        });
        let margin = limit / stats.p50_ns;
        let blueprint = match (budget.blueprint_ns, &budget.blueprint) {
            (Some(ns), Some(text)) => {
                format!("{text} (this host: {:.2}x the target)", stats.p50_ns / ns)
            }
            (None, Some(text)) => text.clone(),
            _ => String::new(),
        };
        let vs_base = gate.map_or_else(
            || "-".to_owned(),
            |g| format!("{:.2}x", stats.p50_ns / g.baseline_ns),
        );
        eprintln!(
            "{:<46} {:>10} {:>10} {:>10} {:>6.1}x {:>8}  {}",
            workload.name,
            human(stats.p50_ns),
            human(stats.p90_ns),
            human(limit),
            margin,
            vs_base,
            blueprint
        );
        if stats.p50_ns > limit {
            failures.push(format!(
                "{}: p50 {} is over its budget of {} ({} iterations, best of up to {ATTEMPTS})",
                workload.name,
                human(stats.p50_ns),
                human(limit),
                stats.iterations
            ));
        }
        if let Some(p99_limit) = p99_limit {
            eprintln!(
                "    p99 {} of {} ({} iterations)",
                human(stats.p99_ns),
                human(p99_limit),
                stats.iterations
            );
            if stats.p99_ns > p99_limit {
                failures.push(format!(
                    "{}: p99 {} is over its ceiling of {} ({} iterations, best of up to {ATTEMPTS})",
                    workload.name,
                    human(stats.p99_ns),
                    human(p99_limit),
                    stats.iterations
                ));
            }
        }
        if let (Some(g), Some(b)) = (gate, &baseline) {
            if stats.p50_ns > g.limit_ns {
                failures.push(format!(
                    "{}: p50 {} is {:.2}x its baseline of {} ({}); the gate is {}x ({} iterations, \
                     best of up to {ATTEMPTS})",
                    workload.name,
                    human(stats.p50_ns),
                    stats.p50_ns / g.baseline_ns,
                    human(g.baseline_ns),
                    b.path.display(),
                    g.tolerance,
                    stats.iterations
                ));
            }
        } else if let Some(b) = &baseline {
            eprintln!(
                "    notice: {} has no row in {}: not gated against the baseline (record it again)",
                workload.name,
                b.path.display()
            );
        }
    }

    // Ratios between two rows of this run: the machine's speed cancels. A failing ratio gets the
    // attempts the rows themselves get: both rows are measured again and each keeps its best.
    if !budgets.ratios.is_empty() {
        eprintln!(
            "{:<26} {:>8} {:>8} {:>7}  rows",
            "ratio", "value", "max", "margin"
        );
    }
    for (name, ratio) in &budgets.ratios {
        let (Some(&(mut num)), Some(&(mut den))) = (p50s.get(&ratio.num), p50s.get(&ratio.den))
        else {
            continue;
        };
        let mut attempt = 1;
        while !ratio.holds(ratio.value(num, den)) && attempt < ATTEMPTS {
            attempt += 1;
            for (row, p50) in [(&ratio.num, &mut num), (&ratio.den, &mut den)] {
                if let Some(workload) = workloads.iter().find(|w| &w.name == row) {
                    let again = best_of(workload, 1, f64::MAX, None);
                    *p50 = p50.min(again.p50_ns);
                }
            }
        }
        let value = ratio.value(num, den);
        ratio_results.push(RatioResult {
            name: name.clone(),
            num: ratio.num.clone(),
            den: ratio.den.clone(),
            value,
            max: ratio.max,
            attempts: attempt,
            holds: ratio.holds(value),
        });
        eprintln!(
            "{:<26} {:>8.2} {:>8.2} {:>6.2}x  {} / {}{}",
            name,
            value,
            ratio.max,
            ratio.max / value,
            ratio.num,
            ratio.den,
            if attempt > 1 {
                format!("  (attempt {attempt})")
            } else {
                String::new()
            }
        );
        if !ratio.holds(value) {
            failures.push(format!(
                "ratio {name}: {} / {} is {value:.2}, over its maximum of {} (best of {attempt} \
                 attempts; the machine's speed cancels in a ratio, so a relationship between two \
                 operations changed)",
                ratio.num, ratio.den, ratio.max
            ));
        }
    }

    if let Some(path) = &recording {
        record(path, &p50s, load_before);
    }
    if let Some(dir) = undra_bench::results::dir_from_env() {
        write_results(
            &dir,
            &rows,
            &ratio_results,
            baseline.as_ref(),
            (scale, load_before, hostinfo::load_average()),
        );
    }
    assert!(
        failures.is_empty(),
        "{} budget(s) exceeded:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// One layer A row of a run, for the results file.
struct RowResult {
    name: String,
    p50_ns: f64,
    p90_ns: f64,
    p99_ns: f64,
    budget_ns: f64,
    baseline_ns: Option<f64>,
    iterations: u64,
    /// Over its absolute budget or over its baseline's gate.
    failed: bool,
}

/// One ratio gate of a run, for the results file.
struct RatioResult {
    name: String,
    num: String,
    den: String,
    value: f64,
    max: f64,
    attempts: usize,
    holds: bool,
}

/// Writes the JSON behind the run: `<dir>/<date>[-<tag>]-layer-a.json`.
fn write_results(
    dir: &Path,
    rows: &[RowResult],
    ratios: &[RatioResult],
    baseline: Option<&Selected>,
    (scale, load_before, load_after): (f64, Option<f64>, Option<f64>),
) {
    use undra_bench::results::{
        command, json_number, json_opt, json_string, machine_members, path_for, tag_from_env, write,
    };
    let (date, tag) = (hostinfo::date(), tag_from_env());
    let how = command(
        "cargo test -p undra-bench --test budgets --release -- --nocapture",
        &[
            "UNDRA_BENCH_RESULTS_TAG",
            "UNDRA_BENCH_SCALE",
            "UNDRA_BENCH_FILTER",
            "UNDRA_BENCH_BASELINE",
            "UNDRA_BENCH_BASELINE_TOLERANCE",
        ],
    );
    let rows_json: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "    {{\"name\": {}, \"p50_ns\": {}, \"p90_ns\": {}, \"p99_ns\": {}, \"budget_ns\": {}, \"baseline_p50_ns\": {}, \"iterations\": {}, \"failed\": {}}}",
                json_string(&r.name),
                json_number(r.p50_ns),
                json_number(r.p90_ns),
                json_number(r.p99_ns),
                json_number(r.budget_ns),
                json_opt(r.baseline_ns),
                r.iterations,
                r.failed
            )
        })
        .collect();
    let ratios_json: Vec<String> = ratios
        .iter()
        .map(|r| {
            format!(
                "    {{\"name\": {}, \"num\": {}, \"den\": {}, \"value\": {}, \"max\": {}, \"attempts\": {}, \"holds\": {}}}",
                json_string(&r.name),
                json_string(&r.num),
                json_string(&r.den),
                json_number(r.value),
                json_number(r.max),
                r.attempts,
                r.holds
            )
        })
        .collect();
    let text = format!(
        "{{\n  \"kind\": \"layer-a\",\n  \"date\": {},\n  \"tag\": {},\n  \"command\": {},\n  {},\n  \"scale\": {},\n  \"baseline\": {},\n  \"rows\": [\n{}\n  ],\n  \"ratios\": [\n{}\n  ]\n}}\n",
        json_string(&date),
        tag.as_deref()
            .map_or_else(|| "null".to_owned(), json_string),
        json_string(&how),
        machine_members(load_before, load_after),
        json_number(scale),
        baseline.map_or_else(
            || "null".to_owned(),
            |b| json_string(&b.path.display().to_string())
        ),
        rows_json.join(",\n"),
        ratios_json.join(",\n"),
    );
    write(&path_for(dir, &date, tag.as_deref(), "layer-a"), &text);
    eprintln!("wrote {} rows to {}", rows.len(), dir.display());
}

/// Writes what this run measured as a baseline, next to the facts about the machine.
fn record(path: &Path, p50s: &BTreeMap<String, f64>, load_before: Option<f64>) {
    let mut baseline = Baseline::default();
    let load = |l: Option<f64>| l.map_or_else(|| "?".to_owned(), |l| format!("{l:.2}"));
    for (key, value) in [
        ("cpu", hostinfo::cpu()),
        ("cores", hostinfo::cores().to_string()),
        ("os", hostinfo::os()),
        ("rustc", hostinfo::rustc()),
        ("date", hostinfo::date()),
        ("git", hostinfo::git_revision()),
        (
            "load_layer_a",
            format!(
                "{} before, {} after (one-minute load average)",
                load(load_before),
                load(hostinfo::load_average())
            ),
        ),
        (
            "recorded_by",
            format!("budgets test, best p50 of {ATTEMPTS} attempts per row"),
        ),
        (
            "command_layer_a",
            undra_bench::results::command(
                "cargo test -p undra-bench --test budgets --release",
                &[
                    "UNDRA_BENCH_RECORD",
                    "UNDRA_BENCH_RECORD_BEST",
                    "UNDRA_BENCH_FILTER",
                ],
            ),
        ),
    ] {
        baseline.meta.insert(key.to_owned(), value);
    }
    for (name, p50) in p50s {
        baseline.benches.insert(
            name.clone(),
            BenchBaseline {
                p50_ns: *p50,
                tolerance: None,
            },
        );
    }
    let rows = baseline.benches.len();
    if let Err(e) = baseline.record_into(path, undra_bench::baseline::record_keeps_best()) {
        panic!("cannot write UNDRA_BENCH_RECORD to {}: {e}", path.display());
    }
    eprintln!("recorded {rows} rows to {}", path.display());
}

// ---------------------------------------------------------------------------------------------
// The ratio gates against recorded samples
// ---------------------------------------------------------------------------------------------

/// Layer A p50s, nanoseconds per iteration, of the rows the ratio gates use, in the order of
/// [`SAMPLE_ROWS`]: the reference host as `budgets.toml` recorded it (a loaded machine), the same
/// host in a quiet run of 2026-09-30, and twelve runs of the Bench workflow on a GitHub
/// `ubuntu-latest` runner (Oct 2026, read from their logs with `gh run view <id> --log`; each is
/// named by its run id). The first six set the gates; the last six (36800899301 on) came after
/// and are the out-of-sample check, which `fanout_100k_vs_10k_observed` failed at its first
/// maximum (2.37 against 2.3 in 36806665630, a docs-only commit). The pool is not one machine
/// class: the same row differs by up to 2.2x between those runs, which is why the ratios are the
/// gate there.
const SAMPLE_ROWS: [&str; 7] = [
    "stress/firehose/txn_x1000",
    "dispatch/call_sync/add",
    "stress/firehose/call_set",
    "stress/firehose/event",
    "stress/fanout/100k_observed_1k_dirty",
    "stress/fanout/10k_observed_1k_dirty",
    "stress/keyed_churn_10k/ops_x1000",
];

const SAMPLES: [(&str, [f64; 7]); 14] = [
    (
        "host, as budgets.toml recorded it",
        [
            78_812.5,
            42.9,
            124.7,
            115.5,
            37_611.1,
            25_644.6,
            5_847_000.0,
        ],
    ),
    (
        "host, quiet run",
        [
            78_160.0,
            44.6,
            136.2,
            115.8,
            30_740.0,
            25_190.0,
            5_380_000.0,
        ],
    ),
    (
        "runner 36798191863",
        [
            143_150.0,
            108.5,
            327.1,
            279.1,
            131_910.0,
            68_540.0,
            7_560_000.0,
        ],
    ),
    (
        "runner 36798621059",
        [
            143_410.0,
            108.8,
            287.1,
            222.0,
            131_290.0,
            68_810.0,
            7_600_000.0,
        ],
    ),
    (
        "runner 36799125443",
        [
            275_200.0,
            151.1,
            429.0,
            380.4,
            179_880.0,
            113_670.0,
            6_460_000.0,
        ],
    ),
    (
        "runner 36799240121",
        [
            122_800.0,
            97.8,
            225.1,
            182.9,
            88_640.0,
            52_990.0,
            5_350_000.0,
        ],
    ),
    (
        "runner 36799876008",
        [
            145_960.0,
            104.5,
            258.4,
            195.7,
            95_090.0,
            57_960.0,
            5_580_000.0,
        ],
    ),
    (
        "runner 36800063172",
        [
            253_230.0,
            130.7,
            387.3,
            344.7,
            106_300.0,
            106_840.0,
            6_950_000.0,
        ],
    ),
    (
        "runner 36800899301",
        [
            144_350.0,
            108.9,
            274.8,
            225.4,
            105_330.0,
            67_870.0,
            7_490_000.0,
        ],
    ),
    (
        "runner 36803857331",
        [
            142_140.0,
            110.0,
            276.8,
            225.0,
            102_780.0,
            68_300.0,
            7_520_000.0,
        ],
    ),
    (
        "runner 36804070521",
        [
            157_390.0,
            124.3,
            345.0,
            281.5,
            95_960.0,
            68_470.0,
            6_420_000.0,
        ],
    ),
    (
        "runner 36806665630",
        [
            134_630.0,
            105.3,
            236.7,
            208.7,
            127_880.0,
            54_060.0,
            5_370_000.0,
        ],
    ),
    (
        "runner 36808385622",
        [
            275_860.0,
            151.2,
            428.6,
            381.8,
            171_390.0,
            114_180.0,
            6_470_000.0,
        ],
    ),
    (
        "runner 36808470795",
        [
            157_950.0,
            127.6,
            286.9,
            237.1,
            97_580.0,
            68_870.0,
            6_330_000.0,
        ],
    ),
];

fn sample_map(values: &[f64; 7]) -> BTreeMap<String, f64> {
    SAMPLE_ROWS
        .iter()
        .zip(values)
        .map(|(row, v)| ((*row).to_owned(), *v))
        .collect()
}

/// Which ratio gates of `budgets` fail on `p50s`.
fn failing_ratios(budgets: &Budgets, p50s: &BTreeMap<String, f64>) -> Vec<String> {
    budgets
        .ratios
        .iter()
        .filter_map(|(name, ratio)| {
            let value = ratio.value(*p50s.get(&ratio.num)?, *p50s.get(&ratio.den)?);
            (!ratio.holds(value)).then(|| format!("{name} = {value:.2} (max {})", ratio.max))
        })
        .collect()
}

/// A commit path `factor` times slower, modelled on a sample: a transaction costs `factor` times
/// as much (`txn_x1000` is the commit and delivery of 1,000 of them), and the two rows that make
/// one commit per operation pay the difference once.
fn with_slower_commit(p50s: &BTreeMap<String, f64>, factor: f64) -> BTreeMap<String, f64> {
    let mut slower = p50s.clone();
    let per_txn = p50s["stress/firehose/txn_x1000"] / 1000.0;
    *slower.get_mut("stress/firehose/txn_x1000").unwrap() *= factor;
    for row in ["stress/firehose/call_set", "stress/firehose/event"] {
        *slower.get_mut(row).unwrap() += (factor - 1.0) * per_txn;
    }
    slower
}

#[test]
fn ratios_hold_on_every_recorded_sample() {
    // The reference host's loaded and quiet runs and twelve runs on the CI runner pool: every
    // ratio gate passes on every one. That is evidence, not proof: a `max` holds on the runner
    // only as long as the next run's hardware stays inside the spread these samples show (the
    // fan-out ratio did not, at its first maximum: see SAMPLES).
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let budgets = load_budgets();
    for (name, values) in SAMPLES {
        let failing = failing_ratios(&budgets, &sample_map(&values));
        assert!(failing.is_empty(), "{name}: {failing:?}");
    }
}

#[test]
fn a_commit_twice_as_slow_fails_the_commit_ratio_on_every_recorded_sample() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let budgets = load_budgets();
    for (name, values) in SAMPLES {
        let slower = with_slower_commit(&sample_map(&values), 2.0);
        let failing = failing_ratios(&budgets, &slower);
        assert!(
            failing.iter().any(|f| f.starts_with("commit_vs_bare_call")),
            "{name}: a 2x commit passed the commit ratio: {failing:?}"
        );
    }
}

#[test]
fn a_commit_1_8_times_as_slow_fails_the_commit_ratio_on_the_host_and_most_runners() {
    // The review's experiment (a commit path made 1.8x slower) on the recorded samples. Three
    // runner runs of the fast class pass: their commit is the cheapest against their bare call
    // (a ratio of 1.24 to 1.27), so 1.8x lands at 2.23 to 2.28 against a maximum of 2.3. A ratio
    // gate can only see a shift larger than the spread of the ratio across hardware, and this is
    // where that spread ends; the baseline of the base commit, measured on the same VM, is what
    // catches 1.8x there (`txn_x1000` alone reads 1.8x its base, over the 1.5x gate).
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let budgets = load_budgets();
    let mut passed = Vec::new();
    for (name, values) in SAMPLES {
        let slower = with_slower_commit(&sample_map(&values), 1.8);
        let failing = failing_ratios(&budgets, &slower);
        if !failing.iter().any(|f| f.starts_with("commit_vs_bare_call")) {
            passed.push(name);
        }
    }
    assert_eq!(
        passed,
        [
            "runner 36799240121",
            "runner 36804070521",
            "runner 36808470795"
        ],
        "{passed:?}"
    );
}

/// Not a test: prints measurements as `budgets.toml` tables.
#[test]
#[ignore = "prints a baseline; run it explicitly with --ignored --nocapture"]
fn baseline() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let existing = Budgets::load(&budgets_path()).unwrap_or_default();
    let factor: f64 = std::env::var("UNDRA_BENCH_FACTOR")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(5.0);
    for workload in selected(common::workloads::all()) {
        let stats = best_of(&workload, 1, f64::MAX, None);
        let budget = round_up(stats.p50_ns * factor);
        println!("[bench.\"{}\"]", workload.name);
        println!("budget_ns = {budget}");
        println!("measured_ns = {:.1}", stats.p50_ns);
        println!(
            "# p99 {:.1} (a row whose claim is its tail takes p99_ns, 10x this, and measured_p99_ns)",
            stats.p99_ns
        );
        if let Some(old) = existing.benches.get(&workload.name) {
            if let Some(ns) = old.blueprint_ns {
                println!("blueprint_ns = {ns}");
            }
            if let Some(text) = &old.blueprint {
                println!("blueprint = \"{text}\"");
            }
        }
        println!();
    }
}

/// Two significant figures, rounded up: 1,337 becomes 1,400, so a budget reads as a budget.
fn round_up(ns: f64) -> u64 {
    let ns = ns.max(1.0).ceil() as u64;
    let digits = ns.ilog10();
    if digits < 2 {
        return ns.div_ceil(10) * 10;
    }
    let step = 10_u64.pow(digits - 1);
    ns.div_ceil(step) * step
}
