import { ChangeOp, Mirror, encodeChangeSet } from "@undra/runtime";
import { afterEach, describe, expect, test, vi } from "vitest";
import { STATS_INTERVAL_MS, StatsChannel, type StatsMessage, startStatsPoster, timerResolutionUs, toStatsMessage, toStressMessage } from "./embed-stats";
import type { StressSnapshot } from "./stress-stats";

/** A clock the test moves by hand, in milliseconds. */
function fakeClock(start = 0): { now: () => number; advance: (ms: number) => void } {
  let t = start;
  return {
    now: () => t,
    advance: (ms) => {
      t += ms;
    },
  };
}

/** A snapshot with every number at a recognisable value. */
const SNAPSHOT: StressSnapshot = {
  generatedPerSec: 10_000.04,
  receivedPerSec: 10_020.66,
  entriesReceivedPerSec: 10_020.66,
  appliedPerSec: 124.26,
  mergeRatio: 0.012_401_234,
  drainsPerSec: 59.96,
  drainP50Us: 300.000_000_000_001_4,
  drainP99Us: 449.96,
  drains: 120,
  drainTotalUs: 36_000.04,
  drainMaxUs: 500.000_000_000_001,
  nsPerChangeSet: 1795.6,
  droppedFrames: 3,
  droppedFramesRecent: 1,
  longestFrameMs: 50.04,
  frameMs: 16.67,
  heapMb: 61.449,
  timerResolutionUs: 99.999_999_999,
  generatedTotal: 40_000,
  receivedTotal: 40_080,
  appliedTotal: 248,
  compactions: 0,
  applies: { value: 120, progress: 40_000 },
};

/** A window of drains with the numbers the base message reads. */
const windowOf = (w: { changeSetsPerSec: number; p50Us: number; p99Us: number; drains?: number; totalMs?: number; maxUs?: number }) => ({
  drains: 0,
  totalMs: 0,
  maxUs: 0,
  ...w,
});

describe("toStatsMessage", () => {
  test("types the message and rounds to a tenth", () => {
    expect(
      toStatsMessage(windowOf({ changeSetsPerSec: 9.987654, p50Us: 100.00000000000142, p99Us: 249.96, drains: 120, totalMs: 0.30000000000000004, maxUs: 200.00000000000003 }), 99.99999999999),
    ).toEqual({
      type: "undra-stats",
      changeSetsPerSec: 10,
      applyP50Us: 100,
      applyP99Us: 250,
      timerResolutionUs: 100,
      applyBatchDrains: 120,
      applyBatchUs: 300,
      applyMaxUs: 200,
    });
    expect(toStatsMessage(windowOf({ changeSetsPerSec: 0, p50Us: 0, p99Us: 0 }), 0)).toEqual({
      type: "undra-stats",
      changeSetsPerSec: 0,
      applyP50Us: 0,
      applyP99Us: 0,
      timerResolutionUs: 0,
      applyBatchDrains: 0,
      applyBatchUs: 0,
      applyMaxUs: 0,
    });
  });

  test("the base message keeps the four fields a consumer of the first version reads, and adds the batch", () => {
    expect(Object.keys(toStatsMessage(windowOf({ changeSetsPerSec: 1, p50Us: 2, p99Us: 3 }), 4)).sort()).toEqual(
      ["applyP50Us", "applyP99Us", "changeSetsPerSec", "timerResolutionUs", "type", "applyBatchDrains", "applyBatchUs", "applyMaxUs"].sort(),
    );
  });

  test("a batch resolves an average drain far below the clock's step", () => {
    // 1,000 drains of 2.3 us each, read by a clock with a 100 us step and a random phase: most read 0, about one in
    // forty reads one step. One reading says nothing; the batch's sum says what they took together.
    let seed = 7;
    const random = () => ((seed = (seed * 16_807) % 2_147_483_647) / 2_147_483_647);
    const stepMs = 0.1;
    const read = (t: number) => Math.floor(t / stepMs) * stepMs;
    let totalMs = 0;
    let maxUs = 0;
    for (let i = 0; i < 1_000; i++) {
      const start = random() * 1_000;
      const took = read(start + 0.0023) - read(start);
      totalMs += took;
      maxUs = Math.max(maxUs, took * 1000);
    }
    const message = toStatsMessage(windowOf({ changeSetsPerSec: 60, p50Us: 0, p99Us: 100, drains: 1_000, totalMs, maxUs }), 100);
    expect(message.applyP50Us).toBe(0);
    expect(message.applyBatchUs / message.applyBatchDrains).toBeGreaterThan(1.3);
    expect(message.applyBatchUs / message.applyBatchDrains).toBeLessThan(3.3);
    expect(message.applyMaxUs).toBe(100);
  });
});

