# drift (`undra drift REFERENCE OTHER...`, Undra 1.2) - adversarial review

**Date:** 2026-10-09 · **Piece:** `wt/drift`, five commits on `453fb6c`; reviewed at `e43c8b2`, fixes after it · **Read:** `CLAUDE.md`,
`.10x/decisions/sde/drift.md`, `git diff 453fb6c` in full, `docs/SPEC.md` 3.1 / 3.3 / 3.8 (the `Lazy<T>` value, the page reply, keyed patches),
`crates/undra-runtime/src/object_table.rs` (handles carry a fresh generation per issue: a released handle is never reused), `persist::decode_dyn`
(trailing bytes are an error, allocations are capped). **Method:** hand-written recordings of the playground's schema under the scratchpad
run through the built binary, the fixtures run with every `--ignore` shape, the goldens regenerated and re-checked, then the fix round below
(macOS, Rust 1.99.0).

## Verdict

Merge once the integrator has read the one note for them (the site section's id). Three Medium findings and one Low are closed with
regression tests; three Lows are recorded with their code paths and left open by choice (each is a design limit, not a bug in what the
piece claims). Nothing High.

## Findings

### Medium

| # | Finding | Repro |
|---|---|---|
| M1 | Two recordings of one platform (a second run of the iOS app, which is exactly how a flaky host gets checked) were indistinguishable in every line: the label is the recording's `platform`, used both in the column and in the texts. | `label-a.json` / `label-b.json` (both `"platform":"ios"`, one `add` title differs): `ios  calls     Todos.add on Todos#0 #2: arguments differ: ios {"title":"a"}, ios {"title":"b"}`; with a divergent state it was `never changed on ios`, with no way to tell which. |
| M2 | Without a schema the header claimed the `Idempotency-Key` header is ignored, while `Rules::strip` can only find it in a decoded request: the piece's own golden `ios-android-raw.txt` said `Ignored: ... the Idempotency-Key header of Http.request, and t` three lines above `Http.request #1: arguments differ`, whose only difference is that header's value (`..3030303130323033..` against `..3430343134323433..`). | The golden itself, and `undra drift ios.json android.json` in the fixtures directory. |
| M3 | A `Lazy<T>` signal's `full` value is `handle u64, len u32, version u64` (SPEC 3.1) and the handle is the page server's, issued per session (SPEC 3.3); `decode_signal` left the whole value as bytes, so two sessions that differ only in that handle drifted. The playground's `Library` store has two such signals (`books`, `evens`). | `lazy-a.json` / `lazy-b.json` (a `Library`, `books` full with `len 3, version 1` and handles `0x..02..` / `0x..0a..`): `android  state     Library#0.books: final value differs: ios {"$bytes":"0000000200000000030000000100000000000000","$type":"Lazy<Item>"}, android {"$bytes":"0000000a00000000030000000100000000000000",..}`. |

### Low

| # | Finding | Repro or code path |
|---|---|---|
| L1 | For a patched signal the compared value is `{"ops", "last"}` (as `docs/TESTING.md`'s table says), but the line said `final value differs`, which a `full` value and a patch sequence that end in the same list contradict. | `pf-a.json` / `pf-b.json` (`full` of `[Buy milk]` against one `patch` adding it): `Todos#0.todos: final value differs: ios [{"done":false,..}], android {"last":{"$bytes":..},"ops":1}`. |
| L2 | `--ignore` accepts a path that names nothing and ignores nothing, silently: `--ignore Http` (one segment, not a function) and, without a schema, `--ignore Http.request.req.headers` both print under `Also ignored:` and change no line. | `undra drift --schema schema.json --ignore Http --ignore '' --ignore 'Http.request.' ios.json android.json`: `Also ignored: Http, Http.request`, the same lines; `Rules::with_ignores` keeps any segments, `ignores_whole` and `strip` match the name exactly. |
| L3 | A port method named whole (`--ignore Http.request`) is left out entirely, count line included (`compare_ports` continues before counting), while a call named whole stays counted (`extra on web (ignored)`); intended by the unit test, unsaid by the docs. | `--ignore Http.request ios.json android.json`: no `Http.request` line at all; `--ignore Todos.add ios.json web.json`: `Todos.add on Todos#0 #6: extra on web (ignored)`. |
| L4 (theory) | `lcs_diff` allocates `(n + 1) * (m + 1)` `usize`s (`vec![vec![0_usize; m + 1]; n + 1]`): two sessions of 20,000 host calls each are 3.2 GB; the comment says sessions are hundreds of events, and a long session paging a lazy list is not. | `crates/undra-testkit/src/drift.rs`, `lcs_diff`. |
| L5 (theory) | Handles inside decoded values (`Option<Todos>`, a `Vec` of objects: `{"$handle": "0x.."}`) are per session like the lazy one and are compared as printed; the decoder has no access to the session's alias table. They match for fresh cores that ran the same flow (the generation counter is process-wide and deterministic), so no fixture shows it. | `crates/undra-testkit/src/decode.rs`, `to_json` for `TypeRef::Object` / a named object. |
| L6 | Port calls of one method pair by index: an extra call in the middle misaligns every later pair, one `arguments differ` and `reply differs` line each on top of the count line. Documented as "the `n`th call". | `compare_ports`, `a.iter().zip(b)`. |

### Info

* **For the integrator:** the section on `site/docs/testing.html` is `id="compare-platforms"` (its TOC entry and `cli.html`'s link `testing.html#compare-platforms` agree), not `id="drift"`. A landing-page link needs `#compare-platforms`, or the id renamed in `testing.html` (heading, anchor, TOC), `cli.html`'s link, and `node site/scripts/build-all.mjs` run.
* The migration note in `.10x/decisions/sde/drift.md` reads as a 1.2.0 `Kind::New` note in the voice of `migrations.rs` (one paragraph, the commands in backticks, what it does and where the docs are).
* `Cargo.lock`: the only change is `undra-testkit` in `undra-cli`'s dependency list; no new crate. `undra-testkit` already depended on `undra-runtime` and `serde_json`, and still builds for `wasm32-unknown-unknown`.
* Checked and sound, with the repro: the ordinal counts `ok` constructor replies only (`fc-a` / `fc-b`: a failed `Todos.new` then a good one against one good one is `Todos.new #2: missing on android`, both stores `Todos#0`); a change-set on a handle no constructor returned is `#unknown-1.signal 3`; an unanswered port call is `"unanswered"`; a length prefix of `0x7fffffff` stays `{"$bytes":"ffffff7f61","$type":"Todos.add"}` and the report completes; a recording without `platform` is labelled by its file; relative paths and paths with spaces are shown as given; the report is stdout only and errors stderr only; no escape code with or without `NO_COLOR`; exit 0, 1 (`--exit-code` with lines, every error), 2 (usage); the same file twice is `no drift`; a file that is not JSON is C0009 naming the file and the reader's error; `--ignore Todos.add` leaves the call counted; a user path matches a name exactly (no prefix reaches another port or method); the `Idempotency-Key` match is case-insensitive and position-independent; the `Clock.` / `Rng.` / `Timer.` rules key on the name with its dot, and a user port of one of those names would carry the standard port's id anyway; the goldens hold no timestamp or absolute path, and `UPDATE_GOLDEN=1` followed by a plain run gives the same bytes.

## What was done

| # | Verdict | What |
|---|---|---|
| M1 | CLOSED | `commands/drift.rs`: when two sessions share a label, every session is named by its file (`ios.json {..}, ios-again.json {..}`). Test `two_recordings_of_one_platform_are_named_by_their_files` (`tests/drift.rs`: `ios.json` against a copy with one title changed). The scratch repro now reads `label-a.json {"title":"a"}, label-b.json {"title":"b"}`. Commit `96d0412`. |
| M2 | CLOSED | `drift.rs`: `BUILT_IN_IGNORES_RAW` for the header without a schema, and the `No schema:` line says the header is compared with the rest of the request; the module and `Rules` docs say the rule needs the schema; `the_report_has_a_header_the_ignores_the_lines_and_a_summary` asserts both lines; the raw golden regenerated; `docs/TESTING.md`, the `long_about`, `site/docs/cli.html` and `llms-full.txt` say "with a schema to find it". Commit `38ce8db`. |
| M3 | CLOSED | `decode.rs`: a lazy `full` decodes to `{"$lazy": "Lazy<T>", "len": n, "version": v}` and an invalidation to `{"$invalidated": .., "len", "version"}` (the handle left out; any other length stays bytes); the test schema has `archive: Lazy<Todo>`; tests `a_lazy_signal_is_its_length_and_version_without_the_page_servers_handle` (decode) and `a_lazy_signal_compares_by_length_and_version_not_by_its_page_server_handle` (drift: another handle is no drift, another length is one line). `docs/TESTING.md` names the shape. The scratch repro now reads `no drift: 2 recordings agree`. Commit `38ce8db`. |
| L1 | CLOSED | `compare_state`: `changes differ` when either side's last op is a patch or an invalidation, `final value differs` only for two `full` values; the unit test and both goldens follow; the `state` row of `docs/TESTING.md` names both texts. Commit `38ce8db`. |
| L2 | NOT CLOSED | Recorded; `docs/TESTING.md` says a path that names nothing is accepted and ignores nothing. A check against the schema's names (methods, port methods, signals, functions) is a small piece of its own, and the schema-less mode has no names to check. |
| L3 | CLOSED | `docs/TESTING.md`: `Http.request` (a port method named whole is left out entirely, its count included). Commit `38ce8db`. |
| L4 | NOT CLOSED | Recorded with the code path; `u32` halves it, Myers' O(ND) removes it. |
| L5 | NOT CLOSED | Recorded with the code path. |
| L6 | NOT CLOSED | Recorded; the design the record chose ("the `n`th call"). |

## Verification (macOS, with the fixes)

`source scripts/env.sh` first in each.

* At `e43c8b2`, before the fixes: `cargo test -p undra-testkit -p undra-cli -p playground-core`: 776 passed, 0 failed over 33 test binaries
  (the whole `undra-cli` integration set included; 15 minutes on this machine).
* After the fixes (`38ce8db`, `96d0412`): `cargo test -p undra-testkit`: 43 unit tests (two new) plus the conformance, fixture and doc
  tests, all passed; `cargo test -p undra-cli --lib --test cli --test drift --test diagnostics`: 476, 10, 5 (one new) and 13 passed;
  `cargo test -p playground-core --test drift`: 1 passed. The other `undra-cli` binaries touch nothing the fixes changed and were not re-run.
* `UPDATE_GOLDEN=1 cargo test -p undra-cli --test drift`, then a plain run: the same bytes (the two goldens changed only by the fixes'
  lines: the raw header and `No schema:` line, and `changes differ` on the four patched-signal lines).
* `cargo fmt --check`: clean. `cargo clippy -p undra-testkit -p undra-cli -p playground-core --all-targets -- -D warnings`: clean.
  `RUSTDOCFLAGS="-D warnings" cargo doc -p undra-testkit -p undra-cli --no-deps`: clean. `cargo build -p undra-testkit --target
  wasm32-unknown-unknown`: builds. `bash scripts/check-no-em-dash.sh`: no em-dash in any tracked file. `node site/scripts/build-all.mjs`:
  `llms-full.txt` updated for the page edit, the search index and sitemap up to date.
* The scratch recordings, re-run on the fixed binary: `label-a` / `label-b` name their files; `lazy-a` / `lazy-b` agree; `pf-a` / `pf-b` say
  `changes differ`; the fixtures without a schema print the raw header; `ios.json android.json web.json` with the schema still end in
  `9 divergences on 2 of 3 recordings`.

## Not verified

* A real `undra dev --record` recording of a platform app was not made (the fixtures come from the Rust `Recorder` through the playground's
  test); the dev server's recorder sets no `platform`, so its files are labelled by name, which the fix for M1 also relies on.
* `scripts/ci-local.sh` was not run by this review (the integrator's merge runs CI on the pushed head).
