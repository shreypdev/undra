import { ChangeOp, Mirror, encodeChangeSet } from "@undra/runtime";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  DROP_THRESHOLD,
  DrainWindow,
  FrameMonitor,
  type FrameSource,
  type MirrorCounters,
  type MirrorLike,
  RateWindow,
  StressMeter,
  browserHeapBytes,
  formatCount,
  formatDuration,
  formatMerge,
  formatStep,
  median,
  percentile,
} from "./stress-stats";

/** A clock the test moves by hand, in milliseconds. */
function fakeClock(start = 0): { now: () => number; advance: (ms: number) => void; set: (ms: number) => void } {
  let t = start;
  return {
    now: () => t,
    advance: (ms) => {
      t += ms;
    },
    set: (ms) => {
      t = ms;
    },
  };
}

const drain = (durationMs: number, changeSets = 1, appliedEntries = 1, entries = changeSets) => ({ changeSets, entries, appliedEntries, durationMs });

describe("percentile", () => {
  test("is nearest-rank over a sorted list", () => {
    const ten = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    expect(percentile(ten, 0.5)).toBe(50);
    expect(percentile(ten, 0.9)).toBe(90);
    expect(percentile(ten, 0.99)).toBe(100);
    expect(percentile(ten, 0)).toBe(10);
    expect(percentile(ten, 1)).toBe(100);
  });

  test("100 samples: p50 is the 50th, p99 the 99th", () => {
    const hundred = Array.from({ length: 100 }, (_, i) => i + 1);
    expect(percentile(hundred, 0.5)).toBe(50);
    expect(percentile(hundred, 0.99)).toBe(99);
    // 0.07 * 100 is 7.000000000000001 in floating point: still rank 7.
    expect(percentile(hundred, 0.07)).toBe(7);
  });

  test("an empty list is 0 and one sample is every percentile", () => {
    expect(percentile([], 0.5)).toBe(0);
    expect(percentile([42], 0.01)).toBe(42);
    expect(percentile([42], 0.99)).toBe(42);
  });

  test("median sorts first", () => {
    expect(median([5, 1, 3])).toBe(3);
    expect(median([4, 1, 3, 2])).toBe(2);
    expect(median([])).toBe(0);
  });
});

