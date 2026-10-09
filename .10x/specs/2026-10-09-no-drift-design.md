# Undra 1.2: no drift (design, 2026-10-09)

Status: proposed to the founder in the 1.2 pull request; this file is the record. The pieces were built in parallel
worktrees from the same integration branch, each with its own adversarial review, and land as one pull request with
the version, as 1.1 did.

## Goal

One Rust core already runs under SwiftUI, Compose, React and React Native, and every public type, store, method and
port is generated from one schema. What still drifts is what the schema cannot see: which calls each platform's UI
makes and in which order, what each platform's port adapters send and answer, and how a custom `#[undra::port]` is
implemented three times by hand. A team that built a trading app on 1.1 found all three in one week: a duplicate
`Accept` header and a wrong host that their replay never saw, and an iOS port that did not compile under Swift's
default main-actor isolation. 1.2 makes drift at the boundary visible and cheap to fail on, makes a custom port
compile on the first try on every platform, and halves the time the project's own merge gate takes.

## Non-goals (1.3 and later)

* A conformance file for the platform default adapters (URLSession, OkHttp, fetch), recorded against one server and
  diffed in CI, the way `testkit/conformance/fakes.json` holds the fakes together. The evidence is recorded in the
  roadmap: the five SSE adapters merge the core's headers differently and the JDK Http adapter drops restricted
  headers where Swift passes them. It needs a rule in the SPEC first (which header wins) and a scenario per column.
* A multi-client dev server (one `undra dev` serves one app at a time; a second server on another port records the
  second platform).
* Native Bazel mode, port cancellation reaching the platform, the Swift SSE backpressure redesign: unchanged from the
  1.1 record.
* Anything that names a domain.

## Pieces

| Slug | What | Acceptance |
|---|---|---|
| `drift` | `undra drift REFERENCE OTHER...` compares recordings of one user flow made on different platforms: the host's calls and their order, the port adapters' answers, the host's events, what was observed, the state each store ended in; arguments and values decoded through the schema; built-in ignores for what legitimately differs (clock and random readings, timer ids, idempotency keys); `--exit-code` for CI | The command, a golden report over fixtures with injected divergences, the playground core's own test recording two platforms and asserting the drift; `docs/TESTING.md`, `docs/DEV_LOOP.md` and the CLI reference |
| `host-ports` (ADR-066) | A custom port compiles on iOS under Swift's default main-actor isolation: bindgen emits a closure-based builder beside the protocol (`@Sendable` closures, no conformance) and the annotation that lets a plain conformer compile; the generated doc says what to import; a compile test with `-default-isolation MainActor` in CI; the playground's `Locale` port implemented in the new form on iOS; a cookbook recipe "Your own port, on three platforms" whose Swift, Kotlin and TypeScript snippets compile | The reproduction and the fix proven by the compile test; every committed Swift tree regenerated; the recipe and the ports page |
| `ci-wall-time` | The Gate in half the time with every job still run: the four longest jobs split into halves that run side by side, the Bazel jobs over a disk cache, macOS jobs queued first | The first Gate run on the branch is the measurement (`.10x/decisions/devops/ci-wall-time.md`) |
| `docs-1-2` | The landing page's drift section, the roadmap's 1.2 group and the adapter-conformance item under Next with its evidence, the architecture page's "where drift is caught" (and its crate table corrected to thirteen), the README, a blog post "Undra 1.2: no drift", `llms.txt`, the search index | `node site/scripts/build-all.mjs` changes nothing after the commit; `check-links` passes; no em-dash |

Then: `scripts/bump-version.sh 1.2.0`, the 1.2.0 migration notes, status checkpoint 41, the handoff.

## Sequencing

1. `ci-wall-time` first on the integration branch (it changes only workflow files and `ci-local.rb`).
2. `drift` and `host-ports` in parallel worktrees (`wt/drift`, `wt/host-ports`); disjoint files: the first owns
   `crates/undra-testkit`, `crates/undra-cli`, `docs/TESTING.md`, `docs/DEV_LOOP.md`, `site/docs/cli.html` and
   `site/docs/testing.html`; the second owns `crates/undra-bindgen`, every committed Swift tree, `examples/cookbook`,
   `examples/playground/ios`, `site/docs/ports.html`, the cookbook page and ADR-066.
3. An adversarial review of each, fixes on the piece's branch, then both merged into the integration branch.
4. `docs-1-2`, the version, the records.

## Method

Fable designs and integrates; Fable implements each piece from a brief that names the files, the acceptance line
above and the rules below; an adversarial review by a model that did not write the piece precedes every merge.

Rules every piece follows: no em-dash anywhere; no test depends on machine speed; `cargo fmt`, clippy clean, docs on
every `pub` item; the recording format, the wire, the ABI and the schema are unchanged (a recording of 1.1 reads in
1.2 and the reverse); nothing about where a requirement came from is written down beyond this file's first
paragraph: the records describe the work as Undra's own direction.

## Risks

* Decoding every wire kind: the dynamic decoder the runtime's persistence uses covers records, enums, options, lists,
  maps and the leaf types; handles, lazy values and patch ops fall back to hex with the type's name, and the report
  never fails on a value.
* Handle aliasing across sessions is by constructor and ordinal; a host that constructs stores in another order
  reads as drift, which is the point, and the report says which handle it could not align.
* A Swift annotation that makes a plain conformer compile under default main-actor isolation must not stop a 1.1
  conformer compiling under the default mode; the compile test runs both modes.
* The CI split is measured on the pull request's own Gate; a job that got slower is reverted, not loosened.
