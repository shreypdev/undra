//! Plain `Instant` timing: what the budgets test uses instead of criterion.
//!
//! Criterion is the right tool for a human comparing two commits; it is the wrong tool for a CI
//! gate (slow, statistical, output to interpret). This routine has one job: estimate the **p50**
//! cost of one iteration of an operation, robustly enough that a shared CI runner does not
//! flake and a real regression still shows.
//!
//! How: after a warm-up that also estimates the cost of one iteration, the operation runs in
//! `samples` batches; each batch is timed with one pair of clock reads and divided by its size,
//! so the clock's own cost (tens of nanoseconds) does not drown operations that take tens of
//! nanoseconds. The p50 over the batch means is the answer; one noisy neighbour costs one
//! sample, not the result. Operations that need their state reset before each run are timed one
//! iteration at a time instead (they are all microseconds or more, so the clock is negligible).

use std::time::{Duration, Instant};

use crate::workload::Bench;

/// How long and how often to run an operation.
#[derive(Clone, Copy, Debug)]
pub struct MeasureConfig {
    /// Stop growing the iteration count once a measurement would take longer than this.
    pub time_cap: Duration,
    /// The fewest samples (and, for slow operations, iterations) taken whatever the cost.
    pub min_samples: usize,
    /// The most iterations in total; fast operations reach it long before `time_cap`.
    pub max_iterations: u64,
    /// The most samples for an operation that has to be reset between runs.
    pub max_reset_samples: usize,
    /// Iterations of warm-up (caches, allocator, branch predictors) before measuring.
    pub warmup_iterations: u32,
}

impl Default for MeasureConfig {
    fn default() -> Self {
        MeasureConfig {
            time_cap: Duration::from_millis(300),
            min_samples: 301,
            max_iterations: 300_000,
            max_reset_samples: 3_000,
            warmup_iterations: 50,
        }
    }
}

/// What one measurement found. All times are nanoseconds per iteration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// The median of the samples: the number a budget is checked against.
    pub p50_ns: f64,
    /// The 90th percentile: how bad the slow tail was.
    pub p90_ns: f64,
    /// The 99th percentile: the tail a row can gate with `p99_ns` in `budgets.toml`. With the
    /// default 301 samples it is the third slowest; an operation that is reset between runs is
    /// timed one run per sample, up to 3,000 samples.
    pub p99_ns: f64,
    /// The fastest sample.
    pub min_ns: f64,
    /// How many samples were taken.
    pub samples: usize,
    /// How many times the operation ran in the timed region, all samples together.
    pub iterations: u64,
}

/// Measures `bench` with `config`.
pub fn measure(bench: &mut dyn Bench, config: &MeasureConfig) -> Stats {
    let resets = bench.needs_reset();

    // Warm up, and learn roughly what one iteration costs (only `run` is counted).
    let mut warm = Duration::ZERO;
    let warmup = config.warmup_iterations.max(1);
    for _ in 0..warmup {
        if resets {
            bench.reset();
        }
        let start = Instant::now();
        bench.run();
        warm += start.elapsed();
    }
    let estimate_ns = (warm.as_nanos() as f64 / f64::from(warmup)).max(1.0);
    let affordable = (config.time_cap.as_nanos() as f64 / estimate_ns) as u64;

    let mut samples: Vec<f64>;
    let iterations = if resets {
        let count = affordable.clamp(config.min_samples as u64, config.max_reset_samples as u64);
        samples = Vec::with_capacity(count as usize);
        for _ in 0..count {
            bench.reset();
            let start = Instant::now();
            bench.run();
            samples.push(start.elapsed().as_nanos() as f64);
        }
        count
    } else {
        let total = affordable.clamp(config.min_samples as u64, config.max_iterations);
        let batches = config.min_samples as u64;
        let batch = (total / batches).max(1);
        samples = Vec::with_capacity(batches as usize);
        for _ in 0..batches {
            let start = Instant::now();
            for _ in 0..batch {
                bench.run();
            }
            samples.push(start.elapsed().as_nanos() as f64 / batch as f64);
        }
        batches * batch
    };

    samples.sort_by(f64::total_cmp);
    let at = |fraction: f64| {
        samples[((samples.len() as f64 * fraction) as usize).min(samples.len() - 1)]
    };
    Stats {
        p50_ns: at(0.5),
        p90_ns: at(0.9),
        p99_ns: at(0.99),
        min_ns: samples[0],
        samples: samples.len(),
        iterations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{plain, with_reset};
    use std::cell::Cell;
    use std::rc::Rc;

    fn quick() -> MeasureConfig {
        MeasureConfig {
            time_cap: Duration::from_millis(20),
            min_samples: 11,
            max_iterations: 2_000,
            max_reset_samples: 50,
            warmup_iterations: 5,
        }
    }

    #[test]
    fn measures_a_known_sleep_within_a_loose_band() {
        let mut op = plain(|| std::thread::sleep(Duration::from_micros(300)));
        let stats = measure(&mut *op, &quick());
        assert!(stats.p50_ns >= 300_000.0, "{stats:?}");
        assert!(stats.p50_ns < 30_000_000.0, "{stats:?}");
        assert!(stats.min_ns <= stats.p50_ns && stats.p50_ns <= stats.p90_ns);
        assert!(stats.p90_ns <= stats.p99_ns);
        assert_eq!(stats.samples, 11);
    }

    #[test]
    fn a_fast_operation_runs_many_times() {
        let count = Rc::new(Cell::new(0_u64));
        let mut op = plain({
            let count = count.clone();
            move || count.set(count.get() + 1)
        });
        let stats = measure(&mut *op, &quick());
        // Warm-up plus the timed region: the timed iterations are what `iterations` reports.
        assert_eq!(count.get(), 5 + stats.iterations);
        assert!(stats.iterations >= 1_000, "{stats:?}");
    }

    #[test]
    fn reset_runs_before_every_iteration_and_is_not_timed() {
        let resets = Rc::new(Cell::new(0_u64));
        let runs = Rc::new(Cell::new(0_u64));
        let mut op = with_reset(
            {
                let runs = runs.clone();
                move || runs.set(runs.get() + 1)
            },
            {
                let resets = resets.clone();
                move || {
                    resets.set(resets.get() + 1);
                    // Far more expensive than the operation: must not show in the result.
                    std::thread::sleep(Duration::from_millis(2));
                }
            },
        );
        let stats = measure(&mut *op, &quick());
        assert_eq!(resets.get(), runs.get());
        assert_eq!(runs.get(), 5 + stats.iterations);
        assert!(
            stats.p50_ns < 1_000_000.0,
            "reset leaked into the timing: {stats:?}"
        );
    }
}
