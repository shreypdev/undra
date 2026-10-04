import { expect } from "vitest";
import {
  ALL_SIGNALS,
  ChangeOp,
  type PatchOp,
  UndraReader,
  type UndraCore,
  decodeLazyInvalidated,
  decodeLazyValue,
  decodePatch,
} from "@undra/runtime";
import { FeedQueryHandle, type Item, ItemCodec, Library, UndraIds, touchFeed } from "@playground/core";
import { PageCallLog } from "./page-calls.js";
import { RawStore, type SignalUpdate, tapEntries } from "./raw-store.js";
import { sleep, step, waitFor } from "./wait.js";

/*
 * The steps of S32 (paged queries and lazy lists, ADR-043), written once for a core in any mode. `contract-tests/ts/test/s32-*.test.ts`
 * runs them on `wasm-main` (the scenario); `paging-worker.test.ts` runs them on `wasm-worker`, where page calls are asynchronous and the
 * change-sets of one call and the next can reach the mirror in one drain. A step that needs the core to answer in the caller's turn
 * (reply versions, a frame held by not yielding) says so and runs only when `sync` is true.
 */

const LibraryIds = UndraIds.Objects.Library;
const PAGE_ROWS = 50;

/** The entries of `signalId` in `entries`. */
const of = (entries: readonly SignalUpdate[], signalId: number): SignalUpdate[] => entries.filter((e) => e.signalId === signalId);

/** The operations of a keyed patch entry of `Item`s. */
function itemOps(entry: SignalUpdate): PatchOp<Item>[] {
  expect(entry.op, "the entry is a keyed patch").toBe(ChangeOp.KeyedPatch);
  const r = new UndraReader(entry.value);
  const ops = decodePatch(r, ItemCodec);
  r.finish();
  return ops;
}

/** Reads the rows `from` up to `to` of a list as a view does when it renders them, and returns what is there. */
function render(list: { get(index: number): Item | undefined }, from: number, to: number): Array<Item | undefined> {
  const rows: Array<Item | undefined> = [];
  for (let i = from; i < to; i++) rows.push(list.get(i));
  return rows;
}

/** A library whose `books` and `evens` this thread has not read yet, and the page calls its core makes. */
async function openLibrary(core: UndraCore): Promise<Library> {
  return Library.create(core);
}

