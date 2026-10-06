# Review: bindings-test (`undra_bindings_test`, ADR-064), 2026-10-05

Branch `wt/bindings-test`, draft PR #29. Adversarial review of the rule, its tests, the committed example, the pin move to
1.0.0 and the `bump-version.sh` additions. Reviewer fixes are on the branch (`1d7fbcf`, `c0f7bd9`).

Verdict: two Medium defects found and fixed with regression tests; one Medium finding open (the ADR's text, not the code);
the rest Low or informational.

## Findings

### F1 (Medium, fixed): the laid-out tree held absolute links into the execroot

`_committed_tree` copied each language tree with `cp -R`. In a sandbox the files of an input tree are links into the execroot,
and `cp -R` copies links as links, so every file of `bazel-bin/bindings_check_tree/` was an absolute symlink into this machine's
output base (`.../execroot/_main/bazel-out/.../bindings_ts/src/core.ts`). The local test passed because the targets exist here;
a disk or remote cache hands such an output to another machine or another checkout, where the links point at nothing, and
whether the output holds files or links depended on the spawn strategy.

* Repro: `find bazel-bin/bindings_check_tree -type l` in `examples/bazel` listed every file. A new test,
  `//tests:copies_test` (`bazel/tests/expect_copies.sh`: every file of `:identical_tree` must resolve inside that tree), failed on
  the original rule ("tests/identical_tree/kotlin/build.gradle.kts is a link to .../all_languages_kotlin/build.gradle.kts").
* Fix: `cp -RL` (what `write_source_file`'s own update does). `copies_test` passes; the example's tree has no link; the update
  still leaves `committed/` unchanged.

### F2 (Medium, fixed): a missing committed directory stopped the whole build

`write_source_file` makes a `fail_with_message_test` when `out_file` is missing, and that rule calls `fail()` in its
implementation, so the failure is at analysis. One `undra_bindings_test` whose directory was not yet created made
`bazel test //...` report every other test `NO STATUS` ("Executed 0 out of 9 tests: 9 were skipped") and `bazel build //...` abort
("Analysis of target '//tests:fresh.update_test' failed; build aborted"). In a monorepo that is every target of CI, for a
mistake in one package.

* Fix: the macro checks for the directory itself (the glob and `subpackages` check `bazel_lib` uses) and, when it is missing,
  calls `write_source_file(diff_test = False)` for the update target and makes its own `<name>.update_test` that fails when it
  runs, printing the same message. Same test name, so the `test_suite` and the docs are unchanged.
* Regression: the `analysistest` was replaced by `//tests:absent_fails_test` (runs the test, expects failure, "do not exist yet"
  and `bazel run //tests:absent.update`); its dependency on `:absent.update_test` also proves the target now analyses. Re-run of
  the repro: the other 12 tests pass, the fresh one fails with the message.
* Also: `committed` that is a label, absolute, `.`, `..` or has an empty segment now fails loading with a message that says it
  must be a directory below the package (before, a `.` gave Bazel's "invalid glob pattern"; no data was at risk).

### F3 (Medium, open): the rule is stricter than ADR-064 decision 3 says

ADR-064 (on `wt/bazel-first-design`) decision 3: "a file the CLI check ignores, the Bazel test ignores." The rule fails on a
file in the committed directory that is not generated, which `undra bindgen --check` ignores when its manifest does not list it,
and the update deletes it. The piece documents this (guide, record, docstring) and the reason holds: the committed tree carries
no `.undra-generated` manifest, so comparing the directory as a whole is the only way the Bazel test can see a file the schema no
longer generates (the CLI's "stale"), and `write_source_file`'s update empties the directory anyway, so ignoring such a file in
the test while deleting it in the update would be worse. Matching the ADR literally would need the manifest committed and a
custom comparison, against decision 2. The code is right; the ADR's sentence is not. Action for the integrator: amend ADR-064
decision 3 in the design pull request ("the same files with the same normalisation; the committed directory belongs to the rule,
so a file in it that is not generated fails, which is how a file the schema stopped generating is caught without a manifest")
before or with this merge. Not changed here: the ADR is not on this branch.

### F4 (Low, fixed in the docs): BUILD files inside the committed directory

A `BUILD.bazel` under `committed/ts/` gives Bazel's "Directory artifact ... crosses package boundary" warning, fails the test
(extra file) and is deleted by the update. That is the intended strictness, but a Bazel repository that commits bindings will
want to build them, and the guide did not say where the BUILD file goes. The guide now says: keep BUILD files out, put the target
in the enclosing package and glob the committed tree.

### F5 (Info, accepted): `**kwargs`

`kwargs` go to `write_source_file` and to a `test_suite`; `size`, `timeout` or `flaky` would fail on the `test_suite`. The
docstring limits them to `visibility` and `tags`, which both accept. A tag on the suite filters its tests, and the test carries the
same tags, so it is still included.

### F6 (Info): the result is reported as `<name>.update_test`

`bazel test //:bindings_check` runs the suite and Bazel prints `//:bindings_check.update_test`. The name is fixed by `bazel_lib`
and the record says so.

## Verified (all on macOS, Bazel 8.8.1 unless noted)

* `bazel test //tests/...` in `bazel/`: 12 pass (after the fixes; 11 before, uncached run).
* `bazel test //...` in `examples/bazel`: 7 pass, before and after the fixes. `bazel run //:bindings_check.update` leaves
  `git status` empty.
* Failure path: a line appended to `committed/ts/src/types.ts` fails `//:bindings_check` with the diff and
  `bazel run //:bindings_check.update`; a same-size in-place edit of a Kotlin file is also caught (Bazel 8 tracks the contents of
  a source directory; no stale cached pass). Reverted, it passes.
* Fewer languages in the bindings than committed fails (`stale_language_fails_test`); more languages than committed gives
  `diff -r` an "Only in" line for the language's directory, the mechanism `missing_file_fails_test` covers.
* The update writes only below `committed` (`rm -Rf "$out"/{*,.[!.]*}` then copy, `$out` the package-relative path).
* Bazel 9.2.0 (Bazelisk fetched it; separate output bases, `--lockfile_mode=off`): `bazel test //tests/...` in `bazel/`, 11 of 11
  pass (original rule); `bazel test //...` in `examples/bazel`, 7 of 7 pass. Nothing to report for `wt/bazel-compat`.
* `cargo build -p undra-cli`; `undra bindgen -C examples/{cookbook,fieldbook,playground} --check --docs`: up to date;
  `examples/two-cores/{a,b}` with `--docs` and `examples/ios15-sample` without: up to date. The checkout's examples resolve the
  runtimes from the checkout, so the 1.0.0 pin does not change their trees. `cargo test -p undra-cli --test schema_retention
  --test schema_docs -- --ignored`: pass. No test asserts the old `"0.1"` pin of an example (the `"0.1"` strings left are inline
  fixtures of `upgrade.rs`).
* `bump-version.sh --check`: says 1.0.0; `--check 1.1.0-rc.1` lists the seven `undra.toml`, the committed `package.json`,
  `Package.swift` and `build.gradle.kts`. `bump-version.test.sh` passes, and fails when any one of the swift, kotlin, committed
  package.json or two-cores pin entries is removed from the script. The patterns are anchored: the Swift one to the Undra URL,
  the Kotlin one to the `com.github.shreypdev.undra:runtime:v` coordinate, the pin to `version = "` inside `[undra]` only.
* The next real bump, end to end: on a copy of the tree, `bump-version.sh 1.1.0-rc.1`, then `bazel test //:bindings_check` passes
  with no update, and `bazel run //:bindings_check.update` changes nothing beyond the four lines the script moved. From the
  generator: under Bazel the staged project is outside a checkout, so the runtimes are `Release { full_version(pin) }`;
  `full_version("1.1.0-rc.1")` is unchanged; `from: "{version}"`, `v{version}` and `^{version}` are plain substitutions; the
  package's own `"version"` is the generator's constant `0.1.0` and is not touched.
* `check-no-em-dash.sh`, `node site/scripts/build-all.mjs` (only `llms-full.txt` moved, from the guide edit, committed) and
  `check-links.mjs`: clean. No added line names an outside party beyond the rulesets the rules already use.

## Not verified

* Linux (CI's runner): the rule uses `cp -RL`, `diff -r` and `find`, all GNU-compatible; the push runs it.
* A remote cache or remote execution: F1 is fixed by construction; not run against one.
* `scripts/ci-local.sh` in full was not run; the steps this piece touches were run one by one above.
