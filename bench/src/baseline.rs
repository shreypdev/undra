//! Per-environment baselines: gates that catch a 2x regression on a given machine without
//! depending on how fast that machine is.
//!
//! The absolute budgets of `budgets.toml` are set at 5x what the reference host measures, so that
//! a slower runner passes. That makes them blunt: a commit path made 1.8x slower passes every one
//! of them. A *baseline* is the other half: what **this machine class** measured for every layer A
//! row and every sustained scenario, recorded from a quiet run, and a gate that fails when a run
//! on the same class is more than 1.5x worse (1.5x slower p50 or 20 ns more where that is more, a
//! throughput under 1/1.5 of it, a p99 over 2.5x it). The absolute budgets stay as the outer
//! guard.
//!
//! Select one with `UNDRA_BENCH_BASELINE`:
//!
//! * a bare name, `apple-m5-pro`, reads `bench/baselines/apple-m5-pro.toml`;
//! * a path (anything with a `/` or ending in `.toml`) reads that file, which is how CI gates a
//!   change against the base commit it measured **in the same job, on the same VM** minutes
//!   earlier (`scripts/bench-record-base.sh`): the machine's speed cancels by construction,
//!   whatever hardware the runner pool hands out;
//! * unset (or `off`): no baseline gate, and the run says so.
//!
//! A name or path that does not exist is an error, never a silent skip. Record one with
//! `UNDRA_BENCH_RECORD=path` on the budgets test and on the stress test (each adds its own
//! tables to the file), best of three attempts per row, on a quiet machine. A shared machine is
//! never quite quiet: record a few times at different moments with `UNDRA_BENCH_RECORD_BEST=1`
//! and the file keeps, row by row, the best of all of them.
//!
//! The file is the same strict TOML subset as `budgets.toml`:
//!
//! ```toml
//! [meta]
//! cpu = "Apple M5 Pro"                # free-form strings, for humans (see `tolerance` below)
//! tolerance = 1.5                     # optional: p50 and throughput factor (default 1.5)
//! tail_tolerance = 2.5                # optional: p99 factor (default 2.5)
//!
//! [bench."stress/firehose/txn_x1000"] # a layer A row
//! p50_ns = 78160.0                    # required
//! tolerance = 3.0                     # optional: this row only (a thread-wake-bound row, say)
//!
//! [stress."firehose/sustained"]       # a sustained scenario
//! per_sec = 5928807                   # required: the throughput floor is this / tolerance
//! p99_ns = 211                        # optional: the ceiling is this * tail_tolerance
//! p999_ns = 295                       # recorded for humans, not gated (too noisy at 2 s)
//! ```
//!
//! `UNDRA_BENCH_BASELINE_TOLERANCE` and `UNDRA_BENCH_BASELINE_TAIL_TOLERANCE` override the two
//! factors for one run. `UNDRA_BENCH_SCALE` does **not** apply to a baseline: it exists for a
//! machine class that has no baseline, and a baseline already is that class.
//!
//! A row's own factor can also live in `budgets.toml` (`baseline_tolerance` in its `[bench."..."]`
//! or `[stress."..."]` table), which is the only place it can live for CI: that baseline is
//! recorded afresh in every job and carries no per-row factor. It gives the row **at least** that
//! factor ([`Selected::bench_gate_with`], [`Selected::stress_failures_with`]); a `tolerance` in
//! the baseline file's own row still wins, being about that machine class.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::budget::{StressObserved, Value, parse_string, parse_value, strip_comment};

/// The default factor for a layer A p50 and for a throughput floor: a 1.5x regression fails.
pub const DEFAULT_TOLERANCE: f64 = 1.5;
/// The default factor for a p99 ceiling: tails are noisier than medians.
pub const DEFAULT_TAIL_TOLERANCE: f64 = 2.5;
/// Nanoseconds a layer A row may exceed its baseline by whatever the factor says. A 14 ns row moves
/// by 5 ns with the code layout of another binary (the same crates, linked again), which is a 1.4x
/// "regression" of nothing; 20 ns is about sixty cycles, far under what any real regression of a
/// row this small costs.
pub const NOISE_FLOOR_NS: f64 = 20.0;

/// What one layer A row measured.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchBaseline {
    /// The p50, nanoseconds per iteration.
    pub p50_ns: f64,
    /// This row's own factor, when it needs more room than the file's.
    pub tolerance: Option<f64>,
}

/// What one sustained scenario measured.
#[derive(Clone, Debug, PartialEq)]
pub struct StressBaseline {
    /// Operations per second of wall time.
    pub per_sec: f64,
    /// The 99th percentile latency in nanoseconds, if the scenario times operations.
    pub p99_ns: Option<f64>,
    /// The 99.9th percentile, recorded for humans and not gated.
    pub p999_ns: Option<f64>,
    /// This scenario's own factor.
    pub tolerance: Option<f64>,
}

/// A parsed baseline file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Baseline {
    /// The `[meta]` table: the machine, the date, the load, the factors.
    pub meta: BTreeMap<String, String>,
    /// Layer A rows by workload name.
    pub benches: BTreeMap<String, BenchBaseline>,
    /// Sustained scenarios by name.
    pub stress: BTreeMap<String, StressBaseline>,
}

