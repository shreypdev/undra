# CTO: Bazel-first integration (1.1), 2026-10-05

**Decision:** 1.1 is the step after ADR-061: Undra fits an existing Bazel monorepo (managed toolchains, committed
generated code with a drift gate, strict linting, a current Bazel, rulesets at the versions a repository already has).
The Cargo and CLI path stays primary and unchanged; every piece is additive.

**Why now:** the Bazel rules are the adoption path for the largest native app teams, and they are the part of 1.0 that
still reaches outside the build (the machine's NDK and `cargo-ndk`), pins a single Bazel version, and declares the newest
rulesets. Each of those is a reason for a platform team to say no before trying the product.

**Not now:** native Bazel mode (the core as `rules_rust` targets, no subprocess), which is 1.2 after its own ADR; a GraphQL
layer (recorded separately, waits for a team that needs it); anything domain-specific in the examples.

**Version:** 1.1.0, a minor release: new rule, new test target, new CI jobs, docs; no wire or API change.
