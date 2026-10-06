# user-lens-fixes: the 1.1 user-lens regression review's findings (2026-10-06)

On `wt/user-lens-fixes`, branched from `wt/undra-1-1` (PR #40), the last piece before the 1.1.0 tag. Every finding is
fixed; nothing is deferred.

## H1: an Android build whose dependency runs `bindgen` failed (a regression against 1.0)

Cause: 1.0 built through `cargo-ndk`, which exported `BINDGEN_EXTRA_CLANG_ARGS_<triple>`, `CLANG_PATH`, `CFLAGS_<triple>`
and the CMake variables. ADR-065's `ndk::environment` set only what Cargo links with (linker, `CC`/`CXX`, `AR`/`RANLIB`,
`ANDROID_NDK_HOME`), so the host's libclang parsed `<stdio.h>` with no Android sysroot. The `cmake` crate failed too:
without a toolchain file it asks CMake's own Android support for the NDK, which reads `ANDROID_NDK_ROOT`, not
`ANDROID_NDK_HOME`.

Fix (`crates/undra-cli/src/builds/ndk.rs`, `builds/android.rs`): per ABI, `BINDGEN_EXTRA_CLANG_ARGS_<triple>` =
`--sysroot=<sysroot> --target=<clang triple><min_sdk> -I<sysroot>/usr/include/<sysroot triple>` (cargo-ndk 4.1.2's
value, from its source in the local registry, plus the target at the API level so the headers declare what `min_sdk`
has; `arm-linux-androideabi` for 32-bit ARM's include; quoted for `bindgen`'s shell split when a path has a space),
`CLANG_PATH` = the NDK's `bin/clang`, `ANDROID_ABI` and `ANDROID_PLATFORM` (as cargo-ndk exported them), and
`CMAKE_TOOLCHAIN_FILE_<triple>` = `<target>/undra-ndk/<triple>-<api>/android.toolchain.cmake`, written by the build: it
sets `ANDROID_ABI` and `ANDROID_PLATFORM` unless the build script defined them and includes the NDK's
`build/cmake/android.toolchain.cmake`. The NDK's file reads those two only as CMake variables (no `$ENV{}` anywhere in
it) and the `cmake` crate passes none, so pointing `CMAKE_TOOLCHAIN_FILE_<triple>` at the NDK's file alone would build
`armeabi-v7a` objects for every ABI. Precedence (there was none): a variable is left out when the user set it or a name
its reader takes first or instead (`BINDGEN_EXTRA_CLANG_ARGS`, `..._<triple-with-dashes>`, `TARGET_CMAKE_TOOLCHAIN_FILE`,
`CMAKE_TOOLCHAIN_FILE`); the linker and compiler variables are always set. Not set: `CFLAGS_<triple>` (cargo-ndk needed
it for a plain clang; the NDK wrapper carries its target) and `CARGO_NDK_*` (no build script in the local registry reads
them, and they would claim a cargo-ndk that is not there). ADR-065 amended; 1.1.0 migration note (Changed).

