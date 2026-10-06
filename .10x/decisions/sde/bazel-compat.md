# SDE: bazel-compat: Bazel 8.8 and 9.x, and the lowest ruleset versions the rules need (ADR-061), 2026-10-05

Branch `wt/bazel-compat`, from `main` `58e373a`. Goal: the Bazel rules support Bazel 8.8 and 9.x, and `bazel/MODULE.bazel` declares
the lowest version of each ruleset the rules need, tested at those versions, so an application on an older ruleset is not forced up.

## What changed

| Where | What |
|---|---|
| `bazel/MODULE.bazel` | every `bazel_dep` at its tested minimum (table below); comments say why |
| `examples/bazel/MODULE.bazel`, `examples/bazel/scripts/at-minimums.sh` | the example pins recent versions (rules_apple 5.2.0, rules_swift 4.1.2, rules_kotlin 2.4.20, aspect_rules_js 3.5.1, aspect_rules_ts 3.10.1, rules_rust 0.74.0: what most applications run); `at-minimums.sh` runs one Bazel command with each of its `bazel_dep` lines rewritten to the rules' version, `--check_direct_dependencies=error` (a minimum the graph raises fails the run) and `--lockfile_mode=off`, then restores the file (review round, below) |
| both `MODULE.bazel.lock` | regenerated; the committed lock is the one Bazel 8.8.1 (`.bazelversion`) writes. A 9.x run rewrites registry entries in it (different built-in modules), so the 9.x steps and the minimums run with `--lockfile_mode=off` and the tree stays clean |
| `.github/workflows/ci.yml` | `bazel-example` (Linux) runs the rules' tests and `bazel test //...` on 8.8.1 and again with `USE_BAZEL_VERSION=9.2.0`, and `//...` at the minimums on both; `bazel-example-macos` runs `//swift/...` and `//:mobile_ios` at both ruleset ends on both Bazels. Both jobs were already in the Gate (`complete.needs`) |
| `site/docs/bazel.html` (+ `llms-full.txt`, `search-index.json`) | "Set up" states Bazel 8.8 and 9.x, tested in CI; new section "Ruleset versions" (minimums, MVS picks the higher, why each floor) |
| `examples/bazel/README.md` | the 9.x command (`--lockfile_mode=off`), `scripts/at-minimums.sh`, the both-ends note, the lock-file note |

`bazel/README.md` does not exist; nothing to update. No rule source (`*.bzl`) needed a change for Bazel 9.2.0: they already load
`cc_library`, `sh_test`, `java_test` and friends from their rulesets, never the removed native rules.

## Minimums, and what a lower one does (each probed alone on Bazel 9.2.0 with the others at a working value; 8.8.1 re-run at the end)

