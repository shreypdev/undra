# Undra under Bazel

A Bazel workspace around the `undra init` to-do core (ADR-061). `bazel test //...` builds the core, generates its bindings and runs
the Kotlin, TypeScript and (on macOS) Swift code that uses them against the real core.

```
examples/bazel/
  MODULE.bazel      undra_rules (bazel/, by path), rules_rust 1.99.0 pinned by checksum, rules_kotlin, rules_swift, aspect_rules_ts/js
  undra.toml        the project file `undra build` and `undra bindgen` read; [core] namespace = "hello_core"
  Cargo.toml, core/ the core: a store, a typed error and a function
  BUILD.bazel       undra_core, undra_bindings, undra_bindings_test, undra_ts_library
  committed/        the bindings, committed too: the Swift, Kotlin and TypeScript trees `undra_bindings` produces
  kotlin/           the JVM test (JNI against libhello_core) and the ktlint test of the generated Kotlin
  ts/               the Node test (hello_core.wasm through the compiled bindings)
  swift/            the Swift test (macOS): the core in process
  consumer/         what an app team adds on top: its own Kotlin library over the store, a Node test importing the runtime
  android/          the bindings as an Android library (manual: --config=android, the Android SDK)
```

```sh
cd examples/bazel
bazel test //...                      # needs Bazelisk; .bazelversion pins Bazel
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
| `//:bindings_check` | `committed/` is what the core's schema generates, byte for byte (`undra bindgen --check`'s comparison); when it is not, the failure says `bazel run //:bindings_check.update`, which rewrites it |
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
* `committed/` names the Undra release `undra.toml` pins (`[undra] version`): `from: "<v>"` in its Swift package, `runtime:v<v>` in its
  Gradle module and `^<v>` twice in its package.json. `scripts/bump-version.sh` moves the pin and those four lines together, so
  `//:bindings_check` stays green on a release's version pull request. Nothing reads `committed/` but that test;
  `bazel run //:bindings_check.update` regenerates it.
