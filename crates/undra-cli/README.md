# undra-cli

The `undra` command: create an Undra app, generate its bindings, build its core for iOS, Android and
the web, and serve the core to a running app while you edit it (`docs/SPEC.md` section 13).

```sh
cargo install --git https://github.com/shreypdev/undra undra-cli   # or brew, npm, curl: see the repository README
undra init todo --platforms ios,android,web
cd todo
undra doctor                         # what this machine has, what is missing, the exact fix (--fix prints them all)
undra dev                            # ws://127.0.0.1:7443: the core, served; rebuilt when core/ changes
undra bindgen                        # after changing a public type: Swift, Kotlin and TypeScript again
undra build --release                # build/ios/TodoCore.xcframework, build/android/jniLibs, build/web/todo_core.wasm
undra upgrade                        # a newer undra: every pin moved in step, bindings regenerated, migration notes
```

`undra build` is not a manual step in a project `undra init` made: Gradle's `undraBuild` task, Xcode's "Build the
Undra core" Run Script phase and the `undra()` Vite plugin (`@undra/runtime/vite`) run it before each app builds
and skip it while the core is unchanged. `undra init` also writes `.github/workflows/undra.yml`: the core (fmt,
clippy, test, `undra bindgen --check`) and one job per app.

| Command | What it does |
|---|---|
| `undra init <name>` | A project: `undra.toml`, a core crate with a working store, generated bindings, and an iOS (Xcode project, SwiftUI), Android (Gradle, Compose) and web (Vite, React) app that use them. Nothing is downloaded; the templates are in the binary. `--undra-path` uses a checkout of the Undra repository instead of released versions. |
| `undra bindgen` | Builds the core as a host library with `undra-ffi` linked in, `dlopen`s it, reads `undra_schema_json` and `undra_schema_hash`, and writes `generated/{swift,kotlin,ts}`. `--schema file.json` skips the build; `--check` fails when the files are stale (CI); `--docs` keeps the core's doc comments. |
| `undra build` | The core for `host`, `ios`, `android`, `web`, with sizes against the blueprint's budgets. `--configuration <Xcode configuration>` (what the Xcode build phase passes) picks debug or release and writes the stamp that tells Xcode which one the XCFramework is. |
| `undra dev` | Builds a small runner around your core, serves it with `undra-transport`, watches `core/` and swaps in the rebuilt core on the same address, carrying the running core's state across (snapshot in memory, restored before the new core listens; ADR-053). A failing build keeps the old core serving. |
| `undra doctor` | Every prerequisite of init, build, dev and the device benchmarks: rustup, stable Rust and its targets, Xcode and a simulator runtime, the Android SDK, platform-tools, NDK r27, `ANDROID_HOME`, a JDK, `adb` and an attached device, Node and npm, `wasm-opt` (optional), free disk space, `undra` on `PATH`. One line each: what was found (`ok`, `missing`, `wrong-version`), the value it saw, the exact fix command and the heading of `docs/ONBOARDING.md` that explains it. `--fix` prints the fixes as one block to paste (nothing is run); `--json` is the report for tools. Exit status 1 if something the project needs is missing. |
| `undra upgrade` | Reads every place a project pins its Undra version (the core's `undra` dependency, `undra.toml`, the web `package.json`, the Gradle scripts, the Xcode project, the workflow), moves them all to this `undra`'s version in the shapes `undra init` writes, regenerates the bindings and prints the migration notes (`src/migrations.rs`) of every release crossed. `--dry-run` shows the lines and writes nothing; a `path` dependency (a checkout) changes nothing; a project newer than the CLI is refused. |
| `undra adopt [path]` | Adds a core to an existing app: one new `undra/` directory and `UNDRA_ADOPT.md` with the exact, path-filled edits for each platform it finds. The app's own project files are never modified. |

Every failure prints what happened, why and what to do, with a stable code
(`error[undra::C0001]` ...; the list is `undra_cli::error::Code`).

## How it is put together

A project is a directory with an `undra.toml`. Its core is an ordinary library crate that depends on
`undra`. What ships to a platform is built from two crates the CLI generates under `target/undra/`:

* the **shim** links the core and the C ABI (`undra-ffi`) into one library that exports the core under
  its namespace (`[core] namespace` in undra.toml, default the core's package name in snake case:
  `todo_core`): one symbol, `todo_core_undra_api`, returning the core's C ABI table (ADR-044). It is
  built with the release profiles of SPEC 7: `cdylib` for the host, Android and web
  (`libtodo_core.{dylib,so}`, `todo_core.wasm`), `staticlib` for iOS;
* the **dev runner** links the core and `undra-transport` into an executable. It is a separate process
  so a core that panics or is being rebuilt cannot take `undra dev` down, and it exits when its
  stdin closes, so it never outlives the CLI.

Both depend on Undra from wherever the core gets it (`cargo metadata` says), so there is exactly one
copy of `undra-runtime` in the build.

### iOS

`TodoCore.xcframework` has one static library per slice, `libtodo_core.a`, **prelinked** (`ld -r`) into
a single object whose only global symbol is `_todo_core_undra_api`: a Rust static library otherwise
exports every Rust symbol it holds, and two of them in one app would collide or silently merge into one
runtime. Each slice carries the core's header, `todo_core_undra.h`, without a module map (the generated
Swift package declares the module `TodoCoreFFI`, and a second definition would fail the build). The
generated package calls the entry, which pulls the one object that holds the whole core, so the app
links the library like any other: no `-force_load`, and another Undra core can sit next to it.

## Tests

`cargo test -p undra-cli` runs the unit tests and the integration tests that drive the real binary:
`init` then `cargo test` on the core, `bindgen --check`, `bindgen --schema` against goldens,
`build --platform web` loaded under Node, and `undra dev` spoken to over a raw WebSocket (including an
edit that rebuilds and restarts the core). The heavy platform builds are switched on by environment
variables (see `tests/platforms.rs`): `UNDRA_TEST_IOS=1`, `UNDRA_TEST_IOS_APP=1`, `UNDRA_TEST_ANDROID=1`,
`UNDRA_TEST_ANDROID_APP=1`, `UNDRA_TEST_WEB_APP=1`. `tests/build_systems.rs` runs the real Gradle, `xcodebuild` and
`npm run build` on a fresh project with no earlier `undra build` and checks that each built the core itself
(`UNDRA_TEST_BUILD_SYSTEMS=1` runs it where the toolchains are, skipping with the reason where they are not;
`UNDRA_REQUIRE_TOOLCHAINS=1` makes a missing toolchain a failure, as CI sets it); the toolchain it needs is decided
by `undra doctor --json`. `tests/upgrade.rs` runs `undra upgrade` on the fixture projects of `tests/fixtures/upgrade/`
(`UNDRA_TEST_UPGRADE_E2E=1` also regenerates the bindings for real against a local clone of the repository), and
`tests/ci_workflow.rs` parses the workflow `undra init` writes.