/// Why a baseline could not be read or selected.
#[derive(Clone, Debug, PartialEq)]
pub enum BaselineError {
    /// The file could not be read.
    Io {
        /// The path.
        path: String,
        /// What the operating system said.
        message: String,
    },
    /// A line is not part of the accepted subset.
    Syntax {
        /// One-based line number.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// A table lacks its required key (`p50_ns` for a bench row, `per_sec` for a scenario).
    Missing {
        /// The table, as written.
        table: String,
        /// The missing key.
        key: &'static str,
    },
    /// Two tables have the same header.
    Duplicate {
        /// The table, as written.
        table: String,
    },
    /// A factor below 1.0 or not a number: it would fail an unchanged run.
    BadTolerance {
        /// Where it came from.
        source: String,
        /// What it said.
        value: String,
    },
}

impl fmt::Display for BaselineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaselineError::Io { path, message } => {
                write!(f, "cannot read the baseline {path}: {message}")
            }
            BaselineError::Syntax { line, message } => {
                write!(f, "baseline file, line {line}: {message}")
            }
            BaselineError::Missing { table, key } => {
                write!(f, "baseline file: {table} has no {key}")
            }
            BaselineError::Duplicate { table } => {
                write!(f, "baseline file: {table} appears twice")
            }
            BaselineError::BadTolerance { source, value } => write!(
                f,
                "{source} must be a number of at least 1.0 (a factor over the baseline), not `{value}`"
            ),
        }
    }
}

impl std::error::Error for BaselineError {}

enum Section {
    None,
    Meta,
    Bench(String),
    Stress(String),
}

#[derive(Default)]
struct Partial {
    p50_ns: Option<f64>,
    per_sec: Option<f64>,
    p99_ns: Option<f64>,
    p999_ns: Option<f64>,
    tolerance: Option<f64>,
}

fn check_factor(source: &str, value: f64) -> Result<f64, BaselineError> {
    if value.is_finite() && value >= 1.0 {
        Ok(value)
    } else {
        Err(BaselineError::BadTolerance {
            source: source.to_owned(),
            value: value.to_string(),
        })
    }
}

