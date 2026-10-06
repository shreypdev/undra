//! `budgets.toml`: the per-operation host budgets the budgets test enforces.
//!
//! The file is a small, strict subset of TOML (no dependency is needed to read it):
//!
//! ```toml
//! [meta]
//! machine = "Apple M-series, macOS"   # free-form strings and numbers, kept for humans
//!
//! [bench."wire/u32/roundtrip"]        # the workload name, quoted
//! budget_ns = 400                     # required: p50 above this fails the test
//! measured_ns = 80                    # criterion median when the budget was set
//! blueprint_ns = 60                   # the blueprint's section 14 target, when the row exists
//! blueprint = "<= 60 ns on iOS (A15)" # the target in words
//! baseline_tolerance = 2              # optional: against a baseline, this row gets at least 2x
//! p99_ns = 60000                      # optional: the p99 of the iterations above this fails the test
//! measured_p99_ns = 11000             # the p99 when the gate was set (kept for humans)
//! ```
//!
//! `p99_ns` is for a row whose claim is its tail, such as how long the core lock is held (the
//! leaderboard rows): with the best p99 of the attempts, like the p50. It is an absolute ceiling and
//! `UNDRA_BENCH_SCALE` multiplies it.
//!
//! Numbers are nanoseconds and may use `_` separators. Anything else is a syntax error, with the
//! line number, so a typo cannot silently disable a budget.
//!
//! A second kind of table gates the **sustained** ("harsh conditions") scenarios of
//! `bench/tests/stress.rs` and the soak binary. Every key is optional but at least one gate must
//! be present:
//!
//! ```toml
//! [stress."firehose/sustained"]       # the scenario name, quoted
//! min_per_sec = 1100000               # gate: operations per second must be at least this
//! p99_ns = 1100                       # gate: the 99th percentile latency must be at most this
//! p999_ns = 3000                      # gate: the 99.9th percentile latency must be at most this
//! bytes_per_op = 37                   # gate: change-set bytes per operation, at most this
//! rss_growth_pct = 1                  # gate: RSS growth after warm-up, at most this percent
//! measured_per_sec = 5699209          # what the harness measured when the gates were set
//! measured_p99_ns = 209               # (kept for humans, and checked to be inside the gates)
//! measured_p999_ns = 292
//! measured_bytes_per_op = 37
//! baseline_tolerance = 2              # optional: against a baseline, at least 2x (not a gate)
//! ```
//!
//! `baseline_tolerance` (at least 1) is the one per-row knob of the baseline gate
//! ([`crate::baseline`]) that survives a baseline recorded afresh in every CI job: a row or
//! scenario that proves noisy on a machine class gets more room here, and only it.
//!
//! `UNDRA_BENCH_SCALE` divides `min_per_sec` and multiplies the two latency ceilings; it never
//! touches `bytes_per_op`, `rss_growth_pct` or a scenario's own invariants (nothing lost,
//! nothing reordered), which are not negotiable on a slower machine. `bytes_per_op` is a
//! **ceiling**: a run that ships fewer bytes passes (what each scenario asserts about its own
//! exact byte count is in its invariants, in code).
//!
//! A third kind of table gates a **ratio between two layer A rows measured in the same run**,
//! so the speed of the machine cancels:
//!
//! ```toml
//! [ratio."commit_vs_call"]            # a name for the relationship, quoted
//! num = "stress/firehose/call_set"    # required: the [bench."..."] row on top
//! den = "dispatch/call_sync/add"      # required: the [bench."..."] row below
//! num_div = 1                         # optional: operations per iteration of `num` (1)
//! den_div = 1                         # optional: operations per iteration of `den` (1)
//! max = 3.4                           # required: (num p50 / num_div) / (den p50 / den_div) at most
//! measured = 2.9                      # the ratio when the gate was set, kept for humans
//! ```
//!
//! `UNDRA_BENCH_SCALE` does not touch a ratio: a slower machine slows both rows.
//!
//! A fourth kind of table gates the **size** of a shipped artefact, measured by
//! `scripts/wasm-size.sh` (the web) or `scripts/native-size.sh` (the iOS and Android cores), not by
//! this crate (ADR-052):
//!
//! ```toml
//! [size."web/hello-wasm"]             # the artefact, quoted (the `artifact` of its JSON line)
//! budget_gzip_bytes = 120000          # required: gzipped bytes, never more than this
//! measured_gzip_bytes = 95838         # the record (bench/results/web-size.jsonl)
//! tolerance = 0.05                    # gate: at most 5% over the record
//!
//! [size."android/hello-arm64-v8a"]    # a native library counts the bytes of the file that ships
//! budget_bytes = 800000               # (budget_bytes and measured_bytes, instead of the _gzip_ pair)
//! measured_bytes = 761000             # the record (bench/results/native-size.jsonl)
//! tolerance = 0.05
//! ```
//!
//! A table uses one pair or the other ([`SizeUnit`]). The gate is [`SizeBudget::ceiling`]: the
//! budget, or the record plus the tolerance when that is lower. `UNDRA_BENCH_SCALE` never applies
//! to a size.
//!
//! A fifth kind of table gates a row of the **web call path**, measured in JavaScript (the
//! playground's device bench in Chromium, `bench.spec.ts`, and `call-path.test.ts` against the
//! fixture core in Node), not by this crate:
//!
//! ```toml
//! [web."sync_call"]                   # the row's id, quoted
//! budget_ns = 3900                    # required: the p50 per operation, never more than this
//! measured_ns = 780                   # the p50 when the budget was set (kept for humans)
//! what = "await bench.benchAdd(1, 2) in Chromium"  # optional: in words
//! ```
//!
//! This crate only reads the table and checks that it is consistent with itself
//! ([`WebBudget::self_check`]); the measuring harnesses read the same file, so there is one place
//! a web budget is written down. `UNDRA_BENCH_SCALE` multiplies it like any latency ceiling.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::rss::RssGrowth;

/// One operation's budget.
#[derive(Clone, Debug, PartialEq)]
pub struct Budget {
    /// The p50, in nanoseconds per iteration, above which the budgets test fails.
    pub budget_ns: f64,
    /// What criterion measured on the reference machine when the budget was set, if recorded.
    pub measured_ns: Option<f64>,
    /// The blueprint section 14 target for this operation on its reference device, if it has one.
    pub blueprint_ns: Option<f64>,
    /// The same target in words (device and unit), for reports.
    pub blueprint: Option<String>,
    /// Against a baseline (`UNDRA_BENCH_BASELINE`), this row's p50 gets at least this factor:
    /// a row that proves noisy on a machine class gets room here rather than every row getting it.
    pub baseline_tolerance: Option<f64>,
    /// The p99, in nanoseconds per iteration, above which the budgets test fails (optional: only
    /// rows whose claim is a tail have one).
    pub p99_ns: Option<f64>,
    /// The p99 the reference machine measured when `p99_ns` was set, if recorded.
    pub measured_p99_ns: Option<f64>,
}

/// A ratio between two layer A rows measured in the same run (a `[ratio."name"]` table): the
/// machine's speed cancels, so the gate holds on any hardware as long as the relationship does.
#[derive(Clone, Debug, PartialEq)]
pub struct RatioBudget {
    /// The row on top: a `[bench."..."]` name.
    pub num: String,
    /// The row below: a `[bench."..."]` name.
    pub den: String,
    /// Operations per iteration of `num` (a row timed as a batch of 1,000 says 1000).
    pub num_div: f64,
    /// Operations per iteration of `den`.
    pub den_div: f64,
    /// The ratio of the per-operation p50s must be at most this.
    pub max: f64,
    /// The ratio measured when the gate was set, if recorded.
    pub measured: Option<f64>,
}

