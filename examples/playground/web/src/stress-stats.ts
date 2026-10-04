/*
 * The numbers the stress screen shows and the landing page repeats, and how they are measured.
 * Pure: no DOM, no timers, no runtime import, every clock injected, so the math is exact and
 * deterministic under vitest. The browser glue (`requestAnimationFrame`, `visibilitychange`,
 * `performance.memory`) is at the bottom, in functions that touch the DOM only when called.
 *
 * WHERE EACH NUMBER COMES FROM
 *
 *   generated / s       the core's own `generated` signal (one write per 10 ms tick of the Timer-paced
 *                       generator), as a delta over the window: what the core really produced.
 *   received / s        `mirror.stats().changeSetsReceived`, as a delta: change-sets the mirror was
 *                       handed (one per core transaction).
 *   applied / s         `mirror.stats().entriesApplied`, as a delta: entries applied to the stores
 *                       AFTER the mirror merged what arrived between two frames (ADR-031).
 *                       `mergeRatio` is applied over received entries: 1 means nothing was merged.
 *   drains / s          `mirror.stats().drains`, as a delta.
 *   drain p50 / p99     the drain listener's `durationMs` per drain, over a rolling window. A drain is
 *                       one application of the queue on the main thread, so this is the time the
 *                       mirror held the thread each time it ran.
 *   ns per change-set   sum of drain time over sum of change-sets in the window: the average that
 *                       survives the browser's clock rounding.
 *   dropped frames      a `requestAnimationFrame` loop: a gap over 1.5 frame intervals drops
 *                       `round(gap / interval) - 1` frames. The interval is the median of the first
 *                       60 gaps (so 60, 90 and 120 Hz displays all work).
 *
 * LIMITS TO STATE WHEREVER THE NUMBERS ARE SHOWN. `performance.now()` is rounded by the browser
 * (Chrome: 0.1 ms unless the page is cross-origin isolated, which GitHub Pages cannot do; Firefox
 * and Safari: 1 ms), so a single drain that takes less than the step reads as 0 or as one step.
 * `timerResolutionUs` is the step this page measured; show a drain time at or below it as "under
 * <step>". The nanoseconds-per-change-set average is the number that survives that rounding. Heap
 * is `performance.memory`, which only Chrome has, and it is the JS heap: the core's wasm memory is
 * not in it.
 */

/** What the mirror reports after each drain (`Mirror.addDrainListener`). */
export interface DrainSample {
  /** Change-sets the drain consumed. */
  readonly changeSets: number;
  /** Entries those change-sets carried. */
  readonly entries: number;
  /** Entries applied to stores after merging. */
  readonly appliedEntries: number;
  /** How long the drain took, in milliseconds. */
  readonly durationMs: number;
}

/** The mirror's cumulative counters that the meter reads (`Mirror.stats()`). */
export interface MirrorCounters {
  readonly changeSetsReceived: number;
  readonly entriesReceived: number;
  readonly entriesApplied: number;
  readonly drains: number;
  /** Times the backlog passed its bound and was folded in place (ADR-031 decision 3); 0 when the mirror does not say. */
  readonly compactions?: number;
}

/** The part of the runtime's `Mirror` that the meter uses. */
export interface MirrorLike {
  /** Calls `listener` after every drain; returns the function that removes it. */
  addDrainListener(listener: (sample: DrainSample) => void): () => void;
  /** The counters since the mirror was created. */
  stats(): MirrorCounters;
}

/**
 * The nearest-rank percentile `p` (0..1) of an ascending-sorted list: the value at rank
 * `ceil(p * n)`. 0 for an empty list.
 *
 * ```ts
 * percentile([10, 20, 30, 40], 0.5); // 20
 * ```
 */
export function percentile(sorted: readonly number[], p: number): number {
  if (sorted.length === 0) return 0;
  // The epsilon keeps 0.07 * 100 = 7.000000000000001 from rounding up to rank 8.
  const rank = Math.min(sorted.length, Math.max(1, Math.ceil(p * sorted.length - 1e-9)));
  return sorted[rank - 1] as number;
}

