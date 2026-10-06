# Fieldbook

A small but real app, written once in Rust and run as a web app, an iOS app and an Android app: **field notes with
photos**, for a team that works where there is no signal. It is the sample of [Undra](https://github.com/shreypdev/undra):
every piece in the [cookbook](https://shreypdev.github.io/undra/docs/cookbook/) working together in one product, through the public
API only (constitution R10).

| What the app does | Where it is written | What it shows |
|---|---|---|
| Signs in with a team code; the tokens live in the secure store; a `401` refreshes once and retries; signing out is refused while notes wait to be sent | `core/src/auth.rs` | the cookbook's [auth](https://shreypdev.github.io/undra/docs/cookbook/auth.html) recipe, slimmed |
| Notes are saved on the device before the screen shows them, and sent to the server when there is a network, even after the app was killed in between | `core/src/notes.rs` (`push_note`, `Notebook::add`) | `Kv`, a persisted offline queue, idempotent mutations ([offline-first](https://shreypdev.github.io/undra/docs/cookbook/offline.html)) |
| The list is searchable, filterable by tag, pinned first, newest first; the tag chips follow the notes | `core/src/notes.rs` (`visible`, `tags`) | a keyed list, a `DerivedList` and a `Computed`: each change is a one-row patch for all three UIs |
| Photos are kept in the app's files and uploaded in the background | `core/src/notes.rs` (`attach_photo`, `push_photo`) | the `Fs` port and the queue |
| An update changes what a note is (`text` became `body`; tags, pins and photos arrived) without losing the notes on the device, the notes in the cache or the writes in the queue | `core/src/notes.rs` (`Note`, `note_from_build_1`) | `#[undra(default)]` and `#[undra::migrate]` ([Shipping an update](https://shreypdev.github.io/undra/docs/updates.html)) |
| Shows who else is in the notebook (feature `presence`) | `core/src/presence.rs` | a WebSocket that reconnects in the core, on the `Timer` port ([real-time](https://shreypdev.github.io/undra/docs/cookbook/realtime.html)) |
| Keeps its state across a rebuild under `undra dev` | every store has a `restore` function; `a_dev_reload_keeps_the_notes_the_filter_and_the_ids` | state-preserving reload (ADR-053) |

```
undra.toml     the project file
core/          the Rust core: auth, notes, presence (feature)
generated/     Swift, Kotlin and TypeScript bindings of it (`undra bindgen`; checked in CI with `--check`)
web/           React + Vite: the whole app, with an in-page demo server (web/src/demo-server.ts)
ios/ android/  SwiftUI and Compose: the same app on the same core
server/        server.mjs: the demo backend for the native apps (Node, no dependencies)
```

## Run it

**Web.** No backend needed: the page answers the core's `Http` port with an in-memory demo server.

```sh
cd web && npm install && npm run dev        # http://localhost:5173, sign in as anyone with the team code `fieldbook`
```

Try it: write a note, tick **offline** (the demo server stops answering and the core is told), write two more, watch
"2 changes waiting to send", reload the page, untick **offline**. The notes are on the device the whole time (IndexedDB),
and the queue is replayed by the core when the network returns.

**iOS and Android** use the same core. They need a server to talk to: `node server/server.mjs` (the Simulator reaches it
at `http://127.0.0.1:8787`, the Android emulator at `http://10.0.2.2:8787`; `FIELDBOOK_SERVER` in the Xcode scheme
points a device elsewhere). Then build the apps as the sections below say.

## Tests

```sh
cargo test -p fieldbook-core                 # the core, against the fakes: no device, no network, no clock
cd web && npm test                           # the demo server, and the whole stack: bindings + the real wasm core
```

The core's tests are the specification of the app: a note is on the device before the screen shows it; a note written
offline is sent, once, with the same `Idempotency-Key`, after the app is killed and relaunched; the view is pinned first
then newest and follows the filter; a restore (what `undra dev` does on a rebuild) keeps the notes, the filter and the
ids; a build-1 note becomes a build-2 note. The web test runs the generated TypeScript bindings against the real core
(compiled to wasm by `undra build --platform web`) and goes through the same offline scenario end to end.

## Previews and stories

The testing kit ([docs/TESTING.md](../../docs/TESTING.md)) runs the app's own core with deterministic fakes as its ports and a manual
clock, so a screen in any state is a few lines. `ios/Fieldbook/Previews/NotebookPreviews.swift` has two SwiftUI `#Preview`s (three
notes; no signal), `web/src/stories/Notebook.stories.tsx` the same two as stories (`npm run dev`, then `/stories.html`), and
`web/src/preview.test.ts` drives the offline scenario with `PreviewCore`: the network, the clock and the idempotency key, exactly.
Android's Compose previews are not included: Android Studio's preview pane cannot load a native core, so they would play a recording
(`RecordedCore`), and the sample has not recorded one.

## Feature `presence`

`cargo test -p fieldbook-core --features presence` runs presence over the opt-in `WebSocket` port (ADR-047; it needs an
`undra` that has the port: `undra = { features = ["websocket"] }`). It is off by default so the default schema, and the
bindings in `generated/`, do not include it; turn it on, run `undra bindgen`, and the stores `Presence`, `Link` and
`Member` appear in all three languages.

## The dev loop

```sh
undra doctor                    # what this machine has, and what is missing
undra dev                       # serve the core over a WebSocket; it rebuilds when core/ changes
undra bindgen                   # after you change a public type or method: regenerate the bindings
undra build --release           # the libraries the apps link, with their sizes (the app builds below run it for you)
undra upgrade                   # move the project to this `undra`'s version, with the migration notes
```

`undra build` is not a manual step: Gradle (`undraBuild`), Xcode (the "Build the Undra core" phase) and Vite (the
`undra()` plugin) each run it before the app builds, and skip it while the core is unchanged. They find `undra`
on `PATH` (`undra doctor` checks that).

`core/src/lib.rs` is the whole app logic. Everything marked `#[undra::api]` crosses into the three
languages; `undra bindgen` reads the core's schema (it builds the core, loads it and asks) and writes
`generated/`. Commit `generated/`, or check it in CI with `undra bindgen --check`.

## iOS

```sh
xcodebuild -project ios/Fieldbook.xcodeproj -scheme Fieldbook \
    -destination 'generic/platform=iOS Simulator' build
```

Open `ios/Fieldbook.xcodeproj` in Xcode 16 or newer to run it. There is no `undra build` to run first: the **Build the
Undra core** Run Script phase, before Compile Sources, runs `undra build --platform ios --configuration $CONFIGURATION`
(a Debug build makes a debug core, a Release build a release one, `build/ios/FieldbookCore.xcframework`). Xcode skips it
while the files in `ios/Config/undra-core-inputs.xcfilelist` are older than the ones in
`ios/Config/undra-core-outputs.xcfilelist`; `undra build` keeps the input list in step with the core's sources, so
commit it. The phase finds `undra` on `PATH` or in `~/.undra/bin`, `~/.cargo/bin` and Homebrew's directories (Xcode
started from the Dock has a short `PATH`) and says how to install it when it is missing; it turns user script
sandboxing off for the target, since it runs Cargo. The app links the core's library, `libfieldbook_core.a`, by its path
in Other Linker Flags, not as a framework: Xcode reads an XCFramework while it plans the build, before the phase could
have made it. Each slice is one prelinked object whose only global symbol is the core's entry, which the bindings call
(`UndraFieldbookCore.load()`), so it needs no `-force_load` and another Undra core can sit next to it in the app.

**Against `undra dev`.** In the scheme's Run environment variables set `UNDRA_DEV_URL` to the `ws://` URL
`undra dev` prints (a simulator can use `ws://127.0.0.1:7443`; a device needs your computer's address and
`undra dev --addr 0.0.0.0:7443`). Debug builds then use that core instead of the linked one: edit the Rust,
save, and the app reconnects and comes back to the screen it was on, with its state (`undra dev` carries the
core's state across a rebuild; the bar at the top says `Reloaded, state kept`, and shows the connection;
`core.connectionState` and `core.connection` are the API). See docs/DEV_LOOP.md in the Undra repository.

## Android

```sh
cd android && ./gradlew :app:assembleDebug    # or open android/ in Android Studio
```

There is no `undra build` to run first: `android/app/build.gradle.kts` has an `undraBuild` task that runs
`undra build --platform android`, `preBuild` depends on it, and Gradle skips it while the core's sources
(`core/src/**`, the Cargo manifests) and `build/android/jniLibs` are unchanged. The variant decides the profile:
`assembleRelease` and `bundleRelease` build a release core (about 1.7 MB per ABI), anything else a debug core (fast to
build, right for the dev loop, but tens of megabytes per ABI: 42 MB for the playground, and the APK carries every
byte of it). `-PundraRelease=true` or `false` overrides, `-PundraSkipBuild=true` (or `UNDRA_SKIP_BUILD=1`) skips
the task when the core was built in an earlier step. Sources outside `core/` (a path dependency in a monorepo) go in
with `undraBuild { sources.from("../../shared/src") }`. The task finds `undra` on `PATH` or in `~/.undra/bin`,
`~/.cargo/bin` and Homebrew's directories (a GUI-launched Gradle has a short `PATH`), `UNDRA_BIN` names one
explicitly, and it says how to install it when it is missing.

The app module packages `build/android/jniLibs` (the path in `android/app/build.gradle.kts` is relative to
the module, `android/app`; after a build `undra` checks that it still names the directory it wrote) and
depends on the generated Kotlin module (`generated/kotlin`, included by `android/settings.gradle.kts`) and on
the Undra runtime (`dev.undra:runtime`).

**Against `undra dev`.** Debug builds can run against the core `undra dev` serves instead of the one in the
APK, so a Rust change needs no rebuild of the app and `undra build --platform android` is not needed at all:

```sh
undra dev --android                                            # prints the addresses; adb reverse for attached devices
cd android && ./gradlew -PundraDevUrl=ws://10.0.2.2:7443 :app:installDebug    # the emulator's name for this machine
adb shell am start -n dev.undra.fieldbook/.MainActivity --es undra_dev_url ws://127.0.0.1:7443   # or switch at run time (USB device)
```

`10.0.2.2` is how the emulator reaches this machine; a USB device uses `adb reverse tcp:7443 tcp:7443` (which
`undra dev --android` runs) and `127.0.0.1`. Cleartext traffic and the dev URL exist in debug builds only
(`android/app/src/debug/AndroidManifest.xml`; release builds get neither). Edit the Rust, save, and the app
reconnects and comes back to the screen it was on, with its state (`undra dev` carries the core's state across a
rebuild; the bar at the top says `Reloaded, state kept` and shows the connection, `core.connectionState` is a
`StateFlow`). See docs/DEV_LOOP.md in the Undra repository.

## Web

```sh
cd web && npm install && npm run dev          # http://localhost:5173
npm run build                                 # type-checks and bundles
```

There is no `undra build` to run first: the `undra()` plugin of `web/vite.config.ts` (`@undra/runtime/vite`) runs
`undra build --platform web` when Vite starts, for `npm run build` as for `npm run dev`, and under `npm run dev` it
rebuilds the core and reloads the page whenever `core/src` changes. It finds `undra` on `PATH` (or `UNDRA_BIN`) and says
how to install it when it is missing; `UNDRA_SKIP_BUILD=1` skips it when the core was built in an earlier step.

The page loads the wasm core and runs it on the main thread. Add `?undra=ws://127.0.0.1:7443` to the URL
(with `undra dev` running) to use the core that `undra dev` serves instead: edit the Rust, save, and the page is
on the rebuilt core with its state, no reload (a dropped connection is reconnected by the runtime; a bar at the top
shows what it is doing, `Reloaded, state kept` after a rebuild, and `core.connection` is the signal behind it). Only
`npm run dev` reads `?undra=`: a production build ignores it.

## Tests

`cargo test` runs the core's tests: no device, no simulator. The core is deterministic (no clock, no
randomness, no threads of its own), so tests are plain Rust.

## More

`undra --help`, `undra <command> --help` and https://shreypdev.github.io/undra/docs/errors.html (every `error[undra::C00NN]` has a page).
