# CTO decisions - index

Cross-cutting principles:

* Undra wins by being *the* honest option: every number links to a benchmark, every
  claim about a competitor must survive that competitor's maintainers reading it.
* Distribution is a launch feature, not an afterthought: one release workflow feeds every
  channel (brew, npm, curl, cargo-from-git).
* Names are ours on every registry we use (ADR-030); we never build on a name we cannot
  claim.

Active features:

* [launch-v2](launch-v2.md) - the rename to Undra, distribution channels, what the site
  must win.
* [v1x-default-choice](v1x-default-choice.md) - the v1.1 / v1.2 program: no reason to say no (2026-10-01).
* [competitive-limitations](competitive-limitations.md) - the five plan changes from the sourced competitor catalogue: Android adapters, query completeness, boundary surface, production operability, ABI/iOS-floor decisions now (2026-10-01).
* [bazel-first](bazel-first.md) - 1.1: Undra fits an existing Bazel monorepo; Cargo stays primary; native mode is 1.2 (2026-10-05).
