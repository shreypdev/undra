//! A live leaderboard of 100,000 players: a big structure that stays off the core, and the small
//! window of it that the UI shows.
//!
//! A snapshot of about 1.4 MB arrives, then a stream of score deltas. The tempting designs put the
//! work, or the players, in the core: parse the snapshot inside a call (the core lock is held for
//! the whole parse), or publish a `Signal<Vec<Player>>` of 100,000 rows (every change re-encodes
//! and ships the list). This recipe does neither.
//!
//! * **Ingest on the blocking pool.** `Board::load` and `Board::apply` hand the bytes to
//!   `spawn_blocking`. The closure parses, sorts and merges into a [`Standings`] that the app owns
//!   behind its own `Mutex`, without the core lock. The parse needs no lock at all: it builds a
//!   finished value, and the lock is held for the swap.
//! * **Publish a window, once.** The closure returns a [`View`] (the top 50, a few rows around the
//!   followed player, and four aggregates). The awaiting task takes it back to the core and writes
//!   three signals in one transaction (`Board::publish`): the core lock is held for as long as
//!   that takes, not for as long as the snapshot takes. Each window carries the version it was
//!   read at, so two updates whose tasks resume in the other order never leave the older window
//!   on screen.
//! * **Nothing reads the big structure on the core.** `follow` moves the window and goes through
//!   the same two steps. No generated getter, no signal and no call ever holds 100,000 rows.
//!
//! The feed is deterministic (`standings::synthetic_snapshot`, seeded from the `Rng` port) and
//! [`Leaderboard::start_demo`] drives it on the `Timer` port, so a test moves it with the fake clock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use undra::ports::HttpRequest;
use undra::prelude::*;

use crate::net::{self, NetError};
use crate::standings::{
    Ranked, SnapshotError, Standings, View, synthetic_deltas, synthetic_snapshot,
};

/// How many of the best players are published.
pub const TOP: usize = 50;
/// How many players are published on each side of the followed one.
pub const AROUND: usize = 3;
/// How often the demo feed sends a batch of deltas.
const TICK: Duration = Duration::from_millis(100);

/// One line of the leaderboard.
#[undra::api]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The competition rank: players with the same score share one.
    pub rank: u32,
    /// The player's identity.
    pub id: u32,
    /// The player's score.
    pub score: u32,
}

/// What the whole ranking comes to, in four numbers.
#[undra::api]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// How many players are ranked.
    pub players: u32,
    /// The highest score.
    pub top_score: u32,
    /// The score in the middle of the ranking.
    pub median_score: u32,
    /// The followed player and where it stands, when it is ranked.
    pub me: Option<Row>,
}

/// A change of one player's score, as the feed sends it.
#[undra::api]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delta {
    /// The player.
    pub id: u32,
    /// Points gained (positive) or lost (negative). The score stays between 0 and the maximum.
    pub points: i32,
}

/// Why the leaderboard could not be updated.
#[undra::error]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaderboardError {
    /// The snapshot could not be fetched.
    #[error("{0}")]
    Net(#[from] NetError),
    /// The snapshot is not readable. The ranking the leaderboard had is untouched.
    #[error("bad snapshot: {0}")]
    BadSnapshot(String),
    /// The core is shutting down.
    #[error("the core is shutting down")]
    Closed,
}

impl From<SnapshotError> for LeaderboardError {
    fn from(error: SnapshotError) -> Self {
        LeaderboardError::BadSnapshot(error.to_string())
    }
}

impl From<&Ranked> for Row {
    fn from(r: &Ranked) -> Row {
        Row {
            rank: r.rank,
            id: r.id,
            score: r.score,
        }
    }
}

// docs:begin leaderboard-store
/// The big structure and who is followed, behind the app's own lock. Only blocking-pool threads
/// take it; the core never does.
#[derive(Default)]
struct Inner {
    standings: Standings,
    me: Option<u32>,
    version: u64, // bumped by every change, under this lock
}