impl RatioBudget {
    /// The ratio of two p50s (nanoseconds per iteration), per operation.
    ///
    /// # Example
    ///
    /// ```
    /// use undra_bench::budget::Budgets;
    ///
    /// let b = Budgets::parse(
    ///     "[ratio.\"r\"]\nnum = \"a\"\nden = \"b\"\nnum_div = 1000\nmax = 2\n",
    /// )
    /// .unwrap();
    /// // 80 us per 1,000 operations against 40 ns per operation: a ratio of 2.
    /// assert_eq!(b.ratios["r"].value(80_000.0, 40.0), 2.0);
    /// ```
    pub fn value(&self, num_p50_ns: f64, den_p50_ns: f64) -> f64 {
        (num_p50_ns / self.num_div) / (den_p50_ns / self.den_div)
    }

    /// Whether `value` (a ratio from [`value`](RatioBudget::value)) is within the gate.
    pub fn holds(&self, value: f64) -> bool {
        value <= self.max
    }

    /// Problems with the table itself: a recorded measurement already over its own gate.
    pub fn self_check(&self) -> Vec<String> {
        match self.measured {
            Some(m) if m > self.max => vec![format!("measured {m} is over max {}", self.max)],
            _ => Vec::new(),
        }
    }
}

/// What a `[size."name"]` table counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeUnit {
    /// Gzipped bytes (zlib, level 9): the web artefacts (`budget_gzip_bytes`, `measured_gzip_bytes`).
    Gzip,
    /// The bytes of the file as it ships: the iOS and Android cores (`budget_bytes`,
    /// `measured_bytes`).
    Raw,
}

impl SizeUnit {
    /// The key of the budget in a table of this unit.
    pub fn budget_key(self) -> &'static str {
        match self {
            SizeUnit::Gzip => "budget_gzip_bytes",
            SizeUnit::Raw => "budget_bytes",
        }
    }

    /// The key of the record in a table of this unit.
    pub fn measured_key(self) -> &'static str {
        match self {
            SizeUnit::Gzip => "measured_gzip_bytes",
            SizeUnit::Raw => "measured_bytes",
        }
    }

    /// The field of a JSON line of the record that holds the number a table of this unit gates.
    pub fn record_field(self) -> &'static str {
        match self {
            SizeUnit::Gzip => "gzipped",
            SizeUnit::Raw => "bytes",
        }
    }
}

/// A shipped artefact's size gate (a `[size."name"]` table), in gzipped bytes (zlib, level 9) or in
/// the bytes of the shipped file, as [`SizeUnit`] says.
#[derive(Clone, Debug, PartialEq)]
pub struct SizeBudget {
    /// What the numbers below count.
    pub unit: SizeUnit,
    /// The budget: the size is never more than this.
    pub budget: f64,
    /// The recorded size, if the artefact has been recorded.
    pub measured: Option<f64>,
    /// How far over the record a measurement may go, as a fraction (`0.05` is 5%).
    pub tolerance: Option<f64>,
}

impl SizeBudget {
    /// The gate: the budget, or `floor(record x (1 + tolerance))` when there is a record and a
    /// tolerance and that is lower. `scripts/wasm-size.sh` and `scripts/native-size.sh` compute the
    /// same number.
    ///
    /// ```
    /// use undra_bench::budget::Budgets;
    ///
    /// let b = Budgets::parse(
    ///     "[size.\"web\"]\nbudget_gzip_bytes = 120000\nmeasured_gzip_bytes = 95838\ntolerance = 0.05\n",
    /// )
    /// .unwrap();
    /// assert_eq!(b.sizes["web"].ceiling(), 100_629.0);
    /// assert!(b.sizes["web"].holds(100_629.0) && !b.sizes["web"].holds(100_630.0));
    /// ```
    pub fn ceiling(&self) -> f64 {
        match (self.measured, self.tolerance) {
            (Some(record), Some(tolerance)) => {
                self.budget.min((record * (1.0 + tolerance)).floor())
            }
            _ => self.budget,
        }
    }

    /// Whether a measured size (in this table's unit) passes the gate.
    pub fn holds(&self, size: f64) -> bool {
        size <= self.ceiling()
    }

    /// Problems with the table itself: a record already over the budget, a tolerance with no
    /// record to apply it to.
    pub fn self_check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(record) = self.measured {
            if record > self.budget {
                problems.push(format!(
                    "{} {record} is over {} {}",
                    self.unit.measured_key(),
                    self.unit.budget_key(),
                    self.budget
                ));
            }
        } else if self.tolerance.is_some() {
            problems.push(format!(
                "tolerance is set but there is no {}",
                self.unit.measured_key()
            ));
        }
        problems
    }
}

/// A web call-path row's budget (a `[web."id"]` table), in nanoseconds per operation.
#[derive(Clone, Debug, PartialEq)]
pub struct WebBudget {
    /// The p50 per operation above which the measuring harness fails.
    pub budget_ns: f64,
    /// The p50 measured when the budget was set, if recorded.
    pub measured_ns: Option<f64>,
    /// What the row measures, in words.
    pub what: Option<String>,
}

impl WebBudget {
    /// Problems with the table itself: a recorded measurement already over its own budget.
    pub fn self_check(&self) -> Vec<String> {
        match self.measured_ns {
            Some(m) if m > self.budget_ns => {
                vec![format!(
                    "measured_ns {m} is over budget_ns {}",
                    self.budget_ns
                )]
            }
            _ => Vec::new(),
        }
    }
}

/// One sustained scenario's gates (a `[stress."name"]` table). A gate that is `None` is not
/// checked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StressBudget {
    /// Operations per second of wall time must be at least this (before scaling).
    pub min_per_sec: Option<f64>,
    /// The 99th percentile latency, in nanoseconds, must be at most this (before scaling).
    pub p99_ns: Option<f64>,
    /// The 99.9th percentile latency, in nanoseconds, must be at most this (before scaling).
    pub p999_ns: Option<f64>,
    /// Bytes per operation must be at most this. It is a **ceiling**: fewer bytes pass, so the
    /// gate alone cannot say a scenario shipped exactly what it should. Where the load is a
    /// fixed cycle at fixed widths the ceiling is the measured value, and the scenario asserts
    /// the exact count in code (an invariant), which is what makes it exact.
    pub bytes_per_op: Option<f64>,
    /// RSS growth from after the warm-up to the end, in percent (see [`RssGrowth::within`]).
    pub rss_growth_pct: Option<f64>,
    /// Throughput measured when the gates were set.
    pub measured_per_sec: Option<f64>,
    /// p99 measured when the gates were set, nanoseconds.
    pub measured_p99_ns: Option<f64>,
    /// p999 measured when the gates were set, nanoseconds.
    pub measured_p999_ns: Option<f64>,
    /// Bytes per operation measured when the gates were set.
    pub measured_bytes_per_op: Option<f64>,
    /// Against a baseline (`UNDRA_BENCH_BASELINE`), this scenario's throughput gets at least this
    /// factor, and its p99 at least this or the tail factor. Not a gate of its own.
    pub baseline_tolerance: Option<f64>,
}

/// What a sustained run observed, in the terms a [`StressBudget`] gates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StressObserved {
    /// Operations per second over the run's wall time.
    pub per_sec: f64,
    /// 99th percentile latency in nanoseconds, when the scenario times operations.
    pub p99_ns: Option<f64>,
    /// 99.9th percentile latency in nanoseconds, when the scenario times operations.
    pub p999_ns: Option<f64>,
    /// Change-set bytes per operation, when the scenario counts them.
    pub bytes_per_op: Option<f64>,
    /// RSS growth, when it could be measured on this platform.
    pub rss: Option<RssGrowth>,
}

/// The outcome of checking a run against a [`StressBudget`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StressVerdict {
    /// Gates that were not met, one sentence each.
    pub failures: Vec<String>,
    /// Gates that could not be checked (RSS on a platform that cannot measure it): printed, and
    /// never a silent pass.
    pub notices: Vec<String>,
}

