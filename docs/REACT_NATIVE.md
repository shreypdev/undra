# Undra under React Native

`@undra/react-native` puts your Undra core under a React Native app: the same native library the
SwiftUI and Compose apps link (`lib<namespace>.a` in `<Bundle>.xcframework` on iOS,
`lib<namespace>.so` on Android; `libplayground_core` for the playground), reached from JavaScript
through JSI, with the TypeScript runtime's `UndraCore`, mirror and React hooks on top and the bindings
`undra bindgen` generates for TypeScript, unchanged. An app may hold several cores, each found by its
namespace (`[core] namespace` in `undra.toml`, ADR-044). The design is ADR-038
(`.10x/adrs/ADR-038-react-native-host.md`) with ADR-044's per-core table; the reference app is
`examples/playground/rn`.

```
 React components ── useSignal / useUndra (@undra/runtime/react)
        │
 generated bindings (@your/core) ── UndraCore, Mirror (@undra/runtime)
        │
 NativeTransport (@undra/react-native) ── JSI host functions (globalThis.__undraNative[namespace])
        │                                       │ one C++ module, iOS and Android, one object per core
 ───────┴──────── C ABI v2 (undra.h: the core's UndraApi table) ─┴──────────
                         your Rust core (undra build --platform rn)
```

Calls run the core on the JS thread (a synchronous method answers before the JSI call returns);
async work runs on the core's own thread; everything the core says goes through one inbox that is
handed to JavaScript in batches, in commit order, and the mirror applies what the core produced on
its own once per display frame (ADR-031), exactly as on the web.

## Requirements

* React Native with the New Architecture, bridgeless, on Hermes. **0.87 is the version it is built and
  proven on** (the playground); the package's peer range starts at 0.82, which has the APIs it uses
  (pure C++ TurboModules on both platforms), but nothing below 0.87 has been run.
* iOS: CocoaPods (`brew install cocoapods`), and the deployment target of your core (`[ios]
  deployment_target` in `undra.toml`, 17.0 by default) as the app's `platform :ios`.