describe("toStressMessage", () => {
  const context = { mode: "firehose", targetRate: 10_000, running: true } as const;

  test("keeps the base fields and adds the stress ones, from one snapshot", () => {
    const message = toStressMessage(SNAPSHOT, context);
    expect(message).toEqual({
      type: "undra-stats",
      // The base shape.
      changeSetsPerSec: 10_020.7,
      applyP50Us: 300,
      applyP99Us: 450,
      timerResolutionUs: 100,
      applyBatchDrains: 120,
      applyBatchUs: 36_000,
      applyMaxUs: 500,
      // The stress fields.
      generatedPerSec: 10_000,
      entriesReceivedPerSec: 10_020.7,
      entriesAppliedPerSec: 124.3,
      mergeRatio: 0.0124,
      drainsPerSec: 60,
      applyNsPerChangeSet: 1796,
      droppedFrames: 3,
      droppedFramesRecent: 1,
      longestFrameMs: 50,
      heapMb: 61.4,
      valueApplies: 120,
      progressApplies: 40_000,
      mode: "firehose",
      targetRate: 10_000,
      running: true,
      runtime: "wasm-main",
    });
  });

  test("the base fields are the same numbers the tiles show: received, and the per-drain percentiles", () => {
    const message = toStressMessage(SNAPSHOT, context);
    expect(message.changeSetsPerSec).toBeCloseTo(SNAPSHOT.receivedPerSec, 1);
    expect(message.applyP50Us).toBeCloseTo(SNAPSHOT.drainP50Us, 1);
    expect(message.applyP99Us).toBeCloseTo(SNAPSHOT.drainP99Us, 1);
  });

  test("outside Chrome there is no heap field, rather than a made-up one", () => {
    const message = toStressMessage({ ...SNAPSHOT, heapMb: null }, context);
    expect("heapMb" in message).toBe(false);
  });

  test("the generator's settings travel with it, and a missing applies count is 0", () => {
    const message: StatsMessage = toStressMessage({ ...SNAPSHOT, applies: {} }, { mode: "progress", targetRate: 100_000, running: false });
    expect(message).toMatchObject({ mode: "progress", targetRate: 100_000, running: false, valueApplies: 0, progressApplies: 0 });
  });

  test("survives a structured clone, which is what postMessage does", () => {
    const message = toStressMessage(SNAPSHOT, context);
    expect(structuredClone(message)).toEqual(message);
  });
});

describe("timerResolutionUs", () => {
  /** A clock that advances `tickMs` per reading and shows only multiples of `stepMs`, like a clamped performance.now. */
  function clamped(stepMs: number, tickMs: number): () => number {
    let t = 0;
    return () => {
      t += tickMs;
      return Math.floor(t / stepMs) * stepMs;
    };
  }

  test("finds the step of a clamped clock", () => {
    expect(timerResolutionUs(clamped(0.1, 0.001))).toBeCloseTo(100, 6);
    expect(timerResolutionUs(clamped(1, 0.001))).toBeCloseTo(1000, 6);
    expect(timerResolutionUs(clamped(0.005, 0.0001))).toBeCloseTo(5, 6);
  });

  test("takes the smallest step it sees", () => {
    const readings = [0, 0, 0.3, 0.3, 0.4, 0.4, 0.4, 0.9];
    let i = 0;
    expect(timerResolutionUs(() => readings[Math.min(i++, readings.length - 1)] as number)).toBeCloseTo(100, 6);
  });

  test("a frozen clock has no step to find, and the spin ends", () => {
    expect(timerResolutionUs(() => 5)).toBe(0);
  });

  test("the real clock gives a positive, sane step", () => {
    const step = timerResolutionUs();
    expect(step).toBeGreaterThan(0);
    expect(step).toBeLessThan(5000);
  });
});

describe("StatsChannel", () => {
  const message = toStatsMessage(windowOf({ changeSetsPerSec: 1, p50Us: 2, p99Us: 3 }), 4);

  test("posts to the parent, addressed to the origin it was given", () => {
    const posted: Array<{ message: unknown; origin: string }> = [];
    const channel = new StatsChannel({ postMessage: (m, origin) => posted.push({ message: m, origin }) }, "https://example.test");
    channel.post(message);
    expect(posted).toEqual([{ message, origin: "https://example.test" }]);
  });

  test("with no origin it is addressed to *", () => {
    const origins: string[] = [];
    new StatsChannel({ postMessage: (_m, origin) => origins.push(origin) }).post(message);
    expect(origins).toEqual(["*"]);
  });

  test("claims nest and release once each", () => {
    const channel = new StatsChannel({ postMessage: () => {} });
    expect(channel.claimed).toBe(false);
    const first = channel.claim();
    const second = channel.claim();
    expect(channel.claimed).toBe(true);
    first();
    first(); // twice is harmless
    expect(channel.claimed).toBe(true);
    second();
    expect(channel.claimed).toBe(false);
  });
});

