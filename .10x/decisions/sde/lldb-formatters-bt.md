# SDE: lldb-formatters-bt: why a full `bt` with the Rust formatters killed Xcode's LLDB, and the two fixes, 2026-10-05

Branch `wt/lldb-formatters-bt`, from `main` `58e373a`. Brief: with the `.lldbinit` of `undra init` sourced, attached to an iOS
simulator process whose core is a debug `undra build --platform ios`, `frame variable name` and `bt 3` worked and `bt 6` or
`bt -c 60` ended LLDB with exit 132 (SIGILL), three of three runs (found by `wt/xcodeproj-debug`, PR #37). Find the formatter
or frame, fix the template or document the limit, and make `crates/undra-cli/tests/debugging.rs` take a `bt` of the full depth
with the template sourced.

Versions: Xcode 26.6 (lldb-2100.0.17.203), Rust 1.99.0 (its `lib/rustlib/etc/lldb_lookup.py`, no `lldb_commands` file any
more), simulator iOS 26.5 (iPhone 17 Pro). Reproduced with the playground harness of the debugging tests (the C host of
`tests/symbols/harness.c`, in the simulator and as the macOS dylib): the same stack as the Bazel app, `explode` under the
generated dispatcher, `run_dispatcher`, two `catch_unwind` guards, `undra_call_sync`, `main`.

## What it was: two defects, one behind the other

**1. LLDB laid out `undra_runtime::runtime::Runtime` as containing itself (SIGILL, 132).** Not a formatter: with the Rust
category disabled and no Python involved, `SBType.GetFieldAtIndex(0)` on `Runtime` alone killed LLDB (crash report: Clang's
`getASTRecordLayout` and `EmptySubobjectMap` recursing, then a fault in a stack guard region). A post-order walk of every
record reachable from `Runtime` (each child laid out before its parent) stopped at `undra_runtime::blocking::Blocking`, an
enum with the tuple variant `Pool(native::Pool)`. `type lookup` showed why: the variant's struct `Blocking::Pool` and the payload
`undra_runtime::blocking::native::Pool` have the same name, the same byte size (0x30) and no declaration file or line, in one
compilation unit; LLDB's type de-duplication takes the second for the first, and `struct Pool { Pool __0; }` is the result. A
type of another crate (`DynValue::String(String)`, `alloc::string::String`) is parsed first and is not confused: it laid out.
The formatters matter only because their type recognizers (`is_udt`, `is_tuple_type`, `is_gnu_enum`: LLDB 19 callback matchers)
read the fields of every candidate type of every argument, the pointee of a `&Runtime` included; without them `bt` prints
`self=0x...` and never lays `Runtime` out, which is why the plain session survived. `frame variable *self` would not have.

Fix: `Blocking::Pool` is `Blocking::Threads` (private, `crates/undra-runtime/src/blocking.rs`, seven lines). After it the
walk completes. A test scans the core crates (`undra-ffi` and what its `[dependencies]` reach: `undra-meta`, `undra-runtime`,
`undra-signals`, `undra-wire`) for a tuple variant named after a type of its own crate, and fails naming the line; it failed
on the old source. The other hits of the pattern in the tree are in tests, in `undra-macros` (not in a core) and in
`undra-transport` (`ServerMsg::Welcome(Welcome)`, `Traveled(Traveled)`: the dev server's, not in a core), left alone.

**2. The struct summary followed every pointer field (SIGABRT, 134).** With `Runtime` laid out, the same `bt` ended in
`Fatal Python error: Cannot recover from stack overflow`: `StructSummaryProvider` prints a struct's fields, each field's
summary comes from LLDB, and a pointer field gets the pointee's struct summary (LLDB applies a summary through one pointer
unless told not to), so a `&Runtime` argument walked the object graph and, through `Weak<Runtime>`, round and round.
Upstream `rust-lang/rust` master still has it (its `eTypeOptionSkipPointers` is on the u8/i8 formats only).

Fix: the template's second `script` line re-registers the `is_udt` struct summary and the `is_gnu_enum` enum summary with
`eTypeOptionSkipPointers | eTypeOptionSkipReferences` (their options otherwise unchanged); LLDB consults the newest entry
first. A pointer argument prints as its address (`self=0x...`), as LLDB prints it without formatters; a value keeps its summary
(`reason="kaboom"`, `__call={method_id:..., args:size=10}`, and `name="generated"` where the raw `&str` printed garbage). The
line does nothing when the formatters are not loaded or are older (no `register_summary`). `undra upgrade` does not rewrite
`.lldbinit`; a project made by an earlier `undra init` takes the file from a fresh one (docs/ONBOARDING.md says so).

Tried and dropped: `eTypeOptionSkipPointers` alone, without the rename (the recognizer callbacks run before LLDB checks the
option, so `Runtime` was still laid out: crash); loading the formatters without the LLDB 19 recognizers (the `.*` synthetic
of the compatibility path dereferences every pointer for its children: the same layout, the same crash).

## Verified

Host dylib and simulator, each: `command source <template>`, attach, `breakpoint set -f lab.rs -l 222`, `continue`,
`frame variable reason` (`"kaboom"`), `bt 3`, `bt` (33 frames, to `main(argc=2, argv=...) at harness.c:220` and `dyld`), `detach`,
exit 0. `cargo test -p undra-cli --test debugging`: the two LLDB tests now source the template and take the full `bt`
(`assert_full_backtrace_with_formatters`: the `main` frame, the summarized `explode` frame, `run_dispatcher(self=0x`, the
`catch_unwind::do_call(data=0x` frame that was frame 4 of the crash), plus the template test (two `script` lines), the scan
and its parser test. Negative check: the scan fails on `main`'s `blocking.rs`.

## Files

| Where | What |
|---|---|
| `crates/undra-cli/templates/project/lldbinit` | the second `script` line and its comment |
| `crates/undra-runtime/src/blocking.rs` | `Blocking::Pool` to `Blocking::Threads` |
| `crates/undra-cli/tests/debugging.rs` | full-depth `bt` with the template on both paths, the template test, the core scan |
| `docs/ONBOARDING.md`, `site/docs/production.html` | the two limits and what holds now |

## For the integrator

* PR #37's `site/docs/bazel.html` and `examples/bazel/README.md` say to take full backtraces in a session that did not source
  the `.lldbinit`; with this piece landed that sentence is false and can go (the Bazel app's core carries the rename once it is
  built from this `main`).
* The LLDB defect (same-named, same-sized types in one CU conflated) is worth a report to llvm-project with the `Blocking`
  DWARF from `dwarfdump --name Pool -p`; not filed here.
