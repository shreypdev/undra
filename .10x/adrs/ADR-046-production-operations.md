# ADR-046: production operations: symbol files from every release build, a debugger path into Rust, background runs, and panic reports for the app's crash reporter

Status: **Accepted** (2026-10-01; proposed on `wt/boundary-adrs`, built on `wt/prod-ops`, see the amendment at the end for what
differs from the text below; Amendment B new Track I "production operations", catalogue M-2, M-5, M-8 and finding 4). Touches SPEC 0 (release artefacts), 5.6/5.1 (what a contained panic
produces), 7 (the web build's debug sections), 8 (one standard port, one standard function, two standard
records), 9 (`Background` flushes persistence), 13, 17 (`runInBackground`, `onPanic`) and ADR-024's standard
table; `undra-cli` (builds, templates, `doctor`), `undra-runtime`, `undra-query`, `undra-ports`,
`undra-bindgen` (the standard table), the three runtimes and two optional platform modules. **No wire change
and no C ABI or wasm ABI change: a background run is an ordinary async call of a standard function, and a
panic report is a standard port call.** The schema of every core changes once (the standard surface grows), so
every core's hash changes once, which is acceptable before publication (ADR-024: "changing the standard
surface already needs an ADR"). Constitution R6, R7, R9, R11, R12.

## Context

The catalogue (`.10x/specs/2026-10-01-competitive-limitations.md`, finding 4): "Running in production is
unplanned. No crash-symbol files are emitted, step-debugging from Swift or Kotlin into the core is
undocumented, and nothing integrates with OS background execution (BGTaskScheduler / WorkManager). Every
production team hits the first. Offline-first teams hit the third. None of the three is in Tracks A–H."
Matrix rows 13 (background execution), 17 (step-debug into shared code) and 33 (crash symbolication): Undra
"no" on all three.

What the code does today:

* **Symbols.** The shim's release profile is `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`,
  `debug = false` (`crates/undra-cli/templates/shim/Cargo.toml.tmpl:48-54`); the web build always runs
  `wasm-opt -Oz … --strip-debug --strip-producers` (`crates/undra-cli/src/builds/web.rs:131-145`; binaryen's
  `--strip-debug` also strips the names section). No platform writes a symbol artefact. The Xcode template's
  Release configuration already asks for `DEBUG_INFORMATION_FORMAT = "dwarf-with-dsym"`
  (`crates/undra-cli/templates/ios/project.pbxproj:194`), but the static library it links has no DWARF, so the
  app's dSYM has no Rust frames. The Android template has no `debugSymbolLevel` (`templates/android/app/build.gradle.kts:16-26`).
  `undra build` has `--release` and nothing about symbols (`crates/undra-cli/src/cli.rs:234-246`).
* **Debugging.** Debug builds use cargo's dev profile (full debug info), and the iOS app links the static core,
  so LLDB could step into Rust, but nothing documents or checks it; on Android the `.so` comes from `cargo ndk`
  unstripped in debug; on the web the module is always the stripped release one.
* **Background.** `Lifecycle` is an event port with `changed(AppState)` (`crates/undra-ports/src/ports.rs:138-144`);
  `undra-query` reacts only to `Active` (`crates/undra-query/src/shared.rs:759-785`); `Background` and
  `Inactive` are ignored by every crate. Swift's Lifecycle is reported by the app (`UndraLifecycle`,
  `Adapters/ConnectivityAdapter.swift:94-129`), Kotlin's is a stub (`adapters/SimplePorts.kt:216-225`), the web
  uses Page Visibility (`adapters/browser.ts:67-97`). No BGTaskScheduler, WorkManager or service-worker sync
  exists anywhere in the repository.
