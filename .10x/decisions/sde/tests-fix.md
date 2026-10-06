# tests-fix: the 1.1 tests-and-flakiness review's findings (2026-10-06)

On the 1.1 integration branch `wt/undra-1-1` (PR #40), test code and records only (no runtime change). The rule
throughout: no test depends on machine speed; a wait is a hang detector with a generous deadline, never a timing
budget; count events; wait on the exact condition; fix causes, never loosen a real assertion, never retry.

## H1: the flood stalls were timed (Swift)

`RealtimeReviewTests.testAnSseFloodStallsTheServerWhileTheCoreDoesNotPull` and its WebSocket twin slept 1 s, read the
server's `written` count, slept 0.5 s, read it again, and asserted `late - early <= 32` and `late < total / 4`. Under
load the server was still filling about 17 MB of socket buffers after a second (3 failures in 45 loaded runs: "the
server wrote 33702 of 100000 events to a stalled reader", "8118 is greater than 32"). The `total / 4` cap was a
number tied to buffer sizes. `URLSessionWebSocketAdapterTests.testAStalledReaderStopsTheServerThenGetsEverythingInOrder`
had the same shape (1 s sleep, `written < 200` of 64 KiB messages, a cap tied to the same buffers).

Fix: `RealtimeServer.waitForTheWriterToStall(path)` polls `/stats` until `written` is the same in ten answers in a row
(hang deadline 60 s). The server is one event loop and a flood's writer yields it only when a write was refused, so
each answer is a turn of the loop in which the writer had no room: the count of answers is what is measured, the
20 ms pause only paces them. The assertion is what the stall proves: the writer stopped before the end of the flood
(`stalled < total`). The WebSocket tests then drain every message in order and the read-ahead stays within the window
(`pumped`, a count); the SSE test reads until the server has written more than at the stall (the stall was the
reader's push-back, released by reading; a `HangDetector` closes the stream should the server never write again).
A reader that is only slow can look stalled; the count is then lower, and every assertion still holds.

## H2: the trickle trials were judged by what the test heard (Swift)

`testATrickleIsAnsweredByTheBurstCapNotHeldUntilItStops` failed 1 in 30 loaded runs ("1 is not greater than 1: the
trickle was one burst"). The valid-trial rule looked only at when the feeder pushed; the binding answers by its own
view, which is when its pump delivered each message, and a loaded machine can hold the pump task up for 2 ms after
it took a message while the feeder keeps its 0.5 ms cadence. The answer time `at` is read when the pull's task
resumes, later than the answer whenever that task was held up.

Fix (the "a stalled trial is not a trial" rule of S04): `ScriptedSocket` records when the pump took each message,
where it happens. Message k reaches the buffer between the pump taking it and taking the next one, so an answer of
`c` messages by the burst gap needs `min(at, taken[c + 1]) - taken[c - 1] >= burstGap`; a trial where that span (or
a feeder gap, as before) is 2 ms or more is not a trickle and is repeated (up to 40). `at` is used only as an upper
bound. The assertions on a valid trial are unchanged.

## H3: RN20 slept 1.5 s for the flood (on the device)

The check connected to `/ws/flood?n=6000`, slept 1.5 s, then read 16 at a time expecting `Closed(1008)`. The adapter
gives up only while more than 4,096 messages wait at once; on a loaded emulator fewer had reached JavaScript by then,
the reads kept the queue short, the server finished and closed 1000 ("end after 6000", run 37419177303).

Fix: the check reads nothing until the connection has ended by itself. For the `/ws/flood` connection it puts a
subclass in place of React Native's global `WebSocket` (the adapter looks the constructor up when it connects) that
counts the messages handed to JavaScript and sees `close` called on the socket (the adapter's give-up) and the
`close` event; the check waits (60 s hang deadline) for either, then reads and asserts the counts as before
(`Closed(1008)`, 4,096 to 4,128 kept). The global is restored as soon as the connection is made.

## H4: the swift-sse-wait record and the two ceilings it removed

The record claimed the remaining `Task.sleep`s "can only make pass, never fail" (false: H1, H2) and that the LazyList
read cost "is a benchmark in bench/" (false: no Swift or host LazyList read row exists). Both claims are corrected
in `swift-sse-wait.md`. The two removed ceilings are replaced by counts, not clocks:

* The lone message (`WebSocketBindingTests.testABurstIsOneAnswerAndALoneMessageIsNotHeldBack`): with fewer than `max`
  messages, no end and nothing after it, only the burst timer can answer the pull (the cap is checked when a message
  arrives), so the answer coming at all is the timer's, and the test now asserts its length (`burstGap == 2 ms`,
  `burstCap == 8 ms`): a gap raised to 2 s fails there. That the timer fires when armed is
  `RealtimeReviewTests.testALoneMessageIsAnsweredWithinAFewMillisecondsOfItsArrival` (against a reference timer).
* 100,000 cached `LazyList` reads (`testReadingCachedRowsIsCheap`), over a row type that counts its decodes: no call,
  no queued request (scheduled flushes), no decode (the 1,200 rows were decoded once, as their pages arrived), no cache
  change, no observation fired. A read that is slow without doing any of these (a scan of the cache) is not caught by
  a count; with no Swift row in `bench/`, that is left to review, and said so in the test.

## Low: fixed sleeps as synchronisation in the React Native unit tests

`test/support/wait.ts` holds `hangDeadline` (60 s, as Swift), `arrived` (`vi.waitFor`), `eventually`, and the test
timeout of a file that waits (vitest's 5 s default was a budget for the whole test, shorter than the old 10 s
`arrived`). Converted: `realtime.test.ts` (the two WebSocket floods wait for this side's close of the connection, the
two SSE limits for the end of the request; the old 3 s `eventually`), `objects-callbacks.test.ts` (the release, the
confirm answer, the callback's release), `defaults.test.ts` (the app's Connectivity report; the Lifecycle test's
first report; `until` at the hang deadline), `transport.test.ts` (waits until the posted drain ran, a new
`FakeNative.drainPosted`), `diagnostics.test.ts` (each `flush()` replaced by its effect, and a `vi.waitFor` that had
the 1 s default), `load.test.ts` (its 4 s loop), `lazy-list.test.ts` (the shared `arrived`). Negative waits stay
(`defaults.test.ts`'s "no Connectivity event" after 5 ms: a slow machine can only make it pass).

## Proof

Under load (one `yes > /dev/null` per core, 18, started and killed by the run scripts; the Android build and other
agents' work on the same Mac too; load average about 60):

* Swift: the whole target once, 920 tests, 3 skipped, 0 failures. The touched suites (`RealtimeReviewTests`,
  `WebSocketBindingTests`, `LazyListTests`, `URLSessionWebSocketAdapterTests`, `URLSessionWebSocketOnAppSessionTests`,
  `URLSessionSseAdapterTests`, `SseBindingTests`, `SseChunk*`, `SseSessionDelegateTests`, `RealtimeEndsReviewTests`)
  39 of 40. The one failure is in an untouched test and not a timing budget: `SseChunkWireTests
  .testABodyThatEndsIsEndedAfterItsEvents` waited its 300 s hang detector for the end of a body
  (`SseChunkTests.swift:781: XCTAssertEqual failed: ("nil") is not equal to ("Optional(the server ended the event
  stream)")`, `:782: the body's end was not seen`), a hang to look into separately.
* React Native unit tests as CI runs them against the production build (`UNDRA_TS_DIST`): 30 of 30; typecheck clean.
* RN20 on the shared emulator (`emulator-5554`, AVD `undra`, arm64-v8a, Release build of this branch):
  RN20 PASS in two runs, each "a stalled flood of 6000 (6000 reached JavaScript) kept 4112 then Closed(1008)". The
  second run passed `UNDRA-RN CHECKS 25/25`. The first passed 20 of the app's 21: RN15 (Connectivity) failed "online"
  because the emulator's Wi-Fi network came up during that run, which this change does not touch. The iOS simulator
  was not run.
* `bash scripts/check-no-em-dash.sh` clean.