impl StressVerdict {
    /// Whether every gate that could be checked was met.
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

impl StressBudget {
    /// Checks `observed` against the gates. `scale` (`UNDRA_BENCH_SCALE`) divides the throughput
    /// floor and multiplies the latency ceilings; bytes and RSS are never scaled.
    pub fn check(&self, observed: &StressObserved, scale: f64) -> StressVerdict {
        let mut verdict = StressVerdict::default();
        let fail = |v: &mut StressVerdict, text: String| v.failures.push(text);
        if let Some(floor) = self.min_per_sec {
            let floor = floor / scale;
            if observed.per_sec < floor {
                fail(
                    &mut verdict,
                    format!(
                        "throughput {:.0}/s is under the floor of {floor:.0}/s",
                        observed.per_sec
                    ),
                );
            }
        }
        for (name, limit, got) in [
            ("p99", self.p99_ns, observed.p99_ns),
            ("p999", self.p999_ns, observed.p999_ns),
        ] {
            let Some(limit) = limit else { continue };
            let limit = limit * scale;
            match got {
                Some(got) if got <= limit => {}
                Some(got) => fail(
                    &mut verdict,
                    format!("{name} {got:.0} ns is over the ceiling of {limit:.0} ns"),
                ),
                None => fail(
                    &mut verdict,
                    format!("{name} is gated but was not measured"),
                ),
            }
        }
        if let Some(limit) = self.bytes_per_op {
            match observed.bytes_per_op {
                Some(got) if got <= limit * (1.0 + 1e-9) => {}
                Some(got) => fail(
                    &mut verdict,
                    format!("{got:.2} bytes per operation is over the ceiling of {limit}"),
                ),
                None => fail(
                    &mut verdict,
                    "bytes per operation is gated but was not counted".into(),
                ),
            }
        }
        if let Some(pct) = self.rss_growth_pct {
            match observed.rss {
                Some(growth) if growth.within(pct) => {}
                Some(growth) => fail(
                    &mut verdict,
                    format!(
                        "RSS grew {:.2}% ({} to {} bytes) after warm-up; the gate is {pct}% or 64 KiB",
                        growth.growth_pct, growth.baseline_bytes, growth.final_bytes
                    ),
                ),
                None => verdict.notices.push(
                    "RSS could not be measured here (no samples or no source on this platform): \
                     the RSS gate was skipped"
                        .into(),
                ),
            }
        }
        verdict
    }

    /// Problems with the table itself: a recorded measurement that is already outside its own
    /// gate, which would mean the gate was set below what the harness measured.
    pub fn self_check(&self) -> Vec<String> {
        // (measured key, gate key, measured, gate, the gate is a floor)
        let pairs = [
            (
                "measured_per_sec",
                "min_per_sec",
                self.measured_per_sec,
                self.min_per_sec,
                true,
            ),
            (
                "measured_p99_ns",
                "p99_ns",
                self.measured_p99_ns,
                self.p99_ns,
                false,
            ),
            (
                "measured_p999_ns",
                "p999_ns",
                self.measured_p999_ns,
                self.p999_ns,
                false,
            ),
            (
                "measured_bytes_per_op",
                "bytes_per_op",
                self.measured_bytes_per_op,
                self.bytes_per_op,
                false,
            ),
        ];
        let mut problems = Vec::new();
        for (measured_key, gate_key, measured, gate, is_floor) in pairs {
            if let (Some(m), Some(g)) = (measured, gate) {
                if is_floor && m < g {
                    problems.push(format!("{measured_key} {m} is under {gate_key} {g}"));
                } else if !is_floor && m > g {
                    problems.push(format!("{measured_key} {m} is over {gate_key} {g}"));
                }
            }
        }
        problems
    }

    /// Whether the table sets at least one gate.
    fn has_gate(&self) -> bool {
        self.min_per_sec.is_some()
            || self.p99_ns.is_some()
            || self.p999_ns.is_some()
            || self.bytes_per_op.is_some()
            || self.rss_growth_pct.is_some()
    }
}

/// The parsed `budgets.toml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budgets {
    /// The `[meta]` table: the machine and the reasoning, for humans.
    pub meta: BTreeMap<String, String>,
    /// The per-operation budgets, by workload name.
    pub benches: BTreeMap<String, Budget>,
    /// The sustained-scenario budgets, by scenario name (`[stress."name"]`).
    pub stress: BTreeMap<String, StressBudget>,
    /// The ratio gates between two layer A rows, by name (`[ratio."name"]`).
    pub ratios: BTreeMap<String, RatioBudget>,
    /// The size gates of shipped artefacts, by artefact name (`[size."name"]`).
    pub sizes: BTreeMap<String, SizeBudget>,
    /// The web call-path budgets, by row id (`[web."id"]`).
    pub web: BTreeMap<String, WebBudget>,
}

/// Why `budgets.toml` could not be read.
#[derive(Clone, Debug, PartialEq)]
pub enum BudgetError {
    /// The file could not be read.
    Io(String),
    /// A line is not part of the accepted subset.
    Syntax {
        /// One-based line number.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// A `[bench."name"]` table has no `budget_ns`.
    MissingBudget {
        /// The workload the table names.
        name: String,
    },
    /// Two tables name the same workload.
    Duplicate {
        /// The workload named twice.
        name: String,
    },
    /// A `[stress."name"]` table sets none of `min_per_sec`, `p99_ns`, `p999_ns`,
    /// `bytes_per_op` and `rss_growth_pct`: it would gate nothing.
    MissingGate {
        /// The scenario the table names.
        name: String,
    },
    /// Two `[stress."name"]` tables name the same scenario.
    DuplicateStress {
        /// The scenario named twice.
        name: String,
    },
    /// A `[ratio."name"]` table lacks `num`, `den` or `max`.
    IncompleteRatio {
        /// The ratio the table names.
        name: String,
        /// The key that is missing.
        missing: &'static str,
    },
    /// A `[size."name"]` table has neither `budget_gzip_bytes` nor `budget_bytes`.
    MissingSizeBudget {
        /// The artefact the table names.
        name: String,
    },
    /// A `[size."name"]` table uses both pairs of keys (`budget_gzip_bytes` and `budget_bytes`),
    /// or a record of the other unit than its budget's.
    MixedSizeUnits {
        /// The artefact the table names.
        name: String,
    },
    /// Two `[size."name"]` tables name the same artefact.
    DuplicateSize {
        /// The artefact named twice.
        name: String,
    },
    /// Two `[ratio."name"]` tables have the same name.
    DuplicateRatio {
        /// The ratio named twice.
        name: String,
    },
    /// A `[web."id"]` table has no `budget_ns`.
    MissingWebBudget {
        /// The row the table names.
        name: String,
    },
    /// Two `[web."id"]` tables name the same row.
    DuplicateWeb {
        /// The row named twice.
        name: String,
    },
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BudgetError::Io(e) => write!(f, "cannot read the budgets file: {e}"),
            BudgetError::Syntax { line, message } => {
                write!(f, "budgets file, line {line}: {message}")
            }
            BudgetError::MissingBudget { name } => {
                write!(f, "budgets file: [bench.\"{name}\"] has no budget_ns")
            }
            BudgetError::Duplicate { name } => {
                write!(f, "budgets file: [bench.\"{name}\"] appears twice")
            }
            BudgetError::MissingGate { name } => write!(
                f,
                "budgets file: [stress.\"{name}\"] sets no gate (min_per_sec, p99_ns, p999_ns, \
                 bytes_per_op or rss_growth_pct)"
            ),
            BudgetError::DuplicateStress { name } => {
                write!(f, "budgets file: [stress.\"{name}\"] appears twice")
            }
            BudgetError::IncompleteRatio { name, missing } => {
                write!(f, "budgets file: [ratio.\"{name}\"] has no {missing}")
            }
            BudgetError::MissingSizeBudget { name } => {
                write!(
                    f,
                    "budgets file: [size.\"{name}\"] has no budget_gzip_bytes (or budget_bytes)"
                )
            }
            BudgetError::MixedSizeUnits { name } => {
                write!(
                    f,
                    "budgets file: [size.\"{name}\"] mixes the gzipped keys (budget_gzip_bytes, \
                     measured_gzip_bytes) with the raw ones (budget_bytes, measured_bytes)"
                )
            }
            BudgetError::DuplicateSize { name } => {
                write!(f, "budgets file: [size.\"{name}\"] appears twice")
            }
            BudgetError::DuplicateRatio { name } => {
                write!(f, "budgets file: [ratio.\"{name}\"] appears twice")
            }
            BudgetError::MissingWebBudget { name } => {
                write!(f, "budgets file: [web.\"{name}\"] has no budget_ns")
            }
            BudgetError::DuplicateWeb { name } => {
                write!(f, "budgets file: [web.\"{name}\"] appears twice")
            }
        }
    }
}