* **Panics.** A contained panic becomes a status 2 reply with `message` and a `Backtrace::force_capture()`
  prefixed with `panicked at file:line:col` (`crates/undra-runtime/src/guard.rs:62-92`, `:127-155`) and a FATAL
  log record, target `undra::panic` (`crates/undra-runtime/src/runtime.rs:759-766`); a detached task's panic is
  logged only (`:1748-1756`). Swift turns status 2 into `UndraCallError.panicked` and reports commands to
  `LoadOptions.onError` (ADR-032); Kotlin has no `onError`; TypeScript's `onError` does not receive traps. An app
  that wants Crashlytics or Sentry to see a core panic has to parse log text.

The probe of ADR-044 shows the symbol half is cheap: a prelinked static Rust object built with
`debug = "line-tables-only"` symbolicates through the app's dSYM (`atos`: `a_undra_report (in t8) (lib.rs:3)`),
and the linked binary is the same size as without debug info.

## Decision

### 1. Every release build writes symbol files

1. The release profile keeps line tables: `debug = "line-tables-only"`, `split-debuginfo = "unpacked"` on Apple
   targets, and **no** `strip` in the profile; `undra build --release` strips the shipped copies itself and
   keeps the unstripped ones. Optimisation and LTO are unchanged, so the code is the same. `--no-symbols` skips
   the symbol outputs; debug builds are unstripped anyway.
2. **Outputs** (namespaced per ADR-044), with a `build/symbols/manifest.json` recording, per artefact, the
   namespace, core version, schema hash and image identity (Mach-O UUID per slice, ELF build id per ABI -
   rustc gets `-C link-arg=-Wl,--build-id` on Android - and the SHA-256 of the shipped wasm):
   * **iOS / macOS:** the prelinked `lib<ns>.a` (ADR-044) keeps its DWARF line tables, so the **app's own dSYM**
     (Xcode Release, `dwarf-with-dsym`, already in the template) contains the Rust frames; Crashlytics and Sentry
     upload that dSYM as they do for Swift. There is no separate core dSYM for a static library.
   * **Android:** `jniLibs/<abi>/lib<ns>.so` is stripped (`llvm-strip --strip-debug --strip-unneeded`, from the
     NDK); `build/symbols/android/<abi>/lib<ns>.so` is the unstripped twin, and
     `build/symbols/android/native-debug-symbols.zip` is the Play Console layout (`<abi>/lib<ns>.so`). The docs
     show Play Console upload and Crashlytics' `unstrippedNativeLibsDir`.
   * **Web:** one `wasm-opt` run produces `build/symbols/web/<ns>.debug.wasm` (optimised, with DWARF line tables
     and the names section, `-g`) and the function map `build/symbols/web/<ns>.wasm.functions.txt`
     (`--print-function-map`); the shipped `build/web/<ns>.wasm` is that module with its debug and name sections
     removed and nothing else changed, so every `wasm-function[i]:0x…` in a production stack trace resolves in
     the debug module and the map.
   * **Host libraries** (JVM desktop, macOS host): `dsymutil` / `objcopy --only-keep-debug` next to the library.
3. **A test proves it** (R9 for symbols): `crates/undra-cli/tests/symbols.rs` builds the playground core in
   release, triggers a known panic and a known crash site, and resolves their addresses with the outputs to the
   right `file:line` on each platform (`atos` with the app dSYM, `llvm-symbolizer` with the Android `.so`, the
   wasm function map). A size check proves the shipped artefacts did not grow.

### 2. A documented, checked debugger path into Rust

1. **Debug builds are the debuggable ones**: `undra build` (no `--release`) for iOS keeps full DWARF in the
   prelinked object; Android debug `.so`s are unstripped and the generated Gradle module sets
   `packaging.jniLibs.keepDebugSymbols += "**/lib<ns>.so"` for the debug variant; the web dev build serves
   `<ns>.debug.wasm`.