impl Baseline {
    /// Reads and parses the file at `path`.
    pub fn load(path: &Path) -> Result<Baseline, BaselineError> {
        let text = std::fs::read_to_string(path).map_err(|e| BaselineError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        Baseline::parse(&text)
    }

    /// Parses the text of a baseline file.
    ///
    /// # Example
    ///
    /// ```
    /// use undra_bench::baseline::Baseline;
    ///
    /// let b = Baseline::parse(
    ///     "[meta]\ncpu = \"x\"\n[bench.\"a/b\"]\np50_ns = 80.5\n[stress.\"s\"]\nper_sec = 1000\n",
    /// )
    /// .unwrap();
    /// assert_eq!(b.benches["a/b"].p50_ns, 80.5);
    /// assert_eq!(b.stress["s"].per_sec, 1000.0);
    /// ```
    pub fn parse(text: &str) -> Result<Baseline, BaselineError> {
        let mut baseline = Baseline::default();
        let mut section = Section::None;
        let mut partials: BTreeMap<String, Partial> = BTreeMap::new();
        let mut order: Vec<(bool, String)> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let syntax = |message: &str| BaselineError::Syntax {
                line,
                message: message.to_owned(),
            };
            let trimmed = strip_comment(raw).trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(header) = trimmed.strip_prefix('[') {
                let header = header
                    .strip_suffix(']')
                    .ok_or_else(|| syntax("a table header must end with `]`"))?;
                section = if header == "meta" {
                    Section::Meta
                } else if let Some(name) = header.strip_prefix("bench.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a bench table is written [bench.\"name\"]"))?;
                    let key = format!("b:{name}");
                    if partials.insert(key, Partial::default()).is_some() {
                        return Err(BaselineError::Duplicate {
                            table: format!("[bench.\"{name}\"]"),
                        });
                    }
                    order.push((true, name.clone()));
                    Section::Bench(name)
                } else if let Some(name) = header.strip_prefix("stress.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a stress table is written [stress.\"name\"]"))?;
                    let key = format!("s:{name}");
                    if partials.insert(key, Partial::default()).is_some() {
                        return Err(BaselineError::Duplicate {
                            table: format!("[stress.\"{name}\"]"),
                        });
                    }
                    order.push((false, name.clone()));
                    Section::Stress(name)
                } else {
                    return Err(syntax(
                        "the only tables are [meta], [bench.\"name\"] and [stress.\"name\"]",
                    ));
                };
                continue;
            }
            let (key, value) = trimmed
                .split_once('=')
                .ok_or_else(|| syntax("expected `key = value`"))?;
            let key = key.trim();
            let value = parse_value(value.trim())
                .ok_or_else(|| syntax("a value is a number or a \"double quoted\" string"))?;
            match &section {
                Section::None => return Err(syntax("a key must be inside a table")),
                Section::Meta => {
                    let text = match value {
                        Value::Text(t) => t,
                        Value::Number(n) => n.to_string(),
                    };
                    baseline.meta.insert(key.to_owned(), text);
                }
                Section::Bench(name) | Section::Stress(name) => {
                    let is_bench = matches!(section, Section::Bench(_));
                    let slot = format!("{}:{name}", if is_bench { "b" } else { "s" });
                    let entry = partials.entry(slot).or_default();
                    let Value::Number(n) = value else {
                        return Err(syntax("a baseline table takes numbers only"));
                    };
                    match (is_bench, key) {
                        (true, "p50_ns") => entry.p50_ns = Some(n),
                        (false, "per_sec") => entry.per_sec = Some(n),
                        (false, "p99_ns") => entry.p99_ns = Some(n),
                        (false, "p999_ns") => entry.p999_ns = Some(n),
                        (_, "tolerance") => {
                            entry.tolerance = Some(
                                check_factor("tolerance", n)
                                    .map_err(|_| syntax("tolerance must be at least 1.0"))?,
                            );
                        }
                        (true, _) => {
                            return Err(syntax("a bench table takes p50_ns and tolerance"));
                        }
                        (false, _) => {
                            return Err(syntax(
                                "a stress table takes per_sec, p99_ns, p999_ns and tolerance",
                            ));
                        }
                    }
                }
            }
        }
        for (is_bench, name) in order {
            let slot = format!("{}:{name}", if is_bench { "b" } else { "s" });
            let partial = partials.remove(&slot).unwrap_or_default();
            if is_bench {
                let p50_ns = partial.p50_ns.ok_or_else(|| BaselineError::Missing {
                    table: format!("[bench.\"{name}\"]"),
                    key: "p50_ns",
                })?;
                baseline.benches.insert(
                    name,
                    BenchBaseline {
                        p50_ns,
                        tolerance: partial.tolerance,
                    },
                );
            } else {
                let per_sec = partial.per_sec.ok_or_else(|| BaselineError::Missing {
                    table: format!("[stress.\"{name}\"]"),
                    key: "per_sec",
                })?;
                baseline.stress.insert(
                    name,
                    StressBaseline {
                        per_sec,
                        p99_ns: partial.p99_ns,
                        p999_ns: partial.p999_ns,
                        tolerance: partial.tolerance,
                    },
                );
            }
        }
        Ok(baseline)
    }

    /// The file's text: `[meta]`, then every row, each in name order, so a re-recording diffs
    /// only where a number moved.
    pub fn to_text(&self) -> String {
        fn quote(text: &str) -> String {
            format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let mut out = String::from("[meta]\n");
        for (key, value) in &self.meta {
            let numeric = value.parse::<f64>().is_ok() && !value.is_empty();
            if numeric && matches!(key.as_str(), "tolerance" | "tail_tolerance") {
                out.push_str(&format!("{key} = {value}\n"));
            } else {
                out.push_str(&format!("{key} = {}\n", quote(value)));
            }
        }
        for (name, row) in &self.benches {
            out.push_str(&format!(
                "\n[bench.{}]\np50_ns = {:.1}\n",
                quote(name),
                row.p50_ns
            ));
            if let Some(t) = row.tolerance {
                out.push_str(&format!("tolerance = {t}\n"));
            }
        }
        for (name, row) in &self.stress {
            out.push_str(&format!(
                "\n[stress.{}]\nper_sec = {:.0}\n",
                quote(name),
                row.per_sec
            ));
            if let Some(p99) = row.p99_ns {
                out.push_str(&format!("p99_ns = {p99:.0}\n"));
            }
            if let Some(p999) = row.p999_ns {
                out.push_str(&format!("p999_ns = {p999:.0}\n"));
            }
            if let Some(t) = row.tolerance {
                out.push_str(&format!("tolerance = {t}\n"));
            }
        }
        out
    }

    /// Adds `other`'s rows (replacing same-named ones) and `other`'s meta keys to this baseline.
    pub fn overlay(&mut self, other: Baseline) {
        self.meta.extend(other.meta);
        self.benches.extend(other.benches);
        self.stress.extend(other.stress);
    }

    /// Like [`overlay`](Baseline::overlay), but a row already here **stays unless `other`'s is
    /// better**: a lower p50, a higher throughput, a lower p99. Recording a few times at
    /// different moments with this keeps the best of all of them, which is what a baseline on a
    /// shared machine should be: noise only ever makes a run worse.
    pub fn overlay_best(&mut self, other: Baseline) {
        for (name, row) in other.benches {
            match self.benches.get_mut(&name) {
                Some(mine) if mine.p50_ns <= row.p50_ns => {}
                Some(mine) => mine.p50_ns = row.p50_ns,
                None => {
                    self.benches.insert(name, row);
                }
            }
        }
        for (name, row) in other.stress {
            match self.stress.get_mut(&name) {
                Some(mine) => {
                    mine.per_sec = mine.per_sec.max(row.per_sec);
                    mine.p99_ns = best_low(mine.p99_ns, row.p99_ns);
                    mine.p999_ns = best_low(mine.p999_ns, row.p999_ns);
                }
                None => {
                    self.stress.insert(name, row);
                }
            }
        }
        self.meta.extend(other.meta);
    }

    /// Writes this baseline over the file at `path`, **keeping what the file already holds** that
    /// this baseline does not mention: the budgets test and the stress test each record their own
    /// tables into one file, one after the other. With `keep_best`, rows the file holds stay
    /// where they are better than these (see [`overlay_best`](Baseline::overlay_best)).
    pub fn record_into(self, path: &Path, keep_best: bool) -> std::io::Result<()> {
        let mut merged = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| Baseline::parse(&text).ok())
            .unwrap_or_default();
        if keep_best {
            merged.overlay_best(self);
        } else {
            merged.overlay(self);
        }
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, merged.to_text())
    }
}