impl std::error::Error for BudgetError {}

/// A scalar value on the right of `key = value`.
pub(crate) enum Value {
    Number(f64),
    Text(String),
}

enum Section {
    None,
    Meta,
    Bench(String),
    Stress(String),
    Ratio(String),
    Size(String),
    Web(String),
}

impl Budgets {
    /// Reads and parses the file at `path`.
    pub fn load(path: &Path) -> Result<Budgets, BudgetError> {
        let text = std::fs::read_to_string(path).map_err(|e| BudgetError::Io(e.to_string()))?;
        Budgets::parse(&text)
    }

    /// Parses the text of a budgets file.
    pub fn parse(text: &str) -> Result<Budgets, BudgetError> {
        let mut budgets = Budgets::default();
        let mut section = Section::None;
        let mut pending: BTreeMap<String, PartialBudget> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut stress_order: Vec<String> = Vec::new();
        let mut ratios: BTreeMap<String, PartialRatio> = BTreeMap::new();
        let mut ratio_order: Vec<String> = Vec::new();
        let mut sizes: BTreeMap<String, PartialSize> = BTreeMap::new();
        let mut size_order: Vec<String> = Vec::new();
        let mut web: BTreeMap<String, PartialWeb> = BTreeMap::new();
        let mut web_order: Vec<String> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let syntax = |message: &str| BudgetError::Syntax {
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
                    if pending.contains_key(&name) {
                        return Err(BudgetError::Duplicate { name });
                    }
                    pending.insert(name.clone(), PartialBudget::default());
                    order.push(name.clone());
                    Section::Bench(name)
                } else if let Some(name) = header.strip_prefix("stress.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a stress table is written [stress.\"name\"]"))?;
                    if budgets.stress.contains_key(&name) {
                        return Err(BudgetError::DuplicateStress { name });
                    }
                    budgets.stress.insert(name.clone(), StressBudget::default());
                    stress_order.push(name.clone());
                    Section::Stress(name)
                } else if let Some(name) = header.strip_prefix("ratio.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a ratio table is written [ratio.\"name\"]"))?;
                    if ratios.contains_key(&name) {
                        return Err(BudgetError::DuplicateRatio { name });
                    }
                    ratios.insert(name.clone(), PartialRatio::default());
                    ratio_order.push(name.clone());
                    Section::Ratio(name)
                } else if let Some(name) = header.strip_prefix("size.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a size table is written [size.\"name\"]"))?;
                    if sizes.contains_key(&name) {
                        return Err(BudgetError::DuplicateSize { name });
                    }
                    sizes.insert(name.clone(), PartialSize::default());
                    size_order.push(name.clone());
                    Section::Size(name)
                } else if let Some(name) = header.strip_prefix("web.") {
                    let name = parse_string(name.trim())
                        .ok_or_else(|| syntax("a web table is written [web.\"id\"]"))?;
                    if web.contains_key(&name) {
                        return Err(BudgetError::DuplicateWeb { name });
                    }
                    web.insert(name.clone(), PartialWeb::default());
                    web_order.push(name.clone());
                    Section::Web(name)
                } else {
                    return Err(syntax(
                        "the only tables are [meta], [bench.\"name\"], [stress.\"name\"], \
                         [ratio.\"name\"], [size.\"name\"] and [web.\"id\"]",
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
                    budgets.meta.insert(key.to_owned(), text);
                }
                Section::Stress(name) => {
                    let entry = budgets.stress.entry(name.clone()).or_default();
                    let Value::Number(n) = value else {
                        return Err(syntax("a stress table takes numbers only"));
                    };
                    match key {
                        "min_per_sec" => entry.min_per_sec = Some(n),
                        "p99_ns" => entry.p99_ns = Some(n),
                        "p999_ns" => entry.p999_ns = Some(n),
                        "bytes_per_op" => entry.bytes_per_op = Some(n),
                        "rss_growth_pct" => entry.rss_growth_pct = Some(n),
                        "measured_per_sec" => entry.measured_per_sec = Some(n),
                        "measured_p99_ns" => entry.measured_p99_ns = Some(n),
                        "measured_p999_ns" => entry.measured_p999_ns = Some(n),
                        "measured_bytes_per_op" => entry.measured_bytes_per_op = Some(n),
                        "baseline_tolerance" if n >= 1.0 => entry.baseline_tolerance = Some(n),
                        _ => {
                            return Err(syntax(
                                "a stress table takes min_per_sec, p99_ns, p999_ns, bytes_per_op, \
                                 rss_growth_pct, measured_per_sec, measured_p99_ns, \
                                 measured_p999_ns, measured_bytes_per_op and baseline_tolerance \
                                 (at least 1)",
                            ));
                        }
                    }
                }
                Section::Ratio(name) => {
                    let entry = ratios.entry(name.clone()).or_default();
                    match (key, value) {
                        ("num", Value::Text(t)) => entry.num = Some(t),
                        ("den", Value::Text(t)) => entry.den = Some(t),
                        ("num_div", Value::Number(n)) if n > 0.0 => entry.num_div = Some(n),
                        ("den_div", Value::Number(n)) if n > 0.0 => entry.den_div = Some(n),
                        ("max", Value::Number(n)) if n > 0.0 => entry.max = Some(n),
                        ("measured", Value::Number(n)) => entry.measured = Some(n),
                        _ => {
                            return Err(syntax(
                                "a ratio table takes num and den (strings), num_div, den_div and \
                                 max (positive numbers) and measured (a number)",
                            ));
                        }
                    }
                }
                Section::Size(name) => {
                    let entry = sizes.entry(name.clone()).or_default();
                    match (key, value) {
                        ("budget_gzip_bytes", Value::Number(n)) if n > 0.0 => {
                            entry.budget_gzip_bytes = Some(n);
                        }
                        ("measured_gzip_bytes", Value::Number(n)) if n > 0.0 => {
                            entry.measured_gzip_bytes = Some(n);
                        }
                        ("budget_bytes", Value::Number(n)) if n > 0.0 => {
                            entry.budget_bytes = Some(n);
                        }
                        ("measured_bytes", Value::Number(n)) if n > 0.0 => {
                            entry.measured_bytes = Some(n);
                        }
                        ("tolerance", Value::Number(n)) => entry.tolerance = Some(n),
                        _ => {
                            return Err(syntax(
                                "a size table takes budget_gzip_bytes and measured_gzip_bytes, or \
                                 budget_bytes and measured_bytes (positive numbers), and tolerance \
                                 (a fraction, 0.05 for 5%)",
                            ));
                        }
                    }
                }
                Section::Web(name) => {
                    let entry = web.entry(name.clone()).or_default();
                    match (key, value) {
                        ("budget_ns", Value::Number(n)) if n > 0.0 => entry.budget_ns = Some(n),
                        ("measured_ns", Value::Number(n)) if n > 0.0 => entry.measured_ns = Some(n),
                        ("what", Value::Text(t)) => entry.what = Some(t),
                        _ => {
                            return Err(syntax(
                                "a web table takes budget_ns and measured_ns (positive numbers) \
                                 and what (a string)",
                            ));
                        }
                    }
                }
                Section::Bench(name) => {
                    let entry = pending.entry(name.clone()).or_default();
                    match (key, value) {
                        ("budget_ns", Value::Number(n)) => entry.budget_ns = Some(n),
                        ("measured_ns", Value::Number(n)) => entry.measured_ns = Some(n),
                        ("blueprint_ns", Value::Number(n)) => entry.blueprint_ns = Some(n),
                        ("blueprint", Value::Text(t)) => entry.blueprint = Some(t),
                        ("baseline_tolerance", Value::Number(n)) if n >= 1.0 => {
                            entry.baseline_tolerance = Some(n);
                        }
                        ("p99_ns", Value::Number(n)) if n > 0.0 => entry.p99_ns = Some(n),
                        ("measured_p99_ns", Value::Number(n)) if n > 0.0 => {
                            entry.measured_p99_ns = Some(n);
                        }
                        _ => {
                            return Err(syntax(
                                "a bench table takes budget_ns, measured_ns, blueprint_ns \
                                 (numbers), blueprint (a string), baseline_tolerance (a number \
                                 of at least 1), p99_ns and measured_p99_ns (positive numbers)",
                            ));
                        }
                    }
                }
            }
        }
        for name in stress_order {
            if !budgets
                .stress
                .get(&name)
                .is_some_and(StressBudget::has_gate)
            {
                return Err(BudgetError::MissingGate { name });
            }
        }
        for name in ratio_order {
            let partial = ratios.remove(&name).unwrap_or_default();
            let missing = |key: &'static str| BudgetError::IncompleteRatio {
                name: name.clone(),
                missing: key,
            };
            let ratio = RatioBudget {
                num: partial.num.clone().ok_or_else(|| missing("num"))?,
                den: partial.den.clone().ok_or_else(|| missing("den"))?,
                num_div: partial.num_div.unwrap_or(1.0),
                den_div: partial.den_div.unwrap_or(1.0),
                max: partial.max.ok_or_else(|| missing("max"))?,
                measured: partial.measured,
            };
            budgets.ratios.insert(name, ratio);
        }
        for name in size_order {
            let partial = sizes.remove(&name).unwrap_or_default();
            let (unit, budget, measured) = match (partial.budget_gzip_bytes, partial.budget_bytes) {
                (Some(budget), None) if partial.measured_bytes.is_none() => {
                    (SizeUnit::Gzip, budget, partial.measured_gzip_bytes)
                }
                (None, Some(budget)) if partial.measured_gzip_bytes.is_none() => {
                    (SizeUnit::Raw, budget, partial.measured_bytes)
                }
                (None, None) => return Err(BudgetError::MissingSizeBudget { name }),
                _ => return Err(BudgetError::MixedSizeUnits { name }),
            };
            budgets.sizes.insert(
                name,
                SizeBudget {
                    unit,
                    budget,
                    measured,
                    tolerance: partial.tolerance,
                },
            );
        }
        for name in web_order {
            let partial = web.remove(&name).unwrap_or_default();
            let Some(budget_ns) = partial.budget_ns else {
                return Err(BudgetError::MissingWebBudget { name });
            };
            budgets.web.insert(
                name,
                WebBudget {
                    budget_ns,
                    measured_ns: partial.measured_ns,
                    what: partial.what,
                },
            );
        }
        for name in order {
            let partial = pending.remove(&name).unwrap_or_default();
            let Some(budget_ns) = partial.budget_ns else {
                return Err(BudgetError::MissingBudget { name });
            };
            budgets.benches.insert(
                name,
                Budget {
                    budget_ns,
                    measured_ns: partial.measured_ns,
                    blueprint_ns: partial.blueprint_ns,
                    blueprint: partial.blueprint,
                    baseline_tolerance: partial.baseline_tolerance,
                    p99_ns: partial.p99_ns,
                    measured_p99_ns: partial.measured_p99_ns,
                },
            );
        }
        Ok(budgets)
    }
}