2. **Per platform, one page and one `doctor` check each** (`site/docs/debugging.html`, `docs/ONBOARDING.md`):
   * **Xcode:** step into Rust from a Swift call; breakpoints by `breakpoint set -f todos.rs -l 42` (or in the
     editor once the `.rs` file is opened); `undra init` writes an `.lldbinit` that loads the toolchain's Rust
     formatters (`rustlib/etc/lldb_commands`), and `undra doctor` checks the path exists.
   * **Android Studio:** the "Dual (Java + Native)" debugger with the debug `.so`; symbol directory
     `build/symbols/android` for release reproductions; `doctor` checks the NDK's LLDB.
   * **Chrome DevTools:** the C/C++ DevTools Support (DWARF) extension reads `<ns>.debug.wasm`; breakpoints in
     `.rs` files. `doctor` cannot check a browser extension and prints the link.
   * **Host tests:** `rust-lldb` or CodeLLDB on `cargo test`.
3. `crates/undra-cli/tests/debugging.rs` (macOS CI): runs LLDB in batch mode against the iOS-simulator debug
   build of the playground, sets a breakpoint by Rust `file:line`, calls the method from Swift and asserts the
   stop. This is the regression test for "steppable" (prelinking included).

### 3. Background runs: the OS grants a window, the core drains in it

1. **A standard function** (ADR-024 rules: in every schema, never generated, implemented by the runtimes'
   own API): `run_background(deadline_ms: u64) -> BackgroundReport` with
   `BackgroundReport { finished: bool, replayed: u32, refetched: u32, still_pending: u32 }`, called like any async
   free function. It runs every registered **background task** concurrently with the deadline (Clock port,
   R12) and returns when all are done or at `deadline − 500 ms`, whichever is first. A host that is about to lose
   its window cancels the call (status 3); work already done is kept (the offline queue persists per item,
   ADR-037). No ABI change: it is an ordinary call with an ordinary reply and cancellation.
2. **Background tasks** are registered through `inventory` (`BackgroundTask { name, run: fn(&Ctx, Deadline) ->
   BoxFuture<'static, BackgroundOutcome> }`); `undra-query` registers "replay the offline queue",
   "refetch observed-or-persisted stale queries" and "flush pending persistence"; app code registers its own with
   `undra::background::register` (or a `#[undra::background]` attribute on an `async fn(ctx, deadline)`, D1's
   call). Each task holds a `WeakCtx` (ADR-034).
3. **The Lifecycle port keeps its shape**; what changes is who listens: on `Background`, `undra-query` flushes
   debounced persistence immediately and records whether background work is pending (the queue is non-empty or a
   persisted query is stale), which the runtimes read (`stats`) to decide whether to schedule a run. Swift and
   Kotlin report Lifecycle by default (`UIApplication`/`NSApplication` notifications; `ProcessLifecycleOwner` in
   C4a), as the gap audit's PO-9 asks.
4. **Platform integration** (runtime packages, not generated):
   * **iOS:** `UndraBackground.register(taskIdentifier:loader:)` at launch registers a `BGProcessingTask`
     (requires network) and a `BGAppRefreshTask` with `BGTaskScheduler`; the handler loads the core if the app
     was launched in the background (`loader` is the generated `Undra<Ns>.load`), calls
     `core.runInBackground(deadline:)`, cancels it from the task's `expirationHandler`, and calls
     `setTaskCompleted(success: report.finished)`. When the app enters the background with pending work, the
     runtime schedules the request and wraps the transition in `beginBackgroundTask` so in-flight mutations
     finish. `undra init` adds `BGTaskSchedulerPermittedIdentifiers` and the background mode to `Info.plist`.
   * **Android:** a new optional module `android-work` (WorkManager is a dependency the base runtime must not
     carry): `UndraWorker : CoroutineWorker` loads the core with the application context, runs
     `runInBackground(deadline = now + 9 min)` (WorkManager stops a worker at 10), returns `success()` or `retry()`;
     `UndraWork.schedule(context)` enqueues unique work with a network constraint when the app goes to the
     background with pending work, or when a mutation is queued offline.
   * **Web:** within the page's life only in v1.x: on `visibilitychange → hidden` and `pagehide`/`freeze` the
     runtime flushes persistence and runs a short background run (deadline 1 s). Background Sync in a service
     worker needs the core to run inside the worker (its own wasm instance and IndexedDB adapters) and is a
     follow-up, stated as such.