describe("DrainWindow", () => {
  test("no drains: everything is 0", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    expect(window.snapshot()).toMatchObject({ drains: 0, drainsPerSec: 0, changeSetsPerSec: 0, appliedPerSec: 0, p50Us: 0, p99Us: 0, nsPerChangeSet: 0 });
    clock.advance(5000);
    expect(window.snapshot().drains).toBe(0);
  });

  test("a steady 60 drains a second reads 60 a second, with the change-sets and applies in them", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    // 60 Hz for 4 s; each drain consumed 166 change-sets, merged into 2 applies, in 0.4 ms.
    for (let i = 0; i < 240; i++) {
      clock.advance(1000 / 60);
      window.record(drain(0.4, 166, 2));
    }
    const s = window.snapshot();
    // The window holds 120 or 121 drains of the 16.7 ms beat: within a percent of 60 a second.
    expect(s.drainsPerSec).toBeGreaterThan(59.5);
    expect(s.drainsPerSec).toBeLessThan(60.6);
    expect(s.changeSetsPerSec / s.drainsPerSec).toBeCloseTo(166, 6);
    expect(s.appliedPerSec / s.drainsPerSec).toBeCloseTo(2, 6);
    expect(s.p50Us).toBeCloseTo(400, 6);
    expect(s.p99Us).toBeCloseTo(400, 6);
    // 0.4 ms over 166 change-sets: 2.4 microseconds each.
    expect(s.nsPerChangeSet).toBeCloseTo(400_000 / 166, 6);
  });

  test("the batch: every drain of the window, their time added up, and the longest one", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    expect(window.snapshot()).toMatchObject({ drains: 0, totalMs: 0, maxUs: 0 });
    for (const ms of [0, 0.1, 0, 0, 0.3, 0]) {
      clock.advance(16);
      window.record(drain(ms, 1, 1));
    }
    const s = window.snapshot();
    expect(s.drains).toBe(6);
    expect(s.totalMs).toBeCloseTo(0.4, 9);
    expect(s.maxUs).toBeCloseTo(300, 6);
  });

  test("the rate uses the time since the start while that is shorter than the window", () => {
    const clock = fakeClock(1000);
    const window = new DrainWindow({ now: clock.now });
    for (let i = 0; i < 5; i++) {
      clock.advance(100);
      window.record(drain(0.05));
    }
    expect(window.snapshot().drainsPerSec).toBeCloseTo(10, 10);
  });

  test("nothing is reported at the instant the window starts", () => {
    const clock = fakeClock(42);
    const window = new DrainWindow({ now: clock.now });
    window.record(drain(0.01));
    expect(window.snapshot().drainsPerSec).toBe(0);
  });

  test("percentiles are nearest-rank over drains, not over change-sets", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    clock.advance(1000);
    // 98 quick drains, one slow one carrying 10,000 change-sets, one slower still.
    for (let i = 0; i < 98; i++) window.record(drain(0.02));
    window.record(drain(0.9, 10_000));
    window.record(drain(5));
    const s = window.snapshot();
    expect(s.p50Us).toBe(20);
    expect(s.p99Us).toBe(900);
  });

  test("the nanoseconds per change-set average survives a coarse clock", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    clock.advance(1000);
    // Chrome's 0.1 ms clock: most drains read 0 and a few read 0.1 ms, but the sum is right.
    for (let i = 0; i < 90; i++) window.record(drain(0, 100));
    for (let i = 0; i < 10; i++) window.record(drain(0.1, 100));
    const s = window.snapshot();
    expect(s.p50Us).toBe(0);
    expect(s.nsPerChangeSet).toBeCloseTo(1e6 / 10_000, 6);
  });

  test("drains leave the window after two seconds", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    clock.advance(1000);
    window.record(drain(0.9));
    clock.advance(1500);
    window.record(drain(0.1));
    expect(window.snapshot().p99Us).toBe(900);
    clock.advance(600);
    expect(window.snapshot().p99Us).toBe(100);
    clock.advance(1500);
    expect(window.snapshot().drains).toBe(0);
  });

  test("the window is (now - windowMs, now]: a drain exactly windowMs old is out", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now, windowMs: 1000 });
    clock.advance(10);
    window.record(drain(0.007));
    clock.advance(999);
    expect(window.snapshot().drains).toBe(1);
    clock.advance(1);
    expect(window.snapshot().drains).toBe(0);
  });

  test("at most maxSamples drains are kept, the oldest dropped first", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now, maxSamples: 3 });
    clock.advance(100);
    for (const ms of [1, 0.001, 0.002, 0.003]) window.record(drain(ms));
    expect(window.snapshot().p99Us).toBe(3);
  });

  test("a long run reclaims expired drains and stays correct", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    for (let i = 0; i < 20_000; i++) {
      clock.advance(1);
      window.record(drain(0.005));
    }
    const s = window.snapshot();
    expect(s.drainsPerSec).toBeCloseTo(1000, 6);
    expect(s.p50Us).toBe(5);
  });

  test("ignores drains with an unusable duration or no change-sets", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    clock.advance(1000);
    window.record(drain(Number.NaN));
    window.record(drain(Number.POSITIVE_INFINITY));
    window.record(drain(-1));
    window.record(drain(1, 0));
    window.record(drain(1, Number.NaN));
    expect(window.snapshot().drains).toBe(0);
    window.record(drain(1, 3, 1));
    expect(window.snapshot().drains).toBe(1);
  });

  test("reset forgets everything and starts the span again", () => {
    const clock = fakeClock();
    const window = new DrainWindow({ now: clock.now });
    clock.advance(1000);
    window.record(drain(1));
    window.reset();
    expect(window.snapshot().drains).toBe(0);
    clock.advance(500);
    window.record(drain(1));
    // One drain in the 500 ms since the reset.
    expect(window.snapshot().drainsPerSec).toBeCloseTo(2, 10);
  });
});

