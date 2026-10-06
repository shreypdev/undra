# Onboarding: build and test Undra on your machine

Everything here was exercised end to end on a clean Apple-Silicon Mac. Linux notes are
inline where the path differs; Windows is untested. Time to a green core suite: ~10
minutes. Time to the full five-language matrix: ~30 minutes including downloads.

## 1. Install the toolchain

`undra doctor` checks everything in this section and prints the exact command for each gap. Every finding
links to the heading below that explains it: `undra doctor --fix` prints all the commands as one block to
read and paste (it never runs them), and `undra doctor --json` gives the same report to tools (state,
observed value, fix and anchor per finding). Rows marked *contributors* matter only if you work on Undra
itself; building an app never needs them.

On macOS the fixes install with Homebrew, and they are written for the shell you run doctor in: when `brew` is
not on that shell's `PATH` but Homebrew is installed, a fix starts with `eval "$(/opt/homebrew/bin/brew shellenv)"`
(Apple silicon's `/opt/homebrew` is not on the default `PATH`); when Homebrew is not installed, it starts with
Homebrew's own installer. A tool Homebrew already installed out of `PATH`'s reach is reported as such, with only
the `shellenv` line as its fix. The paths below are Apple silicon's; on an Intel Mac Homebrew is `/usr/local`
(`brew --prefix` says which).

### Rust and its targets

#### Rust

`rustup` with the stable channel, 1.85 or newer (the MSRV of the workspace, edition 2024); **1.99.0** is the
compiler CI pins (every `rust-toolchain` line of `.github/workflows`), and the one the compile-fail goldens, the
bench budgets and the web size are recorded on: a machine on another stable gets goldens that differ in rustc's
wording and numbers that differ in the last digits. Doctor checks `rustup`, the active channel, `rustc` and `cargo`.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
rustup update stable              # when doctor says the version is too old
rustup default stable             # when a nightly or beta channel is active
```

#### Rust targets

Doctor checks the targets of the platforms in scope: `wasm32-unknown-unknown` for the web;
`aarch64-apple-ios` (devices) and `aarch64-apple-ios-sim` (the simulator on Apple silicon) for iOS, plus
`x86_64-apple-ios` when `[ios] simulator_archs` in `undra.toml` lists `x86_64`; and the target of every ABI in
`[android] abis` (`aarch64-linux-android` for `arm64-v8a`, `x86_64-linux-android` for `x86_64`).

```bash
rustup target add wasm32-unknown-unknown \
  aarch64-apple-ios aarch64-apple-ios-sim \
  aarch64-linux-android x86_64-linux-android
```

### The web app and the TypeScript runtime

#### Node and npm

Node 24 (the active LTS) and the npm that comes with it. It is the version every CI job pins (`actions/setup-node` with
`node-version: 24` in `ci.yml`, `release.yml`, `site.yml`, `bench.yml`, `rn-devices.yml` and `two-cores.yml`, and in the
workflow `undra init` writes) and the one the measured numbers and the platform readings in the contract notes come from.
The packages' `engines` stay `>=20`: an app does not need what follows. Running the runtime's own suite (`npm test`) does,
because two of its adapters are only right on a recent Node: `nodeSqliteDb` over `node:sqlite` refuses to open on a
Node whose `node:sqlite` cuts text at U+0000 (Node 22.23's does; 24's keeps it), and the `browserWebSocket` cases over
Node's global `WebSocket` read the failures of its undici 7.

```bash
brew install node                 # or: fnm install --lts
```

#### wasm-opt

Optional. `undra build --platform web` works without it; with binaryen's `wasm-opt -Oz` the wasm core is
10-20% smaller.

```bash
brew install binaryen             # Linux: sudo apt-get install -y binaryen
```

### iOS (macOS only)

#### Xcode

Install **Xcode from the App Store** (16 or newer; the Command Line Tools alone have no XCTest, no
`xcodebuild` and no simulators), then point the system at it and accept the license. Doctor reports where
`xcode-select` points, and says so when it points at the command line tools although Xcode is installed.

```bash
open macappstore://apps.apple.com/app/xcode/id497799835
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept
```

If you cannot run `sudo`, `undra` and `scripts/env.sh` fall back to setting `DEVELOPER_DIR` for their own
builds (they do not change what `xcodebuild` does when you run it yourself).

#### iOS simulator runtime

At least one iOS simulator runtime, to run the app (building does not need it).

```bash
xcodebuild -downloadPlatform iOS        # one-time, large
```

### Android

#### Android SDK

The SDK with `platform-tools` (`adb`) and platform 35, and `ANDROID_HOME` pointing at it (Gradle and Android
Studio read it; `undra` finds the usual locations without it).

```bash
brew install --cask android-commandlinetools
export ANDROID_HOME="$(brew --prefix)/share/android-commandlinetools"
SDKM=$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager
yes | $SDKM --licenses
$SDKM "platform-tools" "platforms;android-35" "build-tools;35.0.0"
```

On Linux download the command line tools from <https://developer.android.com/studio#command-line-tools-only>
into `$HOME/Android/Sdk/cmdline-tools/latest` and use the same `sdkmanager` lines.

#### Android NDK

NDK r27 or newer (16 KB page alignment, which Google Play requires), and `ANDROID_NDK_HOME` (cargo-ndk and
Gradle read it; `undra` finds `$ANDROID_HOME/ndk/<version>` without it).

```bash
$SDKM "ndk;27.2.12479018"
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
```

#### cargo-ndk

`undra build --platform android` runs `cargo ndk`. 3.5 or newer, which aligns libraries to 16 KB pages.

```bash
cargo install cargo-ndk
```

#### JDK 17

Gradle and the Android Gradle plugin need JDK 17 or newer (Android Studio bundles one). A missing JDK is a
failure inside a project with an Android app and advice elsewhere; `undra build` itself does not use Java.

```bash
brew install openjdk@17               # Linux: sudo apt-get install -y openjdk-17-jdk
export JAVA_HOME="$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home"
export PATH="$JAVA_HOME/bin:$PATH"    # Homebrew's openjdk@17 is keg-only: not on PATH by itself
```

#### Gradle wrapper

A generated project's `android/` has `./gradlew` (`undra init` copies it from a checkout or creates it with
`gradle wrapper`, so install Gradle before `undra init`). Doctor checks for it inside a project.

```bash
brew install gradle
(cd android && gradle wrapper --gradle-version 8.14.3)
```

#### adb and a device

`adb` (from `platform-tools`) and, to run the app, an attached device or a running emulator. Neither is
needed to build. `adb devices` says `device`, not `unauthorized` or `offline`.

```bash
adb devices
$ANDROID_HOME/emulator/emulator -avd undra &      # an emulator, when no phone is attached
```

#### The undra emulator (contributors)

The device tests and benchmarks of this repository boot the AVD named `undra`.

```bash
$SDKM "emulator" "system-images;android-35;google_apis;arm64-v8a"
$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager create avd -n undra \
      -k "system-images;android-35;google_apis;arm64-v8a" -d pixel_7
```

(`x86_64` instead of `arm64-v8a` on Intel and Linux hosts.)

### Tools for contributors

#### Kotlin compiler (contributors)

Only the Kotlin runtime tests of this repository compile with `kotlinc`; an app never does. They also need the
kotlinx-coroutines jar below.

```bash
brew install kotlin
# The Kotlin local test runner needs one jar (any location; env.sh looks in ../.tools/lib)
mkdir -p ../.tools/lib && curl -sSfLo ../.tools/lib/kotlinx-coroutines-core-jvm-1.6.4.jar \
  https://repo1.maven.org/maven2/org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.6.4/kotlinx-coroutines-core-jvm-1.6.4.jar
```

#### Bazelisk (contributors)

Only the Bazel rules (`bazel/`, ADR-061) and their example workspace (`examples/bazel`) use Bazel; an app never needs it unless it
builds with it. Bazelisk reads the Bazel version from `.bazelversion` and downloads it; Bazel then fetches the pinned toolchains
the rules need (Rust 1.99.0 from `rules_rust`, a JDK, Node, binaryen, the crates), once, into its own cache. Nothing it fetches
touches `rustup`, Homebrew or `~/.cargo`. The example turns off the telemetry of the aspect rules (`DO_NOT_TRACK=1` in its `.bazelrc`).

```bash
brew install bazelisk                   # or: npm i -g @bazel/bazelisk
```

### The machine

#### Disk space

Rust, Xcode (DerivedData, simulators) and Gradle keep several gigabytes of caches each. Doctor warns under
10 GB free on the disk of the project (or your home directory).

```bash
cargo clean
xcrun simctl delete unavailable
```

#### undra on PATH

A generated project builds its core by itself: the Gradle task, the Xcode build phase and the Vite plugin
run `undra build` and look for `undra` on `PATH` (the Xcode phase and Gradle also look in `~/.undra/bin`,
`~/.cargo/bin` and Homebrew's directories, since a GUI-launched build has a short `PATH`). Doctor checks
that one is found and that it is the version that is running.

```bash
curl -fsSL https://shreypdev.github.io/undra/install.sh | sh
export PATH="$HOME/.undra/bin:$PATH"
```

### One command wires it all up per shell

```bash
source scripts/env.sh    # PATH, UNDRA_KOTLIN_* jars, DEVELOPER_DIR fallback
undra doctor              # (after `cargo install --path crates/undra-cli`) prints what's missing, with the fix
undra doctor --fix        # the fixes as one block you can read and paste
```

## 2. Run the suites

Every suite is local; nothing needs the network after install (the Bazel suite fetches its pinned toolchains on its first run).

| Suite | Command | Expect |
|---|---|---|
| Rust workspace | `cargo test --workspace` | 3,100+ pass |
| Lints (CI-equivalent) | `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings` | clean |
| TypeScript runtime | `cd runtimes/ts/@undra/runtime && npm ci && npm test`, then `npm run test:dist` (the same suite against the production build, ADR-057: it builds first; the other packages take `UNDRA_TS_DIST=<runtime>/dist/index.js`) | 2,025 pass (1 skipped); 1,983 on the production build |
| Kotlin runtime | `runtimes/kotlin/undra-runtime/scripts/test-local.sh` | 784 cases, 0 failed (2 skipped without a native library); the Kotlin testkit adds 32 |
| Kotlin runtime under CI's compiler | `kotlinc` 2.0.21 on PATH (CI downloads it; brew's is newer and infers more): `PATH=<kotlin-2.0.21>/bin:$PATH runtimes/kotlin/undra-runtime/scripts/test-local.sh` | same count; a passing run under brew's Kotlin alone is not proof |
| Kotlin over the real JNI core | `cargo build --manifest-path crates/undra-ffi/tests/fixture/Cargo.toml` (the fixture core, namespace `undra_fixture`), then `UNDRA_NATIVE_LIB_DIR=$PWD/crates/undra-ffi/tests/fixture/target/debug runtimes/kotlin/undra-runtime/scripts/test-local.sh`; `bash crates/undra-ffi/tests/jni/run.sh` is the end-to-end leg | the JNI smoke cases run (1 skipped: the one that needs the library absent) |
| Android adapters, JVM unit tests (needs the Android SDK) | `cd runtimes/kotlin/undra-runtime && ./gradlew :android-adapters:test` | 304 results, 2 skipped (152 cases; the debug and release variants both run) |
| Android adapters, instrumented tests (needs a booted emulator or device; set `ANDROID_SERIAL` if several are attached) | `cd runtimes/kotlin/undra-runtime && ./gradlew :android-adapters:connectedAndroidTest` | 149 pass, 1 skipped (the test that switches the device's network off runs only with `-Pandroid.testInstrumentationRunnerArguments.undra.networkToggle=true`) |
| Android WorkManager module (optional, ADR-046): JVM unit tests | `cd runtimes/kotlin/undra-runtime && ./gradlew :android-work:testDebugUnitTest` | 8 pass |
| Android WorkManager module: instrumented tests (needs a booted emulator or device) | `cd runtimes/kotlin/undra-runtime && ./gradlew :android-work:connectedDebugAndroidTest` | 11 pass |
| OkHttp module (optional, ADR-060): JVM unit tests (the Http contract and the realtime contract on OkHttp's adapters; the realtime suite needs Node, else it is skipped, saying why) | `cd runtimes/kotlin/undra-runtime && ./gradlew :okhttp-adapters:test` | 142 results (71 per variant) |
| OkHttp module: instrumented tests (a booted emulator or device; start `node contract-tests/servers/realtime-server.mjs --port 0` first and pass its port, as for `:android-adapters`) | `cd runtimes/kotlin/undra-runtime && ANDROID_SERIAL=<serial> ./gradlew :okhttp-adapters:connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.undra.realtimePort=<port>` | 65 pass |
| Playground Android app on the real adapters (offline queue surviving a killed process) | `bash examples/playground/android/smoke.sh` (needs a booted emulator; it switches airplane mode on and off) | `SMOKE PASSED` |
| Swift runtime | `cd runtimes/swift/UndraRuntime && swift test` | 753 pass |
| Swift runtime over the real C ABI table (the fixture core) | `bash crates/undra-ffi/tests/swift/run.sh` | 6 pass |
| wasm ABI (real module + real TS runtime) | `bash crates/undra-ffi/tests/wasm/run.sh` | 22 + 36 pass |
| C host harness (through the fixture core's table, `undra_fixture_undra_api`; `two_cores.c` opens two copies of it side by side) | `bash crates/undra-ffi/tests/c/run.sh` (add `UNDRA_C_SANITIZE=1` for ASan) | `c smoke: ok`, `c lifetime: ok`, `c two cores: ok` |
| Contract scenarios ×3 platforms | `bash contract-tests/run-all.sh` (the TypeScript column installs the runtime's and its own dependencies when missing, builds the wasm cores it loads and S25 imports wa-sqlite from the runtime's `node_modules`) | 86/86 pass (S01–S30 less S21/S22 on native, per platform; S29 "panic report" and S30 "background run" are ADR-046's) |
| The iOS 15 / 16 floor (ADR-045): the runtime, every golden case in `ObservableObject` mode, `examples/ios15-sample` (xcodebuild at 15.0), the playground's and Fieldbook's bindings at iOS 15, and the Swift contract grid against iOS 15 bindings | `scripts/ios-floor.sh` (steps: `runtime golden sample apps contract simulator`; the last needs an iOS 15 or 16 simulator runtime and says so when there is none) | all build and pass; `simulator` skips here (iOS 26.5 runtime only) |
| Two cores in one process (ADR-044): the playground core as `playground_a` and `playground_b` | `examples/two-cores/{ios,android,jvm,node}/run.sh` (iOS: the booted simulator, `CONFIGURATION=Release` for the fat-LTO cores; Android: the attached emulator; Node: `npm ci` for `fake-indexeddb`, the default `Kv` of the web) | `two-cores <platform>: passed`, after the lines that write one default `Kv` key through each core and read two values back (ADR-044 amendment A) |
| Build systems of a generated project: Gradle, `xcodebuild` and `npm run build` each build the core with no earlier `undra build` (a clean project, then up-to-date, then a change, then the other variant and back; the app links the new core) | `UNDRA_TEST_BUILD_SYSTEMS=1 cargo test -p undra-cli --test build_systems -- --nocapture` (`UNDRA_REQUIRE_TOOLCHAINS=1` makes a missing toolchain a failure; needs `java` on PATH, which `scripts/env.sh` puts there) | 6 pass; skips, saying why, where a toolchain is missing |
| `undra upgrade` end to end (regenerates the bindings against a local clone standing in for GitHub) | `UNDRA_TEST_UPGRADE_E2E=1 cargo test -p undra-cli --test upgrade` | 15 pass |
| Symbol files and `undra symbolicate` (R9, ADR-046): the playground core built in release for each platform, made to panic (in a call, and in a task) in a booted iOS simulator, on the emulator, under node and natively, and its frames resolved to the `lab.rs` line; `--no-symbols` and the sizes of the shipped artefacts | `UNDRA_REQUIRE_TOOLCHAINS=1 cargo test -p undra-cli --test symbols -- --test-threads=1` (needs Xcode with a booted simulator, the Android NDK, `cargo-ndk` and the emulator, node and `wasm-opt`; minutes) | 5 pass; skips, saying why, where a toolchain is missing |
| The debugger path into Rust (ADR-046): LLDB in batch mode stops at `breakpoint set -f lab.rs -l <line>` in a debug core, on the host and in a simulator process | `UNDRA_REQUIRE_TOOLCHAINS=1 cargo test -p undra-cli --test debugging -- --test-threads=1` (run it from a login session: LLDB needs Developer Tools access) | 3 pass |
| Distribution: the release's npm assets (`@undra/runtime`, `@undra/testkit`, `@undra/react-native` packed, installed by URL from a local server with no registry, every peer satisfied) | `bash packaging/pack-npm.sh --out /tmp/assets && bash packaging/test-npm-assets.sh --release-dir /tmp/assets` | all checks pass |
| Distribution: the version script on a copy with a throwaway version | `bash scripts/bump-version.test.sh` | all checks pass |
| Distribution: the launch rehearsal (ADR-063): a copy of the repository tagged as a release, the npm assets served, JitPack's command into a local Maven repository, then `undra init` (no checkout) and its web, iOS and Android apps built | `bash packaging/rehearse-launch.sh` (macOS for iOS; the Android SDK, NDK and Gradle for Android; minutes) | "Every platform built from the rehearsal release alone." |
| Distribution: the curl installer against a served release (checksums, tampering, platforms) | `bash packaging/test-install.sh` | all checks pass |
| Bazel rules and example (ADR-061): the core for the host and the web, the bindings from the host library, the Kotlin (JNI) and TypeScript (Node, wasm) consumers against the real core, ktlint over the generated Kotlin with and without the exclusions `undra bindgen` writes, and on macOS the Swift consumer; `bazel build //:mobile_ios` builds the XCFramework | `cd examples/bazel && bazel test //...` (first run downloads the toolchains: minutes; `--jobs=4` is in its `.bazelrc`); the rules' own unit tests: `cd bazel && bazel test //tests/...` | the example: 4 pass on macOS (Kotlin, ktlint, TypeScript, Swift), 3 on Linux; the rules' tests: 2 pass |
| Benchmark budget gate | `cargo test -p undra-bench --test budgets --release` | pass |
| Native size gate (ADR-052, R9): the hello-world core for Android (both ABIs, the stripped `.so` Gradle packages, 16 KB alignment checked) and iOS (the device slice, linked with `-dead_strip` and stripped) against the `[size."android/..."]` and `[size."ios/..."]` tables of `bench/budgets.toml`; `--record` re-records both in one commit | `scripts/native-size.sh` (`--platform android` or `ios`; needs cargo-ndk and NDK r27 for Android, Xcode for iOS; a minute or two) | each row `ok` against its gate; `bench/results/native-size.jsonl` is the record |
| Benchmarks (numbers for humans) | `cargo bench -p undra-bench` | see `bench/RESULTS.md` |
| Device bench: the blueprint rows through the generated binding and the mirror, on a simulator, emulator, browser or phone | `scripts/bench-device.sh --device ios`, `--device android` (boots the `undra` AVD if nothing is attached; `--target <serial>` for a phone), `--device web`; add `--quick` to check the plumbing in seconds | writes `bench/results/device/<date>-<target>.json` and the device tables of `bench/RESULTS.md`; needs the iOS simulator + Xcode, the Android SDK + NDK, or Playwright's Chromium (`cd examples/playground/web && npx playwright install chromium`) |
| React Native runtime: C++ host under ASan + UBSan (both shims) and the JSI layer against React Native's headers | `runtimes/rn/@undra/react-native/cpp/test/run.sh` (needs `npm ci` in `examples/playground/rn` for the headers, and `undra build --platform host` of the playground and of `examples/two-cores/a`, which it runs when missing; `UNDRA_RN_REQUIRE_JSI=1` makes a missing one a failure) | 15 store checks, then 29 + 29 host checks (the linked shim on macOS only); `UndraJsi.cpp`, `UndraTurboModule.cpp` and (macOS) `UndraPlatformApple.mm` compile |
| React Native runtime: unit tests, typecheck, contract column | in `runtimes/rn/@undra/react-native`: `npm ci`, `npm test`, `npm run typecheck` (build `runtimes/ts/@undra/runtime` first), `npm run test:contract` (needs `bash contract-tests/ts/build-cores.sh`, which builds the wasm cores of S14, S15 and S27 that are not committed, and `contract-tests/derived-vectors.sh` for S19) | 100 pass; clean; 19 pass + S17 and S29 skipped (app-tested) |
| React Native playground app on a device | `scripts/rn-device-checks.sh ios` (the iPhone simulator; CocoaPods) or `scripts/rn-device-checks.sh android --target <serial>` (an emulator such as the `undra-rn` AVD, or a phone) | `UNDRA-RN CHECKS 19/19 passed` (iOS) or `20/20` (Android) |
| Device bench report (CI runs it) | `node --test scripts/bench-device-report.test.mjs` | 14 pass |

Gotcha worth knowing: the bindgen tests that compile and run the generated Kotlin, TypeScript and
Swift (`typecheck_kotlin`, `typecheck_ts`, `run_ts`, `typecheck_swift`) **skip, and pass, when their
compiler is not found** (`kotlinc`, `tsc`, `swift`). Put `runtimes/ts/@undra/runtime/node_modules/.bin`
and a JDK 17 on `PATH`, and run with `UNDRA_REQUIRE_TOOLCHAINS=1` so a missing tool fails instead of skipping.

Gotcha worth knowing: since C ABI version 2 (ADR-044) `undra-ffi` exports nothing of its own;
every native harness (C, Swift, JNI) runs against the fixture core
(`crates/undra-ffi/tests/fixture`, namespace `undra_fixture`), which exports its table and, with
the `jni` feature it always builds with on native targets, `JNI_OnLoad`.

### Before you push a branch: `scripts/ci-local.sh`

CI is a list of jobs on hosted runners; a branch that goes there to find out what is red costs a push per failure, and each
push is slow. `scripts/ci-local.sh` runs every step of the **CI, Bench, Two cores and Site** workflows on this machine, in a
clone of what you committed (CI sees nothing else), reading the steps from `.github/workflows/*.yml` themselves, so the two
cannot drift apart. It checks (and never installs) the pinned Rust and its targets, Node 24 and JDK 17, sets what the workflows
set (`UNDRA_BENCH_SCALE`, `RUSTFLAGS`, ...), and prints a summary; every step it cannot run (runner provisioning: apt, sudo,
SDK installs; the Android emulator, which is on hold) is listed with the reason, and the end of the output names what only a
hosted runner can still tell you (Linux behaviour, macOS 15's own frameworks).

```bash
scripts/ci-local.sh                  # all jobs, in a clone of HEAD, incremental (a cache-restored run); --cold for a new clone
scripts/ci-local.sh --slow           # the slow-runner pass (below)
scripts/ci-local.sh --only ci/ts,ci/contracts     # some jobs (<workflow>/<job id>, or the id alone); --skip, --list, -v
scripts/ci-local.sh --here --only ci/ts           # quick, in this checkout (dirty trees allowed: not a proof)
```

The slow-runner pass is what finds the failures that only a slower machine shows: the timing-sensitive suites (the TypeScript
runtime, the Swift and Kotlin runtimes, the contract grid, the React Native model, the real-time recipe, and the Rust workspace's
tests but those of the three packages that drive a compiler (`undra-bindgen`, `undra-macros`, `undra-cli`: starved, those are a
build and say nothing about timing; `bench/tests` and the dev-reload tests of `undra-cli` run on their own))
run three rounds with 8 CPU burners (`yes` at normal priority, `--burners N`; started and killed by the script for the length of
each step, your own processes only, and they stop within a second of the script whatever happens to it), four test threads
(`RUST_TEST_THREADS=4`: a hosted runner has four vCPUs) and four build jobs. Every `cargo test` runs with `--no-fail-fast`, so one
pass lists every failing test binary. Nothing runs under `taskpolicy -b`: that QoS next to burners leaves a test no CPU at all.
It reuses what a normal pass built and installed in the same clone, so run that first:
what is slowed is the tests, not the compiler. A test that fails only there depends on the machine's speed: fix it at its cause (pace by the consumer, scale a
budget by `UNDRA_BENCH_SCALE`, a deadline that covers a slow machine and reports the elapsed time, wait on the condition and
not on a fixed sleep), never by loosening an assertion about behaviour. It needs `ruby` (macOS ships it); logs of every step
are kept (`--logs DIR`).

## 3. Run the reference app

```bash
cargo install --path crates/undra-cli
undra build -C examples/playground --platform web,ios,android   # artifacts under examples/playground/build/
cd examples/playground/web && npm install && npm run dev       # Chrome
bash examples/playground/ios/smoke.sh                          # boots a simulator, installs, screenshots
# Android: bash examples/playground/android/smoke.sh, or see examples/playground/android/README.md (gradlew assembleDebug + the `undra` AVD)
```

The playground is wired by hand, so it still takes the explicit `undra build` above. A project made by `undra init` does not:
its Gradle task, Xcode build phase and Vite plugin run `undra build` themselves before each app builds
([`docs/DEV_LOOP.md`](DEV_LOOP.md#production-builds-are-not-a-separate-step)), `undra init` writes its CI workflow
(`.github/workflows/undra.yml`), and `undra upgrade` moves it to a newer Undra in one command.

The live loop (edit Rust, every app picks up the new core, a dropped connection heals itself) is `undra dev`:
[`docs/DEV_LOOP.md`](DEV_LOOP.md) has the URL of each platform (the Android emulator is `ws://10.0.2.2:<port>`),
`undra dev --android`, how reconnecting works and a troubleshooting table.

### Release symbols and crash reports

A release build writes **symbol files** next to what it ships, so a panic report from production resolves to
`file:line` (ADR-046). `undra build --release` keeps the line tables of the core (`debug = "line-tables-only"` in the
generated shim's release profile: the same optimisation and LTO, so the same code), strips the copies it ships and keeps
the unstripped ones below `build/symbols/`. `--no-symbols` skips them (and builds the shim without debug info, as
before ADR-046); debug builds write none, they are unstripped anyway. A build of one platform replaces its own entries,
so building each platform in turn (or in separate CI jobs, uploading to one directory) accumulates one manifest.

| Platform | Ships | Symbols |
|---|---|---|
| iOS | `build/ios/<Ns>Core.xcframework/<slice>/lib<ns>.a`, nothing stripped | the **app's own dSYM** (Xcode Release, `dwarf-with-dsym`): the prelinked object's debug map names Cargo's objects, which `dsymutil` reads, so the dSYM has the Rust frames; Crashlytics and Sentry take it as they do for Swift. The objects stay in Cargo's target directory: build the app right after `undra build`, as the Xcode build phase does |
| Android | `build/android/jniLibs/<abi>/lib<ns>.so`, stripped (`llvm-strip --strip-debug --strip-unneeded`) | `build/symbols/android/<abi>/lib<ns>.so` (unstripped, same build id) and `build/symbols/android/native-debug-symbols.zip` (`<abi>/lib<ns>.so`: upload it in the Play Console; Crashlytics takes the directory as `unstrippedNativeLibsDir`, `sentry-cli debug-files upload build/symbols/android` Sentry) |
| Web | `build/web/<ns>.wasm`: no names, no DWARF (the same `-Oz` optimisation as ever; `--no-symbols` makes the old bytes) | `build/symbols/web/<ns>.debug.wasm` (the same code with its names: one `wasm-opt -Oz -g` run over the module without DWARF makes it and the function map, and the shipped module is it minus the name section, so every `wasm-function[i]:0x…` of a production stack trace names the same function and byte offset in both; `wasm-opt` breaks ties between functions by name, so this module differs from the nameless one by about 0.1%: the hello world +90 bytes gzipped, the playground -421), `build/symbols/web/<ns>.wasm.functions.txt` (`index:name`, `wasm-opt --print-function-map`) and `build/symbols/web/<ns>.dwarf.wasm` (a second run that keeps the DWARF line tables: for Chrome's DevTools and for the definition lines `undra symbolicate` prints; see below) |
| Host (macOS, Linux) | `build/host/lib<ns>.{dylib,so}`, stripped | next to it: `lib<ns>.dylib.dSYM` (`dsymutil`) / `lib<ns>.so.debug` (`objcopy --only-keep-debug`) |

`build/symbols/manifest.json` lists every shipped image:

```json
{
  "version": 1,
  "undra": "1.0.0",
  "artifacts": [
    {
      "platform": "android",            // ios | android | web | host
      "namespace": "playground_core",   // the core's namespace (ADR-044)
      "coreVersion": "0.1.0",           // the core crate's version (what export_core! reports)
      "schemaHash": "0x53241303b2d08c5e",
      "arch": "arm64-v8a",              // an ABI, an iOS slice (ios-arm64, ios-arm64-simulator), wasm32, or the host's
      "format": "elf",                  // elf | macho | wasm
      "imageId": "16ed68dc3a85066f424f34d3d8020a5f030c614f",
      "sha256": "…",                    // of the shipped file
      "shipped": "android/jniLibs/arm64-v8a/libplayground_core.so",   // relative to build/
      "shippedBytes": 2061680,
      "symbols": "android/arm64-v8a/libplayground_core.so"           // relative to build/symbols; null for iOS
      // web entries add "functionMap": "web/playground_core.wasm.functions.txt" and "dwarf": "web/playground_core.dwarf.wasm"
    }
  ]
}
```

`imageId` is what a `PanicReport` of that image carries as `image_id` (lowercase hex): the ELF GNU build id (the linker is
asked for `--build-id=sha1`), the Mach-O `LC_UUID` of a host library, the SHA-256 of the wasm module. It is `null` for an
iOS slice: a static library has no UUID, the image that holds the core is the app, and the app's UUID is the one of its
dSYM. `schemaHash` is read from a host build of the core the way `undra bindgen` reads it (cached, so it costs a build
once), or, for the web, asked of the wasm module itself under `node`; a debug build and a failure write `null` (a failure
warns).

**`undra symbolicate`** resolves a report against them (`undra symbolicate --help` has the whole story):

```bash
undra symbolicate report.json                       # the JSON of the app's onPanic report; finds the artefact by imageId, then namespace
undra symbolicate report.json --symbols ./symbols   # a CI build's symbols artifact, unpacked
undra symbolicate report.json --dsym App.app.dSYM   # iOS: the app's dSYM (Spotlight finds it by UUID when not given)
undra symbolicate --platform web 'wasm-function[1489]:0x5a692'
undra symbolicate --platform android --image-id 16ed68dc3a85066f424f34d3d8020a5f030c614f 0x998ef 0x9983f
```

```text
0x998ef playground_core::lab::explode (/undra/app/core/src/lab.rs:222)
0x9983f playground_core::lab::__undra_dispatch_fn_explode (/undra/app/core/src/lab.rs:221)
```

A report's `address` is the instruction address minus the load address of the image that holds the core, pointing into
the *call* instruction (nothing is subtracted before the lookup). `llvm-symbolizer` (the NDK's) resolves an Android
address as it is. On iOS `atos` is given `__TEXT vmaddr + address`, the vmaddr read from the app's dSYM (0x100000000 for
an arm64 executable). A wasm address is the module offset of the `wasm-function[i]:0x…` line of a V8 stack trace; `i`
(or, for a bare offset, the function whose body contains it in the debug module) is named by the function map exactly,
and the line is the line where that function is *defined*, taken from the DWARF module by the function's name. A wasm
frame carries no line inside its function: `wasm-opt` skips every pass that cannot update DWARF when it keeps DWARF, so a
module that keeps it is 5 to 7% larger (and has other code), which the shipped module must not be; the panic's own
`file:line:col` and the operation are in the report (the core logs them before it traps). Paths of a release
build name the project `/undra/app`, the Undra checkout `/undra/src`, Cargo's registry and git sources `/undra/deps` and the
builder's home directory `~` (ADR-052: no machine, and the same strings in any checkout); the standard library's are `/rustc/<commit>/…`. Rust symbols are
demangled with Xcode's or the NDK's `llvm-cxxfilt` when there is one.

`crates/undra-cli/tests/symbols.rs` is the regression test (R9): it builds the playground core in release, makes it panic
(in a call, and in a task of its own) in a booted iOS simulator, on the emulator, under node and natively, and resolves
the frames with these files: to the `lab.rs` line of the `panic!` on iOS, Android and the host (the line the core itself
reports as the panic location), to the function that panicked and where it is defined on the web. It also builds the core with `--no-symbols` and checks
that no shipped artefact grew. It needs the toolchains of every platform and skips, saying which one is missing, without
`UNDRA_REQUIRE_TOOLCHAINS=1`:

```bash
UNDRA_REQUIRE_TOOLCHAINS=1 cargo test -p undra-cli --test symbols --test debugging -- --test-threads=1
```

### The size of the shipped core

`undra build --platform ios,android --release` builds the `release-mobile` profile of the generated shim: `release` with
`opt-level = "s"` and **`panic = "unwind"` kept** (R6, ADR-046: a native panic is contained by `catch_unwind` and reported;
`abort` would make it a crash). The crates the call path runs through (`undra-wire`, `undra-signals`, `undra-runtime`, `undra-ffi`) stay
at `opt-level = 3`: `s` everywhere made the core's own operations 17% slower. That is 7% off the Android library of a hello world
(965,968 to 898,176 bytes, arm64-v8a), 12% off the playground's, and 6% off what the iOS slice adds to an app, with the device bench rows
inside their noise (ADR-052, amendment "native size gates"). The host build stays on `release`; `opt-level = "z"` takes 12 to 17 points
more off an Android library and slows a call about 1.5x, so it is not the default. An app with a hard byte budget chooses it per platform, `opt_level = "z"`
in `[android]` or `[ios]` of its `undra.toml` (the `release-mobile-z` profile: 766,528 bytes for the hello world, 2,018,792 for the playground,
arm64-v8a), and `opt_level = "3"` is the speed profile (`release`) for an app whose own hot code should not be optimised for size. The gates
measure the default. The sizes are gated, not just printed:

```bash
scripts/native-size.sh                    # the hello-world core per Android ABI and the iOS device slice, against bench/budgets.toml
scripts/native-size.sh --platform android # (--platform ios on a Mac); --record re-records bench/results/native-size.jsonl and the tables
```

A row fails when the library is over the design's budget (1.2 MB per ABI, 900 KB added to an iOS app) or more than 5% over its
record; the script also fails a library that is not 16 KB aligned or that names the builder's directories. The iOS number is the
slice linked the way an app keeps it (`-dead_strip`, `strip -x`), because the `.a` is no measure (it grows at `z` while the code shrinks).
To find what grew, use the NDK's own tools on the unstripped twin that `--release` keeps (no `cargo-bloat`, no dependency):

```bash
NDK_BIN=$ANDROID_HOME/ndk/27.2.12479018/toolchains/llvm/prebuilt/<host>/bin
$NDK_BIN/llvm-size -A build/android/jniLibs/arm64-v8a/lib<ns>.so          # sections
$NDK_BIN/llvm-nm --print-size --size-sort -C --defined-only build/symbols/android/arm64-v8a/lib<ns>.so | tail -40
size -m build/ios/<Ns>Core.xcframework/ios-arm64/lib<ns>.a                # the slice's sections
```

### Debugging into Rust

A debug build (`undra build`, or the Xcode Debug configuration, the Gradle `debug` variant) keeps full DWARF: nothing is
stripped, and the iOS prelink uses `-all_load`. `undra doctor` checks the pieces below (`rust.lldb-formatters`,
`android.lldb`, and for the web it prints the extension's link). `crates/undra-cli/tests/debugging.rs` runs LLDB in batch
mode against the playground core with the `.lldbinit` sourced, sets `breakpoint set -f lab.rs -l <the line of explode>`,
and asserts the stop and then a `bt` of the full depth (down to the host's `main`, LLDB alive), on the host dylib and in
an iOS simulator process linked with the prelinked core; and it debugs a small Swift program (an object of a reference
cycle as an argument) through the same `.lldbinit`.

* **Xcode.** Step into Rust from a Swift call, or set a breakpoint by file and line in the console
  (`breakpoint set -f todos.rs -l 42`; in the editor once the `.rs` file is opened). `undra init` writes a `.lldbinit`
  that loads the toolchain's Rust formatters (`$(rustc --print sysroot)/lib/rustlib/etc/lldb_lookup.py`, and
  `lldb_commands` where a toolchain has one), so a Rust `String` reads as its text instead of a struct. LLDB reads a
  project's `.lldbinit` only from its working directory and only when allowed: `echo 'settings set
  target.load-cwd-lldbinit true' >> ~/.lldbinit`. Three things Xcode 26.6's LLDB did with the Rust 1.99 formatters on
  a full `bt` through the core (2026-10-05, `.10x/decisions/sde/lldb-formatters-bt.md`), and what holds now:
  * **A tuple variant named after the type it carries kills LLDB.** When the variant and its payload type share a
    name and a byte size in one compilation unit (`Pool(native::Pool)` in the core's `Blocking`), LLDB takes the one
    for the other: the variant contains itself, the record layout recurses until the stack is gone, and LLDB exits
    with SIGILL (code 132) as soon as a value holding it is shown, by `frame variable` or by a `bt` once the formatters
    inspect the pointee of every `&T` argument. The core's variant is `Blocking::Threads` now and the test above scans
    the core crates for the pattern. In app code, rename the variant or the type; a type of another crate
    (`String(String)`) is parsed first and is not confused.
  * **The struct summary followed every pointer field.** The 1.99 formatters print a struct's fields and, through a
    pointer field, the pointee's, so a `&Runtime` argument walked the object graph round its `Weak<Runtime>` until
    Python aborted LLDB (code 134). The `.lldbinit` re-registers the struct and enum summaries with pointers skipped: a
    pointer argument prints as its address (`self=0x...`), as LLDB prints it without formatters, and a value keeps its
    summary.
  * **The formatters printed Swift values too.** They match types by shape, not by language: in a Swift frame an
    array read `{[0]:{}, [1]:{}, ...}`, and a Swift object (a class, so no pointer to skip) was walked round its
    reference cycles until Python aborted LLDB (code 134), at the first stop in such a frame or on a `bt` from the
    core down to the app. The `.lldbinit` makes their type recognizers decline Swift and Objective-C types, which
    keep LLDB's own formatting.

  A project made by an earlier `undra init` takes the file from a fresh one (`undra upgrade` does not rewrite it).
* **Android Studio.** The "Dual (Java + Native)" debugger with the debug APK. The generated Gradle module sets
  `keepDebugSymbols` for the debug variant's `lib<ns>.so` (an app made by `undra adopt` gets the line from the hint a
  debug `undra build` prints); the library comes unstripped from a debug build. For a release reproduction add
  `build/symbols/android` as the symbol directory. `undra doctor` checks that the NDK has its LLDB.
* **Chrome DevTools.** Install the C/C++ DevTools Support (DWARF) extension
  (https://chromewebstore.google.com/detail/cc++-devtools-support-dwa/pdcpmagijalfljmkmjngeonclgbbannb): it reads
  `build/symbols/web/<ns>.dwarf.wasm`, which `vite dev` serves (the generated `vite.config.ts` hands the page the
  path through `__UNDRA_DEBUG_WASM__`; `vite build` bundles the stripped module and never names it), and opens `.rs`
  files for breakpoints. `doctor` cannot check a browser extension.
* **Host tests.** `rust-lldb target/debug/deps/<test binary>` or CodeLLDB on `cargo test`.

## 4. Read before you write code

1. [`CLAUDE.md`](../CLAUDE.md): the constitution (R1–R12) and engineering standards.
   These are enforced in review, not aspirational.
2. [`docs/SPEC.md`](SPEC.md): the binding spec. Code that disagrees with it is wrong
   until an ADR changes it.
3. [`.10x/adrs/`](../.10x/adrs/): why the big decisions were made (ADR-014 through 028).
4. [`.10x/status.md`](../.10x/status.md) and [`.10x/handoff.md`](../.10x/handoff.md):
   where the work stands and what v1.x holds.
5. [`docs/AGENT_WORKFLOW.md`](AGENT_WORKFLOW.md): how changes are made here, one
   worktree per piece, adversarial review before merge, cleanup after. It applies to
   humans and AI agents alike.

## 5. The bar for a change

Every piece lands whole (R4): unit tests next to the code, integration tests in
`tests/`, a contract scenario if the wire is touched, a benchmark if the boundary is
touched, docs on every `pub` item, `clippy -D warnings` clean, and the *full* matrix
green, not just the crate you touched. If your change alters the wire, the runtime
model, the threading model or a generated public shape, write the ADR first (R11).

**Reviewing generated code.** Do not read the generated Swift, Kotlin and TypeScript of a core
change: GitHub collapses those trees (each has a `.gitattributes` marking it
`linguist-generated`; click a file to see its diff) and `undra bindgen --check` proves they are
exactly what the schema generates. Review the schema instead:
`undra schema diff --against origin/main` prints what changed in the public API, one line per
change, each marked `breaking` or `additive` (the rules are `docs/SPEC.md` 2.6; add
`--exit-code` to make a breaking line fail a CI step). The pull request template asks for that
output when a core's API changes. `undra schema export -o schema.json` writes the file the
command reads; commit it next to `generated/`, and run `undra schema export --check` in CI so it
stays the core's schema (`--against` also warns when the file disagrees with the bindings). ADR-062 has the reasoning, and
`node scripts/generated-weight.mjs` measures what the lines of a generated tree are.
