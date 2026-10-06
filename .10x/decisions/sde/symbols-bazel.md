# SDE: symbols-bazel: the `symbols` output group documented, and `undra symbolicate` proven on a crash of a Bazel-built core (ADR-061, ADR-046), 2026-10-05

Branch `wt/symbols-bazel`, from `main` `58e373a`. Undra 1.1, "Bazel-first integration". The binding texts are ADR-061 (the rules; the
`symbols` output group) and ADR-046 (panic reports and symbol files). No boundary change: no wire, ABI, schema-hash or runtime-model
change; the one new API item is a function of the example's own core.

## What landed

| Where | What |
|---|---|
| `bazel/undra/private/run.sh` | the `symbols` output group now has the layout of `undra build --release`: `symbols/` is `build/symbols/`, and for the host `host/` holds the library's dSYM (`.so.debug` on Linux). Before it was `build/symbols/` flattened, which left the host library's symbols out: the manifest names them `../host/<lib>.dSYM`, so the group could not resolve a host crash |
| `bazel/undra/core.bzl` | the `undra_core` docstring says which targets have the group (release ones, and `web`), its layout, and the `filegroup(output_group = "symbols")` form |
| `examples/bazel/BUILD.bazel` | `core_release` (host, `release = True`: the stripped build that ships), and the labels `core_release_symbols` and `core_web_symbols` (`filegroup` with `output_group = "symbols"`); `exports_files` of the core's `lib.rs` for the test to read the line of the `panic!` |
| `examples/bazel/core/src/lib.rs` | `crash_for_symbols_test(reason)`, an `#[undra::api]` function that panics, documented as test-only and called by nothing but the symbols test (it is in the bindings, as every API function is; +0.9 KB gzipped on the web core, 113.6 KB of 120); a unit test that it panics |
| `examples/bazel/symbols/` | `//symbols:symbolicate_test` (`BUILD.bazel`, `SymbolicateTest.kt`) |
| `site/docs/bazel.html`, `site/docs/production.html` | the section "Symbol files and crash reports" (`#symbols`: the group, its layout, keeping it per release, `undra symbolicate`, the test as the reference); the production page's symbols section links to it; `site/search-index.json` and `site/llms-full.txt` regenerated |
| `examples/bazel/README.md` | the test, the labels and the command |

## How the crash report is obtained

The test is a Kotlin JVM program (the generated bindings over the Kotlin runtime, as an app uses them), not a canned fixture and not a
C harness. It loads `//:core_release_host` (the release core Bazel built, stripped) with `LoadOptions(onPanic = ..)`, calls
`crashForSymbolsTest("kaboom")` (the call fails with `UndraCallError.Panicked`, the core lives on), and takes the `UndraPanicReport`
the handler gets from a `CompletableFuture`: nothing waits for a time (the 60 s bound is a failure's, not a delay). It serialises the
report as the JSON `undra symbolicate` reads, runs `@undra//:cli symbolicate --symbols <group>/symbols report.json` and asserts:

* the group has `symbols/manifest.json` and exactly one host symbol twin under `host/`;
* the report is of this core (namespace, version), its location is `core/src/lib.rs:<line>:..` where `<line>` is found in the source
  (the `panic!` after `pub fn crash_for_symbols_test(`), its frames are addresses only (release), its image id is the manifest's;
* `undra symbolicate` exits 0 and a frame reads `hello_core::crash_for_symbols_test (/undra/app/core/src/lib.rs:<line>)`: the file, the
  line the source has and the core reported, and the remapped path (ADR-052).

The tools that read a symbol file are the machine's: `atos` through `/usr/bin` (DEVELOPER_DIR is passed with `env_inherit`) on macOS;
on Linux `llvm-symbolizer` or `llvm-addr2line`, looked up on `PATH` and in `/usr/lib/llvm-*/bin`, and the test fails saying
`apt install llvm` when there is none.

## Verified

* `bazel test //...` in `examples/bazel` on macOS (arm64, Bazel 8.8.1): 7 of 7 pass, the new test in 1.2 s on a warm cache.
* The test fails for the right reason: with the host twin left out of the group (the rule as it was) it fails on "the symbols group has
  the host library's symbols" and on `undra symbolicate`'s "cannot read ...libhello_core.dylib.dSYM"; with the dSYM's DWARF file emptied
  it fails on `undra symbolicate`'s "is not a Mach-O file". Both reverted.
* The group against the CLI: `undra build --release --platform host,web` of a copy of the example, and the Bazel groups of `core_web`
  and `core_release_host`: the same files in the same layout, and the manifests equal once the hashes (`sha256`, `imageId`,
  `shippedBytes`, which depend on the build's directory and toolchain) are masked; the Bazel host and web manifests each hold their own
  platform's entry (the CLI's one holds both).
* `cargo test` and `cargo clippy --all-targets -- -D warnings` in `examples/bazel`; `scripts/check-no-em-dash.sh`;
  `node site/scripts/build-all.mjs` and `check-links.mjs`.

## Not verified, and left

* **Linux.** There is no Linux machine or container here. The ELF path (`objcopy --only-keep-debug`, `llvm-symbolizer`, the GNU build id
  the report carries, which `undra build` does not ask the host linker for: it relies on the distribution's default) is covered by the
  `bazel-example` CI job running this test on its first push. If the runner has no `llvm-symbolizer` the test says so; the job may need
  `apt-get install llvm`.
* The iOS and Android cores' groups (`//:mobile_ios` needs Xcode; Android does not build in the example, ADR-061): the layout rule is the
  same for them, and iOS writes no file of its own.
* A web crash (a wasm trap, resolved with the function map) is not made under Bazel; the CLI's own tests cover that resolution.