impl Inner {
    /// The window, and the version it shows: read under the lock, right after a change.
    fn window(&mut self) -> (u64, View) {
        self.version += 1;
        (self.version, self.standings.view(self.me, TOP, AROUND))
    }
}
// docs:end

/// What the store and its tasks share.
#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    /// The newest version written to the signals (read and written on the core only).
    published: AtomicU64,
    /// Bumped by `stop_demo`: a demo task of an older generation ends.
    generation: AtomicU64,
    running: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// docs:begin leaderboard-store
/// The leaderboard's store: three small signals, and a handle on the structure that is not one.
#[undra::store(restore = "Self::assemble")]
pub struct Leaderboard {
    ctx: WeakCtx,
    shared: Arc<Shared>, // holds the Mutex<Inner>: the 100,000 players
    top: Signal<Vec<Row>>,
    around: Signal<Vec<Row>>,
    summary: Signal<Summary>,
}
// docs:end

/// What a store method and the demo task both need: the shared structure and the three signals.
/// Cheap to clone (handles), so a task can own one.
#[derive(Clone)]
struct Board {
    ctx: WeakCtx,
    shared: Arc<Shared>,
    top: Signal<Vec<Row>>,
    around: Signal<Vec<Row>>,
    summary: Signal<Summary>,
}

fn empty_summary() -> Summary {
    Summary {
        players: 0,
        top_score: 0,
        median_score: 0,
        me: None,
    }
}

impl Board {
    // docs:begin leaderboard-ingest
    /// Parses a snapshot on the blocking pool, swaps it in and publishes the window.
    async fn load(&self, bytes: Vec<u8>) -> Result<u32, LeaderboardError> {
        let ctx = self.ctx.upgrade().map_err(|_| LeaderboardError::Closed)?;
        let shared = self.shared.clone();
        let work = ctx.spawn_blocking(move || -> Result<(u64, View), SnapshotError> {
            // Milliseconds of parsing and sorting, with no lock held at all.
            let fresh = Standings::from_snapshot(&bytes)?;
            // The lock is held for the swap and the window, not for the parse.
            let mut inner = lock(&shared.inner);
            inner.standings = fresh;
            Ok(inner.window())
        });
        drop(ctx);
        let (version, view) = work.await?; // back on the core, with 50 rows and four numbers
        self.publish(version, &view);
        Ok(view.players)
    }
    // docs:end

    /// Merges a batch of deltas on the blocking pool and publishes the window. Returns how many
    /// deltas found their player.
    async fn apply(&self, deltas: Vec<(u32, i32)>) -> Result<u32, LeaderboardError> {
        let ctx = self.ctx.upgrade().map_err(|_| LeaderboardError::Closed)?;
        let shared = self.shared.clone();
        let work = ctx.spawn_blocking(move || {
            let mut inner = lock(&shared.inner);
            let applied = inner.standings.apply(&deltas);
            (applied, inner.window())
        });
        drop(ctx);
        let (applied, (version, view)) = work.await;
        self.publish(version, &view);
        Ok(u32::try_from(applied).unwrap_or(u32::MAX))
    }

    /// Moves the window to `id` (or to nobody) on the blocking pool and publishes it.
    async fn follow(&self, id: Option<u32>) -> Result<(), LeaderboardError> {
        let ctx = self.ctx.upgrade().map_err(|_| LeaderboardError::Closed)?;
        let shared = self.shared.clone();
        let work = ctx.spawn_blocking(move || {
            let mut inner = lock(&shared.inner);
            inner.me = id;
            inner.window()
        });
        drop(ctx);
        let (version, view) = work.await;
        self.publish(version, &view);
        Ok(())
    }

