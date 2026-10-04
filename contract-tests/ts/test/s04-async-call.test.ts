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
  // issued first). It is a claim about the core only on a trial the machine delivered: all three issued close together, none
  // answered long after its delay. A runner that stalls the 50 ms call for 150 ms puts the 200 ms call first, and the order
  // then says nothing, so that trial is repeated; the values and the lower bound on each call's time hold on every trial.
  await step("2. three concurrent calls resolve in delay order", async () => {
    const calls = [
      [1, 400],
      [2, 50],
      [3, 200],
    ] as const;
    const measured: string[] = [];
    for (let trial = 0; trial < 10; trial++) {
      const timed = await Promise.all(
        calls.map(async ([id, delayMs]) => {
          const issued = performance.now();
          const value = await addLater(id, 0, delayMs, core);
          const completed = performance.now();
          return { id, delayMs, value, issued, completed };
        }),
      );
      for (const call of timed) {
        expect(call.value, `the value of addLater(${call.id}, 0, ${call.delayMs})`).toBe(call.id);
        expect(call.completed - call.issued, `addLater(${call.id}, 0, ${call.delayMs}) answered too early`).toBeGreaterThanOrEqual(call.delayMs - 5);
      }
      const issueSpreadMs = Math.max(...timed.map((c) => c.issued)) - Math.min(...timed.map((c) => c.issued));
      const latestMs = Math.max(...timed.map((c) => c.completed - c.issued - c.delayMs));
      measured.push(`trial ${trial}: issued within ${issueSpreadMs.toFixed(1)} ms, answered up to ${latestMs.toFixed(1)} ms after the delay`);
      if (issueSpreadMs >= 50 || latestMs >= 100) continue; // the runner held a call up: its place in the order says nothing about the core
      expect([...timed].sort((a, b) => a.completed - b.completed).map((c) => c.id)).toEqual([2, 3, 1]);
      return;
    }
    expect.fail(
      `the runner never delivered three concurrent calls within the margins (issued within 50 ms of each other, none answered 100 ms or more after its delay) in 10 trials: ${measured.join("; ")}`,
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