describe("startStatsPoster", () => {
  const STORE = 9n;

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  const entry = (i: number): Uint8Array =>
    encodeChangeSet({ txnId: BigInt(i), entries: [{ handle: STORE, signalId: 0, op: ChangeOp.FullValue, value: new Uint8Array([i & 0xff]) }] });

  test("posts an undra-stats message to the parent every 500 ms, zeros while idle", () => {
    vi.useFakeTimers();
    const clock = fakeClock();
    const mirror = new Mirror({ schedule: () => {} });
    const posted: Array<{ message: unknown; origin: string }> = [];
    const channel = new StatsChannel({ postMessage: (message, origin) => posted.push({ message, origin }) });
    const stop = startStatsPoster(mirror, channel, clock.now);
    expect(STATS_INTERVAL_MS).toBe(500);

    vi.advanceTimersByTime(499);
    expect(posted).toEqual([]);
    vi.advanceTimersByTime(1);
    // The fake clock never moves by itself, so no timer step can be observed.
    expect(posted).toEqual([{ message: { type: "undra-stats", changeSetsPerSec: 0, applyP50Us: 0, applyP99Us: 0, timerResolutionUs: 0, applyBatchDrains: 0, applyBatchUs: 0, applyMaxUs: 0 }, origin: "*" }]);

    vi.advanceTimersByTime(1000);
    expect(posted).toHaveLength(3);

    stop();
    vi.advanceTimersByTime(5000);
    expect(posted).toHaveLength(3);
  });

  test("the messages are addressed to the channel's origin and nowhere else", () => {
    vi.useFakeTimers();
    const mirror = new Mirror({ schedule: () => {} });
    const origins: string[] = [];
    const stop = startStatsPoster(mirror, new StatsChannel({ postMessage: (_m, origin) => origins.push(origin) }, "https://example.test"), fakeClock().now);
    vi.advanceTimersByTime(500);
    expect(origins).toEqual(["https://example.test"]);
    stop();
  });

  test("the messages carry the drains the runtime reported: rate and per-drain percentiles", () => {
    vi.useFakeTimers();
    // The mirror times a drain with the global performance.now(); each apply takes 0.2 ms of it.
    const mirrorClock = fakeClock(50);
    vi.spyOn(performance, "now").mockImplementation(mirrorClock.now);
    const scheduled: Array<() => void> = [];
    const mirror = new Mirror({ schedule: (fn) => scheduled.push(fn) });
    mirror.register(STORE, () => {
      mirrorClock.advance(0.2);
    });
    const posted: unknown[] = [];
    const stop = startStatsPoster(mirror, new StatsChannel({ postMessage: (message) => posted.push(message) }), mirrorClock.now);

    // Five change-sets, one drain each, 100 ms apart, 0.2 ms per drain.
    for (let i = 0; i < 5; i++) {
      mirrorClock.advance(100);
      mirror.enqueue(entry(i));
      (scheduled.shift() as () => void)();
    }
    vi.advanceTimersByTime(500);
    expect(posted).toHaveLength(1);
    const message = posted[0] as StatsMessage;
    expect(message.type).toBe("undra-stats");
    expect(message.applyP50Us).toBeCloseTo(200, 6);
    expect(message.applyP99Us).toBeCloseTo(200, 6);
    // The clock advanced 5 * 100.2 ms = 501 ms; 5 change-sets in that span is about 10 a second.
    expect(message.changeSetsPerSec).toBeGreaterThan(9.9);
    expect(message.changeSetsPerSec).toBeLessThan(10.1);
    stop();
  });

  test("under a burst the percentile is per drain, not per change-set", () => {
    vi.useFakeTimers();
    const mirrorClock = fakeClock(0);
    vi.spyOn(performance, "now").mockImplementation(mirrorClock.now);
    const scheduled: Array<() => void> = [];
    const mirror = new Mirror({ schedule: (fn) => scheduled.push(fn) });
    mirror.register(STORE, () => {
      mirrorClock.advance(0.4);
    });
    const posted: unknown[] = [];
    const stop = startStatsPoster(mirror, new StatsChannel({ postMessage: (message) => posted.push(message) }), mirrorClock.now);
    mirrorClock.advance(10);
    for (let i = 0; i < 1000; i++) mirror.enqueue(entry(i));
    (scheduled.shift() as () => void)();
    vi.advanceTimersByTime(500);
    const message = posted[0] as StatsMessage;
    // One drain held the thread for 0.4 ms, however many change-sets it merged.
    expect(message.applyP99Us).toBeCloseTo(400, 6);
    stop();
  });

  test("stays quiet while a screen has claimed the channel, and resumes when it lets go", () => {
    vi.useFakeTimers();
    const mirror = new Mirror({ schedule: () => {} });
    const posted: unknown[] = [];
    const channel = new StatsChannel({ postMessage: (message) => posted.push(message) });
    const stop = startStatsPoster(mirror, channel, fakeClock().now);
    const release = channel.claim();
    vi.advanceTimersByTime(2000);
    expect(posted).toEqual([]);
    release();
    vi.advanceTimersByTime(500);
    expect(posted).toHaveLength(1);
    stop();
  });

  test("stopping removes the drain listener", () => {
    vi.useFakeTimers();
    const mirror = new Mirror({ schedule: () => {} });
    let listeners = 0;
    const add = mirror.addDrainListener.bind(mirror);
    mirror.addDrainListener = (listener) => {
      listeners++;
      const remove = add(listener);
      return () => {
        listeners--;
        remove();
      };
    };
    const stop = startStatsPoster(mirror, new StatsChannel({ postMessage: () => {} }), fakeClock().now);
    expect(listeners).toBe(1);
    stop();
    expect(listeners).toBe(0);
  });
});
