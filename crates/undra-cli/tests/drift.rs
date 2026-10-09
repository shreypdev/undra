//! `undra drift` as the binary runs it: the fixtures `tests/fixtures/drift/{ios,android,web}.json`
//! (one flow recorded as three platforms by `examples/playground/core/tests/drift.rs`, which
//! `UNDRA_BLESS=1` rewrites, with the playground's `schema.json` beside them), the report locked
//! in `tests/golden/drift/ios-android-web.txt` (`UPDATE_GOLDEN=1` rewrites it), the exit status of
//! `--exit-code`, the schema-hash refusal and an unreadable file.

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, run_err, run_ok, undra};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/drift")
}

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/drift")
        .join(name)
}

fn updating() -> bool {
    std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1")
}

/// `undra -C <fixtures> drift <args>`.
fn drift(args: &[&str]) -> std::process::Command {
    let mut cmd = undra();
    cmd.arg("-C").arg(fixtures_dir()).arg("drift").args(args);
    cmd
}

fn check_golden(name: &str, text: &str) {
    let path = golden(name);
    if updating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(
        text, expected,
        "the report changed; UPDATE_GOLDEN=1 cargo test -p undra-cli --test drift after reading the diff"
    );
}

#[test]
fn three_recordings_with_the_schema_print_the_report_locked_in_the_golden() {
    let out = run_ok(&mut drift(&[
        "--schema",
        "schema.json",
        "ios.json",
        "android.json",
        "web.json",
    ]));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    check_golden("ios-android-web.txt", &text);
    // The shape: a header naming the files and the schema, the built-in ignores, aligned lines
    // (platform, an eight-letter kind column, the path, the text), a summary.
    assert!(
        text.starts_with("Drift: ios.json (reference) against android.json, web.json (schema 0x"),
        "{text}"
    );
    assert!(
        text.contains("Ignored: the replies of Clock.* and Rng.*, the arguments of Timer.*, the Idempotency-Key header of Http.request, and t\n"),
        "{text}"
    );
    for expected in [
        "android  calls     Todos.toggle on Todos#0 #5: missing on android ({\"id\":\"00000000-0000-0001-0000-000000000000\"})",
        "android  ports     Http.request #1: reply differs: ios {\"body\":{\"body\":",
        "android  state     Todos#0.remaining: final value differs: ios 2, android 3",
        "web      calls     Todos.add on Todos#0 #6: extra on web ({\"title\":\"Pay the rent\"})",
        "web      state     Todos#0.remaining: final value differs: ios 2, web 3",
        "divergences on 2 of 3 recordings\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // The clock and the random source answered differently on every platform, and no line says
    // so (the header names the rule).
    let body = text.split_once("\n\n").map_or("", |(_, body)| body);
    assert!(
        !body.contains("Clock.") && !body.contains("Rng.") && !body.contains("arguments differ"),
        "{text}"
    );
}

#[test]
fn without_a_schema_ids_and_bytes_are_compared_and_the_header_says_so() {
    let out = run_ok(&mut drift(&["ios.json", "android.json"]));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    check_golden("ios-android-raw.txt", &text);
    assert!(
        text.starts_with("Drift: ios.json (reference) against android.json\n"),
        "{text}"
    );
    assert!(
        text.contains("No schema: ids and bytes are compared") && !text.contains("Todos"),
        "{text}"
    );
    // The standard port is still named, and the same call is missing.
    assert!(
        text.contains("android  ports     Http.request #1: ")
            && text.contains("missing on android"),
        "{text}"
    );
}

#[test]
fn the_exit_status_follows_exit_code_and_only_then() {
    // Divergences: 0 without the flag, 1 with it, and the report is printed either way.
    assert!(
        drift(&["ios.json", "android.json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let gated = drift(&["--exit-code", "ios.json", "android.json"])
        .output()
        .unwrap();
    assert_eq!(gated.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&gated.stdout).contains("of 2 recordings\n"),
        "the report is printed before the exit"
    );
    // A recording against itself agrees: 0 even with the flag.
    let same = run_ok(&mut drift(&["--exit-code", "ios.json", "ios.json"]));
    assert!(
        String::from_utf8_lossy(&same.stdout).ends_with("no drift: 2 recordings agree\n"),
        "{}",
        String::from_utf8_lossy(&same.stdout)
    );
    // Ignoring what differs agrees too (web differs from ios by one add and the list's state).
    let ignored = run_ok(&mut drift(&[
        "--schema",
        "schema.json",
        "--ignore",
        "Todos.add",
        "--ignore",
        "Todos.todos",
        "--ignore",
        "Todos.visible",
        "--ignore",
        "Todos.remaining",
        "ios.json",
        "web.json",
    ]));
    let text = String::from_utf8_lossy(&ignored.stdout);
    assert!(
        text.contains("Also ignored: Todos.add, Todos.todos, Todos.visible, Todos.remaining\n")
            && text.contains("extra on web (ignored)")
            && text.contains("1 divergence on 1 of 2 recordings\n"),
        "an ignored argument is still a call: {text}"
    );
}

#[test]
fn bad_input_teaches() {
    // One file is not a comparison.
    let (code, stderr) = run_err(&mut drift(&["ios.json"]));
    assert_eq!(code, 1);
    assert!(
        stderr.contains("error[undra::C0009]")
            && stderr.contains("at least one other, and 1 was given")
            && stderr.contains("undra dev --record"),
        "{stderr}"
    );
    // A recording of another schema is refused, naming both hashes.
    let scratch = TempDir::new("drift-other-schema");
    let other = scratch.path().join("other.json");
    let text = std::fs::read_to_string(fixtures_dir().join("android.json")).unwrap();
    std::fs::write(
        &other,
        text.replace("0x3c17cd5f59bb6f68", "0x0000000000000042"),
    )
    .unwrap();
    let (_, stderr) = run_err(&mut drift(&["ios.json", other.to_str().unwrap()]));
    assert!(
        stderr.contains("error[undra::C0009]")
            && stderr.contains("(schema 0x0000000000000042) is not a recording of the same core as the reference ios.json (schema 0x3c17cd5f59bb6f68)"),
        "{stderr}"
    );
    // So is a schema that is not the recordings'.
    let (_, stderr) = run_err(&mut drift(&[
        "--schema",
        "../schema-diff/old.schema.json",
        "ios.json",
        "android.json",
    ]));
    assert!(
        stderr.contains("error[undra::C0009]")
            && stderr.contains("is not a recording of the same core as the schema"),
        "{stderr}"
    );
    // A file that is not a recording names itself; a file that is not there is the operating
    // system's complaint, as everywhere.
    let bad = scratch.path().join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let (_, stderr) = run_err(&mut drift(&["ios.json", bad.to_str().unwrap()]));
    assert!(
        stderr.contains("error[undra::C0009]: ")
            && stderr.contains("bad.json is not a recording: "),
        "{stderr}"
    );
    let (_, stderr) = run_err(&mut drift(&["ios.json", "missing.json"]));
    assert!(stderr.contains("error[undra::C0010]"), "{stderr}");
    // A --schema that is not a schema is the usual C0002.
    let (_, stderr) = run_err(&mut drift(&[
        "--schema",
        bad.to_str().unwrap(),
        "ios.json",
        "android.json",
    ]));
    assert!(
        stderr.contains("error[undra::C0002]")
            && stderr.contains("bad.json: the schema is not valid JSON"),
        "{stderr}"
    );
}
