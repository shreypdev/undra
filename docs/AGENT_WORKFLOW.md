# The worktree workflow: how changes land on Undra

This is the working method that built v1, written down so the next contributor, human
or AI agent, follows the same loop. It exists because parallel work on one checkout
destroys itself, and because unreviewed merges destroy trust. One piece, one worktree,
one adversarial review, one merge, then clean up.

The helper for all of it is `scripts/wt.sh`. Worktrees live beside the repo in
`../.work/<slug>` on branch `wt/<slug>`; `main` stays in the primary checkout and only
the integrator writes to it.

## The loop

```text
  brief ──▶ implement in a worktree ──▶ adversarial review ──▶ fix ──▶ re-review
                                                                        │
  push the branch ──▶ pull request, "All green" on its head ──▶ merge ◀┘
                                                              │
                              clean up ◀── full matrix green on main
```

### 1. Open a worktree

```bash
scripts/wt.sh new my-piece        # prints ../.work/my-piece, branch wt/my-piece
```

One piece per worktree: a crate, a fix round, a feature. Never two concerns in one
branch. Never work directly on `main`.

### 2. Implement from a brief

Every piece starts from a written brief (for agents, it is the task prompt; for humans,
an issue). A good brief names, in this order:

1. The exact SPEC sections that bind the piece (`docs/SPEC.md §…`).
2. The real files to read first: the code the piece must fit, and the *consumers* that
   already exist (the three platform runtimes hard-code ids, layouts and semantics; when
   the SPEC and shipped consumers disagree, **the shipped consumers win**, and the SPEC
   gets a documented correction).
3. The quality bar: tests next to code + integration tests, docs on every `pub` item,
   `clippy -D warnings`, `cargo fmt`, wasm32 build where the crate is core, benchmarks
   when the boundary is touched (R4, R9).
4. What is OUT of scope, especially files other worktrees own right now. Two active
   worktrees must not touch the same crate; the integrator enforces this when cutting
   briefs.

Inside the worktree: small `type(scope): summary` commits as you go; never leave
`TODO`/`unimplemented!()`; end with `git status` clean. Write no em-dash (U+2014) anywhere, commit messages and
pull requests included: a colon, a comma, parentheses or two sentences say what it meant (a spaced hyphen is fine;
CLAUDE.md), and `scripts/check-no-em-dash.sh`, the first step of CI, fails on a tracked file that has one. **Run `scripts/ci-local.sh` before pushing a branch**: every
step of CI, Bench, Two cores and Site, in a clone of your commit, read from the workflow files (docs/ONBOARDING.md,
"Before you push a branch"); and `scripts/ci-local.sh --slow` when the piece adds or touches a test that waits, times or
races (it runs the timing-sensitive suites throttled, so the failures a slower runner would show are found here, once,
not one push at a time). Two rules for such code, each from a check that passed on a pull request and failed on `main`
for the same tree: make the whole list of what a step will touch before the step changes any of it (the version bump
built its list in a process beside the loop rewriting those files, and read one while it was empty); and wait for the
exact condition you assert, never for a neighbour of it (S32 waited for one handle to hold 100 rows and then read a
second handle, whose change-set the frame had not drained yet). Do not merge, rebase onto, or
push `main` from a worktree: the integrator merges. Pushing the piece's own `wt/<slug>` branch (which starts its CI
run, section 4) is the integrator's too, unless the brief says the author does it. Do not edit `.10x/status.md` or
`.10x/handoff.md` from a worktree (guaranteed conflicts); record your piece in
`.10x/decisions/<role>/<slug>.md` instead and the integrator folds it in.

If the piece changes the wire, the runtime model, the threading model or a generated
public shape: **write the ADR first** (`.10x/adrs/`, next free number: check for
parallel branches claiming the same number; the integrator renumbers collisions at
merge) and update the SPEC in the same commits (R11, R7).

### 3. Adversarial review before merge

No piece merges on its author's word. A reviewer who did not write the code attacks it:

* Findings must be **confirmed with a minimal repro** (built outside the repo, e.g. a
  scratch crate depending on the workspace by path) or clearly labeled THEORY with the
  exact code path. No speculative findings.
* Severity-ranked report to `.10x/reviews/YYYY-MM-DD-<piece>-review.md`.
* The author (or a fix round) closes every High/Medium with a regression test derived
  from the reviewer's repro, then the **same reviewer re-verifies** with their own
  repros and appends a per-finding CLOSED / NOT CLOSED verdict.
* For `undra-ffi` (the only unsafe crate): the review runs ASan and Miri; a fix to a
  safety finding is re-verified under the same tools.

For small pieces the review can be a focused pass by the integrator; for core crates it
is a full cycle. The four v1 cycles in `.10x/reviews/` are the reference for depth.

### 4. Merge: integrator only, through a pull request