5. **Typed outcome everywhere**: `runInBackground` returns the report (Swift `async -> BackgroundReport`,
   Kotlin `suspend`, TS `Promise`) and throws only `UndraCallError`-class failures; a background run never
   throws a panic into OS callbacks (R6).

### 4. Panic reports reach the app's crash reporter

1. **One structured report per contained panic.** Every panic the runtime contains - a call, a stream, a
   detached task, an effect, a computed (ADR-019/A3), a port or callback dispatcher, a background task, a
   migration hook (ADR-037) - produces a `PanicReport { message: String, location: String /* file:line:col */,
   operation: String /* "Todos.add", "task", "computed Todos.visible" */, thread: String, frames: Vec<PanicFrame
   { address: u64 /* relative to the image base */, symbol: Option<String>, file: Option<String>, line: Option<u32> }>,
   namespace: String, core_version: String, schema_hash: u64, image_id: String /* Mach-O UUID / ELF build id /
   wasm SHA-256 */ }`. `location` and `message` are always present (the panic location is compiled in);
   `symbol`/`file`/`line` are present when the shipped image still has them (debug builds); `address` is always
   present on native so a server-side symbolicator with the files of decision 1 resolves it. Frames are captured
   with the `backtrace` crate's raw instruction pointers on native (a dependency, checked for iOS, Android and
   wasm32 per CLAUDE.md; on wasm32 the frames come from the trap's JavaScript stack instead).
2. **Delivered through a new standard port**, `#[undra::port(sync)] trait Diagnostics { fn panicked(&self,
   report: PanicReport); }`, called fire-and-forget (`port_call_id 0`, the `Log` path, ADR-026 §4) after the
   FATAL log record; the runtimes implement it by default and forward to **`LoadOptions.onPanic`** (Swift,
   Kotlin, TypeScript; called once per report on the main thread; never throws into the core). The default when
   `onPanic` is not set is to log. The status 2 reply and `UndraCallError.panicked` are unchanged.
3. **Recipes, not dependencies.** The docs give the three-line bridges: Crashlytics (`ExceptionModel` with
   `StackFrame(address:)` on iOS, `recordException` with synthetic frames on Android), Sentry (an event with the
   frames' `instruction_addr` and the image's debug id) and the browser's `reportError`. Undra links no reporter.
4. **wasm**: a panic traps the module; the FATAL `undra::panic` record precedes the trap (SPEC 7). The TS
   runtime turns that record plus the trap's stack into the same `PanicReport` (frames as `wasm-function[i]`
   offsets, resolved with decision 1's map) and calls `onPanic` before ADR-049's recovery runs.

## Alternatives considered

* **A separate dSYM for the core.** Static libraries have no dSYM of their own; the app's dSYM is where crash
  reporters look. A dynamic framework would have one, at the costs ADR-044 lists.
* **Keep the release profile stripped and ship a separate debug build for symbols.** The two builds would not be
  the same code, so addresses would not match. One build, two copies is the only reliable way.
* **Full debug info in release.** Larger symbol files and slower links for variable locations nobody reads in
  a crash report; line tables give `file:line` per frame.
* **A `Lifecycle` event `background_window(deadline_ms)`.** Events are fire-and-forget: the host could not
  learn when the core is done (it must call `setTaskCompleted`) and could not cancel. A call has both.
* **WorkManager inside `android-adapters`** forces the dependency on every Android app; an optional module does
  not.
* **Panic reports as parsed log text.** Fragile, loses addresses, and every app writes the parser. A standard
  record is one more ADR-024 item.
* **Link a crash reporter SDK.** Picks a vendor for every app and adds a dependency to the runtimes.

