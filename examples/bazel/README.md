# Undra under Bazel

A Bazel workspace around the `undra init` to-do core (ADR-061). `bazel test //...` builds the core, generates its bindings and runs
the Kotlin, TypeScript and (on macOS) Swift code that uses them against the real core.

```
examples/bazel/
  MODULE.bazel      undra_rules (bazel/, by path), rules_rust 1.99.0 pinned by checksum, rules_kotlin, rules_swift, aspect_rules_ts/js, at recent versions
  scripts/          at-minimums.sh: one Bazel command with every ruleset at the lowest version undra_rules declares
  undra.toml        the project file `undra build` and `undra bindgen` read; [core] namespace = "hello_core"
  Cargo.toml, core/ the core: a store, a typed error and a function
  BUILD.bazel       undra_core, undra_bindings, undra_bindings_test, undra_ts_library
  committed/        the bindings, committed too: the Swift, Kotlin and TypeScript trees `undra_bindings` produces
  kotlin/           the JVM test (JNI against libhello_core) and the ktlint test of the generated Kotlin
  ts/               the Node test (hello_core.wasm through the compiled bindings)
  swift/            the Swift test (macOS): the core in process
  consumer/         what an app team adds on top: its own Kotlin library over the store, a Node test importing the runtime
  android/          the bindings as an Android library (manual: --config=android, the Android SDK), the NDK repositories' BUILD file and the script that uses an installed NDK instead of the archive
```

```sh
cd examples/bazel
bazel test //...                      # needs Bazelisk; .bazelversion pins Bazel 8.8.1, the lowest supported
USE_BAZEL_VERSION=9.2.0 bazel test //... --lockfile_mode=off   # the newest 9.x, which CI tests too
scripts/at-minimums.sh test //...     # every ruleset at undra_rules's minimum (any Bazel command; USE_BAZEL_VERSION works)
bazel build //:core_web               # bazel-bin/core_web/hello_core.wasm: 112.7 KB gzipped (2026-10-02), budget 120 KB
bazel build //:bindings               # the Swift, Kotlin and TypeScript trees, as outputs
bazel build //:mobile_ios             # macOS: the XCFramework (manual target)
bazel build //:mobile_android         # jniLibs/<abi>/libhello_core.so, linked with the NDK, a declared input (manual target; downloads it)
bazel build //:mobile_android $(android/use-installed-ndk.sh)   # the same with the NDK sdkmanager installed (CI does this)
bazel build //android:hello --config=android   # the bindings as an Android library (ANDROID_HOME)
```

| Target | What it proves |
|---|---|
| `//kotlin:hello_test` | the Kotlin bindings, the Kotlin runtime and `core_host` work together: a call, a store's signals, a typed error, a command, through JNI |
| `//ts:hello_test` | the same through the TypeScript bindings (type-checked by `tsc` against the compiled runtime) and `core_web` under Node |
| `//swift:hello_test` | the same in Swift, with the XCFramework's host twin linked (macOS only; skipped on Linux) |
| `//kotlin:lint_test` | ktlint 1.8 reports nothing on the generated Kotlin, and 90 findings once the exclusions `undra bindgen` writes are taken away; the root `.editorconfig` section the Bazel guide gives for `lint_exclusions = "none"` is enough on its own |
| `//:bindings_check` | `committed/` is what the core's schema generates, byte for byte (`undra bindgen --check`'s comparison); when it is not, the failure says `bazel run //:bindings_check.update`, which rewrites it |
| `//kotlin:lint_test` | ktlint 1.8 reports nothing on the generated Kotlin, and 90 findings once the exclusions `undra bindgen` writes are taken away |
| `//consumer:summary_test` | a Kotlin library an app writes over the generated store compiles and runs, the core loaded by `-Dundra.native.hello_core.path=$(rootpath //:core_host)` |
| `//consumer:runtime_test` | a Node test that imports `@undra/runtime` beside the bindings resolves both |