describe("RateWindow", () => {
  test("a counter's change over the time between readings, per second", () => {
    const clock = fakeClock();
    const rates = new RateWindow<"a" | "b">({ now: clock.now });
    rates.push({ a: 0, b: 100 });
    clock.advance(500);
    rates.push({ a: 5000, b: 100 });
    expect(rates.rates()).toEqual({ a: 10_000, b: 0 });
  });

  test("fewer than two readings: no rates", () => {
    const clock = fakeClock();
    const rates = new RateWindow<"a">({ now: clock.now });
    expect(rates.rates()).toEqual({});
    rates.push({ a: 7 });
    expect(rates.rates()).toEqual({ a: 0 });
  });

  test("two readings at the same instant divide by nothing and say 0", () => {
    const clock = fakeClock();
    const rates = new RateWindow<"a">({ now: clock.now });
    rates.push({ a: 1 });
    rates.push({ a: 99 });
    expect(rates.rates()).toEqual({ a: 0 });
  });

  test("the window slides: only the last two seconds count", () => {
    const clock = fakeClock();
    const rates = new RateWindow<"a">({ now: clock.now });
    let a = 0;
    rates.push({ a });
    // A burst of 100,000 in the first second, then 1,000 a second.
    clock.advance(1000);
    a += 100_000;
    rates.push({ a });
    for (let i = 0; i < 8; i++) {
      clock.advance(500);
      a += 500;
      rates.push({ a });
    }
    // The last 2 s (t = 3 s .. 5 s) saw 2,000 updates; the burst is long out.
    expect(rates.rates().a).toBeCloseTo(1000, 6);
  });

  test("reset forgets the readings", () => {
    const clock = fakeClock();
    const rates = new RateWindow<"a">({ now: clock.now });
    rates.push({ a: 0 });
    clock.advance(1000);
    rates.push({ a: 10 });
    rates.reset();
    expect(rates.rates()).toEqual({});
  });
});

/** A frame source the test drives: `tick(t)` delivers the pending request at time `t`. */
function fakeFrames(): { source: FrameSource; tick: (t: number) => void; pending: () => number } {
  let next = 1;
  const pending = new Map<number, (t: number) => void>();
  return {
    source: {
      request: (callback) => {
        const handle = next++;
        pending.set(handle, callback);
        return handle;
      },
      cancel: (handle) => {
        pending.delete(handle);
      },
    },
    tick: (t) => {
      const due = [...pending.values()];
      pending.clear();
      for (const callback of due) callback(t);
    },
    pending: () => pending.size,
  };
}

/** Runs `count` frames `intervalMs` apart starting at `from`, and returns the time of the last. */
function run(frames: ReturnType<typeof fakeFrames>, from: number, count: number, intervalMs: number): number {
  let t = from;
  for (let i = 0; i < count; i++) {
    frames.tick(t);
    t += intervalMs;
  }
  return t - intervalMs;
}

