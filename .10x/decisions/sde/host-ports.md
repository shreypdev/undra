# host-ports: Swift ports under Xcode 26's default main-actor isolation (ADR-066)

Branch `wt/host-ports`, one pull request. Implements ADR-066 (Proposed): every requirement of a generated Swift port
protocol is `nonisolated`, and beside `<name>PortImpl(_:)` the generator always emits a builder from closures, so an app
under `SWIFT_DEFAULT_ACTOR_ISOLATION = MainActor` registers a port without conforming to anything.

## The reproduction

A scratch SwiftPM package (`swift-tools-version: 6.2`, Swift 6.3.3 / Xcode 26.6): the runtime by path, the `ports`
golden as the bindings module (compiled without the setting, as SwiftPM compiles a dependency), an `App` target with
`swiftSettings: [.defaultIsolation(MainActor.self)]` holding `final class MyClock: Clock` and `final class MyKv: Kv`.
In the order a team meets them: `cannot find type 'UndraCore' in scope` (the registration line: the conformance needs
only the bindings module, `UndraCore` / `PortImpl` / `Adapters` need `import UndraRuntime`); `stored property 'store'
of 'Sendable'-conforming class 'MyKv' is mutable` (a class that conforms to a port protocol, which refines `Sendable`,
is inferred nonisolated under the mode, not `@MainActor` like the rest of the file); `call to main actor-isolated
global function 'mainFn()' in a synchronous nonisolated context` and `main actor-isolated var 'offset' can not be
referenced from a nonisolated context` (a witness that reads main-actor state). A stateless conformer that reads
nothing on the main actor compiles under the mode as written. The generated file itself, compiled inside a module
under the mode, fails in the adapter (`call to main actor-isolated instance method 'nowMs()' in a synchronous
nonisolated context`, `main actor-isolated instance method 'flush()' cannot be called from outside of the actor`):
the protocol is inferred `@MainActor` there.

Measured fixes: `nonisolated` on the protocol (SE-0449, Swift 6.1) or on each requirement makes the in-module layout
compile and leaves a conformer as it was; `nonisolated final class` on a conformer is what is inferred anyway; closures
passed to `@Sendable` parameters are nonisolated under the mode, an async one reads main-actor state with
`await MainActor.run { .. }`, and a synchronous one that reads it directly is refused (`main actor-isolated var
'offset' can not be referenced from a nonisolated context`). One more Swift rule, measured on a local error type too:
a closure that throws is inferred as throwing `any Error` unless its clause names the type, so a method with a typed
error takes `{ req async throws(HttpError) in .. }` (`{ () throws(FsError) in .. }` without parameters).

## What

* `crates/undra-bindgen/src/swift.rs`: `nonisolated func` on every requirement (the per-requirement spelling because
  the generated package promises Swift 6.0); a five-line doc on the protocol (the core's thread, what a conformer
  therefore is, the builder's name, `import UndraRuntime`); the builder `<name>PortImpl(<method>: @escaping @Sendable
  (_ a: A, ..) [async] [throws(E)] -> R, ..) -> PortImpl`, one labelled parameter per method in declaration order,
  `-> Void` for unit, emitted by the same `port_method` through a `PortTarget` (`Protocol` calls
  `impl.<method>(label:)`, `Closures` calls `<method>(..)`; an entry's own locals are renamed when a method is called
  `args`, `r`, `result` or `error`, so the closure is never shadowed); the builder's doc says when to use it, that
  the closures run on the core's thread, how to read main-actor state, and, for a port with a typed error, the clause.
* Goldens regenerated (`UPDATE_GOLDEN_LANG=swift`): the six `Ports.swift` with request/response ports (`ports`,
  `full` x2, `newtypes` x2, `stdlib`). Every committed Swift tree regenerated with its project's own command:
  playground, cookbook, Fieldbook, the two-cores pair (`--docs`), the iOS 15 sample, and `examples/bazel/committed/`
  through `bazel run //:bindings_check.update`; Fieldbook and the sample have no such port and did not change.
* `crates/undra-bindgen/tests/typecheck_swift.rs`: a `Layout` (cases, checks, app files, app `swiftSettings`, tools
  version); `APPS` = the `ports` golden with `tests/fixtures/swift-isolation/ports-protocol.swift` (a plain class per
  port; `AppKv` keeps its dictionary behind an `NSLock` with `nonisolated(unsafe) var`) and `ports-closures.swift`
  (the three builders, `Adapters([PortImplAdapter(..)])`, `await MainActor.run` into a `@MainActor` session); both
  compile as the `GoldenPortsApp` target of every default build, and the new test
  `the_ports_golden_is_implementable_under_default_main_actor_isolation` compiles them in a second scratch package
  (`swift-tools-version: 6.2`, `.defaultIsolation(MainActor.self)` on the app target) when `swift --version` is 6.2
  or newer, printing why it skips otherwise. The golden's `HttpError` / `FsError` / `HttpRequest` are qualified
  `GoldenPorts.` in the fixtures because the runtime exports types of the same names (a golden-only clash).
* `examples/playground/ios/PlaygroundApp/UndraBootstrap.swift`: `Locale` registered from a closure through
  `PortImplAdapter` in the app's adapters (`Foundation.Locale` qualified: the generated protocol shares the name).
  No app registered the port before.
