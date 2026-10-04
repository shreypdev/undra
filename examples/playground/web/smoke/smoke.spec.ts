import { type Page, expect, test } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

// The playground's web app, production build, real Chromium: one pass through the four views.
// Screenshots go to examples/playground/.proof/web-<view>.png; what the page logged goes to the
// test output (`npm run smoke | tee ../.proof/web-smoke.log`).

const PROOF = fileURLToPath(new URL("../../.proof/", import.meta.url));

/** The median of a list of numbers. */
const median = (values: number[]): number => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)] ?? Number.NaN;

/** The milliseconds in a readout such as `update: 0.42 ms`. */
async function readoutMs(page: Page): Promise<number> {
  const text = (await page.getByTestId("biglist-timing").textContent()) ?? "";
  const match = /([\d.]+) ms/.exec(text);
  if (match === null) throw new Error(`no timing in "${text}"`);
  return Number(match[1]);
}

test("the playground runs: todos, counter, 10k list, remote", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const logged: string[] = [];
  const problems: string[] = [];
  page.on("console", (message) => {
    logged.push(`[console.${message.type()}] ${message.text()}`);
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => {
    logged.push(`[pageerror] ${error.message}`);
    problems.push(error.message);
  });
  const shot = async (view: string): Promise<void> => {
    await page.screenshot({ path: `${PROOF}web-${view}.png`, fullPage: true });
  };

  await page.goto("/");
  await expect(page.getByTestId("todo-input")).toBeVisible();
  const startedAt = await page.evaluate(() => performance.now());
  console.log(`startup: the first view was interactive ${startedAt.toFixed(0)} ms after navigation started (fetch, compile and instantiate the 538 KB core, create the stores, render)`);
  await expect(page.getByRole("heading", { name: "Undra playground" })).toBeVisible();

  // ---- Todos -------------------------------------------------------------------------------
  await test.step("todos: a refused add shows the typed error", async () => {
    await page.getByTestId("todo-add").click();
    await expect(page.getByTestId("todo-error")).toHaveText("the title cannot be empty");
    await expect(page.getByTestId("todo-item")).toHaveCount(0);
  });

  await test.step("todos: add two, toggle one, remaining follows", async () => {
    await page.getByTestId("todo-input").fill("Read the spec");
    await page.getByTestId("todo-add").click();
    await page.getByTestId("todo-input").fill("Write the smoke test");
    await page.getByTestId("todo-add").click();
    await expect(page.getByTestId("todo-item")).toHaveCount(2);
    await expect(page.getByTestId("todo-error")).toHaveCount(0);
    await expect(page.getByTestId("remaining")).toHaveText("2 left");
    await page.getByTestId("todo-toggle").first().check();
    await expect(page.getByTestId("remaining")).toHaveText("1 left");
    await page.getByTestId("filter-active").click();
    await expect(page.getByTestId("todo-item")).toHaveCount(1);
    await page.getByTestId("filter-all").click();
    await expect(page.getByTestId("todo-item")).toHaveCount(2);
    await shot("todos");
  });

  await test.step("todos: the generic code of the core: newest, a selection store, draft (ADR-058)", async () => {
    // `newest("Todo", rows)`: the core says which row is the newest, the page only shows it.
    await expect(page.getByTestId("todo-latest")).toHaveText("Latest: Write the smoke test");
    await expect(page.getByTestId("todo-selected-count")).toHaveText("0 selected");
    // Ticking is `TodoSelection.toggle(row)`: a keyed insert into the selection store's `rows`, one count.
    await page.getByTestId("todo-select").first().check();
    await expect(page.getByTestId("todo-selected-count")).toHaveText("1 selected");
    await expect(page.getByTestId("todo-picked")).toContainText("Read the spec");
    // `draft("Todo", title)`: a to-do the core made and did not store: ticked, not in the list.
    await page.getByTestId("todo-input").fill("A drafted to-do");
    await page.getByTestId("todo-new-draft").click();
    await expect(page.getByTestId("todo-selected-count")).toHaveText("2 selected");
    await expect(page.getByTestId("todo-picked")).toContainText("A drafted to-do");
    await expect(page.getByTestId("todo-item")).toHaveCount(2);
    await page.getByTestId("todo-select-clear").click();
    await expect(page.getByTestId("todo-selected-count")).toHaveText("0 selected");
    // Select all, then remove what is selected: the selection store drives a command on the other store.
    await page.getByTestId("todo-select-all").click();
    await expect(page.getByTestId("todo-selected-count")).toHaveText("2 selected");
    await page.getByTestId("todo-remove-selected").click();
    await expect(page.getByTestId("todo-item")).toHaveCount(0);
    await expect(page.getByTestId("todo-selected-count")).toHaveText("0 selected");
    await expect(page.getByTestId("todo-latest")).toHaveText("Nothing yet");
    // Back to two items for the views below.
    for (const title of ["Read the spec", "Write the smoke test"]) {
      await page.getByTestId("todo-input").fill(title);
      await page.getByTestId("todo-add").click();
    }
    await expect(page.getByTestId("todo-item")).toHaveCount(2);
  });

  // ---- Counter -----------------------------------------------------------------------------
  await test.step("counter: increment and decrement, parity from the core", async () => {
    await page.getByTestId("tab-counter").click();
    await expect(page.getByTestId("counter-value")).toHaveText("0");
    await expect(page.getByTestId("counter-parity")).toHaveText("even");
    await page.getByTestId("counter-inc").click();
    await expect(page.getByTestId("counter-value")).toHaveText("1");
    await expect(page.getByTestId("counter-parity")).toHaveText("odd");
    await page.getByTestId("counter-inc").click();
    await expect(page.getByTestId("counter-value")).toHaveText("2");
    await expect(page.getByTestId("counter-parity")).toHaveText("even");
    await page.getByTestId("counter-dec").click();
    await expect(page.getByTestId("counter-value")).toHaveText("1");
    await expect(page.getByTestId("counter-parity")).toHaveText("odd");
    await expect(page.getByTestId("counter-changes")).toHaveText("3 changes");
    await shot("counter");
  });

  // ---- 10k list ----------------------------------------------------------------------------
  await test.step("10k list: windowed, one-row patches, a stream of updates", async () => {
    await page.getByTestId("tab-biglist").click();
    await expect(page.getByTestId("biglist-count")).toHaveText("10000");
    const drawn = await page.getByTestId("biglist-row").count();
    expect(drawn, "only the visible rows are in the DOM").toBeLessThan(40);
    expect(drawn).toBeGreaterThan(8);
    await expect(page.getByTestId("biglist-row").first()).toContainText("Item 1");

    await page.getByTestId("biglist-insert-top").click();
    await expect(page.getByTestId("biglist-count")).toHaveText("10001");
    await expect(page.getByTestId("biglist-row").first()).toContainText("Inserted at the top");

    // How long one operation takes, end to end (call, core, change-set, applied to the list): the median of 25.
    const samples: number[] = [];
    for (let i = 0; i < 25; i++) {
      await page.getByTestId("biglist-update").click();
      await expect(page.getByTestId("biglist-timing")).toContainText("update:");
      samples.push(await readoutMs(page));
    }
    console.log(`10k list: update round trip, median of 25 clicks: ${median(samples).toFixed(2)} ms (samples: ${samples.join(", ")})`);

    const versionsBefore = await page.locator(".row-version.flash").count();
    // While the stream runs, watch the frames: a second of requestAnimationFrame timestamps.
    const frames = page.evaluate(
      () =>
        new Promise<number[]>((resolve) => {
          const stamps: number[] = [];
          const tick = (now: number): void => {
            stamps.push(now);
            if (now - (stamps[0] ?? now) < 1_000) requestAnimationFrame(tick);
            else resolve(stamps);
          };
          requestAnimationFrame(tick);
        }),
    );
    await page.getByTestId("biglist-stream").check();
    const stamps = await frames;
    await page.getByTestId("biglist-stream").uncheck();
    const gaps = stamps.slice(1).map((stamp, i) => stamp - (stamps[i] as number));
    console.log(`10k list: streaming ten updates a second: ${stamps.length} frames in one second, longest gap between frames ${Math.max(...gaps).toFixed(1)} ms, median ${median(gaps).toFixed(1)} ms`);
    const streamed = await page.locator(".row-version.flash").count();
    console.log(`10k list: rows with a version above 0 after the update clicks: ${versionsBefore}; after a second of streaming: ${streamed}`);
    expect(streamed, "rows changed while streaming").toBeGreaterThan(versionsBefore);
    await expect(page.getByTestId("biglist-count")).toHaveText("10001");

    await page.getByTestId("biglist-remove").click();
    await expect(page.getByTestId("biglist-count")).toHaveText("10000");
    await shot("biglist");
  });

  // ---- Remote ------------------------------------------------------------------------------
  await test.step("remote: the seeded items, an optimistic add, then the server's", async () => {
    await page.getByTestId("tab-remote").click();
    await expect(page.getByTestId("remote-item")).toHaveCount(3);
    await expect(page.getByTestId("remote-status")).toHaveText("success");
    await expect(page.getByTestId("remote-fetching")).toHaveCount(0);
    await expect(page.getByTestId("remote-item").first()).toContainText("Buy milk");

    await page.getByTestId("remote-input").fill("Ship the playground");
    await page.getByTestId("remote-add").click();
    // The server answers after 300 ms; the placeholder is there before it does.
    await expect(page.getByTestId("remote-item")).toHaveCount(4, { timeout: 250 });
    await expect(page.getByTestId("remote-pending")).toHaveCount(1, { timeout: 250 });
    await expect(page.getByTestId("remote-pending")).toHaveCount(0);
    await expect(page.getByTestId("remote-item")).toHaveCount(4);
    await expect(page.getByTestId("remote-item").last()).toContainText("Ship the playground");

    // The optimistic change is the core's own write, so it reaches the page at the next frame (ADR-031): click, then wait for it.
    await page.getByTestId("remote-toggle").first().click();
    await expect(page.getByTestId("remote-toggle").first()).toBeChecked();
    await expect(page.getByTestId("remote-saving")).toHaveCount(0); // the PATCH was answered

    await page.getByTestId("remote-refresh").click();
    await expect(page.getByTestId("remote-fetching")).toBeVisible();
    await expect(page.getByTestId("remote-fetching")).toHaveCount(0);
    await expect(page.getByTestId("remote-status")).toHaveText("success");
    await expect(page.getByTestId("remote-toggle").first()).toBeChecked();
    await expect(page.getByTestId("remote-item")).toHaveCount(4);
  });

  await test.step("remote: offline, the add waits; online, it is replayed", async () => {
    await page.getByTestId("remote-offline").check();
    await page.getByTestId("remote-input").fill("Written offline");
    await page.getByTestId("remote-add").click();
    await expect(page.getByTestId("remote-item")).toHaveCount(5);
    await expect(page.getByTestId("remote-pending")).toHaveText("waiting for the network");
    // It stays queued: give the server (which is down) time to be asked again, and see that the item is still only a placeholder.
    await page.waitForTimeout(1_000);
    await expect(page.getByTestId("remote-item")).toHaveCount(5);
    await expect(page.getByTestId("remote-pending")).toHaveCount(1);
    await shot("remote-offline");

    await page.getByTestId("remote-offline").uncheck();
    await expect(page.getByTestId("remote-pending")).toHaveCount(0);
    await expect(page.getByTestId("remote-item")).toHaveCount(5);
    await expect(page.getByTestId("remote-item").last()).toContainText("Written offline");
    await expect(page.getByTestId("remote-status")).toHaveText("success");
    await shot("remote");
  });

  // A dark-mode look at the same page, for the record.
  await page.emulateMedia({ colorScheme: "dark" });
  await page.getByTestId("tab-todos").click();
  await expect(page.getByTestId("todo-item")).toHaveCount(2);
  await shot("todos-dark");

  for (const line of logged) console.log(line);
  expect(problems, "the page logged no errors").toEqual([]);
});