describe("FrameMonitor", () => {
  test("steady 60 Hz drops nothing and measures 16.7 ms", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source });
    monitor.start();
    const last = run(frames, 1000, 200, 1000 / 60);
    const s = monitor.snapshot(last);
    expect(s.dropped).toBe(0);
    expect(s.calibrated).toBe(true);
    expect(s.nominalMs).toBeCloseTo(16.667, 2);
    expect(s.frames).toBe(199);
    expect(s.longestMs).toBeCloseTo(16.667, 2);
  });

  test("steady 120 Hz drops nothing and measures 8.3 ms", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source });
    monitor.start();
    const last = run(frames, 0, 300, 1000 / 120);
    const s = monitor.snapshot(last);
    expect(s.dropped).toBe(0);
    expect(s.nominalMs).toBeCloseTo(8.333, 2);
  });

  test("a gap over 1.5 intervals drops round(gap / interval) - 1 frames, at 60 Hz", () => {
    const interval = 1000 / 60;
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: interval });
    monitor.start();
    let t = 0;
    frames.tick(t);
    const gaps = [interval, interval * 1.49, interval * 1.51, interval * 2, interval * 3.1, interval * 6];
    // Not dropped: 1 and 1.49 intervals. Dropped: 1.51 -> 1, 2 -> 1, 3.1 -> 2, 6 -> 5.
    for (const gap of gaps) {
      t += gap;
      frames.tick(t);
    }
    expect(DROP_THRESHOLD).toBe(1.5);
    expect(monitor.snapshot(t).dropped).toBe(1 + 1 + 2 + 5);
  });

  test("the same at 120 Hz: the interval is measured, so a 25 ms gap is two dropped frames", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source });
    monitor.start();
    let t = run(frames, 0, 100, 1000 / 120);
    expect(monitor.snapshot(t).dropped).toBe(0);
    // 25 ms is three 120 Hz frames: two were dropped. (At 60 Hz the same gap is one and a half.)
    t += 25;
    frames.tick(t);
    expect(monitor.snapshot(t).dropped).toBe(2);
    expect(monitor.snapshot(t).longestMs).toBeCloseTo(25, 6);
  });

  test("the interval is the median of the first gaps, so a few long ones do not skew it", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, calibrationFrames: 20 });
    monitor.start();
    let t = 0;
    frames.tick(t);
    for (let i = 0; i < 20; i++) {
      // Three slow frames among twenty.
      t += i === 3 || i === 9 || i === 15 ? 50 : 16.67;
      frames.tick(t);
    }
    const s = monitor.snapshot(t);
    expect(s.calibrated).toBe(true);
    expect(s.nominalMs).toBeCloseTo(16.67, 2);
    // The three slow ones (50 ms: two frames each) were counted while calibrating, with the median so far.
    expect(s.dropped).toBe(6);
  });

  test("before it has enough gaps the interval is the 60 Hz fallback", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source });
    monitor.start();
    frames.tick(0);
    frames.tick(8);
    const s = monitor.snapshot(8);
    expect(s.calibrated).toBe(false);
    expect(s.nominalMs).toBeCloseTo(16.667, 2);
  });

  test("pause and resume bracket a hidden tab: the gap across it is not a slow frame", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: 16.67 });
    monitor.start();
    frames.tick(0);
    frames.tick(16.67);
    monitor.pause();
    expect(frames.pending()).toBe(0);
    monitor.resume();
    expect(frames.pending()).toBe(1);
    frames.tick(9000);
    frames.tick(9016.67);
    const s = monitor.snapshot(9016.67);
    expect(s.dropped).toBe(0);
    expect(s.frames).toBe(2);
  });

  test("a gap over a second is a suspended page, not frames: it counts for nothing", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: 16.67 });
    monitor.start();
    frames.tick(0);
    frames.tick(5000);
    expect(monitor.snapshot(5000)).toMatchObject({ dropped: 0, frames: 0, longestMs: 0 });
  });

  test("recent drops and the longest frame cover the last five seconds; the total does not forget", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: 16.67 });
    monitor.start();
    let t = 0;
    frames.tick(t);
    t += 100; // 100 ms: 5 dropped
    frames.tick(t);
    expect(monitor.snapshot(t)).toMatchObject({ dropped: 5, droppedRecent: 5, longestMs: 100 });
    // Six seconds of good frames.
    for (let i = 0; i < 360; i++) {
      t += 16.67;
      frames.tick(t);
    }
    expect(monitor.snapshot(t)).toMatchObject({ dropped: 5, droppedRecent: 0 });
    expect(monitor.snapshot(t).longestMs).toBeCloseTo(16.67, 2);
  });

  test("reset zeroes the counts and keeps the measured interval", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, calibrationFrames: 10 });
    monitor.start();
    let t = run(frames, 0, 30, 8.33);
    t += 100;
    frames.tick(t);
    expect(monitor.snapshot(t).dropped).toBeGreaterThan(0);
    monitor.reset();
    expect(monitor.snapshot(t)).toMatchObject({ dropped: 0, droppedRecent: 0, frames: 0, calibrated: true });
    expect(monitor.snapshot(t).nominalMs).toBeCloseTo(8.33, 2);
  });

  test("start twice asks for one frame; stop cancels it and keeps the counts", () => {
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: 16.67 });
    monitor.start();
    monitor.start();
    expect(frames.pending()).toBe(1);
    frames.tick(0);
    frames.tick(100);
    monitor.stop();
    expect(frames.pending()).toBe(0);
    expect(monitor.snapshot(100).dropped).toBe(5);
    // Stopped: a late callback request does not restart anything.
    monitor.resume();
    expect(frames.pending()).toBe(0);
  });
});

