//! The leaderboard recipe's rows (`site/docs/cookbook/leaderboard.html`): how long a snapshot of
//! 100,000 players holds the core lock, with the recommended design and with two naive ones.
//!
//! A row times one `Runtime::call_sync` on the bench thread. A call holds the core lock for as long
//! as it runs, so the time of the call **is** the core-lock hold of one snapshot:
//!
//! | Row | What the call does under the lock |
//! |---|---|
//! | `leaderboard/lock_hold/recommended` | publishes the window the blocking pool prepared: three signals, one transaction (what `Board::publish` does) |
//! | `leaderboard/lock_hold/in_core_call` | parses, sorts and swaps the snapshot **inside the call**, then publishes the same window |
//! | `leaderboard/lock_hold/signal_of_rows` | parses and sorts inside the call and sets a `Signal<Vec<Place>>` of every player |
//! | `leaderboard/ingest/off_core` | (no lock) what the blocking pool does for the recommended design: parse, sort, swap, window |
//!
//! The structure is the recipe's own: `standings.rs` of the cookbook core is compiled into this
//! harness by path, so the rows measure the code the recipe shows. The recommended row's staging
//! (the pool's work) is the untimed `reset`; the ingest row times it separately. The stores here
//! are the benchmark's own, like every fixture in `fixtures.rs`: they carry the publish step of
//! `examples/cookbook/core/src/leaderboard.rs` and nothing else, so adding the cookbook's other
//! recipes to every benchmark binary does not change what the other rows measure.
//!
//! The release numbers are in `bench/RESULTS.md` (finding 9).
#![allow(missing_docs, dead_code)]

use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use undra::prelude::*;
use undra::signals::ALL_SIGNALS;
use undra_bench::stats::Histogram;
use undra_bench::workload::{Bench, Workload, plain, with_reset};

use super::host::{CopyingHost, call_ok, construct, method_call, runtime_with};

#[path = "../../examples/cookbook/core/src/standings.rs"]
pub mod standings;

use standings::{Ranked, Standings, View, synthetic_snapshot};

/// Players in the snapshot: the recipe's 100,000. An unoptimised build smoke-runs on 2,000.
pub const PLAYERS: u32 = if cfg!(debug_assertions) {
    2_000
} else {
    100_000
};
/// The followed player.
const ME: u32 = 777;
/// Rows published as the top, and on each side of the followed player (the recipe's numbers).
const TOP: usize = 50;
const AROUND: usize = 3;

/// One line of the board, as the benchmark's stores publish it.
#[undra::api]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub rank: u32,
    pub id: u32,
    pub score: u32,
}

/// The four numbers about the whole ranking.
#[undra::api]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tally {
    pub players: u32,
    pub top_score: u32,
    pub median_score: u32,
    pub my_rank: u32,
}