* Android: the NDK the app builds with (React Native 0.87's template asks for r27), and the ABIs of
  `[android] abis` in `reactNativeArchitectures`.

## Install

1. **Build the core** for both phones and write its pod:

   ```sh
   undra build --platform rn --release      # without --release for a debug core (much slower)
   ```

   This is the iOS and Android builds plus the core's pod. For a core whose namespace is `acme_pay`
   (the bundle name drops a trailing `core`, so `playground_core` is `PlaygroundCore`):

   | Under `build/` | Used by |
   |---|---|
   | `ios/AcmePayCore.xcframework` (one prelinked `libacme_pay.a` per slice), `ios/AcmePayCore.podspec`, `ios/AcmePayCoreTable.m` | the app's Podfile |
   | `android/jniLibs/<abi>/libacme_pay.so` | the app's Gradle `jniLibs` |

   Each slice of the XCFramework is one object whose only global symbol is the core's entry,
   `acme_pay_undra_api`, so it links without `-force_load` and next to other cores (ADR-044). The pod
   also compiles `AcmePayCoreTable.m`, the class `UndraCoreTable_acme_pay` through which the module
   finds the core by its namespace (the module is built once for every core, so it names none).

2. **Add the packages** (`@undra/runtime` and `react-native` are its peers) and your generated bindings. Both are
   assets of the Undra release on GitHub (ADR-063): install them by URL, at the version of your `undra`
   (`undra --version`), and `undra upgrade` moves both URLs together:

   ```sh
   v=1.0.0
   npm install https://github.com/shreypdev/undra/releases/download/v$v/undra-react-native-$v.tgz \
               https://github.com/shreypdev/undra/releases/download/v$v/undra-runtime-$v.tgz
   ```

   The bindings (`generated/ts`) are TypeScript sources, used as they are: do not `npm install` that directory (it is
   not built, and the `@undra/runtime` it imports must be the app's one copy). Let Metro resolve them, and the runtime
   from the app: `watchFolders` with `generated/ts/src`, `resolver.nodeModulesPaths` with the app's `node_modules`, an
   alias of your bindings' package name to `generated/ts/src/index.ts`, and the rule that serves `./x.ts` for an import
   of `./x.js` (`examples/playground/rn/metro.config.js` has all four; its other aliases point at a checkout and are not
   needed with the packages above).

3. **Babel**: Hermes cannot compile `import.meta`, which `@undra/runtime`'s `wasm-worker` mode contains
   (that mode never runs under React Native, but Metro bundles it). Add the package's plugin:

   ```js
   // babel.config.js
   module.exports = {
     presets: ['module:@react-native/babel-preset'],
     plugins: ['@undra/react-native/babel-plugin'],
   };
   ```

   If you consume `@undra/runtime` or your bindings as TypeScript sources (as the playground does),
   also let Babel strip `declare` fields: an override with `['@babel/plugin-transform-typescript',
   { allowDeclareFields: true, allowNamespaces: true }]` for `.ts` files
   (`examples/playground/rn/babel.config.js`).

4. **iOS**: point the Podfile at the core's pod, then `pod install`:

   ```ruby
   platform :ios, '17.0'                         # your core's deployment target
   target 'App' do
     config = use_native_modules!                 # links UndraReactNative (autolinking)
     pod 'AcmePayCore', :path => '../build/ios'   # written by undra build --platform rn
     use_react_native!(...)
   end
   ```

   The path is relative to the Podfile (`ios/`): `../build/ios` when the app sits next to `build/`,
   `../../build/ios` for the playground's `rn/ios/Podfile`. One line per core; the pod adds `-ObjC` to
   the app, which keeps the table class.

5. **Android**: package the core's libraries:

   ```groovy
   // android/app/build.gradle, inside android { }
   sourceSets {
       main.jniLibs.srcDirs += ["../../build/android/jniLibs"]
   }
   ```

   Nothing else: autolinking builds the C++ module into the app's `libappmodules.so` (it opens
   `lib<namespace>.so` from the APK, `dlopen` then `dlsym` of `<namespace>_undra_api`, when JavaScript
   first loads that core) and links the package's small Android library (Java, no dependency), which
   the default ports use for the Keystore and the network state. Its manifest merges
   `ACCESS_NETWORK_STATE` and a provider that hands it the application context; an app that removes the
   provider (`tools:node="remove"`) calls `UndraPlatform.install(context)` in `Application.onCreate`.

6. **Import first**: `@undra/react-native` installs `TextDecoder` (Hermes has none) before anything
   loads `@undra/runtime`, so import it at the top of your entry file:

   ```js
   // index.js
   import '@undra/react-native';
   import { AppRegistry } from 'react-native';
   import App from './App';
   ```

## Use

```tsx
import { loadNative } from '@undra/react-native';
import { useSignal } from '@undra/runtime/react';
import { Todos, UndraAcmePay } from '@acme/pay-core';   // your generated bindings and their entry

const core = await loadNative(UndraAcmePay);           // every port has a default ("Ports")
const todos = await Todos.create();                    // on UndraAcmePay.core, which is this core

function TodoCount() {
  const remaining = useSignal(todos.remaining);
  return <Text>{remaining} left</Text>;
}
```

`loadNative(entry, options)` takes the generated entry of your bindings (`Undra<Namespace>`, ADR-044):
it installs the module of the core `entry.namespace` (`globalThis.__undraNative[namespace]`), checks the
core's schema hash against `entry.schemaHash` before the core starts (a core from another schema is
refused with `UndraSchemaMismatchError`), and attaches through `entry.attach`, so the core becomes
`UndraAcmePay.core`, the default of every generated class and function of those bindings. The first
core loaded is also `UndraCore.shared` (for `useUndra`). The entry may be just
`{ namespace, schemaHash }` (`UndraIds.namespace`, `UndraIds.schemaHash`); then the core is not any
entry's `core`, and generated calls need it passed explicitly.

