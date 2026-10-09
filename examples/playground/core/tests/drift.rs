//! One flow recorded as three platforms and compared with `undra::testing::drift`: the reference
//! app's proof of `undra drift` (constitution R10), and the maker of the CLI's fixtures
//! (`crates/undra-cli/tests/fixtures/drift/{ios,android,web}.json` and the `schema.json` beside
//! them).
//!
//! The flow: a `Todos` store, three items, one toggle, then the remote list configured and one
//! item created through `Http`. "android" skips the toggle and its server answers 500; "web"
//! adds a fourth item. Every platform's clock and random source answer differently, which the
//! built-in ignores hide.
//!
//! `UNDRA_BLESS=1 cargo test -p playground-core --test drift` rewrites the fixtures.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use playground_core::Todo;
use undra::meta::ids;
use undra::ports::{HttpMethod, HttpRequest, HttpResponse};
use undra::runtime::RuntimeConfig;
use undra::runtime::testing::{PortCallRecord, RecordingHost, TestRuntime, sync_ok};
use undra::testing::decode::SchemaIndex;
use undra::testing::drift::{Kind, Rules, Session, compare};
use undra::testing::{EventKind, Recorder, Recording};
use undra::wire::payload::{CallTarget, ReplyStatus};
use undra::wire::{Bytes, Decode, Encode, Handle, Writer};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../crates/undra-cli/tests/fixtures/drift")
}

/// Compares `text` with the checked-in fixture `name`, or rewrites it under `UNDRA_BLESS`.
fn check_fixture(name: &str, text: &str) {
    let path = fixtures().join(name);
    if std::env::var_os("UNDRA_BLESS").is_some() {
        std::fs::create_dir_all(fixtures()).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write the fixture");
        return;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("{name} is missing: UNDRA_BLESS=1 cargo test -p playground-core --test drift")
    });
    assert!(
        on_disk == text,
        "{name} is stale: UNDRA_BLESS=1 cargo test -p playground-core --test drift"
    );
}

fn args(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    f(&mut w);
    w.into_vec()
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        platform: "test".to_owned(),
        mode: "inproc".to_owned(),
        core_threads: 0,
        blocking_threads: 0,
        log_level: 5,
    }
}

/// How one platform's session differs from the flow.
struct Variant {
    platform: &'static str,
    /// The toggle is not made.
    skip_toggle: bool,
    /// The server answers 500 to the create.
    server_fails: bool,
    /// A fourth item is added.
    extra_add: bool,
    /// What the platform's clock and random source answer.
    now_ms: i64,
    rng_seed: u8,
}

const IOS: Variant = Variant {
    platform: "ios",
    skip_toggle: false,
    server_fails: false,
    extra_add: false,
    now_ms: 1_700_000_000_000,
    rng_seed: 0,
};

const ANDROID: Variant = Variant {
    platform: "android",
    skip_toggle: true,
    server_fails: true,
    extra_add: false,
    now_ms: 1_700_000_050_000,
    rng_seed: 0x40,
};

const WEB: Variant = Variant {
    platform: "web",
    skip_toggle: false,
    server_fails: false,
    extra_add: true,
    now_ms: 1_700_000_090_000,
    rng_seed: 0x80,
};