/** The number in a tile such as `10,081`. */
async function tileNumber(page: Page, testId: string): Promise<number> {
  return Number(((await page.getByTestId(testId).textContent()) ?? "").replace(/,/g, ""));
}

test("the stress screen: the core generates, the mirror merges, no_coalesce is applied step by step", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));

  await page.goto("/?screen=stress&mode=firehose&rate=10000&autostart=1");
  await expect(page.getByTestId("stress-state")).toHaveText("running");

  await test.step("firehose: 10,000 updates a second arrive as change-sets and are applied once per frame", async () => {
    // The tiles are measured over a two-second window and refresh every 500 ms.
    await expect.poll(() => tileNumber(page, "stress-received"), { timeout: 10_000 }).toBeGreaterThan(2_000);
    const generated = await tileNumber(page, "stress-generated");
    const received = await tileNumber(page, "stress-received");
    const applied = await tileNumber(page, "stress-applied");
    const valueApplies = await tileNumber(page, "stress-value-applies");
    const value = await tileNumber(page, "stress-value");
    console.log(`stress, firehose 10k/s: generated ${generated}/s, received ${received}/s, applied ${applied}/s, value ${value}, applied ${valueApplies} times`);
    expect(generated, "the generator tracks its rate (a loaded CI machine gets a wide band)").toBeGreaterThan(5_000);
    expect(generated).toBeLessThan(15_000);
    expect(applied, "the mirror merged what arrived between two frames").toBeLessThan(received / 10);
    expect(valueApplies, "the merged signal was applied far less often than it was written").toBeLessThan(value / 10);
    expect(value).toBeGreaterThan(0);
    await page.screenshot({ path: `${PROOF}web-stress.png`, fullPage: true });
  });

  await test.step("progress: the no_coalesce signal is applied once per update", async () => {
    await page.getByTestId("stress-mode-progress").click();
    await expect.poll(() => tileNumber(page, "stress-progress-applies"), { timeout: 10_000 }).toBeGreaterThan(2_000);
    const progress = await tileNumber(page, "stress-progress");
    const applies = await tileNumber(page, "stress-progress-applies");
    console.log(`stress, progress 10k/s: progress ${progress}, applied ${applies} times`);
    // Every update was applied on its own; the count is read up to half a second after the number.
    expect(applies).toBeGreaterThan(progress / 2);
    expect(applies).toBeLessThanOrEqual(progress);
  });

  await test.step("stop: the generator ends and the rates fall to zero", async () => {
    await page.getByTestId("stress-stop").click();
    await expect(page.getByTestId("stress-state")).toHaveText("stopped");
    await expect.poll(() => tileNumber(page, "stress-generated"), { timeout: 10_000 }).toBe(0);
    await expect.poll(() => tileNumber(page, "stress-received"), { timeout: 10_000 }).toBeLessThan(5);
  });

  await test.step("burst: a thousand updates at once, one change-set each, applied as one", async () => {
    await page.getByTestId("stress-mode-firehose").click();
    const before = await tileNumber(page, "stress-value");
    const appliesBefore = await tileNumber(page, "stress-value-applies");
    await page.getByTestId("stress-burst").click();
    await expect.poll(() => tileNumber(page, "stress-value")).toBe(before + 1_000);
    await expect.poll(() => tileNumber(page, "stress-value-applies"), { timeout: 5_000 }).toBeGreaterThan(appliesBefore);
    expect(await tileNumber(page, "stress-value-applies"), "1,000 writes were not 1,000 applies").toBeLessThan(appliesBefore + 50);
  });

  await test.step("leaving the screen stops the generator and releases its store", async () => {
    await page.getByTestId("stress-start").click();
    await expect(page.getByTestId("stress-state")).toHaveText("running");
    await page.getByTestId("tab-counter").click();
    await expect(page.getByTestId("counter-value")).toHaveText("0");
    await page.getByTestId("tab-stress").click();
    // A new store: nothing is running and the count starts again from zero.
    await expect(page.getByTestId("stress-state")).toHaveText("stopped");
    await expect(page.getByTestId("stress-value")).toHaveText("0");
  });

  expect(problems, "the page logged no errors").toEqual([]);
});