The options are those of `UndraCore.attach` (`adapters`, `ports`, `onError`, `onClose`, `mirror`,
`shared`) plus `devtools`, `logLevel` and `platform`. Called again for a namespace whose core is open
(or loading), `loadNative` resolves with that core. `core.close()` shuts the core down; a JavaScript
reload in development shuts it down and the next `loadNative` starts a fresh one.

**Several cores.** Each core is its own pod (iOS) and library (Android) and its own `loadNative`:

```ts
await loadNative(UndraAcmePay, { adapters: { kv: payKv } });
await loadNative(UndraVendorSdk, { shared: false });   // another namespace: its own module, inbox, threads
```

They share nothing (no handles, no types, no threads, ADR-044 decision 6); values pass between them
through your code. `installNative(namespace)` returns a core's JSI object (its `hostCounters()`,
`statsJson()`) and throws `UndraTransportError("unsupported")`, with the module's reason, when the app
has no core of that namespace.

### Ports

Every standard port has a default, so an app writes no adapter (ADR-038, amendment B). Where React Native has
the API, the default is JavaScript; where it has none (no file system, key-value store, keychain or network state
in its core, and the package takes no community module), the default is the module's own native code, which
answers the core directly, off the JS thread:

| Port | Default | iOS | Android |
|---|---|---|---|
| `Kv` | native (C++, one implementation for both) | files in `<Application Support>/<bundle id>/Undra/<namespace>/kv`, the Swift runtime's layout | files in `<filesDir>/undra/<namespace>/kv`, `android-adapters`' layout |
| `SecureStore` | native | the Keychain (service `<namespace>.dev.undra.securestore`, `AfterFirstUnlockThisDeviceOnly`), the Swift runtime's items | AES-256-GCM under the Android Keystore key `<namespace>.dev.undra.securestore`, sealed files in `<noBackupFilesDir>/undra/<namespace>/secure`, `android-adapters`' layout |
| `Fs` | native (C++) | `<Application Support>/<bundle id>/Undra/<namespace>/fs` | `<filesDir>/undra/<namespace>/fs` |
| `Connectivity` | native | `NWPathMonitor` (Network.framework) | `ConnectivityManager` default-network callback |
| `Http` | `reactNativeHttp()`: React Native's `fetch` | `NSURLSession`, through React Native's networking | OkHttp, through React Native's networking |
| `Lifecycle` | `AppState` | `active`, `inactive`, `background` | `active`, `background` (React Native reports no `inactive` on Android) |
| `Clock`, `Rng`, `Log` | native, in the module (`Log` records also reach your `log` adapter, default the console) | | |
| `Timer` | the core's own timer thread | | |
| `Db` (opt-in, ADR-048) | native (C++ binding: migrations, one worker per database, transactions, busy timeout) | the system SQLite, `<Application Support>/<bundle id>/Undra/<namespace>/db/<name>.sqlite`, the Swift runtime's file | `android.database.sqlite` through JNI, `getDatabasePath("undra-<namespace>-<name>.sqlite")`, `android-adapters`' file |
| `WebSocket` (opt-in, ADR-047) | `reactNativeWebSocket()`: React Native's `WebSocket`, headers as its third argument | a dropped connection ends `Network`, or `Closed(1001, "Stream end encountered")` when iOS reports it as the end of its stream (React Native forwards no `wasClean`) | a dropped connection ends `Network` |
| `Sse` (opt-in, ADR-047) | `reactNativeSse()`: `fetch` body streams where present, else `XMLHttpRequest` progress events | | |

`<namespace>` is the core's (the table's `name_space`, the generated entry's `namespace`): **every default store is per core
namespace** (ADR-044 amendment A), so two cores of one app never read or overwrite each other's keys, secrets, files or
databases, and there is no shared location to migrate from. Because the directories, file layouts, Keychain items and
Keystore key are the ones the Swift and Kotlin runtimes use on the same platform for the same namespace, a value the
SwiftUI or Compose shell of an app wrote is read by its React Native shell and the reverse. A value in `adapters` or
`ports` that replaces a default keeps whatever location you gave it. What each default does:

