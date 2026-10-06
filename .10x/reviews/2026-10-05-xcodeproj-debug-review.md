# Review: xcodeproj-debug (wt/xcodeproj-debug), 2026-10-05

Reviewer: the adversarial reviewer of the piece. The implementer's head was `847e744` (draft PR #37). The fixes below are on the
branch, one commit each; the reviewed head is `b507d81` plus this file.

## Verdict

**Clean after the fixes**, pending CI on the pushed head. The piece does what it says: on this Mac the breakpoint on
`hello_core::greeting` stops at `lib.rs:145:5` with the dispatcher, `undra_call_sync`, `InprocTransport.callSync`, the generated
`greeting(name:ctx:)` and `HelloApp.start()` (`HelloApp.swift:23:24`, frame 47), both from the Xcode project (`xcodebuild`, LLDB from
the rules_xcodeproj execution root) and from a plain `bazel build` (LLDB from `bazel info execution_root`). `//ios:symbols_test`
passes with `keep_debug_objects` and fails without it, for the reason it prints. No High finding. What I found: a lock gap in the
new cleanup, a wrong path in the guide's LLDB step, the new test run by no job, coupling of the root package to `//ios`, an app
that would show an error if SwiftUI made its scene again, and several doc inaccuracies.

## Findings

| # | Sev | Finding | State |
|---|---|---|---|
| M1 | Med | `run.sh`'s `keep_debug_objects` cleanup moved the objects to `$WORK.objects`, ran `rm -rf "$WORK"`, then `mv "$WORK.objects" "$WORK"` and rewrote the owner file. Between `rm` and `mv` the lock (the directory) did not exist. A build of the same target waiting in another output base (Xcode's and the command line's share the directory, which the piece relies on) takes it in that window. The `mv` then lands *inside* that build's directory, and the owner file is overwritten with the exiting pid, so a third build sees a dead owner and removes the second build's directory while it is building. Reproduced with the lock loop and cleanup of `run.sh` verbatim, plus a pause in the window: `B: MY BUILD DIRECTORY WAS REMOVED UNDER ME`. | fixed 97c447b: the directory is pruned where it is (`target` renamed aside inside `$WORK`, the objects moved back, everything else but `target` and the owner file removed), so the lock is held until the process exits. Same harness: B waits, takes over after A exits, and C after B. The iOS core rebuilt with it, the dSYM test passes, the kept directory is 700 and 156 MB |
| M2 | Med | The guide, the README and the record said to run LLDB from `$(bazel info output_base)/../rules_xcodeproj.noindex/build_output_base/execroot/_main`. That directory does not exist: rules_xcodeproj's output base is *inside* the example's (`bazel run //ios:xcodeproj` prints `.../<output_base>/rules_xcodeproj.noindex/build_output_base/execroot/_main/...`). The step fails at its `cd` as written. | fixed 65a8c7e (and the record); the breakpoint was then hit from the corrected directory, Swift lines included |
| M3 | Med | `//ios:symbols_test` is the only check of `keep_debug_objects`, and it was `manual` and run by no CI job, so a change to `run.sh`'s cleanup (M1's fix is one) would regress the guarantee unseen. | fixed 96a1027: the macOS Bazel job runs it after the XCFramework build, with `--ios_multi_cpus=sim_arm64 -c dbg --apple_generate_dsym`. `scripts/ci-local.sh --only ci/bazel-example-macos` in a fresh clone: green, 1m40s |
| M4 | Med | `//:mobile` selected `keep_debug_objects` on `//ios:dbg`, so analysing the cores (`//:mobile_ios`, CI's `bazel build //:mobile_ios`) needed the `//ios` package and with it `@rules_xcodeproj`. Nothing could build the cores without rules_xcodeproj, which matters at once for `wt/bazel-compat` (I1). | fixed cb08d92: the `dbg` config_setting is in the root package. Verified: with the `rules_xcodeproj` bazel_dep removed and `--deleted_packages=ios`, `bazel test //...` is 6 of 6 and `//:mobile_ios` builds |
| L1 | Low | The app loaded the core inside `.task`. A core loads once per process (the generated `load` documents `UndraLoadError` when it is already loaded), and SwiftUI runs `.task` again for a scene it makes again, so the app would show "The core did not start". THEORY for the trigger (a scene the system discards and recreates; not reproduced). Separately confirmed: `app_lib` compiled with rules_swift's default `-swift-version 5` (aquery), against CLAUDE.md's Swift 6 rule. | fixed ebe9280: the load is a `static let` `Result`, `app_lib` has `-swift-version 6` (aquery shows it last). No warning; the lines the guide quotes (23, frame 47) are unchanged and the breakpoint was hit again |
| L2 | Low | `keep_debug_objects` reached every platform of `//:mobile`, Android included. Code path: `stage_key` gains `|keep_debug_objects` (another directory, other bytes for a `-c dbg` `.so`) and the cleanup keeps an empty directory, since the Android library is a cdylib with no `libundra_core_*.a`. Not built here (no NDK in this review). | fixed 1ff9e66: the macro passes it to the `ios` core only; `cquery` under `-c dbg`: `//:mobile_ios` has it, `//:mobile_android` does not |
| L3 | Low | `cfg = "exec"` on `undra_bindings.core` is right (the bindgen action runs on the execution platform, as the CLI does), but the cost is not "on a Mac": on Linux too `aquery` shows two `UndraBuild` actions for `//:core_host` (`aarch64-fastbuild` for the JVM tests, `aarch64-opt-exec` for bindgen), so CI builds the host core twice. Nothing in `bazel/tests` covers the attribute; the example covers it (bindings consumed on both systems). | documented 65a8c7e: Good to know and the `core` docstring of `undra_bindings` |
| L4 | Low | The guide's plain-Bazel path stopped at `bazel-bin/ios/app.ipa` with no install step, and its Swift-lines claim ("for a plain `bazel build` it is `bazel info execution_root`") was not verified by the author. | fixed 65a8c7e (unzip, install `Payload/app.app`); verified: from `bazel info execution_root` the backtrace has `Objects.swift:11:28` and `HelloApp.swift:23:24` |
| L5 | Low | The kept objects also go when macOS clears `/tmp`: `/usr/libexec/tmp_cleaner` (launchd, daily) removes files whose atime, mtime and ctime are all over three days old (`daily_clean_tmps_days="3"`), and a restart clears `/tmp`; Bazel's outputs stay, so the app then has no Rust frames. The docs said only "until the next build", and the remedy ("a flag the action reads") was vague. | documented b507d81, with a verified rebuild: `--action_env=UNDRA_REBUILD=<new value>` rebuilt the iOS core (the iOS action reads the shell environment) |
| L6 | Low | The record said the lock diff was "two lines for the module, nothing else moved". `bazel mod graph` on main and on the branch (Linux): no version the example resolved moved, and six modules are added with rules_xcodeproj (`rsync`, `openssl`, `lz4`, `xxhash`, `zstd`, `rules_perl`); the lock also gained extension entries. | record corrected 64083d4 |
| I1 | Info | Merge with `wt/bazel-compat` (not yet on main): its `examples/bazel/scripts/at-minimums.sh` exits 1 for a bazel_dep of the example that `bazel/MODULE.bazel` does not declare, which `rules_xcodeproj` is, and its `--check_direct_dependencies=error` would fail anyway because rules_xcodeproj 4.1.0 asks for `rules_apple` 4.4.0 and `rules_swift` 3.5.0, above the rules' minimums (4.1.0 and 3.4.0). Every at-minimums step goes red once both are on main. The fix belongs in that script at merge: drop the `rules_xcodeproj` line from the rewritten MODULE.bazel and add `--deleted_packages=ios` (M4 makes that enough; verified on this branch). `--ignore_dev_dependency` is no way out: it also drops the example's `local_path_override` of `undra_rules` (tried). Bazel 9.2.0 itself is fine: `bazel test //...` on Linux (5 pass, Swift skipped) and a `cquery` of `//ios:app`, `//ios:xcodeproj`, `//ios:symbols_test` on macOS both work. `ci.yml` also conflicts textually (both add lines at the end of the macOS step: keep both). | for the integrator |
| I2 | Info | Merge with `wt/symbols-bazel`: conflicts in `.10x/decisions/sde/_index.md`, `site/docs/bazel.html` (both add an `h2` section after Platforms and a TOC entry: keep both), `site/llms-full.txt` and `site/search-index.json` (regenerate with `node site/scripts/build-all.mjs`). `run.sh`, `core.bzl`, `examples/bazel/BUILD.bazel` and the README merge cleanly (checked with `git merge-tree`). `greeting` stays at `lib.rs:145` after its new function, so the guide's output holds. Semantic: symbols-bazel's guide says for iOS "the Rust frames are in your app's dSYM, which Xcode already makes", and its `core.bzl` docstring the same. Under Bazel that is true only when the iOS core is built with `keep_debug_objects = True` in the build that makes the dSYM, release cores included (the kept paths cover `release*`): the default `//:mobile_ios` dSYM has Swift only (shown by this piece's test failing with the attribute off). A cross-reference at merge. | for the integrator |
| I3 | Info | Merge with `wt/ndk-input`: textual conflicts in `run.sh` (both add a `case` arm: keep both), `core.bzl` (both add attributes and docstring entries: keep both; its new `extra_path` text replaces main's) and the README's tree (keep both lines). | for the integrator |
| I4 | Info | Out of the piece, pre-existing: the macOS host core (`//:core_host`, debug) is a dylib with a debug map too (97 `OSO` entries into `/tmp/undra-bazel-<key>/target/.../deps`, no DWARF of its own), so LLDB in `//swift:hello_test` finds no Rust lines either. `keep_debug_objects` covers `ios` only (L2). | a follow-up if host debugging under Bazel matters |
| I5 | Info | On Linux `bazel build //:mobile_ios` now fails with "didn't satisfy constraint @@platforms//:incompatible" instead of naming macOS (the `select` default). Correct, and the same shape `undra_swift_library` has. | accepted |

Checked and fine: the stage key (`label|platform|keep_debug_objects`, hashed with the mode) gives the debug core a directory of its
own; the kept directory is mode 700, owned by the user, 156 MB (two 78 MB `libundra_core_*.a`); the iOS core's new
`target_compatible_with` still refuses Linux (above) and accepts an iOS target platform (the app builds); the bundle id
(`dev.undra.bazel.hello.app`) and minimum OS (17.0) match the playground's conventions; the `.lldbinit` the CLI writes is unchanged
(no `crates/` diff); the formatter warning is accurate: with that file sourced, `frame variable name` prints `"Xcode"`, `bt 3`
works and `bt -c 60` ends LLDB with exit 132, as the guide says; no outside party is named in the new text; no em-dash.

## What I ran

On this Mac (Bazel 8.8.1, Xcode 26.6, Rust 1.99.0, the booted iPhone 17 Pro simulator, iOS 26.5):

* `bazel test //...` in `examples/bazel`: 6 of 6, before and after every fix; `bazel test //tests/...` in `bazel/`: 3 of 3.
* `bazel build //ios:app --ios_multi_cpus=sim_arm64 -c dbg --apple_generate_dsym` and `//ios:symbols_test` with the same flags:
  passes; with `keep_debug_objects` set to False in `BUILD.bazel`: `FAIL the dSYM has no line table for lib.rs`, twice (before and
  after M1's fix).
* `bazel run //ios:xcodeproj`, `xcodebuild ... build` (`** BUILD SUCCEEDED **`), `simctl install`, `simctl launch
  --wait-for-debugger`, LLDB from the rules_xcodeproj execution root: the breakpoint and backtrace above. The same from a plain
  Bazel build after L1's change, from `bazel info execution_root`.
* `bash scripts/check-no-em-dash.sh`, `node site/scripts/build-all.mjs` (no diff after the regenerated files were committed),
  `node site/scripts/check-links.mjs` (54 pages OK), `scripts/ci-local.sh --only ci/bazel-example-macos` (green).

On Linux, in an `ubuntu:24.04` container on this Mac (aarch64, a non-root user, Bazel 8.8.1 from Bazelisk 1.29.0, processwrapper
sandbox), on the reviewed head: `bazel test //tests/...` 3 of 3; `bazel test //...` in the example 5 pass and `//swift:hello_test`
skipped, the same as main; `bazel query //ios:all` lists the package; `bazel build //... -c dbg --nobuild` analyses; `bazel build
//:mobile_ios` is refused as incompatible. With `USE_BAZEL_VERSION=9.2.0`: `bazel test //...` 5 pass, 1 skipped.

**Linux decision: no gating.** rules_xcodeproj 4.1.0 loads and its `//ios` package analyses on Linux without error (its targets are
`manual`, so `//...` never builds them; loading `defs.bzl` fetches no macOS-only repository). `--deleted_packages=ios` or a
`.bazelignore` would only hide a package that works; after M4 they remain available to a run that must not have rules_xcodeproj
(I1).

## What could not be verified

* x86_64 Linux and the hosted `ubuntu-latest` image (the container is aarch64), and the `macos-15` runner (its Xcode is older than
  26.6) for the new CI step: CI on the pushed head is the gate.
* A physical device, Xcode's own UI (the project was built with `xcodebuild`), and the scene-recreation path of L1.
* The Android core with the attribute (no NDK here; L2 is from the code path).