test("embedded, the stress screen posts the extended undra-stats message to its parent", async ({ page }) => {
  await page.goto("/?screen=todos");
  // A same-origin parent (as the landing page is) with the stress screen in an iframe, as "Push it" opens it.
  const message = await page.evaluate(
    () =>
      new Promise<Record<string, unknown>>((resolve, reject) => {
        const frame = document.createElement("iframe");
        frame.src = "/?screen=stress&embed=1&rate=10000&mode=firehose&autostart=1";
        window.addEventListener("message", (event: MessageEvent<Record<string, unknown>>) => {
          const data = event.data;
          if (event.source === frame.contentWindow && data?.["type"] === "undra-stats" && Number(data["generatedPerSec"]) > 2_000) resolve(data);
        });
        setTimeout(() => reject(new Error("the iframe posted no stress stats within 20 s")), 20_000);
        document.body.appendChild(frame);
      }),
  );
  console.log(`embedded stress message: ${JSON.stringify(message)}`);
  // The base shape is still there, for a consumer that knows only that.
  for (const field of ["changeSetsPerSec", "applyP50Us", "applyP99Us", "timerResolutionUs", "applyBatchDrains", "applyBatchUs", "applyMaxUs"]) expect(typeof message[field], field).toBe("number");
  // And the stress fields.
  expect(message["mode"]).toBe("firehose");
  expect(message["targetRate"]).toBe(10_000);
  expect(message["running"]).toBe(true);
  expect(message["runtime"]).toBe("wasm-main");
  for (const field of ["generatedPerSec", "entriesReceivedPerSec", "entriesAppliedPerSec", "mergeRatio", "drainsPerSec", "applyNsPerChangeSet", "droppedFrames", "droppedFramesRecent", "longestFrameMs", "valueApplies", "progressApplies"]) {
    expect(typeof message[field], field).toBe("number");
  }
  expect(Number(message["entriesAppliedPerSec"]), "merged").toBeLessThan(Number(message["entriesReceivedPerSec"]) / 10);
  expect(Number(message["mergeRatio"])).toBeLessThan(0.1);
});

