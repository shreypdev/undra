# rn-lazylist-flake: the React Native LazyList test waited a fixed 5 ms for a chain of two timers

Branch `wt/rn-lazylist-flake` (from main `58e373a`), one pull request, one test file.

## Cause

CI / React Native (host + model), step "Unit tests and contract scenarios against the production build", failed on a
documentation-only pull request:

```text
FAIL test/lazy-list.test.ts > a LazyList over the native transport > an op 2 from a core thread takes the length at once and re-pages only the window
AssertionError: expected [] to deeply equal [ { handle: 4294967303n, ...(2) } ]
```

The test sent an op 2 (`LazyInvalidated`) from a "core thread" and then waited `await tick(5)` (a 5 ms timer) before
asserting that the window was re-paged. The change-set does not reach the list in one hop but in two timers:

1. `FakeNative.queue(..., "core")` posts its drain with `setTimeout(drain, 0)`;
2. the drain hands the batch to the transport, which applies it through the mirror, whose `schedule` in this test is
   `setTimeout(fn, 0)`; only that second timer reaches `LazyList.applyInvalidated`, which queues the re-page for a
   microtask.

Node runs due timers in order of expiry. When the event loop is late by more than about 4 ms (a busy runner, a GC
pause), the drain timer (expiry t+1) and the 5 ms timer (expiry t+5) are both due in the same pass: the drain runs,
creates the mirror timer with a later expiry, and the 5 ms timer, already due, resumes the test first. The test
reads `server.calls` before the list has been told anything: `[]`.

The line `[undra::runtime] the native module failed: ... unknown native inbox record kind 99` in the log is noise: it
comes from `test/load.test.ts`, which injects that record on purpose, in another vitest worker. It is related only
as CPU contention on a runner with other workers.

## Fix

The test arms a reference beside the edit (`list.revision.peek()` before `server.edit`) and waits for the condition
the assertion is about: the revision moved, which happens when the re-paged window arrives
(`_install` bumps it). The wait is `vi.waitFor(..., { timeout: 10_000, interval: 5 })`, the helper shape
`transport.test.ts` already uses (`arrived`, from `fcbd3ae`); the 10 s deadline only turns a delivery that never
comes into a failure. No sleep is the synchronisation, no expectation was loosened, nothing retries.

The runtime itself is right (the page calls of one change go out in one microtask batch, so once the first re-page
is visible all of the window's calls are in `server.calls`), so there is no runtime change and no new unit test beyond
this one.

## Siblings audited (same file and the other React Native tests)

* The other four tests of `lazy-list.test.ts` wait on `tick()` after `list.get`, whose page request is a microtask and
  its reply synchronous (`callSync`): microtasks all run before the next timer, so those cannot be late.
* `transport.test.ts` already waits on conditions (`arrived`); its one `tick(5)` repeats an assertion that holds already.
* `objects-callbacks.test.ts`, `diagnostics.test.ts`, `defaults.test.ts`: their fixed waits follow a record queued from
  a core thread, but the record is a reply or a port call, which the transport handles inside the drain with no timer
  of its own (the only timer hop is the mirror's `schedule`, which a change-set takes). Their drain timer expires
  before their wait timer and runs first, so the pattern cannot fail by lateness; the checks that are about absence
  (`defaults.test.ts` first case) cannot be made conditions.
* `realtime.test.ts`: the fixed waits let a flood of real socket traffic accumulate against a stalled reader (the
  thing measured), and the ends are asserted on counts; not this pattern.

## Proof

* Reproduction: the failure does not show on a quiet machine (60 runs under eight busy loops passed), because the
  late loop has to land in a window of a few milliseconds. Forcing the lateness reproduces it every time with the
  exact CI message: a scratch copy of the test with a 10 ms (and 80 ms) synchronous block in a timer queued just
  before the edit fails before the fix (`expected [] to deeply equal [ { handle: 4294967303n, ...(2) } ]`) and
  passes with it, in both source mode and against the production build (`UNDRA_TS_DIST`).
* 200 consecutive runs of `test/lazy-list.test.ts` against the production build under eight busy CPU loops: see the
  pull request for the count.
* The whole React Native unit suite, `npm run typecheck`, and `scripts/check-no-em-dash.sh` pass.