#[derive(Default)]
struct PartialRatio {
    num: Option<String>,
    den: Option<String>,
    num_div: Option<f64>,
    den_div: Option<f64>,
    max: Option<f64>,
    measured: Option<f64>,
}

#[derive(Default)]
struct PartialWeb {
    budget_ns: Option<f64>,
    measured_ns: Option<f64>,
    what: Option<String>,
}

#[derive(Default)]
struct PartialSize {
    budget_gzip_bytes: Option<f64>,
    measured_gzip_bytes: Option<f64>,
    budget_bytes: Option<f64>,
    measured_bytes: Option<f64>,
    tolerance: Option<f64>,
}

#[derive(Default)]
struct PartialBudget {
    budget_ns: Option<f64>,
    measured_ns: Option<f64>,
    blueprint_ns: Option<f64>,
    blueprint: Option<String>,
    baseline_tolerance: Option<f64>,
    p99_ns: Option<f64>,
    measured_p99_ns: Option<f64>,
}

/// Removes a `#` comment, unless the `#` is inside a double-quoted string.
pub(crate) fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if in_string => escaped = !escaped,
            '"' if !escaped => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => escaped = false,
        }
    }
    line
}

/// `"text"` with `\"` and `\\` escapes, nothing after the closing quote.
pub(crate) fn parse_string(text: &str) -> Option<String> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(match chars.next()? {
                '"' => '"',
                '\\' => '\\',
                _ => return None,
            }),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

