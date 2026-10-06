//! The structure the leaderboard recipe keeps off the core: every player's score, in rank order.
//!
//! This file knows nothing about Undra. It is the "app-owned structure" of the recipe
//! ([`leaderboard`](crate::leaderboard)): plain Rust that a blocking-pool thread builds and changes
//! behind its own lock, and that the core only ever asks for a small [`View`] of. The benchmark
//! harness (`bench/common/leaderboard.rs`) compiles this very file, so the numbers on the recipe's
//! page are measured on the code the recipe shows.
//!
//! * [`Standings::from_snapshot`] parses a snapshot (a JSON array of `[id, score]` pairs, about
//!   14 bytes a player) and sorts it. It builds a value and touches no shared state, so the caller
//!   decides how long a lock is held: only for the swap.
//! * [`Standings::apply`] adds score deltas and moves only the players they touched: one pass over
//!   the ranking, not a sort.
//! * [`Standings::view`] is everything the UI needs, computed in `O(top + around + log n)`: the top
//!   rows, the rows around one player, and the aggregates.
//!
//! Ranks are competition ranks: players with the same score share a rank, and the next rank
//! skips the tied ones (1, 2, 2, 4). Ties are listed by player id.

use std::cmp::Ordering;
use std::fmt;

/// The highest score a player can have. Deltas saturate here and at zero.
pub const MAX_SCORE: u32 = 99_999;

/// One player's standing in the ranking, without a rank: the rank depends on everyone else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The player's identity.
    pub id: u32,
    /// The player's score, at most [`MAX_SCORE`].
    pub score: u32,
}

/// A player as it is shown: the entry and where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ranked {
    /// The competition rank, 1 for the highest score.
    pub rank: u32,
    /// The player's identity.
    pub id: u32,
    /// The player's score.
    pub score: u32,
}

/// What the UI shows of a ranking of any size: a few rows and a few numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The best players, best first.
    pub top: Vec<Ranked>,
    /// The followed player and its neighbours, best first (empty when nobody is followed or the
    /// player is not in the ranking).
    pub around: Vec<Ranked>,
    /// How many players there are.
    pub players: u32,
    /// The followed player, when it is in the ranking.
    pub me: Option<Ranked>,
    /// The highest score (0 for an empty ranking).
    pub top_score: u32,
    /// The score in the middle of the ranking (0 for an empty ranking).
    pub median_score: u32,
}

