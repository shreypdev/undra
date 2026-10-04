/*
 * The live numbers the landing page shows next to the embedded playground, and how they are
 * measured. In embed mode (`?embed=1`, inside an iframe) the page posts
 *
 *   { type: "undra-stats", changeSetsPerSec, applyP50Us, applyP99Us, timerResolutionUs,
 *     applyBatchDrains, applyBatchUs, applyMaxUs, ...}
 *
 * to its parent every 500 ms. The fields above are the base shape and always there; the
 * stress screen (`?screen=stress`) adds the optional fields of {@link StatsMessage}, so a consumer
 * that knows only the base keeps working.
 *
 * WHAT THE APPLY TIME MEASURES. The runtime's mirror reports every drain (`Mirror.addDrainListener`,
 * ADR-031 decision 5): how many change-sets it consumed, how many entries it applied after merging
 * them, and how long it took, timed with `performance.now()`. A drain is one application of the
 * mirror's queue on the main thread: decoding the entries into the stores' signals (the generated
 * `_apply`) and notifying their subscribers. It does not include the wasm call that produced the
 * change-set, the wait for the frame, or the React re-render the subscribers schedule. Change-sets
 * of every store in the core are measured, not only the ones the visible screen draws, and the
 * loading of the stores (before the first message) is not.
 *
 * `applyP50Us` and `applyP99Us` are nearest-rank percentiles, in microseconds, of the DURATIONS OF
 * DRAINS over the last two seconds (at most the most recent 50,000): the time the mirror held the
 * main thread each time it ran. Since ADR-031 a drain merges every change-set that arrived since
 * the previous one, so under a firehose one drain stands for hundreds of change-sets, and the
 * stress screen's `applyNsPerChangeSet` (all drain time over all change-sets) says what that costs
 * per change-set. When the core is quiet, one drain is one change-set and the two agree.
 * `changeSetsPerSec` is the number of change-sets the mirror consumed in the window divided by its
 * span (or by the time since the stats started, when that is shorter); it can be fractional. All
 * are 0 while the window holds no drain.
 *
 * Two limits to state wherever the numbers are shown. First, the resolution of `performance.now()`
 * is set by the browser, not by this code: about 100 microseconds in Chrome and 1 millisecond in
 * Firefox and Safari for a page that is not cross-origin isolated (5 microseconds in Chrome when it
 * is). A drain that takes less than the step reads as 0 or as one step of it. `timerResolutionUs` is
 * the step this page measured at start-up: show a single time at or below it as "under <step>", not
 * as a precise value. A batch is finer: the window's drain times added up (`applyBatchUs`, over
 * `applyBatchDrains` drains) carry a reading error of under a step each, which evens out across the
 * batch, so their average is a real number once the batch spans a few steps. Second, it is the mirror's apply time, not the latency of a write: it says nothing
 * about the core call, which the page does not time.
 */

import { DrainWindow, type DrainWindowSnapshot, type MirrorLike, type StressSnapshot } from "./stress-stats";

/** The kinds of update the stress generator makes. */
export type StressModeName = "firehose" | "progress";

/**
 * The message the page posts to its parent: what the landing page's counters read. The fields up
 * to `applyMaxUs` are the base shape; the rest are present on the stress screen only.
 */