| Ruleset | Was | Now | Why not lower |
|---|---|---|---|
| rules_kotlin | 2.4.20 | 2.3.0 | 2.2.2 and 2.2.0: `rules_android` too old, "The CcInfo symbol has been removed" (loaded through `undra_android_library`); 2.1.0: `name 'JavaPluginInfo' is not defined` |
| rules_swift | 4.1.2 | 3.4.0 | 3.3.0 and 3.1.2: the Swift worker's `@nlohmann_json//:json` target is not declared on Bazel 9; 3.0.0 is yanked |
| rules_apple | 5.2.0 | 4.1.0 | forced by rules_swift 3.4.0 (a lower declaration resolves to 4.1.0) |
| aspect_rules_ts | 3.10.1 | 3.10.0 | 3.9.0 and older have no `typescript` module extension (`undra_rules` uses it) |
| aspect_rules_js | 3.5.1 | 2.0.0 | forced by aspect_rules_ts 3.10.0 (1.42.3 and 1.32.0 resolve to 2.0.0) |
| rules_rust | 0.74.0 | 0.70.0 | 0.69.0, 0.68.1, 0.65.0, 0.60.0: "No matching toolchains found for types @bazel_tools//tools/cpp:toolchain_type" (rules_rust's allocator library, wasm target) |
| rules_nodejs | 6.7.5 | 6.7.5 | 6.7.4 and 6.7.3 do not know Node 24.18.0 (the example's pin): "No nodejs is available for darwin_arm64 at version 24.18.0". The rules alone would take 6.7.3 (what aspect_rules_js asks) |
| apple_support / rules_cc / rules_java / rules_shell / platforms / bazel_skylib / bazel_lib | 2.8.4 / 0.2.20 / 9.3.0 / 0.6.1 / 1.1.0 / 1.9.2 / 3.7.0 | 1.24.2 / 0.2.17 / 9.3.0 / 0.6.1 / 1.1.0 / 1.8.2 / 3.1.0 | not independently lowerable: declared lower (apple_support 1.17.1, rules_cc 0.1.0, rules_java 8.6.0, rules_shell 0.3.0, platforms 0.0.10, skylib 1.7.1, bazel_lib 3.0.0) they resolve up to these (`bazel mod graph` and the root-module warning); rules_cc is 0.2.16 on Bazel 8.8.1 and 0.2.17 on 9.2.0, so 0.2.17 |

The first candidates (rules_apple 5.0.1, rules_swift 4.0.1, rules_kotlin 2.4.10, aspect_rules_js 3.4.1) also passed on both Bazels; they
were then pushed down until something failed, which is what the table records. rules_apple's own 3.x line was not isolated: a 3.16.0
declaration with rules_swift 3.4.0 or 3.3.0 printed a module-extension load error from `@rules_apple//apple:apple.bzl` on Bazel 9, and the
root warning that rules_apple resolved to 4.1.0.

## Verified (macOS, Apple silicon, Xcode, local; Bazel from Bazelisk)

```sh
source scripts/env.sh
R=$HOME/.cache/bazel-repository
for v in 9.2.0 8.8.1; do                       # 9.2.0 is the newest 9.x release (9.3.0 at rc4 on 2026-10-05); 8.8.1 is .bazelversion
  (cd bazel          && USE_BAZEL_VERSION=$v bazel test //tests/... --lockfile_mode=off --repository_cache=$R --test_output=errors --nocache_test_results)   # 3 of 3
  (cd examples/bazel && USE_BAZEL_VERSION=$v bazel test //...       --lockfile_mode=off --repository_cache=$R --test_output=errors --nocache_test_results)   # 6 of 6, recent rulesets
  (cd examples/bazel && USE_BAZEL_VERSION=$v bazel build //:mobile_ios --lockfile_mode=off --repository_cache=$R)                                          # the XCFramework
  USE_BAZEL_VERSION=$v examples/bazel/scripts/at-minimums.sh test //... --repository_cache=$R --test_output=errors --nocache_test_results          # 6 of 6, minimums
  USE_BAZEL_VERSION=$v examples/bazel/scripts/at-minimums.sh build //:mobile_ios --repository_cache=$R
done
node site/scripts/build-all.mjs && node site/scripts/check-links.mjs     # 54 pages OK
bash scripts/check-no-em-dash.sh
```

Before any change `bazel test //...` already passed on 9.2.0 with the old pins (80 s on 8.8.1, warm repository cache), so Bazel 9 itself
needed no fix. After the change the example, the rules' tests and `//:mobile_ios` pass on both Bazels, and no root-module version warning
is printed on either (the declared versions are what resolves).

## Not verified / limits

* Linux: not runnable here. The Linux job's JVM, Node and ktlint targets are the same Starlark and rulesets, but the first hosted run of the
  9.x steps is the proof; a failure there on Linux only would show up as a step "on Bazel 9.x".
* Bazel 9.0.x and 9.1.x are not run; 9.2.0 is the one pin. The 9.3.0 release candidates were not tried.
* The Android core (`//:mobile_android`) and `//android:hello` stay as before (declared / by hand); `//android` loads on both Bazels.
* The minimums were found on macOS arm64 (host and web cores, Swift, Kotlin, TypeScript, the iOS XCFramework).
* `rules_nodejs` is held at 6.7.5 only because the example's Node pin needs it; moving the example to Node 24.13.0 would let both files declare 6.7.3.

## Review round (2026-10-05, `.10x/reviews/2026-10-05-bazel-compat-review.md`)

The first version pinned the example to the minimums, so CI tested only the oldest rulesets and a break in the versions most
applications run would have gone unnoticed. The example is back at the versions it had (recent ones), and
`examples/bazel/scripts/at-minimums.sh` runs it at the minimums: CI now runs both ends on both Bazels (the reproduce block above). A
minimum is the lowest that works on both Bazels: rules_swift 3.3.0 and rules_kotlin 2.2.2 still work on 8.8.1 (Swift consumer built;
`//android:hello` loaded), and rules_rust 0.69.0 fails on both. `//android:hello --config=android` (`kt_android_library`) builds at both
ends on both Bazels, by hand (the Android SDK). The guide's setup sample shows rules_rust 0.74.0, "0.70.0 or later".
