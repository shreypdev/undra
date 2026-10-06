# sse-end-race: URLSession lost the end of a short event stream (suspend, resume, suspend as the body ends)

Branch `wt/sse-end-race` (from `wt/undra-1-1` at `e2a2c3f`), one commit, Swift runtime only (no wire, ABI or generated-shape
change, no ADR).

## Symptom

Under a load average of about 60, `SseChunkWireTests.testABodyThatEndsIsEndedAfterItsEvents` failed once in 40 suite runs:
the three events arrived, `SseError.ended` never did, and the 300 s `HangDetector` closed the stream.

## Cause

`URLSessionSseStream.receive` suspended the data task before parsing every chunk and resumed it after (unless the queue had
reached `room`). The test's body arrives in two chunks (the 8-byte opening comment, then the 45 bytes of three events), so
every run made suspend, resume, suspend, resume within about 100 microseconds, while the server's FIN arrived. When CFNetwork
read the end of the body inside that window, it never finished the task: no `didCompleteWithError`, no
`didFinishCollecting metrics`, `task.state` `.running` with all 53 bytes received, for as long as anyone waited. The
adapter's state was correct throughout (no lost wakeup: `finish` was never called, so `end` was never set; `head` was nil;
the pull was parked with `waiter` set, as it should be).

A failing run, traced (microseconds since the test began, `t` the thread; the server's side is the test server's):

```text
684us   server sent 104B (head and opening)
749us   didReceive response
766us   server sent 45B (three events)
769us   server shutdown(SHUT_WR) result=0
780us   receive 8B chunk#1: suspend() ... parsed 0 events, resume
818us   server: client left            (CFNetwork read the FIN and closed the socket)
902us   resume() returned
947us   receive 45B chunk#2: suspend() ... parsed 3 events, resume, wake the pull
981us   park: waiter set
20104038us close (hang detector); task.state=0 received=53 expected=-1 error=nil
```

The observable order of these steps is the same in thousands of passing runs; the race is inside CFNetwork. What pins it
on the suspend and resume calls (a loop of the test's scenario, run under 18 `yes` loops, load average 50 to 65; "hung" means
no end within 3 to 20 s):

| Variant (scratch build, instrumented)                                          | Hung / runs |
| ------------------------------------------------------------------------------ | ----------- |
| The adapter as it was (room 16)                                                | 22 / 28,944 |
| No suspend or resume at all                                                    | 0 / 22,000  |
| Only the first chunk suspended and resumed                                     | 0 / 12,000  |
| Only the second chunk suspended and resumed                                    | 0 / 12,000  |
| Room 2, pull 1 ms late (the second chunk stays suspended until the pull)       | 90 / 3,473  |
| Room 2, pull 1 ms late, only the second chunk suspended                        | 0 / 3,000   |
| Room 2, pull 1 ms late, only the first chunk suspended                         | 0 / 3,000   |
| Room 2, two events fill the queue, three more and the end wait in the socket; a pull resumes, the next chunk suspends (run with a first cut of the fix, which still suspended these chunks) | 0 / 3,000 |

So a single suspension, short or long, does not lose the end, and a resume by the core's pull followed by a suspend did not
either; a suspend and resume around one chunk followed at once by a suspend for the next, with the end of the body arriving
meanwhile, does. Once lost, the end stays lost: a further `suspend()` and `resume()` on the hung task (a probe in 6 hung runs)
brought nothing in 4 s, and in the hung runs the metrics callback, which precedes the completion in every passing run, never
came either, so there is no other signal of the end to watch for. The test server is not involved: `send` wrote every byte,
`shutdown` returned 0 on the live client fd, and the client closed its end (CFNetwork had read the FIN).

## Fix

The task is suspended before a chunk is parsed only when the chunk may take the queue to the mark:
`waiting + SseParser.mostEvents(in: chunk, upTo: room) >= room`. `mostEvents` counts line ends (an event is dispatched only
at a line end; the first one a chunk completes may need one, every later one at least two), stopping once the count is
enough to reach the mark. A chunk that cannot reach the mark is parsed with the task running, as it would have been resumed
right after anyway. Nothing else in the backpressure design changes: the resume rules, the close and cancel paths and the
suspend that a filling chunk gets are as they were.

Against the documented read-ahead bound: unchanged. The task is still suspended before parsing every chunk that could take
`waiting` to `room`, so the queue is still at most `room` plus one chunk's events. What changes is below the mark only: a
chunk that cannot fill the queue used to be parsed with the socket paused, and now URLSession may read on while it is
parsed; what it reads is the next chunk, which gets the same check. The line-end scan runs before the suspend decision; it
stops early on a chunk dense with events and is a single byte comparison per byte otherwise (no copy, no UTF-8 check), so
it is much cheaper than the parse it decides about.

Tests (`SseChunkTests.swift`): `SseParserBoundTests` (a seeded run of 3,000 bodies made of every kind of line, cut anywhere,
never completes more events than the bound, with any limit; the densest bytes reach it), and in `SseChunkBackpressureTests`
the wire test's short stream never suspends the task, a chunk that cannot reach the mark is parsed with the task running,
and one that may reach it is still suspended and resumed. The three fail on the old adapter with
`[suspend, resume, suspend, resume]`. `testAChunkThatLeavesRoomResumesTheTaskAtOnce` became the last two (its old
expectation was the old rule).

## Proof

Same load (18 `yes` loops, load average 55 to 65):

* The scenario loop against the clean fix: 0 hung in 12,000 (room 16; 22 in 28,944 before) and 0 in 3,000 on the room-2
  reproducer (90 in 3,473 before).
* `swift test --filter "SseChunk|SseParserBound"` (the four suites of the file, 26 tests): 60 runs, 0 failures.
  Before the fix the wire suite alone passed 60 of 60 under the same load: at about one hang per 1,300 ends, 60 suite runs
  (three ends each) seldom show the bug, which is why the loop is the evidence.
* The whole Swift target once, under the same load: 924 tests, 3 skipped, 0 failures.

## Not proven, still open

* The mechanism inside CFNetwork is inferred from the experiments above, not read from its source; the observations are on
  macOS 26 (Darwin 25.5), arm64, an ephemeral session, HTTP/1.1 on loopback.
* The fix removes the pattern for a stream whose chunks cannot fill the queue; it does not make suspend and resume safe. A
  chunk that may reach the mark (by line ends) and does not, followed at once by another suspend as the body ends, is still
  exposed: built on purpose (room 2, a chunk of three comment lines just before the events and the end, the pull 1 ms late),
  it hung 2 times in 2,000. A real stream gets there only when its consumer is at the mark or a chunk's line ends overstate
  its events by enough (comments, multi-line `data`, CR LF) while the server ends the stream.
* No change to the Kotlin or web adapters (they do not use URLSession).