pub(crate) fn parse_value(text: &str) -> Option<Value> {
    if text.starts_with('"') {
        return parse_string(text).map(Value::Text);
    }
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let number: f64 = digits.parse().ok()?;
    (number.is_finite() && number >= 0.0).then_some(Value::Number(number))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# a comment
[meta]
machine = "Apple M-series # not a comment"
factor = 5

[bench."wire/u32/roundtrip"]
budget_ns = 1_000
measured_ns = 80.5    # trailing comment
blueprint_ns = 60
blueprint = "<= 60 ns on iOS (A15)"

[bench."x/y"]
budget_ns = 7
"#;

    #[test]
    fn parses_meta_and_benches() {
        let b = Budgets::parse(SAMPLE).expect("parses");
        assert_eq!(b.meta["machine"], "Apple M-series # not a comment");
        assert_eq!(b.meta["factor"], "5");
        let w = &b.benches["wire/u32/roundtrip"];
        assert_eq!(w.budget_ns, 1000.0);
        assert_eq!(w.measured_ns, Some(80.5));
        assert_eq!(w.blueprint_ns, Some(60.0));
        assert_eq!(w.blueprint.as_deref(), Some("<= 60 ns on iOS (A15)"));
        let x = &b.benches["x/y"];
        assert_eq!(
            (x.budget_ns, x.measured_ns, x.blueprint_ns),
            (7.0, None, None)
        );
    }

    #[test]
    fn rejects_what_it_does_not_understand_with_a_line_number() {
        let err = Budgets::parse("[meta]\nmachine = unquoted").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[other]\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");
        let err = Budgets::parse("budget_ns = 1\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");
        let err = Budgets::parse("[bench.\"a\"]\nbudget_ns = \"fast\"\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[bench.\"a\"]\nbudget = 4\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[bench.a]\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");
        let err = Budgets::parse("[bench.\"a\"]\nbudget_ns = -3\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
    }

    #[test]
    fn a_table_without_a_budget_or_a_repeated_table_is_an_error() {
        let err = Budgets::parse("[bench.\"a\"]\nmeasured_ns = 4\n").unwrap_err();
        assert_eq!(err, BudgetError::MissingBudget { name: "a".into() });
        let err = Budgets::parse("[bench.\"a\"]\nbudget_ns = 1\n[bench.\"a\"]\nbudget_ns = 2\n")
            .unwrap_err();
        assert_eq!(err, BudgetError::Duplicate { name: "a".into() });
    }

    #[test]
    fn display_names_the_problem() {
        let err = Budgets::parse("x").unwrap_err();
        assert!(err.to_string().contains("line 1"), "{err}");
        assert!(BudgetError::Io("nope".into()).to_string().contains("nope"));
    }

    #[test]
    fn the_shipped_budgets_file_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("budgets.toml");
        if path.exists() {
            let budgets = Budgets::load(&path).expect("bench/budgets.toml parses");
            for (name, table) in &budgets.stress {
                assert!(table.has_gate(), "{name} sets no gate");
                assert!(
                    table.self_check().is_empty(),
                    "{name}: {:?}",
                    table.self_check()
                );
            }
            for (name, size) in &budgets.sizes {
                assert!(
                    size.self_check().is_empty(),
                    "{name}: {:?}",
                    size.self_check()
                );
            }
            for (name, web) in &budgets.web {
                assert!(
                    web.self_check().is_empty(),
                    "{name}: {:?}",
                    web.self_check()
                );
            }
            for (name, ratio) in &budgets.ratios {
                assert!(
                    ratio.self_check().is_empty(),
                    "{name}: {:?}",
                    ratio.self_check()
                );
                assert!(
                    budgets.benches.contains_key(&ratio.num)
                        && budgets.benches.contains_key(&ratio.den),
                    "{name}: names a row that has no [bench] table"
                );
            }
        }
    }

    const RATIOS: &str = r#"
[bench."a"]
budget_ns = 10
[bench."b"]
budget_ns = 10

[ratio."a_vs_b"]
num = "a"
den = "b"
num_div = 1000
max = 2.5
measured = 1.8
"#;

    #[test]
    fn parses_ratio_tables_and_computes_per_operation_ratios() {
        let b = Budgets::parse(RATIOS).expect("parses");
        let r = &b.ratios["a_vs_b"];
        assert_eq!((r.num.as_str(), r.den.as_str()), ("a", "b"));
        assert_eq!((r.num_div, r.den_div, r.max), (1000.0, 1.0, 2.5));
        assert_eq!(r.measured, Some(1.8));
        // 100 us per 1,000 operations (100 ns each) over a 50 ns row is 2.
        assert_eq!(r.value(100_000.0, 50.0), 2.0);
        assert!(r.holds(2.5), "the gate is inclusive");
        assert!(!r.holds(2.51));
        assert!(r.self_check().is_empty());
    }

    #[test]
    fn a_ratio_needs_both_rows_and_a_maximum_and_rejects_typos() {
        for (text, missing) in [
            ("[ratio.\"r\"]\nden = \"b\"\nmax = 2\n", "num"),
            ("[ratio.\"r\"]\nnum = \"a\"\nmax = 2\n", "den"),
            ("[ratio.\"r\"]\nnum = \"a\"\nden = \"b\"\n", "max"),
        ] {
            let err = Budgets::parse(text).unwrap_err();
            assert_eq!(
                err,
                BudgetError::IncompleteRatio {
                    name: "r".into(),
                    missing
                },
                "{text}"
            );
            assert!(err.to_string().contains(missing), "{err}");
        }
        let err = Budgets::parse(
            "[ratio.\"r\"]\nnum = \"a\"\nden = \"b\"\nmax = 2\n[ratio.\"r\"]\nmax = 3\n",
        )
        .unwrap_err();
        assert_eq!(err, BudgetError::DuplicateRatio { name: "r".into() });
        assert!(err.to_string().contains("appears twice"));
        // A zero divisor or maximum, a number where a string goes, an unknown key: line numbers.
        for text in [
            "[ratio.\"r\"]\nmax = 0\n",
            "[ratio.\"r\"]\nnum_div = 0\n",
            "[ratio.\"r\"]\nnum = 3\n",
            "[ratio.\"r\"]\nbudget_ns = 3\n",
            "[ratio.r]\n",
        ] {
            let err = Budgets::parse(text).unwrap_err();
            assert!(matches!(err, BudgetError::Syntax { .. }), "{text}: {err}");
        }
    }

    #[test]
    fn a_bench_row_may_gate_its_p99_too() {
        let b = Budgets::parse(
            "[bench.\"a\"]\nbudget_ns = 10\np99_ns = 40\nmeasured_p99_ns = 8.5\n\
             [bench.\"b\"]\nbudget_ns = 10\n",
        )
        .unwrap();
        assert_eq!(b.benches["a"].p99_ns, Some(40.0));
        assert_eq!(b.benches["a"].measured_p99_ns, Some(8.5));
        assert_eq!(b.benches["b"].p99_ns, None);
        // A ceiling of zero could never pass.
        assert!(Budgets::parse("[bench.\"a\"]\nbudget_ns = 10\np99_ns = 0\n").is_err());
    }

    #[test]
    fn a_row_or_scenario_may_carry_its_own_baseline_factor_of_at_least_one() {
        let b = Budgets::parse(
            "[bench.\"a\"]\nbudget_ns = 10\nbaseline_tolerance = 2\n\
             [stress.\"s\"]\nmin_per_sec = 5\nbaseline_tolerance = 1.8\n",
        )
        .unwrap();
        assert_eq!(b.benches["a"].baseline_tolerance, Some(2.0));
        assert_eq!(b.stress["s"].baseline_tolerance, Some(1.8));
        // It is not a gate: a scenario with only it still has none.
        assert!(Budgets::parse("[stress.\"s\"]\nbaseline_tolerance = 2\n").is_err());
        // Below 1 it would fail an unchanged run.
        for text in [
            "[bench.\"a\"]\nbudget_ns = 10\nbaseline_tolerance = 0.9\n",
            "[stress.\"s\"]\nmin_per_sec = 5\nbaseline_tolerance = 0.5\n",
        ] {
            let err = Budgets::parse(text).unwrap_err();
            assert!(
                matches!(err, BudgetError::Syntax { line: 3, .. }),
                "{text}: {err}"
            );
        }
    }

    #[test]
    fn a_ratio_measured_over_its_own_maximum_is_flagged() {
        let b =
            Budgets::parse("[ratio.\"r\"]\nnum = \"a\"\nden = \"b\"\nmax = 2\nmeasured = 2.5\n")
                .unwrap();
        assert_eq!(b.ratios["r"].self_check().len(), 1);
    }

    const STRESS: &str = r#"
[bench."a/b"]
budget_ns = 10

[stress."firehose/sustained"]
min_per_sec = 1_100_000
p99_ns = 1100
p999_ns = 3000
bytes_per_op = 37          # exact
rss_growth_pct = 1
measured_per_sec = 5699209
measured_p99_ns = 209
measured_p999_ns = 292
measured_bytes_per_op = 37

[stress."soak/mixed"]
rss_growth_pct = 1
"#;

    #[test]
    fn parses_stress_tables_next_to_bench_tables() {
        let b = Budgets::parse(STRESS).expect("parses");
        assert_eq!(b.benches.len(), 1);
        assert_eq!(b.stress.len(), 2);
        let f = &b.stress["firehose/sustained"];
        assert_eq!(f.min_per_sec, Some(1_100_000.0));
        assert_eq!(f.p99_ns, Some(1100.0));
        assert_eq!(f.p999_ns, Some(3000.0));
        assert_eq!(f.bytes_per_op, Some(37.0));
        assert_eq!(f.rss_growth_pct, Some(1.0));
        assert_eq!(f.measured_per_sec, Some(5_699_209.0));
        assert_eq!(f.measured_p99_ns, Some(209.0));
        assert_eq!(f.measured_p999_ns, Some(292.0));
        assert_eq!(f.measured_bytes_per_op, Some(37.0));
        assert!(f.self_check().is_empty());
        let soak = &b.stress["soak/mixed"];
        assert_eq!(soak.rss_growth_pct, Some(1.0));
        assert_eq!(soak.min_per_sec, None);
    }

    #[test]
    fn stress_tables_reject_what_they_do_not_understand_with_a_line_number() {
        let err = Budgets::parse("[stress.\"a\"]\nbudget_ns = 4\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[stress.\"a\"]\np99_ns = \"fast\"\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[stress.a]\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");
        let err = Budgets::parse("[stress.\"a\"]\np99_ns = -1\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        // A bench key in a stress table is a typo, not a second meaning.
        let err = Budgets::parse("[stress.\"a\"]\nmeasured_ns = 4\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
    }

    #[test]
    fn a_stress_table_without_a_gate_or_repeated_is_an_error() {
        // `measured_*` alone gates nothing.
        let err = Budgets::parse("[stress.\"a\"]\nmeasured_per_sec = 4\n").unwrap_err();
        assert_eq!(err, BudgetError::MissingGate { name: "a".into() });
        let err = Budgets::parse("[stress.\"a\"]\n").unwrap_err();
        assert_eq!(err, BudgetError::MissingGate { name: "a".into() });
        let err =
            Budgets::parse("[stress.\"a\"]\nmin_per_sec = 1\n[stress.\"a\"]\nmin_per_sec = 2\n")
                .unwrap_err();
        assert_eq!(err, BudgetError::DuplicateStress { name: "a".into() });
        // The same name in the two kinds of table is two different things.
        Budgets::parse("[bench.\"a\"]\nbudget_ns = 1\n[stress.\"a\"]\nmin_per_sec = 2\n")
            .expect("a bench and a stress table may share a name");
        assert!(err.to_string().contains("appears twice"));
        assert!(
            BudgetError::MissingGate { name: "x".into() }
                .to_string()
                .contains("[stress.\"x\"] sets no gate")
        );
    }

    fn rss(baseline: u64, last: u64) -> Option<RssGrowth> {
        Some(RssGrowth {
            baseline_bytes: baseline,
            final_bytes: last,
            peak_bytes: last.max(baseline),
            growth_pct: (last as f64 - baseline as f64) / baseline as f64 * 100.0,
        })
    }

    fn good() -> StressObserved {
        StressObserved {
            per_sec: 5_700_000.0,
            p99_ns: Some(209.0),
            p999_ns: Some(292.0),
            bytes_per_op: Some(37.0),
            rss: rss(10_000_000, 10_000_000),
        }
    }

    #[test]
    fn a_run_inside_every_gate_passes() {
        let b = Budgets::parse(STRESS).unwrap();
        let v = b.stress["firehose/sustained"].check(&good(), 1.0);
        assert!(v.passed(), "{v:?}");
        assert!(v.notices.is_empty());
    }

    #[test]
    fn each_gate_fails_on_its_own() {
        let b = Budgets::parse(STRESS).unwrap();
        let t = &b.stress["firehose/sustained"];
        let cases: [(StressObserved, &str); 5] = [
            (
                StressObserved {
                    per_sec: 1_000_000.0,
                    ..good()
                },
                "throughput",
            ),
            (
                StressObserved {
                    p99_ns: Some(1_101.0),
                    ..good()
                },
                "p99",
            ),
            (
                StressObserved {
                    p999_ns: Some(3_001.0),
                    ..good()
                },
                "p999",
            ),
            (
                StressObserved {
                    bytes_per_op: Some(37.5),
                    ..good()
                },
                "bytes per operation",
            ),
            (
                StressObserved {
                    rss: rss(10_000_000, 10_500_000),
                    ..good()
                },
                "RSS",
            ),
        ];
        for (observed, what) in cases {
            let v = t.check(&observed, 1.0);
            assert_eq!(v.failures.len(), 1, "{what}: {v:?}");
            assert!(v.failures[0].contains(what), "{what}: {v:?}");
        }
        // Gated but not measured is a failure, not a pass.
        let blind = StressObserved {
            p99_ns: None,
            p999_ns: None,
            bytes_per_op: None,
            ..good()
        };
        assert_eq!(t.check(&blind, 1.0).failures.len(), 3);
    }

    #[test]
    fn scale_loosens_rates_and_latencies_but_never_bytes_or_memory() {
        let b = Budgets::parse(STRESS).unwrap();
        let t = &b.stress["firehose/sustained"];
        // Half the throughput and double the latency pass at a scale of 2...
        let slow = StressObserved {
            per_sec: 600_000.0,
            p99_ns: Some(2_150.0),
            p999_ns: Some(5_900.0),
            ..good()
        };
        assert!(!t.check(&slow, 1.0).passed());
        assert!(t.check(&slow, 2.0).passed(), "{:?}", t.check(&slow, 2.0));
        // ...but more bytes and more memory fail at any scale.
        let fat = StressObserved {
            bytes_per_op: Some(38.0),
            ..good()
        };
        assert!(!t.check(&fat, 100.0).passed());
        let leaky = StressObserved {
            rss: rss(10_000_000, 12_000_000),
            ..good()
        };
        assert!(!t.check(&leaky, 100.0).passed());
    }

    #[test]
    fn an_unmeasurable_rss_is_a_notice_never_a_silent_pass() {
        let b = Budgets::parse(STRESS).unwrap();
        let v = b.stress["soak/mixed"].check(&StressObserved::default(), 1.0);
        assert!(v.passed());
        assert_eq!(v.notices.len(), 1);
        assert!(v.notices[0].contains("RSS"), "{v:?}");
    }

    #[test]
    fn self_check_flags_a_measurement_outside_its_own_gate() {
        let t = StressBudget {
            min_per_sec: Some(100.0),
            p99_ns: Some(50.0),
            p999_ns: Some(60.0),
            bytes_per_op: Some(10.0),
            measured_per_sec: Some(99.0),
            measured_p99_ns: Some(51.0),
            measured_p999_ns: Some(61.0),
            measured_bytes_per_op: Some(11.0),
            ..StressBudget::default()
        };
        assert_eq!(t.self_check().len(), 4, "{:?}", t.self_check());
    }

    #[test]
    fn parses_a_size_table_and_its_gate() {
        let b = Budgets::parse(
            "[size.\"web/hello-wasm\"]\nbudget_gzip_bytes = 120_000\n\
             measured_gzip_bytes = 95838\ntolerance = 0.05\n",
        )
        .expect("parses");
        let size = &b.sizes["web/hello-wasm"];
        assert_eq!(size.unit, SizeUnit::Gzip);
        assert_eq!(size.budget, 120_000.0);
        assert_eq!(size.measured, Some(95_838.0));
        // floor(95,838 x 1.05) = 100,629: the record's tolerance is lower than the budget.
        assert_eq!(size.ceiling(), 100_629.0);
        assert!(size.self_check().is_empty());

        // Without a record, or with one so close to the budget that the tolerance would pass it,
        // the budget is the gate.
        let b = Budgets::parse("[size.\"a\"]\nbudget_gzip_bytes = 1000\n").unwrap();
        assert_eq!(b.sizes["a"].ceiling(), 1000.0);
        let b = Budgets::parse(
            "[size.\"a\"]\nbudget_gzip_bytes = 1000\nmeasured_gzip_bytes = 990\ntolerance = 0.05\n",
        )
        .unwrap();
        assert_eq!(b.sizes["a"].ceiling(), 1000.0);
        assert!(!b.sizes["a"].holds(1001.0));
    }

    #[test]
    fn a_size_table_is_read_strictly() {
        let err = Budgets::parse("[size.\"a\"]\nmeasured_gzip_bytes = 4\n").unwrap_err();
        assert_eq!(err, BudgetError::MissingSizeBudget { name: "a".into() });
        let err = Budgets::parse(
            "[size.\"a\"]\nbudget_gzip_bytes = 1\n[size.\"a\"]\nbudget_gzip_bytes = 2\n",
        )
        .unwrap_err();
        assert_eq!(err, BudgetError::DuplicateSize { name: "a".into() });
        let err = Budgets::parse("[size.\"a\"]\nbudget = 1\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[size.\"a\"]\nbudget_gzip_bytes = 0\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[size.a]\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");
        assert!(
            BudgetError::MissingSizeBudget { name: "a".into() }
                .to_string()
                .contains("budget_gzip_bytes")
        );

        let over =
            Budgets::parse("[size.\"a\"]\nbudget_gzip_bytes = 10\nmeasured_gzip_bytes = 11\n")
                .unwrap();
        assert_eq!(over.sizes["a"].self_check().len(), 1);
        let dangling =
            Budgets::parse("[size.\"a\"]\nbudget_gzip_bytes = 10\ntolerance = 0.1\n").unwrap();
        assert_eq!(dangling.sizes["a"].self_check().len(), 1);
    }

    #[test]
    fn a_native_size_table_counts_the_bytes_of_the_shipped_file() {
        // `scripts/native-size.sh` (ADR-052, "native size gates"): the same gate over raw bytes.
        let b = Budgets::parse(
            "[size.\"android/hello-arm64-v8a\"]\nbudget_bytes = 813_000\n\
             measured_bytes = 774_480\ntolerance = 0.05\n",
        )
        .expect("parses");
        let size = &b.sizes["android/hello-arm64-v8a"];
        assert_eq!(size.unit, SizeUnit::Raw);
        assert_eq!((size.budget, size.measured), (813_000.0, Some(774_480.0)));
        // floor(774,480 x 1.05) = 813,204 is over the budget of 813,000, so the budget is the gate.
        assert_eq!(size.ceiling(), 813_000.0);
        assert!(size.holds(813_000.0) && !size.holds(813_001.0));
        assert!(size.self_check().is_empty());
        assert_eq!(size.unit.record_field(), "bytes");
        assert_eq!(SizeUnit::Gzip.record_field(), "gzipped");

        // One pair or the other, never both in a table, and a record of the budget's own unit.
        for text in [
            "[size.\"a\"]\nbudget_gzip_bytes = 10\nbudget_bytes = 10\n",
            "[size.\"a\"]\nbudget_bytes = 10\nmeasured_gzip_bytes = 9\n",
            "[size.\"a\"]\nbudget_gzip_bytes = 10\nmeasured_bytes = 9\n",
        ] {
            assert_eq!(
                Budgets::parse(text).unwrap_err(),
                BudgetError::MixedSizeUnits { name: "a".into() },
                "{text}"
            );
        }
        let over =
            Budgets::parse("[size.\"a\"]\nbudget_bytes = 10\nmeasured_bytes = 11\n").unwrap();
        assert_eq!(over.sizes["a"].self_check().len(), 1);
        assert!(over.sizes["a"].self_check()[0].contains("measured_bytes"));
        let err = Budgets::parse("[size.\"a\"]\nbudget_bytes = 0\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
    }

    #[test]
    fn parses_a_web_table_and_reads_it_strictly() {
        let b = Budgets::parse(
            "[web.\"sync_call\"]\nbudget_ns = 3_900\nmeasured_ns = 780\nwhat = \"a call\"\n",
        )
        .unwrap();
        let row = &b.web["sync_call"];
        assert_eq!(row.budget_ns, 3900.0);
        assert_eq!(row.measured_ns, Some(780.0));
        assert_eq!(row.what.as_deref(), Some("a call"));
        assert!(row.self_check().is_empty());

        let err = Budgets::parse("[web.\"a\"]\nmeasured_ns = 4\n").unwrap_err();
        assert_eq!(err, BudgetError::MissingWebBudget { name: "a".into() });
        let err =
            Budgets::parse("[web.\"a\"]\nbudget_ns = 1\n[web.\"a\"]\nbudget_ns = 2\n").unwrap_err();
        assert_eq!(err, BudgetError::DuplicateWeb { name: "a".into() });
        let err = Budgets::parse("[web.\"a\"]\nbudget = 1\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[web.\"a\"]\nbudget_ns = 0\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 2, .. }), "{err}");
        let err = Budgets::parse("[web.a]\n").unwrap_err();
        assert!(matches!(err, BudgetError::Syntax { line: 1, .. }), "{err}");

        let over = Budgets::parse("[web.\"a\"]\nbudget_ns = 10\nmeasured_ns = 11\n").unwrap();
        assert_eq!(over.web["a"].self_check().len(), 1);
    }

    /// The number after `"key":` in a flat JSON object, as `scripts/wasm-size.sh` writes them.
    fn json_number(line: &str, key: &str) -> Option<f64> {
        let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
        let rest = line[at..].trim_start();
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        rest[..end].trim().parse().ok()
    }

    /// The string after `"key":` in a flat JSON object (no escapes needed for artefact names).
    fn json_string(line: &str, key: &str) -> Option<String> {
        let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
        let rest = line[at..].trim_start().strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_owned())
    }

    /// The record file a `[size."artifact"]` table is measured into: `scripts/wasm-size.sh` writes
    /// the web artefacts' (`web-size.jsonl`), `scripts/native-size.sh` the iOS and Android cores'
    /// (`native-size.jsonl`).
    fn record_file(artifact: &str) -> &'static str {
        if artifact.starts_with("web/") {
            "results/web-size.jsonl"
        } else {
            "results/native-size.jsonl"
        }
    }

    #[test]
    fn the_size_record_is_the_one_the_table_gates_against() {
        // `scripts/wasm-size.sh --record` and `scripts/native-size.sh --record` write both the record
        // and the table; a hand edit of one of them fails here.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let budgets = Budgets::load(&root.join("budgets.toml")).expect("bench/budgets.toml parses");
        let read = |file: &str| {
            std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("bench/{file} exists: {e}"))
        };
        for (name, size) in &budgets.sizes {
            let file = record_file(name);
            let record = read(file);
            let line = record
                .lines()
                .find(|l| json_string(l, "artifact").as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no line for {name} in {file}"));
            assert_eq!(
                json_number(line, size.unit.record_field()),
                size.measured,
                "{name}: the record and {} disagree",
                size.unit.measured_key()
            );
            assert_eq!(json_number(line, "budget"), Some(size.budget));
            assert_eq!(json_number(line, "ceiling"), Some(size.ceiling()));
        }
        // The web artefacts are gated (ADR-052 decision 2), and so are the native cores the
        // amendment of 2026-10-02 names.
        for gated in [
            "web/hello-wasm",
            "web/hello-runtime-js",
            "android/hello-arm64-v8a",
            "android/hello-x86_64",
            "ios/hello-arm64",
        ] {
            assert!(
                budgets.sizes.contains_key(gated),
                "{gated} has no [size] table"
            );
        }
        // Nothing is recorded without a gate.
        for file in ["results/web-size.jsonl", "results/native-size.jsonl"] {
            for line in read(file).lines().filter(|l| !l.trim().is_empty()) {
                let artifact =
                    json_string(line, "artifact").expect("every line names its artefact");
                assert_eq!(
                    record_file(&artifact),
                    file,
                    "{artifact} is in the wrong record"
                );
                assert!(
                    budgets.sizes.contains_key(&artifact),
                    "{artifact} is recorded in {file} but has no [size] table"
                );
            }
        }
    }
}