## Consequences

* Rust frames symbolicate in Crashlytics, Sentry and Play Console; debugging into Rust is documented and
  tested on every platform; offline queues drain while the app is in the background on iOS and Android; core
  panics reach the app's reporter as structured values. Catalogue rows 13, 17 and 33 move to "yes" (web
  background: "part.").
* Every core's schema hash changes once (the standard surface gains `Diagnostics`, `PanicReport`, `PanicFrame`,
  `run_background`, `BackgroundReport`); `undra_bindgen::stdlib` and the runtimes' standard types follow
  (ADR-024).
* Release builds take longer (symbol extraction, a second wasm-opt output) and write more files; shipped
  artefacts do not grow (tested).
* Two optional platform modules: `android-work` (WorkManager) and the iOS `UndraBackground` helper (in the Swift
  runtime, BackgroundTasks framework, iOS only).

## Risks

* **Background windows are short and unreliable** (the OS decides). The design never assumes a window: it
  persists progress per item and reports what is still pending.
* **iOS background launch** loads the core without UI; every adapter must be safe without a window scene (the
  Swift adapters are). Covered by a test that runs the BG handler in the simulator
  (`_simulateLaunchForTaskWithIdentifier` in the debugger, documented by Apple).
* **The `backtrace` dependency** must pass the dependency rule; if it does not, frames fall back to std's
  `Backtrace` text (symbols when present, no addresses) and the report says so.

## Implementation brief

1. `crates/undra-cli`: the shim's release profile (decision 1.1); `builds/ios.rs` (keep DWARF in the prelinked
   object), `builds/android.rs` (strip the shipped copy, keep the twin, the Play zip, `--build-id`),
   `builds/web.rs` (`-g` debug module, function map, stripped shipped module), `builds/host.rs`; `manifest.json`;
   `--no-symbols`; templates (Gradle `keepDebugSymbols` for debug, `.lldbinit`, `Info.plist` keys, the background
   registration in the iOS bootstrap and the Android `Application`); `doctor` checks; tests `symbols.rs` and
   `debugging.rs`.
2. `crates/undra-ports`: `Diagnostics` port, `PanicReport`, `PanicFrame`, `BackgroundReport`, the
   `run_background` function and the `BackgroundTask` registry types; fakes (`CaptureDiagnostics`).
3. `crates/undra-runtime`: build a `PanicReport` at every containment site (`guard.rs`, `runtime.rs`
   `note_panic`, task, stream, effect, computed, dispatcher, background task), the `Diagnostics` call after the
   FATAL record, frames via `backtrace` on native; `run_background` (spawn registered tasks with the deadline,
   return at the deadline margin, cancellation).
4. `crates/undra-query`: the three background tasks; `Background` flushes persistence and updates the pending
   flag in `stats_json`.
5. `crates/undra-bindgen`: the stdlib table gains the new items (filtered from app bindings).
6. Runtimes: `LoadOptions.onPanic` and the default `Diagnostics` adapter (Swift, Kotlin, TS); `runInBackground`;
   Swift `UndraBackground` (BackgroundTasks) and default Lifecycle reporting from `UIApplication` notifications;
   Kotlin `android-work` (`UndraWorker`, `UndraWork`); TS page-lifecycle flush and the trap-to-report path.
7. Contract scenarios (provisional numbers): **S27 "panic report"** (a panicking method: the caller gets status 2
   and `onPanic` receives message, `file:line`, operation, namespace and schema hash; a panicking detached task
   reports too) and **S28 "background run"** (a mutation queued offline; connectivity returns; `runInBackground`
   replays it and reports `finished`; a run cancelled at its deadline leaves the item queued, intact).
8. Bench: no runtime row changes (the panic path is cold); the release-size budget rows (`bench/RESULTS.md`
   size table) must not move; a build-time row for `undra build --release` with symbols.