/// A stand-in for the platform's adapters, answering as `v` says.
fn platform(host: &RecordingHost, v: &Variant) {
    use undra::meta::ids::{port_id, port_method_id};
    let now_ms = v.now_ms;
    host.script_port(
        port_id("Clock"),
        port_method_id("Clock", "now_ms"),
        move |c| sync_ok(c, &now_ms.to_le_bytes()),
    );
    let ticks = Arc::new(AtomicU64::new(0));
    host.script_port(
        port_id("Clock"),
        port_method_id("Clock", "monotonic_ns"),
        move |c| {
            sync_ok(
                c,
                &(ticks.fetch_add(1_000_000, Ordering::SeqCst)).to_le_bytes(),
            )
        },
    );
    let seed = v.rng_seed;
    host.script_port(port_id("Rng"), port_method_id("Rng", "fill"), move |c| {
        let n = u32::decode_exact(&c.args).unwrap_or(0) as usize;
        sync_ok(
            c,
            &Bytes((0..n).map(|i| seed.wrapping_add(i as u8)).collect()).encode_to_vec(),
        )
    });
    host.script_port(port_id("Log"), port_method_id("Log", "log"), |c| {
        sync_ok(c, &[])
    });
    let fails = v.server_fails;
    host.script_port(
        port_id("Http"),
        port_method_id("Http", "request"),
        move |c: &PortCallRecord| {
            let request = HttpRequest::decode_exact(&c.args).expect("a request");
            let response = match (fails, request.method) {
                (true, _) => HttpResponse::new(500, b"try again later".to_vec()),
                (false, HttpMethod::Post) => {
                    HttpResponse::new(200, br#"{"id":7,"title":"Walk","done":false}"#.to_vec())
                }
                (false, _) => {
                    HttpResponse::new(200, br#"[{"id":7,"title":"Walk","done":false}]"#.to_vec())
                }
            };
            sync_ok(c, &response.encode_to_vec())
        },
    );
}

/// Records the flow as `v`'s platform.
fn record(v: &Variant) -> Recording {
    // Time is a counter that moves differently per platform: `t` is never compared.
    let step = 3 + u64::from(v.rng_seed % 7);
    let now = Arc::new(AtomicU64::new(0));
    let clock = now.clone();
    let rec = Arc::new(
        Recorder::with_clock(
            undra::meta::collect_schema("playground-core").hash(),
            "test",
            move || clock.fetch_add(step, Ordering::SeqCst),
        )
        .with_platform(v.platform),
    );
    let t = TestRuntime::with_host(config(), |h| rec.host(h));
    platform(t.host(), v);
    let mut call_id = 0_u32;
    let mut run = |target: CallTarget, a: Vec<u8>| -> (ReplyStatus, Vec<u8>) {
        call_id += 1;
        rec.call(&t, target, call_id, &a);
        t.run_pending();
        let replies = t.take_replies();
        let reply = replies
            .iter()
            .find(|r| r.call_id == call_id)
            .expect("the call was answered");
        (reply.status, reply.body.clone())
    };

    // The to-do list.
    let (_, body) = run(
        CallTarget::Constructor {
            type_id: ids::type_id("Todos"),
            method_id: ids::method_id("Todos", "new"),
        },
        vec![],
    );
    let handle = u64::from_le_bytes(body[..8].try_into().expect("a handle"));
    rec.observe(&t, handle, u32::MAX, true);
    t.run_pending();
    let on = |name: &str| CallTarget::Method {
        handle: Handle(handle),
        method_id: ids::method_id("Todos", name),
    };
    let mut first = None;
    for title in ["Buy milk", "Walk the dog", "Write the docs"] {
        let (status, body) = run(on("add"), args(|w| w.write_str(title)));
        assert_eq!(status, ReplyStatus::Ok);
        first.get_or_insert(Todo::decode_exact(&body).expect("the added item"));
    }
    let first = first.expect("an item was added");
    if !v.skip_toggle {
        run(on("toggle"), first.id.encode_to_vec());
    }
    if v.extra_add {
        run(on("add"), args(|w| w.write_str("Pay the rent")));
    }

    // The remote list: one item created through `Http`.
    run(
        CallTarget::Function {
            method_id: ids::function_id("configure_remote"),
        },
        args(|w| w.write_str("https://api.test")),
    );
    let (status, _) = run(
        CallTarget::Function {
            method_id: ids::function_id("create_remote_todo"),
        },
        args(|w| {
            w.write_str("inbox");
            w.write_str("Walk");
        }),
    );
    assert_eq!(
        status,
        if v.server_fails {
            ReplyStatus::Error
        } else {
            ReplyStatus::Ok
        }
    );

    rec.push(EventKind::Release { handle });
    t.runtime().release(handle);
    rec.finish()
}

/// `schema` without its doc comments: the fixture is read, not documented, and the hash does not
/// cover them.
fn without_docs(schema: &undra::meta::Schema) -> undra::meta::Schema {
    let mut s = schema.clone();
    for r in &mut s.records {
        r.docs.clear();
        r.fields.iter_mut().for_each(|f| f.docs.clear());
    }
    for e in &mut s.enums {
        e.docs.clear();
        for v in &mut e.variants {
            v.docs.clear();
            v.fields.iter_mut().for_each(|f| f.docs.clear());
        }
    }
    for o in &mut s.objects {
        o.docs.clear();
        o.constructors.iter_mut().for_each(|m| m.docs.clear());
        o.methods.iter_mut().for_each(|m| m.docs.clear());
    }
    s.functions.iter_mut().for_each(|f| f.docs.clear());
    for p in &mut s.ports {
        p.docs.clear();
        p.methods.iter_mut().for_each(|m| m.docs.clear());
    }
    s
}

fn paths(d: &[undra::testing::drift::Divergence], kind: Kind) -> Vec<&str> {
    d.iter()
        .filter(|d| d.kind == kind)
        .map(|d| d.path.as_str())
        .collect()
}

#[test]
fn two_platforms_recordings_of_one_flow_name_where_they_diverged() {
    let schema = undra::meta::collect_schema("playground-core");
    let (ios, android, web) = (record(&IOS), record(&ANDROID), record(&WEB));
    for r in [&ios, &android, &web] {
        assert_eq!(r.schema_hash, schema.hash());
    }
    // The fixtures the CLI's tests run on.
    check_fixture("ios.json", &ios.to_json());
    check_fixture("android.json", &android.to_json());
    check_fixture("web.json", &web.to_json());
    check_fixture("schema.json", &without_docs(&schema).to_json_pretty());

    let rules = Rules::new(Some(SchemaIndex::new(&schema)));
    let reference = Session::from_recording(&ios);
    assert_eq!(reference.label, "ios");

    // android: the toggle is missing, the server answered differently, so the list ended differently.
    let d = compare(&reference, &Session::from_recording(&android), &rules);
    assert_eq!(
        paths(&d, Kind::Calls),
        ["Todos.toggle on Todos#0 #5"],
        "{d:?}"
    );
    assert!(
        d.iter()
            .any(|d| d.kind == Kind::Calls && d.text.starts_with("missing on android")),
        "{d:?}"
    );
    assert_eq!(paths(&d, Kind::Ports), ["Http.request #1"], "{d:?}");
    let http = d
        .iter()
        .find(|d| d.kind == Kind::Ports)
        .expect("a port line");
    assert!(
        http.text.starts_with("reply differs: ios {")
            && http.text.contains("\"status\":200")
            && http.text.contains("\"status\":500"),
        "{}",
        http.text
    );
    assert!(
        paths(&d, Kind::State).contains(&"Todos#0.todos"),
        "the toggle changed the list: {d:?}"
    );
    assert!(paths(&d, Kind::Events).is_empty() && paths(&d, Kind::Observes).is_empty());
    // The clock and the random source answered differently on every platform: the built-in ignores
    // hide that (the Rng reading reaches the Idempotency-Key header of the request).
    assert!(
        !d.iter()
            .any(|d| d.path.starts_with("Clock.") || d.path.starts_with("Rng.")),
        "{d:?}"
    );
    assert!(
        !d.iter().any(|d| d.text.starts_with("arguments differ")),
        "{d:?}"
    );

    // web: one more item.
    let d = compare(&reference, &Session::from_recording(&web), &rules);
    assert_eq!(paths(&d, Kind::Calls), ["Todos.add on Todos#0 #6"], "{d:?}");
    assert_eq!(
        d[0].text, "extra on web ({\"title\":\"Pay the rent\"})",
        "{d:?}"
    );
    assert!(paths(&d, Kind::Ports).is_empty(), "{d:?}");
    assert!(
        paths(&d, Kind::State).contains(&"Todos#0.remaining"),
        "{d:?}"
    );

    // A platform against itself has nothing to say; the user's ignores take lines away.
    assert!(compare(&reference, &reference, &rules).is_empty());
    let d = compare(
        &reference,
        &Session::from_recording(&web),
        &Rules::new(Some(SchemaIndex::new(&schema))).with_ignores([
            "Todos.add",
            "Todos.todos",
            "Todos.visible",
            "Todos.remaining",
        ]),
    );
    assert_eq!(
        paths(&d, Kind::Calls),
        ["Todos.add on Todos#0 #6"],
        "an ignored argument is still a call: {d:?}"
    );
    assert!(paths(&d, Kind::State).is_empty(), "{d:?}");

    // Without a schema the same divergences are found, on ids and bytes.
    let raw = compare(
        &reference,
        &Session::from_recording(&android),
        &Rules::new(None),
    );
    assert_eq!(paths(&raw, Kind::Calls).len(), 1);
    // The Idempotency-Key header cannot be found in bytes, so the arguments differ too.
    assert_eq!(
        paths(&raw, Kind::Ports),
        ["Http.request #1", "Http.request #1"]
    );
    assert!(raw[1].text.starts_with("arguments differ"), "{raw:?}");
    assert!(
        raw.iter().all(|d| !d.path.contains("Todos")),
        "no names without a schema: {raw:?}"
    );
}
