# CTO: no drift (1.2), 2026-10-09

**Decision:** 1.2 closes the ways three platforms still disagree after the schema has removed most of them: a command
that compares one flow recorded on each platform at the boundary (`undra drift`), a custom port that compiles on iOS on
the first try (ADR-066), and the project's own merge gate in half the time with every job still run. The wire, the ABI,
the schema and the recording format are unchanged; everything is additive.

**Why now:** the first week of a real team on 1.1 produced three findings, all outside the schema (a duplicate header
one platform sent, a wrong host on one platform, a port that did not compile under Swift's default isolation). Each is
the kind of finding that makes a platform team conclude that "one core" did not buy agreement. The answer has to be a
tool and a shape, not a doc.

**Not now:** adapter conformance across the platform default adapters (the evidence is recorded; it needs a rule in the
SPEC first); a multi-client dev server; native Bazel mode; port cancellation; the Swift SSE backpressure redesign.

**Version:** 1.2.0, a minor release: a new command, a new generated Swift builder beside an unchanged protocol, a
cookbook recipe, CI structure; no wire or API break.