9. Docs: SPEC 0, 5, 7, 8, 9, 13, 17; `site/docs/debugging.html`, `site/docs/crash-reporting.html`,
   `site/docs/background.html`; the cookbook's offline-first page (background drains).

## Dependencies

ADR-044 (namespaced artefacts and the prelinked iOS object, which decision 1 keeps DWARF in), ADR-034
(`WeakCtx` for background tasks), ADR-037 (per-item queue persistence makes cancelled runs safe), C4a (Android
Lifecycle adapter), ADR-049 (web recovery runs after the panic report). D3 (build-system integration) shares
the build plumbing and should land the template changes with it.

## Amendment (2026-10-01, `wt/prod-ops`): what was built, and where it differs from the text above

Everything above is built: the structured panic report on every platform with one shape, symbol files and
`undra symbolicate`, the debugger path, background runs with the two OS helpers, the standard port, function and
records, the fakes, scenarios, SPEC sections, docs. The decisions below are the ones the implementation made.

**Panic reports (decision 4)**

1. **The report is the record of decision 4.1, delivered as decided**: `Diagnostics.panicked(report)`, fire and forget,
   `port_call_id 0`, after the FATAL record, once per contained panic, at every containment site the ADR lists plus
   the executor's and the timer's wakers and the `undra-ffi` boundary entries. A reply to a call with id 0 is ignored by
   the runtime (`Runtime::port_reply`), as `undra-ffi` already did, so a remote dev client may answer one like any port
   call. In `undra dev` the report therefore reaches the attached app's `onPanic`.
2. **Frames without the `backtrace` crate.** `undra-ffi` reads the stack with the platform unwinder
   (`_Unwind_Backtrace`, the symbol `std::backtrace` itself uses, so it is always linked) and installs it as the
   runtime's `FrameSource` (`undra_runtime::install_frame_source`), which keeps `unsafe` in `undra-ffi` (R2) and adds
   no dependency (the risk paragraph's fallback is not needed). Each address is the instruction address less the
   load address of the image that holds the core and points into the call instruction (return address less one), so
   a symbolicator takes it as it is. A debug build also names the frames from the backtrace text. `image_id` is read from
   the image's own headers in memory (Mach-O `LC_UUID`, ELF GNU build id). `namespace` and `core_version` are a
   `CoreIdentity` that `export_core!(.., version = "..")` submits through `inventory`; the generated shim passes the core
   crate's version. A runtime without a frame source (the test runtime, the dev runner) reports no addresses and an
   empty `image_id`.
3. **wasm: the FATAL record carries the rest.** Its text is the message, then `    at <file>:<line>:<col>` and `    in <operation>` on
   lines of their own (SPEC 7), so the TypeScript host builds the same report without the core calling out (it
   cannot). The operation of a wasm trap is tracked by a static set in `dispatch` and `poll_one` on wasm only (not paid
   on native, where the guards know). `namespace` comes from `LoadOptions.namespace`, which the generated entry now passes
   (`UndraIds.namespace`); `coreVersion` stays `""` unless the app passes it (the module carries none); `imageId` is the
   SHA-256 of the module's bytes, computed in the background only when `onPanic` is set (so a recovery-only app's
   `UndraCoreRestarted.report.imageId` is empty); a trap during `undra_init`, before the lazily imported report builder
   has arrived, gets no `onPanic` report (`load` still rejects with the trap).
4. **Names.** The three types are `UndraPanicReport`, `UndraPanicFrame` and `UndraBackgroundReport` in all three languages
   (ADR-024 amendment), not the bare schema names. `LoadOptions.onPanic` is the last parameter, so existing calls
   compile. Swift's default `Diagnostics` adapter is part of `Adapters.platformDefault` (like `Log` and `Clock`), not forced on a
   core loaded with `Adapters.none`; Kotlin registers its own even with `defaultAdapters = false`.
5. **Computed panics** (caught by `undra-signals`) are reported with the hook's recording of the panic; **a panic caught at
   an `undra-ffi` entry** has the entry's name as its operation and no frames.

