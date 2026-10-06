//! Criterion benches for the leaderboard recipe (`leaderboard/*`): how long a snapshot of 100,000
//! players holds the core lock with the recommended design and with the naive ones. See
//! `common/leaderboard.rs` for what each one measures and `bench/RESULTS.md` for the numbers.

use criterion::{Criterion, criterion_group, criterion_main};

#[path = "../common/mod.rs"]
mod common;

fn leaderboard(c: &mut Criterion) {
    common::run(c, common::workloads::group("leaderboard"));
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = leaderboard
}
criterion_main!(benches);