/** A mirror the test drives: its counters, and the listeners a drain calls. */
function fakeMirror(): { mirror: MirrorLike; counters: { -readonly [K in keyof MirrorCounters]: number }; drain: (sample: ReturnType<typeof drain>) => void; listeners: () => number } {
  const listeners = new Set<(sample: ReturnType<typeof drain>) => void>();
  const counters = { changeSetsReceived: 0, entriesReceived: 0, entriesApplied: 0, drains: 0 };
  return {
    mirror: {
      addDrainListener: (listener) => {
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      },
      stats: () => ({ ...counters }),
    },
    counters,
    drain: (sample) => {
      counters.drains++;
      for (const listener of [...listeners]) listener(sample);
    },
    listeners: () => listeners.size,
  };
}

describe("StressMeter", () => {
  test("reads the runtime's counters and drains: rates, merge ratio, percentiles", () => {
    const clock = fakeClock();
    const m = fakeMirror();
    let generated = 0;
    const meter = new StressMeter({ mirror: m.mirror, generated: () => generated, now: clock.now, heapBytes: () => null, timerResolutionUs: 100 });
    meter.start();
    expect(m.listeners()).toBe(1);
    meter.snapshot(); // the baseline at t = 0

    // Four seconds of 10,000 updates a second at 60 drains a second: each drain merges ~167 change-sets into 1 apply.
    let last = meter.snapshot();
    for (let half = 0; half < 8; half++) {
      for (let i = 0; i < 30; i++) {
        clock.advance(500 / 30);
        m.drain(drain(0.3, 167, 1, 167));
      }
      clock.set((half + 1) * 500);
      generated += 5000;
      m.counters.changeSetsReceived += 5010; // the updates, and the generator's 5 counter writes
      m.counters.entriesReceived += 5010;
      m.counters.entriesApplied += 31;
      last = meter.snapshot();
    }
    expect(last.generatedPerSec).toBeCloseTo(10_000, 6);
    expect(last.receivedPerSec).toBeCloseTo(10_020, 6);
    expect(last.entriesReceivedPerSec).toBeCloseTo(10_020, 6);
    expect(last.appliedPerSec).toBeCloseTo(62, 6);
    expect(last.mergeRatio).toBeCloseTo(62 / 10_020, 6);
    expect(last.drainsPerSec).toBeGreaterThan(59.5);
    expect(last.drainsPerSec).toBeLessThan(60.6);
    expect(last.drainP50Us).toBeCloseTo(300, 6);
    expect(last.drainP99Us).toBeCloseTo(300, 6);
    expect(last.nsPerChangeSet).toBeCloseTo(300_000 / 167, 6);
    expect(last.generatedTotal).toBe(40_000);
    expect(last.receivedTotal).toBe(40_080);
    expect(last.appliedTotal).toBe(248);
    expect(last.timerResolutionUs).toBe(100);
    expect(last.heapMb).toBeNull();
  });

  test("an idle core reads zero everywhere, with no division by zero", () => {
    const clock = fakeClock();
    const m = fakeMirror();
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 0, now: clock.now, heapBytes: () => null });
    meter.start();
    meter.snapshot();
    clock.advance(500);
    const s = meter.snapshot();
    expect(s).toMatchObject({
      generatedPerSec: 0,
      receivedPerSec: 0,
      appliedPerSec: 0,
      mergeRatio: 0,
      drainsPerSec: 0,
      drainP50Us: 0,
      drainP99Us: 0,
      nsPerChangeSet: 0,
      droppedFrames: 0,
    });
  });

  test("the first snapshot has no rates yet, not NaN", () => {
    const m = fakeMirror();
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 5, now: fakeClock().now, heapBytes: () => null });
    const s = meter.snapshot();
    expect(s.generatedPerSec).toBe(0);
    expect(s.mergeRatio).toBe(0);
    expect(Number.isNaN(s.drainsPerSec)).toBe(false);
  });

  test("counts how many times each watched signal notified, and reset starts over", () => {
    const clock = fakeClock();
    const m = fakeMirror();
    const subscribers = new Set<() => void>();
    const progress = {
      subscribe: (listener: () => void) => {
        subscribers.add(listener);
        return () => {
          subscribers.delete(listener);
        };
      },
    };
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 0, now: clock.now, heapBytes: () => null, applies: { progress } });
    meter.start();
    expect(subscribers.size).toBe(1);
    for (let i = 0; i < 7; i++) for (const listener of subscribers) listener();
    expect(meter.snapshot().applies).toEqual({ progress: 7 });
    meter.reset();
    expect(meter.snapshot().applies).toEqual({ progress: 0 });
    for (const listener of subscribers) listener();
    expect(meter.snapshot().applies).toEqual({ progress: 1 });
    meter.stop();
    expect(subscribers.size).toBe(0);
    expect(m.listeners()).toBe(0);
  });

  test("reset zeroes the totals and the windows; the core's counters keep counting from where they were", () => {
    const clock = fakeClock();
    const m = fakeMirror();
    let generated = 1000;
    m.counters.changeSetsReceived = 1000;
    m.counters.entriesApplied = 10;
    const meter = new StressMeter({ mirror: m.mirror, generated: () => generated, now: clock.now, heapBytes: () => null });
    meter.start();
    generated += 500;
    m.counters.changeSetsReceived += 500;
    m.counters.entriesApplied += 5;
    expect(meter.snapshot()).toMatchObject({ generatedTotal: 500, receivedTotal: 500, appliedTotal: 5 });
    meter.reset();
    expect(meter.snapshot()).toMatchObject({ generatedTotal: 0, receivedTotal: 0, appliedTotal: 0 });
  });

  test("frames: dropped frames flow through, and the monitor starts and stops with the meter", () => {
    const clock = fakeClock();
    const m = fakeMirror();
    const frames = fakeFrames();
    const monitor = new FrameMonitor({ source: frames.source, nominalMs: 16.67 });
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 0, now: clock.now, frames: monitor, heapBytes: () => null });
    expect(frames.pending()).toBe(0);
    meter.start();
    expect(frames.pending()).toBe(1);
    frames.tick(0);
    frames.tick(100);
    clock.set(100);
    expect(meter.snapshot()).toMatchObject({ droppedFrames: 5, droppedFramesRecent: 5, longestFrameMs: 100 });
    meter.reset();
    expect(meter.snapshot().droppedFrames).toBe(0);
    meter.stop();
    expect(frames.pending()).toBe(0);
  });

  test("heap is in megabytes where the browser has it", () => {
    const m = fakeMirror();
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 0, now: fakeClock().now, heapBytes: () => 64 * 1024 * 1024 });
    expect(meter.snapshot().heapMb).toBe(64);
  });

  test("start twice listens once", () => {
    const m = fakeMirror();
    const meter = new StressMeter({ mirror: m.mirror, generated: () => 0, now: fakeClock().now, heapBytes: () => null });
    meter.start();
    meter.start();
    expect(m.listeners()).toBe(1);
  });
});