/// The best of several runs of one scenario: the highest throughput, the lowest tails (each
/// metric on its own, as a recording keeps them). `None` for no runs.
pub fn best_of(runs: &[StressObserved]) -> Option<StressObserved> {
    let (first, rest) = runs.split_first()?;
    let mut best = StressObserved {
        per_sec: first.per_sec,
        p99_ns: first.p99_ns,
        p999_ns: first.p999_ns,
        ..StressObserved::default()
    };
    for run in rest {
        best.per_sec = best.per_sec.max(run.per_sec);
        best.p99_ns = best_low(best.p99_ns, run.p99_ns);
        best.p999_ns = best_low(best.p999_ns, run.p999_ns);
    }
    Some(best)
}

fn best_low(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Whether `UNDRA_BENCH_RECORD_BEST` asks a recording to keep the better of what the file holds
/// and what this run measured, rather than replace it.
pub fn record_keeps_best() -> bool {
    std::env::var_os("UNDRA_BENCH_RECORD_BEST").is_some_and(|v| !v.is_empty() && v != "0")
}

/// A baseline chosen for this run, with the factors that apply.
#[derive(Clone, Debug)]
pub struct Selected {
    /// What `UNDRA_BENCH_BASELINE` said (a name or a path).
    pub label: String,
    /// The file it resolved to.
    pub path: PathBuf,
    /// The rows.
    pub baseline: Baseline,
    /// The factor for p50s and throughput floors.
    pub tolerance: f64,
    /// The factor for p99 ceilings.
    pub tail_tolerance: f64,
}

/// What a layer A row must meet against its baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BenchGate {
    /// The p50 the baseline recorded, nanoseconds.
    pub baseline_ns: f64,
    /// The factor that applies.
    pub tolerance: f64,
    /// `baseline_ns * tolerance`, or `baseline_ns + NOISE_FLOOR_NS` where that is more (rows of a
    /// few tens of nanoseconds): a p50 above this fails.
    pub limit_ns: f64,
}

impl Selected {
    /// Resolves `value` (a name or a path) against `manifest_dir/baselines/` and loads it, with
    /// the factors from the file's `[meta]` and the two overrides (`None` for "not set").
    pub fn resolve(
        value: &str,
        manifest_dir: &Path,
        tolerance_override: Option<&str>,
        tail_override: Option<&str>,
    ) -> Result<Selected, BaselineError> {
        let path = if value.contains('/') || value.ends_with(".toml") {
            PathBuf::from(value)
        } else {
            manifest_dir.join("baselines").join(format!("{value}.toml"))
        };
        let baseline = Baseline::load(&path)?;
        let from_meta = |key: &str, default: f64| -> Result<f64, BaselineError> {
            match baseline.meta.get(key) {
                Some(text) => text
                    .parse::<f64>()
                    .map_err(|_| BaselineError::BadTolerance {
                        source: format!("[meta] {key} of {}", path.display()),
                        value: text.clone(),
                    })
                    .and_then(|v| check_factor(&format!("[meta] {key} of {}", path.display()), v)),
                None => Ok(default),
            }
        };
        let resolve = |env_value: Option<&str>, name: &str, key: &str, default: f64| match env_value
        {
            Some(text) => text
                .parse::<f64>()
                .map_err(|_| BaselineError::BadTolerance {
                    source: name.to_owned(),
                    value: text.to_owned(),
                })
                .and_then(|v| check_factor(name, v)),
            None => from_meta(key, default),
        };
        let tolerance = resolve(
            tolerance_override,
            "UNDRA_BENCH_BASELINE_TOLERANCE",
            "tolerance",
            DEFAULT_TOLERANCE,
        )?;
        let tail_tolerance = resolve(
            tail_override,
            "UNDRA_BENCH_BASELINE_TAIL_TOLERANCE",
            "tail_tolerance",
            DEFAULT_TAIL_TOLERANCE,
        )?;
        Ok(Selected {
            label: value.to_owned(),
            path,
            baseline,
            tolerance,
            tail_tolerance,
        })
    }