export async function pagingSteps(core: UndraCore, mode: { readonly sync: boolean }): Promise<void> {
  const pages = PageCallLog.watch(core);
  const library = await openLibrary(core);

  await step("1. a lazy list is not sent whole", async () => {
    expect(library.books.length.peek(), "the length is there after the first drain").toBe(10_000);
    // The first change-set of a second store of the same type: 20 bytes for `books`, not 10,000 rows.
    const raw = await RawStore.open(core, LibraryIds);
    const initial = raw.take();
    const books = of(initial, 0);
    expect(books).toHaveLength(1);
    expect(books[0]?.op).toBe(ChangeOp.FullValue);
    expect(books[0]?.value, "a LazyValue: handle u64, len u32, version u64").toHaveLength(20);
    expect(decodeLazyValue(books[0]?.value as Uint8Array).len).toBe(10_000);
    expect(initial.reduce((n, e) => n + e.value.length, 0), "the whole first change-set is far smaller than the rows").toBeLessThan(2_048);
    raw.close();

    const before = pages.count;
    expect(library.books.get(0), "a row is nothing until its page arrives").toBeUndefined();
    const first = await waitFor("row 0", () => library.books.get(0));
    expect(first.id).toBe(1);
    expect(library.books.get(9_999), "the last row is nothing until its page arrives").toBeUndefined();
    const last = await waitFor("row 9,999", () => library.books.get(9_999));
    expect(last.id).toBe(10_000);
    const afterReads = pages.count;
    expect(afterReads - before, "pages 0 and 1, then 198 and 199 (there is no page 200)").toBe(4);

    for (const outside of [10_000, 10_001, -1, 1e9, 0.5]) expect(library.books.get(outside), `index ${outside}`).toBeUndefined();
    await sleep(30);
    expect(pages.count, "an index outside the list requests nothing").toBe(afterReads);
  });

  await step("2. a page is requested once, with one page of prefetch each side", async () => {
    const other = await openLibrary(core);
    pages.take();
    const before = pages.count;
    expect(other.books.get(120)).toBeUndefined();
    await waitFor("row 120", () => other.books.get(120));
    await sleep(30);
    const made = pages.take();
    expect(pages.count - before, "pages 2, 1 and 3").toBe(3);
    expect(made.map((c) => c.offset).sort((a, b) => a - b)).toEqual([PAGE_ROWS, 2 * PAGE_ROWS, 3 * PAGE_ROWS]);
    expect(made.every((c) => c.limit === PAGE_ROWS)).toBe(true);

    // Any row of page 2 reads at once and asks for nothing: its neighbours are loaded.
    for (const index of [100, 120, 149]) expect(other.books.get(index)?.id, `row ${index}`).toBe(index + 1);
    await sleep(30);
    expect(pages.count - before, "reading the rows of the loaded pages makes no call").toBe(3);
    // A row of page 1 or 3 is read at once too, and asks for the page beyond it (0, 4): the read keeps the prefetch one page ahead.
    expect(other.books.get(50)?.id).toBe(51);
    expect(other.books.get(199)?.id).toBe(200);
    await waitFor("the pages beyond", () => pages.count - before === 5);
    await sleep(30);
    expect(pages.count - before, "pages 0 and 4, once").toBe(5);
    expect(other.books.get(0)?.id).toBe(1);
    expect(other.books.get(249)?.id).toBe(250);

    if (mode.sync) {
      // Every reply carries the version of the list it was read at, which is the list's version.
      expect(made.length).toBe(3);
      expect(made.every((c) => c.version === other.books.version), "the version of the replies is the list's").toBe(true);
    }
    other.close();
  });

  await step("3. a change is one 12-byte entry and the host re-pages only its window", async () => {
    // The view shows rows 100 to 149 and has read around them: pages 1, 2 and 3 are the window.
    render(library.books, 100, 150);
    await waitFor("the window", () => render(library.books, 100, 150).every((row) => row !== undefined));
    render(library.books, 60, 61);
    render(library.books, 160, 161);
    await waitFor("its neighbours", () => library.books.get(60) !== undefined && library.books.get(160) !== undefined);
    await sleep(30);
    const entries = tapEntries(library);
    const changeSets = core.mirror.changeSets;
    const versionBefore = library.books.version;
    const callsBefore = pages.count;
    const shown = library.books.get(125);

    await library.addRows(1);
    const books = of(entries, 0);
    expect(books, "one entry for books").toHaveLength(1);
    expect(books[0]?.op, "an invalidation, op 2").toBe(ChangeOp.LazyInvalidated);
    expect(books[0]?.value, "len u32 and version u64").toHaveLength(12);
    const invalidated = decodeLazyInvalidated(books[0]?.value as Uint8Array);
    expect(invalidated.len).toBe(10_001);
    expect(invalidated.version).toBeGreaterThan(versionBefore);
    expect(core.mirror.changeSets - changeSets, "one change-set").toBe(1);
    expect(library.books.length.peek()).toBe(10_001);
    expect(library.books.get(125), "rows on screen stay readable").toEqual(shown);

    await sleep(60);
    // The window is the pages read since the last change: 0 and 199 (step 1) and 1, 2 and 3 (the view): five pages, not 200.
    expect(pages.count - callsBefore, "the window is asked again, not 200").toBe(5);
    expect(pages.count - callsBefore).toBeLessThan(200);
    expect(library.books.get(125)?.id).toBe(126);

    // rename(121, "x"): row 121 reads "x" with version 1 after the re-page.
    await library.rename(121, "x");
    const renamed = await waitFor("row 121 to read x", () => {
      const row = library.books.get(121);
      return row?.label === "x" ? row : undefined;
    });
    expect(renamed.version).toBe(1);
    expect(renamed.id).toBe(122);

    // remove_at(0): one row fewer, and the first is now the second.
    await library.removeAt(0);
    expect(library.books.length.peek()).toBe(10_000);
    const firstRow = await waitFor("row 0 to be the second row", () => {
      const row = library.books.get(0);
      return row?.id === 2 ? row : undefined;
    });
    expect(firstRow.id).toBe(2);
  });

  if (mode.sync) {
    await step("4a. a drain folds three invalidations into one delivery, with the last length and version", async () => {
      // The calls are made in one turn of the event loop, so the mirror holds their three change-sets until it drains: the
      // frame is held by not yielding. (A reply drains the mirror before its caller resumes, so awaiting between them would not.)
      const entries = tapEntries(library);
      const changeSets = core.mirror.changeSets;
      const calls = [library.addRows(1), library.addRows(1), library.addRows(1)];
      await Promise.all(calls);
      expect(core.mirror.changeSets - changeSets, "three change-sets").toBe(3);
      const books = of(entries, 0);
      expect(books, "folded into one delivery").toHaveLength(1);
      expect(books[0]?.op).toBe(ChangeOp.LazyInvalidated);
      const folded = decodeLazyInvalidated(books[0]?.value as Uint8Array);
      expect(folded.len, "the last length").toBe(10_003);
      expect(folded.version, "the last version").toBe(library.books.version);
      expect(library.books.length.peek()).toBe(10_003);
    });
  }

  await step("4b. a store's first value and a change that follows it in one drain: the page server is not lost", async () => {
    const handle = await core.construct(LibraryIds.typeId, LibraryIds.new, new Uint8Array(0));
    const Class = Library as unknown as new (core: UndraCore, handle: bigint) => Library;
    const second = new Class(core, handle);
    let observed: Promise<void> | undefined;
    let added: Promise<void> | undefined;
    if (mode.sync) {
      // The core answers in the caller's turn, so the second store's first drain is as late as this thread makes it: from a
      // subscriber (which runs inside the drain of the first library's change) it is observed and `add_rows` is called, and the
      // drain's next round finds the store's value (op 0) and its invalidation (op 2) together, as the frame-held run of the
      // native columns does.
      const stop = library.books.length.subscribe(() => {
        stop();
        observed = core.observe(handle, ALL_SIGNALS, true);
        added = second.addRows(1);
      });
      await library.addRows(1);
      await Promise.all([observed, added]);
    } else {
      // Over a worker the replies come later and each is its own drain: observed and changed at once, the store ends the same.
      observed = core.observe(handle, ALL_SIGNALS, true);
      added = second.addRows(1);
      await Promise.all([observed, added]);
    }
    await waitFor("the length", () => second.books.length.peek() === 10_001);
    expect(second.books.length.peek()).toBe(10_001);
    const row = await waitFor("row 0 of the second library", () => second.books.get(0));
    expect(row.id).toBe(1);
    second.close();
  });

  await step("5. a view pages through the derived index", async () => {
    const view = await openLibrary(core);
    expect(view.evens.length.peek()).toBe(10);
    const first = await waitFor("evens[0]", () => view.evens.get(0));
    expect(first.id).toBe(2);
    const ninth = await waitFor("evens[9]", () => view.evens.get(9));
    expect(ninth.id).toBe(20);

    await view.addRows(2);
    expect(view.evens.length.peek()).toBe(11);
    const tenth = await waitFor("evens[10]", () => view.evens.get(10));
    expect(tenth.id).toBe(10_002);

    await view.dropSource(20);
    expect(view.evens.length.peek()).toBe(1);
    const only = await waitFor("evens[0] after drop_source", () => {
      const row = view.evens.get(0);
      return row?.id === 10_002 ? row : undefined;
    });
    expect(only.id).toBe(10_002);
    view.close();
  });

  await step("6. an infinite query grows a page at a time", async () => {
    const feed = await FeedQueryHandle.create(false, core);
    await waitFor("the first page", () => feed.data.peek().length === 50);
    expect(feed.hasNextPage.peek()).toBe(true);
    expect(feed.data.peek().map((row) => row.id)).toEqual(Array.from({ length: 50 }, (_, i) => i + 1));

    const entries = tapEntries(feed);
    const fetching: boolean[] = [];
    feed.fetchingNextPage.subscribe((value) => fetching.push(value));
    await feed.fetchNextPage();
    await waitFor("the second page", () => feed.data.peek().length === 100);
    await waitFor("the fetch to end", () => !feed.fetchingNextPage.peek());
    expect(fetching, "fetchingNextPage went true, then false").toEqual([true, false]);
    expect(feed.data.peek().map((row) => row.id)).toEqual(Array.from({ length: 100 }, (_, i) => i + 1));
    const grown = of(entries, 0);
    expect(grown, "one entry for data").toHaveLength(1);
    const inserts = itemOps(grown[0] as SignalUpdate);
    expect(inserts, "a keyed patch of 50 inserts").toHaveLength(50);
    expect(inserts.every((op) => op.op === "insert")).toBe(true);
    expect(grown[0]?.value.length, "under 5 KB, not a full value").toBeLessThan(5_000);

    // A second handle with the same parameter shares the entry: it has the 100 rows at once, and fetches nothing.
    const shared = await FeedQueryHandle.create(false, core);
    expect(shared.data.peek()).toHaveLength(100);
    await sleep(60);
    expect(shared.fetching.peek()).toBe(false);
    expect(shared.data.peek()).toHaveLength(100);

    // A refetch finds what changed: the even rows now show revision 7, and only they are patched.
    await touchFeed(7, core);
    entries.length = 0;
    await feed.refetch();
    await waitFor("the even rows to show revision 7", () => feed.data.peek().length === 100 && feed.data.peek().every((row) => row.version === (row.id % 2 === 0 ? 7 : 0)));
    await waitFor("the refetch to end", () => !feed.fetching.peek());
    const refreshed = of(entries, 0);
    expect(refreshed, "one entry for data").toHaveLength(1);
    const updates = itemOps(refreshed[0] as SignalUpdate);
    expect(updates.length).toBeGreaterThan(0);
    expect(updates.length, "at most 50 updates").toBeLessThanOrEqual(50);
    expect(updates.every((op) => op.op === "update")).toBe(true);
    // The refetch reaches both handles in one transaction but as one change-set per store, which a core in a worker posts
    // one after the other: the shared handle is waited for on its own, then holds the same rows as the first.
    await waitFor("the shared entry to follow", () => shared.data.peek().length === 100 && shared.data.peek().every((row) => row.version === (row.id % 2 === 0 ? 7 : 0)));
    expect(shared.data.peek(), "the shared entry follows: the same rows as the first").toEqual(feed.data.peek());

    // The even-only feed is its own entry: 2, 4, .., 100.
    const evens = await FeedQueryHandle.create(true, core);
    await waitFor("the even-only page", () => evens.data.peek().length === 50);
    expect(evens.data.peek().map((row) => row.id)).toEqual(Array.from({ length: 50 }, (_, i) => 2 * (i + 1)));
    for (const handle of [feed, shared, evens]) handle.close();
  });

  library.close();
  pages.stop();
}
