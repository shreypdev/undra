# Undra 1.1: Bazel-first integration (design, 2026-10-05)

Status: approved by the founder on 2026-10-05 (the plan of record for 1.1.0). Decided in conversation; this file is the
record. Pieces land one per worktree through the Gate; the version is rehearsed as `1.1.0-rc.1` before `1.1.0`.

## Goal

ADR-061 made Undra build under Bazel. 1.1 makes it fit an existing Bazel monorepo: managed toolchains, committed
generated code with a drift gate, strict linting over the whole tree, a current Bazel, and rulesets at the versions a
repository already has. Nothing changes for the Cargo and CLI path, which stays the primary one; every piece here is
additive.

## Non-goals (1.2 and later)

* Native Bazel mode: the core as `rust_static_library` per Apple slice and `rust_shared_library` per Android ABI with no
  Cargo or Xcode subprocess inside the action. Needs its own ADR and byte-for-byte parity with the CLI's builds.
* Undra's crates resolved through an application's existing `crate_universe` hub.
* Anything that names a domain. The high-frequency recipe is a leaderboard; it is not a market or a book.

## Pieces

| Slug | What | Acceptance |
|---|---|---|
| `bazel-compat` | Bazel 8.8 and 9.x both supported; the lowest ruleset versions the rules need declared in `bazel/MODULE.bazel` and tested | CI runs `examples/bazel` on 8.8.1 and on 9.x; the example also builds with the rules' minimum versions of `rules_apple`, `rules_swift`, `rules_kotlin`, `aspect_rules_js`; the guide states the range and the minimums |
| `bindings-test` (ADR-064) | `undra_bindings_test`: a Bazel test that fails when committed bindings differ from the schema's output, and a runnable updater | The example commits one set of bindings and the test guards it; failure output names the update command; the guide explains when to commit and when to generate |
| `ndk-input` (ADR-065) | The CLI builds Android with the NDK's clang directly (no `cargo-ndk`); `undra_core` takes the NDK as a labelled input | `undra build --platform android` works with only the NDK installed; `undra doctor` no longer asks for `cargo-ndk`; the Bazel Android core builds with no `--action_env` and no `extra_path`; the Android core and `undra_android_library` build in a Linux CI job |
| `lint-exclusions` | The lint exclusion files bindgen writes become configurable (`beside`, the default, or `none`), and the exact file list is documented with the root-config snippets a repository that lints everything needs | A test covers both settings; the guide lists the files |
| `symbols-bazel` | The `symbols` output group documented, and `undra symbolicate` proven on a crash of a Bazel-built core | A Bazel test in the example symbolicates a recorded report against the Bazel-built symbol files; the guide has the commands |
| `leaderboard-recipe` | Cookbook: a live leaderboard of 100,000 players fed by a snapshot and a stream of deltas; ingest on `spawn_blocking` into an app-owned structure; a windowed derived list and aggregates published in one small transaction; the lock hold during a 1 MB snapshot apply measured | A cookbook page with snippets for Swift, Kotlin and TypeScript that `snippets/check.sh` compiles; a bench row with a budget for the lock hold p99; the page states the measured numbers |
| `docs-bazel-first` | The Bazel guide linked from Getting started, the README quick start and the CLI reference; "a release core under a debug app" as the recommended dev loop with the Bazel flag; a page "One core, several hosts" (native, React Native, web in a worker, one bindings package); a roadmap row | Links present; pages pass `check-links` and the site build; no em-dash |
| `rn-086` | React Native 0.86 verified or the exact API that needs 0.87 named | `docs/REACT_NATIVE.md` says which, with the run that proved it |
| `xcodeproj-debug` | The debugger path from a `rules_apple` application through `rules_xcodeproj`: a breakpoint in a Rust frame on the simulator | A section of the Bazel guide with the verified steps; the example gains the `rules_xcodeproj` target it needs (macOS only) |

Then: `scripts/bump-version.sh 1.1.0-rc.1`, the rehearsal (Release and Release smoke workflows), `1.1.0`, release notes
written for a reader, state checkpoint.

## Sequencing

1. This design (spec, ADR-064, ADR-065, the ADR-061 amendment, the decision records) lands first.
2. `bazel-compat` lands next: every other Bazel piece is tested on its matrix.
3. `bindings-test`, `ndk-input`, `lint-exclusions`, `symbols-bazel`, `leaderboard-recipe`, `rn-086`, `xcodeproj-debug` run in
   parallel worktrees. Each merges `main` before its pull request; the shared files (`bazel/undra/*.bzl`,
   `.github/workflows/ci.yml`, `site/docs/bazel.html`) are edited in small, separate sections to keep merges clean.
4. `docs-bazel-first` lands last, so it describes what shipped.
5. Version, rehearsal, release.

## Method

A cheaper model implements each piece from a brief that names the files, the acceptance line above and the rules below;
an adversarial review by a stronger model precedes every landing; the integrator lands through `scripts/wt.sh merge` once
the Gate is green on the exact head.

Rules every piece follows: no em-dash anywhere; no test depends on machine speed; commit by path; `cargo fmt`, clippy
clean, docs on every `pub` item; nothing about where a requirement came from is written down, the records describe the
work as Undra's own direction (it is: the step after ADR-061).

## Risks

* Lowering ruleset minimums may hit a version that lacks something the rules use. Then the lowest working version is
  declared and the guide says why.
* Bazel 9 may remove an API the rules or the example use. The compat piece fixes it or records the gap.
* The NDK archive is large; CI uses the repository cache and the existing retry pattern for `dl.google.com`.
* Two bindings models (committed, generated) must stay equivalent: the test is the proof, and the guide says which to
  choose.
* The leaderboard bench must not depend on machine speed: a lock-hold budget is measured against a baseline recorded in
  the same job, as every other row.