export interface StatsMessage {
  readonly type: "undra-stats";
  /** Change-sets per second the mirror consumed (on the stress screen: received), over the last two seconds. May be fractional. */
  readonly changeSetsPerSec: number;
  /** Median duration of one drain, in microseconds; 0 when there are no samples. */
  readonly applyP50Us: number;
  /** 99th-percentile duration of one drain, in microseconds; 0 when there are no samples. */
  readonly applyP99Us: number;
  /**
   * The smallest step of this browser's `performance.now()`, in microseconds, observed when the
   * stats started: durations at or below it are not resolved. 0 when no step could be observed.
   */
  readonly timerResolutionUs: number;
  /**
   * The batch: every drain of the window (`applyBatchDrains` of them) and their durations added up
   * (`applyBatchUs`, microseconds). Each duration is read to the clock's step, too high or too low by
   * less than a step, and over a batch those errors even out: `applyBatchUs / applyBatchDrains` is the
   * average drain, resolved below the step once the batch spans a few steps. 0 with no drains.
   */
  readonly applyBatchDrains: number;
  readonly applyBatchUs: number;
  /** The longest single drain of the window, in microseconds: one reading, good to a step. 0 with no drains. */
  readonly applyMaxUs: number;
  /** Stress: updates per second the core's generator committed. */
  readonly generatedPerSec?: number;
  /** Stress: entries per second the change-sets carried. */
  readonly entriesReceivedPerSec?: number;
  /** Stress: entries per second applied to the stores, after the mirror merged them. */
  readonly entriesAppliedPerSec?: number;
  /** Stress: applied over received entries; 1 is nothing merged, 0.01 a hundred entries per apply. */
  readonly mergeRatio?: number;
  /** Stress: drains per second. */
  readonly drainsPerSec?: number;
  /** Stress: all drain time over all change-sets in the window, in nanoseconds. */
  readonly applyNsPerChangeSet?: number;
  /** Stress: frames dropped since the generator was started. */
  readonly droppedFrames?: number;
  /** Stress: frames dropped in the last five seconds. */
  readonly droppedFramesRecent?: number;
  /** Stress: the longest gap between two frames in the last five seconds, in milliseconds. */
  readonly longestFrameMs?: number;
  /** Stress: the JS heap in megabytes. Chrome only: absent elsewhere. */
  readonly heapMb?: number;
  /** Stress: how many times the merged `value` signal notified its subscribers since the start. */
  readonly valueApplies?: number;
  /** Stress: the same for the `no_coalesce` `progress` signal. */
  readonly progressApplies?: number;
  /** Stress: what the generator makes. */
  readonly mode?: StressModeName;
  /** Stress: the rate it was asked for, in updates per second. */
  readonly targetRate?: number;
  /** Stress: whether the generator is running. */
  readonly running?: boolean;
  /** Stress: where the core runs. */
  readonly runtime?: "wasm-main";
}

/** Rounds to a tenth, so a clock's floating-point residue (`100.00000000000142`) does not reach the page. */
const tenth = (value: number): number => Math.round(value * 10) / 10;

/** Rounds to four decimals: the merge ratio is a small fraction. */
const fraction = (value: number): number => Math.round(value * 10_000) / 10_000;

/** The base message for a window of drains and the clock's resolution: numbers rounded to a tenth. */
export function toStatsMessage(
  window: Pick<DrainWindowSnapshot, "changeSetsPerSec" | "p50Us" | "p99Us" | "drains" | "totalMs" | "maxUs">,
  timerResolutionUs: number,
): StatsMessage {
  return {
    type: "undra-stats",
    changeSetsPerSec: tenth(window.changeSetsPerSec),
    applyP50Us: tenth(window.p50Us),
    applyP99Us: tenth(window.p99Us),
    timerResolutionUs: tenth(timerResolutionUs),
    applyBatchDrains: window.drains,
    applyBatchUs: tenth(window.totalMs * 1000),
    applyMaxUs: tenth(window.maxUs),
  };
}

/** What the stress screen adds to a snapshot to make its message. */
export interface StressContext {
  readonly mode: StressModeName;
  readonly targetRate: number;
  readonly running: boolean;
}

/**
 * The message of the stress screen: the base fields from the same snapshot the tiles show (so the
 * two always agree), and the stress fields. `heapMb` is left out where the browser has no heap number.
 */
