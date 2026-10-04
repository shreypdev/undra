# launch-polish: eight fixes before the public launch

Branch `wt/launch-polish` (from main `c58d0c8`, merged with `70a129a`), one pull request, a commit per item.

## 1a. `scripts/bump-version.test.sh` failed once on main ("--check without git")

**Cause.** `bump-version.sh` read its list of files from a process substitution (`done < <(files_and_kinds)`) that ran
beside the loop rewriting those files. The list grepped each manifest for the runtime's range while the loop truncated
and rewrote the same manifests; when the grep landed between the truncation of `runtimes/rn/@undra/react-native/package.json`
(its version) and its write, the file was empty, its peer range was never listed and stayed at the old version. The
git and no-git modes visited the same set in the copy (both list 31 files), so the walk was not the difference: the
last case is simply the first to look after the bump that missed. Passed on the pull request and failed on main
because whether the grep falls inside a microsecond window depends on how the runner schedules two processes.

**Fix.** The plan (every file and its kind) is made before the first write, and the manifests that name the range are
a fixed list of paths and globs (`range_files`), the same with git and without; nothing an install leaves in the tree
is read. The test's existing check (no tracked manifest still names `^<old>`) fails when the list misses one.

**Proof.** The test replays the bump that failed with a planted one-second pause between truncating that file and
writing it: the old script fails it every time (`react-native/package.json's peer range was left behind`), the new one
passes (three runs, with git and without).

## 1b. S32 (Swift) "the shared handle follows: expected 100, got 50"

**Cause.** A transaction makes one change-set per store (`Ctx::txn`), handed to the host one after the other from the
core's thread; the mirror drains whatever has arrived when a frame runs. The page that grows the feed reaches its two
handles as two change-sets, and the test waited for the first handle and read the second at once. Passed on the pull
request (both change-sets enqueued before the frame) and failed on main (the core thread was descheduled between the
two handoffs and a frame drained in between). The TypeScript column's worker mode (`paging-steps.ts`, after the
refetch) had the same shape; Kotlin's S32 closes its second handle before anything follows, and the other "follows"
checks wait on their own condition or flush.

**Fix.** Each waits for the shared handle's own condition (hang-detector deadline) and then checks it holds the same
rows as the first. **Proof.** Swift S32 5/5, the whole Swift column (33/33), TS S32 and its worker mode 3/3.

## 1c. `docs/AGENT_WORKFLOW.md` section 2: the two rules (make the whole list before changing what it reads; wait for
the exact condition you assert).

## 2. `URLSessionWebSocketAdapter` crashed on a background `URLSession`

**Cause.** A background session runs no WebSocket task and takes no task delegate; URLSession raises an Objective-C
exception, which aborts the app. `HttpAdapter` had the same defect (`data(for:)` on a background session aborts).

**Fix.** `connect` refuses a background session or configuration with `WsError.refused(status: nil, ...)` before a task
exists; a request on a background session fails with `HttpError.network`. Documented on both initializers, the Http
class doc and the README's "Your own URLSession".

**Proof.** `BackgroundSessionTests` runs each case in a child process (the test bundle again via `xcrun xctest`,
filtered to one `testChild...` test, which skips outside the child): before the fix all three children died of
SIGABRT and the suite reported three failures and carried on; after it, the three pass. Swift runtime suite 920
executed (3 are the child tests, skipped in the parent run), 0 failures.

## 3a. Bench `stream/backpressure` failed a pull request with byte-identical runtime code

**Cause.** The base is recorded first (best of three) and the head measured minutes later; one slow stretch of the
shared runner covered all three of the head's attempts (12.4 M/s against 18.97).

