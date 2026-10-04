/* Undra site, landing page only: the live demo. Progressive enhancement: with JS off the demo links to the playground. */
(function () {
  "use strict";
  var doc = document;
  var $ = function (sel, ctx) { return (ctx || doc).querySelector(sel); };
  var $$ = function (sel, ctx) { return Array.prototype.slice.call((ctx || doc).querySelectorAll(sel)); };
  var canObserve = "IntersectionObserver" in window;

  /* ---------- the live demo: the real playground, mounted when scrolled into view ---------- */
  var live = $("[data-live]");
  if (live) {
    var mount = $("[data-live-mount]", live), note = $("[data-live-note]", live), src = mount && mount.getAttribute("data-src");
    var stat = { rate: $('[data-stat="rate"]', live), avg: $('[data-stat="avg"]', live), avgOf: $('[data-stat="avg-of"]', live), max: $('[data-stat="max"]', live), gen: $('[data-stat="gen"]', live), merge: $('[data-stat="merge"]', live), dropped: $('[data-stat="dropped"]', live) };
    var stepCells = $$("[data-clock-step]", live);
    var stressCells = $$("[data-stress]", live);
    // Browsers clamp performance.now() to a step of at least 100 µs unless the page is cross-origin isolated (Chrome:
    // 5 µs then), and GitHub Pages cannot send the isolating headers. The playground measures its own step, but a
    // frozen or unmeasurable clock reports 0, so the floor below is what is known and the counters never claim finer.
    var floorUs = window.crossOriginIsolated ? 5 : 100;
    var pushBtn = $("[data-push-it]", live), bar = $(".frame-bar span", live), frame = null, gotStats = false, giveUp = 0, step = floorUs;
    var theme = function () { return doc.documentElement.dataset.theme === "light" ? "light" : "dark"; };
    var stepText = function () { return step >= 100 ? (step / 1000) + " ms" : (step || 1) + " µs"; }; // as the playground writes it (0.1 ms)
    // One reading of k steps is a time between k - 1 and k + 1 steps, so it is shown as the bound that is always true:
    // under k + 1 steps ("< 0.1 ms" for a reading of 0, "< 0.2 ms" for one step).
    var fmtOne = function (us) {
      if (!isFinite(us) || us < 0) return "–";
      var bound = (Math.round(us / step) + 1) * step;
      return "< " + (bound >= 100 ? +(bound / 1000).toFixed(3) + " ms" : Math.round(bound) + " µs");
    };
    // A batch's average: resolved below the step (the readings' errors even out), so it keeps a decimal under 10 µs.
    var fmtAvg = function (us) {
      if (!isFinite(us) || us < 0) return "–";
      return us >= 1000 ? (us / 1000).toFixed(2) + " ms" : us >= 10 ? Math.round(us) + " µs" : us.toFixed(1) + " µs";
    };
    var count = function (n) { return isFinite(n) ? Math.round(n).toLocaleString("en-US") : "–"; };
    var listUrl = function () { return src + "&theme=" + theme(); };
    // "Push it": the playground's stress screen. The core generates 10,000 updates a second on its own timer.
    var stressUrl = function () { return src.split("?")[0] + "?screen=stress&embed=1&rate=10000&mode=firehose&autostart=1&theme=" + theme(); };
    var open = function (url, label) {
      if (frame) frame.remove();
      gotStats = false; clearTimeout(giveUp);
      stressCells.forEach(function (el) { el.hidden = true; });
      [stat.rate, stat.avg, stat.max, stat.gen, stat.dropped].forEach(function (el) { el.textContent = "–"; });
      stat.merge.textContent = ""; stat.avgOf.textContent = "";
      if (bar) bar.textContent = label;
      frame = doc.createElement("iframe");
      frame.loading = "lazy"; frame.src = url;
      frame.title = "Undra playground: the real Rust core, running in this page as WebAssembly";
      mount.appendChild(frame);
      if (note) note.hidden = true;
      giveUp = setTimeout(function () {
        if (gotStats) return;
        if (note) { note.textContent = "The live counters did not start. Your browser may block WebAssembly or embedded frames; "; var a = doc.createElement("a"); a.href = "playground/?screen=list&stream=1"; a.textContent = "open the playground in its own tab"; note.appendChild(a); note.appendChild(doc.createTextNode(".")); note.hidden = false; }
      }, 5000);
    };
    window.addEventListener("message", function (e) {
      var d = e.data;
      if (!frame || e.source !== frame.contentWindow || e.origin !== location.origin || !d || d.type !== "undra-stats") return;
      var r = Number(d.changeSetsPerSec), n = Number(d.applyBatchDrains), total = Number(d.applyBatchUs), worst = Number(d.applyMaxUs);
      if (!isFinite(r) || !isFinite(n) || !isFinite(total) || !isFinite(worst)) return;
      if (!gotStats) { gotStats = true; clearTimeout(giveUp); if (note) note.hidden = true; }
      if (isFinite(Number(d.timerResolutionUs))) step = Math.max(floorUs, Number(d.timerResolutionUs));
      stepCells.forEach(function (el) { el.textContent = stepText(); });
      // The average of every drain of the last two seconds, timed as one batch: a number once the batch spans a
      // clock step, "under" the step only when even the whole batch took less.
      if (n > 0) {
        stat.avg.textContent = total >= step ? fmtAvg(total / n) : "< " + stepText();
        stat.avgOf.textContent = "average of " + count(n) + (n === 1 ? " drain" : " drains");
      } else {
        stat.avg.textContent = "–"; stat.avgOf.textContent = "";
      }
      stat.max.textContent = n > 0 ? fmtOne(worst) : "–";
      // The stress screen adds the core's own counter and what the mirror did with it (all measured in this page).
      var g = Number(d.generatedPerSec), applied = Number(d.entriesAppliedPerSec), received = Number(d.entriesReceivedPerSec), ratio = Number(d.mergeRatio), dropped = Number(d.droppedFrames);
      var stress = d.generatedPerSec !== undefined && isFinite(g) && isFinite(applied);
      stressCells.forEach(function (el) { el.hidden = !stress; });
      if (stress) {
        stat.gen.textContent = count(g);
        stat.rate.textContent = count(applied);
        // Say what happened to the rest: 10,000 received and 120 applied is the merge, not a loss.
        stat.merge.textContent = isFinite(ratio) && isFinite(received) && received > 0 ? count(received) + " received, merged into " + (ratio * 100).toFixed(ratio < 0.01 ? 2 : 1) + " %" : "";
        stat.dropped.textContent = isFinite(dropped) ? count(dropped) : "–";
      } else {
        stat.rate.textContent = r.toFixed(1);
        stat.merge.textContent = "";
      }
    });
    doc.addEventListener("undra:theme", function (e) {
      if (!frame || !frame.contentWindow) return;
      frame.contentWindow.postMessage({ type: "undra-theme", theme: e.detail }, location.origin);
    });
    if (pushBtn) pushBtn.addEventListener("click", function () { open(stressUrl(), "playground · stress"); pushBtn.disabled = true; });
    if (src && canObserve) {
      var lo = new IntersectionObserver(function (es) {
        if (es[0].isIntersecting) { lo.disconnect(); if (!frame) open(listUrl(), "playground · 10k list"); }
      }, { rootMargin: "200px 0px" });
      lo.observe(live);
    }
  }
})();
