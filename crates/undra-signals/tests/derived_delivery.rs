//! Derived lists in a store (ADR-039 section 5): exactly what reaches the host for each source
//! operation, transactions, the bounds (4,096 pending ops each way, 256 ops for a parameter
//! change), raw writes, the ADR-019/020 delivery rules (abandon, observe rollback, `no_coalesce`,
//! empty change-sets, ordering), the isolation of a panicking closure, and concurrent writers.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use undra_signals::testing::CaptureSink;
use undra_signals::{ALL_SIGNALS, ChangeSink, DerivedList, Signal, StoreCell, txn, with_sink};
use undra_wire::payload::{ChangeOp, ChangeSet};
use undra_wire::{KeyedPatch, PatchOp, Writer};

/// The bounds of ADR-039 section 4 (`TAP_LIMIT`, `OUT_LIMIT`, `PARAM_WALK_LIMIT`).
const TAP_LIMIT: usize = 4096;
const OUT_LIMIT: usize = 4096;
const PARAM_WALK_LIMIT: usize = 256;

const LIST: u32 = 0;
const OPEN: u32 = 1;

/// `list` (0, keyed) and `open` (1) = the todos that are not done, both observed.
struct Fixture {
    rig: Rig,
    list: Signal<Vec<Todo>>,
    open: DerivedList<Todo>,
}

fn fixture(initial: Vec<Todo>) -> Fixture {
    let rig = Rig::new();
    let list = Signal::new(initial);
    let open = list.derive().filter(|t: &Todo| !t.done).build();
    rig.cell.attach_keyed(&list, LIST, todo_key).unwrap();
    rig.cell.attach_derived(&open, OPEN, todo_key).unwrap();
    let entries = rig.observe_all();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].op, ChangeOp::Full);
    Fixture { rig, list, open }
}

impl Fixture {
    /// Runs `f` and returns the view's entry of the one change-set it produced (`None`: the
    /// change-set has no entry for the view).
    fn view_entry(&self, f: impl FnOnce()) -> Option<undra_wire::payload::ChangeEntry> {
        self.rig.run(f);
        let set = self.rig.one_set();
        set.entries.into_iter().find(|e| e.signal_id == OPEN)
    }

    fn view_ops(&self, f: impl FnOnce()) -> Vec<PatchOp<Todo>> {
        let entry = self.view_entry(f).expect("the view has an entry");
        patch_of::<Todo>(&entry).ops
    }
}

fn ins(index: u32, item: Todo) -> PatchOp<Todo> {
    PatchOp::Insert { index, item }
}

fn upd(index: u32, item: Todo) -> PatchOp<Todo> {
    PatchOp::Update { index, item }
}

