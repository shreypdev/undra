# Undra

**One Rust core. Native everywhere.**

[![CI](https://github.com/shreypdev/undra/actions/workflows/ci.yml/badge.svg)](https://github.com/shreypdev/undra/actions/workflows/ci.yml)
[![Benchmarks](https://github.com/shreypdev/undra/actions/workflows/bench.yml/badge.svg)](https://github.com/shreypdev/undra/actions/workflows/bench.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**[Docs & site → shreypdev.github.io/undra](https://shreypdev.github.io/undra/)**

Undra owns everything **under the pixels** of your iOS, Android and web apps (domain
logic, reactive state, the data layer, persistence, and the dev loop) while the UI stays
100% native: SwiftUI on iOS, Jetpack Compose on Android, React on the web, and React
Native, written by hand, the way platform engineers want to write them.

It is the opposite of a cross-platform UI framework. Your screens never leave the
platform. Your *logic* stops being written three times.

```text
        SwiftUI            Compose             React
           │                  │                  │
   @Observable stores   StateFlow stores   hooks + signals     ← generated, idiomatic
           └──────────────────┼──────────────────┘
                      binary change-sets
                (one boundary crossing per transaction)
                              │
                     ╔════════╧════════╗
                     ║   Rust core     ║   your app: stores, queries,
                     ║  (one codebase) ║   mutations, persistence, offline
                     ╚═════════════════╝
```

## Why it's fast

Reads never cross the language boundary: each platform holds a mirror of your state,
updated by compact binary change-sets, once per transaction. Lists cross as O(change)
patches, not O(list) copies, and a filtered or sorted view of a list costs what changed. The
numbers below are measured by the benchmark suite in [`bench/`](bench/RESULTS.md) on an
Apple-Silicon host. The core operations are gated in CI against host budgets (a regression fails
the build), and so are the web size and the sizes of the native cores (Android per ABI, iOS). The
"Budget" column holds the design's per-device targets. Rows from the iPhone
simulator, the Android emulator and headless Chromium are in
[`bench/RESULTS.md`](bench/RESULTS.md#device-numbers-ios-android-web); none is from a physical
phone, so no device target is claimed as met:

| Operation | Measured | Budget |
|---|---|---|
| Synchronous core call (C ABI, core side) | **49.8 ns** | ≤ 60 ns |
| 1 KB record round trip | **228 ns** | ≤ 3 µs |
| One insert into an observed 10,000-row list | **6.3 µs** | ≤ 20 µs |
| Filtered view of a 10,000-row list, one row changed (158 bytes on the wire, was 353 KB) | **392 ns** | ≤ 1 µs |
| Change-set for 100 dirty signals | **2.3 µs** | ≤ 100 µs |
| Cold start restoring 100 KB of state | **85 µs** | ≤ 3 ms |
| Web core: Undra's runtime and a hello-world core, one wasm module | **<!--measured:web-size-->113.1 KB<!--/measured-->** gzipped | ≤ 120 KB |
| Android core (`.so`, arm64-v8a, release, hello world) | **<!--measured:android-size-->898.2 KB<!--/measured-->** | ≤ 1.2 MB |

The web size is measured, not typed: [`scripts/wasm-size.sh`](scripts/wasm-size.sh) builds the
`undra init` template for the web the way an app does (`wasm-opt -Oz`, gzip level 9), and CI fails
a change that takes it over 120 KB or more than 5% over its record
([`bench/results/web-size.jsonl`](bench/results/web-size.jsonl), [ADR-052](.10x/adrs/ADR-052-web-bundle-size.md)).
The JavaScript runtime the page loads up front with it is gated the same way:
<!--measured:web-runtime-js-->15.8 KB<!--/measured--> gzipped against a 16 KB budget, for the production
build of `@undra/runtime` as an app installs it (what only a feature or a mode needs, such as streams, the worker and
remote transports and the default ports, loads when the app asks for it and is not in it; messages are an error code
with a link, and the readable sentences ship in the development build,
[ADR-057](.10x/adrs/ADR-057-js-runtime-16kb.md)). The Android and iOS cores are gated the same
way: [`scripts/native-size.sh`](scripts/native-size.sh) builds the template with `undra build --release`
(the size-tuned `release-mobile` profile, which still unwinds on a panic) and CI fails a library that is
over the design's budget or more than 5% over its record
([`bench/results/native-size.jsonl`](bench/results/native-size.jsonl)).
Sustained-load results (a firehose, keyed churn, fan-out, a 60-second soak) are under
[Harsh conditions](bench/RESULTS.md#harsh-conditions).

## Why you can trust it

* **<!--trust:tests-total-->7,770<!--/trust--> tests across the platforms**: Rust
  <!--trust:tests-rust-->3,812<!--/trust--> · TypeScript <!--trust:tests-typescript-->2,043<!--/trust--> ·
  Kotlin <!--trust:tests-kotlin-->890<!--/trust--> · Swift <!--trust:tests-swift-->914<!--/trust--> ·
  React Native <!--trust:tests-react-native-->111<!--/trust-->. These are the tests that passed in a recent green CI
  run, each suite's own count ([`site/data/tests.json`](site/data/tests.json) names the run).
* **<!--trust:scenarios-->35<!--/trust--> wire-level contract scenarios, run on every platform**
  (<!--trust:cells-->101<!--/trust-->/<!--trust:cells-->101<!--/trust--> cells pass; two scenarios are about the web
  host and run on TypeScript only): sync/async calls, typed errors, cancellation, stream backpressure, keyed patches,
  optimistic rollback, offline queue replay, snapshot/restore, schema-mismatch rejection, panic containment, a
  coalesced 1,000-transaction burst applied in one drain, a derived keyed list whose 60,000 recorded operations replay
  to the core's views, typed storage failures, WebSocket, SSE and Db, two cores in one process, objects and host
  callbacks, panic reports, background runs, newtypes, paged queries and polling. See
  [`contract-tests/`](contract-tests/scenarios.md).
* **Adversarial reviews**: the four core crates (signals, runtime, macros, ffi) first, every High/Medium finding
  fixed and independently re-verified, the unsafe boundary under ASan and Miri; then every feature since,
  before it merged. The reports live in [`.10x/reviews/`](.10x/reviews/).
* **The reference app is real**: [`examples/playground`](examples/playground) runs one
  Rust core on Chrome, an iPhone simulator and an Android emulator, with proof screenshots
  committed; its React Native app runs on the same simulator and emulator.

## What it looks like

Write your domain once, in Rust:

```rust
use undra::prelude::*;

/// The to-do list: what every UI observes and calls.
#[undra::store]
pub struct Todos {
    #[undra(key = "id")]
    todos: Signal<Vec<Todo>>,
    filter: Signal<Filter>,
    #[undra(key = "id")]
    visible: DerivedList<Todo>,
    remaining: Computed<u32>,
}

#[undra::api(store)]
impl Todos {
    pub async fn add(&self, title: String) -> Result<Todo, TodoError> { /* … */ }
    pub fn set_filter(&self, filter: Filter) { /* … */ }
}
```

`undra bindgen` emits code a native reviewer would sign off on (no wrappers, no
reflection, no `Any`):

```swift
// SwiftUI: the store is @Observable; reads are local, instant.
struct TodoScreen: View {
    let store: Todos           // try Todos() after UndraPlaygroundCore.load()
    var body: some View {
        List(store.visible) { TodoRow($0) }
        Text("\(store.remaining) left")
    }
}
```

```kotlin
// Compose: signals are StateFlows.
val todos by store.visible.collectAsState()
LazyColumn { items(todos, key = { it.id }) { TodoRow(it) } }
```

```tsx
// React and React Native: hooks from @undra/runtime/react (vue / svelte / solid adapters ship too).
const store = useUndra(Todos);
const todos = useSignal(store?.visible);
```

Under the hood, `store.add("milk")` crosses the boundary once; the resulting change-set
updates `todos`, `visible` and `remaining` on every observer in a single main-thread
apply. Data fetching, caching, optimistic mutations with precise rollback, offline queues
and persistence are built in (`#[undra::query]` / `#[undra::mutation]`).

## Install

Two ways to get the same `undra` command (macOS and Linux, x86_64 and arm64):

```bash
curl -fsSL https://shreypdev.github.io/undra/install.sh | sh         # checks the sha256, installs to ~/.undra/bin, no sudo
cargo install --git https://github.com/shreypdev/undra undra-cli     # from source (Rust 1.85+)
```

Not supported yet: Windows, and Alpine (musl) for the prebuilt binaries. Homebrew (`brew install undra`) comes
later, through homebrew-core ([what is not done](#what-is-not-done)).
`undra --version` prints `undra <version> (<commit>)` (`unknown` for a build from source). Maintainers: [docs/RELEASING.md](docs/RELEASING.md).

## Quick start

```bash
undra init myapp                              # a core, its bindings, and an app per platform
cd myapp/web && npm install && npm run dev    # the web app on its core: http://localhost:5173
```

Edit `core/src/lib.rs` and save: the Vite plugin rebuilds the core and reloads the page. `undra dev` (in `myapp`)
serves one core to every running app and keeps its state across a rebuild.

A new project depends on the release of the `undra` that created it, all from this repository (ADR-063): the crates
by its git tag, the Swift package at its root (`.package(url: "https://github.com/shreypdev/undra", from: "1.0.0")`),
the Kotlin runtime from JitPack (`com.github.shreypdev.undra:runtime:v1.0.0`) and `@undra/runtime` as the GitHub
Release's asset. No registry account, no token. To work on Undra itself, a project can use a checkout's crates and
runtimes by path instead:

```bash
git clone https://github.com/shreypdev/undra.git && cd undra
cargo install --path crates/undra-cli
undra init myapp --dir .. --undra-path .  # the project next to the checkout, using it
```

Then open `web/` (`npm install && npm run dev`), `ios/` (Xcode) or `android/` (Gradle).
Each shell is a plain native project wired to your core, and each builds the core itself (a Gradle
task, an Xcode build phase, a Vite plugin: there is no manual `undra build`). `undra build --platform
ios,android,web` is the explicit form: an XCFramework, 16 KB-aligned `.so`s and a `wasm-opt`'d module.
Building with Bazel? [`bazel/`](bazel/) has the rules (`undra_core`, `undra_bindings`, and the Swift, Kotlin and
TypeScript libraries over the bindings), hermetic and offline, and [`examples/bazel`](examples/bazel/README.md) a workspace
that `bazel test`s a core on the JVM, Node and (on macOS) Swift; the [guide](https://shreypdev.github.io/undra/docs/bazel.html) has the setup.
`undra doctor` tells you exactly what your machine is missing, with the command that fixes it
(`undra doctor --fix` prints them all). `undra init` also writes a CI workflow, and `undra upgrade`
moves a project to a newer Undra in one step.

Prefer to explore first? The [playground](examples/playground/README.md) is the same
thing, fully built, on all three platforms: todos, a counter, a 10,000-row keyed list, and remote
queries/mutations with an offline switch.

## What's in it

The core: records, enums, typed errors, **stores** (signals, computeds, keyed lists), **queries and
mutations** (staleness, dedup, retry with jitter, optimistic updates with surgical rollback, offline queue,
persistence), ten **ports** (Http, Kv, SecureStore, Fs, Clock, Rng, Log, Timer, Connectivity, Lifecycle) with
platform default adapters and deterministic Rust fakes, streams with backpressure, cancellation,
snapshot/restore, a schema-hash compatibility gate, `undra dev` with a live remote core, and teaching
diagnostics for every macro mistake (`error[undra::E0007]: …` with what/why/fix/docs).

And, each with its page:

* **React Native.** The same bindings and TypeScript mirror over a TurboModule on the C ABI, with the ten
  default adapters: [docs/REACT_NATIVE.md](docs/REACT_NATIVE.md).
* **Devtools with time travel.** A page served by `undra dev`: live stores, a change-set timeline you can scrub,
  port and query logs, behind a per-run token; state, and the query handles on screen, are kept across a rebuild:
  [docs/DEV_LOOP.md](docs/DEV_LOOP.md).
* **Derived lists.** A filtered or sorted view of a keyed list costs what changed (158 bytes, not 353 KB, for one
  edited row in 10,000): [docs](https://shreypdev.github.io/undra/docs/concepts.html#derived-lists).
* **WebSocket, SSE and Db ports.** Opt-in real-time streams and SQL over SQLite with deterministic fakes on every
  platform: [real time](https://shreypdev.github.io/undra/docs/realtime.html),
  [database](https://shreypdev.github.io/undra/docs/db.html).
* **Migrations.** A changed schema still restores what the last build stored, and storage ports fail with typed
  errors: [Shipping an update](https://shreypdev.github.io/undra/docs/updates.html).
* **A testing kit.** Previews that run your real core with scripted ports and a manual clock, and recorded sessions
  that replay in tests on Swift, Kotlin, TypeScript and Rust: [docs/TESTING.md](docs/TESTING.md).
* **iOS 15 and 16.** A lower deployment target generates `ObservableObject` stores; proven by compilation and a
  runtime probe on iOS 26.5, not yet on an iOS 15 or 16 runtime: [docs/IOS_15_16.md](docs/IOS_15_16.md).
* **Generics.** A generic record, enum, function, method, object or store crosses as the instantiations you list:
  overloads for functions, one ordinary class per alias for objects and stores. Declared, not open-ended; no native
  generic types: [Generic functions, objects and stores](https://shreypdev.github.io/undra/docs/generics.html).
* Also: objects and host callbacks across the boundary, newtypes and `Decimal`, paged and lazy lists,
  polling, panic reports with symbolication, background runs, several cores in one app, and a
  [cookbook](https://shreypdev.github.io/undra/docs/cookbook/) with a sample app.

## What is not done

Open, with the work done around it (the same list as the [roadmap](https://shreypdev.github.io/undra/roadmap/)):

* Benchmark rows from physical phones; today's device rows are a simulator, an emulator and headless Chromium.
* Maven Central and the npm registry beside GitHub (the launch ships everything from this repository's tags), then
  the crates on crates.io.
* Homebrew (`brew install undra`), through homebrew-core once the repository is 30 days old and notable (75 stars, or 30
  forks or watchers); there is no tap at launch.
* The `undra-compose`, `android-adapters` and `android-work` tests in CI (they pass locally and on the emulator);
  Android under Bazel, built and tested; a byte-reproducible `undra build`.
* A cancelled port call reaching the platform (today it is abandoned in the core); a Windows CLI; Dart and Flutter.

Undra is one team's work with no production users yet.

Not planned, by design: shared UI of any kind and hosted services; a sync engine, if it comes, is a separate
package (see [`docs/blueprint.html`](docs/blueprint.html)).

## Repository map

| Path | What |
|---|---|
| `crates/` | the 13 Rust crates: schema (`undra-meta`), wire codec, macros, signals, runtime, ports, query, testkit, ffi (the only `unsafe`), transport, bindgen, cli, facade |
| `runtimes/` | the Swift, Kotlin, TypeScript and React Native runtime packages the generated code sits on |
| `examples/` | `playground` (the reference app: one core, three platforms and React Native, proof screenshots), `cookbook`, `fieldbook` (a sample app), `two-cores`, `ios15-sample` |
| `contract-tests/` | the <!--trust:scenarios-->35<!--/trust--> scenarios + a runner per platform |
| `bench/` | criterion benches + the budget gate; `RESULTS.md` has the numbers |
| `docs/SPEC.md` | the binding specification (wire, ABI, runtime model, generated shapes) |
| `.10x/` | the project's decision record: ADRs, reviews, status ledger |

## For contributors

* **[docs/ONBOARDING.md](docs/ONBOARDING.md)**: machine setup and how to run every suite.
* **[docs/AGENT_WORKFLOW.md](docs/AGENT_WORKFLOW.md)**: the worktree-per-piece workflow for humans and AI
  agents alike (brief → implement → adversarial review → merge → clean up).
* **[CLAUDE.md](CLAUDE.md)**: the constitution, twelve non-negotiable rules (R1–R12)
  every change is held to.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