* `Kv` and `SecureStore`: one file per key, written to a temporary file, flushed and renamed, so a killed app keeps
  the old value or the new one. A failing disk, Keychain or Keystore answers the core a typed `StorageError`
  (ADR-049), never "missing" for a value that exists.
* `WebSocket` and `Sse`: React Native's timers fire on frame boundaries, so the 2 ms quiet period of a pull
  (ADR-047 §3) is one frame here: a lone message reaches the core about 16.7 ms after it arrived (the platform's own
  `WebSocket` delivers it in under 1 ms). React Native reports every failure before a socket opens as an error event
  without a status, so a refused upgrade, a DNS failure and a refused port are all `Refused { status: null }` (SSE's
  DNS failure is `Network`, as elsewhere).
* `Fs`: paths are relative to the root; any `..` and any symbolic link on a path is `FsError.Denied`, so the core
  cannot leave the root; `write` creates directories and is atomic; `delete` removes a directory with everything in
  it, never the root.
* `Http`: any status is a response. No connection (airplane mode, a refused connection, a DNS failure) is
  `HttpError.Network`, never "unavailable"; the request's timeout covers the body and is `HttpError.Timeout`; a URL
  that is not `http(s)://host...` is `HttpError.InvalidUrl`. Bodies cross React Native's networking module as base64
  (its limitation). Android blocks `http://` unless the app's network security config allows the host.
* `Connectivity`: the current state as soon as the core is up, then every change, identical consecutive reports
  once. Online means the default network provides internet, as `NWPathMonitor` and `android-adapters` say.

`nativePlatformDefaults(namespace)` tells which ports the module answers natively on this device and where they keep their
data (the device's answer, the same for every core of the app).

**Replacing a default.** Pass your own adapter to `loadNative`: a value in `adapters` replaces that port's default
(a JavaScript adapter, as on the web), `null` removes it (the core sees the port as unavailable), and an
implementation in `ports` replaces it too. The playground keeps every default and wraps one, to show how:

```ts
await loadNative(UndraPlaygroundCore, {
  adapters: { http: taggedHttp(reactNativeHttp()) },   // a header on every request, then the default
});
```

Replace a native default there, not with `core.registerPort` after the load: the module registers its native ports
before the core's `init` (so start-up work such as the query cache's hydration never races them), and a JavaScript
registration made later does not reach a port the module answers itself.

Your own async ports work as on the web (`core.registerPort` or `ports:`). A port the core calls
**synchronously** (`#[undra::port(sync)]`) that you implement in JavaScript is answered only when the
core calls it from a call you made on the JS thread; from the core's own thread it is unavailable (and
logged once), because the core must never wait for the JS thread. Implement such a port natively, or
make its methods `async`.

**Why the Android half is Java, not Kotlin.** It is a few hundred lines that only call platform APIs, and Java
adds no Kotlin plugin whose version would have to match the app's and no standard library to the APK; it has no
dependency at all. (Reusing `android-adapters` itself was not possible: it is not on a Maven repository an npm
package can reach, and it is built on the Kotlin runtime and coroutines, a second Undra host in an app whose core
the C++ module drives.)

## Development

* **Fast refresh and reloads** keep working: a reload tears the JS runtime down, the module shuts the
  core down with it, and your app's `loadNative` starts a fresh core (proven on the iOS simulator and
  the Android emulator, reloading in the middle of work too).
* **`undra dev`**: in development the app can reach a core served by `undra dev` over the WebSocket
  remote transport instead of the linked one, `UndraCore.load({ mode: 'remote', url })` (Hermes has
  `WebSocket`; from the Android emulator the host is `10.0.2.2`, or `adb reverse tcp:7443 tcp:7443`).
  Reconnecting and Android parity of the dev loop are tracks B1 and B2.