**`main` is protected: nothing lands except through a pull request whose required checks are green on its exact
head.** The required check is **"All green"**, the last job of the Gate workflow (`.github/workflows/gate.yml`). On a
pull request the Gate runs CI, Bench, Two cores and Site as its own jobs (they are reusable workflows) and "All
green" needs all four, so it cannot report before they finish and is green only when every job of every one
succeeded. Each of the four ends in a job named "Complete" that fails on a failed, cancelled or skipped job. (They
used to end in four jobs all named "All green"; the ruleset names a check by its name, and GitHub took the first to
finish, Site's, as the required one: a pull request was mergeable while CI still ran. One name, one job.) A newer
push cancels the run it supersedes. Site always builds and checks on a pull request; its deploy stays `main`-only.
A piece is not done until "All green" is green on its head: an author whose run is red fixes the cause (not the
test) and pushes again.

The order is: the review is done, the branch contains `main` (`git merge main` in the worktree), the branch is
pushed, and `scripts/wt.sh merge <slug>` from the primary checkout does the rest: opens the pull request if none is
open (`scripts/wt.sh pr <slug>` opens a draft early, so CI runs from the first push), marks it ready, waits for the
checks, requires "All green" on the head (`scripts/wt-ci-check.sh pr-verdict`), squash-merges (the tree
that lands is the tree CI tested), verifies that `origin/main` contains the commit GitHub made, and cleans up
(section 5). `main`'s ruleset allows squash merges only and does not ask for "branches up to date", so the script keeps the
rule itself: the branch contains `main`, or merging `main` into it changes nothing (a piece stacked on the one
landing before it: `git merge origin/wt/<that>` before the final push, and build the whole stack before pushing;
two clean merges can still fail to compile together). A squash is a new commit, so what is verified after the
merge is the tree: `main`'s tree is the tested head's tree. The workflows also run on `merge_group`, for the day a
merge queue is turned on. State commits (`.10x/status.md`,
`.10x/handoff.md`) ride in the piece's pull request, pushed before its last run, or in a small pull request of
their own. `--ff` is the fast-forward path for a repository without branch protection and for the scratch
repositories of the tests; `--no-ci` goes with it only.

From the primary checkout, on `main`:

```bash
scripts/wt.sh pr my-piece       # optional, early: a draft pull request, so every push runs CI
scripts/wt.sh merge my-piece    # the pull request, its "All green" check, the merge, verify, clean up (section 5)
```

Then the integrator runs the **full matrix**, not just the touched crate:

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
(cd runtimes/ts/@undra/runtime && npm test)
runtimes/kotlin/undra-runtime/scripts/test-local.sh
(cd runtimes/swift/UndraRuntime && swift test)
bash crates/undra-ffi/tests/wasm/run.sh && bash crates/undra-ffi/tests/c/run.sh
bash contract-tests/run-all.sh
cargo test -p undra-bench --test budgets --release
```

Two rules of merge hygiene, both learned the hard way:

* **Cross-branch interactions are the integrator's findings.** Two branches green in
  isolation can break each other (a new background task landing inside another suite's
  timing window; a schema change invalidating another branch's snapshots). When the
  matrix fails after a merge, fix it on `main` with a commit that explains the
  interaction; never loosen a test without stating the precise new semantics.
* **`Cargo.lock` conflicts are never hand-merged**: take either side, then
  `cargo update --workspace -q` and commit the regenerated file.

After the matrix is green: update `.10x/status.md` (what landed, new totals, debts) and
`.10x/handoff.md`, commit `state(<piece>): …`.

### 5. Clean up: `merge` does it, and nothing is left lying around

`scripts/wt.sh merge <slug>` does not stop at the fast-forward. After the CI gate and the merge it

1. **pushes `main`** (`--no-push` merges locally and deletes nothing) and **verifies**: `wt/<slug>` is an ancestor of
   `main`, and, after `git fetch`, `origin/main` contains the head. If either fails it stops and deletes nothing;
2. **deletes the piece**: the remote branch (`git push origin --delete wt/<slug>`), the local branch, the worktree *with its
   build output* (`.work/<slug>`, `target/` and all), and the `scripts/ci-local.sh` clone of the branch;
3. **deletes the piece's helper branches**, local and remote: `proto/<slug>`, every `wt/<slug>-*` sub-piece, and each branch
   named with `--also <branch>`, **only when every commit of it is in `origin/main`**. Anything else is *kept*, and the
   output says which and why (`NOT merged (3 commit(s) that origin/main lacks)`, `its worktree has uncommitted changes`,
   `merged into the local main only`). A branch is never force-deleted on a guess;
4. **prints the final state**: `git worktree list`, the remaining `wt/*` branches local and on `origin`.

```bash
scripts/wt.sh merge my-piece                      # pull request, checks, merge, verify, clean up, list
scripts/wt.sh merge my-piece --also spike/probe   # a helper branch with another name, swept if merged
```

`scripts/wt.sh clean` is the same sweep for **every** `wt/*` branch, local or on `origin`, that is already merged into
`origin/main`: its worktree and build output, its branches, its `ci-local` clone. It keeps, and names, a branch with commits
`origin/main` lacks, a worktree with uncommitted changes, a worktree not on a `wt/*` branch, and a piece that was only just
created and has no commits of its own. Run it after a `--no-push` merge (once `main` is pushed), and whenever worktrees pile up:
stale ones hold gigabytes of `target/` each. `scripts/wt.sh rm <slug>` removes one worktree and its local branch when
fully merged. `scripts/wt-cleanup.test.sh` (a CI step) runs all of this against scratch repositories.

A worktree must never outlive its merge: the full matrix of section 4 runs from the primary checkout, so the worktree is
not needed for it.

## Parallelism rules

* Any number of worktrees may be active if their **crate sets are disjoint**. The
  integrator assigns ownership in each brief and is the only one who resolves overlap.
* Shared files nobody may touch from a worktree: `.10x/status.md`, `.10x/handoff.md`,
  root `Cargo.lock` (regenerate at merge), `.github/workflows/ci.yml` (one owner per
  round; put new jobs in a new workflow file if in doubt).
* The stash stack is shared across all worktrees. Never bare `git stash`; prefer a WIP
  commit; if you must stash, tag it (`git stash push -u -m "<slug>"`) and `apply` by
  SHA, never `pop`.

## Bringing a branch across the rename

The product was renamed to Undra (ADR-030) by `scripts/rename-keel-to-undra.sh`. A branch cut
before the rename crosses it mechanically, in the same order the rename branch did:

1. **Commit your work, and `git add` every new file.** The script only touches tracked files
   (it lists untracked ones that need it as a warning).
2. **`git merge main`.** For a conflict in a file you own, keep your side
   (`git checkout --ours -- <file>`); in any other file, take main's (`--theirs`). A file you
   added inside a directory the rename moved is placed at the new path by git
   (`CONFLICT (file location)`): `git add` it there.
3. **`scripts/rename-keel-to-undra.sh <your paths>`**: files or directories, relative to where
   you stand, in either spelling (`site/blog`, `crates/undra-foo`). It `git mv`s every path
   whose name carries the old name (deepest first) and rewrites the content of every tracked
   text file under the paths; running it again changes nothing. Without paths it renames the
   whole tree except `site/` and the launch-v2 planning records; naming a path lifts those two
   exclusions, never the immutable history (`.10x/reviews/`, ADR-018 to ADR-030).
4. **Regenerate, never hand-edit:** lockfiles (`cargo build`; `npm install --package-lock-only`
   in each package), the goldens (`UPDATE_GOLDEN=1 cargo test -p undra-bindgen --test golden`
   and `-p undra-cli --test bindgen_schema`; `UPDATE_SNAPSHOTS=1 cargo test -p undra-macros
   --lib`; `TRYBUILD=overwrite cargo test -p undra-macros --test compile_fail`; the message
   goldens of the diagnostics catalogue (`UPDATE_GOLDEN=1 cargo test -p undra-bindgen --test
   diagnostics`, `-p undra-ports --test ports_runtime`, `-p undra-cli --test diagnostics`) and
   the error-codes page after them (`node site/scripts/build-errors.mjs`);
   `undra bindgen -C examples/playground --docs`), then `cargo fmt`. The regenerated output
   differs from the script's only in import order (the new name sorts later than the old one did),
   line wrapping and caret underlines; read the diff to confirm nothing else
   moved.
5. **What a text rename cannot see** fails a test, and the fix is to recompute the
   expectation, not to loosen it: fixed-width text (a padded table), hashes of names
   (`fnv1a64` known-answer vectors, file names derived from a key), and sort order. The four
   envelope magic bytes are wire format and did not change in the rename; ADR-033 changed them
   afterwards to `55 4E 44 52` (`UNDR`). A branch that holds an envelope frame of its own
   (a test fixture, a vector) from before ADR-033 takes the new magic: every decoder rejects
   the old one with the typed bad-magic error. A test that needs a wrong magic spells it as
   bytes.
6. **Re-run your suites.**

## For AI agents specifically

* Your world is exactly the worktree path in your brief. Do not touch any other
  checkout. Do not merge or push. Report back: branch, what you built, test totals,
  deviations from the brief **with reasons**, and open questions; the integrator reads
  the report before the diff.
* If a defect in already-merged code blocks you, do not silently work around it:
  minimal-repro it, flag it prominently in your report, and work around it loudly in a
  clearly-marked commit if you must proceed.
* Cost discipline: implementation runs on a cheaper model from a precise brief;
  adversarial review and integration decisions run on the strongest model. The brief
  carries the architecture so the implementer doesn't have to re-derive it.