/** The median of an unsorted list; 0 for an empty one. */
export function median(values: readonly number[]): number {
  return percentile([...values].sort((a, b) => a - b), 0.5);
}

/** Sums of a window of drains and what they say. */
export interface DrainWindowSnapshot {
  /** Drains in the window. */
  readonly drains: number;
  /** Change-sets they consumed, entries applied after merging, and the drain time in milliseconds. */
  readonly changeSets: number;
  readonly appliedEntries: number;
  readonly totalMs: number;
  /** Per second over the window (the time since the start while that is shorter). */
  readonly drainsPerSec: number;
  readonly changeSetsPerSec: number;
  readonly appliedPerSec: number;
  /** Nearest-rank percentiles of the drain durations, in microseconds; 0 with no drains. */
  readonly p50Us: number;
  readonly p99Us: number;
  /** The longest single drain, in microseconds; 0 with no drains. */
  readonly maxUs: number;
  /** Sum of drain time over sum of change-sets, in nanoseconds; 0 with no change-sets. */
  readonly nsPerChangeSet: number;
}

/** Options of a {@link DrainWindow}. */
export interface DrainWindowOptions {
  /** The clock, in milliseconds (`performance.now` in the page). */
  readonly now: () => number;
  /** How far back the window reaches. Default 2000. */
  readonly windowMs?: number;
  /** Drains kept at most; the oldest are dropped first. Default 50,000. */
  readonly maxSamples?: number;
}

interface TimedDrain {
  readonly at: number;
  readonly sample: DrainSample;
}

const EMPTY_WINDOW: DrainWindowSnapshot = {
  drains: 0,
  changeSets: 0,
  appliedEntries: 0,
  totalMs: 0,
  drainsPerSec: 0,
  changeSetsPerSec: 0,
  appliedPerSec: 0,
  p50Us: 0,
  p99Us: 0,
  maxUs: 0,
  nsPerChangeSet: 0,
};

/**
 * A rolling window over the drains the mirror reports. Pure, with an injected clock.
 *
 * ```ts
 * const window = new DrainWindow({ now: () => performance.now() });
 * mirror.addDrainListener((drain) => window.record(drain));
 * window.snapshot(); // { drainsPerSec, p50Us, p99Us, nsPerChangeSet, ... }
 * ```
 */
export class DrainWindow {
  readonly #now: () => number;
  readonly #windowMs: number;
  readonly #maxSamples: number;
  #startedAt: number;
  #samples: TimedDrain[] = [];
  /** Index of the oldest live sample; earlier ones expired and are reclaimed lazily. */
  #head = 0;

  /** @param options See {@link DrainWindowOptions}. The window starts now. */
  constructor(options: DrainWindowOptions) {
    this.#now = options.now;
    this.#windowMs = options.windowMs ?? 2000;
    this.#maxSamples = options.maxSamples ?? 50_000;
    this.#startedAt = options.now();
  }

  /** Records one drain, now. A drain with no change-sets or an unusable duration is ignored. */
  record(sample: DrainSample): void {
    if (!Number.isFinite(sample.durationMs) || sample.durationMs < 0) return;
    if (!Number.isFinite(sample.changeSets) || sample.changeSets < 1) return;
    const at = this.#now();
    this.#expire(at);
    this.#samples.push({ at, sample });
    if (this.#samples.length - this.#head > this.#maxSamples) this.#head++;
  }

  /** Forgets every drain and starts the window again, now. */
  reset(): void {
    this.#samples = [];
    this.#head = 0;
    this.#startedAt = this.#now();
  }

