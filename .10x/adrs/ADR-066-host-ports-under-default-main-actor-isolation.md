# ADR-066: Host ports under default main-actor isolation: nonisolated requirements and a builder from closures

Status: **Proposed** (2026-10-09). Part of the 1.2 no-drift design (`.10x/specs/2026-10-09-no-drift-design.md`).
Changes a generated public Swift shape (`Ports.swift`: the port protocols and a second `<name>PortImpl` beside the
adapter). Nothing on the wire, in the ABI or in the schema changes; the Kotlin and TypeScript shapes are untouched.

## Context

A team built an iOS app on Undra 1.1 with a port of its own (`#[undra::port] trait Credentials`). Their class
conforming to the generated protocol did not compile under Xcode 26's default main-actor isolation
(`SWIFT_DEFAULT_ACTOR_ISOLATION = MainActor`, `-default-isolation MainActor`, SE-0466: what Xcode 26 sets for a new
project), and their file needed an `import UndraRuntime` nobody had told them about. They asked for "a generated
helper for host ports".

Reproduced with Swift 6.3.3 (Xcode 26.6) in a scratch package: the runtime by path, the `ports` golden as the
bindings package (compiled as SwiftPM compiles it, without the setting), and an app target with
`swiftSettings: [.defaultIsolation(MainActor.self)]` holding a plain `final class MyClock: Clock` (sync) and a
plain `final class MyKv: Kv` (async). The exact errors, in the order a team meets them:

* `error: cannot find type 'UndraCore' in scope`: the registration line, before any isolation question. The
  conformance itself needs only the bindings module; `UndraCore`, `PortImpl` and `Adapters` need `import UndraRuntime`.
* `error: stored property 'store' of 'Sendable'-conforming class 'MyKv' is mutable`: a class that conforms to a
  generated port protocol (which refines `Sendable`) is inferred **nonisolated** under the mode, not `@MainActor`
  as the rest of the file is, so its `var`s are refused the way they are under plain strict concurrency.
* `error: call to main actor-isolated global function 'mainFn()' in a synchronous nonisolated context`, and for
  a property `error: main actor-isolated var 'offset' can not be referenced from a nonisolated context`: a witness
  that reads what the rest of the app keeps on the main actor.