* Metro can serve `@undra/runtime`, `@undra/react-native` and your bindings from their TypeScript
  sources (the playground's `metro.config.js` maps them and serves `./x.ts` for `./x.js` imports).

## Limits

* **High-rate updates fit a frame, with room, on the simulator.** At 100,000 keyed-patch updates a
  second (1,667 per 60 Hz frame) the mirror needs about 5 ms of every 16.7 ms frame on the iPhone 17
  Pro simulator, Release build, Hermes (parsing the change-sets as they arrive, 2.4 to 2.6 ms, plus
  the frame's drain, 2.6 to 2.8 ms), where the runtime before ADR-056 needed 22 ms on the same host
  and load (13.1 and 12.9 ms of parsing, 8.9 and 8.6 ms of drain, measured alternately in two runs
  each at a host load of about 30): it did not fit; now about a third of the frame goes to it
  and the rest is React's. The cost was `@undra/runtime`'s JavaScript under Hermes, not the boundary:
  the `#private` fields Babel lowers to helper calls on every access, a `DataView` made for every
  reader and writer, an allocation per call and per change-set. What this is not: a phone (the
  simulator runs on an M5 Pro, a phone is slower, and the figure scales with it),
  or the Android emulator, which was not measured again (it needed more than the simulator before).
  Keep core-driven update rates to what your frame has left after your own rendering; 100,000 a second
  is no longer the number that breaks it.
* Per-call cost is the same JavaScript: a synchronous call is about 0.2 to 0.35 us through JSI and the
  core, about 1.7 us through `UndraCore.callSync` (before: 9.7 us at the same load; 1.5 us of it is
  building the payload, before: 6 to 6.7 us), and about 7.7 us as an awaited generated method on the iOS
  simulator (before: 24.6 to 25.3 us); a 1 KB round trip through `callSync` is 3.4 to 3.7 us (before: 15.5).
  The native transport answers through its inbox, so the web's direct call does not apply to it.
* One running `UndraCore` per core namespace per process (ADR-044; ADR-038 decision 11 applies per
  namespace). Several cores, of several namespaces, run side by side, and each keeps the native default
  ports' storage of its own (a `Kv` directory, an `Fs` root, a Keychain service and a Keystore key per namespace). A Swift or Kotlin
  Undra host in the same process can hold other cores, but not the same one: a core image is
  initialised once per process. Two React Native instances in one process (a brownfield app with two
  `ReactHost`s) cannot both load the same core: the module cannot tell a second instance from a
  reloaded one, so the second `loadNative` of a namespace stops the first instance's core of that
  namespace, whose calls then fail with `UndraTransportError("closed")` (`UndraCallError.Unavailable`
  through the generated bindings, whose `transport.reason` is `closed`); they never reach the other
  instance's core. Cores of other namespaces are not touched.
* JavaScript-implemented synchronous ports: see "Ports".
* `Clock`, `Rng` and `Timer` are native and cannot be replaced from JavaScript.
* A native default (`Kv`, `SecureStore`, `Fs`, `Connectivity`) is replaced in `loadNative`'s options, not with
  `core.registerPort` afterwards ("Ports").
* `Http` runs on the JS thread's `fetch`: while the JS thread is busy the core's requests wait for it, and bodies
  are base64 across React Native's networking module. A native `Http` is a follow-up if a measurement asks for one.
* In the background the mirror waits: React Native on Android pauses the JavaScript timers of a backgrounded app,
  and the mirror's drain runs on one, so what the core produced meanwhile (a `Lifecycle` or `Connectivity` report,
  a query that refetched) reaches your signals when the app is back, folded to the last value of each signal unless
  the signal is `#[undra(no_coalesce)]`. The core's own threads run on as long as the system lets the app run. If
  your app needs every transition, keep them in the core (a counter, or a bounded history in a signal) rather than
  relying on the mirror to replay them: React and the other UI frameworks render the last value of a frame anyway.

## What is tested where

| Layer | How | Command | In CI |
|---|---|---|---|
| The C++ host (inbox, ports, ownership, shutdown), both shims: finding a core by namespace and refusing its table (unknown core, ABI 1, too short, another namespace, a missing entry), two real cores side by side (`playground_core` and `playground_a`), one host each; the native default ports through the real core (the playground core's `platform` module, a test platform); the JSI layer and the TurboModule compiled against React Native 0.87's headers with `-Werror`; the Apple platform against the iOS SDK, and on this Mac, asked where each core's stores are: per namespace (macOS) | against the real cores, ASan + UBSan | `runtimes/rn/@undra/react-native/cpp/test/run.sh` | `ci.yml`, job "React Native (host + model)", every push and pull request (Linux, clang 18: the dlopen shim; the linked shim needs the Objective-C runtime and runs on macOS) |
| The portable `Kv` and `Fs` (layouts against the Swift runtime's own vectors and SHA-256's, `..`, symbolic links, a directory swapped for a link while a read or write walks through it, atomic writes and a writer killed mid-write, deep deletes) | this machine's file system, ASan + UBSan | the same `run.sh` (step 0) | the same job |
| The Android library's pure Java (the seal against an AES-GCM vector from Node, which `android-adapters` opens too; the network classification; the per-namespace store names) | `javac` and the JDK | `runtimes/rn/@undra/react-native/android/test/run.sh` | the same job |
| `NativeTransport`, the frame scheduler, the polyfills, `loadNative`, which ports are native for which options, `reactNativeHttp()` | a fake module and a scripted `fetch`, on Node | `npm test` in `runtimes/rn/@undra/react-native` | the same job |
| Types of the package and its build config | `tsc`, against `@undra/runtime`'s sources and its emitted declarations (`npm run build` in `runtimes/ts/@undra/runtime` first) | `npm run typecheck` | the same job |
| The contract scenarios S01..S19 and S30 | through `NativeTransport` over a stand-in of the module on the wasm core: 19 pass, S17 (native panic containment) and S29 (the panic report through `Diagnostics`) are app-tested | `npm run test:contract` | the same job |
| JSI, Hermes, `invokeAsync`, the vsync sources, both builds, native panic containment; every default port on the real platform | the playground app's on-device checks RN01..RN21 (RN11..RN16: each default through the core; RN17..RN21: the opt-in `Db`, natively, and `WebSocket` and `Sse` against the script's realtime server), then the script's RN22..RN25 (a secret not in the app's files, `Kv` across a killed process, `Lifecycle` to the background and back, `Connectivity` in airplane mode on Android): the script prints `UNDRA-RN CHECKS 25/25 passed` (24/24 on the iOS simulator, which has no airplane mode) | `scripts/rn-device-checks.sh ios` (iPhone simulator) and `scripts/rn-device-checks.sh android` (emulator or phone; RN22 needs `su`, which emulator images have) | `rn-devices.yml`: on demand, every Monday, and on pull requests that touch `runtimes/rn/**` or `examples/playground/rn/**` |

