# ADR-064: Committed bindings under Bazel, and the drift test that guards them

Status: **Accepted** (2026-10-05). Part of the 1.1 Bazel-first design (`.10x/specs/2026-10-05-bazel-first-design.md`).
Adds a public Bazel rule; touches no wire, ABI or schema.

## Context

ADR-061 made the bindings an output of the build under Bazel: `undra_bindings` generates the Swift, Kotlin and TypeScript
trees from the host library, nothing is committed, nothing can be stale. ADR-062 made the same point for the CLI path:
generated code is an artifact, reviewed through `undra schema diff`, with `undra bindgen --check` as the gate when a
project commits it.

Many Bazel repositories commit generated code by policy: it is indexed by the IDE, it is reviewable in a pull request, it
is linted by the same configuration as everything else, and a drift gate in CI proves it current. Under Bazel such a
repository has had no way to run that gate: `undra bindgen --check` is a CLI command, not a Bazel test, and the generated
trees are build outputs that never reach the source tree.

## Decision

1. **A second, equivalent model: committed bindings.** A project may commit the trees `undra_bindings` produces, at a
   path of its choosing, and keep using the generated outputs or the committed copy as it prefers. Both are the same
   bytes by construction.
2. **`undra_bindings_test(name, bindings, committed, ...)`**: a test target that fails when the committed trees differ
   from the schema's output, and a runnable `<name>.update` target that writes the outputs into the source tree. The
   failure message names the update command. It is built on `bazel_lib`'s `write_source_files`, already a dependency of
   the rules, so the diff, the update and the test are the ones every Bazel user knows.
3. **The comparison is `undra bindgen --check`'s.** The same files, the same normalisation: a file the CLI check ignores,
   the Bazel test ignores. One definition of "current" for both paths.
4. **The guide says when to choose which.** Generated outputs when the repository treats generated code as an
   artifact; committed with the test when its policy requires generated code in the tree. Neither is the default.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep bindings generated only (status quo) | Excludes every repository whose policy requires committed generated code; the policy is common and reasonable |
| A custom diff rule with a shell script | Reinvents `write_source_files` with fewer features and no updater; one more thing to maintain |
| Run the CLI's `--check` inside a `sh_test` against the source tree | Works, but the test's inputs would be the whole committed tree plus the CLI; `write_source_files` declares exactly the files it compares and gives the updater for free |

## Consequences

* Positive: a repository with a drift-gate policy can adopt Undra without an exception; the updater is one command.
* Negative: two models to document and keep equivalent. The test is the proof of equivalence and runs in the example.
* Risk: `write_source_files` compares file trees; if `undra bindgen --check` ever normalises more than tree equality, the
  test must follow it, which is why decision 3 ties them together.
