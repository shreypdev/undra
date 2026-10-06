//! Every benchmarked operation, described once.
//!
//! The criterion benches (`benches/*.rs`) and the budgets test (`tests/budgets.rs`) both take
//! their operations from here, so a number in `RESULTS.md` and a line in `budgets.toml` always
//! name the same code. Names are `group/operation[/variant]`.
//!
//! Each operation checks, while it is being built, that it does what its name says (a keyed
//! insert really ships a patch, not the whole list; a 100-signal write really makes one
//! change-set of 100 entries; the "1 KB" record really is 1,024 bytes). A benchmark that
//! silently measures the wrong thing is worse than none.
#![allow(missing_docs, dead_code)]

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use undra::meta::ids;
use undra::runtime::testing::{call_payload, decode_reply, drive_from_this_thread};
use undra::runtime::{Runtime, RuntimeConfig};
use undra::signals::{ALL_SIGNALS, ChangeSink, Computed, Signal, StoreCell, txn, with_sink};
use undra::wire::payload::{CallTarget, ChangeSetRef};
use undra::wire::{
    Bytes, Decimal, Decode, Encode, Handle, KeyedPatch, Reader, Timestamp, Uuid, Writer,
};
use undra_bench::workload::{Bench, Workload, plain, with_reset};

use super::fixtures::{self, Item, Shape};
use super::host::{Core, CountingHost, call_ok, construct, method_call, runtime, runtime_with};

/// Every operation the budgets test gates: one per wire type (the round trip), plus dispatch,
/// signals, lazy lists, snapshot and the per-operation rows of the harsh-conditions scenarios (`stress`).
pub fn all() -> Vec<Workload> {
    let mut all = wire();
    all.extend(dispatch());
    all.extend(boundary());
    all.extend(signals());
    all.extend(lazy());
    all.extend(snapshot());
    all.extend(super::stress::workloads());
    all.extend(super::query_rows::rows_group());
    all.extend(super::ports::ports());
    all.extend(super::ports::db());
    all.extend(super::leaderboard::workloads());
    all
}

/// The operations of one group (`wire`, `dispatch`, `signals`, `lazy`, `snapshot`, `stress`, `query`, `ports`, `db`, `leaderboard`).
pub fn group(name: &str) -> Vec<Workload> {
    match name {
        "wire" => wire(),
        "dispatch" => dispatch(),
        "boundary" => boundary(),
        "signals" => signals(),
        "lazy" => lazy(),
        "snapshot" => snapshot(),
        "stress" => super::stress::workloads(),
        "query" => super::query_rows::rows_group(),
        "ports" => super::ports::ports(),
        "db" => super::ports::db(),
        "leaderboard" => super::leaderboard::workloads(),
        other => panic!("no benchmark group `{other}`"),
    }
}

fn enc<T: Encode>(value: &T) -> Vec<u8> {
    value.encode_to_vec()
}

// ---------------------------------------------------------------------------------------------
// wire
// ---------------------------------------------------------------------------------------------

/// Encode, decode and round trip of each wire type; the budgets test gates the round trips.
pub fn wire() -> Vec<Workload> {
    let mut all = wire_types(false);
    all.extend(keyed_patch());
    all
}

/// Each wire type's encode and decode on their own (criterion only).
pub fn wire_halves() -> Vec<Workload> {
    wire_types(true)
}

fn wire_types(halves: bool) -> Vec<Workload> {
    let mut list = Vec::new();
    let out = &mut list;
    add_wire(out, halves, "bool", || true);
    add_wire(out, halves, "u8", || 200_u8);
    add_wire(out, halves, "u32", || 0x1234_5678_u32);
    add_wire(out, halves, "i64", || -1_234_567_890_123_i64);
    add_wire(out, halves, "f64", || 1234.5678_f64);
    add_wire(out, halves, "string_short", || "hello, undra".to_owned());
    add_wire(out, halves, "string_1kb", || "k".repeat(1024));
    add_wire(out, halves, "bytes_1kb", || {
        Bytes((0..1024_u32).map(|i| (i * 31) as u8).collect())
    });
    add_wire(out, halves, "option_some", || Some(7_u32));
    add_wire(out, halves, "option_none", || None::<u32>);
    add_wire(out, halves, "vec_u32_1k", || {
        (0..1000_u32).collect::<Vec<_>>()
    });
    add_wire(out, halves, "map100_string_u32", || {
        (0..100_u32)
            .map(|i| (format!("key-{i:03}"), i))
            .collect::<HashMap<_, _>>()
    });
    add_wire(out, halves, "map100_u32_u32", || {
        (0..100_u32).map(|i| (i * 7, i)).collect::<HashMap<_, _>>()
    });
    add_wire(out, halves, "duration", || Duration::new(3, 500_000_000));
    add_wire(out, halves, "timestamp", || Timestamp(1_700_000_000_123));
    add_wire(out, halves, "uuid", || {
        Uuid([
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
            0xde, 0xf0,
        ])
    });
    // ADR-042: a money amount at the largest scale the wire holds (16 bytes of mantissa, 1 of scale).
    add_wire(out, halves, "decimal", || {
        Decimal::new(-1_999_999_999_999_999_999_i128, 38)
    });
    add_wire(out, halves, "record5", fixtures::record5);
    add_wire(out, halves, "record1k", || {
        let record = fixtures::record1k();
        assert_eq!(
            record.encode_to_vec().len(),
            1024,
            "the 1 KB record must be 1,024 bytes"
        );
        record
    });
    add_wire(out, halves, "enum_rect", || Shape::Rect { w: 3.0, h: 4.5 });
    add_wire(out, halves, "enum_label", || {
        Shape::Label("a label".to_owned())
    });
    add_wire(out, halves, "result_ok", || Ok::<u32, String>(42));
    add_wire(out, halves, "result_err", || {
        Err::<u32, String>("the server said no".to_owned())
    });
    list
}