The contract column proves `NativeTransport` and the module's rules as modelled by the stand-in, not the
C++ module: that is the C++ host test plus RN01..RN10 on a device. Count it as "`NativeTransport` over a
stand-in", never as a native column next to Swift and Kotlin.

## What is verified

**After ADR-044's table** (2026-10-01, `wt/abi-table`): the module reaches each core through its `UndraApi`
table, by namespace. On a Mac (Apple clang, Xcode 26.6):

| What | Where | Result |
|---|---|---|
| The C++ host, both shims, ASan + UBSan, against `playground_core` and `playground_a` in one process | `cpp/test/run.sh`, macOS | 21 + 21 checks (the refusals, two cores side by side, the 14 of before, now per core); `UndraJsi.cpp` and `UndraTurboModule.cpp` compile against 0.87's headers |
| `NativeTransport`, `loadNative` (the generated entry, two namespaces) and friends | `npm test`, `npm run typecheck`, Node 24 | 50 tests; typecheck clean |
| Contract scenarios through `NativeTransport` | `npm run test:contract` | 19 pass, S17 and S29 skipped (app-tested) |
| The on-device checks, iOS | iPhone 17 Pro simulator (iOS 26.5), release core (`PlaygroundCore` pod, no `-force_load`), Release app | `CHECKS 10/10 passed`; RN01 calls through `UndraPlaygroundCore.core`, RN10 also sees `no_such_core` refused |
| The on-device checks, Android | `undra` emulator (arm64, API 35), release core (`libplayground_core.so`), release APK | `CHECKS 10/10 passed`, the same lines |