  /** The sums, rates and percentiles over the window that ends now. All zero when it holds no drain. */
  snapshot(): DrainWindowSnapshot {
    const now = this.#now();
    this.#expire(now);
    const live = this.#samples.slice(this.#head);
    if (live.length === 0) return EMPTY_WINDOW;
    let changeSets = 0;
    let appliedEntries = 0;
    let totalMs = 0;
    const durationsUs: number[] = [];
    for (const { sample } of live) {
      changeSets += sample.changeSets;
      appliedEntries += sample.appliedEntries;
      totalMs += sample.durationMs;
      durationsUs.push(sample.durationMs * 1000);
    }
    durationsUs.sort((a, b) => a - b);
    // The window is shorter than `windowMs` until it has run that long.
    const spanMs = Math.min(this.#windowMs, now - this.#startedAt);
    const perSec = (count: number): number => (spanMs > 0 ? (count * 1000) / spanMs : 0);
    return {
      drains: live.length,
      changeSets,
      appliedEntries,
      totalMs,
      drainsPerSec: perSec(live.length),
      changeSetsPerSec: perSec(changeSets),
      appliedPerSec: perSec(appliedEntries),
      p50Us: percentile(durationsUs, 0.5),
      p99Us: percentile(durationsUs, 0.99),
      maxUs: durationsUs[durationsUs.length - 1] ?? 0,
      nsPerChangeSet: (totalMs * 1e6) / changeSets,
    };
  }

  /** Drops what left the window `(now - windowMs, now]`. */
  #expire(now: number): void {
    const oldest = now - this.#windowMs;
    while (this.#head < this.#samples.length && (this.#samples[this.#head] as TimedDrain).at <= oldest) this.#head++;
    if (this.#head > 1024 && this.#head * 2 > this.#samples.length) {
      this.#samples = this.#samples.slice(this.#head);
      this.#head = 0;
    }
  }
}

/**
 * Per-second rates of cumulative counters, over a rolling window: each {@link RateWindow.push}
 * records the counters now, and the rate of a counter is its change since the oldest reading
 * still inside the window (or the oldest there is) over the time between them.
 *
 * ```ts
 * const rates = new RateWindow<"generated">({ now: () => performance.now() });
 * rates.push({ generated: 0 });        // at t = 0 ms
 * rates.push({ generated: 5000 });     // at t = 500 ms
 * rates.rates();                       // { generated: 10000 }
 * ```
 */
export class RateWindow<K extends string> {
  readonly #now: () => number;
  readonly #windowMs: number;
  #readings: Array<{ readonly at: number; readonly values: Readonly<Record<K, number>> }> = [];

  /** @param options `now`, the clock in milliseconds, and `windowMs` (default 2000). */
  constructor(options: { readonly now: () => number; readonly windowMs?: number }) {
    this.#now = options.now;
    this.#windowMs = options.windowMs ?? 2000;
  }

  /** Records the counters as they are now. */
  push(values: Readonly<Record<K, number>>): void {
    const at = this.#now();
    this.#readings.push({ at, values });
    // The baseline is the newest reading at or before the window's start: drop the ones older than that.
    const start = at - this.#windowMs;
    while (this.#readings.length > 2 && (this.#readings[1] as { at: number }).at <= start) this.#readings.shift();
  }

  /** Forgets every reading. */
  reset(): void {
    this.#readings = [];
  }

  /** Counter changes per second between the oldest reading kept and the newest. All 0 with fewer than two readings. */
  rates(): Record<K, number> {
    const first = this.#readings[0];
    const last = this.#readings[this.#readings.length - 1];
    const out = {} as Record<K, number>;
    if (first === undefined || last === undefined) return out;
    const spanMs = last.at - first.at;
    for (const key of Object.keys(last.values) as K[]) {
      out[key] = spanMs > 0 ? (((last.values[key] as number) - (first.values[key] as number)) * 1000) / spanMs : 0;
    }
    return out;
  }
}

/** Where frames come from: `requestAnimationFrame` in a page, a fake in tests. */
export interface FrameSource {
  /** Calls `callback(timestampMs)` once, at the next frame; returns a handle for {@link FrameSource.cancel}. */
  request(callback: (timestampMs: number) => void): number;
  /** Cancels a pending request. */
  cancel(handle: number): void;
}

/** Options of a {@link FrameMonitor}. */
export interface FrameMonitorOptions {
  readonly source: FrameSource;
  /** The nominal frame interval, in milliseconds. Default: the median of the first {@link calibrationFrames} gaps. */
  readonly nominalMs?: number;
  /** Gaps measured to find the interval. Default 60. */
  readonly calibrationFrames?: number;
  /** The interval assumed until it is known. Default 1000 / 60. */
  readonly fallbackMs?: number;
  /** How far back "recent" reaches, in milliseconds. Default 5000. */
  readonly windowMs?: number;
}

/** What a {@link FrameMonitor} has seen. */
export interface FrameSnapshot {
  /** The frame interval in use, in milliseconds. */
  readonly nominalMs: number;
  /** Whether it was measured (`true`) or is the fallback still. */
  readonly calibrated: boolean;
  /** Frames seen since the last reset. */
  readonly frames: number;
  /** Frames dropped since the last reset. */
  readonly dropped: number;
  /** Frames dropped in the recent window. */
  readonly droppedRecent: number;
  /** The longest gap between two frames in the recent window, in milliseconds. */
  readonly longestMs: number;
}

/** A gap over this many frame intervals means frames were dropped. */
export const DROP_THRESHOLD = 1.5;

/** A gap longer than this is the page having been hidden or suspended, not a slow frame. */
const SUSPENDED_GAP_MS = 1000;

/**
 * Counts dropped frames from the gaps between animation frames. The interval is the median of the
 * first gaps (or `nominalMs`); a gap above {@link DROP_THRESHOLD} intervals drops
 * `round(gap / interval) - 1` frames. {@link pause} and {@link resume} bracket a time the page was
 * hidden, so that gap counts for nothing.
 */
export class FrameMonitor {
  readonly #source: FrameSource;
  readonly #calibrationFrames: number;
  readonly #fallbackMs: number;
  readonly #windowMs: number;
  #nominalMs: number | undefined;
  #gaps: number[] = [];
  #previous: number | undefined;
  #handle: number | undefined;
  #running = false;
  #frames = 0;
  #dropped = 0;
  #recent: Array<{ readonly at: number; readonly gap: number; readonly dropped: number }> = [];