/// The keyed patch (`undra-wire`) on its own, for a 10,000-row list that gains one row in the
/// middle: the diff with a cheap key and `PartialEq` (the fallback that a raw `set` / `update`
/// takes, see `signals/keyed_10k/raw_update_diff`; recorded list operations do not run it), the
/// patch's own round trip, and the host-side replay.
fn keyed_patch() -> Vec<Workload> {
    const ROWS: u32 = 10_000;
    fn rows() -> Vec<Item> {
        (0..ROWS)
            .map(|n| Item {
                id: u64::from(n) * 2 + 1,
                title: fixtures::title_of(n, 24),
                done: false,
            })
            .collect()
    }
    fn gained() -> Item {
        Item {
            id: 10_000_000,
            title: fixtures::title_of(ROWS, 24),
            done: false,
        }
    }
    fn patch_of(old: &[Item]) -> KeyedPatch<Item> {
        let mut new = old.to_vec();
        new.insert(old.len() / 2, gained());
        KeyedPatch::diff(old, &new, |i| i.id, |a, b| a == b).expect("a patch, not the full list")
    }
    vec![
        Workload::new("wire/keyed_patch_10k/diff", || {
            let old = rows();
            let mut new = old.clone();
            new.insert(old.len() / 2, gained());
            plain(move || {
                black_box(KeyedPatch::diff(
                    black_box(&old),
                    black_box(&new),
                    |i| i.id,
                    |a, b| a == b,
                ));
            })
        }),
        Workload::new("wire/keyed_patch_10k/roundtrip", || {
            let patch = patch_of(&rows());
            assert_eq!(patch.ops.len(), 1, "one insert is one op");
            plain(move || {
                let mut w = Writer::new();
                black_box(&patch).encode(&mut w);
                let bytes = w.into_vec();
                let mut r = Reader::new(black_box(&bytes));
                black_box(KeyedPatch::<Item>::decode(&mut r)).ok();
            })
        }),
        Workload::new("wire/keyed_patch_10k/apply", || {
            let old = rows();
            let patch = patch_of(&old);
            let middle = old.len() / 2;
            let list = std::rc::Rc::new(std::cell::RefCell::new(old));
            let undo = list.clone();
            with_reset(
                move || {
                    patch
                        .apply(&mut list.borrow_mut())
                        .expect("the patch applies");
                },
                move || {
                    undo.borrow_mut().remove(middle);
                },
            )
        }),
    ]
}

/// Registers `wire/<name>/roundtrip`, and with `halves` also `/encode` and `/decode`.
///
/// * `encode` writes into a reused buffer: the codec's own cost, no allocation.
/// * `decode` reads from a fixed byte string into an owned value.
/// * `roundtrip` is what a call argument or a return value pays: `encode_to_vec` (one buffer
///   allocation) and `decode_exact`.
fn add_wire<T>(out: &mut Vec<Workload>, halves: bool, name: &str, make: fn() -> T)
where
    T: Encode + Decode + 'static,
{
    // Prove once, at registration, that the value survives its own encoding.
    {
        let value = make();
        let bytes = value.encode_to_vec();
        let back = T::decode_exact(&bytes).expect("a fixture decodes");
        assert_eq!(
            back.encode_to_vec(),
            bytes,
            "wire/{name} does not round trip"
        );
    }
    if halves {
        out.push(Workload::new(format!("wire/{name}/encode"), move || {
            let value = make();
            let mut w = Writer::with_capacity(4096);
            plain(move || {
                w.clear();
                value.encode(&mut w);
                black_box(w.as_slice());
            })
        }));
        out.push(Workload::new(format!("wire/{name}/decode"), move || {
            let bytes = make().encode_to_vec();
            plain(move || {
                let mut r = Reader::new(black_box(&bytes));
                black_box(T::decode(&mut r)).ok();
            })
        }));
    } else {
        out.push(Workload::new(format!("wire/{name}/roundtrip"), move || {
            let value = make();
            plain(move || {
                let bytes = black_box(&value).encode_to_vec();
                black_box(T::decode_exact(black_box(&bytes))).ok();
            })
        }));
    }
}

// ---------------------------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------------------------

/// `undra_call_sync`'s path without the C ABI: payload decode, handle lookup, the guarded
/// generated dispatcher, reply encode.
pub fn dispatch() -> Vec<Workload> {
    vec![
        Workload::new("dispatch/call_sync/add", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let args = [enc(&1_i64), enc(&2_i64)].concat();
            let payload = method_call(calc, "Calculator", "add", 2, &args);
            call_ok(&rt, &payload);
            plain(move || {
                black_box(rt.call_sync(black_box(&payload)));
            })
        }),
        Workload::new("dispatch/call_sync_with/add", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let args = [enc(&1_i64), enc(&2_i64)].concat();
            let payload = method_call(calc, "Calculator", "add", 2, &args);
            call_ok(&rt, &payload);
            plain(move || {
                black_box(rt.call_sync_with(black_box(&payload), |reply| black_box(reply.len())));
            })
        }),
        Workload::new("dispatch/call_sync/function", || {
            let (rt, _host) = runtime();
            let payload = call_payload(
                CallTarget::Function {
                    method_id: ids::function_id("add_one"),
                },
                2,
                &enc(&41_u32),
            );
            call_ok(&rt, &payload);
            plain(move || {
                black_box(rt.call_sync(black_box(&payload)));
            })
        }),
        // ADR-058: an instantiation of a generic function, `add_one_for<Record5>`: the same body as
        // `function` above, through the same path. `generic_fn_vs_function` gates the pair.
        Workload::new("dispatch/call_sync/generic_fn", || {
            let (rt, _host) = runtime();
            let payload = call_payload(
                CallTarget::Function {
                    method_id: ids::function_id("add_one_for<Record5>"),
                },
                2,
                &enc(&41_u32),
            );
            call_ok(&rt, &payload);
            plain(move || {
                black_box(rt.call_sync(black_box(&payload)));
            })
        }),
        Workload::new("dispatch/call_sync/echo_record1k", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let payload = method_call(calc, "Calculator", "echo", 2, &enc(&fixtures::record1k()));
            let reply = call_ok(&rt, &payload);
            assert!(reply.len() > 1024, "the record comes back");
            plain(move || {
                black_box(rt.call_sync(black_box(&payload)));
            })
        }),
        // ADR-040: an object handed to the host. `return_object` issues a handle that did not
        // exist (a new entry, one reference); the release that gives it back runs outside the
        // clock. `return_interned_object` returns the object the host already holds: the same
        // handle, one more reference (released outside the clock too).
        Workload::new("dispatch/call_sync/return_object", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let payload = method_call(calc, "Calculator", "fresh_dock", 2, &[]);
            let handle = Handle::decode_exact(&reply_body(&call_ok(&rt, &payload))).unwrap();
            assert!(rt.objects().host_refs_of(handle).is_some());
            rt.release(handle.0);
            let last = std::rc::Rc::new(std::cell::Cell::new(0_u64));
            let (rt_run, rt_reset) = (rt.clone(), rt);
            let (last_run, last_reset) = (last.clone(), last);
            with_reset(
                move || {
                    let reply = rt_run.call_sync(black_box(&payload));
                    last_run.set(u64::from_le_bytes(reply[5..13].try_into().unwrap()));
                },
                move || rt_reset.release(last_reset.replace(0)),
            )
        }),
        Workload::new("dispatch/call_sync/return_interned_object", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let payload = method_call(calc, "Calculator", "held_dock", 2, &[]);
            let held = Handle::decode_exact(&reply_body(&call_ok(&rt, &payload))).unwrap();
            let rt_reset = rt.clone();
            with_reset(
                move || {
                    black_box(rt.call_sync(black_box(&payload)));
                },
                move || rt_reset.release(held.0),
            )
        }),
        Workload::new("dispatch/call_sync/object_param", || {
            let (rt, _host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let held = method_call(calc, "Calculator", "held_dock", 2, &[]);
            let dock = Handle::decode_exact(&reply_body(&call_ok(&rt, &held))).unwrap();
            let payload = method_call(calc, "Calculator", "dock_slots", 3, &enc(&dock));
            call_ok(&rt, &payload);
            plain(move || {
                black_box(rt.call_sync(black_box(&payload)));
            })
        }),
        Workload::new("dispatch/call_async/ready_add", || {
            let (rt, host) = runtime();
            let calc = construct(&rt, "Calculator", &enc(&7_i64));
            let args = [enc(&1_i64), enc(&2_i64)].concat();
            let next = AtomicU64::new(10);
            let call = move |rt: &Runtime| {
                let id = (next.fetch_add(1, Ordering::Relaxed) % 4_000_000_000) as u32 + 10;
                let payload = method_call(calc, "Calculator", "ready_add", id, &args);
                let status = rt.call(&payload);
                rt.run_pending();
                status
            };
            let before = host.replies();
            assert_eq!(call(&rt), 0, "the call is accepted");
            assert_eq!(host.replies(), before + 1, "and answered by the executor");
            plain(move || {
                black_box(call(&rt));
            })
        }),
    ]
}

