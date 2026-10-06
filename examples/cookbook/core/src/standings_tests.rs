//! Tests of [`standings`](crate::standings), kept in their own file because the benchmark harness
//! compiles `standings.rs` itself (`bench/common/leaderboard.rs`) and must not compile these with it.

use crate::standings::*;

fn ids(rows: &[Ranked]) -> Vec<u32> {
    rows.iter().map(|r| r.id).collect()
}

fn small() -> Standings {
    // Scores 50, 90, 50, 70, 90: ranked 2 (90), 5 (90), 4 (70), 1 (50), 3 (50).
    Standings::from_snapshot(br#"[[1,50],[2,90],[3,50],[4,70],[5,90]]"#).unwrap()
}

#[test]
fn players_are_ranked_by_score_then_id_and_ties_share_a_rank() {
    let view = small().view(None, 5, 0);
    assert_eq!(ids(&view.top), [2, 5, 4, 1, 3]);
    let ranks: Vec<u32> = view.top.iter().map(|r| r.rank).collect();
    assert_eq!(ranks, [1, 1, 3, 4, 4], "competition ranks: 1, 1, 3, 4, 4");
}

#[test]
fn the_aggregates_come_from_the_whole_ranking() {
    let view = small().view(None, 2, 0);
    assert_eq!(view.players, 5);
    assert_eq!(view.top_score, 90);
    assert_eq!(view.median_score, 70);
    assert_eq!(view.top.len(), 2);
    assert!(view.around.is_empty() && view.me.is_none());
}

#[test]
fn the_rows_around_a_player_are_clipped_at_both_ends() {
    let standings = small();
    let first = standings.view(Some(2), 1, 2);
    assert_eq!(ids(&first.around), [2, 5, 4], "nothing above the best");
    assert_eq!(
        first.me,
        Some(Ranked {
            rank: 1,
            id: 2,
            score: 90
        })
    );
    let middle = standings.view(Some(4), 1, 1);
    assert_eq!(ids(&middle.around), [5, 4, 1]);
    let last = standings.view(Some(3), 1, 2);
    assert_eq!(ids(&last.around), [4, 1, 3], "nothing below the last");
    assert_eq!(last.me.map(|m| m.rank), Some(4));
}

#[test]
fn a_player_who_is_not_ranked_has_no_neighbourhood() {
    let view = small().view(Some(99), 1, 3);
    assert!(view.me.is_none() && view.around.is_empty());
}

#[test]
fn an_empty_ranking_has_a_view_with_zeros() {
    let view = Standings::new().view(Some(1), 50, 3);
    assert_eq!((view.players, view.top_score, view.median_score), (0, 0, 0));
    assert!(view.top.is_empty());
    assert!(Standings::from_snapshot(b"[]").unwrap().is_empty());
}

#[test]
fn a_snapshot_that_cannot_be_trusted_is_refused() {
    assert!(matches!(
        Standings::from_snapshot(b"{\"players\": 3}"),
        Err(SnapshotError::Malformed(_))
    ));
    assert_eq!(
        Standings::from_snapshot(b"[[1,5],[2,6],[1,7]]").unwrap_err(),
        SnapshotError::DuplicateId(1)
    );
    assert_eq!(
        Standings::from_snapshot(b"[[1,100000]]").unwrap_err(),
        SnapshotError::ScoreTooHigh {
            id: 1,
            score: 100_000
        }
    );
    assert!(SnapshotError::DuplicateId(1).to_string().contains("twice"));
}

#[test]
fn deltas_move_players_and_saturate() {
    let mut standings = small();
    // Player 1 climbs past the tied leaders; player 4 falls to zero; player 99 is unknown.
    let applied = standings.apply(&[(1, 45), (4, -500), (99, 10)]);
    assert_eq!(applied, 2);
    assert_eq!(standings.score_of(1), Some(95));
    assert_eq!(standings.score_of(4), Some(0));
    assert_eq!(standings.score_of(99), None);
    let view = standings.view(None, 5, 0);
    assert_eq!(ids(&view.top), [1, 2, 5, 3, 4]);
    assert_eq!(standings.apply(&[(2, 99_999)]), 1);
    assert_eq!(
        standings.score_of(2),
        Some(MAX_SCORE),
        "saturates at the maximum"
    );
    assert_eq!(standings.apply(&[(99, 1)]), 0);
}

#[test]
fn a_player_touched_twice_in_one_batch_moves_once() {
    let mut standings = small();
    standings.apply(&[(1, 10), (1, 10), (1, -5)]);
    assert_eq!(standings.score_of(1), Some(65));
    let view = standings.view(None, 5, 0);
    assert_eq!(ids(&view.top), [2, 5, 4, 1, 3]);
    assert_eq!(view.top.len(), 5, "nobody is listed twice");
}

#[test]
fn merging_deltas_equals_sorting_everyone_again() {
    // 2,000 players, 40 batches of 150 deltas: after each batch the incremental ranking is the
    // ranking a plain sort of the same scores gives.
    let snapshot = synthetic_snapshot(7, 2_000);
    let mut standings = Standings::from_snapshot(&snapshot).unwrap();
    let mut model: Vec<(u32, u32)> = serde_json::from_slice(&snapshot).unwrap();
    for round in 0..40 {
        let deltas = synthetic_deltas(100 + round, 2_000, 150);
        standings.apply(&deltas);
        for &(id, points) in &deltas {
            let score = &mut model[id as usize - 1].1;
            let moved = (i64::from(*score) + i64::from(points)).clamp(0, i64::from(MAX_SCORE));
            *score = u32::try_from(moved).unwrap();
        }
        let mut expected = model.clone();
        expected.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let ranking: Vec<(u32, u32)> = standings
            .ranking()
            .iter()
            .map(|r| (r.id, r.score))
            .collect();
        assert_eq!(ranking, expected, "round {round}");
    }
}

#[test]
fn the_whole_ranking_agrees_with_the_window() {
    let standings = small();
    let all = standings.ranking();
    assert_eq!(all, standings.view(None, 5, 0).top);
    assert_eq!(
        all.iter().map(|r| r.rank).collect::<Vec<_>>(),
        [1, 1, 3, 4, 4]
    );
    assert!(Standings::new().ranking().is_empty());
}

#[test]
fn the_feed_is_the_same_on_every_run() {
    assert_eq!(synthetic_snapshot(1, 50), synthetic_snapshot(1, 50));
    assert_ne!(synthetic_snapshot(1, 50), synthetic_snapshot(2, 50));
    assert_eq!(synthetic_deltas(3, 50, 20), synthetic_deltas(3, 50, 20));
    assert!(
        synthetic_deltas(3, 50, 200)
            .iter()
            .all(|&(id, p)| (1..=50).contains(&id) && (-100..=100).contains(&p))
    );
}

#[test]
fn a_hundred_thousand_players_are_about_a_megabyte_and_a_half() {
    let snapshot = synthetic_snapshot(1, 100_000);
    assert!(
        (1_300_000..1_500_000).contains(&snapshot.len()),
        "{} bytes",
        snapshot.len()
    );
    let standings = Standings::from_snapshot(&snapshot).unwrap();
    assert_eq!(standings.len(), 100_000);
    let view = standings.view(Some(500), 50, 3);
    assert_eq!(view.top.len(), 50);
    assert_eq!(view.around.len(), 7);
    assert!(view.top[0].score >= view.median_score);
}