* `examples/cookbook/core/src/haptics.rs`: `Haptics { fn tap(&self, strength: u8) }` (sync) and `confirm(ctx)`, with a
  test on a recording fake; `lib.rs` row and exports; the cookbook bindings regenerated (`--docs`); the three snippets
  (`custom-port-swift` closure and class forms plus `Adapters.platformDefault.replacing(PortImplAdapter(..))`,
  `custom-port-kotlin` `object : Haptics` plus `registerPort`, `custom-port-ts` object literal plus `load({ ports })`).
* `crates/undra-bindgen/src/ts.rs` (a separate, marked commit): `ports.ts` imported `encodeValue` even for a port whose
  methods all return unit; `Haptics` was the first such port in a tree compiled with `noUnusedLocals`
  (`snippets/check.sh ts`: `TS6133: 'encodeValue' is declared but its value is never read`). The import is now
  requested by the entries that encode. No TypeScript golden changed.
* Docs: `site/docs/ports.html` "Writing your own port" over the real recipe (Rust, Swift both forms with the
  main-actor note and the import line, Kotlin, TypeScript; `ports.html` joins `build-cookbook.mjs`'s pages);
  `site/docs/cookbook/custom-port.html` in the recipe format, registered in `site/data/docs.json` and the cookbook
  index; `docs/SPEC.md` section 10 (the port lines of the shape listing and one paragraph); `node
  site/scripts/build-all.mjs` (nav on every docs page, `reference/swift.html`, search index, llms, sitemap).
* `.10x/adrs/ADR-066-host-ports-under-default-main-actor-isolation.md` (Proposed).

## Verified

* `cargo test -p undra-bindgen` (tsc from the runtime's `node_modules/.bin` on PATH): 217 passed, 0 failed, 13
  binaries; `UNDRA_REQUIRE_TOOLCHAINS=1 cargo test -p undra-bindgen --test typecheck_swift`: 5 passed (the three
  variants with the app target, the manifest test, the main-actor pass; Swift 6.3.3); `typecheck_ts` 23,
  `typecheck_kotlin` 1, `run_ts` 14 with toolchains required. `cargo clippy -p undra-bindgen --all-targets --
  -D warnings`, `cargo fmt`, `cargo doc` with `RUSTDOCFLAGS=-D warnings`: clean.
* `cargo test -p undra-cli --test bindgen_schema`: 11 passed. `undra bindgen --check --docs` on the playground, the
  cookbook, Fieldbook and the two-cores pair, `--check` on the iOS 15 sample: up to date.
* `cargo test -p cookbook`: 56 + 1 doctest. `bash examples/cookbook/snippets/check.sh swift kotlin ts`: ok.
* `undra build -C examples/playground --platform ios`, then `xcodebuild -quiet -project
  examples/playground/ios/PlaygroundApp.xcodeproj -scheme PlaygroundApp -configuration Debug -destination
  'generic/platform=iOS Simulator' build`: exit 0.
* `scripts/check-no-em-dash.sh`: clean. `node site/scripts/build-all.mjs`: run, output committed.

## Findings

* The brief's hypothesis (an implicitly `@MainActor` class whose synchronous witness cannot satisfy a nonisolated
  requirement) is not what the compiler does: the conformer is inferred nonisolated, and what fails is its state and
  its reads of main-actor state. The ADR records the measured errors; the generated doc and the site say the rule.
* `nonisolated` on a conformer buys nothing (it is inferred), and `nonisolated class` needs Swift 6.1 like
  `nonisolated protocol`, so neither the snippet nor the fixtures use it; the recipe explains the inference instead.
* A `@unchecked Sendable` helper class is still inferred `@MainActor` under the mode (its `init` became main-actor
  isolated, which a nonisolated conformer could not call); the fixture keeps the lock in the conformer itself.
* The `typecheck_kotlin` scratch directory is shared: running it concurrently with the full suite fails one of
  the two runs; alone it passes.
* `site/docs/cookbook/leaderboard.html`'s "Edit this page" link names `realtime.html` (pre-existing; not touched).

## Not done

* A Swift 6.0 compiler was not available (only Xcode 26): that `nonisolated` on a protocol requirement is accepted
  there rests on its being legal since Swift 5.5, not on a run.
* The main-actor pass runs only where CI's Swift is 6.2 or newer; on an older image it prints its skip and the
  fixtures still compile in the default build.
* No screen of the iOS app calls `localizedGreeting`, as none did before: the registration is what the app gains.

## Migration note

For `crates/undra-cli/src/migrations.rs`, the 1.2.0 entry (not edited here; the version bump adds it):

```rust
Note {
    kind: Kind::New,
    text: "Every generated Swift port has a second `<name>PortImpl`, from closures: one labelled `@Sendable` parameter per method (`clockPortImpl(nowMs:monotonicNs:log:)`), built on the same method table as `<name>PortImpl(_:)`. Under Xcode 26's default main-actor isolation a closure literal passed to it is nonisolated, so an app registers a port without conforming to anything; an async closure reads main-actor state with `await MainActor.run { .. }`, and a closure for a method with a typed error names it in its clause (`{ req async throws(HttpError) in .. }`). Registering needs `import UndraRuntime` (ADR-066).",
},
Note {
    kind: Kind::Changed,
    text: "Every requirement of a generated Swift port protocol is `nonisolated`, so the file also compiles in a module under default main-actor isolation. A class that conforms compiles as before: it is nonisolated and `Sendable`, keeps its state in `let`s or behind a lock, and reads main-actor state from an async method only (ADR-066).",
},
```
