# ndk-input (ADR-065, the Android build drives the NDK directly; the NDK is a declared input under Bazel) - adversarial review

**Date:** 2026-10-05 · **Piece:** `wt/ndk-input` (draft PR #32), from `main` `58e373a`; reviewed at `0181650`, fixes after it · **Read:** `CLAUDE.md`,
`docs/AGENT_WORKFLOW.md`, `git diff origin/main...HEAD`, `.10x/decisions/sde/ndk-input.md`. **Method:** a reading review (the reviewer
could not run anything), then a fix round that ran the verification below on macOS (Apple silicon, NDK r27 27.2.12479018, Rust 1.99.0,
Bazel 8.8.1 through Bazelisk).

## Verdict

Merge after `wt/bazel-compat` lands and the notes at the end are done. Nothing blocking is open; the six findings are closed.

## Confirmed by reading (reviewer)

* The rust-std checksums in `MODULE.bazel` match the `.sha256` files of static.rust-lang.org.
* The NDK toolchain paths and host tags are right (`darwin-x86_64`, `linux-x86_64`, `windows-x86_64` with `.cmd` / `.exe`).
* `min_sdk` 21..99, and a teaching C0003 when the clang wrapper of the level is missing; the 32-bit ABIs still work (`armv7a-` names).
* The cargo-ndk removal is consistent: doctor, the fixture, the parser, the CI template and `tests/ci_workflow.rs`.
* `bazel test //...` fetches neither the NDK nor the Android standard library (both repositories are lazy).
* The CI wiring is in place (`bazel-android` in `complete`'s `needs`, the `android` job's plain `cargo build`).

## Findings and what was done

| # | Sev | Finding | What was done |
|---|---|---|---|
| 1 | Medium | `site/docs/getting-started.html` still listed `cargo-ndk` in the Android row of "What you need" (and so did `llms-full.txt` and `search-index.json`). | The row says "the Android SDK and NDK r27 (no `cargo-ndk`: Undra links with the NDK's clang itself)". `git grep cargo-ndk -- site`: the three left mentions (bazel.html twice via the index, getting-started) all say "no cargo-ndk". Repository-wide, the other mentions are the record, ADRs and reviews of 1.0.x (historical), the 0.0.9 upgrade fixture, doctor's "no cargo-ndk" test, and the sentences "there is no cargo-ndk to install". `node site/scripts/build-all.mjs` regenerated `llms-full.txt` and `search-index.json`; a second run changes nothing. Commit `cd216c8`. |
| 2 | Low | `bazel-android` passed `$(android/use-installed-ndk.sh ..)` inline; a failed substitution in an argument is not caught by `bash -e`, so Bazel would download the 664 MB archive. | `flags="$(android/use-installed-ndk.sh ..)"` on its own line (a failure stops the step), then `bazel build //:mobile_android $flags ..`, with a comment. Commit `ecdc5b5`. |
| 3 | Low | `release-smoke.yml` installed cargo-ndk unconditionally. | `case "$V" in 0.* \| 1.0.*) cargo install cargo-ndk ;; esac`, step renamed, `set -euo pipefail`, comment that 1.1.0 and later need none so the smoke proves a machine without it builds Android. `ci.yml` and `release-smoke.yml` parse as YAML (Ruby `YAML.load_file`). Same commit `ecdc5b5`. |
| 4 | Low (R8) | `undra.android_std(version, sha256s)` was not checked against the toolchain's rustc; a mismatch is E0514 whose advice, `cargo clean`, does not point at MODULE.bazel. | The std repository writes `UNDRA_ANDROID_STD_VERSION` (part of `@undra_android_std//:std`); `undra_core` passes it to the action as `std_version=`; `private/run.sh` compares it with `rustc -V` right after the rustc wrapper exists, before any build, and dies naming `undra.android_std(version = "<tag>") in MODULE.bazel`, the toolchain's release, the checksum URLs of the toolchain's release and the alternative (change the toolchain's `versions`). Rules test `//tests:std_version_test` (`sh_test` over `run.sh` with a fake rustc and a version file that is wrong and one that is right; the right one gets as far as the next failure, "no app_files"). Run by hand the message reads as intended. The end-to-end mismatch (a real wrong pin) was not run: it would need a second rust-std archive and its checksum. Commit `4227f3b`. |
| 5 | Low (theory) | On Windows the NDK's `.cmd` wrappers as the linker may mishandle rustc's arguments. | Fixed, because it was small and testable without Windows. `ndk::linker` is `clang.exe` on Windows and `ndk::rustc_args(os, triple, api)` adds `-C link-arg=--target=<triple><api>` there (`armv7a-linux-androideabi<api>` for the 32-bit ABI); the wrapper stays as `CC_`/`CXX_` and as the check that the NDK has the API level (`missing_tools` requires the wrapper and `clang.exe`). Three unit tests: the Windows layout and `--target` (both ABIs), no `--target` on macOS and Linux, the missing wrapper of an API level above the NDK's on Windows. Windows is unsupported and unrun (docs/RELEASING.md); the `.cmd` as `CC` is the part nobody has exercised. Commit `72de42f`. |
| 6 | Info | A `migrations.rs` note for 1.1.0, and the site deploying before the release. | `migrations.rs` cannot hold a 1.1.0 entry now (`the_table_is_valid_sorted_and_not_ahead_of_this_release` refuses a release newer than the workspace's 1.0.0), so the note text is in `.10x/decisions/sde/ndk-input.md` for the version pull request. `undra upgrade` moves pins only and does not touch a workflow's other lines; removing the `cargo install cargo-ndk` line itself was not done (the workflow is the author's file once written; the line is harmless). The record also says that the site deploys from `main`, so "no cargo-ndk" pages go live before 1.1.0 ships while 1.0.x still needs it. |

## Verification (macOS, with the fixes)

`source scripts/env.sh` first in each.

* `cargo test -p undra-cli`: green (455 unit tests, every integration binary; exit 0).
* `UNDRA_TEST_ANDROID=1 cargo test -p undra-cli --test platforms android`: 2 passed.
* `cargo fmt --check`: clean. `cargo clippy -p undra-cli --all-targets -- -D warnings`: clean.
* CLI smoke without cargo-ndk: a `PATH` of a directory of symlinks to `~/.cargo/bin/*` except `cargo-ndk`, then the other directories of `PATH` that do not hold it (checked: `command -v cargo-ndk` fails in that shell; run through `/bin/sh -c`, as zsh re-adds `~/.cargo/bin` and does not word-split `$PATH`). `undra init claude-ndk-smoke --dir <scratchpad> --platforms android --undra-path <worktree>`, then `undra build --platform android` and `--release` in it: both ABIs; "16 KB aligned"; release 898.5 KB (arm64-v8a) and 961.2 KB (x86_64), the sizes of the record. `llvm-readelf -lW`: ELF64 AArch64 and X86-64, every LOAD segment aligned `0x4000`; `llvm-readelf -n`: a GNU build id in both ABIs, the same in the shipped library and its unstripped twin (`e3922eb5..`, `ab4b1bca..`).
* `examples/bazel`: `flags="$(android/use-installed-ndk.sh "$ANDROID_HOME/ndk/27.2.12479018")"; bazel build //:mobile_android $flags` builds (run.sh changed, so a real rebuild, the version check passed against the example's pin: 40.9 and 41.0 MB debug, 16 KB aligned); `bazel build //android:hello --config=android` builds; `bazel test //...`: 6 of 6 pass. `MODULE.bazel.lock` of both workspaces: unchanged (`git status`).
* `bazel/`: `bazel test //tests/...`: 4 of 4 pass (the three existing and `std_version_test`).
* `bash scripts/check-no-em-dash.sh`, `node site/scripts/build-all.mjs` (no diff afterwards), `node site/scripts/check-links.mjs`: `check-no-em-dash.sh`: no em-dash in any tracked file; `build-all.mjs`: nothing changed on a second run; `check-links.mjs`: 54 pages OK.

## Not verified

* The Linux runs of `bazel-android` and of the `android` job's JNI step (they were written from commands that ran on macOS), and of `release-smoke.yml`'s new step (a case statement; it runs after the next release).
* The `http_archive` fetch of the NDK archives themselves (the SHA-256 values were taken from one download of each, 2026-10-05; the review did not download them). The build used the installed NDK through `use-installed-ndk.sh`.
* Windows: the linker change is unit-tested for its layout only.
* A real wrong pin of `undra.android_std` through Bazel (the rules test covers `run.sh`'s check with a fake rustc).
* `scripts/ci-local.sh` was not run by this fix round.

## For the integrator

* `wt/bazel-compat` lands first (it lowers the ruleset versions, adds Bazel 9.2.0 steps and regenerates both `MODULE.bazel.lock` files). After merging `main` into this branch: build `//:mobile_android` once more in `examples/bazel` on the lowered rulesets, `bazel test //tests/...` in `bazel/` (the new `sh_test` loads `@rules_shell`, which `bazel/MODULE.bazel` already depends on; check the version after the lowering), and look at both lock files.
* Text conflicts are expected in `.10x/decisions/sde/_index.md`, `site/docs/bazel.html` and `site/search-index.json` (regenerate the index with `node site/scripts/build-all.mjs`; `llms-full.txt` too).
* At release time: the 1.1.0 migration note in `.10x/decisions/sde/ndk-input.md`.
* Housekeeping: while checking `run.sh` by hand a stray `rm -rf /tmp/undra-bazel-*` ran, which can remove the work directories of Bazel actions that were running at that moment on this machine (their own `trap` removes them anyway at the end); a build of another worktree that failed oddly around 2026-10-05 on this Mac should simply be run again.