**Fix.** The recording names the base's stress binary (`stress_binary` in `[meta]`, asked for by
`bench-record-base.sh`; it lives in `target/bench-base`). A scenario that misses only that baseline is decided by three
rounds of base and head in turn, best against best, with the same factors (`Selected::interleaved_stress_failures`;
the base's binary runs one scenario with `UNDRA_BENCH_SCENARIO` and `UNDRA_STRESS_ATTEMPTS=1`). No tolerance changed.

**Proof.** Unit tests (identical code with a swinging runner passes, a 2x regression in every round fails). On this
Mac, a base recorded from main and a doctored baseline 1.6x too fast (a base from a fast moment): five runs of the
head, each missed the recorded baseline and passed the interleaved rounds (base 28.2 to 30.2 M/s, head 29.1 to
29.9 M/s).

## 3b. Kotlin `RemoteReconnectTests` "maxAttempts" timed out at 10 s once in about forty runs

**Cause.** The backoff was already injected (the sleeper records), but each attempt connected for real to a port the
test had closed, and an attempt is bounded only by its 5 s patience. A closed loopback port answers at once only while
no other socket holds it, and the port came from the ephemeral range: an attempt that is not refused waits out 5 s,
and two of those overran the 10 s wait.

**Fix.** The server stays up and answers every reconnect with a 503 to the upgrade; each event (attempts 1 to 3, the
close) is waited for on its own with a 60 s hang detector, and the server counts the three refusals. **Proof.** 20/20
runs in one JVM (about 1.5 s each); the Kotlin suites 891 cases, 0 failed.

## 3c. Two cores' emulator job failed on an HTTP 502 from dl.google.com

**Fix.** `scripts/android-sdk-install.sh`: licenses, then `sdkmanager --install` up to 4 times (15, 30, 60 s apart),
checking `--list_installed` after each try. Before the emulator action (Two cores, RN devices): emulator,
platform-tools, platform 34 and the system image. The NDK installs of ci.yml, bench.yml's size job, the launch
rehearsal and RN devices use it too. Tested against a stand-in sdkmanager (fails twice, then installs; never installs).

## 4. pnpm and yarn in the launch rehearsal

After the npm build, the same generated web app is built with pnpm and with yarn, each that is on PATH or that corepack
holds without a download, and node_modules must hold one copy of `@undra/runtime`. pnpm gets `--no-frozen-lockfile`
(its CI default refuses an app without a lockfile) and yarn 2+ `nodeLinker: node-modules`. Locally both are "skipped:
not installed" (corepack has neither cached); the plumbing was exercised with a stand-in `pnpm`. The real managers run
in the Launch rehearsal workflow on the pull request (the macOS image).

## 5. Test counts: 7,770 from Gate run 37166026801 (Rust 3,812, TypeScript 2,043, Kotlin 890, Swift 914, RN 111).

## 6. The live demo

The playground posts the batch (`applyBatchDrains`, `applyBatchUs`: every drain of the last two seconds and their
times added up) and the slowest drain (`applyMaxUs`). The panel shows "Apply, average" (average of N drains: the
readings' errors even out across the batch) and "Slowest drain" as a bound that is always true (under k + 1 steps for a
reading of k), instead of p50 and p99 over single drains. "< 0.1 ms" stays only when the whole batch took less than a
step. The caption under the numbers gives the measured step and the 100-signal change-set (2.3 us, from bench.json via
the measured slot). One deviation from the dictated caption: "one drain of 100 signals takes 2.3 us" became "one
change-set of 100 signals takes 2.3 us", because the bench row is the core building and delivering a change-set, not
the browser's drain.

## 7. Landing clarity: the copy as decided; the wide diagram's labels are 10.5px (were 12.5px) so they clear the
titles; the narrow variant's call line moved down 6px; getting-started's first mention is a code comment for `undra
init`, which makes no React Native shell, so it names the three it makes; the social card re-rendered with local
Chrome. 332 of 350 words.

## Found on the pull request: RN devices (Android) could not bundle the Release app

Editing `rn-devices.yml` (item 3c) ran that workflow on this pull request, its first run under this name. Its Android
job failed in `createBundleReleaseJsAndAssets`, before anything of this piece: Metro stopped on the runtime's
`export * as codecs from "./codecs.js"` ("Export namespace should be first transformed by
@babel/plugin-transform-export-namespace-from"), which the React Native preset's module transform does not take.
Reproduced on main's tree with `npx react-native bundle --platform android --dev false` in `examples/playground/rn`.
An app built the way docs/REACT_NATIVE.md says would hit it too. **Fix:** `@undra/react-native/babel-plugin` (which
every app adds, step 3) rewrites the form as `import * as _codecs from "./codecs.js"; export { _codecs as codecs };`,
leaving the runtime and its size untouched; a test of the plugin, and a row in the guide's troubleshooting table.
**Proof:** the Release bundle builds locally; the package's 112 tests pass.