    // docs:begin leaderboard-publish
    /// Writes the window: three signals, one transaction, and nothing that grows with the players.
    /// This is the only part of an update that runs on the core.
    fn publish(&self, version: u64, view: &View) {
        // Two updates can come back from the pool in either order: never write an older window.
        if self.shared.published.fetch_max(version, Ordering::SeqCst) >= version {
            return;
        }
        let top: Vec<Row> = view.top.iter().map(Row::from).collect();
        let around: Vec<Row> = view.around.iter().map(Row::from).collect();
        let summary = Summary {
            players: view.players,
            top_score: view.top_score,
            median_score: view.median_score,
            me: view.me.as_ref().map(Row::from),
        };
        txn(|| {
            // A write is a change-set whether or not the value changed: skip what did not.
            if self.top.with(|have| *have != top) {
                self.top.set(top);
            }
            if self.around.with(|have| *have != around) {
                self.around.set(around);
            }
            if self.summary.with(|have| *have != summary) {
                self.summary.set(summary);
            }
        });
    }
    // docs:end

    fn stopped(&self, mine: u64) -> bool {
        self.shared.generation.load(Ordering::SeqCst) != mine
    }

    /// The demo task: a synthetic snapshot, then a batch of deltas every `TICK` on the `Timer`
    /// port. It holds a `WeakCtx`, never a `Ctx` across a wait (ADR-034).
    async fn run_demo(self, players: u32, per_tick: usize, seed: u64, mine: u64) {
        let Ok(ctx) = self.ctx.upgrade() else { return };
        // Generating 1.4 MB of feed is blocking work too.
        let snapshot = ctx.spawn_blocking(move || synthetic_snapshot(seed, players));
        drop(ctx);
        let snapshot = snapshot.await;
        if self.stopped(mine) || self.load(snapshot).await.is_err() {
            return;
        }
        let mut tick: u64 = 0;
        while self.ctx.sleep(TICK).await.is_ok() {
            if self.stopped(mine) {
                return;
            }
            tick += 1;
            let batch_seed = seed.wrapping_add(tick.wrapping_mul(0xA076_1D64_78BD_642F));
            if self
                .apply(synthetic_deltas(batch_seed, players, per_tick))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

#[undra::api(store)]
impl Leaderboard {
    /// An empty leaderboard. Call `load_snapshot`, or `start_demo` for a synthetic feed.
    pub fn new(ctx: Ctx) -> Self {
        Self::assemble(
            ctx,
            Signal::new(vec![]),
            Signal::new(vec![]),
            Signal::new(empty_summary()),
        )
    }

    // Used by `new` and, through `restore = ".."`, to rebuild the store from a snapshot. The players
    // are not in a snapshot (that is the point), so a restored store has none: whatever window the
    // snapshot held is cleared, and the app loads the standings again.
    fn assemble(
        ctx: Ctx,
        top: Signal<Vec<Row>>,
        around: Signal<Vec<Row>>,
        summary: Signal<Summary>,
    ) -> Self {
        if top.with(|rows| !rows.is_empty()) {
            top.set(vec![]);
        }
        if around.with(|rows| !rows.is_empty()) {
            around.set(vec![]);
        }
        if summary.get() != empty_summary() {
            summary.set(empty_summary());
        }
        Self {
            ctx: ctx.downgrade(),
            shared: Arc::new(Shared::default()),
            top,
            around,
            summary,
        }
    }

    fn board(&self) -> Board {
        Board {
            ctx: self.ctx.clone(),
            shared: self.shared.clone(),
            top: self.top.clone(),
            around: self.around.clone(),
            summary: self.summary.clone(),
        }
    }

    /// Fetches the snapshot at `path` on the configured server and ranks it. Returns how many
    /// players there are. A snapshot that cannot be read leaves the current ranking as it was.
    pub async fn load_snapshot(&self, path: String) -> Result<u32, LeaderboardError> {
        let ctx = self.ctx.upgrade().map_err(|_| LeaderboardError::Closed)?;
        let url = net::url(&ctx, &path)?;
        let body = net::ok(net::send(&ctx, HttpRequest::get(url)).await?)?;
        drop(ctx);
        self.board().load(body).await
    }

    /// Adds a batch of score changes. Returns how many found their player (a change for a player
    /// the snapshot did not hold is ignored). Call it from whatever receives the feed.
    pub async fn apply_deltas(&self, deltas: Vec<Delta>) -> Result<u32, LeaderboardError> {
        let deltas = deltas.into_iter().map(|d| (d.id, d.points)).collect();
        self.board().apply(deltas).await
    }

    /// Shows the rows around the player `id`, and where it stands in the summary. No id shows nobody.
    pub async fn follow(&self, id: Option<u32>) -> Result<(), LeaderboardError> {
        self.board().follow(id).await
    }

    /// Starts a synthetic feed instead of a server: `players` players, and about `per_second` score
    /// changes a second in batches every 100 ms. The feed is seeded from the `Rng` port, so a test
    /// with the fake port sees the same players every run. Does nothing if it is already running.
    pub fn start_demo(&self, players: u32, per_second: u32) {
        let Ok(ctx) = self.ctx.upgrade() else { return };
        if self.shared.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let seed = {
            let bytes = ctx.rng().fill(8).0;
            bytes
                .iter()
                .take(8)
                .fold(0_u64, |seed, byte| (seed << 8) | u64::from(*byte))
        };
        let mine = self.shared.generation.load(Ordering::SeqCst);
        let per_tick = (per_second / 10) as usize;
        ctx.spawn(self.board().run_demo(players, per_tick, seed, mine));
    }

    /// Stops the synthetic feed. The ranking stays as it is.
    pub fn stop_demo(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use undra::meta::ids;
    use undra::ports::HttpResponse;
    use undra::ports::fakes::Matcher;
    use undra::wire::payload::{CallTarget, ChangeSet, ReplyStatus};
    use undra::wire::{Decode, Handle, Reader};

    use super::*;
    use crate::net::testing::{App, BASE};
    use crate::standings::MAX_SCORE;

    fn serve(app: &App, players: u32) -> Vec<u8> {
        let snapshot = synthetic_snapshot(11, players);
        app.fakes.http.respond(
            Matcher::get(format!("{BASE}/standings")),
            HttpResponse::new(200, snapshot.clone()),
        );
        snapshot
    }

    fn ranks(rows: &[Row]) -> Vec<u32> {
        rows.iter().map(|r| r.rank).collect()
    }

    #[test]
    fn a_snapshot_of_a_hundred_thousand_players_publishes_a_window_of_sixty_rows() {
        let app = App::new();
        let snapshot = serve(&app, 100_000);
        assert!(snapshot.len() > 1_000_000, "{} bytes", snapshot.len());
        let board = Leaderboard::new(app.ctx());
        assert_eq!(
            app.run(board.load_snapshot("/standings".into())),
            Ok(100_000)
        );
        let top = board.top.get();
        assert_eq!(top.len(), TOP);
        assert!(top.windows(2).all(|w| w[0].score >= w[1].score));
        assert_eq!(top[0].rank, 1);
        let summary = board.summary.get();
        assert_eq!(summary.players, 100_000);
        assert_eq!(summary.top_score, top[0].score);
        assert!(summary.median_score > 0 && summary.median_score < summary.top_score);
        assert_eq!(summary.me, None);
        assert!(board.around.with(Vec::is_empty), "nobody is followed yet");
    }

    #[test]
    fn following_a_player_shows_its_neighbours_and_its_rank() {
        let app = App::new();
        serve(&app, 5_000);
        let board = Leaderboard::new(app.ctx());
        app.run(board.load_snapshot("/standings".into())).unwrap();
        app.run(board.follow(Some(1_234))).unwrap();
        let around = board.around.get();
        assert_eq!(around.len(), 2 * AROUND + 1);
        let me = board.summary.get().me.expect("player 1234 is ranked");
        assert_eq!(me.id, 1_234);
        assert!(around.contains(&me));
        assert!(
            around.windows(2).all(|w| w[0].rank <= w[1].rank),
            "{:?}",
            ranks(&around)
        );
        app.run(board.follow(None)).unwrap();
        assert!(board.around.with(Vec::is_empty));
        assert_eq!(board.summary.get().me, None);
    }

    #[test]
    fn deltas_reorder_the_window() {
        let app = App::new();
        serve(&app, 5_000);
        let board = Leaderboard::new(app.ctx());
        app.run(board.load_snapshot("/standings".into())).unwrap();
        app.run(board.follow(Some(77))).unwrap();
        let top_score = board.summary.get().top_score;
        // Player 77 jumps to the top: a delta large enough to beat everyone, then saturate.
        let applied = app.run(board.apply_deltas(vec![
            Delta {
                id: 77,
                points: i32::try_from(MAX_SCORE).unwrap(),
            },
            Delta {
                id: 999_999,
                points: 5,
            },
        ]));
        assert_eq!(applied, Ok(1), "the unknown player is ignored");
        assert_eq!(board.top.get()[0].id, 77);
        let me = board.summary.get().me.unwrap();
        assert_eq!((me.id, me.rank, me.score), (77, 1, MAX_SCORE));
        assert!(board.summary.get().top_score >= top_score);
        assert_eq!(
            board.around.get()[0].rank,
            1,
            "the window moved with the player"
        );
    }

    #[test]
    fn a_bad_snapshot_is_a_typed_error_and_leaves_the_ranking_alone() {
        let app = App::new();
        serve(&app, 500);
        let board = Leaderboard::new(app.ctx());
        app.run(board.load_snapshot("/standings".into())).unwrap();
        let before = board.top.get();
        app.fakes.http.reset();
        app.fakes.http.respond(
            Matcher::get(format!("{BASE}/standings")),
            HttpResponse::new(200, b"[[1,2],[1,3]]".to_vec()),
        );
        assert!(matches!(
            app.run(board.load_snapshot("/standings".into())),
            Err(LeaderboardError::BadSnapshot(_))
        ));
        assert_eq!(board.top.get(), before);
        assert_eq!(board.summary.get().players, 500);
        // The structure itself is untouched, not only the signals: a window read from it again is
        // the same one.
        assert_eq!(app.run(board.apply_deltas(vec![])), Ok(0));
        assert_eq!(board.top.get(), before);
        assert_eq!(board.summary.get().players, 500);
        app.fakes.http.reset();
        app.fakes
            .http
            .respond(Matcher::any(), HttpResponse::new(503, b"down".to_vec()));
        assert_eq!(
            app.run(board.load_snapshot("/standings".into())),
            Err(LeaderboardError::Net(NetError::Status { code: 503 }))
        );
    }

    #[test]
    fn updates_that_resume_out_of_order_never_leave_an_older_window_on_screen() {
        // Eight batches of +1 for one player, awaited together: the pool takes them in some order
        // and the core resumes them in another (here, in the order they are polled). Whatever
        // the order, the window on screen at the end is the newest one.
        let app = App::new();
        serve(&app, 5_000);
        let board = Leaderboard::new(app.ctx());
        app.run(board.load_snapshot("/standings".into())).unwrap();
        app.run(board.follow(Some(2_500))).unwrap();
        for round in 0..20 {
            let start = board.summary.get().me.expect("player 2500 is ranked").score;
            let mut batches: Vec<_> = (0..8)
                .map(|_| {
                    Box::pin(board.apply_deltas(vec![Delta {
                        id: 2_500,
                        points: 1,
                    }]))
                })
                .collect();
            let mut done = [false; 8];
            app.run(std::future::poll_fn(|cx| {
                for (batch, done) in batches.iter_mut().zip(done.iter_mut()) {
                    if !*done && batch.as_mut().poll(cx).is_ready() {
                        *done = true;
                    }
                }
                if done.iter().all(|d| *d) {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }));
            let shown = board.summary.get().me.unwrap().score;
            assert_eq!(shown, (start + 8).min(MAX_SCORE), "round {round}");
        }
    }

    #[test]
    fn an_update_that_changes_nothing_in_the_window_writes_nothing() {
        let app = App::new();
        serve(&app, 5_000);
        let board = Leaderboard::new(app.ctx());
        app.run(board.load_snapshot("/standings".into())).unwrap();
        app.t.take_change_sets();
        // A delta for a player nobody can see, of zero points: the window is the same.
        assert_eq!(
            app.run(board.apply_deltas(vec![Delta {
                id: 4_000,
                points: 0
            }])),
            Ok(1)
        );
        assert!(app.t.take_change_sets().is_empty());
    }

    #[test]
    fn the_demo_feed_runs_on_the_timer_port_and_is_the_same_every_time() {
        let run = || {
            let app = App::new();
            let board = Leaderboard::new(app.ctx());
            board.start_demo(2_000, 1_000);
            app.t.run_pending();
            assert_eq!(board.summary.get().players, 2_000);
            let first = board.top.get();
            app.advance(1_000); // ten ticks of 100 deltas
            assert_ne!(board.top.get(), first, "scores moved");
            board.stop_demo();
            app.advance(1_000);
            let stopped = board.top.get();
            app.advance(1_000);
            assert_eq!(board.top.get(), stopped, "a stopped feed does not move");
            (first, stopped)
        };
        assert_eq!(run(), run(), "a seeded feed is reproducible");
    }

    #[test]
    fn what_a_platform_receives_is_one_small_change_set_whatever_the_players() {
        let app = App::new();
        serve(&app, 100_000);
        let reply = app.t.call_sync(
            CallTarget::Constructor {
                type_id: ids::type_id("Leaderboard"),
                method_id: ids::method_id("Leaderboard", "new"),
            },
            1,
            &[],
        );
        assert_eq!(reply.status, ReplyStatus::Ok);
        let store = Handle::decode_exact(&reply.body).unwrap();
        app.t.take_change_sets();
        app.t
            .runtime()
            .observe(store.0, undra::signals::ALL_SIGNALS, true);
        // A player is followed before the snapshot arrives, so all three signals change with it.
        let me = undra::wire::Encode::encode_to_vec(&Some(777_u32));
        let follow = CallTarget::Method {
            handle: store,
            method_id: ids::method_id("Leaderboard", "follow"),
        };
        assert_eq!(app.t.call(follow, 2, &me), 0);
        app.t.run_pending();
        app.t.host().take_decoded_change_sets();

        let args = undra::wire::Encode::encode_to_vec(&"/standings".to_owned());
        let target = CallTarget::Method {
            handle: store,
            method_id: ids::method_id("Leaderboard", "load_snapshot"),
        };
        assert_eq!(app.t.call(target, 3, &args), 0);
        app.t.run_pending();
        let raw = app.t.take_change_sets();
        assert_eq!(raw.len(), 1, "the three signals arrive together");
        // 12 bytes of header, 17 a signal, 50 + 7 rows of 12 bytes with their two lengths, and the
        // summary's three numbers and the followed row: the same for 1,000 players or 1,000,000.
        assert_eq!(raw[0].len(), 780, "bytes for 100,000 players");
        let decoded = ChangeSet::decode(&mut Reader::new(&raw[0])).expect("a change-set");
        assert_eq!(decoded.entries.len(), 3);
        assert!(decoded.entries.iter().all(|e| e.handle == store));
        let rows: Vec<usize> = decoded
            .entries
            .iter()
            .filter_map(|e| Vec::<Row>::decode_exact(&e.value).ok())
            .map(|rows| rows.len())
            .collect();
        assert_eq!(
            rows,
            [TOP, 2 * AROUND + 1],
            "the top and the rows around the player"
        );
    }
}
