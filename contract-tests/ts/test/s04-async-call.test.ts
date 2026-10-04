import { expect, test } from "vitest";
import { Counter, Probe, addLater } from "@playground/core";
import { boot } from "../src/harness.js";
import { WAIT_TIMEOUT_MS, step, waitFor } from "../src/wait.js";

// S04 async call: a call that waits on the core's Timer port resolves later, several at once
// resolve in the order their timers fire, and the platform may call again from inside a
// continuation or a change observer.

test("S04 async call", async () => {
  const { core } = await boot();

  await step("1. add_later(20, 22, 50) resolves to 42 after 45 ms", async () => {
    const started = performance.now();
    expect(await addLater(20, 22, 50, core)).toBe(42);
    const elapsedMs = performance.now() - started;
    expect(elapsedMs).toBeGreaterThanOrEqual(45);
    // The upper bound is WAIT_TIMEOUT_MS, a hang detector: a timer that never fires is what it catches. How long after 50 ms
    // the answer comes is the machine's (CI runners stall); step 2's order is the claim that the delays are honoured.
    expect(elapsedMs, "add_later(.., 50) within the 5 s wait").toBeLessThan(WAIT_TIMEOUT_MS);
  });

  // The claim: the core honours each call's delay and does not answer in the order the calls were made (the 400 ms call is
  // issued first). It is a claim about the core only on a trial the machine delivered: the three issued together, and no call
  // answered late enough to pass the next one. A call is put behind the next-slower one only by arriving late by the gap to
  // it, less the 20 ms the issues may be apart: 130 ms for the 50 ms call, 180 ms for the 200 ms call (the 400 ms call is
  // last: late is still last). A hosted runner has answered the 50 ms call 150 ms late, which puts the 200 ms call first and
  // says nothing about the core, so that trial is repeated; the values and the lower bound on each call's time hold on every
  // trial.
  await step("2. three concurrent calls resolve in delay order", async () => {
    const calls = [
      { id: 1, delayMs: 400, lateLimitMs: undefined },
      { id: 2, delayMs: 50, lateLimitMs: 130 },
      { id: 3, delayMs: 200, lateLimitMs: 180 },
    ];
    const measured: string[] = [];
    for (let trial = 0; trial < 30; trial++) {
      const timed = await Promise.all(
        calls.map(async ({ id, delayMs, lateLimitMs }) => {
          const issued = performance.now();
          const value = await addLater(id, 0, delayMs, core);
          const completed = performance.now();
          return { id, delayMs, lateLimitMs, value, issued, completed, late: completed - issued - delayMs };
        }),
      );
      for (const call of timed) {
        expect(call.value, `the value of addLater(${call.id}, 0, ${call.delayMs})`).toBe(call.id);
        expect(call.completed - call.issued, `addLater(${call.id}, 0, ${call.delayMs}) answered too early`).toBeGreaterThanOrEqual(call.delayMs - 5);
      }
      const issueSpreadMs = Math.max(...timed.map((c) => c.issued)) - Math.min(...timed.map((c) => c.issued));
      measured.push(`trial ${trial}: issued within ${issueSpreadMs.toFixed(1)} ms, late by ${timed.map((c) => `${c.id}: ${c.late.toFixed(1)} ms`).join(", ")}`);
      // The runner held a call up: its place in the order says nothing about the core.
      if (issueSpreadMs >= 20 || timed.some((c) => c.lateLimitMs !== undefined && c.late >= c.lateLimitMs)) continue;
      expect([...timed].sort((a, b) => a.completed - b.completed).map((c) => c.id)).toEqual([2, 3, 1]);
      return;
    }
    expect.fail(
      `the runner never delivered three concurrent calls within the margins (issued within 20 ms of each other, the 50 ms call answered under 130 ms late, the 200 ms call under 180 ms late) in 30 trials: ${measured.join("; ")}`,
    );
  });

  await step("3. Probe.wait(10) resolves to 10", async () => {
    const probe = await Probe.create(core);
    expect(await probe.wait(10)).toBe(10);
    probe.close();
  });

  await step("4a. a call made from another call's completion resolves", async () => {
    const second = await addLater(1, 2, 10, core).then((sum) => addLater(sum, 4, 10, core));
    expect(second).toBe(7);
  });

  await step("4b. a call made from inside a change observer resolves", async () => {
    const counter = await Counter.create(core);
    const followUps: Promise<number>[] = [];
    const stop = counter.count.subscribe((count) => {
      // The observer runs inside the runtime's flush; calling the core from here must not deadlock or be dropped.
      followUps.push(addLater(count, 41, 10, core));
    });
    await counter.increment();
    await waitFor("the observer's follow-up call", () => followUps.length === 1 && followUps[0]);
    expect(await followUps[0]).toBe(42);
    stop();
    counter.close();
  });
});