## What is hermetic, and what is not

The actions run the `undra` CLI of this checkout with the Rust toolchain Bazel resolved, crates.io packages downloaded by the
checksum `Cargo.lock` records and unpacked into a Cargo directory source, and no network. An action copies exactly the files its
target declares into a scratch directory named by the target (`/tmp/undra-bazel-<key>`), so a core built twice, or in two
checkouts, is the same file. The C linker (and Xcode for iOS) are the machine's.

**The Android core** (`//:mobile_android`, ADR-065) has no machine inputs. `undra_core(ndk = ..)` takes the NDK as a label, an
input of the action, and the action sets `ANDROID_NDK_HOME` to it itself: no `--action_env`, no `extra_path`, no `cargo-ndk`. It is
a Cargo cross-build that `undra build` links with the NDK's clang, so the target does not transition to an Android platform (that
would need a C++ toolchain for one, `rules_android_ndk`) and runs with the host's Rust toolchain; the standard library of the
Android targets is `undra.android_std` in `MODULE.bazel`, by checksum. The NDK here is the archive of the host OS
(`android_ndk_linux`, `android_ndk_macos`: r27c, 27.2.12479018, the version CI and the size gate use), selected by OS in
`BUILD.bazel`, with `android/ndk.BUILD` as its BUILD file. A build that asks for the core fetches it (664 MB on Linux, 836 MB on
macOS); `bazel test //...` does not. `android/use-installed-ndk.sh` overrides the two repositories with the NDK that `sdkmanager`
or Android Studio installed, which is what CI's "Bazel example (Android core and library)" job does.

`//android:hello`, the bindings as an Android library, needs only the SDK (`ANDROID_HOME`). The same CI job builds it; `rules_android`
then downloads its own tools, one archive without a checksum, which is why it is a `manual` target and CI is where it is built.

## Notes

* The Undra crates are not on crates.io yet, so the core names the `undra` crate by version (`scripts/bump-version.sh` keeps it at the checkout's) and the rules (and `.cargo/config.toml`, for a plain
  `cargo test` here) stand the checkout in for the registry with `[patch.crates-io]`.
* `.bazelrc` sets `DO_NOT_TRACK=1`: `aspect_rules_js` and `aspect_rules_ts` depend on a telemetry module that reports the rulesets a
  build uses to Aspect.
* `MODULE.bazel.lock` is committed. `.bazelversion` is a link to `bazel/.bazelversion`: one pin for the rules and the example.
* The rulesets are at recent versions, the ones most applications run; `undra_rules` (`bazel/MODULE.bazel`) declares the lowest it needs,
  and Bazel resolves to the highest version anyone asks for, so an app keeps its own. CI runs this workspace at both ends, on Bazel
  8.8.1 and 9.2.0 (the lowest and the newest 9.x): as written, and through `scripts/at-minimums.sh`, which rewrites each `bazel_dep` of
  `MODULE.bazel` to the rules' minimum for one command and puts the file back. It adds `--check_direct_dependencies=error`, so a
  minimum something else in the graph raises fails the run, and `--lockfile_mode=off`. Why each minimum is where it is: the
  "Ruleset versions" section of the Bazel guide (`site/docs/bazel.html`).
* The lock files are committed as Bazel 8.8.1 (`.bazelversion`) writes them. Bazel 9.x records other registry files in them, so run 9.x
  with `--lockfile_mode=off`, as CI does, and the tree stays clean.
* `committed/` names the Undra release `undra.toml` pins (`[undra] version`): `from: "<v>"` in its Swift package, `runtime:v<v>` in its
  Gradle module and `^<v>` twice in its package.json. `scripts/bump-version.sh` moves the pin and those four lines together, so
  `//:bindings_check` stays green on a release's version pull request. Nothing reads `committed/` but that test;
  `bazel run //:bindings_check.update` regenerates it.