/// Why a snapshot could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// The bytes are not a JSON array of `[id, score]` pairs of unsigned integers.
    Malformed(String),
    /// Two entries carry the same player id.
    DuplicateId(u32),
    /// A score is above [`MAX_SCORE`].
    ScoreTooHigh {
        /// The player.
        id: u32,
        /// The score that was too high.
        score: u32,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Malformed(why) => write!(f, "the snapshot is not readable: {why}"),
            SnapshotError::DuplicateId(id) => write!(f, "player {id} appears twice"),
            SnapshotError::ScoreTooHigh { id, score } => {
                write!(
                    f,
                    "player {id} has {score}, above the maximum of {MAX_SCORE}"
                )
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

/// The ranking order: higher score first, then lower id.
fn order(a: &Entry, b: &Entry) -> Ordering {
    b.score.cmp(&a.score).then(a.id.cmp(&b.id))
}

/// Every player's score, twice: by id (to find a player) and in rank order (to answer for the top,
/// for a neighbourhood and for the middle).
#[derive(Clone, Debug, Default)]
pub struct Standings {
    /// Sorted by id.
    by_id: Vec<Entry>,
    /// Sorted by [`order`].
    ranked: Vec<Entry>,
}

impl Standings {
    /// An empty ranking.
    pub fn new() -> Standings {
        Standings::default()
    }

    /// Parses and sorts a snapshot. This is the expensive step (milliseconds for 100,000 players)
    /// and it needs no lock: it returns a finished value to swap in.
    ///
    /// ```
    /// use cookbook::standings::Standings;
    /// let standings = Standings::from_snapshot(br#"[[1,50],[2,90],[3,50]]"#).unwrap();
    /// assert_eq!(standings.len(), 3);
    /// assert_eq!(standings.view(None, 2, 0).top[0].id, 2);
    /// ```
    ///
    /// # Errors
    ///
    /// [`SnapshotError`] when the bytes are not a snapshot, a player appears twice or a score is
    /// out of range.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Standings, SnapshotError> {
        let pairs: Vec<(u32, u32)> =
            serde_json::from_slice(bytes).map_err(|e| SnapshotError::Malformed(e.to_string()))?;
        let mut by_id = Vec::with_capacity(pairs.len());
        for (id, score) in pairs {
            if score > MAX_SCORE {
                return Err(SnapshotError::ScoreTooHigh { id, score });
            }
            by_id.push(Entry { id, score });
        }
        by_id.sort_unstable_by_key(|e| e.id);
        if let Some(pair) = by_id.windows(2).find(|w| w[0].id == w[1].id) {
            return Err(SnapshotError::DuplicateId(pair[0].id));
        }
        let mut ranked = by_id.clone();
        ranked.sort_unstable_by(order);
        Ok(Standings { by_id, ranked })
    }

    /// How many players there are.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the ranking is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The score of `id`, if it is in the ranking.
    pub fn score_of(&self, id: u32) -> Option<u32> {
        let at = self.by_id.binary_search_by_key(&id, |e| e.id).ok()?;
        Some(self.by_id[at].score)
    }

    /// Adds each `(id, points)` to the player's score, saturating at 0 and [`MAX_SCORE`], and
    /// returns how many deltas found their player. A delta for a player the ranking does not hold
    /// is ignored: the next snapshot is the source of truth for who plays.
    ///
    /// Only the players the batch touched move: they are taken out of the ranking and merged back
    /// in one pass, `O(players + touched * log touched)`, never a sort of everyone.
    pub fn apply(&mut self, deltas: &[(u32, i32)]) -> usize {
        let mut applied = 0;
        let mut before: Vec<Entry> = Vec::new();
        for &(id, points) in deltas {
            let Ok(at) = self.by_id.binary_search_by_key(&id, |e| e.id) else {
                continue;
            };
            let entry = &mut self.by_id[at];
            before.push(*entry);
            let moved = (i64::from(entry.score) + i64::from(points)).clamp(0, i64::from(MAX_SCORE));
            entry.score = u32::try_from(moved).unwrap_or(MAX_SCORE);
            applied += 1;
        }
        if applied == 0 {
            return 0;
        }
        // A player touched twice is taken out once, at the score it had before the batch (the sort is
        // stable, so the first of its entries is the oldest).
        before.sort_by_key(|e| e.id);
        before.dedup_by_key(|e| e.id);
        let mut after: Vec<Entry> = before
            .iter()
            .filter_map(|e| {
                let at = self.by_id.binary_search_by_key(&e.id, |b| b.id).ok()?;
                Some(self.by_id[at])
            })
            .collect();
        before.sort_unstable_by(order);
        after.sort_unstable_by(order);

        let old = std::mem::take(&mut self.ranked);
        let mut merged = Vec::with_capacity(old.len());
        let (mut gone, mut came) = (0, 0);
        for entry in old {
            if before.get(gone) == Some(&entry) {
                gone += 1;
                continue;
            }
            while came < after.len() && order(&after[came], &entry) == Ordering::Less {
                merged.push(after[came]);
                came += 1;
            }
            merged.push(entry);
        }
        merged.extend_from_slice(&after[came..]);
        self.ranked = merged;
        applied
    }

    /// The competition rank a score would have: one more than the players above it.
    fn rank_of_score(&self, score: u32) -> u32 {
        let above = self.ranked.partition_point(|e| e.score > score);
        u32::try_from(above + 1).unwrap_or(u32::MAX)
    }

    fn ranked_at(&self, index: usize) -> Ranked {
        let entry = self.ranked[index];
        Ranked {
            rank: self.rank_of_score(entry.score),
            id: entry.id,
            score: entry.score,
        }
    }

    /// Every player in rank order, with ranks, in one pass (`O(n)`). The UI never needs this, which
    /// is the point of [`view`](Standings::view): it is what the wrong design publishes (a signal of
    /// every player), and what a test compares the incremental ranking with.
    pub fn ranking(&self) -> Vec<Ranked> {
        let mut rows: Vec<Ranked> = Vec::with_capacity(self.ranked.len());
        for (i, entry) in self.ranked.iter().enumerate() {
            let rank = match rows.last() {
                Some(previous) if previous.score == entry.score => previous.rank,
                _ => u32::try_from(i + 1).unwrap_or(u32::MAX),
            };
            rows.push(Ranked {
                rank,
                id: entry.id,
                score: entry.score,
            });
        }
        rows
    }

    /// What the UI shows: the best `top` players, the player `me` with `around` neighbours on each
    /// side, and the aggregates. `O(top + around + log n)`.
    pub fn view(&self, me: Option<u32>, top: usize, around: usize) -> View {
        let n = self.ranked.len();
        let rows = |range: std::ops::Range<usize>| range.map(|i| self.ranked_at(i)).collect();
        let position = me.and_then(|id| {
            let mine = Entry {
                id,
                score: self.score_of(id)?,
            };
            self.ranked.binary_search_by(|e| order(e, &mine)).ok()
        });
        let (around_rows, me_row) = match position {
            Some(at) => (
                rows(at.saturating_sub(around)..(at + around + 1).min(n)),
                Some(self.ranked_at(at)),
            ),
            None => (Vec::new(), None),
        };
        View {
            top: rows(0..top.min(n)),
            around: around_rows,
            players: u32::try_from(n).unwrap_or(u32::MAX),
            me: me_row,
            top_score: self.ranked.first().map_or(0, |e| e.score),
            median_score: self.ranked.get(n / 2).map_or(0, |e| e.score),
        }
    }
}

// ----- a deterministic feed ---------------------------------------------------------------------

/// A seeded generator (SplitMix64): the same seed makes the same feed on every machine. It is a
/// function of its seed, not a source of randomness: the core takes the seed from the `Rng` port.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// A snapshot of `players` players with ids `1..=players` and scores spread over `0..=MAX_SCORE`,
/// in the format [`Standings::from_snapshot`] reads. 100,000 players are about 1.4 MB.
pub fn synthetic_snapshot(seed: u64, players: u32) -> Vec<u8> {
    let mut rng = SplitMix(seed);
    let mut out = String::with_capacity(players as usize * 14 + 2);
    out.push('[');
    for id in 1..=players {
        if id > 1 {
            out.push(',');
        }
        let score = rng.next() % (u64::from(MAX_SCORE) + 1);
        out.push_str(&format!("[{id},{score}]"));
    }
    out.push(']');
    out.into_bytes()
}

/// `count` score changes for players `1..=players`, each between -100 and +100 points.
pub fn synthetic_deltas(seed: u64, players: u32, count: usize) -> Vec<(u32, i32)> {
    let mut rng = SplitMix(seed);
    (0..count)
        .map(|_| {
            let id = 1 + u32::try_from(rng.next() % u64::from(players.max(1))).unwrap_or(0);
            let points = i32::try_from(rng.next() % 201).unwrap_or(0) - 100;
            (id, points)
        })
        .collect()
}