/// The body of a `Reply` payload (`call_id u32, status u8, body`).
fn reply_body(reply: &[u8]) -> Vec<u8> {
    reply[5..].to_vec()
}

// ---------------------------------------------------------------------------------------------
// boundary: calls from the core into the host (ADR-041)
// ---------------------------------------------------------------------------------------------

/// A host that answers port calls the way a platform's synchronous adapter does: it counts them
/// and, for an `echo`, replies at once with the number it was given.
#[derive(Default)]
struct PingHost {
    calls: AtomicU64,
}

impl undra::runtime::Host for PingHost {
    fn reply(&self, _: u32, _: &[u8]) {}
    fn change_set(&self, _: &[u8]) {}
    fn stream_item(&self, _: u32, _: &[u8]) {}
    fn port_call(
        &self,
        _port: u32,
        method: u32,
        id: u32,
        args: &[u8],
    ) -> undra::runtime::PortCallOutcome {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if method == ids::port_method_id("Pinger", "echo") {
            // `instance u64, n u32` -> the `Ok` body is `n`.
            let mut reply = Vec::with_capacity(9 + 4);
            reply.extend_from_slice(&id.to_le_bytes());
            reply.push(0);
            reply.extend_from_slice(&args[8..12]);
            return undra::runtime::PortCallOutcome::Sync(reply);
        }
        undra::runtime::PortCallOutcome::Sync(Vec::new())
    }
    fn log(&self, _: u8, _: &str, _: &str) {}
}

/// What calling a host callback instance costs the core: the proxy a dispatcher makes for an
/// instance handle, called as Rust code calls it. The round trip is the core's half only: the host
/// here answers at once, so there is no wait and no thread hop.
pub fn boundary() -> Vec<Workload> {
    vec![
        // The primitive every port call is made of: a fire-and-forget call with no proxy.
        Workload::new("boundary/port_call/notify", || {
            let host = Arc::new(PingHost::default());
            let rt = runtime_with(host.clone(), 0);
            let port = ids::port_id("Pinger");
            let method = ids::port_method_id("Pinger", "ping");
            let args = [enc(&1_u64), enc(&7_u32)].concat();
            rt.port_notify(port, method, args.clone());
            assert_eq!(host.calls.load(Ordering::Relaxed), 1);
            plain(move || {
                rt.port_notify(black_box(port), black_box(method), black_box(args.clone()));
            })
        }),
        // The same through the generated proxy of a callback interface: the instance handle, the
        // weak context, the writer.
        Workload::new("boundary/callback/notify", || {
            let host = Arc::new(PingHost::default());
            let rt = runtime_with(host.clone(), 0);
            let pinger = rt.callback::<dyn fixtures::Pinger>(1);
            pinger.ping(7);
            assert_eq!(host.calls.load(Ordering::Relaxed), 1);
            plain(move || {
                // The runtime is shut down when `rt` drops: it must outlive the loop.
                black_box(&rt);
                black_box(&pinger).ping(black_box(7));
            })
        }),
        // An `async` callback method answered at once: encode, port call, the reply, decode.
        Workload::new("boundary/callback/async_roundtrip", || {
            let host = Arc::new(PingHost::default());
            let rt = runtime_with(host.clone(), 0);
            let pinger = rt.callback::<dyn fixtures::Pinger>(1);
            let answer = poll_ready(pinger.echo(7));
            assert_eq!(answer, Ok(7));
            plain(move || {
                black_box(&rt);
                black_box(poll_ready(black_box(&pinger).echo(black_box(7))).ok());
            })
        }),
    ]
}

/// Polls a future that is already complete.
fn poll_ready<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("the host answers at once, so the call is ready"),
    }
}

// ---------------------------------------------------------------------------------------------
// signals
// ---------------------------------------------------------------------------------------------

/// A sink that counts what it is given.
#[derive(Default)]
struct CountingSink {
    change_sets: AtomicU64,
    bytes: AtomicU64,
}

impl ChangeSink for CountingSink {
    fn deliver(&self, change_set: &[u8]) {
        self.change_sets.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(change_set.len() as u64, Ordering::Relaxed);
    }
}