test("a core that cannot be loaded is reported on the page", async ({ page }) => {
  // The page asks for the wasm core; make that fail, as a missing file or a schema mismatch would.
  await page.route("**/*.wasm", (route) => route.abort());
  await page.goto("/");
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Undra did not start");
  await expect(alert).toContainText("wasm core");
  await expect(page.getByTestId("tab-todos")).toHaveCount(0);
});

test("live and notes: the core's WebSocket echoes through the browser, and its SQLite keeps notes in OPFS", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));

  await page.goto("/#live");
  await expect(page.getByTestId("live-url")).toHaveValue("ws://127.0.0.1:4180/ws/echo");
  await page.getByTestId("live-connect").click();
  await expect(page.getByTestId("live-state")).toHaveText("open");
  await page.getByTestId("live-draft").fill("hello from the core");
  await page.getByTestId("live-send").click();
  await expect(page.locator('[data-testid="live-message"][data-direction="out"]')).toHaveText(["→ hello from the core"]);
  await expect(page.locator('[data-testid="live-message"][data-direction="in"]')).toHaveText(["← hello from the core"]);
  await page.getByTestId("live-disconnect").click();
  await expect(page.getByTestId("live-state")).toHaveText("closed");
  await expect(page.getByTestId("live-error")).toHaveCount(0);
  await page.screenshot({ path: `${PROOF}web-live.png`, fullPage: true });

  // A refused upgrade is a typed error on the page (a browser hides the status).
  await page.getByTestId("live-url").fill("ws://127.0.0.1:4180/ws/deny?status=401");
  await page.getByTestId("live-connect").click();
  await expect(page.getByTestId("live-error")).toContainText("the WebSocket was refused");

  // Notes: the core's Db port over wa-sqlite in a dedicated worker on OPFS. A row added survives a reload.
  await page.getByTestId("tab-notes").click();
  await expect(page.getByTestId("notes-version")).toHaveText("schema version 2");
  await expect(page.getByTestId("notes-item")).toHaveCount(0);
  await page.getByTestId("notes-input").fill("Buy milk");
  await page.getByTestId("notes-add").click();
  await page.getByTestId("notes-input").fill("Write the report");
  await page.getByTestId("notes-add").click();
  await expect(page.getByTestId("notes-item")).toHaveText(["Buy milk", "Write the report"]);
  // The box follows the database: it is checked once the core's UPDATE ran and the mirror says so.
  await page.getByTestId("notes-toggle").first().click();
  await expect(page.getByTestId("notes-toggle").first()).toBeChecked();
  await expect(page.getByTestId("notes-error")).toHaveCount(0);
  // The notes' selection is the same generic `Selection<T>` the to-do screen uses, instantiated for notes (ADR-058):
  // its own store and handle, `newest("Note", rows)` for the line, `draft("Note", title)` for a note not in the database.
  await expect(page.getByTestId("note-latest")).toHaveText("Latest: Write the report");
  await page.getByTestId("notes-select").last().check();
  await expect(page.getByTestId("note-selected-count")).toHaveText("1 selected");
  await expect(page.getByTestId("note-picked")).toHaveText("Write the report");
  await page.getByTestId("note-new-draft").click();
  await expect(page.getByTestId("note-selected-count")).toHaveText("2 selected");
  await expect(page.getByTestId("notes-item")).toHaveCount(2);
  await page.getByTestId("note-select-clear").click();
  await expect(page.getByTestId("note-selected-count")).toHaveText("0 selected");
  await page.screenshot({ path: `${PROOF}web-notes.png`, fullPage: true });
  await page.reload();
  await expect(page.getByTestId("notes-version")).toHaveText("schema version 2");
  await expect(page.getByTestId("notes-item"), "the notes are in OPFS, not in the page").toHaveText(["Buy milk", "Write the report"]);
  await expect(page.getByTestId("notes-toggle").first()).toBeChecked();

  // The refused upgrade is logged by Chromium itself ("WebSocket connection to ... failed"); nothing else may be.
  expect(problems.filter((text) => !text.includes("WebSocket connection to"))).toEqual([]);
});

