# Handoff - the v1.1 / v1.2 program is landing (2026-10-01)

The product is **Undra** (renamed from the working name Keel on 2026-09-30, ADR-030). `main` is
`shreypdev/undra`; the site is https://shreypdev.github.io/undra/.

## Where things stand (2026-10-01, evening)
Launch v2 is complete and live (status.md checkpoints 1–4). The v1.1/v1.2 program (the section
below) is landing piece by piece: checkpoints 5–6 are merged, the queue is listed there. The
founder-only release steps (`docs/RELEASING.md`; the dry run is green; corrected 2026-10-03: distribution is
ADR-063's, from GitHub, and there is no tap) are: reserve the npm scope `@undra` (nothing is published there); the
`release` environment with a `v*` restriction and a `v*` tag ruleset (recommended); the rehearsal tag
`v1.0.0-rc.1`; then `scripts/bump-version.sh 1.0.0` → tag `v1.0.0`. **The release needs no secret** beyond the
`GITHUB_TOKEN` GitHub gives every run: no npm token, no Homebrew tap, no tap token (ADR-063, amendment of 2026-10-03).

## How to run everything
`source scripts/env.sh`, then the commands in status.md's matrix table and `docs/ONBOARDING.md`;
`bash contract-tests/run-all.sh` for the scenario grid.

## v1.1 / v1.2 program (2026-10-01)
Spec: `.10x/specs/2026-10-01-v1x-default-choice-design.md` (tracks A–H; Amendments A–D: all six v1.2
bets approved; gap-audit and boundary ADRs 034–051 accepted in direction). Distribution stays parked
until after v2.

**Merged (checkpoints 5–6 in status.md):** gaps + competitive + ADR-039 (design), schema-json (C1, C2),
device-bench (E1; the honest web size is 135 KB gzipped, over budget - E5), diagnostics (D1), dev-loop
(B1, B2, ADR-051), parity (C3, C4: the Kotlin/TS error channel), react-native (G1, ADR-038; G1b default
adapters, E4 Hermes cost and a site page are open), android-adapters (M1 Maven publishing open), runtime-lifecycle (Track A: ADR-034/035/036 + the
ADR-019 amendment; Lows L2–L5/L8 open), docs-reference (H3), tooling (D2–D5), wasm-size (E5, ADR-052; `ts-runtime-size` owed), dev-reload (B3, ADR-053), rn-adapters (G1b), swift-fs, derived-lists (E2, ADR-039), abi-table (ADR-044; `ns-storage` owed), testkit (ADR-055), docs-v1x (cookbook + Fieldbook), ports (ADR-047/048), devtools (B4, ADR-054: sonnet review `.10x/reviews/2026-10-02-devtools-review.md`, 3 Medium fixed - the one 404, a silent panicking inspector, cache sampling under its lock; Rust 2,789 · contracts 60/60). Main moves with each checkpoint. CI pins Rust 1.98.1
(1.99.0 broke it on 2026-10-01; the bump is a deliberate piece: four workflow pins, `rustup update`,
`TRYBUILD=overwrite` goldens, bench re-baseline).

**Merge queue (cross-merge main on the branch, full matrix, fast-forward).** Each branch lives in
`/Users/shrey/Desktop/src/.work/<name>` with its record in `.10x/decisions/sde/<name>.md` there. State on
2026-10-02 when the usage budget ran low (5-hour cap, then the weekly cap; agents stop mid-step and resume
with their context when told to):
Landed: devtools, persistence, testkit, docs-v1x, ports (all reviewed) (ADR-037/049 Accepted; opus review `.10x/reviews/2026-10-02-persistence-review.md`: 3 High fixed - a wrong-typed migration hook spliced bytes, RN storage not on ADR-049, a dead web core after a trap during restart; hello wasm 116.8 KB, JS 25,984/26,000; Rust 2,891 · Swift 553 · Kotlin 652 · TS 1,264 · contracts 65/65; hash `0xfa536b9ac6f06149`). Still to land: testkit (review) then ports (review; it must cross persistence: `check.sh`, `run-all.sh`, `scenarios.md`, SPEC §8).

**Launch wave (2026-10-02): closed with `main` green and three branches open.** Rules now binding: (1) no
piece lands unless CI is green on its pushed head (`scripts/wt.sh merge` enforces it; it also demands a Site run on
the exact head when site paths changed - `gh workflow run site.yml --ref wt/<name>` when the filter skipped it);
(2) Fable designs where the difficulty is in deciding, a cheaper model implements, an adversarial review follows;
(3) **`main` is protected by the founder's ruleset (2026-10-03): a pull request, squash only, the required check
`All green` (the last job of the Gate workflow, which runs CI, Bench, Two cores and Site and needs all four), no bypass, no "up to date" rule and no merge queue.**
`scripts/wt.sh merge <slug>` opens the pull request, waits for that check, squash-merges, verifies `main`'s tree is
the tested head's tree and cleans up; `scripts/wt.sh pr <slug>` opens a draft early; `--ff` is only for repositories
without protection and the script tests. A branch must contain `main` or be stacked on the piece landing before it
(merging `main` into it changes nothing); build the whole stack before pushing. State commits ride in a piece's pull
request. Section 4 of `docs/AGENT_WORKFLOW.md` is the reference. Landed today, in order: `ci-green` (`fc326d6`), `diagram-rn`
(`b7efd61`), `generics-fn-obj` (`aa04821`), `generics-followups` (`3c279a6`). Main is `3c279a6` plus state commits.

Landed after that: `test-pacing` (`14a4689`), `reload-handles` (`7e5d238`), `ts-runtime-16k` (`784c361`) -
status checkpoints 32 and 33 - and then the feedback wave of checkpoint 34: the pull-request gate (#2, #6, #9),
`generated-weight` (#3), `sse-chunks` (#5), `okhttp-adapters` (#4), `bazel` (#7), `android-size` (#8). **Nothing is
open: `origin` has `main` only.** What each review left recorded is listed at the end of checkpoint 34; the two
findings not taken on are U2 (Xcode 27, needs the reporter's crash log) and U6 (GraphQL).

**After checkpoint 35 (status checkpoint 36):** the single Gate check (#16), no Homebrew tap (#17), no em-dash in any tracked file with a CI check (#18) and the launch polish (#19) are on `main` (`3797ae7`). Rules that came out of them: one required check, `All green`, the Gate's last job; no em-dash (U+2014) in anything tracked, in a commit message or in a pull request (a plain hyphen is fine); a script that lists files and rewrites them makes its whole plan first; a test that follows one handle waits for the other on its own condition.

**The release is verified (2026-10-04, status checkpoint 38).** After the founder removed the failed build on
jitpack.io, JitPack built `v1.0.0` and the *Release smoke* workflow passed for 1.0.0 on clean runners: the installer,
cargo, web, iOS and Android. Nothing about the Android channel is open any more; the paragraph below is how it got
there. What remains is his: step 2 of `docs/RELEASING.md` and the announcement.

**The release (2026-10-04, status checkpoint 37).** `v1.0.0-rc.1` and `v1.0.0` are tagged and published from
GitHub (the founder asked the integrator to carry out `docs/RELEASING.md` steps 3 to 8; this replaces "nothing is
tagged" and "an agent must not create tags" below, for that request only). The rehearsal passed on clean runners for
every channel. For `v1.0.0` the CLI, cargo and the web channel are verified; **JitPack's build of `v1.0.0` failed on
JitPack's side** (a broken Build-Tools download), so Android apps of `v1.0.0` do not build until the founder signs in at
jitpack.io, removes the failed build and requests it again (until 2026-10-11), and the iOS and Android checks of the
*Release smoke* workflow have not run for `v1.0.0`. After that: `gh workflow run release-smoke.yml -f version=1.0.0`.
Nothing is announced. Rules that came out of it: a version change is rehearsed as a prerelease first (it found three
defects 0.1.0 could not show); `scripts/bump-version.sh` owns every file that names the version, Bazel's included.

**Launch readiness (2026-10-03, status checkpoint 35).** The site for launch (#11), the posts' fact-check (#13), the web
size re-record (#12) and distribution from GitHub (#14, ADR-063) are on `main` (`da087cc`); `origin` has `main` only.
The live site already reads as on launch day: its installer and cargo commands become true with the release (there is no
Homebrew tap; `brew install undra` comes later through homebrew-core, roadmap Next). **Nothing is announced and nothing is
tagged.** The founder's steps are `docs/RELEASING.md`, in order; step 3, the `v1.0.0-rc.1` rehearsal tag, is required.
An agent must not create tags, releases, repositories or tokens, and must not be given a token (the release needs none
beyond GitHub's own). After the tag: check each channel from a
clean machine before any post. Follow-ups that are safe to take as small pull requests: the three CI flakes named in
checkpoint 35, the test count on the landing page, `schema.json` in the examples.
- **On hold by the founder:** the Android emulator CI job for `android-adapters`, `android-work` and
  `undra-compose`. Not started. Ask him before starting it. (The "Android emulator (API 34, x86_64)" job in
  `two-cores.yml` is older and unrelated.)
- `cold-restore-regression` (PR #1, reviewed, re-verified CLOSED): the two cold-start rows were 1.8x the machine
  baseline because the bench binary's schema grew 1.79x and `Runtime::new` hashes it; `undra-meta`'s `schema_json`
  writes the canonical and exchange JSON straight from the schema (same bytes, serde's serializer out of every core:
  hello web 116,224 -> 111,355 B gz), rows back at 1.18x / 1.10x, baseline not re-recorded, new row
  `snapshot/cold_start_schema_hash`. Open from its review: R1 (the closure collector and reader keep a `TypeRef`
  catch-all), and a native core hashing its schema twice per launch (`.10x/decisions/sde/cold-restore-regression.md`).
- Lessons that cost a CI cycle each today, for every brief: no absolute time bound in a test (measure against a
  reference armed beside the thing, or count events); wait for what is in flight to land before changing a fake;
  commit by path; never push while a run is in flight on the branch; stress loops must not leave `yes` burners
  (56 orphans once put the load average at 91); never `taskpolicy -b` under load; signal only your own PIDs
  (a `pgrep -f ci-local` kill took out two other agents' runs).

**Also owed:** a custom port in the playground for the reference's Ports section; `undra bindgen --declarations`.

**Next:** wave 0 of `.10x/specs/2026-10-01-boundary-surface-plan.md` (`abi-table` ADR-044 - after
Track A and RN merge, it rewrites the FFI they touch; `ios-floor` ADR-045 - after parity;
`newtypes` ADR-042 - after parity and Track A), `persistence-v2` (ADR-037 + A6/A7 of ADR-049),
`dev-reload` (B3), `testkit` (F1, F2), derived lists (ADR-039), E4 binding call path, then waves
1–3 and the v1.2 bets (B4 devtools, G2/G3 ports, G4 Dart), H1–H4 as the APIs settle.
