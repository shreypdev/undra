# drift: `undra drift REFERENCE OTHER...`, where the hosts diverged (Undra 1.2, "no drift")

Branch `wt/drift`, one piece of the 1.2 release. A CLI command that compares two or more recordings of the same user
flow made on different platforms (iOS, Android, web; `undra dev --record`) and reports where the hosts diverged at the
boundary. The core is deterministic, so with the same inputs every platform gets the same change-sets: the drift that
matters is in the inputs, and that is what is compared. The first file is the reference; every other file is compared
with it. Exit status 0 unless `--exit-code` and a divergence was found, as `undra schema diff` does.

## What

* `crates/undra-testkit/src/decode.rs` (new, `undra::testing::decode`): `SchemaIndex`, built from a `Schema`, names and
  decodes what a recording holds: `method_name` / `call_name` (`Todos.add`, `Todos.new`, `configure_remote`),
  `decode_args` (an object by parameter name), `decode_reply` (the return type on `ok`, the error type on `error`, a
  handle for a constructor), `port_name` (the schema, then the standard names table), `decode_port_args`,
  `decode_port_reply`, `signal_name` (`Todos.todos`, `Todos.*`), `decode_signal` (`full` decoded; a patch or a lazy
  invalidation stays bytes named after the type), `decode_value`, `type_name` (`Vec<Todo>`). It is built on
  `undra_runtime::persist::{decode_dyn, decode_params}`: object references in a type are rewritten to `u64` for the
  decoder and turned back into `{"$handle": "0x.."}` by a walk over the original type. The JSON conventions: records as
  objects, enums as `{"$": "Variant", ...fields}` (tuple fields `"0"`, `"1"`), maps as `{"$map": [[k, v], ...]}`, bytes
  as lower-case hex, 64-bit integers as numbers up to 53 bits else strings, `Duration` `{"$dur_ns": n}`, `Timestamp`
  `{"$ts_ms": n}`, `Uuid` hyphenated, `Decimal` as decimal text, `None` as `null`, anything undecodable
  `{"$bytes": "hex", "$type": "name"}`. Decoding never fails a report. Six unit tests over a schema built in the test
  (`decode::tests::schema`, shared with `drift`'s tests).
* `crates/undra-testkit/src/drift.rs` (new, `undra::testing::drift`):
  * `Session::from_recording`: the normal form. Handles become an `Alias`: `Object { type_id, ordinal }` (the ordinal of
    that constructor's `ok` reply within the session, read from the first 8 LE bytes of the body) or `Unknown(n)` by
    first appearance. Calls in order with the reply paired by call id; port calls per `(port, method)` with the reply
    paired by the core's call id; events in order; observes (`on`) deduplicated in first-observe order; releases;
    change-sets folded per `(alias, signal)` to the last entry's op and bytes plus the count of entries; the commit
    count. `t`, call ids, txn ids and timer ids are dropped. Stream items, timer reports and cancels are not compared.
  * `Rules`: `Rules::new(Option<SchemaIndex>).with_ignores(paths)`. Built in and always on: the replies of `Clock.*`
    and `Rng.*`, the arguments of `Timer.*` (rules on the name, so they hold without a schema), the value of an
    `Idempotency-Key` header inside decoded `Http.request` arguments (`req.headers[*]`, replaced by `"$ignored"`; needs
    the schema), and `t`. A user path is `name` (the first two segments, or one when the schema says it is a free
    function) plus a path inside the decoded value; an empty path ignores the whole arguments (the call still counts)
    or a signal's final value; a path into a list applies to every item.
  * `compare(&reference, &other, &rules) -> Vec<Divergence { kind, path, text }>`, in a fixed order: `calls` (an LCS
    over `(CallKey, decoded arguments after the rules)`; within one run of deletions and insertions a deleted and an
    inserted call of the same target become one "arguments differ" line, the rest "missing on X" / "extra on X" with the
    1-based position), `ports` (per method, sorted by name: a count line, then for the first `min(n)` pairs "arguments
    differ" and "reply differs" with `{"status", "body"}`, an unanswered call shown as `"unanswered"`), `events` (the
    same LCS over `(port, method, payload)`), `observes` (set difference: "observed on X only"), `state` (per `(alias,
    signal)`: "final value differs" with the decoded value, or `{"ops", "last"}` for a patched signal; "never changed
    on X" when one side has no entry).
  * `Report { reference, schema, user_ignores, compared: Vec<Compared { file, label, divergences }> }` and
    `Report::render`: the header `Drift: ios.json (reference) against android.json, web.json (schema 0x..)`, the
    `Ignored:` line (`BUILT_IN_IGNORES`), `Also ignored:` for user paths, a `No schema:` line when there is none, one
    line per divergence `<platform>  <kind>  <path>: <text>` (the kind padded to eight letters, the platform column to
    the longest label), and `N divergences on M of K recordings` or `no drift: K recordings agree`. Nine unit tests:
    aliasing, folding, the LCS, each divergence kind, each built-in ignore, the path parser, the report.
* `crates/undra-testkit/src/lib.rs`: both modules public (`undra::testing` re-exports the crate whole).
* `examples/playground/core/tests/drift.rs` (new): the reference app's proof (R10). One flow (a `Todos` store, three
  items, a toggle, then `configure_remote` and `create_remote_todo` over `Http`) recorded as "ios", "android" (the
  toggle skipped, the server answering 500, another clock and random seed) and "web" (a fourth item) through the Rust
  `Recorder`; asserts the divergences by kind and path, that the built-in ignores hide the clock, the random source
  and the `Idempotency-Key` header, that a platform against itself agrees, that `--ignore`-style rules take lines away,
  and that the comparison without a schema finds the same things on ids and bytes. `UNDRA_BLESS=1` writes the three
  recordings and the playground's `schema.json` (without docs) under `crates/undra-cli/tests/fixtures/drift/`.
* The command: `crates/undra-cli/src/cli.rs` (`Command::Drift(DriftArgs)`: `REFERENCE OTHER...`, `--schema FILE`,
  `--ignore PATH` repeatable, `--exit-code`; a `long_about` and an EXAMPLES / OUTPUT / RECORDING EACH PLATFORM block;
  the top-level help's "ACROSS PLATFORMS"; a parse test), `crates/undra-cli/src/commands/drift.rs` (reads the files;
  the schema is `--schema`, else the project's `schema.json`, else the built core as `schema export` reads it, else
  none; refuses a file whose `schema_hash` differs from the reference's, or a reference that differs from the schema's,
  with `C0009` naming both hashes; a file that is not a recording is `C0009` naming the file and the reader's error;
  an unreadable file `C0010`; a `--schema` that is not a schema `C0002`; prints the report through `ui.line`;
  `Ok(false)` only with `--exit-code` and divergences), `lib.rs` (dispatch, the doc table), `commands/mod.rs`,
  `tests/cli.rs` (`drift` in both help lists), `README.md` (a table row), `Cargo.toml` (`undra-testkit`).
* `crates/undra-cli/tests/drift.rs` (new): the real binary on the fixtures. The report with `--schema` locked in
  `tests/golden/drift/ios-android-web.txt` and the schema-less one in `ios-android-raw.txt` (`UPDATE_GOLDEN=1`
  rewrites); the exit status with and without `--exit-code`, a recording against itself, `--ignore`; the C0009s (one
  file, another schema hash in a file, a schema that is not the recordings', a file that is not a recording), C0010,
  C0002. No new error code, so `tests/diagnostics.rs` is untouched.
* Docs: `docs/TESTING.md` "Comparing sessions across platforms: `undra drift`" (after "Replaying port traffic"),
  `docs/DEV_LOOP.md` a bullet under "Recording a session", `site/docs/cli.html` (the overview line, an `undra drift`
  section after `undra dev`, the TOC entry, `dateModified` 2026-10-09), `site/docs/testing.html` ("Compare platforms"
  after "Replay port traffic", the TOC entry, `dateModified`).

## Verified

* `cargo test -p undra-testkit` (the 15 new unit tests among them), `cargo test -p undra-cli --lib --test cli
  --test drift --test diagnostics`, `cargo test -p playground-core --test drift`; `cargo clippy --all-targets -D warnings`
  on the three crates; `cargo doc` with `-D warnings` on `undra-testkit` and `undra-cli`; `cargo fmt`;
  `scripts/check-no-em-dash.sh`.

## Left out, and why

* The three platform kits do not compare sessions; the comparison and the rendering live in the testkit so a kit can
  reuse the format later, and `undra drift` is the one tool.
* Stream items, timer reports and cancels are not compared: a stream's items are outputs of the core, and timer ids are
  per session; a timer's firing shows up as the calls and change-sets it causes.
* Host-call replies are not compared (they are outputs of the deterministic core); a reply that differs shows up as the
  port answer or the call that caused it.
* A patched signal compares by the number of patches and the last patch's bytes, not by a replayed value: applying
  keyed patches dynamically needs the key field and a patch decoder the testkit does not have, and the count plus the
  last op already name the signal that drifted.
* `site/llms-full.txt` and `site/search-index.json` were not regenerated (the piece owns the two named site files only;
  `node site/scripts/build-all.mjs` does it).

## Review

`.10x/reviews/2026-10-09-drift-review.md` (2026-10-09, at `e43c8b2`; the fixes it made follow on the branch).

## Migration note

For the 1.2.0 entry of `crates/undra-cli/src/migrations.rs`:

```rust
Note {
    kind: New,
    text: "`undra drift REFERENCE OTHER...` compares recordings of one flow made on different platforms \
           (`undra dev --record FILE`, one server per platform) and reports where the hosts diverged at the \
           boundary: a call missing, extra or made with other arguments, a port answered differently, an event \
           pushed on one side only, a signal observed on one side only, a signal whose final value differs. The \
           replies of Clock and Rng, the arguments of Timer, the Idempotency-Key header and `t` are ignored; \
           `--ignore PATH` adds your own; `--exit-code` makes it a CI gate. docs/TESTING.md has it.",
}
```