* When the generated file is compiled inside a module that has the setting (an app that drops the generated
  sources into its own target rather than depending on the package): `error: call to main actor-isolated instance
  method 'nowMs()' in a synchronous nonisolated context` in the adapter's `@Sendable` closures, and `error: main
  actor-isolated instance method 'flush()' cannot be called from outside of the actor`. The protocol is inferred
  `@MainActor` there, and the adapter cannot call it from the core's thread.

So a stateless conformer that touches nothing on the main actor compiles under the mode as written. What breaks is
state (a `var`), a read of main-actor state in a synchronous method, and the generated file itself in a module
under the mode. A `@MainActor` protocol cannot be the answer: the core calls a port from its own thread, a
synchronous method has nothing to hop with, and the core may hold its lock while it waits for the reply.

What makes a plain conformer compile under the mode, measured:

* `nonisolated` on the protocol declaration (SE-0449) or on each requirement: either makes the generated file
  compile when it is itself under the mode, and a nonisolated caller can then call the requirements. Neither
  changes a conformer compiled against the package: it was nonisolated already. `nonisolated protocol` needs
  Swift 6.1; the generated package promises Swift 6.0 (SPEC section 10.1, `swift-tools-version: 6.0`).
* `nonisolated final class` on the conformer: what the compiler infers anyway; it buys nothing.
* Closures passed to `@Sendable` parameters: nonisolated whatever the file's default, so an app registers a port
  without conforming to anything, and an async closure reads main-actor state with `await MainActor.run { .. }`.
  A synchronous closure that reads it directly is refused with a clear diagnostic. One Swift rule to know: a
  closure that throws is inferred as throwing `any Error` unless its clause names the type, so a method with a
  typed error takes `{ req async throws(HttpError) in .. }` (measured on a local error type too: it is not the
  golden's name clash).

## Decision

1. **Every requirement of a generated port protocol is `nonisolated`.** `public protocol Http: UndraPort,
   Sendable { nonisolated func request(..) async throws(HttpError) -> HttpResponse }`. The generated file compiles
   wherever an app compiles it, under the mode or not, and the adapter keeps calling the host from the core's
   thread. The per-requirement spelling is accepted by every Swift 6 compiler; SE-0449's `nonisolated protocol`
   would need 6.1. A 1.1 conformer compiles unchanged: the requirements were nonisolated before.
2. **A builder from closures beside the adapter, always.** For every request/response port, `undra bindgen` emits
   `public func <name>PortImpl(<method>: @escaping @Sendable (_ a: A, ..) [async] [throws(E)] -> R, ..) ->
   PortImpl`: one labelled parameter per method, in declaration order, each the method's signature as a
   `@Sendable` function type (`-> Void` for a unit return; `throws(E)` with typed throws, `throws` without), built
   on the same `.sync([..])` / `.async([..])` table code as `<name>PortImpl(_ impl: any <Name>)`. Nothing conforms,
   and the closures run on the core's thread.
3. **The generated doc comments teach the rule.** The protocol's says that the core calls it from its own thread,
   that a conforming class is therefore nonisolated and `Sendable` (no `var` without a lock, no main-actor read in a
   synchronous method), names the builder, and says that registering needs `import UndraRuntime`. The builder's
   says when to use it, that the closures run on the core's thread, how to read main-actor state from an async
   one, and, for a port with a typed error, the closure clause that names it.
4. **The proof runs in CI.** `typecheck_swift` compiles two files against the `ports` golden as part of every
   default build: a plain class per port (the protocol form, state behind a lock) and the closure form of all
   three. From Swift 6.2 a second pass compiles the same two files in an app target with
   `.defaultIsolation(MainActor.self)`; an older compiler prints why it skips.
5. **The reference app and the recipe use it.** The playground's iOS app registers its `Locale` port from a
   closure, through `PortImplAdapter` in its adapters. The cookbook gains `Haptics`, a sync port of the app's own,
   with a recipe on three platforms whose code is what `cargo test -p cookbook` and `snippets/check.sh` compile.

## Alternatives considered

* **A `@MainActor` protocol.** The natural fit for an app under the mode, and wrong for a port: the core calls
  from its own thread, often with its lock held, and a synchronous requirement cannot wait for the main thread
  without the two blocking on each other. Async requirements could hop, at a frame per call, and the sync ones
  (`Clock`, `Log`, the app's own) could not.
* **A hopping adapter.** `<name>PortImpl` could take a `@MainActor` implementation and hop on each call. For sync
  ports that is `DispatchQueue.main.sync` from a thread the main thread may be waiting on: a deadlock the core
  cannot see. For async ports it costs a hop per call and hides where the code runs, which is the thing the
  team needed to know.
* **Stubs written by `undra bindgen`.** A `MyCredentials.swift` the app edits. A file that is both generated and
  edited drifts, and 1.2 is the release that removes drift; a regeneration either overwrites the app's code or
  stops being able to.
* **Documentation only.** It would have left the generated file breaking in a module under the mode, and the
  team asked for a helper, not a page.

## Consequences

* `Ports.swift` grows: per request/response port, five doc lines on the protocol, and a builder about the size
  of the adapter. Every committed Swift tree changes (the goldens, the playground, the cookbook, Fieldbook, the
  two-cores pair, the Bazel example's `committed/`; the iOS 15 sample and Fieldbook only where they have such a
  port). No runtime change.
* `nonisolated` on a requirement of a protocol that is not actor-isolated is redundant for a compiler that is
  not in the mode; it is the modifier that keeps the file right in one that is. Its acceptance by a Swift 6.0
  compiler was not verified locally (only Xcode 26 is installed); the modifier has been legal on requirements
  since Swift 5.5.
* A conformer still cannot hold a `var` without a lock and still cannot read main-actor state synchronously: that
  is Swift's rule for a nonisolated `Sendable` class, and the generated doc now says so instead of letting the
  compiler say it first.
* A port registered from closures is not a type: there is nothing to replace by type in `Adapters`; it goes in
  as `PortImplAdapter(portId:impl:)`, which is how the playground registers `Locale`.
* SPEC section 10 documents both shapes; the 1.2.0 migration note names the builder and the modifier.
