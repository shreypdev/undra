# Review: the Undra 1.1 integration (PR #40, `wt/undra-1-1`), 2026-10-06

Role: lead adversarial reviewer and fixer of the whole pull request (the eight pieces merged, with the three test fixes from
`main`). Reviewed at `4d29d6e` (the merge of `main` `d0b4ac3`); the fixes are the commits after it on the branch, one per
finding. Four reviewers read the same head through one lens each (the 1.0 user, the Bazel integration, supply chain and
release process, tests and flakiness) and reported; their findings that were mine to address are below with the others.
Everything was run on this Mac (Apple silicon, Xcode 26.6, NDK r27 27.2.12479018, Rust 1.99.0, Bazelisk with 8.8.1 and
9.2.0), shared with the other reviewers' builds (load average 40 to 150), so no timing below is a measurement.

Read first: `CLAUDE.md`, `docs/AGENT_WORKFLOW.md`, the PR body, the 1.1 design (`.10x/specs/2026-10-05-bazel-first-design.md`),
ADR-061 with its amendment, ADR-064, ADR-065, the six piece reviews under `.10x/reviews/2026-10-05-*`, then the diff
(`git diff origin/main...HEAD`, 270 files) area by area: the rules and scripts, the CLI, the bench harness, the cookbook, the
workflows, the docs, the records.

## Verdict

**Clean after the fixes below; nothing above Low stays open on this branch.** The eight pieces hold together: the merged
`run.sh` and `core.bzl` keep every piece's arm; the committed bindings are what the integrated core generates; the Android
core builds with the NDK as a declared input on Bazel 8.8.1 and the iOS app's dSYM resolves the Rust core; the CLI builds
Android with the NDK's clang and no `cargo-ndk`; the leaderboard recipe's numbers are the harness's. What I found and fixed:
one correctness defect in the rules (a kept stage directory that a later build could wait 30 minutes for), one safety defect
in a helper script (an unguarded `rm -rf` of a predictable shared path), one stale number on a page, a guide whose sections
did not read in order, and hygiene in the records. The other reviewers' findings that were mine are closed in the same way,
each with its commit and the check that was re-run.

## Findings

| # | Sev | Finding | Status |
|---|---|---|---|
| F1 | Medium | **A kept stage directory could hold the next build for 30 minutes.** `run.sh` keeps `/tmp/undra-bazel-<key>` after a `keep_debug_objects` build with `.undra-bazel-owner` naming the build's pid; the lock loop takes a directory over only when that pid is dead, so once the system handed the pid to another long-lived process, the next build of the target waited 30 minutes and died ("held by process"). Found by the supply-chain reviewer (M2), confirmed by reading `cleanup` and the loop. | Fixed `b561fea`: the kept directory's owner file says `kept` (no process is), and the loop takes it over as it does a dead build's. New `//tests:stage_lock_test` (a stand-in CLI, two builds over one kept directory, one after a dead owner, one without the attribute; the test's timeout is the hang bound). It fails on the previous `run.sh`: "the owner file does not say kept: '73853'". |
| F2 | Medium | **`use-installed-ndk.sh` removed a predictable shared path, and its second argument, unguarded.** The repository of links was `$TMPDIR/undra-example-ndk-<cksum>`, the same for every user of a machine, `rm -rf`'d and remade on each run; `use-installed-ndk.sh <ndk> <dir>` ran `rm -rf <dir>` on whatever was given. The newest NDK below `$ANDROID_HOME/ndk` was also picked by `sort` of names (`9.0.0` after `27.2.x`). Supply-chain reviewer M3 and L2. | Fixed `4751f6a`: `$XDG_CACHE_HOME/undra/example-ndk-<n>` (`~/.cache`), `mkdir -m 700`, a marker file; a directory without the marker is never touched and is named in the error; the script removes only the five entries it wrote; a repository path with whitespace is refused (the flags are printed for an unquoted `$flags`, which CI and the README use); the NDK is chosen by version. Exercised by hand: version order with `9.0.1`, `26.1.x`, `27.2.x`; a second run; an unmarked directory refused; whitespace refused; a relative second argument; no NDK at all. Then the real build (below). |
| F3 | Low | **The leaderboard page said 771 bytes once.** "On the web" in What to watch kept the old fixture's change-set size; the recipe's test asserts 780 and every other line of the page says 780. | Fixed `7c43cce`. |
| F4 | Low | **The Bazel guide read out of order and said the lint exclusions twice.** "Committed bindings" sat after two lint sections and before Good to know, away from the bindings topics; "Generated code and your linters" was a shorter copy of "Linters over the whole tree" (the first predates the `lint_exclusions` setting on `main`). | Fixed `7c43cce`: one lint section (the ktlint claim and the `//kotlin:lint_test` reference kept; `<span id="lint">` keeps the anchor the 1.0 site has linked to), Committed bindings before Symbol files and Debugging, the table of contents rebuilt. `node site/scripts/build-all.mjs` (no diff on a second run), `check-links.mjs` 55 pages OK. |
| F5 | Low | **Records named a machine path and a scratch project.** `rn-086.md` carried a `/private/tmp/...` path; `ndk-input.md` and its review a scratch project named after the session and `--dir /tmp`. Supply-chain reviewer L5. | Fixed `e7cff4d` for the two records ("a scratch directory"). The review record's line is that reviewer's and stays. |
| F6 | Low | **The three Bazel CI jobs had no timeout** (the 360-minute default). Supply-chain reviewer M4. | Fixed `8b1f62f`: `timeout-minutes: 60` on `bazel-example`, `bazel-example-macos`, `bazel-android`; the branch's Gate run measured 15, 25 and 3 minutes. |
| F7 | Low | **The NDK pins were fetched by nobody.** `bazel-android` overrides both NDK repositories with the installed NDK, so the archives' urls and sha256 in `examples/bazel/MODULE.bazel` were checked only on a user's machine. Supply-chain reviewer M1. | Fixed `b44945d`: `.github/workflows/ndk-pins.yml`, `workflow_dispatch` and monthly, Linux, `contents: read`, 30 minutes, `bazel fetch @android_ndk_linux//:ndk @android_ndk_macos//:ndk --lockfile_mode=off` (both archives: an archive is checked the same way on any host); one sentence in the guide's NDK paragraph. The YAML parses (Ruby); the fetch itself was not run here (the archives are not downloaded on this machine): the first dispatch is its proof. |
| F8 | Low | **`at-minimums.sh` truncated MODULE.bazel before it knew the backup existed.** Supply-chain reviewer L1. | Fixed `6af45cb`: `restore()` copies only from a backup that is there. |
| F9 | Low | **The leaderboard rows' baseline runs the head's `standings.rs` on both sides.** `bench-record-base.sh` copies that file into the base worktree with the harness (a base without it has no baseline at all), so a change that slows it is invisible to the same-job baseline; the absolute budgets and the ratio catch it. Supply-chain reviewer L3. | Said `20dbe0b` in the script and in finding 9 of `bench/RESULTS.md`. |
| F10 | Low | **`ndk::environment` mangled a non-UTF-8 NDK path** (`to_string_lossy`), so Cargo would have been told a linker that does not exist. Supply-chain reviewer L6. | Fixed `dc80248`: the function returns `C0003` naming the path, why (the variables hold text) and the fix (a path of plain characters, or `ANDROID_NDK_HOME`); the one caller propagates it; a unit test with a `\xff` byte. |