JavaScript reloads were not re-run after the table (the reload logic is the same, keyed by namespace; the
host test's reload race passes on both shims). The rows below are the record from before it.

Everything below was run on 2026-10-01 at `wt/react-native` with `main` merged in (the schema JSON, device
bench, diagnostics, dev loop and parity pieces: the typed failure model, `snapshot()` and `restore()`), on a Mac (Apple clang, Xcode 26.6) shared with other agents' builds.

| What | Where | Result |
|---|---|---|
| The C++ host, both shims, ASan + UBSan | macOS; also a clean `git clone` (Node 20) running the CI job's steps | 14 + 14 checks, `UndraJsi.cpp` compiles against 0.87's headers |
| `NativeTransport` and friends | Node 24 and Node 20 | 41 tests; typecheck clean |
| Contract scenarios through `NativeTransport` | Node 24 and Node 20 | 19 pass, S17 and S29 skipped (app-tested); S15 uses the public `core.snapshot()` and `core.restore()` |
| The on-device checks, iOS | iPhone 17 Pro simulator (iOS 26.5), release core, Release app, `scripts/rn-device-checks.sh ios` | `CHECKS 10/10 passed` |
| The on-device checks, Android | `undra-rn` emulator (arm64, API 35), release core, release APK, `scripts/rn-device-checks.sh android` | `CHECKS 10/10 passed` |
| JavaScript reload, iOS | debug build on Metro, `POST /reload`: three reloads, one in the middle of the benchmarks (review, before the merge) | four runtimes in one process, 10/10 each |
| JavaScript reload, Android, with the review's fixes | debug build on Metro, `POST /reload`, one process throughout: an idle reload, two in the middle of the benchmarks and two more in quick succession (6 runtimes); three reloads timed to land inside the self-checks (5 runtimes); then 20 in a row (21 runtimes) | every run that reached its end passed 10/10 (6, 2 and 21 runs; the interrupted ones print no verdict), no `CHECK ... FAIL`, no native crash in `logcat` |
| Android frame source, per reload | the same reloads with a counter on the choreographer callbacks (not committed) | 0 callbacks pending when the source was destroyed in 66 reloads (26 plain, 40 with the 10k list's Stream running), `posted == ran` every time; the native heap read from `dumpsys meminfo` is flat within the noise (see below) |
| The default ports (ADR-038 amendment B), on `wt/rn-adapters`, after its review | the C++ under ASan + UBSan on macOS: the stores test, then the host test through the real core with each shim (a reload's start while an old port worker is mid-job included); the Java on the JDK; `npm test` | 15 + 22 + 22 checks, the Apple platform compiles against the iOS SDK; 4 Java checks; 60 tests, typecheck clean; the contract column unchanged (17, S17 app-tested) |
| The default ports on the devices | iPhone 17 Pro simulator (iOS 26.5) and the `undra-rn` emulator (arm64, API 35), release core, Release app, `scripts/rn-device-checks.sh ios` / `android` | `UNDRA-RN CHECKS 24/24 passed` (iOS: RN01..RN24) and `UNDRA-RN CHECKS 25/25 passed` (Android: RN01..RN25, airplane mode included; 2026-10-01, ports-v2) |
| `rn-devices.yml` and the CI job on a GitHub runner | not yet: a runner has not run them | the job's steps were run in a clean clone on the Mac (Linux-only parts, `apt` and `clang++-18`, were not) |

On Android a callback posted with `AChoreographer_postFrameCallback` that is still pending when a reload
quits the JS thread's looper never runs, and the one small allocation it carries (a `std::weak_ptr`, and
the frame source's state it keeps alive, about 100 bytes) is not freed. That needs a frame requested in
the instant of the teardown; it did not happen once in the 66 reloads above (the counter saw no pending
callback each time the source was destroyed), and the native heap (`dumpsys meminfo`, Heap Alloc) was
112.6 MB at launch and 100.6, 101.6, 100.1 and 100.6 MB after 5, 10, 15 and 20 reloads (a second run: 111.0, then 98.8, 99.9, 100.0, 99.4): the noise of a
React Native reload (about a megabyte) is ten thousand times larger than that. It is a development-only cost, bounded by the
number of reloads. The fix, if it is ever seen, is to pass an id instead of a pointer and keep the
pending states in a table that the source's destructor empties.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `Property 'TextDecoder' doesn't exist` at start | something imported `@undra/runtime` (your bindings) before `@undra/react-native`: import it first (Install, step 6) |
| `'import.meta' is currently unsupported` from the Hermes compiler | add `@undra/react-native/babel-plugin` (step 3) |
| `Export namespace should be first transformed by @babel/plugin-transform-export-namespace-from` from Metro | add `@undra/react-native/babel-plugin` (step 3): it rewrites the runtime's `export * as codecs` |
| `the UndraNative TurboModule is not linked into this app` | the package is not a dependency of the app, or `pod install` / the Gradle sync did not run after adding it |
| ``cannot load the Undra core `acme_pay`, libacme_pay.so`` (Android) | the core's `jniLibs` are not packaged (step 5), or not built for the device's ABI |
| ``no Undra core `acme_pay` is linked into this app (there is no class UndraCoreTable_acme_pay)`` (iOS) | the core's pod is missing from the Podfile, or `pod install` did not run after adding it (step 4) |
| `... speaks C ABI 1, this module speaks 2` | the core was built by an Undra older than `@undra/react-native` (before ADR-044's table): rebuild it with `undra build --platform rn` of the same version |
| ``the native module is the core `x`, not `y` `` | a `NativeTransport` was given another core's module: pass the module `installNative(namespace)` returns for its own namespace |
| `found architecture 'arm64', required architecture 'x86_64'` for `lib<namespace>.a`, in a Release build for the simulator | the Release configuration builds every simulator architecture and the core's simulator slice has only `[ios] simulator_archs` (`arm64` by default): build for the active architecture (`ONLY_ACTIVE_ARCH=YES ARCHS=arm64`, as Xcode's Run does) or add `x86_64` to `simulator_archs` |
| `the native default ports are off on this device: ...` in the log (Android) | the package's Android library is not in the app (run the Gradle sync after adding the package), or its context provider was removed without `UndraPlatform.install(context)`; until then `Kv`, `SecureStore`, `Fs` and `Connectivity` are unavailable unless you pass adapters |
| `Http` calls fail with `HttpError.Network` mentioning cleartext (Android) | Android blocks `http://` unless the app's network security config allows the host; use `https://` or allow it (the playground allows only its loopback) |
| `UndraSchemaMismatchError` | the linked core and the generated bindings come from different schemas: run `undra bindgen` and `undra build --platform rn` again |
| `UndraError("state")`: the core is already loaded | the generated entry's core was loaded another way (`entry.load`, `entry.attach`) and is still open: close it first, or keep using `entry.core` |
| `this core is already running in this process (for another JavaScript runtime)` | `NativeTransport.start` for a core that another transport of the process runs; `loadNative` of an open namespace returns its core instead. The running core is not disturbed. (After a reload, a core whose old runtime is still shutting it down is waited for, up to 5 s, before this is reported.) |
| `UndraTransportError("closed")`, or `UndraCallError.Unavailable` with `transport.reason` `closed`, in a runtime that did not close its core | another React Native instance of the process (or the reloaded runtime) started the same core, which stops this one (Limits) |