test("the workshop: shelves the core hands out, merged by one call; a job that calls the page back", async ({ page }) => {
  const problems: string[] = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto("/?screen=workshop");
  const left = page.getByTestId("shelf-left");
  const right = page.getByTestId("shelf-right");
  await expect(left).toHaveText("left: 0");
  await page.getByTestId("shelf-stock").click();
  await page.getByTestId("shelf-stock").click();
  await expect(left).toHaveText("left: 2");
  await page.getByTestId("shelf-merge").click();
  await expect(right).toHaveText("right: 2");
  await expect(left).toHaveText("left: 0");
  await page.getByTestId("job-run").click();
  await expect(page.getByTestId("job-question")).toContainText("ran 3 steps; go on?");
  await expect(page.getByTestId("job-step")).toHaveText("step 3 of 3");
  await expect(page.getByTestId("job-notes").getByRole("listitem")).toHaveCount(3);
  await page.getByTestId("job-yes").click();
  await expect(page.getByTestId("job-outcome")).toHaveText("done: 3 steps");
  await expect(page.getByTestId("job-count")).toHaveText("1 job run");
  expect(problems).toEqual([]);
});

test("the library: 10,000 books the page never receives whole, paged through a window; a view of the even rows", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto("/?screen=library");
  const viewport = page.getByTestId("library-viewport");

  await test.step("the length arrives at once, the rows by page, and only a window is drawn", async () => {
    await expect(page.getByTestId("library-count")).toHaveText("10000");
    // The first read asks for the pages: the rows are placeholders until they arrive, then real rows.
    await expect(page.getByTestId("library-row").first()).toContainText("Item 1");
    await expect(page.getByTestId("library-placeholder")).toHaveCount(0);
    const drawn = await page.getByTestId("library-row").count();
    expect(drawn, "only the visible rows are in the DOM").toBeLessThan(40);
    expect(drawn).toBeGreaterThan(8);
    await expect(page.getByTestId("library-window")).toContainText("Drawing rows 1 to 15 of 10,000");
    await page.screenshot({ path: `${PROOF}web-library.png`, fullPage: true });
  });

  await test.step("a far scroll shows placeholders, then the rows of the pages it asked for", async () => {
    await viewport.evaluate((element) => {
      element.scrollTop = 36 * 7_000;
    });
    await expect(page.getByTestId("library-window")).toContainText("Drawing rows 6996 to 7015 of 10,000");
    await expect(page.getByTestId("library-row").first()).toContainText("Item 6996");
    await expect(page.getByTestId("library-placeholder")).toHaveCount(0);
    await viewport.evaluate((element) => {
      element.scrollTop = 36 * 9_990;
    });
    await expect(page.getByTestId("library-row").last()).toContainText("Item 10000");
  });

  await test.step("a change is one notice: the length follows at once and the rows on screen are read again", async () => {
    await viewport.evaluate((element) => {
      element.scrollTop = 0;
    });
    await expect(page.getByTestId("library-row").first()).toContainText("Item 1");
    const version = await page.getByTestId("library-version").textContent();
    await page.getByTestId("library-add").click();
    await expect(page.getByTestId("library-count")).toHaveText("10100");
    await expect(page.getByTestId("library-version")).not.toHaveText(version ?? "");
    await page.getByTestId("library-rename").click();
    await expect(page.getByTestId("library-row").first()).toContainText("Renamed at");
    await expect(page.getByTestId("library-row").first()).toContainText("v1");
    await page.getByTestId("library-remove").click();
    await expect(page.getByTestId("library-count")).toHaveText("10099");
    await expect(page.getByTestId("library-row").first()).toContainText("Item 2");
    await page.getByTestId("library-reset").click();
    await expect(page.getByTestId("library-count")).toHaveText("10000");
    await expect(page.getByTestId("library-row").first()).toContainText("Item 1");
    await expect(page.getByTestId("library-error")).toHaveCount(0);
  });

  await test.step("the view of the even rows follows its source", async () => {
    // A new store: the tab owns its Library, so leaving and coming back starts from the core's fresh one.
    await page.getByTestId("tab-counter").click();
    await expect(page.getByTestId("counter-value")).toHaveText("0");
    await page.getByTestId("tab-library").click();
    await expect(page.getByTestId("library-count")).toHaveText("10000");
    await expect(page.getByTestId("evens-count")).toHaveText("10");
    await expect(page.getByTestId("evens-row").first()).toHaveText(/^#2 /);
    await expect(page.getByTestId("evens-row").nth(9)).toHaveText(/^#20 /);
    await page.getByTestId("evens-add").click();
    await expect(page.getByTestId("evens-count")).toHaveText("11");
    await expect(page.getByTestId("evens-source")).toHaveText("source: 22 rows");
    await expect(page.getByTestId("evens-row").nth(10)).toHaveText(/^#\d+ /);
    await page.getByTestId("evens-drop").click();
    await expect(page.getByTestId("evens-source")).toHaveText("source: 17 rows");
    await expect(page.getByTestId("evens-count")).toHaveText("9");
    await expect(page.getByTestId("evens-row").first()).toHaveText(/^#6 /);
  });

  expect(problems, "the page logged no errors").toEqual([]);
});

test("the feed: pages that load as the sentinel scrolls into view; refresh fetches the loaded pages again", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto("/?screen=feed");
  const scroll = page.getByTestId("feed-scroll");

  await test.step("the first page is fifty rows, and the next waits for the sentinel", async () => {
    await expect(page.getByTestId("feed-row")).toHaveCount(50);
    await expect(page.getByTestId("feed-count")).toHaveText("50");
    await expect(page.getByTestId("feed-status")).toHaveText("success");
    await expect(page.getByTestId("feed-footer")).toHaveText("Scroll for more");
    await expect(page.getByTestId("feed-row").first()).toContainText("Item 1");
    await page.screenshot({ path: `${PROOF}web-feed.png`, fullPage: true });
    // Nothing scrolled it into view: a moment later there is still one page.
    await page.waitForTimeout(500);
    await expect(page.getByTestId("feed-row")).toHaveCount(50);
  });

  await test.step("scrolling to the end loads the next page, and the next, as the sentinel comes into view", async () => {
    await scroll.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    await expect(page.getByTestId("feed-row")).toHaveCount(100);
    await scroll.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    await expect(page.getByTestId("feed-row")).toHaveCount(150);
    await expect(page.getByTestId("feed-footer")).toHaveText("Scroll for more");
    await expect(page.getByTestId("feed-row").nth(149)).toContainText("Item 150");
  });

  await test.step("refresh fetches the three pages again and finds nothing new; touching the even rows finds them", async () => {
    await page.getByTestId("feed-refresh").click();
    await expect(page.getByTestId("feed-status")).toHaveText("success");
    await expect(page.getByTestId("feed-row")).toHaveCount(150);
    // A refetch asked for while one is in flight is the one in flight (the buttons are disabled meanwhile): let the three pages finish.
    await page.waitForTimeout(400);
    await expect(page.getByTestId("feed-touch")).toBeEnabled();
    await page.getByTestId("feed-touch").click();
    await expect(page.getByTestId("feed-revision")).toHaveText("revision 1");
    await expect(page.getByTestId("feed-row").nth(1)).toContainText("v1"); // row 2 is even
    await expect(page.getByTestId("feed-row").nth(0)).toContainText("v0"); // row 1 is odd
    await expect(page.getByTestId("feed-row")).toHaveCount(150);
    await expect(page.getByTestId("feed-error")).toHaveCount(0);
  });

  await test.step("even rows only is another feed with its own pages", async () => {
    await page.getByTestId("feed-even-only").check();
    await expect(page.getByTestId("feed-row")).toHaveCount(50);
    await expect(page.getByTestId("feed-row").first()).toContainText("Item 2");
    await expect(page.getByTestId("feed-row").nth(49)).toContainText("Item 100");
    await page.getByTestId("feed-even-only").uncheck();
    await expect(page.getByTestId("feed-row").first()).toContainText("Item 1");
  });

  expect(problems, "the page logged no errors").toEqual([]);
});

test("the ticker: a counter the core polls every second; an override, a failure that does not stop it", async ({ page }) => {
  mkdirSync(PROOF, { recursive: true });
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto("/?screen=ticker");
  const value = page.getByTestId("ticker-value");
  const current = async (): Promise<number> => Number((await value.textContent()) ?? "0");

  await test.step("the counter goes up by itself", async () => {
    await expect(value).toHaveText("1");
    await expect(page.getByTestId("ticker-status")).toHaveText("success");
    await expect(value).toHaveText("2", { timeout: 4_000 });
    await expect(page.getByTestId("ticker-line")).toContainText("polling every 1 s");
    await expect(page.getByTestId("ticker-fetches")).toContainText("fetches in the core");
    await page.screenshot({ path: `${PROOF}web-ticker.png`, fullPage: true });
  });

  await test.step("an override of five seconds slows this observer down, and clearing it brings the second back", async () => {
    await page.getByTestId("ticker-slow").click();
    await expect(page.getByTestId("ticker-slow")).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByTestId("ticker-line")).toContainText("polling every 5 s");
    // The fetch that was already scheduled still happens; then nothing for most of five seconds.
    const seen = await current();
    await expect.poll(current, { timeout: 7_000 }).toBeGreaterThan(seen);
    const slowed = await current();
    await page.waitForTimeout(3_000);
    expect(await current(), "no fetch for three seconds with a five-second interval").toBe(slowed);
    await page.getByTestId("ticker-slow").click();
    await expect(page.getByTestId("ticker-line")).toContainText("polling every 1 s");
    await expect.poll(current, { timeout: 6_000 }).toBeGreaterThan(slowed);
  });

  await test.step("a failure shows the error, keeps the last value and keeps polling; the next success clears it", async () => {
    await page.getByTestId("ticker-fail").click();
    await expect(page.getByTestId("ticker-error")).toHaveText("the ticker is failing on purpose", { timeout: 6_000 });
    await expect(page.getByTestId("ticker-status")).toHaveText("error");
    const held = await current();
    const before = await page.getByTestId("ticker-fetches").textContent();
    await expect(page.getByTestId("ticker-fetches")).not.toHaveText(before ?? "", { timeout: 4_000 });
    expect(await current(), "the value is the last success").toBe(held);
    await page.getByTestId("ticker-fail").click();
    await expect(page.getByTestId("ticker-error")).toHaveCount(0, { timeout: 6_000 });
    await expect.poll(current, { timeout: 4_000 }).toBeGreaterThan(held);
  });

  await test.step("leaving the tab stops the polling", async () => {
    await page.getByTestId("tab-counter").click();
    await expect(page.getByTestId("counter-value")).toHaveText("0");
    await page.getByTestId("tab-ticker").click();
    // A new handle: the counter in the core kept counting only while somebody watched, and the new view starts from its cached value.
    await expect(page.getByTestId("ticker-value")).toBeVisible();
  });

  expect(problems, "the page logged no errors").toEqual([]);
});