Proof: unit tests over `environment()` (each variable for arm64 on a Mac, x86_64 and 32-bit ARM on Linux, Windows'
`clang.exe`, a quoted path, every user-set form kept, another triple's variable ignored) and over the toolchain text.
End to end: a scratch crate `sysprobe` (build script: `cc` compiles a C file that includes `<stdio.h>` and
`<android/log.h>`, the `cmake` crate builds a static library whose `cprobe_abi()` returns 1000 x ABI + `__ANDROID_API__`,
`bindgen` over a header with `<stdio.h>` and `<android/log.h>`; its `#[no_mangle] sysprobe_check` makes the link resolve
both C libraries) as a dependency of the core of a fresh `undra init --undra-path` project, built offline with the SDK's
CMake 3.22.1 on `PATH`. The 1.1 binary before the fix: CMake "Neither the NDK or a standalone toolchain was found", and
with the CMake variable supplied by hand, `wrapper.h:1:10: fatal error: 'stdio.h' file not found`, then C0004. After:
`undra build --platform android` and `--release` both exit 0 (both ABIs, 16 KB aligned, release 899.0 KB and 961.0 KB);
the CMake objects are AArch64 and x86-64, and the linked `cprobe_abi` returns 1026 on arm64 and 2026 on x86_64 (the ABI
and API 26 each).

## M4: an `ANDROID_NDK_HOME` that is not a directory was skipped silently

Cause: `toolchain::find_android_ndk` skipped any NDK variable that was not a directory and fell back to the SDK's newest
NDK; the "not set; using ..." note was suppressed because the variable was set; `undra doctor` said `ok ANDROID_NDK_HOME is
<path>`.

Fix: the first NDK variable that is set decides (`NDK_VARIABLES`); when it is not a directory, `find_android_ndk` returns
`UnusableNdk`, `Toolchain::android_ndk` stays `None`, and `undra build --platform android` stops with a C0003 naming the
variable and the path (what, why, fix, docs link). `undra doctor` fails both `android.ndk` and `android.ndk-home` with
that message, and the fix exports the variable to the SDK's newest NDK (or `unset`s it when the SDK has none).

Proof: unit tests (toolchain: the error text, no fallback, no note, first-set-wins, unset still finds the SDK's NDK;
doctor: fail, observed path, the fix, `passed()` false). The real binary with `ANDROID_NDK_HOME=/typo/ndk`: the build
exits 1 with the C0003; doctor prints two FAILs with `export ANDROID_NDK_HOME=<sdk>/ndk/27.2.12479018` (the old binary:
`ok Android NDK r27 at <sdk>/ndk/27.2...` and `ok ANDROID_NDK_HOME is /typo/ndk`).

## M1, M2, M3: three pages said what is not so

* `getting-started.html`: the Run Script's last line (`exec env -i ...`) ends `--configuration "$CONFIGURATION"`, quoted
  (`templates/ios/project.pbxproj`); the unquoted text is only on its `echo "note: ..."` line. The iOS cell now says to
  edit the last line's quoted form.
* `hosts.html`: "both read one state during the move" contradicted `docs/REACT_NATIVE.md` (a core image is initialised
  once per process). Rewritten: the core and its stores are unchanged, native screens bind to the Swift or Kotlin classes
  of the same stores, one process holds one instance of a core, so the host switches per app, not per screen; the limit
  is linked; `undra adopt` for an existing native app.
* `cookbook/leaderboard.html` (and `bench/RESULTS.md` finding 9, which said the same): `-- --nocapture leaderboard` ran
  0 tests (checked: "running 0 tests ... 7 filtered out"). Now `UNDRA_BENCH_FILTER=leaderboard cargo test -p undra-bench
  --test budgets --release -- --nocapture --exact budgets` (checked: 1 test, the four `leaderboard/` rows and the ratio).
  `stall_report` defaults to 5 s (`bench/tests/leaderboard.rs`); the page and the record now give
  `UNDRA_STRESS_SECONDS=8` (checked: "8 s per design").

## L1 to L4

* L1, `docs/ONBOARDING.md`: counted from the BUILD files. The example's `bazel test //...`: `//:bindings_check`,
  `//kotlin:{hello_test,lint_test}`, `//swift:hello_test` (macOS), `//symbols:symbolicate_test`,
  `//consumer:{summary_test,runtime_test}`, `//ts:hello_test` and the four `//tests:machine_path_tests_*` analysis tests
  (the iOS one macOS only); `//ios:symbols_test` is `manual`. 12 on macOS, 10 run on Linux. The rules' `//tests/...`:
  2 (cargo_vendor) + 1 (actions) + 9 (bindings: 2 identical, 5 `*_fails_test`, `update_writes_test`, `copies_test`) +
  `std_version_test` + `stage_lock_test` = 14. The brief's "8 of 8 and 13 of 13" were the integration review's counts at
  `4d29d6e`, before `7b94f39` added the machine-path suite and before `stage_lock_test`. Not run (Bazel would fetch).
* L2, `docs/REACT_NATIVE.md`: "a Mac shared with other agents' builds" (twice) is now "a Mac under load from other
  builds". The 0.87 playground pins Gradle 9.4.1 (`gradle-wrapper.properties`) and `kotlinVersion = "2.2.0"`
  (`android/build.gradle`); Gradle 9.4.1's distribution on disk carries `kotlin-stdlib-2.3.0.jar` (9.3.1: 2.2.21). Both
  places now say "Gradle 9.4.1, which bundles Kotlin 2.3.0" and name 2.2.0 as the Kotlin Gradle plugin.
* L3: the 1.1.0 Changed note about `cargo-ndk` says `undra doctor --json` has no `android.cargo-ndk` finding and a tool
  reading the JSON should drop that id.
* L4: `undra upgrade` moves the tag of an Undra dependency and keeps its URL (`move_dependency`); the stand-in is written
  only in place of the old `undra-swift` package URL (`edit_pbxproj`). `Dist::announced_for_upgrade` warns that. The
  upgrade then runs `undra bindgen`, whose own warning was untrue for the same project (the Swift package follows
  core/Cargo.toml's repository, `follow_repository`): bindgen now says which repository the package names when the core
  has one. Unit tests for both texts.

## The two size claims

* `debug_size_hint` measured the unstripped library in `target/` (6.5 MB). A release build now writes the shipped
  (stripped) size beside Cargo's output (`<lib>.shipped-size`) and the hint reads it ("the last release build shipped
  961.0 KB, 42x smaller", measured on the scratch project after a release build); without the record, the rule of thumb.
  Unit test: an unstripped library alone gives the rule of thumb, the record gives the number.
* The generated README said a release core is "about 1.5 MB per ABI"; the starter core is 899 KB (arm64) and 961 KB
  (x86_64): "about 0.9 MB per ABI for the starter core". `examples/fieldbook/README.md` carries a copy of the old
  sentence; Fieldbook's core is larger and was not measured, so it is left.

## Checks

`cargo test -p undra-cli` (every suite, 24 binaries, all pass), `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check`, `scripts/check-no-em-dash.sh`, `node site/scripts/build-all.mjs` (search index and llms-full
regenerated), `node site/scripts/check-links.mjs` (56 pages OK).

## Reviewed (2026-10-06, `.10x/reviews/2026-10-06-user-lens-and-sse-review.md`)

Reproduced with the CLI built from `wt/user-lens-pr` on the same scratch project: `undra build --platform android --release`
exits 0 with 899.0 KB (arm64-v8a) and 961.0 KB (x86_64), the records hold 898960 and 961048 bytes, the debug build's hint
says "the last release build shipped 961.0 KB, 42x smaller"; with `sysprobe`'s build directories removed, its build script
ran again under this CLI and the `cc` object and the CMake archive are AArch64 and x86-64 per ABI. `cargo test -p undra-cli`:
24 binaries, 601 passed, 0 failed, 3 ignored. The `cprobe_abi` values (1026, 2026) need a device and stand as the author's
claim. The Bazel counts were recounted from the BUILD files (12, 10, 14) and not run.