fn place(r: &Ranked) -> Place {
    Place {
        rank: r.rank,
        id: r.id,
        score: r.score,
    }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The app-owned structure of the recipe, as a blocking-pool thread holds it: behind its own lock,
/// never taken by the core.
pub struct Ranking {
    snapshot: Vec<u8>,
    standings: Mutex<Standings>,
    staged: Mutex<Option<View>>,
}

impl Ranking {
    fn new(players: u32) -> Ranking {
        Ranking {
            snapshot: synthetic_snapshot(1, players),
            standings: Mutex::new(Standings::new()),
            staged: Mutex::new(None),
        }
    }

    /// What `Board::load` does on the blocking pool: parse and sort with no lock, swap under the
    /// structure's own lock, and read the window. Nothing here takes the core lock.
    pub fn ingest(&self) -> View {
        let fresh = Standings::from_snapshot(&self.snapshot).expect("the synthetic snapshot reads");
        let mut standings = locked(&self.standings);
        *standings = fresh;
        standings.view(Some(ME), TOP, AROUND)
    }

    /// Leaves `view` where the next `publish_staged` call finds it: what the awaiting task holds
    /// when the blocking pool's closure returns.
    pub fn stage(&self, view: View) {
        *locked(&self.staged) = Some(view);
    }

    pub fn snapshot_len(&self) -> usize {
        self.snapshot.len()
    }
}

/// The recipe's store: three small signals.
#[undra::store(restore = "Self::rebuild")]
pub struct Standing {
    ranking: Arc<Ranking>,
    top: Signal<Vec<Place>>,
    around: Signal<Vec<Place>>,
    summary: Signal<Tally>,
}

impl Standing {
    /// The recipe's `Board::publish`, but every write unconditional: the worst case, a window
    /// that changed in every signal.
    fn publish(&self, view: &View) {
        let top: Vec<Place> = view.top.iter().map(place).collect();
        let around: Vec<Place> = view.around.iter().map(place).collect();
        let summary = Tally {
            players: view.players,
            top_score: view.top_score,
            median_score: view.median_score,
            my_rank: view.me.map_or(0, |m| m.rank),
        };
        txn(|| {
            self.top.set(top);
            self.around.set(around);
            self.summary.set(summary);
        });
    }
}

#[undra::api(store)]
impl Standing {
    pub fn new(ctx: Ctx) -> Self {
        Self::rebuild(
            ctx,
            Signal::new(vec![]),
            Signal::new(vec![]),
            Signal::new(Tally {
                players: 0,
                top_score: 0,
                median_score: 0,
                my_rank: 0,
            }),
        )
    }

    // The fixtures are never restored; this is the one place a `Standing` is built.
    fn rebuild(
        _ctx: Ctx,
        top: Signal<Vec<Place>>,
        around: Signal<Vec<Place>>,
        summary: Signal<Tally>,
    ) -> Self {
        Standing {
            ranking: Arc::new(Ranking::new(PLAYERS)),
            top,
            around,
            summary,
        }
    }

    /// A call that does nothing: what a UI call that merely needs the core lock costs, and how
    /// long it waits when someone else holds it.
    pub fn ping(&self) -> u32 {
        1
    }

    /// The recommended design's step on the core: the pool's closure returned a window, the
    /// awaiting task writes it. Takes the staged window.
    pub fn publish_staged(&self) {
        let view = locked(&self.ranking.staged)
            .take()
            .expect("a window was staged");
        self.publish(&view);
    }

    /// The first naive design: the snapshot is applied inside the call (parse, sort, swap), and
    /// the same window is published.
    pub fn apply_in_call(&self) -> u32 {
        let view = self.ranking.ingest();
        self.publish(&view);
        view.players
    }
}

/// The second naive design: every player in a signal.
#[undra::store(restore = "Self::rebuild")]
pub struct Roster {
    snapshot: Arc<Vec<u8>>,
    players: Signal<Vec<Place>>,
}

#[undra::api(store)]
impl Roster {
    pub fn new(ctx: Ctx) -> Self {
        Self::rebuild(ctx, Signal::new(vec![]))
    }

    // The fixtures are never restored; this is the one place a `Roster` is built.
    fn rebuild(_ctx: Ctx, players: Signal<Vec<Place>>) -> Self {
        Roster {
            snapshot: Arc::new(synthetic_snapshot(1, PLAYERS)),
            players,
        }
    }

    /// Parses and sorts the snapshot inside the call and publishes every player with its rank.
    pub fn apply_in_call(&self) -> u32 {
        let standings =
            Standings::from_snapshot(&self.snapshot).expect("the synthetic snapshot reads");
        let rows: Vec<Place> = standings.ranking().iter().map(place).collect();
        let count = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        self.players.set(rows);
        count
    }
}

/// The four rows, added to `workloads::all()` and `group("leaderboard")`.
pub fn workloads() -> Vec<Workload> {
    vec![
        Workload::new("leaderboard/lock_hold/recommended", recommended),
        Workload::new("leaderboard/lock_hold/in_core_call", in_core_call),
        Workload::new("leaderboard/lock_hold/signal_of_rows", signal_of_rows),
        Workload::new("leaderboard/ingest/off_core", off_core),
    ]
}

/// A runtime whose host copies every change-set, as every platform's FFI callback does (and the
/// callback runs while the core lock may be held).
fn core() -> (Arc<super::host::Core>, Arc<CopyingHost>) {
    let host = Arc::new(CopyingHost::default());
    (runtime_with(host.clone(), 0), host)
}

fn recommended() -> Box<dyn Bench> {
    let (rt, host) = core();
    let store = construct(&rt, "Standing", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let standing = rt.object::<Standing>(store.0).expect("the store");
    let ranking = standing.ranking.clone();
    assert!(
        PLAYERS < 100_000 || ranking.snapshot_len() > 1_000_000,
        "the snapshot is the recipe's megabyte and a half"
    );
    // The blocking pool's work, once, off the core: the window every run publishes.
    let view = ranking.ingest();
    assert_eq!(view.top.len(), TOP);
    assert_eq!(view.around.len(), 2 * AROUND + 1);
    assert_eq!(view.players, PLAYERS);
    let payload = method_call(store, "Standing", "publish_staged", 2, &[]);
    let (sets, bytes) = (host.counts.change_sets(), host.counts.change_set_bytes());
    ranking.stage(view.clone());
    call_ok(&rt, &payload);
    assert_eq!(
        host.counts.change_sets() - sets,
        1,
        "the window is one change-set"
    );
    let shipped = host.counts.change_set_bytes() - bytes;
    assert_eq!(
        shipped, 771,
        "the window is 771 bytes whatever the players: 50 + 7 rows of 12 bytes and four numbers"
    );
    with_reset(
        move || {
            black_box(rt.call_sync(black_box(&payload)));
        },
        move || ranking.stage(view.clone()),
    )
}

fn in_core_call() -> Box<dyn Bench> {
    let (rt, host) = core();
    let store = construct(&rt, "Standing", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let payload = method_call(store, "Standing", "apply_in_call", 2, &[]);
    let (sets, bytes) = (host.counts.change_sets(), host.counts.change_set_bytes());
    call_ok(&rt, &payload);
    assert_eq!(host.counts.change_sets() - sets, 1);
    assert_eq!(host.counts.change_set_bytes() - bytes, 771);
    plain(move || {
        black_box(rt.call_sync(black_box(&payload)));
    })
}

fn signal_of_rows() -> Box<dyn Bench> {
    let (rt, host) = core();
    let store = construct(&rt, "Roster", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let payload = method_call(store, "Roster", "apply_in_call", 2, &[]);
    let (sets, bytes) = (host.counts.change_sets(), host.counts.change_set_bytes());
    call_ok(&rt, &payload);
    assert_eq!(host.counts.change_sets() - sets, 1);
    let shipped = host.counts.change_set_bytes() - bytes;
    assert_eq!(
        shipped,
        u64::from(PLAYERS) * 12 + 33,
        "every player crosses the boundary, 12 bytes each"
    );
    plain(move || {
        black_box(rt.call_sync(black_box(&payload)));
    })
}

fn off_core() -> Box<dyn Bench> {
    let ranking = Ranking::new(PLAYERS);
    assert_eq!(ranking.ingest().players, PLAYERS);
    plain(move || {
        black_box(ranking.ingest());
    })
}

/// How long the probe pauses between two calls.
const PROBE_PAUSE: Duration = Duration::from_micros(20);

/// What a caller that only needs the core lock saw while snapshots were being applied.
pub struct Stall {
    /// Snapshots applied during the run.
    pub snapshots: u64,
    /// Calls the probe made.
    pub probes: u64,
    /// How long a probe call took (it waits while someone holds the core lock), in nanoseconds.
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub max_ns: u64,
}

/// Applies a snapshot of [`PLAYERS`] players every few milliseconds for `run`, with the recommended
/// design (`naive == false`: the pool's work off the core, then the publish) or with the snapshot
/// applied inside a call (`naive == true`), while this thread makes a call that does nothing
/// in a loop. What the probe waits is what a UI call waits: the core lock hold, seen by a caller
/// (`bench/RESULTS.md`, finding 9).
pub fn stall(naive: bool, run: Duration) -> Stall {
    let (rt, _host) = core();
    let store = construct(&rt, "Standing", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let ranking = rt
        .object::<Standing>(store.0)
        .expect("the store")
        .ranking
        .clone();
    let ping = method_call(store, "Standing", "ping", 2, &[]);
    let publish = method_call(store, "Standing", "publish_staged", 3, &[]);
    let apply = method_call(store, "Standing", "apply_in_call", 4, &[]);
    call_ok(&rt, &ping);
    // Warm up: the first snapshot allocates the structure.
    ranking.stage(ranking.ingest());
    call_ok(&rt, &publish);

    let done = AtomicBool::new(false);
    let snapshots = AtomicU64::new(0);
    let mut hist = Histogram::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if naive {
                    call_ok(&rt, &apply);
                } else {
                    // On a blocking-pool thread in the recipe; here, a thread without the core lock.
                    ranking.stage(ranking.ingest());
                    call_ok(&rt, &publish);
                }
                snapshots.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        // A closed loop: one call, a pause, the next. A call that waits out a core-lock hold is one
        // slow sample, so the percentiles say how often a caller is held up and `max` says for how long
        // (a probe that is itself descheduled by a busy machine adds its own noise: best of several runs).
        let start = Instant::now();
        while start.elapsed() < run {
            let t = Instant::now();
            black_box(rt.call_sync(black_box(&ping)));
            hist.record(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            std::thread::sleep(PROBE_PAUSE);
        }
        done.store(true, Ordering::Relaxed);
    });
    Stall {
        snapshots: snapshots.load(Ordering::Relaxed),
        probes: hist.count(),
        p50_ns: hist.percentile(0.50),
        p99_ns: hist.percentile(0.99),
        p999_ns: hist.percentile(0.999),
        max_ns: hist.max(),
    }
}
