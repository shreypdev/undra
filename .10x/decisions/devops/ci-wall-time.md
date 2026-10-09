# ci-wall-time - the Gate's critical path, measured twice (2026-10-09)

**What the numbers said before.** Three green Gate runs of 2026-10-06 took 26 to 31 minutes. The macOS runner pool runs
a handful of jobs at once and the rest queue ("Swift runtime + C ABI (macOS)" waited 9 minutes for a runner). Four
jobs ran their work in a row where nothing depended on the order: the macOS Bazel example (four Bazel configurations,
23 minutes of wall time), the Swift runtime job (the runtime's tests, then the Xcode projects, 14 minutes), the Two cores
iOS job (Debug cores, then Release cores) and, on Linux, the Rust job (`cargo test --workspace`, 683 s, of which the
CLI's integration tests are two thirds: 486 of 743 s measured on an M-series host).

**The first change** split each of the four into two jobs, added `--disk_cache` to every Bazel command and listed the
macOS jobs first. **The first Gate run on the branch (37904093066) measured it:** 29 minutes, no better. Two reasons,
both visible in the job timings. Only three macOS jobs ran at once, not five: the RN devices and Launch rehearsal
workflows start on the same pull request and took two macOS runners for most of the run, so the Gate's ten macOS jobs
queued behind each other (the last one started 17 minutes in). And the five extra macOS jobs added about ten
runner-minutes of setup (checkout, toolchain, the CLI built again, Bazelisk and the repository cache restored again)
to a pool that was already full: 82 runner-minutes of macOS work against 72 before. The disk cache showed no gain
(the macOS Bazel job's two recent-ruleset configurations took 1,228 s where they took 937 s in one job without it).
On Linux the splits did what they were meant to: the Rust job's halves took 6 and 14 minutes where the one job took 17
to 21, and the Bazel example's halves 8 and 9 where the one job took 17.

**What stays** (`.github/workflows/ci.yml`, `scripts/ci-local.rb`): the Rust job split into `rust` (`cargo test
--workspace --exclude undra-cli` and what needs no built CLI) and `rust-cli` (`cargo test -p undra-cli`, the bindings
checks of the cookbook and Fieldbook, the installer, Fieldbook's web app, the build integrations, the cdylib schema
check; its toolchain has rustfmt and clippy, which the CLI's tests run over the projects they make, the cause of the
first run's one red job); the Linux Bazel example split into `bazel-example` (recent rulesets on 8.8.1 and 9.x) and
`bazel-example-minimums` (the ruleset minimums on both); the macOS jobs listed first, longest first; one step in the
`macos` job that runs ADR-066's main-actor compile pass on the image's newest stable Xcode and fails on a skip (the
image's default Xcode is 16.4 with Swift 6.1, below the pass's 6.2). Every roll-up needs every job; nothing runs on a
path filter; no test was loosened.

**What was reverted:** the macOS Bazel job, the Swift runtime job and the Two cores iOS job are their 1.1 shape again;
no `--disk_cache`.

**What would shorten the Gate from here.** The macOS pool is the limit, and it is shared: a pull request also starts
Launch rehearsal (one macOS job, 10 minutes) and, when its paths match, RN devices (an iOS simulator job of 45 minutes
or more). Running those two on `main` only, or after the Gate, would give the Gate the whole pool; so would less macOS
work per job (the Two cores Release cores and the Xcode projects each build the CLI again). Both are decisions for the
founder, not a piece. The second Gate run on the branch is the measurement of what stays.

**Not done, and why.** A persisted Bazel disk cache (actions/cache) grows without bound and evicts the Rust caches (10
GB per repository). `cargo nextest` would run the test binaries in parallel but changes process isolation for tests
that share a process-wide core, which needs its own piece. A `[profile.dev] debug = 0` for CI shortens compiles and
shrinks the caches but the symbolication tests read debug information; also its own piece.