describe("StressMeter on the runtime's real Mirror", () => {
  const STORE = 7n;
  afterEach(() => {
    vi.restoreAllMocks();
  });

  /** A change-set with one full-value entry for signal `signalId` of the store. */
  const changeSet = (txnId: number, signalId = 0): Uint8Array =>
    encodeChangeSet({ txnId: BigInt(txnId), entries: [{ handle: STORE, signalId, op: ChangeOp.FullValue, value: new Uint8Array([txnId & 0xff]) }] });

  test("a thousand change-sets before one frame: received 1,000, applied once, merge ratio 0.001", () => {
    // The mirror times its drains with the global performance.now(): the store's apply moves it by 0.25 ms.
    const mirrorClock = fakeClock(100);
    vi.spyOn(performance, "now").mockImplementation(mirrorClock.now);
    const scheduled: Array<() => void> = [];
    const mirror = new Mirror({ schedule: (fn) => scheduled.push(fn) });
    mirror.register(STORE, () => {
      mirrorClock.advance(0.25);
    });
    const clock = fakeClock();
    const meter = new StressMeter({ mirror, generated: () => 1000, now: clock.now, heapBytes: () => null });
    meter.start();
    meter.snapshot();

    for (let i = 0; i < 1000; i++) mirror.enqueue(changeSet(i));
    clock.advance(500);
    (scheduled.shift() as () => void)();
    const s = meter.snapshot();
    expect(s.receivedTotal).toBe(1000);
    expect(s.appliedTotal).toBe(1);
    expect(s.receivedPerSec).toBeCloseTo(2000, 6);
    expect(s.entriesReceivedPerSec).toBeCloseTo(2000, 6);
    expect(s.appliedPerSec).toBeCloseTo(2, 6);
    expect(s.mergeRatio).toBeCloseTo(0.001, 9);
    expect(s.drainsPerSec).toBeCloseTo(2, 6);
    expect(s.drainP50Us).toBeCloseTo(250, 6);
    expect(s.nsPerChangeSet).toBeCloseTo(250, 6);
    meter.stop();
  });

  test("an unmerged signal is applied entry by entry", () => {
    const scheduled: Array<() => void> = [];
    const mirror = new Mirror({ schedule: (fn) => scheduled.push(fn) });
    const seen: number[] = [];
    mirror.register(STORE, (signalId) => seen.push(signalId), { noCoalesce: [1] });
    const clock = fakeClock();
    const meter = new StressMeter({ mirror, generated: () => 0, now: clock.now, heapBytes: () => null });
    meter.start();
    meter.snapshot();
    for (let i = 0; i < 300; i++) mirror.enqueue(changeSet(i, 1));
    for (let i = 0; i < 300; i++) mirror.enqueue(changeSet(1000 + i, 0));
    clock.advance(1000);
    (scheduled.shift() as () => void)();
    const s = meter.snapshot();
    // 300 progress entries one by one, 300 value entries merged into 1.
    expect(seen.filter((id) => id === 1)).toHaveLength(300);
    expect(seen.filter((id) => id === 0)).toHaveLength(1);
    expect(s.appliedTotal).toBe(301);
    expect(s.receivedTotal).toBe(600);
    meter.stop();
  });

  test("a backlog past the mirror's bound is folded before a frame comes, and the snapshot says how often", () => {
    const scheduled: Array<() => void> = [];
    const mirror = new Mirror({ schedule: (fn) => scheduled.push(fn), maxPendingEntries: 100 });
    mirror.register(STORE, () => {});
    const clock = fakeClock();
    const meter = new StressMeter({ mirror, generated: () => 0, now: clock.now, heapBytes: () => null });
    meter.start();
    expect(meter.snapshot().compactions).toBe(0);
    // 1,000 change-sets for one signal before any frame: the queue passes 100 entries and is folded in place.
    for (let i = 0; i < 1000; i++) mirror.enqueue(changeSet(i));
    expect(mirror.stats().compactions).toBeGreaterThan(0);
    expect(mirror.pending).toBeLessThan(1000);
    clock.advance(500);
    (scheduled.shift() as () => void)();
    const s = meter.snapshot();
    expect(s.compactions).toBe(mirror.stats().compactions);
    expect(s.receivedTotal).toBe(1000);
    expect(s.appliedTotal).toBe(1);
    // A reset measures from now: the folds so far are forgotten, later ones are counted.
    meter.reset();
    expect(meter.snapshot().compactions).toBe(0);
    meter.stop();
  });
});