    /// The baseline `UNDRA_BENCH_BASELINE` selects, or `None` when it is unset (or `off`). A
    /// name or path that does not resolve is an error: a typo must not switch the gate off.
    pub fn from_env() -> Result<Option<Selected>, BaselineError> {
        let Ok(value) = std::env::var("UNDRA_BENCH_BASELINE") else {
            return Ok(None);
        };
        if value.is_empty() || value == "off" {
            return Ok(None);
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        Selected::resolve(
            &value,
            manifest,
            std::env::var("UNDRA_BENCH_BASELINE_TOLERANCE")
                .ok()
                .as_deref(),
            std::env::var("UNDRA_BENCH_BASELINE_TAIL_TOLERANCE")
                .ok()
                .as_deref(),
        )
        .map(Some)
    }

    /// The factor for a row: the baseline row's own, else at least `row_tolerance` (the row's
    /// `baseline_tolerance` in `budgets.toml`), else the run's.
    fn factor_for(&self, own: Option<f64>, row_tolerance: Option<f64>) -> f64 {
        own.unwrap_or_else(|| row_tolerance.map_or(self.tolerance, |t| t.max(self.tolerance)))
    }

    /// The gate for layer A row `name`, if the baseline has it.
    pub fn bench_gate(&self, name: &str) -> Option<BenchGate> {
        self.bench_gate_with(name, None)
    }

    /// Like [`bench_gate`](Selected::bench_gate), with `row_tolerance` (the row's
    /// `baseline_tolerance` in `budgets.toml`) as the least factor of a row the baseline file
    /// gives none of its own.
    pub fn bench_gate_with(&self, name: &str, row_tolerance: Option<f64>) -> Option<BenchGate> {
        let row = self.baseline.benches.get(name)?;
        let tolerance = self.factor_for(row.tolerance, row_tolerance);
        Some(BenchGate {
            baseline_ns: row.p50_ns,
            tolerance,
            limit_ns: (row.p50_ns * tolerance).max(row.p50_ns + NOISE_FLOOR_NS),
        })
    }

    /// How a sustained run compares with the baseline of scenario `name`: the sentences of every
    /// gate it missed (empty: it passed), or `None` when the baseline has no such scenario.
    pub fn stress_failures(&self, name: &str, observed: &StressObserved) -> Option<Vec<String>> {
        self.stress_failures_with(name, observed, None)
    }

    /// Like [`stress_failures`](Selected::stress_failures), with `row_tolerance` (the scenario's
    /// `baseline_tolerance` in `budgets.toml`) as the least factor of a scenario the baseline file
    /// gives none of its own. The p99 gets the larger of it and the tail factor.
    pub fn stress_failures_with(
        &self,
        name: &str,
        observed: &StressObserved,
        row_tolerance: Option<f64>,
    ) -> Option<Vec<String>> {
        let row = self.baseline.stress.get(name)?;
        let tolerance = self.factor_for(row.tolerance, row_tolerance);
        let mut failures = Vec::new();
        let floor = row.per_sec / tolerance;
        if observed.per_sec < floor {
            failures.push(format!(
                "throughput {:.0}/s is under {floor:.0}/s ({tolerance}x under the baseline's {:.0}/s)",
                observed.per_sec, row.per_sec
            ));
        }
        if let Some(p99) = row.p99_ns {
            let ceiling = p99 * self.tail_tolerance.max(tolerance);
            match observed.p99_ns {
                Some(got) if got <= ceiling => {}
                Some(got) => failures.push(format!(
                    "p99 {got:.0} ns is over {ceiling:.0} ns ({}x the baseline's {p99:.0} ns)",
                    self.tail_tolerance.max(tolerance)
                )),
                None => failures.push("p99 is in the baseline but was not measured".to_owned()),
            }
        }
        Some(failures)
    }

    /// The verdict of interleaved rounds: the base and the head of scenario `name` measured in
    /// turn on this machine (base, head, base, head, ...), the best of each side compared with the
    /// factors of [`stress_failures_with`](Selected::stress_failures_with): the highest throughput
    /// and the lowest p99 of the head against those of the base.
    ///
    /// A baseline recorded minutes before the head ran compares two moments of a shared runner as
    /// well as two trees: a slow stretch during the head alone fails a scenario whose code did not
    /// change. Interleaved, a slow stretch lowers whichever rounds it covers, on both sides, and
    /// the best of each side comes from its fastest moment; a real regression is slower in every
    /// round. `None` when a side has no sample or the baseline has no row for `name`.
    pub fn interleaved_stress_failures(
        &self,
        name: &str,
        base: &[StressObserved],
        head: &[StressObserved],
        row_tolerance: Option<f64>,
    ) -> Option<Vec<String>> {
        let recorded = self.baseline.stress.get(name)?;
        let best_base = best_of(base)?;
        let best_head = best_of(head)?;
        let mut interleaved = self.clone();
        interleaved.baseline.stress.insert(
            name.to_owned(),
            StressBaseline {
                per_sec: best_base.per_sec,
                p99_ns: best_base.p99_ns,
                p999_ns: best_base.p999_ns,
                tolerance: recorded.tolerance,
            },
        );
        interleaved.stress_failures_with(name, &best_head, row_tolerance)
    }

    /// A line for the top of a run: which baseline gates it and how.
    pub fn describe(&self) -> String {
        let meta = |key: &str| {
            self.baseline
                .meta
                .get(key)
                .map_or_else(|| "?".to_owned(), Clone::clone)
        };
        format!(
            "baseline {} ({}; recorded {}): p50 and throughput within {}x, p99 within {}x",
            self.path.display(),
            meta("cpu"),
            meta("date"),
            self.tolerance,
            self.tail_tolerance
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# a comment
[meta]
cpu = "Apple M5 Pro # not a comment"
tolerance = 1.4

[bench."stress/firehose/txn_x1000"]
p50_ns = 78_160.5
[bench."a/b"]
p50_ns = 40
tolerance = 3

[stress."firehose/sustained"]
per_sec = 5_928_807
p99_ns = 211
p999_ns = 295
[stress."stream/backpressure"]
per_sec = 28_000_000
"#;

    fn sample() -> Baseline {
        Baseline::parse(SAMPLE).expect("parses")
    }

    fn observed(per_sec: f64, p99: Option<f64>) -> StressObserved {
        StressObserved {
            per_sec,
            p99_ns: p99,
            ..StressObserved::default()
        }
    }

    #[test]
    fn parses_meta_rows_and_per_row_factors() {
        let b = sample();
        assert_eq!(b.meta["cpu"], "Apple M5 Pro # not a comment");
        assert_eq!(b.meta["tolerance"], "1.4");
        assert_eq!(b.benches["stress/firehose/txn_x1000"].p50_ns, 78_160.5);
        assert_eq!(b.benches["a/b"].tolerance, Some(3.0));
        let s = &b.stress["firehose/sustained"];
        assert_eq!(
            (s.per_sec, s.p99_ns, s.p999_ns),
            (5_928_807.0, Some(211.0), Some(295.0))
        );
        assert_eq!(b.stress["stream/backpressure"].p99_ns, None);
    }

    #[test]
    fn writing_and_reading_a_baseline_round_trips_it() {
        let text = sample().to_text();
        let again = Baseline::parse(&text).expect("what we write, we read");
        // The writer rounds (p50 to 0.1 ns, throughput to 1/s): the sample is already that coarse.
        assert_eq!(again, sample(), "{text}");
        assert!(text.starts_with("[meta]\n"), "{text}");
        // A tolerance in [meta] stays a bare number so it parses as one again.
        assert!(text.contains("tolerance = 1.4\n"), "{text}");
    }

    #[test]
    fn rejects_what_it_does_not_understand_with_a_line_number() {
        for (text, line) in [
            ("[meta]\ncpu = unquoted\n", 2),
            ("[other]\n", 1),
            ("p50_ns = 1\n", 1),
            ("[bench.\"a\"]\np50_ns = \"fast\"\n", 2),
            ("[bench.\"a\"]\nbudget_ns = 3\n", 2),
            ("[stress.\"a\"]\np50_ns = 3\n", 2),
            ("[bench.\"a\"]\ntolerance = 0.5\n", 2),
            ("[bench.a]\n", 1),
        ] {
            let err = Baseline::parse(text).unwrap_err();
            assert!(
                matches!(err, BaselineError::Syntax { line: l, .. } if l == line),
                "{text}: {err}"
            );
        }
    }

    #[test]
    fn a_table_needs_its_key_and_may_not_repeat() {
        let err = Baseline::parse("[bench.\"a\"]\ntolerance = 2\n").unwrap_err();
        assert_eq!(
            err,
            BaselineError::Missing {
                table: "[bench.\"a\"]".into(),
                key: "p50_ns"
            }
        );
        let err = Baseline::parse("[stress.\"a\"]\np99_ns = 2\n").unwrap_err();
        assert!(
            matches!(err, BaselineError::Missing { key: "per_sec", .. }),
            "{err}"
        );
        let err =
            Baseline::parse("[bench.\"a\"]\np50_ns = 1\n[bench.\"a\"]\np50_ns = 2\n").unwrap_err();
        assert!(matches!(err, BaselineError::Duplicate { .. }), "{err}");
        assert!(err.to_string().contains("appears twice"));
        // The same name as a bench row and as a scenario is two different things.
        Baseline::parse("[bench.\"a\"]\np50_ns = 1\n[stress.\"a\"]\nper_sec = 2\n").unwrap();
    }

    #[test]
    fn a_bench_row_fails_over_its_factor_and_a_row_factor_overrides_the_default() {
        let dir = tempdir("gate");
        std::fs::write(dir.join("m.toml"), SAMPLE).unwrap();
        let sel = Selected::resolve("m", &dir.parent().unwrap().join("nowhere"), None, None)
            .map(|_| ())
            .unwrap_err();
        assert!(
            matches!(sel, BaselineError::Io { .. }),
            "a bare name looks in baselines/"
        );
        let sel =
            Selected::resolve(dir.join("m.toml").to_str().unwrap(), &dir, None, None).unwrap();
        // [meta] tolerance = 1.4 is the default factor.
        let g = sel.bench_gate("stress/firehose/txn_x1000").unwrap();
        assert_eq!((g.baseline_ns, g.tolerance), (78_160.5, 1.4));
        assert!((g.limit_ns - 78_160.5 * 1.4).abs() < 1e-6);
        // A row of a few tens of nanoseconds gets the noise floor instead of 1.4x: 14 ns may
        // measure 34 ns in another binary's layout.
        std::fs::write(dir.join("tiny.toml"), "[bench.\"w\"]\np50_ns = 14\n").unwrap();
        let tiny =
            Selected::resolve(dir.join("tiny.toml").to_str().unwrap(), &dir, None, None).unwrap();
        assert_eq!(
            tiny.bench_gate("w").unwrap().limit_ns,
            14.0 + NOISE_FLOOR_NS
        );
        // A row's own factor wins.
        assert_eq!(sel.bench_gate("a/b").unwrap().tolerance, 3.0);
        assert!(sel.bench_gate("not/a/row").is_none());
    }

    #[test]
    fn interleaved_rounds_compare_the_best_of_each_side() {
        let dir = tempdir("interleaved");
        std::fs::write(dir.join("m.toml"), SAMPLE).unwrap();
        let sel =
            Selected::resolve(dir.join("m.toml").to_str().unwrap(), &dir, None, None).unwrap();
        let name = "stream/backpressure";
        let rounds = |rates: &[f64]| -> Vec<StressObserved> {
            rates.iter().map(|&r| observed(r, None)).collect()
        };
        // The same code on a runner whose speed swings: the recorded 28 M/s came from a fast moment and
        // the head's first try (12.4 M/s) from a slow one, but in turn each side has a fast round.
        let first_try = sel
            .stress_failures(name, &observed(12_400_000.0, None))
            .unwrap();
        assert_eq!(first_try.len(), 1, "{first_try:?}");
        let same = sel
            .interleaved_stress_failures(
                name,
                &rounds(&[19e6, 12e6, 13e6]),
                &rounds(&[12.5e6, 18.5e6, 12e6]),
                None,
            )
            .unwrap();
        assert!(same.is_empty(), "{same:?}");
        // A real 2x regression is slower in every round, however the runner swings.
        let slower = sel
            .interleaved_stress_failures(
                name,
                &rounds(&[19e6, 12e6, 13e6]),
                &rounds(&[9e6, 6e6, 6.5e6]),
                None,
            )
            .unwrap();
        assert!(
            slower.len() == 1 && slower[0].contains("throughput"),
            "{slower:?}"
        );
        // The scenario's own factor still applies, and nothing is judged without samples or a row.
        let roomy = sel
            .interleaved_stress_failures(name, &rounds(&[19e6]), &rounds(&[9e6]), Some(2.5))
            .unwrap();
        assert!(roomy.is_empty(), "{roomy:?}");
        assert_eq!(
            sel.interleaved_stress_failures(name, &[], &rounds(&[1.0]), None),
            None
        );
        assert_eq!(
            sel.interleaved_stress_failures(
                "not/a/scenario",
                &rounds(&[1.0]),
                &rounds(&[1.0]),
                None
            ),
            None
        );
    }

    #[test]
    fn the_best_of_runs_takes_each_metric_on_its_own() {
        let runs = [
            observed(10.0, Some(300.0)),
            observed(30.0, Some(500.0)),
            observed(20.0, None),
        ];
        let best = best_of(&runs).unwrap();
        assert_eq!((best.per_sec, best.p99_ns), (30.0, Some(300.0)));
        assert_eq!(best_of(&[]), None);
    }

    #[test]
    fn a_sustained_run_fails_on_throughput_or_p99() {
        let dir = tempdir("stress");
        std::fs::write(dir.join("m.toml"), SAMPLE).unwrap();
        let sel = Selected::resolve(
            dir.join("m.toml").to_str().unwrap(),
            &dir,
            Some("1.5"),
            None,
        )
        .unwrap();
        // Baseline 5.93 M/s, p99 211 ns: floor 3.95 M/s, ceiling 211 * 2.5 = 527 ns.
        let ok = sel
            .stress_failures("firehose/sustained", &observed(5_000_000.0, Some(300.0)))
            .unwrap();
        assert!(ok.is_empty(), "{ok:?}");
        let slow = sel
            .stress_failures("firehose/sustained", &observed(3_900_000.0, Some(300.0)))
            .unwrap();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!(slow[0].contains("throughput"), "{slow:?}");
        let tail = sel
            .stress_failures("firehose/sustained", &observed(5_900_000.0, Some(530.0)))
            .unwrap();
        assert!(tail.len() == 1 && tail[0].contains("p99"), "{tail:?}");
        let blind = sel
            .stress_failures("firehose/sustained", &observed(5_900_000.0, None))
            .unwrap();
        assert!(blind[0].contains("not measured"), "{blind:?}");
        assert!(
            sel.stress_failures("not/a/scenario", &observed(1.0, None))
                .is_none()
        );
    }

    #[test]
    fn a_row_factor_from_the_budgets_file_widens_a_row_the_baseline_gives_none() {
        // What CI needs: its baseline is recorded afresh in each job and carries no row factor,
        // so a row that proves noisy on the runner gets one from budgets.toml.
        let dir = tempdir("row-factor");
        std::fs::write(dir.join("m.toml"), SAMPLE).unwrap();
        let sel =
            Selected::resolve(dir.join("m.toml").to_str().unwrap(), &dir, None, None).unwrap();
        // [meta] tolerance = 1.4; the row has none of its own.
        let row = "stress/firehose/txn_x1000";
        assert_eq!(sel.bench_gate_with(row, None).unwrap().tolerance, 1.4);
        assert_eq!(sel.bench_gate_with(row, Some(2.0)).unwrap().tolerance, 2.0);
        // At least: a budgets.toml factor below the run's never tightens the row.
        assert_eq!(sel.bench_gate_with(row, Some(1.1)).unwrap().tolerance, 1.4);
        // The baseline file's own row factor is about that machine class, and wins.
        assert_eq!(
            sel.bench_gate_with("a/b", Some(5.0)).unwrap().tolerance,
            3.0
        );
        // A scenario: throughput floor 5.93 M/s / 2 = 2.96 M/s, p99 ceiling 211 * 2.5.
        let slow = observed(3_000_000.0, Some(300.0));
        assert_eq!(
            sel.stress_failures("firehose/sustained", &slow)
                .unwrap()
                .len(),
            1,
            "3.0 M/s is under 5.93 / 1.4"
        );
        assert!(
            sel.stress_failures_with("firehose/sustained", &slow, Some(2.0))
                .unwrap()
                .is_empty()
        );
        // The p99 gets the larger of the row factor and the tail factor.
        let tail = observed(5_900_000.0, Some(211.0 * 2.9));
        assert_eq!(
            sel.stress_failures_with("firehose/sustained", &tail, Some(2.0))
                .unwrap()
                .len(),
            1
        );
        assert!(
            sel.stress_failures_with("firehose/sustained", &tail, Some(3.0))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_factors_come_from_the_file_then_the_environment_and_must_be_sane() {
        let dir = tempdir("factors");
        std::fs::write(dir.join("m.toml"), SAMPLE).unwrap();
        let path = dir.join("m.toml");
        let path = path.to_str().unwrap();
        let sel = Selected::resolve(path, &dir, None, None).unwrap();
        assert_eq!(
            (sel.tolerance, sel.tail_tolerance),
            (1.4, DEFAULT_TAIL_TOLERANCE)
        );
        let sel = Selected::resolve(path, &dir, Some("2"), Some("4")).unwrap();
        assert_eq!((sel.tolerance, sel.tail_tolerance), (2.0, 4.0));
        for bad in ["0.9", "fast", "NaN", "inf"] {
            let err = Selected::resolve(path, &dir, Some(bad), None).unwrap_err();
            assert!(
                matches!(err, BaselineError::BadTolerance { .. }),
                "{bad}: {err}"
            );
            assert!(err.to_string().contains("at least 1.0"), "{err}");
        }
    }

    #[test]
    fn a_missing_baseline_is_an_error_not_a_silent_skip() {
        let err = Selected::resolve("/nonexistent/dir/base.toml", Path::new("."), None, None)
            .unwrap_err();
        assert!(matches!(err, BaselineError::Io { .. }), "{err}");
        assert!(
            err.to_string().contains("cannot read the baseline"),
            "{err}"
        );
    }

    #[test]
    fn recording_keeps_what_another_test_already_wrote() {
        let dir = tempdir("record");
        let path = dir.join("rec.toml");
        let mut layer_a = Baseline::default();
        layer_a.meta.insert("cpu".into(), "x".into());
        layer_a.benches.insert(
            "a/b".into(),
            BenchBaseline {
                p50_ns: 12.5,
                tolerance: None,
            },
        );
        layer_a.clone().record_into(&path, false).unwrap();
        let mut layer_b = Baseline::default();
        layer_b.stress.insert(
            "s".into(),
            StressBaseline {
                per_sec: 100.0,
                p99_ns: Some(7.0),
                p999_ns: None,
                tolerance: None,
            },
        );
        layer_b.record_into(&path, false).unwrap();
        let merged = Baseline::load(&path).unwrap();
        assert_eq!(
            merged.benches["a/b"].p50_ns, 12.5,
            "layer A survived layer B"
        );
        assert_eq!(merged.stress["s"].per_sec, 100.0);
        assert_eq!(merged.meta["cpu"], "x");
        // Recording again replaces the same-named rows.
        layer_a.benches.get_mut("a/b").unwrap().p50_ns = 13.5;
        layer_a.record_into(&path, false).unwrap();
        assert_eq!(Baseline::load(&path).unwrap().benches["a/b"].p50_ns, 13.5);
    }

    #[test]
    fn keeping_the_best_never_makes_a_recorded_row_worse() {
        let dir = tempdir("best");
        let path = dir.join("best.toml");
        let row = |p50: f64| BenchBaseline {
            p50_ns: p50,
            tolerance: None,
        };
        let scenario = |per_sec: f64, p99: f64| StressBaseline {
            per_sec,
            p99_ns: Some(p99),
            p999_ns: None,
            tolerance: None,
        };
        let mut first = Baseline::default();
        first.benches.insert("fast".into(), row(10.0));
        first.benches.insert("slow".into(), row(50.0));
        first.stress.insert("s".into(), scenario(100.0, 40.0));
        first.record_into(&path, true).unwrap();
        // A second, noisier recording of one row, a better one of the other, a better scenario.
        let mut second = Baseline::default();
        second.benches.insert("fast".into(), row(14.0));
        second.benches.insert("slow".into(), row(40.0));
        second.benches.insert("new".into(), row(5.0));
        second.stress.insert("s".into(), scenario(90.0, 30.0));
        second.record_into(&path, true).unwrap();
        let kept = Baseline::load(&path).unwrap();
        assert_eq!(kept.benches["fast"].p50_ns, 10.0, "the better p50 stays");
        assert_eq!(
            kept.benches["slow"].p50_ns, 40.0,
            "the better one replaces it"
        );
        assert_eq!(kept.benches["new"].p50_ns, 5.0);
        assert_eq!(
            kept.stress["s"].per_sec, 100.0,
            "the higher throughput stays"
        );
        assert_eq!(
            kept.stress["s"].p99_ns,
            Some(30.0),
            "the lower p99 replaces it"
        );
    }

    /// A fresh scratch directory under the target dir (no `tempfile` dependency).
    fn tempdir(tag: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("baseline-tests")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