**Background runs (decision 3)**

6. **Tasks are registered per runtime, linked by use.** `Runtime::add_background_task(name, pending, run)` (and
   `undra::background::register(ctx, ..)` in the facade) instead of an `inventory`-submitted `BackgroundTask` and a
   `#[undra::background]` attribute: the registration is what installs the run machinery and its timer path, so a core
   with no task (a hello world) links none of it (ADR-052) and `run_background` answers an idle run (`finished`, nothing
   done). `BackgroundOutcome` is `Done | Incomplete`; progress goes to the `Deadline` (`note_replayed`,
   `note_refetched`, `reach_refetched`) as it happens, so a cut-off run still reports it. The deadline is measured by the
   runtime's own monotonic clock and enforced by a sleep on the `Timer` port (no `Clock` port call: it would link the
   Clock proxy into every core). `still_pending` is the sum of the tasks' `pending` after the run.
7. **The query tasks** are replay (reads an unreadable queue again first, ADR-049; offline it ends at once, incomplete),
   refetch and flush, and each ends by *settling* (the fetches in flight finish, what they fetched and what waits out
   its debounce is written, the queue's writer is idle), so a run is finished when the client is quiet, not when the queue
   is empty. `Lifecycle.Background` writes debounced cache entries at once and retries an unreadable queue.
   `stats_json` gains `panic_reports` and, once a task is registered, a `background` section.
8. **iOS (3.4): BackgroundTasks does not run in the simulator.** `BGTaskScheduler.submit` is unavailable there and
   `_simulateLaunchForTaskWithIdentifier` finds nothing scheduled, so the Risks paragraph's "a test that runs the BG
   handler in the simulator" cannot be done: the handler's logic is `BackgroundEngine` over four seams and is tested with
   fakes on macOS and on the iOS simulator, and the registration was run on a simulator (it does not crash); the
   debugger commands are for a device. The window's work is 180 s (processing) and 25 s (refresh); `setTaskCompleted` is
   called once with `finished && stillPending == 0`.
9. **Android (3.4): `android-adapters` already reported Lifecycle** by activity counting, so `ProcessLifecycleOwner` and
   `lifecycle-process` were not added; the adapter gained the opt-out and a callback (`onBackgroundWorkPending`) that fires
   after it reported `Background` with `stats().background.pending > 0`. `android-work` is the optional module as decided
   (`UndraWorker`, `UndraWork`; WorkManager 2.10).
10. **Web (3.4)** is as decided: within the page's life (`visibilitychange` to hidden, `pagehide`, `freeze`: Background,
    then a one-second run when work is pending; `backgroundRun: false` opts out); Background Sync is the follow-up.
11. **Scenarios are S29 (panic report) and S30 (background run)**: S27 and S28 are `objects-callbacks`'.

**Symbols and the debugger path (decisions 1 and 2)**

12. **iOS carries a debug map, not DWARF.** `ld -r` writes `N_OSO` stabs naming Cargo's objects (no flag changes that,
    all tried); the app's own dSYM does contain the Rust frames (`dsymutil` follows the map; proved with `atos`), which needs
    Cargo's objects unchanged at the app's link, as the Xcode phase has. A warning fires when they are missing. The
    manifest's iOS `imageId` is `null`: the image that holds the core is the app, and the report's `image_id` is the UUID of
    the app's dSYM.
13. **The web build runs wasm-opt twice, not once.** When binaryen keeps DWARF it skips every DWARF-unsafe pass (measured:
    shipped +4.7% raw on the hello world). Run 1 (`-Oz -g`, no DWARF) writes `<ns>.debug.wasm` (the shipped code with its
    names), `<ns>.wasm.functions.txt` and the shipped module (that module minus the name section, its other sections
    byte-identical); run 2 keeps DWARF and writes `<ns>.dwarf.wasm`, which `vite dev` and the browser's DevTools extension
    read. A wasm frame resolves to the function and its definition line; the panic's own `location` has the line.