describe("formatters", () => {
  test("formatDuration says 'under' at or below the clock's step", () => {
    expect(formatDuration(0, 100)).toBe("< 0.1 ms");
    expect(formatDuration(100, 100)).toBe("< 0.1 ms");
    // One clock step as the drain window computes it (0.10000000000000853 ms x 1000) is still one step.
    expect(formatDuration(0.10000000000000853 * 1000, 100)).toBe("< 0.1 ms");
    expect(formatDuration(101, 100)).toBe("101 µs");
    expect(formatDuration(0, 1000)).toBe("< 1 ms");
    expect(formatDuration(0, 5)).toBe("< 5 µs");
    expect(formatDuration(450, 100)).toBe("450 µs");
    expect(formatDuration(2500, 100)).toBe("2.50 ms");
    expect(formatDuration(12_345, 100)).toBe("12.3 ms");
  });

  test("with no known step only a literal zero is 'under'", () => {
    expect(formatDuration(0, 0)).toBe("< 1 µs");
    expect(formatDuration(0.5, 0)).toBe("< 1 µs");
    expect(formatDuration(3, 0)).toBe("3 µs");
  });

  test("a duration that is not a number is a dash", () => {
    expect(formatDuration(Number.NaN, 100)).toBe("–");
    expect(formatDuration(-1, 100)).toBe("–");
  });

  test("formatStep, formatCount and formatMerge", () => {
    expect(formatStep(100)).toBe("0.1 ms");
    expect(formatStep(1000)).toBe("1 ms");
    expect(formatStep(5)).toBe("5 µs");
    expect(formatStep(99.999_994_039_535_52)).toBe("0.1 ms");
    expect(formatDuration(100, 99.999_994_039_535_52)).toBe("< 0.1 ms");
    expect(formatCount(10_000.4)).toBe("10,000");
    expect(formatCount(Number.NaN)).toBe("–");
    expect(formatMerge(0)).toBe("–");
    expect(formatMerge(1)).toBe("nothing merged");
    expect(formatMerge(0.5)).toBe("1 apply per 2.0 received");
    expect(formatMerge(1 / 167)).toBe("1 apply per 167 received");
    expect(formatMerge(1 / 1500)).toBe("1 apply per 1,500 received");
  });

  test("browserHeapBytes is a number where there is performance.memory and null where there is not", () => {
    const perf = performance as Performance & { memory?: unknown };
    const had = Object.getOwnPropertyDescriptor(perf, "memory");
    Object.defineProperty(perf, "memory", { value: { usedJSHeapSize: 123 }, configurable: true });
    expect(browserHeapBytes()).toBe(123);
    Object.defineProperty(perf, "memory", { value: undefined, configurable: true });
    expect(browserHeapBytes()).toBeNull();
    Object.defineProperty(perf, "memory", { value: { usedJSHeapSize: "x" }, configurable: true });
    expect(browserHeapBytes()).toBeNull();
    if (had) Object.defineProperty(perf, "memory", had);
    else delete perf.memory;
  });
});
