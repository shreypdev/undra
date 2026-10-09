# Architect: no drift (1.2), 2026-10-09

ADR: ADR-066 (host ports under Swift's default main-actor isolation: a closure form beside the generated protocol, the
annotation that lets a plain conformer compile, a compile test). `undra drift` needs no ADR by the ADR-062 precedent:
it is a tool over an existing format, and the format, the wire, the ABI and the schema are unchanged.

**Boundaries.** The recording format (`undra.recording`, version 1) is read as it is; nothing is added to it, so the
four readers and the byte-locked fixtures stay as they are, and a recording of 1.1 reads in 1.2. The comparison lives
in `undra-testkit` (`drift`, `decode`), reachable as `undra::testing`, so the three kits can grow the same comparison
later from one reference. The decoder is the runtime's own dynamic value model (`undra_runtime::persist`), driven by
the schema's type closures; a value kind outside it falls back to hex with the type's name, and the report never
fails on a value.

**What is compared, and what is not.** The host's calls and their order (target, method, arguments after the ignore
rules), the port adapters' answers per port and method, the host-pushed events, the set of observed signals and the
state each store ended in. Not compared: `t`, host call ids, the core's port-call ids, txn ids, timer ids; handles are
aligned by (constructor type, ordinal of its reply) across sessions. Built-in ignores, named in the report's header:
Clock and Rng replies, Timer arguments, the Idempotency-Key header's value (found in the decoded request, so with a
schema), and each entry's `t`. A `Timestamp` or `Uuid` field is compared like any other value; `--ignore` names one.

**The Swift shape.** The core calls a port from its own thread, so a `@MainActor` protocol is not an option. The
reproduction (Xcode 26.6) corrected the hypothesis: under default main-actor isolation a class conforming to the
generated protocol (which refines `Sendable`) is inferred nonisolated, so a stateless conformer compiles as written;
what fails is mutable state in a `Sendable` class, a read of main-actor state from the witness, and the generated
adapter when its file is compiled inside a module built in that mode (the mode made the un-annotated requirements
main-actor isolated). So every requirement is `nonisolated` (per requirement, not `nonisolated protocol`, which needs
Swift 6.1 where the generated package promises 6.0), and the closure form is the answer that needs no conformance at
all: a `@Sendable` closure is nonisolated in every mode, an async one reaches main-actor state through
`MainActor.run`, and a sync one that tries is refused with a clear diagnostic. A 1.1 conformer keeps compiling under
the default mode. The compile test runs both forms in both modes.

**Failure modes watched.** A host that constructs stores in another order reads as drift (the point; the report names
the handle it could not align). Two recordings of different schema hashes are refused with both hashes. A devtools
page attached during a recording changes the core's port-call pattern (the hub observes every store); documented, not
detected. The CI split is measured on the release's own Gate; a job that got slower is reverted, not loosened.
