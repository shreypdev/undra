//! The pieces every bench and the budgets test share: fixtures, a counting host and the
//! workload list. Included by path (`#[path = "../common/mod.rs"] mod common;`) rather than
//! compiled into the library, so the macro-generated registrations live in each binary's own
//! crate and can never be dropped by the linker (the same arrangement as `undra-ffi`'s
//! boundary bench).
#![allow(missing_docs, dead_code)]

pub mod fixtures;
pub mod host;
pub mod leaderboard;
pub mod ports;
pub mod query_rows;
pub mod stress;
pub mod workloads;

use std::time::{Duration, Instant};

use criterion::Criterion;
use undra_bench::workload::Workload;

/// A criterion configuration that keeps the whole suite to a few minutes: the operations are
/// stable enough that a short warm-up and two seconds of samples give tight medians.
pub fn criterion() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50)
        .configure_from_args()
}

/// Runs each workload as a criterion benchmark named after it.
pub fn run(c: &mut Criterion, workloads: Vec<Workload>) {
    for workload in workloads {
        let mut op = workload.build();
        if op.needs_reset() {
            // Reset outside the clock, one iteration at a time.
            c.bench_function(&workload.name, |b| {
                b.iter_custom(|iterations| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iterations {
                        op.reset();
                        let start = Instant::now();
                        op.run();
                        total += start.elapsed();
                    }
                    total
                });
            });
        } else {
            c.bench_function(&workload.name, |b| b.iter(|| op.run()));
        }
    }
}
