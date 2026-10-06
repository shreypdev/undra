# swift-sse-wait: the Swift tests' waits were speed assertions (5 s per step, 30 s per test)

Branch `wt/swift-sse-wait` (from main `ead291e`), one pull request, test files only (no runtime change).

## Cause

CI / "Swift runtime + C ABI (macOS)", step "Swift tests", failed on a documentation-only pull request:

```text
SseChunkTests.swift:629: error: SseChunkWireTests testEveryByteOfTheBodyArrivingAsItsOwnChunkParsesAsTheWhole : failed - timed out waiting ...
Test Case ... failed (8.424 seconds)
```

The test sends the awkward body one byte at a time through a real `URLSession` and a loopback socket, and after each byte
waits `eventually("byte N of the body") { stream.received.bytes == sent }`. `eventually` had a hard 5 s deadline. A socket
round trip per byte on a loaded macOS runner can stall for longer than that once in a while, so the deadline was a speed
assertion: the byte always arrives, the test only failed when the machine was slow. Three more things had the same shape:

* The test's `HangDetector` ran 30 s for the whole body (about 150 round trips), so it measured the length of the test, not a
  hang, and it could end the wait before an individual wait ran out. After a failed `eventually` the loop went on and failed
  once per remaining byte.
* The other waits of the target had their own short deadlines: `waitUntil` 3 s (about 30 call sites) and 5 s, the private
  `eventually` of `RemoteReconnectTests`, `waitFor` of `ObjectsCallbacksTests`, the polling loops of `AppSessionTests`,
  `RealtimeAdapterTests` and `PortsV2ReviewTests`, the `wait(for:)`, `fulfillment(of:)` and `DispatchGroup`/semaphore waits of 5 to 30 s,
  and a 3 s `Task.sleep` in `PortsV2ReviewTests` that cancelled 12 `open` calls that had not finished by then.
* Four upper-bound elapsed-time assertions: a lone realtime message answered under 1 s, the server seeing a close under 1 s, and
  100,000 cached `LazyList` reads under 5 s.

Every call site of the shared `eventually` and `waitUntil` was checked: all are positive waits ("until X happens"). The
`Task.sleep`s that remain were taken to be negative waits (assert that something did not happen after a pause), which a slow
machine can only make pass, or a stimulus the test then waits on by condition. Correction (`tests-fix.md`, 2026-10-06): that was
not true of all of them. Three were positive waits in disguise, a fixed second assumed to be long enough for a flood's server to
stall (`RealtimeReviewTests`, the WebSocket and SSE floods, and `URLSessionWebSocketAdapterTests`' stalled reader), and failed under
load; and the trickle test judged its trials by when the test heard of the answer, not by what the binding saw.

## Fix

* One constant, `hangDeadline` = 60 s, in `FakeTransport.swift`; every wait above uses it. A passing wait returns the moment
  its condition holds (polling every 5 ms, 2 ms for `waitUntil`), so a fast machine stays fast; only a genuine hang waits 60 s.
* `eventually` returns `Bool` (`@discardableResult`): `false` after it failed the test. The byte loop stops at the first
  byte that does not arrive instead of waiting again for each of the rest.
* `HangDetector` defaults to 5 x `hangDeadline` (300 s) and has `kick()`, which restarts its countdown. The byte test kicks it
  after every byte, so it measures the time since the last step, never the length of the whole body. It still ends a real hang
  (closes the stream, so `reading.value` returns and the assertions fail instead of hanging the suite).
* The four elapsed-time ceilings are gone; what they stood beside is still asserted (the answer arrives, the server sees the
  close, no read of a cached row asks the core for anything). Correction (`tests-fix.md`, 2026-10-06): this record said the read
  cost budget is a benchmark in `bench/`; it is not, no row of `bench/` measures the Swift `LazyList` read path, and two of the
  removed ceilings had left a property unproven. `tests-fix.md` puts it back without a clock: the lone message's answer is the
  burst timer's, whose length is asserted, and a cached read is counted (no call, no queued request, no decode, no cache change,
  no observation fired).

* Found by the load run: `SseSessionDelegateTests.testATaskLevelDelegateAnswersTheStreamsTrustAndAuthentication` asserted the exact
  challenge list `[trust, basic]`, but whether the authenticated retry reuses the connection (or handshakes again, a second trust
  challenge) is URLSession's choice; it failed 1 run in 50 with `[trust, basic, trust]`. It now asserts the first challenge is trust,
  basic occurs once and nothing else is asked.

The test still proves what it proved: every byte arrives, each as a chunk of its own, the events parse as the whole, the stream ends.

## Proof

Under load (one `yes > /dev/null` per core, started and killed by the run script; other agents building on the same Mac as well):

* `SseChunkWireTests` (6 tests) 50 of 50 passes.
* The touched suites (`SseChunk*`, `WebSocketBindingTests`, `RemoteReconnectTests`, `AppSessionTests`, `SseSessionDelegateTests`,
  `SseBindingTests`, `DiagnosticsTests`, `CallErrorTests`) 49 of 50, the one failure being the delegate test above; after its fix,
  `SseSessionDelegateTests` plus `SseChunkWireTests` 50 of 50.
* The whole Swift target once: 920 tests, 3 skipped, 0 failures.
* `bash scripts/check-no-em-dash.sh` clean.
