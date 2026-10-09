# ci-wall-time - the Gate in half the time, with every job still run (2026-10-09)

**What the numbers said.** Three green Gate runs of 2026-10-06 took 26 to 31 minutes. The critical path was never the
Linux jobs: the macOS runner pool runs five jobs at once, and the Gate queued seven macOS jobs plus the Two cores
simulator job and the iOS size job, so "Swift runtime + C ABI (macOS)" waited 9 minutes for a runner and "Bazel
example (macOS)" 3 minutes before either started. Inside the pool, four jobs ran their work in a row where nothing
depended on the order: the macOS Bazel example (four Bazel configurations, 23 minutes: 381 + 216 + 556 + 152 s), the
Swift runtime job (the runtime's own tests, then the Xcode projects, 14 minutes), the Two cores iOS job (Debug cores,
then Release cores, 400 + 413 s) and, on Linux, the Rust job (`cargo test --workspace`, 683 s, of which the CLI's
integration tests are two thirds: 486 of 743 s measured on an M-series host, `tests/dev_reload.rs` alone 68 s).

**What changed** (`.github/workflows/ci.yml`, `two-cores.yml`, `scripts/ci-local.rb`):

* Each of the four is two jobs that run side by side, with the same steps between them and nothing dropped:
  `bazel-example-macos` (recent rulesets on Bazel 8.8.1, then on 9.x) and `bazel-example-macos-minimums` (the ruleset
  minimums on both); `bazel-example` and `bazel-example-minimums` the same way on Linux; `macos` (the Swift runtime,
  the C ABI, the goldens, the cdylib check, the iOS cross-build) and `macos-apps` (the Xcode projects: `undra init`'s
  Run Script phase, the cookbook's Swift, Fieldbook's and the playground's iOS apps); `rust` (`cargo test --workspace
  --exclude undra-cli` and everything that does not need the built CLI) and `rust-cli` (`cargo test -p undra-cli`, the
  bindings checks of the cookbook and Fieldbook, the installer, Fieldbook's web app, the build integrations, the
  cdylib schema check); Two cores' `ios` (Debug cores) and `ios-release` (Release cores, which now names its simulator
  as the Debug job does: there is no booted one to reuse).
* The Bazel jobs are split by ruleset set, not by Bazel version, so that each job's 9.x run can read what its 8.8.1
  run built: every `bazel` command of the five Bazel jobs adds `--disk_cache="$HOME/.cache/bazel-disk"` (the runner's
  disk, not persisted between runs; an action's key does not change with the Bazel version, and the minimums are
  another graph, which is why they got the other job).
* The macOS jobs come first in `ci.yml`, longest first. Jobs are queued in the order the file lists them and the
  macOS pool serves its queue first come, first served, so the long ones no longer wait behind the short ones.
* Every roll-up (`Complete`) needs every job of its workflow, the new ids included; nothing runs on a path filter.

**What stays as it was.** No test was loosened, no step was removed, no job can be skipped. `ffi-miri` stays on macOS
(Miri's c-variadic check on Linux's futex path, as its comment says). The Bench and Site workflows are unchanged.
`scripts/ci-local.sh` reads the job list from the workflow files, so it runs the new jobs as it ran the old ones; its
provisioning skips and the slow-pass tables name the new ids (`two-cores/ios-release`, `ci/bazel-example-minimums`,
`ci/bazel-example-macos-minimums`, `ci/rust-cli`).

**Expected.** Ten macOS jobs of 2.5 to 12 minutes instead of seven of 2.5 to 23; the pool's total work is the same
(about 72 runner-minutes), so with five runners the floor is about 15 minutes and the longest single job about 12.
The first Gate run on this branch is the measurement; this record is updated with it.

**Added at the integration review (2026-10-09).** The `macos` job has one more step, after the goldens: the ports golden's
conformer and closure files compiled in an app target under `.defaultIsolation(MainActor.self)` (ADR-066's proof,
`typecheck_swift`'s main-actor pass) on the newest stable Xcode of the image. The pass needs Swift 6.2; the image's default
Xcode is 16.4 (Swift 6.1), on which the test skips it, so until this step the Gate compiled the two files without the mode
only. The goldens step stays on the default Xcode (the oldest compiler CI has); the new step selects the Xcode as the Two cores
jobs do, prints `swift --version`, runs the one test with `--nocapture` and fails if the test reported a skip. On a machine
without an `Xcode_*.app` (a developer's Mac through ci-local) it uses the machine's Swift. About half a minute.

**Not done, and why.** A persisted Bazel disk cache (actions/cache) would make the Bazel jobs minutes shorter again
but grows without bound and evicts the Rust caches (10 GB per repository). `cargo nextest` would run the test
binaries in parallel but changes process isolation for tests that share a process-wide core, which needs its own
piece. A `[profile.dev] debug = 0` for CI shortens compiles and shrinks the caches but the symbolication tests read
debug information; also its own piece.

## Review

`.10x/reviews/2026-10-09-undra-1-2-integration-review.md` (2026-10-09, the release as a whole; the step above is its F1).