  /** @param options See {@link FrameMonitorOptions}. */
  constructor(options: FrameMonitorOptions) {
    this.#source = options.source;
    this.#nominalMs = options.nominalMs;
    this.#calibrationFrames = options.calibrationFrames ?? 60;
    this.#fallbackMs = options.fallbackMs ?? 1000 / 60;
    this.#windowMs = options.windowMs ?? 5000;
  }

  /** Starts watching frames. Does nothing when already started. */
  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#previous = undefined;
    this.#request();
  }

  /** Stops watching. The counts stay until {@link reset}. */
  stop(): void {
    this.#running = false;
    if (this.#handle !== undefined) this.#source.cancel(this.#handle);
    this.#handle = undefined;
    this.#previous = undefined;
  }

  /** The page was hidden: no frame will come, and the time until the next one is not a slow frame. */
  pause(): void {
    this.#previous = undefined;
    if (this.#handle !== undefined) this.#source.cancel(this.#handle);
    this.#handle = undefined;
  }

  /** The page is visible again: watching resumes if it was started. */
  resume(): void {
    this.#previous = undefined;
    if (this.#running && this.#handle === undefined) this.#request();
  }

  /** Zeroes the counts (not the measured interval). */
  reset(): void {
    this.#frames = 0;
    this.#dropped = 0;
    this.#recent = [];
  }

  /** What has been seen; `now` (milliseconds, the frame clock) bounds the recent window. */
  snapshot(now: number): FrameSnapshot {
    this.#trim(now);
    let droppedRecent = 0;
    let longestMs = 0;
    for (const entry of this.#recent) {
      droppedRecent += entry.dropped;
      if (entry.gap > longestMs) longestMs = entry.gap;
    }
    return {
      nominalMs: this.#interval(),
      calibrated: this.#nominalMs !== undefined,
      frames: this.#frames,
      dropped: this.#dropped,
      droppedRecent,
      longestMs,
    };
  }

  /** One frame at `timestampMs`. Public so tests can drive it without a clock. */
  frame(timestampMs: number): void {
    this.#handle = undefined;
    const previous = this.#previous;
    this.#previous = timestampMs;
    if (previous !== undefined) this.#gap(timestampMs, timestampMs - previous);
    if (this.#running) this.#request();
  }

  #gap(at: number, gap: number): void {
    if (gap <= 0 || gap > SUSPENDED_GAP_MS) return;
    if (this.#nominalMs === undefined) {
      this.#gaps.push(gap);
      if (this.#gaps.length >= this.#calibrationFrames) this.#nominalMs = median(this.#gaps);
    }
    const interval = this.#interval();
    this.#frames++;
    let dropped = 0;
    if (gap > DROP_THRESHOLD * interval) dropped = Math.max(1, Math.round(gap / interval) - 1);
    this.#dropped += dropped;
    this.#recent.push({ at, gap, dropped });
    this.#trim(at);
  }

  /** The interval in use: measured, else the median so far once there are enough gaps, else the fallback. */
  #interval(): number {
    if (this.#nominalMs !== undefined) return this.#nominalMs;
    return this.#gaps.length >= 10 ? median(this.#gaps) : this.#fallbackMs;
  }

  #trim(now: number): void {
    const oldest = now - this.#windowMs;
    let drop = 0;
    while (drop < this.#recent.length && (this.#recent[drop] as { at: number }).at <= oldest) drop++;
    if (drop > 0) this.#recent.splice(0, drop);
  }

  #request(): void {
    this.#handle = this.#source.request((t) => this.frame(t));
  }
}

/** Something whose changes can be counted: a core `Signal`. */
export interface Subscribable {
  subscribe(listener: () => void): () => void;
}

/** Options of a {@link StressMeter}. */
export interface StressMeterOptions {
  /** The runtime's mirror: drain listener and counters. */
  readonly mirror: MirrorLike;
  /** The core's `generated` counter: updates the generator has committed so far. */
  readonly generated: () => number;
  /** The clock, in milliseconds. Default `performance.now`. */
  readonly now?: () => number;
  /** How far back rates and percentiles reach. Default 2000. */
  readonly windowMs?: number;
  /** Counts dropped frames; omitted, the frame fields stay 0. */
  readonly frames?: FrameMonitor;
  /** The smallest step of `performance.now()` this page measured, in microseconds. */
  readonly timerResolutionUs?: number;
  /** The JS heap in bytes, or `null` where the browser does not say. Default {@link browserHeapBytes}. */
  readonly heapBytes?: () => number | null;
  /** Signals whose notifications to count, by name: how often each one was applied. */
  readonly applies?: Readonly<Record<string, Subscribable>>;
}

/** Everything the stress screen shows, measured over the last window. */
export interface StressSnapshot {
  /** Updates the core generated per second. */
  readonly generatedPerSec: number;
  /** Change-sets the mirror received per second. */
  readonly receivedPerSec: number;
  /** Entries those change-sets carried, per second. */
  readonly entriesReceivedPerSec: number;
  /** Entries applied to the stores per second, after merging. */
  readonly appliedPerSec: number;
  /** Applied over received entries: 1 means nothing merged, 0.01 means a hundred entries per apply. */
  readonly mergeRatio: number;
  /** Drains per second. */
  readonly drainsPerSec: number;
  /** Median and 99th-percentile duration of one drain, in microseconds. */
  readonly drainP50Us: number;
  readonly drainP99Us: number;
  /** The drains in the window, all their time together in microseconds, and the longest one. */
  readonly drains: number;
  readonly drainTotalUs: number;
  readonly drainMaxUs: number;
  /** Drain time over change-sets in the window, in nanoseconds. */
  readonly nsPerChangeSet: number;
  /** Dropped frames since the last reset, and in the last five seconds. */
  readonly droppedFrames: number;
  readonly droppedFramesRecent: number;
  /** The longest gap between frames in the last five seconds, in milliseconds. */
  readonly longestFrameMs: number;
  /** The frame interval in use, in milliseconds. */
  readonly frameMs: number;
  /** The JS heap in megabytes; `null` outside Chrome. */
  readonly heapMb: number | null;
  /** The smallest step of `performance.now()`, in microseconds; 0 when it could not be measured. */
  readonly timerResolutionUs: number;
  /** Totals since the last reset: updates generated, change-sets received, entries applied. */
  readonly generatedTotal: number;
  readonly receivedTotal: number;
  readonly appliedTotal: number;
  /** Times since the last reset the mirror folded its backlog before a frame came (the queue passed its bound). */
  readonly compactions: number;
  /** How many times each watched signal notified its subscribers since the last reset. */
  readonly applies: Readonly<Record<string, number>>;
}

/**
 * Measures a mirror under load: reads its counters and drain listener and the core's `generated`
 * counter, and answers {@link StressMeter.snapshot} with the {@link StressSnapshot} the screen
 * shows. Call `snapshot()` on a timer (every 500 ms); each call also records the counters for the
 * next rates.
 */
export class StressMeter {
  readonly #mirror: MirrorLike;
  readonly #generated: () => number;
  readonly #now: () => number;
  readonly #drains: DrainWindow;
  readonly #rates: RateWindow<"generated" | "received" | "entries" | "applied">;
  readonly #frames: FrameMonitor | undefined;
  readonly #heap: () => number | null;
  readonly #timerResolutionUs: number;
  readonly #watched: ReadonlyArray<readonly [string, Subscribable]>;
  #counts: Record<string, number> = {};
  #stops: Array<() => void> = [];
  /** The counters at the last reset: totals are measured from them. */
  #base: { generated: number; received: number; applied: number; compactions: number };

  /** @param options See {@link StressMeterOptions}. */
  constructor(options: StressMeterOptions) {
    this.#mirror = options.mirror;
    this.#generated = options.generated;
    this.#now = options.now ?? (() => performance.now());
    this.#drains = new DrainWindow({ now: this.#now, windowMs: options.windowMs });
    this.#rates = new RateWindow({ now: this.#now, windowMs: options.windowMs });
    this.#frames = options.frames;
    this.#heap = options.heapBytes ?? browserHeapBytes;
    this.#timerResolutionUs = options.timerResolutionUs ?? 0;
    this.#watched = Object.entries(options.applies ?? {});
    for (const [name] of this.#watched) this.#counts[name] = 0;
    this.#base = this.#readBase();
  }

  /** Starts listening to the mirror and watching frames. Does nothing when already started. */
  start(): void {
    if (this.#stops.length > 0) return;
    this.#stops.push(this.#mirror.addDrainListener((drain) => this.#drains.record(drain)));
    for (const [name, signal] of this.#watched) {
      this.#stops.push(
        signal.subscribe(() => {
          this.#counts[name] = (this.#counts[name] ?? 0) + 1;
        }),
      );
    }
    this.#frames?.start();
  }

  /** Stops listening and watching. */
  stop(): void {
    for (const stop of this.#stops) stop();
    this.#stops = [];
    this.#frames?.stop();
  }

  /** Starts the measurement over: windows, totals, apply counts and dropped frames go to zero. */
  reset(): void {
    this.#drains.reset();
    this.#rates.reset();
    this.#frames?.reset();
    for (const [name] of this.#watched) this.#counts[name] = 0;
    this.#base = this.#readBase();
  }

  /** Records the counters now and returns what the last window shows. */
  snapshot(): StressSnapshot {
    const counters = this.#mirror.stats();
    const generated = this.#generated();
    this.#rates.push({
      generated,
      received: counters.changeSetsReceived,
      entries: counters.entriesReceived,
      applied: counters.entriesApplied,
    });
    const rates = this.#rates.rates();
    const drains = this.#drains.snapshot();
    const frames = this.#frames?.snapshot(this.#now());
    const heap = this.#heap();
    const entriesPerSec = rates.entries ?? 0;
    const appliedPerSec = rates.applied ?? 0;
    return {
      generatedPerSec: rates.generated ?? 0,
      receivedPerSec: rates.received ?? 0,
      entriesReceivedPerSec: entriesPerSec,
      appliedPerSec,
      mergeRatio: entriesPerSec > 0 ? appliedPerSec / entriesPerSec : 0,
      drainsPerSec: drains.drainsPerSec,
      drainP50Us: drains.p50Us,
      drainP99Us: drains.p99Us,
      drains: drains.drains,
      drainTotalUs: drains.totalMs * 1000,
      drainMaxUs: drains.maxUs,
      nsPerChangeSet: drains.nsPerChangeSet,
      droppedFrames: frames?.dropped ?? 0,
      droppedFramesRecent: frames?.droppedRecent ?? 0,
      longestFrameMs: frames?.longestMs ?? 0,
      frameMs: frames?.nominalMs ?? 0,
      heapMb: heap === null ? null : heap / (1024 * 1024),
      timerResolutionUs: this.#timerResolutionUs,
      generatedTotal: generated - this.#base.generated,
      receivedTotal: counters.changeSetsReceived - this.#base.received,
      appliedTotal: counters.entriesApplied - this.#base.applied,
      compactions: (counters.compactions ?? 0) - this.#base.compactions,
      applies: { ...this.#counts },
    };
  }

  #readBase(): { generated: number; received: number; applied: number; compactions: number } {
    const counters = this.#mirror.stats();
    return {
      generated: this.#generated(),
      received: counters.changeSetsReceived,
      applied: counters.entriesApplied,
      compactions: counters.compactions ?? 0,
    };
  }
}

/**
 * A duration in microseconds as the screen shows it: `"< 0.1 ms"` at or below the clock's step
 * (the browser cannot tell it from nothing), else microseconds or milliseconds.
 *
 * ```ts
 * formatDuration(0, 100);     // "< 0.1 ms"
 * formatDuration(450, 100);   // "450 µs"
 * formatDuration(2500, 100);  // "2.50 ms"
 * ```
 */
export function formatDuration(us: number, stepUs: number): string {
  if (!Number.isFinite(us) || us < 0) return "–";
  // Both to a tenth: a drain of one clock step is 0.1 ms x 1000 = 100.00000000000853 µs, and that is "under".
  const rounded = Math.round(us * 10) / 10;
  if (stepUs > 0 ? rounded <= Math.round(stepUs * 10) / 10 : rounded < 1) return `< ${formatStep(stepUs > 0 ? stepUs : 1)}`;
  return us >= 1000 ? `${(us / 1000).toFixed(us >= 10_000 ? 1 : 2)} ms` : `${Math.round(us)} µs`;
}

/** The clock's step as a unit-bearing number: `"0.1 ms"` for 100 µs, `"5 µs"` for 5 µs. */
export function formatStep(stepUs: number): string {
  // The measured step carries floating-point residue (99.99999403953552 for 100 µs).
  const us = Math.round(stepUs * 10) / 10;
  return us >= 100 ? `${us / 1000} ms` : `${us} µs`;
}

/** A rate or a count with thousands separators and no decimals: `10,001`. */
export function formatCount(value: number): string {
  return Number.isFinite(value) ? Math.round(value).toLocaleString("en-US") : "–";
}

/** The merge ratio as the screen says it: `"1 apply per 160 received"`, or `"nothing merged"`. */
export function formatMerge(ratio: number): string {
  if (!(ratio > 0)) return "–";
  if (ratio >= 0.95) return "nothing merged";
  const per = 1 / ratio;
  return `1 apply per ${per >= 10 ? Math.round(per).toLocaleString("en-US") : per.toFixed(1)} received`;
}

// ---- Browser glue: used by the page, never by the tests ---------------------------------------

/** `requestAnimationFrame` as a {@link FrameSource}. */
export function browserFrameSource(): FrameSource {
  return {
    request: (callback) => requestAnimationFrame(callback),
    cancel: (handle) => cancelAnimationFrame(handle),
  };
}

/**
 * Pauses `monitor` while the document is hidden (no frames come, and the gap is not a slow frame).
 * Returns the function that stops listening.
 */
export function pauseWhenHidden(monitor: FrameMonitor, doc: Pick<Document, "hidden" | "addEventListener" | "removeEventListener"> = document): () => void {
  const onChange = (): void => {
    if (doc.hidden) monitor.pause();
    else monitor.resume();
  };
  doc.addEventListener("visibilitychange", onChange);
  if (doc.hidden) monitor.pause();
  return () => doc.removeEventListener("visibilitychange", onChange);
}

/** The JS heap in use, in bytes, from Chrome's `performance.memory`; `null` in browsers that have none. */
export function browserHeapBytes(): number | null {
  const memory = (typeof performance === "object" ? (performance as Performance & { memory?: { usedJSHeapSize?: number } }).memory : undefined)?.usedJSHeapSize;
  return typeof memory === "number" && Number.isFinite(memory) ? memory : null;
}
