# bindings-test: committed bindings, kept equal to the schema by the build (ADR-064)

Branch `wt/bindings-test`, one pull request. Implements ADR-064's second model: a project may commit the trees
`undra_bindings` produces, and `undra_bindings_test` fails when they are not what the schema generates.

## What

* `bazel/undra/bindings_test.bzl`, exported from `defs.bzl`: `undra_bindings_test(name, bindings, committed, **kwargs)`.
  * `<name>`: a `test_suite` over the one diff test; this is the target a user runs (`bazel test //pkg:<name>`).
  * `<name>.update`: `bazel run //pkg:<name>.update` writes the generated trees into `committed` and removes everything else in
    it. `<name>.update_test` is the diff test `write_source_file` makes (its name is fixed by `bazel_lib`).
  * Built on `bazel_lib`'s `write_source_file` (already a dependency of the module), so the diff (`diff -r`), the update and the
    test are the standard ones; the failure and the missing-directory messages are ours and name the update command.
  * A private rule, `_committed_tree`, lays the `swift`, `kotlin` and `ts` output groups of an `undra_bindings` target out as one
    directory (`swift/`, `kotlin/`, `ts/`: what `undra bindgen --out` writes), so the committed directory is compared as a whole
    with one test and one update. The languages the bindings were generated for are the subdirectories.
* `examples/bazel/committed/` (41 files, the three trees) and `undra_bindings_test(name = "bindings_check", ...)` in its
  `BUILD.bazel`, so `bazel test //...` guards it. The committed tree is what `bazel run //:bindings_check.update` wrote.
* Tests in `bazel/tests/`: `bindings_test_test.bzl` (a suite), `fake_bindings.bzl` (a stand-in for `undra_bindings` with files
  the test writes: the rule reads its output groups and nothing else), the fixtures under `bazel/tests/committed/`, and two
  scripts. Cases: identical trees pass (all languages, and TypeScript only); a changed file, an extra file, a missing file and a
  committed language the bindings no longer have each fail, and the output names `bazel run //tests:<case>.update` and the diff;
  the update target, run against a scratch workspace that starts with a leftover file, writes a directory equal to the expected
  one (dotfiles included) and removes the leftover; a `committed` that does not exist fails the test when it runs with a message
  that names the update target (`absent_fails_test`; review of 2026-10-05: it used to fail analysis); the laid-out directory
  holds copies, not links into the execroot (`copies_test`, same review).
* The guide (`site/docs/bazel.html`): the "What you get" row and the section "Committed bindings" (the rule, the two commands,
  when to choose which model, what is compared). `llms-full.txt` and `search-index.json` regenerated.

## What `undra bindgen --check` compares, and how the rule matches it

Read from `crates/undra-cli/src/bindgen.rs` (`check`) and `commands/bindgen.rs`:

| `--check` | The Bazel test |
|---|---|
| For each file the schema generates: `read_to_string` of the committed file and `==` against the generated text. No normalisation of any kind: not line endings, not trailing space, not Unicode. A missing file is "missing"; a different one "differs". | `diff -r` of the generated tree and the committed one: byte for byte, no `--strip-trailing-cr`, no ignore flags. A missing or different file fails. Same predicate on every file the CLI compares. |
| Mode bits are not looked at. | `diff` does not look at them either; the update writes files without the execute bit (`write_source_file`'s default), as `undra bindgen` does. |
| A file the manifest `.undra-generated` lists that the schema no longer generates, if it is still there: "stale". | A file in the committed directory that is not generated fails (`Only in ...`), whether or not any manifest lists it. |
| A file beside the generated ones that the manifest does not list: ignored (the manifest protects such files from deletion). | **Stricter**: it fails the test, and the update removes it (`write_source_file` empties the directory before copying). |
| The manifest itself is not compared (it is not among the generated files). | Not part of the trees (`undra_bindings` declares the three trees; the manifest sits above them in `undra bindgen`'s output), so it is not committed and not compared. |

Reuse against re-implementation: the Bazel test does not run the CLI's `check`; it needs the host library and the CLI as test
inputs and a manifest the trees do not carry, and the predicate is plain equality of files, which `diff` is. What is shared is
the generator: the trees under test are `undra bindgen`'s own output (`undra_bindings` runs it), so "the same files" is not a
second implementation. `write_source_file` expresses everything the CLI does except one difference, the unlisted extra file,
which the rule keeps strict on purpose (a directory that belongs to the rule has one meaning; the guide says so). Nothing the CLI
normalises is lost, because the CLI normalises nothing.

## Verified

* `bazel test //tests/...` in `bazel/`: 11 tests pass (the 3 existing, 8 new: 2 identical, 4 expected failures, the update
  target, the absent-directory message).
* `bazel test //...` in `examples/bazel/` on macOS: all 7 tests pass (`//:bindings_check`, the Kotlin, TypeScript and Swift
  tests, ktlint, the consumers).
* Failure path on the example: appended a line to `committed/ts/src/types.ts` and ran `bazel test //:bindings_check`: it failed,
  `diff -r` printed the added line, and the message said `bazel run //:bindings_check.update`; reverted, it passes. With
  `committed/` absent the update target still builds and the test names it ("do not exist yet ... run").
* Equivalence with the CLI, on the same tree: `undra bindgen -C examples/bazel --library bazel-bin/core_host/libhello_core.dylib
  --out examples/bazel/committed --check` reports the committed tree up to date (schema hash `0xce5731fc471dc5b8`); after the
  same edit it reports `ts/src/types.ts differs from what the schema generates`; an unlisted stray file in a tree is ignored by
  it and fails the Bazel test (the one difference above).

## Findings

* The committed trees embed the release `undra.toml` pins (`[undra] version`, ADR-063: the full version): `from: "<v>"` in
  `swift/Package.swift`, `runtime:v<v>` in `kotlin/build.gradle.kts`, `^<v>` twice in `ts/package.json` (nothing else in the 41
  files names it). That couples them to every version bump. The example's pin was stale (`"0.1"`: every build printed that the
  project is on Undra 0.1.0 while the CLI is 1.0.0), so it is now `"1.0.0"` and the committed trees say `1.0.0`. The coupling
  is owned by `scripts/bump-version.sh`: new kinds `undra_pin` (`[undra] version` of every `examples/*/undra.toml` and
  `examples/two-cores/*/undra.toml`, seven files, which also ends the stale-pin message everywhere), `swift_release` and
  `kotlin_release` (the two committed lines), and `examples/bazel/committed/ts/package.json` joins `range_files`.
  `scripts/bump-version.test.sh` asserts every tracked example `undra.toml` and the committed files say the new version and
  that none still names the old one. Verified end to end: in a scratch copy of the repository, `bump-version.sh 9.8.7-test.1`,
  then `bazel test //:bindings_check` passed with no update, and `bazel run //:bindings_check.update` left `git status` empty:
  the committed trees equal what the build generates at the bumped version. `docs/RELEASING.md` step 4 says so in one clause.
  The directory is still `committed/` (not `generated/`), which keeps it out of the `examples/*/generated/` glob; it is listed
  by name.
* A `committed` that does not exist failed at analysis (`write_source_file`'s `fail_with_message_test` calls `fail`), which
  stopped every target of a `bazel build //...` or `bazel test //...` until the update had run once. Since the review of
  2026-10-05 the rule checks for the directory itself (the same glob `bazel_lib` uses), and when it is missing makes a test
  that fails when it runs, with the same message; the update target is `write_source_file`'s either way.
* The failure message is placed inside a double-quoted shell string by `bazel_lib`'s `diff_test`: it has no backquote, dollar
  sign or quotation mark.

## Not done

* The ADR itself (ADR-064) lands in its own pull request.
* The manifest `.undra-generated` is not committed; a project that wants `undra bindgen`'s own pruning beside the trees can run
  the CLI (its `--check` accepts the committed directory, as verified above).