14. **Shipped artefacts did not grow** (a test asserts it): Android `.so` smaller (the NDK's `llvm-strip` beats the
    linker's), the iOS linked app byte-identical, the host dylib +16 bytes (one `N_OPT` stab Apple `strip` cannot remove),
    the hello-world web core +2,146 bytes gzipped from the ADR's *schema and dispatch*, not the symbols
    (117,081 to 119,227, measured on the merged tree: the standard surface is in every schema, `run_background` is dispatched by every core, and the
    wasm FATAL record carries location and operation; `--no-symbols` reproduces the old build's size exactly).
    The hello-world JavaScript runtime, as the up-front chunk `main`'s ts-size-e4 gates, grew by 420 bytes gzipped (21,336 to
    21,756), over the 21,500 gate: `web/hello-runtime-js` is raised to 22,000 in `bench/budgets.toml` and recorded at 21,756
    (ADR-052's amendment of 2026-10-01 has the ablation); the report builder (853 bytes) and the `Diagnostics` port are lazy.
    *Review (2026-10-02):* `runInBackground` and the `Diagnostics` registration load on demand too. Merged with
    objects-callbacks: the JavaScript up front is 22,005 (+333 on `main`'s 21,672), gate 22,100; the hello wasm had reached
    121,164, over its 120,000 gate, and is 119,654 after two changes that lose nothing (`guarded` is `Ok(f())` on wasm,
    where nothing can catch a panic; `run_background`'s dispatcher answers an idle core synchronously and links the
    asynchronous run only through `add_background_task`). ADR-052's prod-ops review note has the numbers.
15. **The Android release build ignored ADR-052's home remap** (17 `/Users/<name>` strings in the baseline `.so`); fixed
    (`cargo ndk` gets the remap through `--config build.rustflags`).
16. **`.lldbinit`**: Rust 1.98's sysroot has `lldb_lookup.py` and no `lldb_commands`, so the generated file is one `script` line
    that asks `rustc --print sysroot` and loads whichever exists. The LLDB test attaches to a harness that stops itself
    (a launch hung); both the host and the iOS-simulator-process variants exist.
17. **Release workflows**: the generated CI uploads `build/symbols/` per namespace and platform on a push (the repository's own
    `release.yml` ships no core artefacts and is unchanged).
18. **The TypeScript runtime follows ts-size-e4's structure** (`private _field`, the lazy default ports, the events in `events.ts`
    and `browser-events.ts`): the `Diagnostics` port is registered in `_start` from a dynamic import of the `ports` chunk for
    every non-wasm mode, so a page that loads a native core over `remote` now fetches that chunk (about 2.3 KB gzipped) at
    load; `LoadOptions.namespace` is main's `AttachOptions.namespace` (a wasm report's `namespace` is `""` when none is
    given); `startEventSources` takes an optional `onBackground` callback for the page's background window.
19. **S29 on React Native is a SKIP, with the reason**: the RN column runs a wasm stand-in over `NativeTransport`, which traps
    instead of reporting through `Diagnostics`, as S17 already is there; the native path is `diagnostics.test.ts`, the C++
    host test and RN04 on a device. S30 passes through `NativeTransport`.

Dated 2026-10-01 for ADR-049's "the panic report precedes a restart" and "background runs retry an unreadable queue": both
hold (`onPanic` before the restart sequence; the replay task reads an unreadable queue again).

Dated 2026-10-05 (`wt/symbols-bazel` review): the Linux host library is linked with `-C link-arg=-Wl,--build-id=sha1`, as
the Android libraries are (`builds/host.rs`, `identity_args`). Its GNU build id is what a panic report carries and the
manifest keys `lib<ns>.so.debug` by, and it was the linker's default only: `lld` writes none unless asked, and whether `cc`
asks is a choice of the distribution that built it.