/// t1..t6, every even one done: the view is t1, t3, t5.
fn mixed() -> Vec<Todo> {
    (1..=6)
        .map(|id| todo(id, &format!("t{id}"), id % 2 == 0))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// One source op in, at most two derived ops out (ADR-039 section 2's table)
// ---------------------------------------------------------------------------------------------

#[test]
fn inserts_send_an_insert_at_the_rank_or_nothing() {
    let f = fixture(mixed()); // source t1..t6, view t1 t3 t5
    // Passing: Insert at its rank in the view.
    assert_eq!(
        f.view_ops(|| f.list.insert(1, todo(7, "new", false))),
        vec![ins(1, todo(7, "new", false))]
    );
    // source t1 t7 t2 t3 t4 t5 t6, view t1 t7 t3 t5. Filtered out: the view has no entry.
    assert!(
        f.view_entry(|| f.list.push(todo(8, "done", true)))
            .is_none()
    );
    // Removing a filtered-out row (t2 at source 2): nothing either.
    assert!(
        f.view_entry(|| {
            f.list.remove(2);
        })
        .is_none()
    );
    assert_eq!(f.open.len(), 4);
}

#[test]
fn removes_updates_moves_and_clear() {
    let f = fixture(mixed()); // source t1..t6, view t1 t3 t5
    // Remove a passing row (t3 at source 2): Remove at its rank 1.
    assert_eq!(
        f.view_ops(|| {
            f.list.remove(2);
        }),
        vec![PatchOp::Remove { index: 1 }]
    );
    // source t1 t2 t4 t5 t6, view t1 t5
    // Update that stays: Update at its rank.
    assert_eq!(
        f.view_ops(|| f.list.update_at(3, |t| t.title = "five".into())),
        vec![upd(1, todo(5, "five", false))]
    );
    // Update that leaves: Remove.
    assert_eq!(
        f.view_ops(|| f.list.update_at(0, |t| t.done = true)),
        vec![PatchOp::Remove { index: 0 }]
    );
    // source t1* t2* t4* t5 t6*, view t5
    // Update that enters: Insert (a filter transition is an insert).
    assert_eq!(
        f.view_ops(|| f.list.update_at(2, |t| t.done = false)),
        vec![ins(0, todo(4, "t4", false))]
    );
    // view t4 t5. Update out -> out: nothing.
    assert!(
        f.view_entry(|| f.list.update_at(1, |t| t.title = "x".into()))
            .is_none()
    );
    // Move of a passing row across another passing row: Move.
    assert_eq!(
        f.view_ops(|| f.list.move_item(3, 0)),
        vec![PatchOp::Move { from: 1, to: 0 }]
    );
    // source t5 t1* t2* t4 t6*, view t5 t4. A move that keeps its rank: nothing.
    assert!(f.view_entry(|| f.list.move_item(3, 1)).is_none());
    // Clear of a non-empty view: Clear.
    assert_eq!(f.view_ops(|| f.list.clear()), vec![PatchOp::Clear]);
    assert!(f.open.is_empty());
    // Clear again (empty source): the source's empty patch, nothing for the view.
    assert!(f.view_entry(|| f.list.clear()).is_none());
}

#[test]
fn a_sort_key_change_is_a_move_then_an_update() {
    let rig = Rig::new();
    let list = Signal::new(todos(4)); // t1..t4
    let by_title = list
        .derive()
        .sort_by_key(|t: &Todo| t.title.clone())
        .build();
    rig.cell.attach_derived(&by_title, 0, todo_key).unwrap();
    rig.observe_all();
    rig.run(|| list.update_at(0, |t| t.title = "z".into()));
    let set = rig.one_set();
    assert_eq!(
        patch_of::<Todo>(entry(&set, 0)).ops,
        vec![
            PatchOp::Move { from: 0, to: 3 },
            upd(3, todo(1, "z", false))
        ],
        "the row keeps its identity on the platform"
    );
    // The same key: an Update in place.
    rig.run(|| list.update_at(0, |t| t.done = true));
    assert_eq!(
        patch_of::<Todo>(entry(&rig.one_set(), 0)).ops,
        vec![upd(3, todo(1, "z", true))]
    );
}

#[test]
fn ties_keep_source_order_and_a_source_move_reorders_them() {
    let rig = Rig::new();
    // Every row has the same key: the view is the source order.
    let list = Signal::new(todos(4));
    let view = list.derive().sort_by_key(|_: &Todo| 0_u8).build();
    rig.cell.attach_derived(&view, 0, todo_key).unwrap();
    rig.observe_all();
    rig.run(|| list.move_item(0, 2));
    assert_eq!(
        patch_of::<Todo>(entry(&rig.one_set(), 0)).ops,
        vec![PatchOp::Move { from: 0, to: 2 }],
        "equal keys follow the source"
    );
    let ids: Vec<u32> = view.get().iter().map(|t| t.id).collect();
    assert_eq!(ids, [2, 3, 1, 4]);
}

#[test]
fn a_transaction_is_one_patch_of_its_ops_in_order() {
    let f = fixture(mixed());
    let mut replica = f.open.get();
    let ops = f.view_ops(|| {
        txn(|| {
            f.list.push(todo(10, "a", false));
            f.list.update_at(0, |t| t.done = true);
            f.list.insert(0, todo(11, "b", false));
            f.list.move_item(0, 3);
            f.list.update_at(1, |t| t.done = false); // t2 enters
        });
    });
    assert_eq!(ops.len(), 5, "one op per view change, no more: {ops:?}");
    KeyedPatch { ops }.apply(&mut replica).unwrap();
    assert_eq!(replica, f.open.get());
    assert_eq!(f.open.stats().patches, 1);
}

#[test]
fn a_change_that_does_not_reach_the_view_sends_nothing_at_all() {
    // Only the view is observed: toggling a filtered-out row's title changes no view row, so the
    // change-set would be empty, and an empty change-set is not delivered.
    let f = fixture(mixed());
    f.rig.observe_off(LIST);
    f.rig
        .run(|| f.list.update_at(1, |t| t.title = "still done".into()));
    assert!(f.rig.sets().is_empty(), "no entry, no change-set");
    // And the next one that does reach it is a patch, relative to what the host had.
    let ops = f.view_ops(|| f.list.update_at(1, |t| t.done = false));
    assert_eq!(ops, vec![ins(1, todo(2, "still done", false))]);
}

#[test]
fn a_raw_write_rebuilds_and_sends_the_full_value() {
    let f = fixture(mixed());
    let before = f.open.stats();
    let entry = f
        .view_entry(|| f.list.update(|l| l[0].title = "raw".into()))
        .unwrap();
    assert_eq!(entry.op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<Todo>>(&entry), f.open.get());
    let after = f.open.stats();
    assert_eq!(after.rebuilds, before.rebuilds + 1);
    assert_eq!(after.full_values, before.full_values + 1);
    // Then patches again.
    assert_eq!(f.view_ops(|| f.list.push(todo(9, "p", false))).len(), 1);
    // `set` and `replace` too, and a transaction that mixes them is a full value.
    let entry = f
        .view_entry(|| {
            txn(|| {
                f.list.push(todo(10, "q", false));
                f.list.replace(todos(3));
                f.list.push(todo(11, "r", false));
            })
        })
        .unwrap();
    assert_eq!(entry.op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<Todo>>(&entry), f.open.get());
}

#[test]
fn entries_are_ordered_by_signal_id_and_txn_ids_increase() {
    let rig = Rig::new();
    let list = Signal::new(todos(3));
    let tag = Signal::new(0_u32);
    let a = list.derive().filter(|t: &Todo| t.id % 2 == 1).build();
    let b = list.derive().map(|t: &Todo| t.id).build();
    rig.cell
        .attach_derived(&b, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.cell.attach(&tag, 1).unwrap();
    rig.cell.attach_derived(&a, 2, todo_key).unwrap();
    rig.observe_all();
    rig.run(|| {
        txn(|| {
            tag.set(1);
            list.push(todo(5, "x", false));
        })
    });
    rig.run(|| list.push(todo(6, "y", false)));
    let sets = rig.sets();
    assert_eq!(sets.len(), 2);
    assert_eq!(ids(&sets[0]), [0, 1, 2]);
    assert_eq!(
        ids(&sets[1]),
        [0],
        "6 is even: the filtered view sends nothing"
    );
    assert!(sets[1].txn_id > sets[0].txn_id);
}

#[test]
fn an_unobserved_no_coalesce_view_is_sent_in_full_each_time() {
    let rig = Rig::new();
    let list = Signal::new(todos(3));
    let view = list.derive().build();
    rig.cell.attach_derived(&view, 0, todo_key).unwrap();
    rig.cell.set_no_coalesce(0).unwrap();
    for n in 4..6 {
        rig.run(|| list.push(todo(n, "x", false)));
        let set = rig.one_set();
        assert_eq!(entry(&set, 0).op, ChangeOp::Full);
        assert_eq!(value_of::<Vec<Todo>>(entry(&set, 0)), list.get());
    }
    // Observed, it becomes patches.
    rig.observe_all();
    rig.run(|| list.push(todo(9, "x", false)));
    assert_eq!(entry(&rig.one_set(), 0).op, ChangeOp::KeyedPatch);
}

#[test]
fn an_unobserved_view_keeps_no_ops_and_observing_sends_the_full_value() {
    let f = fixture(mixed());
    f.rig.observe_off(OPEN);
    f.rig.run(|| {
        f.list.push(todo(7, "a", false));
        f.list.remove(0);
    });
    for set in f.rig.sets() {
        assert!(set.entries.iter().all(|e| e.signal_id != OPEN));
    }
    let entries = f.rig.observe_on(OPEN);
    assert_eq!(entries[0].op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<Todo>>(&entries[0]), f.open.get());
    // Only what happens after the observe is patched on top of it.
    assert_eq!(
        f.view_ops(|| f.list.push(todo(8, "b", false))),
        vec![ins(3, todo(8, "b", false))]
    );
}

#[test]
fn an_abandoned_commit_is_followed_by_the_full_value() {
    let rig = Rig::new();
    let list = Signal::new(mixed());
    let open = list.derive().filter(|t: &Todo| !t.done).build();
    let armed = Arc::new(AtomicBool::new(false));
    let bomb = Signal::new(EncodeBomb::new(&armed, 1));
    rig.cell.attach_derived(&open, 0, todo_key).unwrap();
    rig.cell.attach(&bomb, 1).unwrap();
    rig.observe_all();
    armed.store(true, Ordering::SeqCst);
    let aborted = catch_unwind(AssertUnwindSafe(|| {
        rig.run(|| {
            txn(|| {
                list.push(todo(7, "a", false));
                bomb.update(|b| b.value = 2);
            })
        })
    }));
    assert!(aborted.is_err());
    assert!(rig.sets().is_empty());
    armed.store(false, Ordering::SeqCst);
    // The ops the abandoned change-set carried are gone: the next commit sends the full value.
    rig.run(|| list.push(todo(8, "b", false)));
    let set = rig.one_set();
    assert_eq!(entry(&set, 0).op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<Todo>>(entry(&set, 0)), open.get());
    // And patches resume.
    rig.run(|| list.push(todo(9, "c", false)));
    assert_eq!(entry(&rig.one_set(), 0).op, ChangeOp::KeyedPatch);
}

#[test]
fn a_failed_observe_keeps_no_ops_for_a_host_that_has_nothing() {
    let rig = Rig::new();
    let list = Signal::new(mixed());
    let open = list.derive().filter(|t: &Todo| !t.done).build();
    let armed = Arc::new(AtomicBool::new(true));
    let bomb = Signal::new(EncodeBomb::new(&armed, 1));
    rig.cell.attach_derived(&open, 0, todo_key).unwrap();
    rig.cell.attach(&bomb, 1).unwrap();
    let failed = catch_unwind(AssertUnwindSafe(|| {
        rig.cell.observe(ALL_SIGNALS, true, &mut Writer::new())
    }));
    assert!(failed.is_err());
    assert!(!rig.cell.is_observed(0), "rolled back");
    armed.store(false, Ordering::SeqCst);
    rig.run(|| list.push(todo(7, "a", false)));
    assert!(rig.sets().is_empty(), "unobserved: nothing is sent");
    let entries = rig.observe_on(0);
    assert_eq!(value_of::<Vec<Todo>>(&entries[0]), open.get());
}

#[test]
fn derived_lists_are_left_out_of_snapshots() {
    let f = fixture(mixed());
    let mut out = Writer::new();
    f.rig.cell.encode_snapshot(&mut out);
    let mut r = undra_wire::Reader::new(out.as_slice());
    let snapshot = undra_wire::payload::StoreSnapshot::decode(&mut r).unwrap();
    let ids: Vec<u32> = snapshot.signals.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids,
        [LIST],
        "only the source is stored; the view is derived on restore"
    );
}

#[test]
fn encode_signal_materialises_the_view() {
    let f = fixture(mixed());
    let mut out = Writer::new();
    assert!(f.rig.cell.encode_signal(OPEN, &mut out));
    let value: Vec<Todo> = undra_wire::Decode::decode_exact(out.as_slice()).unwrap();
    assert_eq!(value, f.open.get());
}

#[test]
fn a_list_attaches_to_one_store_slot_for_life() {
    let rig = Rig::new();
    let list = Signal::new(todos(2));
    let view = list.derive().build();
    rig.cell.attach_derived(&view, 0, todo_key).unwrap();
    assert!(view.is_attached());
    assert_eq!(
        rig.cell.attach_derived(&view, 1, todo_key),
        Err(undra_signals::SignalsError::AlreadyAttached)
    );
    let other = list.derive().build();
    assert!(matches!(
        StoreCell::new(1).attach_derived(&other, 3, todo_key),
        Err(undra_signals::SignalsError::OutOfOrder { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// Panicking closures: isolated like a panicking computed (ADR-019 amendment)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_panicking_filter_holds_back_only_its_view_and_recovers_with_the_full_value() {
    let rig = Rig::new();
    let list = Signal::new(todos(3));
    let tag = Signal::new(0_u32);
    // Panics on a title of "boom".
    let view = list
        .derive()
        .filter(|t: &Todo| {
            assert!(t.title != "boom", "filter failure");
            true
        })
        .build();
    rig.cell.attach_keyed(&list, 0, todo_key).unwrap();
    rig.cell.attach_derived(&view, 1, todo_key).unwrap();
    rig.cell.attach(&tag, 2).unwrap();
    rig.observe_all();

    let result = catch_unwind(AssertUnwindSafe(|| {
        rig.run(|| {
            txn(|| {
                list.update_at(1, |t| t.title = "boom".into());
                tag.set(1);
            })
        })
    }));
    assert!(
        result.is_ok(),
        "the write that triggered the commit succeeds"
    );
    let set = rig.one_set();
    assert_eq!(ids(&set), [0, 2], "the rest of the store is delivered");
    assert_eq!(rig.cell.failed_signals().len(), 1);
    assert!(rig.cell.is_failed(1));
    assert!(rig.cell.failed_signals()[0].1.contains("filter failure"));

    // Still failing: still held back.
    rig.run(|| list.push(todo(4, "d", false)));
    assert_eq!(ids(&rig.one_set()), [0]);

    // The input changes so that the closure succeeds: the full value, and the slot recovers.
    let rebuilds = view.stats().rebuilds;
    rig.run(|| list.update_at(1, |t| t.title = "fine".into()));
    let set = rig.one_set();
    assert_eq!(entry(&set, 1).op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<Todo>>(entry(&set, 1)), list.get());
    assert!(!rig.cell.is_failed(1));
    assert!(
        view.stats().rebuilds > rebuilds,
        "a drain that unwound rebuilds the index"
    );
    // And patches resume, consistent with the index.
    rig.run(|| list.update_at(0, |t| t.done = true));
    assert_eq!(
        patch_of::<Todo>(entry(&rig.one_set(), 1)).ops,
        vec![upd(0, todo(1, "t1", true))]
    );
}

#[test]
fn a_panicking_map_is_isolated_on_observe_too() {
    let rig = Rig::new();
    let list = Signal::new(vec![todo(1, "boom", false)]);
    let view = list
        .derive()
        .map(|t: &Todo| {
            assert!(t.title != "boom", "map failure");
            t.id
        })
        .build();
    let tag = Signal::new(7_u32);
    rig.cell
        .attach_derived(&view, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.cell.attach(&tag, 1).unwrap();
    let entries = rig.observe_all();
    assert_eq!(entries.len(), 1, "the view is held back, the tag is sent");
    assert_eq!(entries[0].signal_id, 1);
    assert!(rig.cell.is_failed(0));
    rig.run(|| list.update_at(0, |t| t.title = "ok".into()));
    let set = rig.one_set();
    assert_eq!(entry(&set, 0).op, ChangeOp::Full);
    assert_eq!(value_of::<Vec<u32>>(entry(&set, 0)), [1]);
    assert!(!rig.cell.is_failed(0));
}

#[test]
fn a_panicking_rust_read_leaves_the_list_usable() {
    let list = Signal::new(vec![1_u32, 2, 3]);
    let armed = Arc::new(AtomicBool::new(false));
    let trip = Arc::clone(&armed);
    let view = list
        .derive()
        .filter(move |n: &u32| {
            assert!(!trip.load(Ordering::SeqCst) || *n != 2, "closure failure");
            true
        })
        .build();
    assert_eq!(view.get(), [1, 2, 3]);
    armed.store(true, Ordering::SeqCst);
    list.update_at(1, |n| *n = 2);
    assert!(catch_unwind(AssertUnwindSafe(|| view.len())).is_err());
    armed.store(false, Ordering::SeqCst);
    list.push(4);
    assert_eq!(
        view.get(),
        [1, 2, 3, 4],
        "rebuilt from the list after the unwind"
    );
}

// ---------------------------------------------------------------------------------------------
// The bounds
// ---------------------------------------------------------------------------------------------

/// An unobserved, attached-nowhere view, read once so its tap records; then `n` pending ops.
fn rebuilds_after(n: usize) -> u64 {
    let list = Signal::new(vec![0_u32; 4]);
    let view = list.derive().filter(|n: &u32| n % 2 == 0).build();
    assert_eq!(view.len(), 4);
    let base = view.stats().rebuilds;
    assert_eq!(base, 1, "the first read builds the index");
    for i in 0..n {
        list.update_at(i % 4, |v| *v = u32::try_from(i).unwrap());
    }
    let expected: Vec<u32> = list.get().into_iter().filter(|n| n % 2 == 0).collect();
    assert_eq!(view.get(), expected);
    view.stats().rebuilds - base
}

#[test]
fn the_tap_keeps_exactly_4096_ops_before_it_rebuilds() {
    assert_eq!(rebuilds_after(TAP_LIMIT - 1), 0);
    assert_eq!(
        rebuilds_after(TAP_LIMIT),
        0,
        "exactly the limit is replayed"
    );
    assert_eq!(rebuilds_after(TAP_LIMIT + 1), 1, "one more rebuilds once");
}

#[test]
fn an_unobserved_view_after_100_000_ops_rebuilds_once_on_read() {
    let list = Signal::new((0..1_000_u32).collect::<Vec<_>>());
    let view = list.derive().filter(|n: &u32| n % 3 == 0).build();
    let _ = view.len();
    for i in 0..100_000_u32 {
        list.update_at((i % 1_000) as usize, |v| *v = i);
    }
    let rebuilds = view.stats().rebuilds;
    let expected: Vec<u32> = list.get().into_iter().filter(|n| n % 3 == 0).collect();
    assert_eq!(view.get(), expected);
    assert_eq!(view.stats().rebuilds, rebuilds + 1);
}

/// One transaction of `n` view ops, drained by reads every 100 source ops so the tap never
/// overflows: what does the commit send?
fn commit_of(n: usize) -> ChangeOp {
    let rig = Rig::new();
    let list = Signal::new(Vec::<u32>::new());
    let view = list.derive().build();
    rig.cell
        .attach_derived(&view, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.observe_all();
    rig.run(|| {
        txn(|| {
            for i in 0..n {
                list.push(u32::try_from(i).unwrap());
                if i % 100 == 0 {
                    let _ = view.len();
                }
            }
        })
    });
    let set = rig.one_set();
    let e = entry(&set, 0);
    let mut host = Vec::new();
    match e.op {
        ChangeOp::Full => host = value_of(e),
        ChangeOp::KeyedPatch => {
            let patch = patch_of::<u32>(e);
            assert_eq!(patch.len(), n);
            patch.apply(&mut host).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(host, list.get());
    e.op
}

#[test]
fn an_observed_view_keeps_exactly_4096_ops_for_its_commit() {
    assert_eq!(commit_of(OUT_LIMIT), ChangeOp::KeyedPatch);
    assert_eq!(commit_of(OUT_LIMIT + 1), ChangeOp::Full);
}

/// A parameter change that moves exactly `n` rows in or out of the view.
fn param_flip(n: usize) -> (ChangeOp, usize) {
    let rig = Rig::new();
    let rows: Vec<u32> = (0..1_000).collect();
    let list = Signal::new(rows);
    let cut = Signal::new(0_u32);
    let view = list
        .derive()
        .filter_with(&cut, |cut, n: &u32| *n >= *cut)
        .build();
    rig.cell
        .attach_derived(&view, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.cell.attach(&cut, 1).unwrap();
    rig.observe_all();
    rig.run(|| cut.set(u32::try_from(n).unwrap()));
    let set = rig.one_set();
    let e = entry(&set, 0);
    let mut host: Vec<u32> = (0..1_000).collect();
    let ops = match e.op {
        ChangeOp::Full => {
            host = value_of(e);
            0
        }
        ChangeOp::KeyedPatch => {
            let patch = patch_of::<u32>(e);
            patch.apply(&mut host).unwrap();
            patch.len()
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(host, view.get());
    (e.op, ops)
}

#[test]
fn a_parameter_change_is_a_patch_up_to_256_ops_then_the_full_value() {
    assert_eq!(param_flip(1), (ChangeOp::KeyedPatch, 1));
    assert_eq!(
        param_flip(PARAM_WALK_LIMIT),
        (ChangeOp::KeyedPatch, PARAM_WALK_LIMIT)
    );
    assert_eq!(param_flip(PARAM_WALK_LIMIT + 1).0, ChangeOp::Full);
}

#[test]
fn a_parameter_set_to_the_same_value_sends_nothing() {
    let rig = Rig::new();
    let list = Signal::new((0..10_u32).collect::<Vec<_>>());
    let cut = Signal::new(5_u32);
    let view = list
        .derive()
        .filter_with(&cut, |cut, n: &u32| *n >= *cut)
        .build();
    rig.cell
        .attach_derived(&view, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.observe_all();
    rig.run(|| cut.set(5));
    assert!(rig.sets().is_empty());
}

#[test]
fn source_ops_replay_with_the_old_parameter_then_the_walk_runs_with_the_new() {
    let rig = Rig::new();
    let list = Signal::new((0..10_u32).collect::<Vec<_>>());
    let cut = Signal::new(5_u32);
    let view = list
        .derive()
        .filter_with(&cut, |cut, n: &u32| *n >= *cut)
        .sort_by_key(|n: &u32| u32::MAX - *n)
        .build();
    rig.cell
        .attach_derived(&view, 0, |n: &u32| u64::from(*n))
        .unwrap();
    rig.observe_all();
    let mut host = view.get();
    rig.run(|| {
        txn(|| {
            list.push(3);
            list.update_at(9, |n| *n = 1);
            cut.set(2);
            list.push(20);
        })
    });
    let set = rig.one_set();
    patch_of::<u32>(entry(&set, 0)).apply(&mut host).unwrap();
    let mut expected: Vec<u32> = list.get().into_iter().filter(|n| *n >= 2).collect();
    expected.sort_by_key(|n| u32::MAX - *n);
    assert_eq!(host, expected);
}

// ---------------------------------------------------------------------------------------------
// count()
// ---------------------------------------------------------------------------------------------

#[test]
fn count_follows_rows_entering_and_leaving_without_a_scan() {
    let rig = Rig::new();
    let list = Signal::new(mixed());
    let open = list.derive().filter(|t: &Todo| !t.done).count();
    rig.cell.attach_computed(&open, 0).unwrap();
    rig.observe_all();
    rig.run(|| list.update_at(1, |t| t.done = false));
    assert_eq!(value_of::<u32>(entry(&rig.one_set(), 0)), 4);
    rig.run(|| list.remove(0));
    assert_eq!(value_of::<u32>(entry(&rig.one_set(), 0)), 3);
    assert_eq!(open.get(), 3);
}

// ---------------------------------------------------------------------------------------------
// Concurrency (as tests/concurrency.rs): writers on the source, an observer re-observing
// ---------------------------------------------------------------------------------------------

#[test]
fn concurrent_writers_and_a_re_observer_never_strand_an_op() {
    use parking_lot::Mutex;
    use std::sync::atomic::AtomicU32;

    /// Applies change-sets in arrival order (one store: they are ordered by the delivery lock).
    struct Host {
        view: Mutex<(Vec<Todo>, u64)>,
    }
    impl undra_signals::ChangeSink for Host {
        fn deliver(&self, change_set: &[u8]) {
            let set: ChangeSet = undra_signals::testing::decode(change_set).unwrap();
            let mut state = self.view.lock();
            assert!(set.txn_id > state.1, "commit order");
            state.1 = set.txn_id;
            for e in &set.entries {
                if e.signal_id != OPEN {
                    continue;
                }
                match e.op {
                    ChangeOp::Full => state.0 = value_of(e),
                    ChangeOp::KeyedPatch => patch_of::<Todo>(e).apply(&mut state.0).unwrap(),
                    other => panic!("{other:?}"),
                }
            }
        }
    }

    let host = Arc::new(Host {
        view: Mutex::new((Vec::new(), 0)),
    });
    let cell = StoreCell::new(0xC0C0);
    cell.set_handle(HANDLE);
    let list = Signal::new(Vec::<Todo>::new());
    let open = list.derive().filter(|t: &Todo| !t.done).build();
    cell.attach_keyed(&list, LIST, todo_key).unwrap();
    cell.attach_derived(&open, OPEN, todo_key).unwrap();
    let next = AtomicU32::new(1);
    let sink: Arc<dyn undra_signals::ChangeSink> = host.clone();
    let observe_now = |cell: &StoreCell, host: &Host| {
        cell.observe_and_deliver(&[ALL_SIGNALS], |payload| host.deliver(payload));
    };
    observe_now(&cell, &host);
    std::thread::scope(|scope| {
        for writer in 0..4 {
            let (list, sink, next) = (&list, sink.clone(), &next);
            scope.spawn(move || {
                with_sink(sink, || {
                    for i in 0..400 {
                        let id = next.fetch_add(1, Ordering::SeqCst);
                        match (i + writer) % 4 {
                            0 | 1 => list.push(todo(id, "w", i % 3 == 0)),
                            2 => {
                                // The length is read before the write, so another writer's `remove` can
                                // land in between and leave the index one past the end: `update_at` then
                                // refuses it, as `remove` does below. Neither refusal is what this test is
                                // about (a runner hit it once in the Gate of 2026-10-04).
                                let len = list.with(Vec::len);
                                if len > 0 {
                                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                        list.update_at(id as usize % len, |t| t.done = !t.done);
                                    }));
                                }
                            }
                            _ => {
                                let len = list.with(Vec::len);
                                if len > 2 {
                                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                        list.remove(id as usize % len)
                                    }));
                                }
                            }
                        }
                    }
                });
            });
        }
        let (cell, host) = (&cell, &host);
        scope.spawn(move || {
            for _ in 0..20 {
                observe_now(cell, host);
                std::thread::yield_now();
            }
        });
    });
    // Everything was committed: the host's view is the view.
    let expected: Vec<Todo> = list.get().into_iter().filter(|t| !t.done).collect();
    assert_eq!(open.get(), expected);
    assert_eq!(host.view.lock().0, expected);
}

#[test]
fn a_view_with_capture_sink_matches_after_a_burst() {
    // Many writes, several commits: what the host replays equals the view (sanity for the bench).
    let f = fixture(Vec::new());
    let mut host = Vec::new();
    for round in 0..50_u32 {
        f.rig.run(|| {
            txn(|| {
                for i in 0..20 {
                    f.list.push(todo(round * 100 + i, "x", i % 4 == 0));
                }
                let len = f.list.with(Vec::len);
                f.list
                    .update_at((round as usize * 7) % len, |t| t.done = !t.done);
                f.list.move_item(0, len - 1);
            })
        });
        for set in f.rig.sets() {
            for e in set.entries.iter().filter(|e| e.signal_id == OPEN) {
                match e.op {
                    ChangeOp::Full => host = value_of(e),
                    ChangeOp::KeyedPatch => patch_of::<Todo>(e).apply(&mut host).unwrap(),
                    other => panic!("{other:?}"),
                }
            }
        }
        assert_eq!(host, f.open.get());
    }
    let stats = f.open.stats();
    assert_eq!(stats.full_values, 1, "the observe only: {stats:?}");
    assert_eq!(stats.patches, 50);
    let _ = CaptureSink::new();
}