Checked and found sound (no finding):

* **The merged `run.sh`.** The `std_version` check runs after the rustc wrapper exists and before any build; `keep_debug_objects` prunes in place (the lock is held to the end); the NDK is set as `ANDROID_NDK_HOME` from the declared input with the machine's variables unset; iOS alone inherits `PATH`. `//tests:std_version_test` and the new `stage_lock_test` cover the two arms.
* **`core.bzl`, `bindings.bzl`, `bindings_test.bzl`, `android_std.bzl`.** The NDK marker (`source.properties`, the shallowest), the standard library's sysroots and its version file, `cfg = "exec"` on the bindings' core, the iOS core's `target_compatible_with`, `keep_debug_objects` passed to the iOS core only, the stage key carrying it; `_exists` for a committed directory that is not there yet; every public symbol documented. The example's `select()` values for `ndk` and `keep_debug_objects` pass the macro's checks (a `select` is not `None`).
* **The CLI.** `ndk.rs` computes the variables Cargo and the `cc` crate read, the 16 KB page argument for 64-bit ABIs, the build id, Windows's `clang.exe` with `--target`; `android.rs` checks the NDK's tools for every ABI before the first build and builds one `cargo rustc` per ABI through the same `build_library` the host uses; doctor, the fixture, the CI template and `ci_workflow.rs` agree that `cargo-ndk` is gone; `identity_args` gives a Linux host library its build id. R2: no `unsafe` in the diff. R12: the cookbook core reads no clock, no randomness (the seed is the `Rng` port's), starts no thread; the harness's `Instant` and threads are the harness's.
* **`bump-version.sh`.** The three new kinds are dispatched like the others (`rewrite_<kind>`); the plan is made before the first write; `bump-version.test.sh` passes and `--check` says 1.0.0.
* **The bench harness.** `p99_ns` parses as the other numbers, is gated with `UNDRA_BENCH_SCALE`, is never gated by a baseline, and `best_of` keeps the best p99 separately; the smoke test runs until a snapshot was applied (no budget on time).
* **The recipe.** `publish` skips an older window (`fetch_max`); the merge in `apply` is checked against a full sort for 40 batches; the exact 780 bytes belong to the wire format (a major version). Open and noted by its own review (L4): a demo task that ends by itself leaves `running` set, so `start_demo` does nothing until `stop_demo`; a demo feed, not the recipe's claim.
* **The workflows.** Every changed workflow parses; `release-smoke.yml`'s `$V` is the job's env; `complete` needs `bazel-android`; `ci-local.rb` parses and skips the two provisioning steps.
* **Hygiene.** `scripts/check-no-em-dash.sh` clean; no credential; no outside party beyond the toolchains and rulesets the text must name (the reference host is named in `bench/RESULTS.md` as it was in 1.0); every `pub` item and public Starlark symbol documented (`cargo doc -D warnings`, read).
* **Docs as a reader.** The guide's commands run as written (the symbolicate command, the LLDB steps, the update command); the quoted lines (`lib.rs:145`, `lib.rs:154`, `runtime.rs:1937`) are the sources' lines; `getting-started.html` and `cli.html` say "no cargo-ndk"; the leaderboard page's numbers are finding 9's; `docs/REACT_NATIVE.md` says what was run on 0.86.3 and what was not; the README's targets table lists each test once.

## Verified, and how

On the reviewed head `4d29d6e` first (the integration as merged), then again on the fixed tree where a fix touches what a
check covers. `source scripts/env.sh`; Bazel through Bazelisk with `--repository_cache=$HOME/.cache/bazel-repository`.

* **The rules' tests**, `bazel test //tests/...` in `bazel/`: 13 of 13 on 8.8.1 and on 9.2.0 (`--lockfile_mode=off`) at the
  reviewed head; 14 of 14 on both with `stage_lock_test`, on the fixed `run.sh`.
* **The example**, in `examples/bazel`: `bazel test //...` 8 of 8 on 8.8.1 (twice: reviewed head, fixed tree) and on 9.2.0
  (`--lockfile_mode=off`); `scripts/at-minimums.sh test //...` 8 of 8 on 8.8.1 (twice) and on 9.2.0, `MODULE.bazel` restored
  and the lock unchanged after each; `bazel run //:bindings_check.update` leaves `git status` clean (twice).
* **Android**: `flags="$(android/use-installed-ndk.sh "$ANDROID_HOME/ndk/27.2.12479018" <dir>)"; bazel build //:mobile_android $flags`
  at the reviewed head, then with the fixed script and no arguments (the repository in `~/.cache/undra/example-ndk-<n>`,
  mode 700): both ABIs, 41.0 MB debug, "16 KB aligned"; `bazel build //android:hello --config=android` builds. Through the CLI,
  no `cargo-ndk` on the path of the test: `UNDRA_TEST_ANDROID=1 cargo test -p undra-cli --test platforms android`, 2 pass
  (release libraries 899,048 and 960,528 bytes, 16 KB aligned, the debug hint).
* **iOS**: `bazel build //:mobile_ios`; `bazel build //ios:app --ios_multi_cpus=sim_arm64 -c dbg --apple_generate_dsym` and
  `bazel test //ios:symbols_test` with the same flags: passes, on the fixed `run.sh` (the kept directory's owner file says `kept`).
* **`ndk-pins.yml`'s command**: `bazel fetch @android_ndk_linux//:ndk @android_ndk_macos//:ndk --lockfile_mode=off` with the
  override flags, so the syntax ran against the overridden repositories: "All external dependencies for the requested targets
  fetched successfully"; the lock untouched. (A `bazel fetch --repo=@..` form was tried first and refused by 8.8.1: "No
  repository visible as '@..' from main repository"; the target form is the one in the workflow.)
* **Cargo**, the whole workspace: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test --workspace --no-fail-fast`: 200 test binaries, 3,857 passed, 0 failed, 25 ignored;
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. After the CLI fix: fmt, `clippy -p undra-cli`, the 11 `builds::ndk`
  unit tests, `cargo doc -p undra-cli` with `-D warnings`.
* **Scripts and workflows**: `bash scripts/bump-version.test.sh` (all checks pass), `scripts/bump-version.sh --check` (1.0.0);
  every changed workflow and `ndk-pins.yml` load as YAML (Ruby); `ruby -c scripts/ci-local.rb`; `bash scripts/check-no-em-dash.sh`.
* **The site**: `node site/scripts/build-all.mjs` (no diff on a second run, after the docs commit), `node site/scripts/check-links.mjs`
  (55 pages OK).
* **The other suites once**: `swift test` in `runtimes/swift/UndraRuntime` (920 tests, 3 skipped, 0 failures);
  `bash examples/cookbook/snippets/check.sh swift kotlin ts` (ok); `npm test` (112 tests, 11 files) and `npm run typecheck` in
  `runtimes/rn/@undra/react-native`.
* **The Gate** on `4d29d6e` (run 37419177491): green (Site's deploy skipped, as on every pull request).

## Not verified

* The Linux jobs (the hosted runner is the gate; the branch's Gate run `37419177491` was green on `4d29d6e` before these fixes;
  the push of this head starts the next).
* `ndk-pins.yml`'s fetch (the archives are not downloaded on this machine): its first dispatch.
* A real wrong pin of `undra.android_std` through Bazel, Windows, a physical iOS device: as the piece reviews say.
* The RN devices workflow (not a required check) failed on this branch's head in its Android job at RN20, "a stalled flood ends
  with WsError.Closed(1008 ..), got closed (1000): end after 6000": the WebSocket flood check on the emulator, whose outcome
  depends on which side closes first. Nothing in 1.1 touches `runtimes/rn`, the WebSocket adapter or that check (the
  workflow's only change is the `cargo-ndk` line), and the same workflow passed on `main` on 2026-10-05. Left to the
  tests-and-flakiness lens; it is not this pull request's to fix and not in the Gate.
