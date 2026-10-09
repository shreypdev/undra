#!/usr/bin/env node
// Fills the code blocks of the cookbook pages (site/docs/cookbook/*.html, site/docs/sample.html) from the
// sources they quote, so a page's code is the code that compiled and ran in CI:
//
//   <!-- snippet:examples/cookbook/core/src/auth.rs#auth-401 rust -->
//   ...the block, written by this script...
//   <!-- /snippet -->
//
// A source file marks what a page may quote with `// docs:begin <id>` and `// docs:end` lines (the markers
// themselves are left out, the text is dedented). An id may appear several times in a file: the pieces are
// joined, in file order, with a blank line. The Rust crates are built and tested by `cargo test -p cookbook`
// and the platform lines by `examples/cookbook/snippets/check.sh`. A marker that names a missing file or id
// fails the build.
//
//   node site/scripts/build-cookbook.mjs
import { readdirSync, existsSync } from "node:fs";
import { join, resolve, basename } from "node:path";
import { SITE, read, writeIfChanged, esc } from "./lib.mjs";

const ROOT = resolve(SITE, "..");
const PAGES = [join(SITE, "docs", "sample.html"), join(SITE, "docs", "ports.html")];
const dir = join(SITE, "docs", "cookbook");
if (existsSync(dir)) for (const f of readdirSync(dir).sort()) if (f.endsWith(".html")) PAGES.push(join(dir, f));

/** The text of every `docs:begin <id>` .. `docs:end` region of `file`, joined by a blank line, dedented. */
function region(file, id) {
  const lines = read(join(ROOT, file)).split("\n");
  const pieces = [];
  for (let i = 0; i < lines.length; i++) {
    const m = /\bdocs:begin\s+(\S+)/.exec(lines[i]);
    if (!m || m[1] !== id) continue;
    let j = i + 1;
    while (j < lines.length && !/\bdocs:end\b/.test(lines[j])) j++;
    if (j === lines.length) throw new Error(`${file}: docs:begin ${id} has no docs:end`);
    pieces.push(lines.slice(i + 1, j));
  }
  if (!pieces.length) throw new Error(`${file}: no docs:begin ${id}`);
  const strip = (block) => {
    const pad = Math.min(...block.filter((l) => l.trim()).map((l) => /^ */.exec(l)[0].length));
    return block.map((l) => l.slice(pad)).join("\n").replace(/\s+$/, "");
  };
  return pieces.map(strip).join("\n\n");
}

const SHOWN = { rs: "rust", swift: "swift", kt: "kotlin", ts: "ts", tsx: "tsx" };
const label = (file, lang) => (lang === "rust" ? file.replace(/^examples\/[a-z]+\//, "") : `<span class="lang">${lang}</span>`);

let changed = 0;
for (const page of PAGES) {
  if (!existsSync(page)) continue;
  const before = read(page);
  const after = before.replace(/<!-- snippet:(\S+?)#(\S+?)(?: (\S+))? -->[\s\S]*?<!-- \/snippet -->/g, (m, file, id, lang) => {
    lang ||= SHOWN[file.split(".").pop()] || "text";
    const code = region(file, id);
    const bar = lang === "rust" ? `<span>${esc(label(file, lang))}</span>` : `<span>${label(file, lang)}</span>`;
    return `<!-- snippet:${file}#${id} ${lang} -->\n<div class="code"><div class="code-bar">${bar}</div><pre><code data-lang="${lang}">${esc(code)}</code></pre></div>\n<!-- /snippet -->`;
  });
  if (writeIfChanged(page, after)) changed++;
}
console.log(`build-cookbook: ${PAGES.filter(existsSync).length} pages; ${changed ? `${changed} updated` : "up to date"}`);
