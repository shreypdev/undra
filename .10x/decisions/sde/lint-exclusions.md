# `lint-exclusions`: the lint exclusion files are a project setting (ADR-061, section 5 and its amendment)

Worktree `wt/lint-exclusions`, from `main` `58e373a`. One piece: no wire, ABI, schema, schema-hash or generated-declaration change, and
no golden moved (every golden is generated with the default).

## What landed

* **The setting.** `[bindings] lint_exclusions = "beside" | "none"` in `undra.toml` (`LintExclusions` in `crates/undra-cli/src/config.rs`).
  `beside` is the default and today's behaviour: the four files `undra bindgen` writes beside the trees. `none` writes none of them. The
  key is in `[bindings]` because that table is where `undra.toml` already holds what shapes the generated code; there is no `[bindgen]` table.
* **Validation (R8).** A value other than the two, or one that is not a string, is `C0002` naming the line, saying why (the setting decides
  whether the files that keep the linters out of the bindings are written), the fix (both spellings, and what to add to a root configuration
  for `none`) and the guide's URL, beside the `errors.html#C0002` link every diagnostic carries.
* **Applied in one place.** `Plan::lint_exclusions` (set by `undra bindgen` from the project, by `undra init` from the config it writes);
  `plan_files` leaves `lint::fragments` out for `none`. The files are manifest entries, so the rest follows with no new code: `--check`
  names the old four as stale after a switch to `none`, and the next `undra bindgen` removes them (and writes them again on the way back).
* **Not an exclusion file, so not affected.** The `@file:Suppress("ALL", "ktlint")` line in every Kotlin file (it is part of the file and travels
  with it), the per-tree `.gitattributes` (ADR-062) and the Kotlin `.gitignore`. A test holds the first; the docs and `--help` say so.
* **`undra init`** writes `# lint_exclusions = "none"` as a comment under `[bindings]` (default `beside`), and a project that chose `none` has
  the line as a setting (render and parse round-trip).
* **Bazel.** Nothing assumes the files exist: `undra_bindings` declares one tree per language, the Kotlin srcjar (only `*.kt` files go in it) and
  the Swift files `swift_files` names (sources, C module files), never a lint file, and it runs the CLI over the `undra.toml` it is given, so
  the setting flows through with no edit to a `.bzl` file. The example's ktlint test relies on the default and still does.
* **Docs.** `site/docs/bazel.html`, "Linters over the whole tree" (`#lint-everything`): the four files with their linter and content, the
  setting, and the root-configuration lines for `none` (SwiftLint `excluded:`, ktlint `.editorconfig` `[**/generated/**]` and the Gradle filter,
  ESLint 9 `ignores` and ESLint 8 `ignorePatterns`, Vale's empty style list for the tree); `site/docs/cli.html` (the `undra.toml` listing, a
  paragraph, the `undra bindgen` text); `undra bindgen --help`; SPEC 13's sentence on the lint files; `lint.rs`'s module comment.
  Regenerated: `site/search-index.json`, `site/llms-full.txt`.

## Tests

* `config.rs`: absent, `beside` and `none` parse; `undra init`'s text carries the commented line and a `none` project round-trips; four wrong
  values (a word, wrong case, empty, a boolean) each give code, what with the line, why, fix with the guide link, and the docs link; the key
  is unknown outside `[bindings]`.
* `bindgen.rs`: with `none` exactly the four files are missing and every other path is the same; the Kotlin suppression stays; switching an
  applied tree to `none` and back leaves `--check` clean each way.
* `tests/bindgen_schema.rs`: through the binary with a project file, absent and `beside` write the four files, `none` writes no one and names none in the
  manifest, the rest of the manifest is identical, `--check` is clean, a switch is stale until the next run, and a wrong value stops with the teaching error and
  writes nothing.

## Not verified

SwiftLint, ESLint and Vale are not installed on this machine (as in the ADR), so the root-configuration lines for them follow each tool's
documentation and say so on the page; ktlint's `[**/generated/**]` is the form the generated `.editorconfig` already shows.