/// Change-sets, keyed patches, computeds and first observation.
pub fn signals() -> Vec<Workload> {
    vec![
        Workload::new("signals/changeset_100/cell", changeset_100_cell),
        Workload::new("signals/changeset_100/runtime", changeset_100_runtime),
        Workload::new("signals/changeset_100/decode", changeset_100_decode),
        Workload::new("signals/observe_100_initial", observe_100_initial),
        Workload::new("signals/keyed_10k/insert", || {
            keyed(10_000, KeyedOp::Insert)
        }),
        Workload::new("signals/keyed_10k/update", || {
            keyed(10_000, KeyedOp::Update)
        }),
        Workload::new("signals/keyed_10k/move", || keyed(10_000, KeyedOp::Move)),
        // The fallback: the same one-row edit written with the raw `update`, which the commit
        // finds by diffing the list against what the host has (O(list)).
        Workload::new("signals/keyed_10k/raw_update_diff", || {
            keyed(10_000, KeyedOp::RawUpdate)
        }),
        // The same insert on shorter lists: how the cost scales with the list, not the change.
        Workload::new("signals/keyed_1k/insert", || keyed(1_000, KeyedOp::Insert)),
        Workload::new("signals/keyed_100/insert", || keyed(100, KeyedOp::Insert)),
        // One write and its commit, nobody observing: pins the cost of the write check that
        // every build runs since ADR-035.
        Workload::new("signals/set_attached", set_attached),
        Workload::new("signals/derived_10k/update_visible", || {
            derived(10_000, DerivedOp::UpdateVisible)
        }),
        Workload::new("signals/derived_10k/toggle_membership", || {
            derived(10_000, DerivedOp::ToggleMembership)
        }),
        Workload::new("signals/derived_10k/sort_key_change", || {
            derived(10_000, DerivedOp::SortKeyChange)
        }),
        Workload::new("signals/derived_100k/update_visible", || {
            derived(100_000, DerivedOp::UpdateVisible)
        }),
        Workload::new("signals/derived_100k/sort_key_change", || {
            derived(100_000, DerivedOp::SortKeyChange)
        }),
        Workload::new("signals/derived_10k/insert_sorted", || {
            derived(10_000, DerivedOp::InsertSorted)
        }),
        Workload::new("signals/derived_10k/param_flip", || {
            derived(10_000, DerivedOp::ParamFlip)
        }),
        Workload::new("signals/derived_10k/rebuild_after_replace", || {
            derived(10_000, DerivedOp::RebuildAfterReplace)
        }),
        Workload::new("signals/derived_10k/count_toggle", || {
            derived(10_000, DerivedOp::CountToggle)
        }),
        Workload::new("signals/computed/recompute_1", computed_recompute_1),
        Workload::new(
            "signals/computed/recompute_chain_10",
            computed_recompute_chain_10,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// lazy lists (ADR-043)
// ---------------------------------------------------------------------------------------------

/// Lazy lists the host pages through: a window of 50 rows out of a list of 100,000, the change
/// that tells the host to ask again (12 bytes whatever the list holds), and a window of a filtered,
/// sorted view of a 100,000-row list (`Lazy::over`, through the derived index).
pub fn lazy() -> Vec<Workload> {
    vec![
        Workload::new("lazy/page_50_of_100k", || lazy_page(100_000, false)),
        Workload::new("lazy/page_50_of_10k", || lazy_page(10_000, false)),
        Workload::new("lazy/view_page_50_of_100k", || lazy_page(100_000, true)),
        Workload::new("lazy/invalidate", || lazy_invalidate(100_000)),
        Workload::new("lazy/invalidate_10k", || lazy_invalidate(10_000)),
    ]
}

/// The signals of `fixtures::Shelf`.
const SHELF_BOOKS: u32 = 0;
const SHELF_OPEN: u32 = 2;

/// Observes `signal` of `shelf` and returns its page server's handle and the stamp it was sent at.
fn shelf_server(
    rt: &Runtime,
    host: &InspectingHost,
    shelf: Handle,
    signal: u32,
) -> (Handle, undra::wire::payload::LazyValue) {
    use undra::wire::payload::{ChangeOp, LazyValue};
    host.keeping(true);
    rt.observe(shelf.0, signal, true);
    host.keeping(false);
    let entries = host.last_entries();
    assert_eq!(entries.len(), 1, "one entry for the one signal observed");
    let (id, op, value) = &entries[0];
    assert_eq!((*id, *op), (signal, ChangeOp::Full), "op 0 on observe");
    assert_eq!(
        value.len(),
        20,
        "a LazyValue: handle u64, len u32, version u64"
    );
    let value = LazyValue::decode(&mut Reader::new(value)).expect("a LazyValue");
    (value.handle, value)
}

/// One page call for 50 rows from the middle of a list of `rows` (of the view with `view`):
/// dispatch, the page server's lookup, the encoding of the 50 rows, the reply. The rows are
/// checked before anything is timed.
fn lazy_page(rows: u32, view: bool) -> Box<dyn Bench> {
    use undra::wire::payload::LazyPage;
    let host = Arc::new(InspectingHost::default());
    let rt = super::host::runtime_with(host.clone(), 0);
    let shelf = construct(&rt, "Shelf", &[]);
    call_ok(&rt, &method_call(shelf, "Shelf", "seed", 2, &enc(&rows)));
    let signal = if view { SHELF_OPEN } else { SHELF_BOOKS };
    let (server, stamp) = shelf_server(&rt, &host, shelf, signal);
    let shown = if view { rows / 4 * 3 } else { rows };
    assert_eq!(stamp.len, shown, "the length the host is told");
    let offset = shown / 2;
    let page = call_payload(
        CallTarget::LazyPage {
            handle: server,
            offset,
            limit: 50,
        },
        3,
        &[],
    );

    // What a page is: the header, then 50 rows that decode as the rows of the list.
    let reply = decode_reply(&rt.call_sync(&page));
    assert_eq!(reply.status, undra::wire::payload::ReplyStatus::Ok);
    let mut r = Reader::new(&reply.body);
    let header = LazyPage::decode(&mut r).expect("a page header");
    assert_eq!((header.total, header.count), (shown, 50));
    assert_eq!(
        header.version, stamp.version,
        "read at the version it was announced at"
    );
    let items: Vec<Item> = (0..50)
        .map(|_| Item::decode(&mut r).expect("a row"))
        .collect();
    r.finish().expect("exactly 50 rows");
    let expected = if view {
        // The view: the rows not done, by title.
        let mut open: Vec<Item> = fixtures::views_rows(rows)
            .into_iter()
            .filter(|row| !row.done)
            .collect();
        open.sort_by(|a, b| a.title.cmp(&b.title));
        open
    } else {
        fixtures::views_rows(rows)
    };
    assert_eq!(items, expected[offset as usize..offset as usize + 50]);

    plain(move || {
        black_box(rt.call_sync_with(black_box(&page), |reply| black_box(reply.len())));
    })
}

/// A change to a list of `rows` rows the host pages: one method call that `update_at`s a row of an
/// observed `Lazy<Item>`, and the commit, which sends 12 bytes however long the list is. The
/// change-set is checked (one entry, op 2, 12 bytes, the new length and version) before it is
/// timed.
fn lazy_invalidate(rows: u32) -> Box<dyn Bench> {
    use undra::wire::payload::{ChangeOp, LazyInvalidated};
    let host = Arc::new(InspectingHost::default());
    let rt = super::host::runtime_with(host.clone(), 0);
    let shelf = construct(&rt, "Shelf", &[]);
    call_ok(&rt, &method_call(shelf, "Shelf", "seed", 2, &enc(&rows)));
    let (_, stamp) = shelf_server(&rt, &host, shelf, SHELF_BOOKS);
    let toggle = method_call(shelf, "Shelf", "toggle", 3, &enc(&(rows / 2)));

    host.keeping(true);
    call_ok(&rt, &toggle);
    host.keeping(false);
    let entries = host.last_entries();
    assert_eq!(entries.len(), 1, "only the list changed");
    let (id, op, value) = &entries[0];
    assert_eq!((*id, *op), (SHELF_BOOKS, ChangeOp::LazyInvalidated));
    assert_eq!(
        value.len(),
        12,
        "an invalidation is 12 bytes, whatever the list holds"
    );
    let invalidated = LazyInvalidated::decode(&mut Reader::new(value)).expect("a LazyInvalidated");
    assert_eq!(
        (invalidated.len, invalidated.version),
        (stamp.len, stamp.version + 1)
    );

    plain(move || {
        black_box(rt.call_sync(black_box(&toggle)));
    })
}

/// One write to a signal of a published store (owner recorded, handle set) and its implicit
/// commit, with nothing observed: the write check of every build (ADR-035: the checker the
/// runtime installed, three thread-local reads on a driver thread), the claim and the
/// nothing-to-send path.
fn set_attached() -> Box<dyn Bench> {
    // A runtime installs the process's write checker; this thread plays its core's driver.
    let (rt, _host) = runtime();
    drive_from_this_thread();
    let cell = StoreCell::new(0xBE_C0_03);
    let value = Signal::new(0_u32);
    cell.attach(&value, 0).expect("attach");
    cell.set_owner(rt.id());
    cell.set_handle(Handle::new(1, 1).0);
    assert!(value.can_write(), "a driver thread may write");
    value.set(1);
    assert_eq!(value.get(), 1);
    plain(move || {
        let _keep = &rt;
        value.update(|n| *n = n.wrapping_add(1));
        black_box(&value);
    })
}

/// 100 signals of one store written in one transaction: build the change-set and hand it to the
/// sink. No runtime, no dispatch: the signals crate on its own.
fn changeset_100_cell() -> Box<dyn Bench> {
    drive_from_this_thread();
    let cell = StoreCell::new(0xBE_C0_01);
    let signals: Vec<Signal<u32>> = (0..100).map(|_| Signal::new(0)).collect();
    for (id, signal) in signals.iter().enumerate() {
        cell.attach(signal, id as u32).expect("attach");
    }
    cell.set_handle(Handle::new(1, 1).0);
    cell.observe(ALL_SIGNALS, true, &mut Writer::new());
    let sink = Arc::new(CountingSink::default());
    let bump = move || {
        with_sink(sink.clone(), || {
            txn(|| {
                for signal in &signals {
                    signal.update(|n| *n = n.wrapping_add(1));
                }
            });
        });
        sink.change_sets.load(Ordering::Relaxed)
    };
    assert_eq!(bump(), 1, "one transaction, one change-set");
    plain(move || {
        black_box(bump());
    })
}

/// The same 100 writes as one method call on a store, through the runtime: dispatch, the writes,
/// the change-set, and the host callback.
fn changeset_100_runtime() -> Box<dyn Bench> {
    let (rt, host) = runtime();
    let store = construct(&rt, "Wide100", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let payload = method_call(store, "Wide100", "bump_all", 2, &[]);
    let (sets, bytes) = (host.change_sets(), host.change_set_bytes());
    call_ok(&rt, &payload);
    assert_eq!(
        host.change_sets(),
        sets + 1,
        "one transaction, one change-set"
    );
    assert!(
        host.change_set_bytes() - bytes >= 100 * 4,
        "the change-set carries 100 values"
    );
    plain(move || {
        black_box(rt.call_sync(black_box(&payload)));
    })
}

/// What a platform runtime does with that change-set before it touches its own state: validate
/// it and walk its entries (borrowed, no allocation).
fn changeset_100_decode() -> Box<dyn Bench> {
    let (rt, host) = runtime();
    let store = construct(&rt, "Wide100", &[]);
    rt.observe(store.0, ALL_SIGNALS, true);
    let before = host.change_set_bytes();
    call_ok(&rt, &method_call(store, "Wide100", "bump_all", 2, &[]));
    let len = (host.change_set_bytes() - before) as usize;
    // Rebuild the same payload shape with the cell (the runtime hands out no copy): 100 entries.
    let bytes = {
        let cell_sink = Arc::new(CaptureOne::default());
        drive_from_this_thread();
        let cell = StoreCell::new(0xBE_C0_02);
        let signals: Vec<Signal<u32>> = (0..100).map(|_| Signal::new(0)).collect();
        for (id, signal) in signals.iter().enumerate() {
            cell.attach(signal, id as u32).expect("attach");
        }
        cell.set_handle(Handle::new(1, 1).0);
        cell.observe(ALL_SIGNALS, true, &mut Writer::new());
        with_sink(cell_sink.clone(), || {
            txn(|| {
                for signal in &signals {
                    signal.set(9);
                }
            });
        });
        cell_sink.take()
    };
    assert_eq!(bytes.len(), len, "same shape as the runtime's change-set");
    plain(move || {
        let mut r = Reader::new(black_box(&bytes));
        let set = ChangeSetRef::decode(&mut r).expect("a change-set");
        let mut total = 0_usize;
        for entry in &set {
            total += entry.value.len() + entry.signal_id as usize;
        }
        black_box(total);
    })
}

/// Keeps the last payload it was given.
#[derive(Default)]
struct CaptureOne(std::sync::Mutex<Vec<u8>>);

impl CaptureOne {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().expect("not poisoned"))
    }
}

impl ChangeSink for CaptureOne {
    fn deliver(&self, change_set: &[u8]) {
        *self.0.lock().expect("not poisoned") = change_set.to_vec();
    }
}

/// A host starting to observe a 100-signal store: every current value comes back at once.
fn observe_100_initial() -> Box<dyn Bench> {
    let (rt, host) = runtime();
    let store = construct(&rt, "Wide100", &[]);
    let rt2 = rt.clone();
    let before = host.change_sets();
    rt.observe(store.0, ALL_SIGNALS, true);
    assert_eq!(
        host.change_sets(),
        before + 1,
        "the initial emission is one change-set"
    );
    rt.observe(store.0, ALL_SIGNALS, false);
    with_reset(
        move || rt.observe(store.0, ALL_SIGNALS, true),
        move || rt2.observe(store.0, ALL_SIGNALS, false),
    )
}

#[derive(Clone, Copy)]
enum KeyedOp {
    Insert,
    Update,
    Move,
    /// An update through the raw `Signal::update`: the diff path.
    RawUpdate,
}

/// A keyed list of `rows` rows, observed, changed by one insert, one update or one move written
/// with the recorded list operations (what generated store code does, ADR-027), or by one update
/// through the raw `Signal::update`, which takes the diff path. Through the runtime: dispatch,
/// argument decode, the write, the patch, the host callback.
fn keyed(rows: u32, op: KeyedOp) -> Box<dyn Bench> {
    let (rt, host) = runtime();
    let (middle, low, high) = (rows / 2, rows / 10, rows / 10 * 9);
    let feed = construct(&rt, "Feed", &[]);
    call_ok(
        &rt,
        &method_call(
            feed,
            "Feed",
            "seed",
            2,
            &[enc(&rows), enc(&24_u32)].concat(),
        ),
    );
    rt.observe(feed.0, ALL_SIGNALS, true);
    let at = |method: &str, id: u32, args: Vec<u8>| method_call(feed, "Feed", method, id, &args);

    // The two halves of every scenario, prebuilt: the timed call, and what undoes or alternates it.
    let (first, second, resets) = match op {
        KeyedOp::Insert => {
            let row = Item {
                id: 10_000_000, // even: the seeded ids are odd
                title: fixtures::title_of(rows, 24),
                done: false,
            };
            (
                at("insert_at", 3, [enc(&middle), enc(&row)].concat()),
                at("remove_at", 4, enc(&middle)),
                true,
            )
        }
        KeyedOp::Update => (
            at(
                "rename",
                3,
                [enc(&middle), enc(&"first title".to_owned())].concat(),
            ),
            at(
                "rename",
                4,
                [enc(&middle), enc(&"second title".to_owned())].concat(),
            ),
            false,
        ),
        KeyedOp::Move => (
            at("move_item", 3, [enc(&low), enc(&high)].concat()),
            at("move_item", 4, [enc(&high), enc(&low)].concat()),
            false,
        ),
        KeyedOp::RawUpdate => (
            at(
                "rename_raw",
                3,
                [enc(&middle), enc(&"first title".to_owned())].concat(),
            ),
            at(
                "rename_raw",
                4,
                [enc(&middle), enc(&"second title".to_owned())].concat(),
            ),
            false,
        ),
    };

    // Prove that what ships is a patch of a few dozen bytes, not the list.
    let before = host.change_set_bytes();
    call_ok(&rt, &first);
    let shipped = host.change_set_bytes() - before;
    assert!(
        (1..2_000).contains(&shipped),
        "a keyed change must ship a patch; {shipped} bytes shipped"
    );
    call_ok(&rt, &second);

    if resets {
        let rt2 = rt.clone();
        let (first, second) = (first.clone(), second.clone());
        with_reset(
            move || {
                black_box(rt.call_sync(black_box(&first)));
            },
            move || {
                black_box(rt2.call_sync(&second));
            },
        )
    } else {
        // Alternate between the two calls: every iteration is one real change of the same size.
        let mut flip = false;
        plain(move || {
            flip = !flip;
            let payload = if flip { &first } else { &second };
            black_box(rt.call_sync(black_box(payload)));
        })
    }
}

// ---------------------------------------------------------------------------------------------
// signals: derived lists (ADR-039)
// ---------------------------------------------------------------------------------------------

/// A [`CountingHost`] that can keep the last change-set, so a workload can check what it ships
/// before it is timed (and keeps nothing while it is: one relaxed load per change-set).
#[derive(Default)]
struct InspectingHost {
    counts: CountingHost,
    keep: std::sync::atomic::AtomicBool,
    last: std::sync::Mutex<Vec<u8>>,
}

impl InspectingHost {
    fn keeping(&self, on: bool) {
        self.keep.store(on, Ordering::Relaxed);
    }

    /// The entries of the last change-set kept: `(signal_id, op, value)`.
    fn last_entries(&self) -> Vec<(u32, undra::wire::payload::ChangeOp, Vec<u8>)> {
        let bytes = std::mem::take(&mut *self.last.lock().expect("not poisoned"));
        let set = ChangeSetRef::decode(&mut Reader::new(&bytes)).expect("a change-set");
        set.into_iter()
            .map(|e| (e.signal_id, e.op, e.value.to_vec()))
            .collect()
    }
}

impl undra::runtime::Host for InspectingHost {
    fn reply(&self, call_id: u32, payload: &[u8]) {
        self.counts.reply(call_id, payload);
    }

    fn change_set(&self, payload: &[u8]) {
        self.counts.change_set(payload);
        if self.keep.load(Ordering::Relaxed) {
            *self.last.lock().expect("not poisoned") = payload.to_vec();
        }
    }

    fn stream_item(&self, call_id: u32, payload: &[u8]) {
        self.counts.stream_item(call_id, payload);
    }

    fn port_call(&self, p: u32, m: u32, id: u32, args: &[u8]) -> undra::runtime::PortCallOutcome {
        self.counts.port_call(p, m, id, args)
    }

    fn log(&self, level: u8, target: &str, message: &str) {
        self.counts.log(level, target, message);
    }
}

/// The signals of `fixtures::Views`.
const VIEWS_ROWS: u32 = 0;
const VIEWS_OPEN: u32 = 2;
const VIEWS_BY_TITLE: u32 = 3;
const VIEWS_SHOWN: u32 = 4;
const VIEWS_OPEN_COUNT: u32 = 5;

#[derive(Clone, Copy, PartialEq)]
enum DerivedOp {
    /// `update_at` of a row the unsorted view shows: `rows` and `open` observed, one change-set of
    /// two one-op entries (`Update`, `Update`).
    UpdateVisible,
    /// `update_at` that moves a row out of the filter and back: `Remove` / `Insert`.
    ToggleMembership,
    /// A title change on the sorted view: `Move` + `Update`, across the whole view and back.
    SortKeyChange,
    /// An insert in the middle of the source and its removal, the sorted view observed: one
    /// `Insert` / `Remove` each.
    InsertSorted,
    /// The parameter flipped: 2,500 rows enter or leave, sent as the full value (past 256 ops).
    ParamFlip,
    /// `replace` of the source: the view rebuilds and is sent whole.
    RebuildAfterReplace,
    /// A membership toggle with `count()` observed as a `Computed<u32>` beside the source.
    CountToggle,
}

/// The ops of a keyed-patch entry.
fn ops_of(value: &[u8]) -> Vec<undra::wire::PatchOp<Item>> {
    KeyedPatch::<Item>::decode(&mut Reader::new(value))
        .expect("a keyed patch")
        .ops
}

/// The shape check of one change-set: per expected signal, `Some(ops)` for a patch of exactly
/// those op kinds (`'i'`, `'r'`, `'u'`, `'m'`), `None` for a full value.
fn expect_entries(
    host: &InspectingHost,
    expected: &[(u32, Option<&str>)],
    what: &str,
) -> Vec<(u32, undra::wire::payload::ChangeOp, Vec<u8>)> {
    use undra::wire::PatchOp;
    use undra::wire::payload::ChangeOp;
    let entries = host.last_entries();
    let ids: Vec<u32> = entries.iter().map(|(id, ..)| *id).collect();
    let want: Vec<u32> = expected.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, want, "{what}: the entries of the change-set");
    for ((id, op, value), (_, kinds)) in entries.iter().zip(expected) {
        match kinds {
            None => assert_eq!(*op, ChangeOp::Full, "{what}: signal {id} is a full value"),
            Some(kinds) => {
                assert_eq!(*op, ChangeOp::KeyedPatch, "{what}: signal {id} is a patch");
                let got: String = ops_of(value)
                    .iter()
                    .map(|op| match op {
                        PatchOp::Insert { .. } => 'i',
                        PatchOp::Remove { .. } => 'r',
                        PatchOp::Update { .. } => 'u',
                        PatchOp::Move { .. } => 'm',
                        PatchOp::Clear => 'c',
                    })
                    .collect();
                assert_eq!(got, *kinds, "{what}: the ops of signal {id}");
                assert!(
                    value.len() < 200,
                    "{what}: signal {id} ships {} bytes",
                    value.len()
                );
            }
        }
    }
    entries
}

/// `fixtures::Views` seeded with `rows` rows (every fourth done), observing only what `op`
/// measures, and the two calls the iterations alternate (or run and undo). Each call's change-set
/// is checked before anything is timed. Through the runtime, as `keyed` is: dispatch, argument
/// decode, the write, the drain of the views, the change-set, the host callback.
fn derived(rows: u32, op: DerivedOp) -> Box<dyn Bench> {
    let host = Arc::new(InspectingHost::default());
    let rt = super::host::runtime_with(host.clone(), 0);
    let views = construct(&rt, "Views", &[]);
    call_ok(&rt, &method_call(views, "Views", "seed", 2, &enc(&rows)));
    let middle = rows / 2; // an open row: `middle % 4 != 3` for the sizes used
    assert_ne!(middle % 4, 3, "the middle row is open");
    let observed: &[u32] = match op {
        DerivedOp::UpdateVisible | DerivedOp::ToggleMembership => &[VIEWS_ROWS, VIEWS_OPEN],
        DerivedOp::SortKeyChange | DerivedOp::InsertSorted => &[VIEWS_ROWS, VIEWS_BY_TITLE],
        DerivedOp::ParamFlip => &[VIEWS_SHOWN],
        DerivedOp::RebuildAfterReplace => &[VIEWS_OPEN],
        DerivedOp::CountToggle => &[VIEWS_ROWS, VIEWS_OPEN_COUNT],
    };
    for id in observed {
        rt.observe(views.0, *id, true);
    }
    let at = |method: &str, id: u32, args: Vec<u8>| method_call(views, "Views", method, id, &args);
    let rename = |id: u32, title: &str| {
        at(
            "rename",
            id,
            [enc(&middle), enc(&title.to_owned())].concat(),
        )
    };
    let toggle = |id: u32| at("toggle", id, enc(&middle));
    let (first, second, resets) = match op {
        DerivedOp::UpdateVisible => (rename(3, "first title"), rename(4, "second title"), false),
        // Titles sort before and after every seeded one ("item n xx.."): the row crosses the view.
        DerivedOp::SortKeyChange => (
            rename(3, "a first title"),
            rename(4, "z second title"),
            false,
        ),
        DerivedOp::ToggleMembership | DerivedOp::CountToggle => (toggle(3), toggle(4), false),
        DerivedOp::InsertSorted => {
            let row = Item {
                id: 10_000_000,
                title: "m inserted".to_owned(),
                done: false,
            };
            (
                at("insert_at", 3, [enc(&middle), enc(&row)].concat()),
                at("remove_at", 4, enc(&middle)),
                true,
            )
        }
        DerivedOp::ParamFlip => (at("flip", 3, Vec::new()), at("flip", 4, Vec::new()), false),
        DerivedOp::RebuildAfterReplace => (
            at("reset", 3, Vec::new()),
            at("reset", 4, Vec::new()),
            false,
        ),
    };

    // Prove what ships: the ops of the view, never the view, unless the row says so.
    let sets = host.counts.change_sets();
    host.keeping(true);
    call_ok(&rt, &first);
    let what_first = match op {
        DerivedOp::UpdateVisible => vec![(VIEWS_ROWS, Some("u")), (VIEWS_OPEN, Some("u"))],
        DerivedOp::ToggleMembership => vec![(VIEWS_ROWS, Some("u")), (VIEWS_OPEN, Some("r"))],
        DerivedOp::SortKeyChange => vec![(VIEWS_ROWS, Some("u")), (VIEWS_BY_TITLE, Some("mu"))],
        DerivedOp::InsertSorted => vec![(VIEWS_ROWS, Some("i")), (VIEWS_BY_TITLE, Some("i"))],
        DerivedOp::ParamFlip | DerivedOp::RebuildAfterReplace => {
            vec![(observed[0], None)]
        }
        DerivedOp::CountToggle => vec![(VIEWS_ROWS, Some("u")), (VIEWS_OPEN_COUNT, None)],
    };
    let entries = expect_entries(&host, &what_first, "the first call");
    call_ok(&rt, &second);
    let what_second = match op {
        DerivedOp::ToggleMembership => vec![(VIEWS_ROWS, Some("u")), (VIEWS_OPEN, Some("i"))],
        DerivedOp::InsertSorted => vec![(VIEWS_ROWS, Some("r")), (VIEWS_BY_TITLE, Some("r"))],
        _ => what_first.clone(),
    };
    expect_entries(&host, &what_second, "the second call");
    host.keeping(false);
    assert_eq!(
        host.counts.change_sets() - sets,
        2,
        "one change-set per call"
    );
    match op {
        DerivedOp::ParamFlip => {
            // `show` went from true to false: the 2,500 done rows left the view.
            let shown: Vec<Item> = Decode::decode_exact(&entries[0].2).expect("the view");
            assert_eq!(
                shown.len(),
                rows as usize / 4 * 3,
                "2,500 rows left a 10,000-row view"
            );
        }
        DerivedOp::RebuildAfterReplace => {
            let open: Vec<Item> = Decode::decode_exact(&entries[0].2).expect("the view");
            assert_eq!(open.len(), rows as usize / 4 * 3, "the open rows");
        }
        DerivedOp::CountToggle => {
            let count = u32::decode_exact(&entries[1].2).expect("the count");
            assert_eq!(count, rows / 4 * 3 - 1, "one row fewer is open");
        }
        _ => {}
    }

    if resets {
        let rt2 = rt.clone();
        with_reset(
            move || {
                black_box(rt.call_sync(black_box(&first)));
            },
            move || {
                black_box(rt2.call_sync(&second));
            },
        )
    } else {
        let mut flip = false;
        plain(move || {
            flip = !flip;
            let payload = if flip { &first } else { &second };
            black_box(rt.call_sync(black_box(payload)));
        })
    }
}

/// One signal write and the read that recomputes a computed derived from it.
fn computed_recompute_1() -> Box<dyn Bench> {
    drive_from_this_thread();
    let source = Signal::new(0_u32);
    let derived = Computed::new(&source, |n| n.wrapping_mul(3));
    let mut next = 0_u32;
    assert_eq!(derived.get(), 0);
    plain(move || {
        next = next.wrapping_add(1);
        source.set(next);
        black_box(derived.get());
    })
}

/// The same through a chain of ten computeds: one write dirties ten nodes, one read recomputes
/// all ten.
fn computed_recompute_chain_10() -> Box<dyn Bench> {
    drive_from_this_thread();
    let source = Signal::new(0_u32);
    let mut chain = vec![Computed::new(&source, |n| n.wrapping_add(1))];
    for _ in 1..10 {
        let previous = chain.last().expect("non-empty").clone();
        chain.push(Computed::new(&previous, |n| n.wrapping_add(1)));
    }
    let last = chain.last().expect("non-empty").clone();
    assert_eq!(last.get(), 10);
    let mut next = 0_u32;
    plain(move || {
        next = next.wrapping_add(1);
        source.set(next);
        black_box(last.get());
        black_box(&chain);
    })
}

// ---------------------------------------------------------------------------------------------
// snapshot
// ---------------------------------------------------------------------------------------------

/// `stores` stores holding 250 rows of 100 bytes each: four of them are the blueprint's 100 KB
/// cold-start scenario, forty its 1 MB crash-recovery one.
fn snapshot_fixture(stores: u32) -> (Arc<Core>, Arc<CountingHost>, Vec<u8>) {
    let (rt, host) = runtime();
    for _ in 0..stores {
        let feed = construct(&rt, "Feed", &[]);
        call_ok(
            &rt,
            &method_call(
                feed,
                "Feed",
                "seed",
                2,
                &[enc(&250_u32), enc(&87_u32)].concat(),
            ),
        );
    }
    let snapshot = rt.snapshot();
    let expected = stores as usize * 25_000;
    assert!(
        (expected..=expected + expected / 100 + 64).contains(&snapshot.len()),
        "{stores} stores should snapshot to about {expected} bytes, not {}",
        snapshot.len()
    );
    (rt, host, snapshot)
}

/// Snapshot, restore, and a whole cold start.
pub fn snapshot() -> Vec<Workload> {
    let mut v = vec![
        Workload::new("snapshot/encode_100kb", || {
            let (rt, _host, _snapshot) = snapshot_fixture(4);
            plain(move || {
                black_box(rt.snapshot());
            })
        }),
        Workload::new("snapshot/restore_100kb", || {
            let (rt, _host, snapshot) = snapshot_fixture(4);
            rt.restore(&snapshot).expect("the snapshot restores");
            plain(move || {
                rt.restore(black_box(&snapshot)).expect("restore");
            })
        }),
        // The blueprint's web crash-recovery row: a 1 MB state restored into a live runtime.
        Workload::new("snapshot/restore_1mb", || {
            let (rt, _host, snapshot) = snapshot_fixture(40);
            rt.restore(&snapshot).expect("the snapshot restores");
            plain(move || {
                rt.restore(black_box(&snapshot)).expect("restore");
            })
        }),
        // ADR-037: the same 100 KB, written by an older build whose `Item.id` was a `u32`: every
        // store's fingerprint differs, so each one is decoded by name and migrated structurally
        // (a widening of every row's id) before it is rebuilt. The budget is 10x the fast path.
        Workload::new("snapshot/restore_100kb_migrated", || {
            let (rt, _host, snapshot) = snapshot_fixture(4);
            let older = older_feed_snapshot(&rt, &snapshot);
            let report = rt
                .restore_with_report(&older)
                .expect("the older snapshot migrates");
            assert_eq!(report.migrated, ["Feed"], "every store migrated");
            plain(move || {
                rt.restore(black_box(&older)).expect("restore");
            })
        }),
        // The part of a cold start that is not the restore (RESULTS.md, Finding 8): the hash of
        // the schema this binary registered, which `Runtime::new` computes before anything else.
        // A row of its own, so that the canonical form going back through a clone of the schema
        // and `serde` (1.84x) fails the baseline gates here and not as a quarter of the
        // cold-start rows. (The clone alone is 1.47x, just inside the gate.)
        Workload::new("snapshot/cold_start_schema_hash", || {
            let schema = undra::meta::collect_schema("undra-core");
            let (rt, _host) = runtime();
            assert_eq!(
                schema.hash(),
                rt.schema_hash(),
                "the schema a runtime hashes at start"
            );
            assert!(
                schema.canonical_json().len() > 20_000,
                "the harness registers tens of kilobytes of schema"
            );
            plain(move || {
                black_box(schema.hash());
            })
        }),
        Workload::new("snapshot/cold_start_restore_100kb", || cold_start(0)),
        Workload::new("snapshot/cold_start_restore_100kb_core_thread", || {
            cold_start(1)
        }),
    ];
    // ADR-059: the records of 100 query handles.
    v.extend(super::query_rows::snapshot_handles());
    v
}

/// `snapshot` as a build whose `Item.id` was a `u32` would have written it: the rows re-encoded
/// with 32-bit ids, the `Feed` type's fingerprint and description of that build.
fn older_feed_snapshot(rt: &Runtime, snapshot: &[u8]) -> Vec<u8> {
    use undra::meta::TypeRef;
    use undra::wire::payload::{Snapshot, SnapshotType};

    let mut old = rt.schema().clone();
    let item = old
        .records
        .iter_mut()
        .find(|r| r.name == "Item")
        .expect("the fixture's Item");
    item.fields[0].ty = TypeRef::U32;
    let mut snap = Snapshot::decode(&mut Reader::new(snapshot)).expect("a snapshot");
    for store in &mut snap.stores {
        for (_, value) in &mut store.signals {
            let rows = Vec::<Item>::decode_exact(value).expect("Feed rows");
            let older: Vec<(u32, String, bool)> = rows
                .into_iter()
                .map(|row| {
                    (
                        u32::try_from(row.id).expect("small ids"),
                        row.title,
                        row.done,
                    )
                })
                .collect();
            *value = older.encode_to_vec();
        }
    }
    let type_ids: Vec<u32> = snap.types.iter().map(|t| t.type_id).collect();
    snap.types = type_ids
        .iter()
        .map(|&type_id| SnapshotType {
            type_id,
            fingerprint: old.store_fingerprint(type_id).expect("a store"),
        })
        .collect();
    snap.description = old.stores_closure(&type_ids).canonical_json();
    let mut w = Writer::new();
    snap.encode(&mut w);
    w.into_vec()
}

/// A new runtime that restores the 100 KB snapshot: what launching the app pays in the core
/// before the first screen. `core_threads == 1` also starts (and, in the untimed reset, joins)
/// the `undra-core` thread, as a platform does; 0 is the wasm shape.
fn cold_start(core_threads: u8) -> Box<dyn Bench> {
    let (_rt, _host, snapshot) = snapshot_fixture(4);
    let slot: std::rc::Rc<std::cell::RefCell<Option<Core>>> = Default::default();
    let held = slot.clone();
    let run = move || {
        let host = Arc::new(CountingHost::default());
        let config = RuntimeConfig {
            platform: "bench".to_owned(),
            core_threads,
            ..RuntimeConfig::default()
        };
        let rt = Core::new(config, host);
        rt.restore(black_box(&snapshot))
            .expect("the snapshot restores");
        *slot.borrow_mut() = Some(rt);
    };
    // The previous runtime is shut down and dropped outside the timed region: an app does not
    // time its own exit.
    with_reset(run, move || drop(held.borrow_mut().take()))
}
