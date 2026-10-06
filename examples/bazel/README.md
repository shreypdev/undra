# Undra under Bazel

A Bazel workspace around the `undra init` to-do core (ADR-061). `bazel test //...` builds the core, generates its bindings and runs
the Kotlin, TypeScript and (on macOS) Swift code that uses them against the real core.

```
examples/bazel/
  MODULE.bazel      undra_rules (bazel/, by path), rules_rust 1.99.0 pinned by checksum, rules_kotlin, rules_swift, aspect_rules_ts/js, all at their minimums
  undra.toml        the project file `undra build` and `undra bindgen` read; [core] namespace = "hello_core"
  Cargo.toml, core/ the core: a store, a typed error and a function
  BUILD.bazel       undra_core, undra_bindings, undra_ts_library
  kotlin/           the JVM test (JNI against libhello_core) and the ktlint test of the generated Kotlin
  ts/               the Node test (hello_core.wasm through the compiled bindings)
  swift/            the Swift test (macOS): the core in process
  consumer/         what an app team adds on top: its own Kotlin library over the store, a Node test importing the runtime
  android/          the bindings as an Android library (manual: --config=android, the Android SDK)
```

```sh
cd examples/bazel
bazel test //...                      # needs Bazelisk; .bazelversion pins Bazel 8.8.1, the lowest supported
USE_BAZEL_VERSION=9.2.0 bazel test //...   # the newest 9.x CI tests: Bazel 8.8 and 9.x are supported
bazel build //:core_web               # bazel-bin/core_web/hello_core.wasm: 112.7 KB gzipped (2026-10-02), budget 120 KB
bazel build //:bindings               # the Swift, Kotlin and TypeScript trees, as outputs
bazel build //:mobile_ios             # macOS: the XCFramework (manual target)
bazel build //android:hello --config=android   # the bindings as an Android library (ANDROID_HOME)
```

| Target | What it proves |
|---|---|
| `//kotlin:hello_test` | the Kotlin bindings, the Kotlin runtime and `core_host` work together: a call, a store's signals, a typed error, a command, through JNI |
| `//ts:hello_test` | the same through the TypeScript bindings (type-checked by `tsc` against the compiled runtime) and `core_web` under Node |
| `//swift:hello_test` | the same in Swift, with the XCFramework's host twin linked (macOS only; skipped on Linux) |
| `//kotlin:lint_test` | ktlint 1.8 reports nothing on the generated Kotlin, and 90 findings once the exclusions `undra bindgen` writes are taken away |
| `//consumer:summary_test` | a Kotlin library an app writes over the generated store compiles and runs, the core loaded by `-Dundra.native.hello_core.path=$(rootpath //:core_host)` |
| `//consumer:runtime_test` | a Node test that imports `@undra/runtime` beside the bindings resolves both |

## What is hermetic, and what is not

The actions run the `undra` CLI of this checkout with the Rust toolchain Bazel resolved, crates.io packages downloaded by the
checksum `Cargo.lock` records and unpacked into a Cargo directory source, and no network. An action copies exactly the files its
target declares into a scratch directory named by the target (`/tmp/undra-bazel-<key>`), so a core built twice, or in two
checkouts, is the same file. The C linker (and Xcode for iOS, the NDK and `cargo-ndk` for Android) are the machine's.

The Android core (`//:mobile_android`) is declared and does not build: the core's Rust toolchain for an Android platform needs a
C++ toolchain for it (`rules_android_ndk`), which this example does not register, and analysis stops at "Unable to find a CC
toolchain" even on a machine with the NDK and `cargo-ndk`. `//android:hello`, the bindings as an Android library, needs only the
SDK and builds (by hand: CI does not run it, since `rules_android` then downloads its own tools, one archive without a checksum).

## Notes

* The Undra crates are not on crates.io yet, so the core names the `undra` crate by version (`scripts/bump-version.sh` keeps it at the checkout's) and the rules (and `.cargo/config.toml`, for a plain
  `cargo test` here) stand the checkout in for the registry with `[patch.crates-io]`.
* `.bazelrc` sets `DO_NOT_TRACK=1`: `aspect_rules_js` and `aspect_rules_ts` depend on a telemetry module that reports the rulesets a
  build uses to Aspect.
* `MODULE.bazel.lock` is committed. `.bazelversion` is a link to `bazel/.bazelversion`: one pin for the rules and the example.
* The rulesets are at the lowest versions `undra_rules` declares (`bazel/MODULE.bazel`), on purpose: this workspace is what shows they build and
  test on Bazel 8.8.1 and 9.2.0 (the lowest and the newest 9.x CI runs). Bazel resolves to the highest version anyone asks for, so an app on a newer
  ruleset keeps it. Why each minimum is where it is: the "Ruleset versions" section of the Bazel guide (`site/docs/bazel.html`). The lock file
  is written by whichever Bazel last ran, and the two Bazel versions record different registry files in it; commit the one `.bazelversion` wrote.
