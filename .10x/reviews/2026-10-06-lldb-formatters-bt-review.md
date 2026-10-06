# Review: lldb-formatters-bt (PR #38), 2026-10-06

Branch `wt/lldb-formatters-bt` at `6cfc856` (six commits on `main` `58e373a`), adversarial review with fixes.
Machine: macOS, Xcode 26.6 (lldb-2100.0.17.203), Rust 1.99.0, iOS 26.5 simulator (iPhone 17 Pro), under heavy
load from other sessions.

Verdict: **the piece's two fixes are right and needed, but they did not make a full `bt` survive in an app.** A
fresh `undra init` iOS app still killed LLDB (exit 134) on `bt -c 60` from a breakpoint in the core, in its Swift
frames. Fixed on the branch (finding 1), with a regression test; one Low on the scan fixed; the rest noted. With
the fixes, ready to merge once "All green" is green on the head.

## What was run

* `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p undra-runtime`,
  `cargo test -p undra-bindgen` (13 suites), `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`,
  `cargo test -p undra-cli --no-fail-fast`, `bash scripts/check-no-em-dash.sh`, `node site/scripts/build-all.mjs`
  (no diff before the docs fix, regenerated with it) and `check-links.mjs`: all pass on the final head.
* `UNDRA_REQUIRE_TOOLCHAINS=1 cargo test -p undra-cli --test debugging`: 6 of 6 (the PR's 5 and the new Swift test),
  host dylib and booted simulator.
* The scan fails on `main`'s `blocking.rs` (`undra-runtime/src/blocking.rs:316: Pool(Pool)`), proved by putting
  `main`'s file back and restoring it.
* **The real app.** `undra init lldbsmoke --platforms ios --undra-path <this worktree>`, `xcodebuild` Debug for the
  simulator (`CODE_SIGNING_ALLOWED=NO`), `simctl install`, `simctl launch --wait-for-debugger`, `xcrun lldb -b`
  with the project's `.lldbinit` sourced, `process attach`, `breakpoint set -f <core>/src/lib.rs -l 79`
  (`Todos::new`, reached from `MainApp.init` through `Todos.__allocating_init`, `UndraCore.construct`,
  `InprocTransport.callSync`, `undra_call_sync`, the guards and `run_dispatcher`), `continue`, `bt -c 60`,
  `frame variable` in frames 0, 1, 9, 30, 47, `frame variable *self` in `run_dispatcher`:

  | Core source | `.lldbinit` | Result |
  |---|---|---|
  | `main` (`Blocking::Pool`) | `main`'s | exit 132 at the breakpoint stop itself (frame 0's `ctx` holds the `Runtime`) |
  | `main` | this branch's, fixed | exit 132, same place: the rename is needed |
  | `main` | none | `bt` fine (49 lines); `frame variable *self` exit 132 |
  | branch (`Threads`) | the PR's two lines | **exit 134**, `Fatal Python error: Cannot recover from stack overflow` in `bt -c 60` |
  | branch | fixed (three lines) | exit 0: 55 frames to `dyld start`, Rust frames with file and line and summaries (`ctx={0:strong=4, weak=4}`, `name="generated"`), Swift frames as Swift prints them, `*self` laid out (`blocking:Threads({...})`) |

## Findings

### 1. HIGH: in an app, the `bt` still killed LLDB, in the Swift frames (CLOSED)

