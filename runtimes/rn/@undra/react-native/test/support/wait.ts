// The waits of these tests: each one waits for a condition, never for a time.
import { vi } from "vitest";

/**
 * The deadline of every wait in these tests, in milliseconds. A wait is a hang detector, never a speed assertion: what it
 * waits for always comes, however busy the machine is, so the deadline is far longer than any machine needs and only a
 * genuine hang reaches it (a wait returns the moment its condition holds, so the length costs a passing test nothing). The
 * same 60 s as the Swift runtime's `hangDeadline`.
 */
export const hangDeadline = 60_000;

/**
 * The timeout of a test that waits: longer than a few waits in a row, so that a test is ended by the wait that hung, with
 * what it waited for, and not by vitest's default 5 s (a timing budget for the whole test). A file that waits calls
 * `vi.setConfig({ testTimeout })`.
 */
export const testTimeout = 5 * hangDeadline;

/** Waits until `check` holds (it throws until then, as an `expect` does). */
export const arrived = (check: () => void): Promise<void> => vi.waitFor(check, { timeout: hangDeadline, interval: 5 });

/** Waits until `condition()`; `what` names it in the failure. */
export async function eventually(condition: () => boolean, what = "the condition"): Promise<void> {
  const until = Date.now() + hangDeadline;
  while (!condition()) {
    if (Date.now() > until) throw new Error(`timed out after ${hangDeadline} ms waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}
