# Review: symbols-bazel (wt/symbols-bazel), 2026-10-05

Reviewer: the adversarial reviewer of the piece. The implementer's head was `281eb1b` (draft PR #31). The fixes below are
on the branch, one commit each.

## Verdict

**Clean after the fixes**, pending CI on the pushed head. The core of the piece is right: the old `symbols` group flattened
`build/symbols/` and dropped the host library's dSYM / `.so.debug`, so it could not resolve a host crash; the new layout
(`<target>.symbols/symbols/` and `<target>.symbols/host/`) is the CLI's, file for file, on macOS and on Linux. The reference
test fails for the right reasons and passes on both systems. What I found was one reliability gap on Linux (the build id
was the linker's default), one CI gap (no `llvm-symbolizer` guaranteed), a misleading docstring example, two inaccurate
sentences in the guide, and a check whose message claimed more than it tested. No High finding.

## Findings

| # | Sev | Finding | State |
|---|---|---|---|
| M1 | Med | The Linux host library's GNU build id was left to the linker's default. A panic report names its image by that id and the manifest keys `lib<ns>.so.debug` by it; the test asserts the two agree. `lld` writes none unless asked, and whether `cc` asks is a distribution's build choice (in the container below, gcc does: a plain cdylib gets a sha1 id, and an 8-byte one through `-fuse-ld=lld`, because gcc passes a bare `--build-id`). Elsewhere the report would carry `""` and the test would fail; `undra symbolicate` would fall back to the namespace with a warning. | fixed c30f456: `identity_args` passes `-Clink-arg=-Wl,--build-id=sha1` on Linux, as the Android build does (`BUILD_ID_LINK_ARG`); unit test; ADR-046 dated line |
| M2 | Med | The Linux CI job had no guaranteed `llvm-symbolizer` (the CLI looks for `llvm-symbolizer` / `llvm-addr2line` only; GNU `addr2line` does not take `--obj`, so accepting it in the test would not help). | fixed fdcb0d8: a step in `bazel-example` that installs the distribution's `llvm` only when neither `PATH` nor `/usr/lib/llvm-*/bin` has one; `ci-local.rb` skips it as provisioning |
| L1 | Low | `core.bzl`'s docstring example was `filegroup(srcs = [":core_host"], output_group = "symbols")`. With the macro's default (`release = False`) that target has no group, and Bazel gives such a filegroup no files and no error (shown with a probe filegroup over `//:core_host`). "iOS has none" was also off: its group holds the manifest. | fixed 6264e27 |
| L2 | Low | The guide said a stale symbol directory fails "with the CLI's error". On macOS it does (the dSYM's UUID is checked: shown, C0009 "it is the dSYM of another build"); on the ELF path the CLI only warns that the image id is not in the manifest and resolves with the file it has (`commands/symbolicate.rs`, `Format::Elf` reads no build id from the `.debug`). The reference test fails either way, on its own image-id check. The guide also did not say that a debug core's group is silently empty. | fixed 584cb6e (wording; `search-index.json`, `llms-full.txt` regenerated) |
| L3 | Low | The test printed "the core lives on" without calling the core after the panic. | fixed 769d3e9: `greeting("symbols")` after the panic, checked |
| L4 | Low | THEORY, CLI, out of the piece: `undra symbolicate` on an ELF entry does not compare the `.debug` file's build id with the report's (the Mach-O path does). A stale `.so.debug` in a one-entry manifest resolves to wrong lines with exit 0 and a warning. | open (a follow-up for the CLI) |
| L5 | Low | The CLI finds `llvm-symbolizer` on `PATH`, in the NDK and through `xcrun` only, not in `/usr/lib/llvm-*/bin`, where a distribution's versioned LLVM lives without the `llvm` metapackage. The test works around it by adding that directory to the CLI's `PATH`. | open (CLI follow-up) |
| I1 | Info | Merge interaction: `wt/ndk-input` edits `core.bzl`'s docstring on the Android bullet, the line just above the `symbols` paragraph this piece rewrites, and adds a job to `ci.yml` after `bazel-example-macos`. Expect a textual conflict in `core.bzl` at the second merge (keep both). The new CI step is placed after the cache step, away from the Bazelisk lines `wt/bazel-compat` may touch. | for the integrator |

Accepted as is: the layout change of the web group (`<target>.symbols/symbols/web/..` instead of `<target>.symbols/web/..`)
is breaking, nothing in the repository consumed it and 1.1 is unreleased; the record says so. The test-only API function
(`crash_for_symbols_test`) is described by the schema like any other (R1), documented, called by the symbols test and its
unit test only (`git grep`), and costs the example's web core +0.9 KB: 113,600 bytes gzipped (`gzip -9`) of Bazel's
`core_web`, as the README says. The 120,000-byte gate of `bench/budgets.toml` measures the `undra init` template, not this
example, so it is untouched. The frame is not inlined on arm64 macOS nor arm64 Linux, the same as the playground's
`explode` on every platform the CLI's symbol tests run. `DEVELOPER_DIR` reaches the test through `env_inherit`; the 60 s
`get` is a hang bound only; nothing sleeps.

## Verified, and how

On this Mac (arm64, Bazel 8.8.1 through Bazelisk, `source scripts/env.sh`):

* `bazel test //...` in `examples/bazel`: 7 of 7, before and after the fixes; the symbols test run uncached with
  `--test_output=all` resolves `0x96d03 hello_core::crash_for_symbols_test (/undra/app/core/src/lib.rs:154)`.
* The two failure modes, each by a temporary edit of `run.sh` (reverted, `git status` clean): the host twin left out of the
  group fails "the symbols group has the host library's symbols: []" and the CLI's C0009 "cannot read
  ...libhello_core.dylib.dSYM"; the dSYM's DWARF file emptied fails the CLI's "is not a Mach-O file". The image id was the
  same after the rebuild (the build is reproducible across sandbox directories).
* A stale symbol directory: the CLI-built symbols of a copy of the example (another image id) given to `undra symbolicate`
  with the Bazel report fail with C0009 (two entries) and, cut to the host entry alone, with the dSYM UUID check.
* The layout against the CLI: `undra build --release --platform host,web` of a copy of the example; `build/symbols/` and
  `build/host/<lib>.dSYM` are, file for file, the two Bazel groups; the manifests differ only in hashes and image ids.
* `bazel test //tests/...` in `bazel/`: 3 of 3. `cargo test`, `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --check` in `examples/bazel`; `cargo test -p undra-cli --lib` (446), `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo doc -p undra-cli` with `-D warnings`; `scripts/check-no-em-dash.sh`; `node
  site/scripts/build-all.mjs` (the regenerated files committed) and `check-links.mjs`; `scripts/ci-local.sh --list --only
  ci/bazel-example` lists the new step as skipped.

On Linux, in an `ubuntu:24.04` container on this Mac (aarch64, gcc 13.3, Rust 1.99.0, a non-root user, Bazel's
processwrapper sandbox):

* The CI step's script with no LLVM installed: it installs `llvm` and `/usr/bin/llvm-symbolizer` is there.
* `bazel test //tests/...` in `bazel/`: 3 of 3. `bazel test //...` in `examples/bazel`: 6 pass, `//swift:hello_test`
  skipped. The symbols test: the group is `symbols/manifest.json` and `host/libhello_core.so.debug`; the report's image id
  is the shipped `.so`'s sha1 build id and the manifest's; `undra symbolicate` (llvm-symbolizer 18) resolves
  `0x9d83 hello_core::crash_for_symbols_test (/undra/app/core/src/lib.rs:154)`.
* The CLI alone: `undra build --release --platform host` of the example and of the playground write `libX.so` and
  `libX.so.debug` with the same sha1 build id and `"symbols": "../host/libX.so.debug"`; the C harness of
  `crates/undra-cli/tests/symbols/` makes the stripped playground library panic and `undra symbolicate` resolves
  `playground_core::lab::explode` to `lab.rs:222`, the panic's own line.

## What could not be run

* **x86_64 Linux and the hosted runner image.** The container is aarch64 Ubuntu 24.04, not `ubuntu-latest` x86_64: rustc's
  default linker there is `lld` through `cc` (the build id is now explicit, so it no longer matters which), Bazel's
  `linux-sandbox` was not used (processwrapper), and whether the image already has `/usr/lib/llvm-*/bin/llvm-symbolizer`
  (the step installs `llvm` when not) is for the first CI run to show.
* The iOS and Android groups under Bazel (not built here: `//:mobile_ios` is manual, Android is `wt/ndk-input`'s), and a
  web crash resolved under Bazel (the CLI's own tests cover the wasm path).
* `scripts/ci-local.sh` in full was not run (the touched suites were run one by one above); CI on the pushed head is the
  gate.