The PR's harness is C: its stack has Rust frames and a C `main`. An app's `bt` from the core goes on into Swift.
Probing frame by frame (a Python loop printing each frame's variables, flushed, until the abort) stopped at frame
30, `closure #1 in InprocTransport.callSync`, variable `self : UndraRuntime.InprocTransport`. The Rust 1.99
formatters register their struct summary on the recognizer `is_udt`, which matches any type that is not basic, not a
pointer, not an array, in any language (upstream's own comment says the category cannot be limited to Rust). A Swift
class instance is not a pointer type to LLDB, so `eTypeOptionSkipPointers` does not apply, and the summary walks the
Swift object graph round its reference cycles until Python's stack is gone (exit 134). A pure Swift program with two
objects pointing at each other passed to a function dies the same way at its first breakpoint stop, with `main`'s
`.lldbinit` as with the PR's: every Swift developer who let the project's `.lldbinit` load was one Swift frame with a
cycle away from losing the session. The same matching made Swift values unreadable: `bytes={[0]:{}, [1]:{}, ...}`
for an `UnsafeBufferPointer<UInt8>`, and Objective-C objects printed as Rust structs.

Tried and dropped: `SBTypeCategory.AddLanguage` (with `eLanguageTypeC` or `eLanguageTypeRust`, the category stopped
formatting Rust values too: Rust types live in TypeSystemClang and their runtime language is neither).

Fix (`6ae809b`): a third `script` line in the template wraps the recognizers `is_udt`, `is_tuple_type` and
`is_gnu_enum` of `lldb_lookup` so that they decline a type whose `GetTypeFlags()` has `eTypeIsSwift` (Apple's LLDB;
0 elsewhere) or `eTypeIsObjC`. LLDB resolves a callback matcher by name at each match, so replacing the module's
functions is enough; `if hasattr(L, n)` makes it a no-op on older formatters, `if L is not None` without
formatters; sourcing the file twice wraps twice, harmlessly. Test:
`lldb_with_the_lldbinit_shows_a_swift_frame_as_swift_and_survives_a_reference_cycle` compiles a 20-line Swift
program (`xcrun swiftc -g`), stops it with `SIGSTOP`, attaches with the template sourced, breaks in `visit(node:bytes:)`,
runs `bt` and `frame variable`, and asserts the formatters were loaded, the frame printed, and `[0] = 1` (Swift's
formatting); it fails with exit 134 on the PR's template (checked). Re-verified on the app above: exit 0.

### 2. LOW: the variant scan read only half of what a core links (CLOSED)

`core_crates()` started from `undra-ffi`, whose `[dependencies]` reach `undra-runtime`, `undra-meta`, `undra-wire`
and `undra-signals`. The shim (`crates/undra-cli/src/shim.rs`) links the app's core, which depends on `undra`, and
`undra` brings `undra-ports`, `undra-query` and `undra-testkit` too: the pattern could come back there unseen
(a probe `Probe(Probe)` in `undra-query/src/lib.rs` was not reported). Fix (`fd90078`): start from `undra` and
`undra-ffi`, skip proc-macro crates (`undra-macros` runs in the compiler; its `Expand::Instance(Instance)` is
compile-time only), and assert the five crates every core links are in the set. The probe is reported now. No
offender exists in those crates today.

### 3. LOW: what the scan's parser does not see (OPEN, noted)

A source scan, not DWARF, so independent of the Rust version's mangling and fine on Linux CI (it reads files only). It
misses: an enum whose `{` is on a later line (a `where` clause), a one-line enum (`enum X { A(A) }`), a closing brace
indented differently from the `enum` line (tabs: the rest of the file is then skipped), and dependencies written as
`[dependencies.undra-x]` tables. None occurs in the tree. Struct variants (`A { a: A }`) are not flagged, which is
right: the DWARF of those was not shown to be confused. Acceptable for a guard; a DWARF check of a built core would
be exact but needs a build.

### 4. INFO: what the pointer-skipping line no longer shows

A `&T` argument or field prints as its address (`self=0x...`), as without formatters; `frame variable *self` still
gives the full summary, and values keep theirs (`__call={method_id:..., args:size=0}`, `ctx={0:strong=4, weak=4}`,
`name="generated"`, where a plain session prints the raw `&str` running into the next strings). Fine. On
`frame variable *self` upstream's `StdRcSyntheticProvider` prints a Python traceback (`OverflowError: can't convert
negative int to unsigned` for a weak count); LLDB carries on. Upstream's, not ours.

### 5. INFO: versions

CI pins Rust 1.99.0 (`.github/workflows/ci.yml`), and the debugging tests skip off macOS, so CI's Linux job runs
only the scan and the template test. Rust 1.98 is not installed here, so the template on its formatters was not
run; lines 2 and 3 are guarded by `hasattr` and do nothing where the 1.99 names are absent. Android Studio's LLDB
(no `eTypeIsSwift`) gets the Objective-C exclusion only, which matches nothing in a Rust or Kotlin process.

### 6. INFO: public surface, R12, docs, the record

`Blocking` is `pub(crate)`; no schema, C ABI, wasm export or generated code changes (bindgen's 13 suites and the
CLI's goldens pass). No clock, randomness or thread change (R12). ONBOARDING and the production page described two
limits; the third and its fix are added (`0e4bbb0`), and the search index and `llms-full.txt` regenerated. The record
names upstream projects only (`rust-lang/rust`, `llvm-project`), no outside party. No em-dash. The LLDB tests'
waits are hang detectors (120 s caps, a 10 s wait for `SIGSTOP`), not speed assertions.

## PR #40 (`wt/undra-1-1`)

`git merge-tree` of this head with `origin/wt/undra-1-1` (`0cbaac0`) and with `origin/main` (`d0b4ac3`): no
conflicts, and `build-all.mjs` on each merged tree finds the search index and `llms-full.txt` up to date.
`site/docs/bazel.html` on #40 says to "take full backtraces in a session that did not source" the `.lldbinit`,
because a `bt` of six frames or more crashed. Once this lands, a Bazel app whose core is built from that `main` has
both causes fixed (the rename in the core, the Swift exclusion in the template: the Bazel app is a Swift app, so the
PR's original two lines would not have been enough there either). The sentence should then say that a full `bt` works
with the `.lldbinit` sourced. Not re-run on the Bazel example here.