export function toStressMessage(snapshot: StressSnapshot, context: StressContext): StatsMessage {
  return {
    type: "undra-stats",
    changeSetsPerSec: tenth(snapshot.receivedPerSec),
    applyP50Us: tenth(snapshot.drainP50Us),
    applyP99Us: tenth(snapshot.drainP99Us),
    timerResolutionUs: tenth(snapshot.timerResolutionUs),
    applyBatchDrains: snapshot.drains,
    applyBatchUs: tenth(snapshot.drainTotalUs),
    applyMaxUs: tenth(snapshot.drainMaxUs),
    generatedPerSec: tenth(snapshot.generatedPerSec),
    entriesReceivedPerSec: tenth(snapshot.entriesReceivedPerSec),
    entriesAppliedPerSec: tenth(snapshot.appliedPerSec),
    mergeRatio: fraction(snapshot.mergeRatio),
    drainsPerSec: tenth(snapshot.drainsPerSec),
    applyNsPerChangeSet: Math.round(snapshot.nsPerChangeSet),
    droppedFrames: snapshot.droppedFrames,
    droppedFramesRecent: snapshot.droppedFramesRecent,
    longestFrameMs: tenth(snapshot.longestFrameMs),
    ...(snapshot.heapMb === null ? {} : { heapMb: tenth(snapshot.heapMb) }),
    valueApplies: snapshot.applies["value"] ?? 0,
    progressApplies: snapshot.applies["progress"] ?? 0,
    mode: context.mode,
    targetRate: context.targetRate,
    running: context.running,
    runtime: "wasm-main",
  };
}

/** Consecutive readings of the clock that {@link timerResolutionUs} spins through, at most. */
const MAX_SPIN_READS = 500_000;

/**
 * The smallest step of `now()` (milliseconds), in microseconds, seen over a few consecutive
 * changes of its reading: what the browser's clock clamp leaves of `performance.now()`. Spins for
 * a fraction of a millisecond in Chrome and a few in Firefox and Safari, and gives up after
 * {@link MAX_SPIN_READS} readings; returns 0 when it saw no step (a frozen clock).
 */
export function timerResolutionUs(now: () => number = () => performance.now(), steps = 3): number {
  let smallest = Number.POSITIVE_INFINITY;
  let last = now();
  for (let reads = 0, seen = 0; seen < steps && reads < MAX_SPIN_READS; reads++) {
    const t = now();
    if (t > last) {
      smallest = Math.min(smallest, t - last);
      last = t;
      seen++;
    }
  }
  return Number.isFinite(smallest) ? smallest * 1000 : 0;
}

/** Where the page posts its stats: the parent window. */
export interface MessageTarget {
  postMessage(message: unknown, targetOrigin: string): void;
}

/** How often the stats are posted. */
export const STATS_INTERVAL_MS = 500;

/**
 * The line to the embedding page. Addressed to `targetOrigin` (the embedding page's origin; the
 * landing page is served from this page's own origin, so `location.origin` is exact and a frame
 * from anywhere else never receives a message). The stress screen {@link claim}s it, so while that
 * screen is open it is the only sender and the base poster stays quiet.
 */
export class StatsChannel {
  readonly #parent: MessageTarget;
  readonly #origin: string;
  #claims = 0;

  /** @param parent The embedding window. @param targetOrigin Its origin; `"*"` when not known. */
  constructor(parent: MessageTarget, targetOrigin = "*") {
    this.#parent = parent;
    this.#origin = targetOrigin;
  }

  /** Posts one message to the parent. */
  post(message: StatsMessage): void {
    this.#parent.postMessage(message, this.#origin);
  }

  /** Becomes the sender of the stats until the returned function is called. Claims nest. */
  claim(): () => void {
    this.#claims++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#claims--;
    };
  }

  /** Whether a screen has claimed the channel. */
  get claimed(): boolean {
    return this.#claims > 0;
  }
}

/**
 * Starts measuring `mirror`'s drains and posts a base {@link StatsMessage} to `channel` every
 * {@link STATS_INTERVAL_MS}, unless a screen has claimed the channel (the stress screen posts its
 * own, richer message from the numbers it shows). Call it once the stores are created, so their
 * loading is not measured. Returns a function that stops both the posting and the measuring.
 */
export function startStatsPoster(mirror: MirrorLike, channel: StatsChannel, now: () => number = () => performance.now()): () => void {
  const resolutionUs = timerResolutionUs(now);
  const drains = new DrainWindow({ now });
  const stopListening = mirror.addDrainListener((drain) => drains.record(drain));
  const timer = setInterval(() => {
    if (!channel.claimed) channel.post(toStatsMessage(drains.snapshot(), resolutionUs));
  }, STATS_INTERVAL_MS);
  return () => {
    clearInterval(timer);
    stopListening();
  };
}
