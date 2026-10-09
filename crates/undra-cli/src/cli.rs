//! The command line: arguments and the help text that goes with them.
//!
//! The help is meant to teach: each command says what it does, what it needs, what it writes and
//! shows an example, so `undra <command> --help` is enough to use it.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// The `undra` command.
#[derive(Parser, Debug)]
#[command(
    name = "undra",
    version = crate::version::LINE,
    about = "Undra: one Rust core, native iOS, Android and web apps",
    long_about = "Undra owns everything under the pixels of a native app (domain logic, reactive state, the data \
layer, persistence) in one Rust core. The UI stays SwiftUI, Compose and React; `undra` generates the bindings \
they call, builds the core for each platform and serves it to a running app while you edit it.\n\n\
An Undra project is a directory with an undra.toml. Commands find it from the current directory upwards (or use -C).",
    after_long_help = "\
GETTING STARTED
    undra init todo --platforms ios,android,web    a new project: core crate + one app per platform
    cd todo
    undra dev                                      serve the core to running apps, rebuilding on change
    undra build --release                          XCFramework, jniLibs and wasm for the apps to link
    undra bindgen                                  regenerate Swift, Kotlin and TypeScript after a core change

EXISTING APP
    undra adopt ../MyApp                           add an Undra core to an app you already have, step by step

NEW VERSION
    undra upgrade                                  move the project to this `undra`: every pin, the bindings, the migration notes

REVIEWING A CHANGE
    undra schema diff --against origin/main        what changed in the public API, each line breaking or additive (review this, not the bindings)
    undra bindgen --check                          the committed bindings are exactly what the schema generates (CI)

ACROSS PLATFORMS
    undra drift ios.json android.json web.json     where the iOS, Android and web hosts diverged in one recorded flow (--exit-code for CI)

CHECK THE MACHINE
    undra doctor                                   every prerequisite, with the exact fix for each gap (--fix, --json)

Errors are printed as `error[undra::C00NN]` with what happened, why, and what to do; every code is
explained at https://shreypdev.github.io/undra/docs/errors.html#C00NN."
)]
pub struct Cli {
    /// Run as if started in this directory (the project is the nearest undra.toml from here up).
    #[arg(short = 'C', long = "project-dir", global = true, value_name = "DIR")]
    pub project_dir: Option<PathBuf>,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new Undra project: a core crate and an app shell per platform.
    #[command(
        long_about = "Creates a project directory with:\n\
  undra.toml        the project file\n\
  core/            the Rust core: a working to-do store with #[undra::store] and #[undra::api]\n\
  generated/       Swift, Kotlin and TypeScript bindings for that core (already generated)\n\
  ios/ android/ web/   one minimal, real app per platform, using the generated bindings\n\n\
Nothing is downloaded: the templates are part of this binary. The core depends on the Undra crates of the release \
this binary belongs to (`undra = { git = \"https://github.com/shreypdev/undra\", tag = \"v<version>\" }`) and the \
apps on the runtimes of the same release, all from that repository: the Swift package at its root, the Kotlin \
artifacts JitPack builds from the tag (`com.github.shreypdev.undra:runtime:v<version>`) and the npm package attached \
to the GitHub Release (ADR-063). UNDRA_DIST_GIT_URL, UNDRA_DIST_RELEASE_URL and UNDRA_DIST_MAVEN_REPO replace those \
addresses (a rehearsal of a release, or a mirror). Point --undra-path at a checkout of the Undra repository to use its crates and \
runtimes directly instead (a project created inside a checkout does that without the flag).",
        after_long_help = "\
EXAMPLES
    undra init todo                                   all three platforms
    undra init todo --platforms ios,web               only those shells
    undra init todo --id dev.acme.todo                choose the application id / bundle identifier
    undra init todo --undra-path ~/src/undra            use a local checkout of Undra
    undra init todo --ios-deployment-target 15.0      an iOS app that supports iOS 15 and 16 (ObservableObject stores)

NEXT
    cd todo && undra dev        then run an app shell (each README says how)"
    )]
    Init(InitArgs),
    /// Generate the Swift, Kotlin and TypeScript bindings from the core's schema.
    #[command(
        long_about = "Reads the core's schema and writes the bindings the app shells import:\n\
  <out>/swift/     a Swift package (Sources/<Module>/Generated/*.swift + Package.swift)\n\
  <out>/kotlin/    a Gradle module (src/main/kotlin/<package>/*.kt, build.gradle.kts, and a .gitignore for Gradle's output)\n\
  <out>/ts/        an npm package (src/*.ts, package.json, tsconfig.json)\n\n\
Every file says it is generated (the generator, the schema hash, \"Do not edit\"), and each tree gets a .gitattributes \
that marks it linguist-generated, so GitHub collapses it in a pull request (the diff is one click away). Review the \
schema, not the bindings (`undra schema diff`); `--check` proves the bindings match it.\n\n\
Beside the trees it also writes the files that keep a repository's linters out of them: kotlin/.editorconfig (ktlint), \
swift/.swiftlint.yml (SwiftLint, an `excluded:` list to merge into yours), ts/.eslintrc.json (ESLint 8) and \
ts/eslint.config.undra.mjs (ESLint 9, to spread into eslint.config.js). They are part of the manifest like the rest. \
`lint_exclusions = \"none\"` in [bindings] of undra.toml writes none of them, for a repository that configures its \
linters at the root (the Bazel guide lists the lines to add); `\"beside\"` is the default. The `@file:Suppress` line at the \
top of every Kotlin file is not one of these files: it is part of the file and stays.\n\n\
By default the schema comes from the core itself: the core is built as a host library with `undra-ffi` \
linked in, loaded, and asked for `undra_schema_json` (docs/SPEC.md 13). With --schema it is read from a file \
instead and nothing is built. Files an earlier run wrote and this one does not are removed; files you added \
next to them are never touched.",
        after_long_help = "\
EXAMPLES
    undra bindgen                          build the core, extract the schema, write <project>/generated
    undra bindgen --out ../shared/bindings
    undra bindgen --schema schema.json     no build: generate from a schema file
    undra bindgen --check                  exit 1 if the generated files are stale (for CI)
    undra bindgen --docs                   include the core's doc comments (see below)

DOC COMMENTS
    The core's schema carries the doc comments of the Rust source, and the generated code can carry them
    too: with --docs every type, field, variant, method and port in the bindings is documented with the
    text of its `///` comment. Without it the bindings have no doc comments, as before. The schema hash
    covers neither, so the two outputs have the same hash and the same wire."
    )]
    Bindgen(BindgenArgs),
    /// Build the core for iOS, Android, the web or this machine.
    #[command(
        long_about = "Builds the core for each platform and puts the result where the app shells look for it (below \
`build/`):\n\
  ios       build/ios/<Namespace>Core.xcframework       device + simulator slices, each one prelinked lib<namespace>.a, and the core's header\n\
  android   build/android/jniLibs/<abi>/lib<namespace>.so   arm64-v8a and x86_64, 16 KB page aligned\n\
  web       build/web/<namespace>.wasm                  release profile, then wasm-opt -Oz if installed\n\
  host      build/host/lib<namespace>.{dylib,so}        for the JVM tests and undra bindgen\n\n\
A release build also writes the symbol files that resolve a crash report to file:line, below `build/symbols` with a \
manifest.json (`--no-symbols` skips them): the unstripped Android libraries (`android/<abi>/lib<namespace>.so`, and \
`android/native-debug-symbols.zip` for the Play Console), the web module with its names (`web/<namespace>.debug.wasm`, the shipped \
code) with its function map (`web/<namespace>.wasm.functions.txt`) and a debug module with DWARF for browsers \
(`web/<namespace>.dwarf.wasm`), and next to a host library its dSYM. iOS needs no file of its own: the prelinked core's debug \
info lands in your app's dSYM (Xcode Release, `dwarf-with-dsym`). The shipped copies are stripped; they are the same code, and \
not larger than without symbols. `undra symbolicate` resolves a report with these files.\n\n\
Every name comes from the core's namespace (`[core] namespace` in undra.toml, default the core's package name in snake \
case): a core exports one symbol, `<namespace>_undra_api`, so several cores can sit in one app (ADR-044).\n\n\
The library is the core plus the Undra C ABI, built from a small crate generated below `target/undra/` \
of Cargo's target directory (the workspace's, when the core is a member of one; you never write the crate). \
Sizes are printed at the end next to the budgets of the design. A release build for iOS or Android uses the \
`release-mobile` profile of that crate (`release` with `opt-level = \"s\"`: smaller, and panics still unwind); \
`opt_level = \"z\"` in `[ios]` or `[android]` of undra.toml builds the smallest library instead (calls about 1.5x \
slower), `opt_level = \"3\"` the fastest. The web build has `release-wasm`, the host `release`. A debug Android core is \
tens of megabytes per ABI: package with `undra build --platform android --release`.",
        after_long_help = "\
EXAMPLES
    undra build                                   every platform in undra.toml, debug
    undra build --release                         optimized: what you ship, and its symbol files (build/symbols)
    undra build --release --no-symbols            the same shipped files, without build/symbols
    undra build --platform web --release
    undra build --platform android,host

iOS
    Each slice is one prelinked object whose only global symbol is the core's entry, which the generated
    Swift package calls: link it like any other library (no -force_load), next to other cores if you have them.

NOT A MANUAL STEP
    In a project made by `undra init` the app builds run this for you, each only when the core changed:
    Gradle's `undraBuild` task (preBuild depends on it; debug or release follows the variant), the
    \"Build the Undra core\" Run Script phase of the Xcode project (it passes `--configuration
    $CONFIGURATION`) and the `undra()` Vite plugin (on start, and on every change of core/src under
    `vite dev`). They find `undra` on PATH; `undra doctor` checks that it is there."
    )]
    Build(BuildArgs),
    /// Resolve the addresses of a panic report to `symbol (file:line)` with the symbol files of a release build.
    #[command(
        long_about = "Takes the frames of a panic report, as the app's `onPanic` receives it (an `UndraPanicReport`, serialised as JSON), \
or bare addresses, and prints one line per frame: `address symbol (file:line)`. The symbol files are the ones \
`undra build --release` wrote below `build/symbols` (not with `--no-symbols`); this command finds the right one by the \
report's image id (then its namespace) in `build/symbols/manifest.json`, and reads it with the platform's tool: `atos` for an \
iOS app (with the app's dSYM), `llvm-symbolizer` for an Android library (the unstripped twin; the NDK's is used) and for the \
web module (the function map names the function, the DWARF module the line where it is defined).\n\n\
THE INPUT\n\
A report is a JSON object with `frames` (each `{ \"address\": 134567 }`; a number, \"0x20e5f\", or a V8 stack position \
`wasm-function[41]:0x1a2b`) and, to choose the symbol file, `imageId` (lowercase hex; a UUID with dashes is fine) and `namespace`; \
`coreVersion` and `schemaHash` are checked against the symbols and a mismatch is warned about. `-` reads the report from \
standard input. Without a report, give addresses and `--platform` (and `--image-id` when there is more than one build).\n\n\
WHAT AN ADDRESS IS\n\
Native (iOS, Android, host): the instruction address minus the load address of the image that holds the core, pointing into the \
call instruction (so nothing is subtracted before the lookup). On iOS the image is the app executable: `atos` is given the \
address plus the `__TEXT` start of the app's dSYM. wasm: the module offset of the `wasm-function[i]:0x…` line of a V8 stack \
trace (`0x…` counts bytes from the start of the module): the function map names the function it is in exactly, and the line is where \
that function is defined (the shipped module cannot keep line tables inside its functions: `wasm-opt` would skip the passes that make it small), \
so the panic's own `location` in the report is the line of the panic.\n\n\
THE MANIFEST\n\
`build/symbols/manifest.json` lists, per shipped image (an Android ABI, an iOS slice, the web module, a host library): platform, \
namespace, coreVersion, schemaHash, arch, format (elf, macho, wasm), imageId (ELF build id; the SHA-256 of the wasm module; for iOS \
null, because the image is the app, whose UUID is that of its dSYM), sha256 and size of the shipped file, and `symbols` (and for \
the web `functionMap` and `dwarf`), paths relative to the manifest's directory.",
        after_long_help = "\
EXAMPLES
    undra symbolicate report.json                              resolve a report with build/symbols of this project
    undra symbolicate report.json --symbols ./symbols          the symbols artifact of a CI build, unpacked
    undra symbolicate report.json --dsym App.app.dSYM          iOS: the app's dSYM (Spotlight finds it by UUID when not given)
    undra symbolicate --platform web 'wasm-function[1489]:0x5a692'
    undra symbolicate --platform android --image-id 16ed68dc3a85066f424f34d3d8020a5f030c614f 0x998ef 0x9983f
    cat report.json | undra symbolicate -

OUTPUT
    0x998ef playground_core::lab::explode (/undra/app/core/src/lab.rs:222)
    Release builds name the project /undra/app, the Undra checkout /undra/src, Cargo's sources /undra/deps and the
    home directory ~ in the paths; the standard library's are /rustc/<commit>/…"
    )]
    Symbolicate(SymbolicateArgs),
    /// Serve the core over a WebSocket to running apps, rebuilding when the code changes.
    #[command(
        long_about = "Runs the core on this machine and serves it over a WebSocket. A simulator, a phone or a \
browser tab connects with the `remote` transport of its Undra runtime and uses this core instead of a built-in \
one: edit Rust, save, and the core is rebuilt and restarted. The apps reconnect by themselves, with backoff, \
whenever the connection drops (a rebuild, a phone that slept, a restarted adb): nothing to relaunch. The \
state of the core is carried across a rebuild: before the old core stops it is snapshotted (in this \
process's memory, never on disk) and the new core is restored from it, so the apps come back to the screen \
they were on; a changed schema, or --no-keep-state, starts the new core fresh and says so.\n\n\
Logs, including the development records of docs/SPEC.md 5.10 (a line per transaction commit, port call and \
panic), are printed here, with a line for each client that connects, reconnects or leaves. Clocks, randomness and \
logging are answered by this machine because a remote client cannot answer a synchronous port.\n\n\
The banner also prints the address of the devtools page (behind a per-run token, on a loopback address): the \
stores and their live values, a timeline of every change-set with its diff, a scrubber that restores the core to \
an earlier step (the app follows), the port-call and query-cache logs and the counters. docs/DEV_LOOP.md has it.",
        after_long_help = "\
EXAMPLES
    undra dev                               listen on 127.0.0.1:7443
    undra dev --android                     also `adb reverse` the port to every attached Android device
    undra dev --addr 0.0.0.0:7443           reachable from a phone on your network (no authentication!)
    undra dev --no-watch                    build once and serve
    undra dev --no-keep-state               every rebuilt core starts fresh, as before the state was carried over
    undra dev --record session.json         also write the session as an undra.recording, to preview or test with (docs/TESTING.md)
    undra dev --record s.json --record-secrets   keep SecureStore values in it (by default they are left out)
    undra dev --devtools off                do not serve the devtools page (auto: loopback addresses only)

CONNECTING
    web       UndraCore.load({ mode: \"remote\", url: \"ws://127.0.0.1:7443\", expectedSchemaHash })   (or ?undra=ws://... in the page URL)
    iOS       UNDRA_DEV_URL=ws://<your Mac>:7443 (the generated app reads it in debug builds)
    Android   emulator: ws://10.0.2.2:7443; USB device: `adb reverse tcp:7443 tcp:7443` (or --android), then ws://127.0.0.1:7443;
              the generated app reads it from the undra_dev_url launch extra, or ./gradlew -PundraDevUrl=... (debug builds only)

The banner prints these with the real port. docs/DEV_LOOP.md has the rest: reconnecting, troubleshooting.

The server has no authentication. Keep the default loopback address unless a device has to reach it."
    )]
    Dev(DevArgs),
    /// Check the toolchains and SDKs this machine has against what the project needs.
    #[command(
        long_about = "Checks every prerequisite that `undra init`, `build`, `dev` and the device benchmarks use, and prints \
one line per finding. Each one says what was found (ok, missing, or the wrong version), the value it saw, the exact \
command that fixes it and the heading of docs/ONBOARDING.md that explains it. It covers: rustup, stable Rust at the \
MSRV or newer and the Rust targets of the platforms (wasm32; the iOS device and simulator targets; the Android ABIs \
of undra.toml); full Xcode against the command line tools and a simulator runtime; the Android SDK, platform-tools, \
NDK r27, ANDROID_HOME and ANDROID_NDK_HOME, JDK 17, adb and whether a device or emulator is attached, and \
the Gradle wrapper; Node 20+ and npm; wasm-opt (optional: it makes the wasm core 10-20% smaller); free disk space \
(a warning under 10 GB); and whether `undra` is on PATH, since the Gradle task, the Xcode build phase and the Vite \
plugin of a project run it by name. Checks for people who work on Undra (the Kotlin compiler, the `undra` emulator) are \
marked `for contributors`. Inside a project only the platforms of undra.toml are checked; elsewhere all of them. \
Exits with status 1 when something the project needs is missing.",
        after_long_help = "\
EXAMPLES
    undra doctor
    undra doctor --platform ios
    undra doctor --fix                  the commands that close every gap, as one block to paste (nothing is run)
    undra doctor --json                 the report for tools: state, observed value, fix and docs per finding

STATUS
    ok         present and fine
    warn       something is off, builds still work (an optional tool, a variable that is not set)
    FAIL       a build for a platform in scope cannot work
    skip       does not apply here (iOS on Linux, a contributor-only tool for everyone else)"
    )]
    Doctor(DoctorArgs),
    /// Add an Undra core to an existing app, without touching the app's own project files.
    #[command(
        long_about = "Looks at an existing iOS, Android or web app repository and adds an Undra core to it the \
conservative way: one new `undra/` directory (a core crate, undra.toml and UNDRA_ADOPT.md) and a list of the \
exact steps to wire it into the app. The app's own project files are never modified: you make the few \
edits yourself, and the steps are written down with the paths already filled in.",
        after_long_help = "\
EXAMPLES
    undra adopt ../MyApp                          detect platforms, write ../MyApp/undra/
    undra adopt . --platform ios                  only the iOS steps
    undra adopt ../MyApp --undra-path ~/src/undra   use a local checkout of Undra

WHAT IT DETECTS
    iOS      *.xcodeproj / Package.swift        Android  settings.gradle(.kts) + AndroidManifest.xml
    web      package.json (vite, webpack, next, ...)"
    )]
    Adopt(AdoptArgs),
    /// Compare the public API of two schemas (breaking or additive), or export the core's schema to a file.
    #[command(
        long_about = "The schema is the API (docs/SPEC.md 2): the generated Swift, Kotlin and TypeScript are an artifact of it, \
thousands of lines that `undra bindgen --check` proves current and that a reviewer should not have to read. This command \
group is what to review instead:\n\
  undra schema diff     what changed between two schemas, one line per change, each marked breaking or additive\n\
  undra schema export   write the core's schema as the JSON file `diff` reads\n\n\
Commit the schema file (`schema.json`, next to `generated/`) and a pull request shows the API change as a few lines of \
JSON and, with `undra schema diff --against <base>`, as a list of what it means for the apps.",
        after_long_help = "\
EXAMPLES
    undra schema export -o schema.json                 write the API of this project's core (commit it)
    undra schema diff --against origin/main            the same file at origin/main against the one in the working tree
    undra schema diff --against origin/main --exit-code    ... and exit 1 when a line is breaking (CI)
    undra schema diff old.json new.json                two schema files"
    )]
    Schema(SchemaArgs),
    /// Compare recordings of one flow made on different platforms and report where the hosts diverged.
    #[command(
        long_about = "Compares two or more recordings of the same user flow (`undra dev --record FILE`, one per platform: iOS, \
Android, web) and reports where the hosts diverged at the boundary. The core is deterministic, so with the same inputs \
every platform gets the same change-sets: the drift that matters is in the inputs, and that is what is compared. The \
first file is the reference; every other file is compared with it. Five kinds of line: `calls` (a host call missing, \
extra, or made with other arguments; the sequence is diffed, so one skipped tap does not shift every later line), \
`ports` (a port method called another number of times, or with other arguments, or answered differently by the \
platform's adapters), `events` (the host-pushed events, Connectivity and Lifecycle), `observes` (a signal observed on \
one side only) and `state` (a signal whose final value differs). Handles are matched by constructor and order, so the \
first `Todos` of one session is the first `Todos` of another; call ids, timer ids and `t` are never compared.\n\n\
Always ignored, and said so in the header: the replies of `Clock.*` and `Rng.*`, the arguments of `Timer.*`, and, with a \
schema to find it, the value of an `Idempotency-Key` header inside `Http.request` arguments. --ignore adds dotted paths, matched after \
decoding: `Todos.add.title` (a parameter), `Http.request.req.headers` (a field inside one), `Todos.add` (the whole \
arguments), `Todos.todos` (a signal's final value).\n\n\
The schema names the ids and decodes the bytes (`Todos.add`, `{\"title\":\"Buy milk\"}`): --schema FILE, else \
`schema.json` in the project directory, else the project's core is built and asked, as `undra schema export` does; \
outside a project without --schema, ids and bytes are compared and the report says so. Every file must be a recording \
of the same schema as the reference (and as the schema); another one is refused (C0009).\n\n\
The output goes to stdout and the exit status is 0 whatever is found, so it can be read freely; --exit-code makes it 1 \
when any line was printed, for a CI gate. docs/TESTING.md has how to record each platform.",
        after_long_help = "\
EXAMPLES
    undra drift ios.json android.json web.json          where the three hosts diverged, iOS as the reference
    undra drift ios.json android.json --exit-code       ... and exit 1 on any divergence (CI)
    undra drift --schema schema.json ios.json web.json  name and decode with this schema (outside the project)
    undra drift --ignore Todos.add.title a.json b.json  leave a parameter out of the comparison

OUTPUT
    Drift: ios.json (reference) against android.json, web.json (schema 0x3c17cd5f59bb6f68)
    Ignored: the replies of Clock.* and Rng.*, the arguments of Timer.*, the Idempotency-Key header of Http.request, and t

    android  calls     Todos.toggle on Todos#0 #5: missing on android ({\"id\":\"00000000-0000-0001-0000-000000000000\"})
    android  ports     Http.request #1: reply differs: ios {\"body\":{..,\"status\":200},\"status\":\"ok\"}, android {..}
    web      calls     Todos.add on Todos#0 #6: extra on web ({\"title\":\"Pay the rent\"})

    3 divergences on 2 of 3 recordings

RECORDING EACH PLATFORM
    undra dev --record ios.json                          one server per platform: the dev server serves one client at a time
    undra dev --addr 127.0.0.1:7444 --record android.json"
    )]
    Drift(DriftArgs),
    /// Move a project to the version of this `undra`: every pin in step, bindings regenerated, migration notes.
    #[command(
        long_about = "Reads the Undra version a project pins in every place `undra init` writes it: the core's \
`undra` dependency in Cargo.toml (a git tag, or a registry version; a dependency pinned by `rev` or `branch` is \
pinned by the release's tag afterwards), `[undra] version` in undra.toml, \
`@undra/runtime` (and `@undra/react-native`, `@undra/testkit`) in package.json, the runtime's Kotlin modules \
(`com.github.shreypdev.undra:runtime`, `:android-adapters`, ...; the `dev.undra:android-adapters` group of before \
too) in the Gradle scripts, the Undra Swift package in the Xcode project, and `UNDRA_VERSION` in the CI workflow. It \
moves them all to the version of this `undra` in one step, shaped as `undra init` would write them (the release \
asset's URL, the release's tag; so an upgraded project and a new one agree, ADR-063), adds the Kotlin runtime's \
repository to settings.gradle.kts when the pins move to it, regenerates the bindings (`undra bindgen`), and prints the migration notes of every release the project crosses.\n\n\
Only the version text changes; comments and formatting stay. A version that is not written out (a Gradle variable) \
is left and named. The files are written all or none. Your app's own project files (the Xcode build \
phase, the Gradle task, vite.config.ts) are not edited: the notes say what a newer `undra init` adds. A project that \
depends on a checkout of the Undra repository (`--undra-path`, a `path` dependency) is on whatever that checkout \
is: the command says so and changes nothing. A project newer than this `undra` is refused (error C0014): update the CLI.",
        after_long_help = "\
EXAMPLES
    undra upgrade                      move the project here, regenerate the bindings, print the notes
    undra upgrade --dry-run            show the lines that would change and the notes; write nothing
    undra upgrade --no-bindgen         move the pins only (run `undra bindgen` yourself)
    undra upgrade --docs               keep the core's doc comments in the regenerated bindings

AFTER
    git diff                           review what moved
    cd web && npm install              refresh package-lock.json when the web pin moved
    Cargo.lock follows at the next build (it fetches the new tag, so it needs the network)"
    )]
    Upgrade(UpgradeArgs),
}

/// Arguments of `undra init`.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// The project name: letters, digits, `-` and `_` (for example `todo-app`).
    pub name: String,

    /// Platforms to create app shells for: ios, android, web (comma separated).
    #[arg(
        long,
        visible_alias = "targets",
        value_name = "LIST",
        default_value = "ios,android,web"
    )]
    pub platforms: String,

    /// Application id and iOS bundle identifier; default com.example.<name>.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Use the crates and runtimes of this checkout of the Undra repository instead of the release
    /// of this binary (default: the checkout the project is created inside, if there is one).
    #[arg(long, value_name = "DIR", env = "UNDRA_PATH")]
    pub undra_path: Option<PathBuf>,

    /// Create the project inside this directory (default: the current one).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// The lowest iOS the app supports (`[ios] deployment_target`): 17.0 by default; 15.0 or 16.0 make
    /// the Swift stores `ObservableObject`s and the iOS app `@StateObject` based (docs/IOS_15_16.md).
    #[arg(long, value_name = "VERSION")]
    pub ios_deployment_target: Option<String>,
}

/// Arguments of `undra bindgen`.
#[derive(Args, Debug)]
pub struct BindgenArgs {
    /// Generate from this schema JSON file instead of building the core.
    #[arg(long, value_name = "FILE")]
    pub schema: Option<PathBuf>,

    /// Read the schema from this host library of the core, built already (`undra build --platform host`, or what a build
    /// system such as Bazel made), instead of building one. The project's `[core] namespace` names its entry point.
    #[arg(long, value_name = "FILE", conflicts_with = "schema")]
    pub library: Option<PathBuf>,

    /// Write below this directory (default: `[paths] generated` of undra.toml, else `generated`).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Extract the schema from a release build of the core.
    #[arg(long)]
    pub release: bool,

    /// Only these platforms' bindings (ios = Swift, android = Kotlin, web = TypeScript).
    #[arg(long, value_name = "LIST")]
    pub platforms: Option<String>,

    /// Write nothing; exit with an error if the generated files differ from what would be written.
    #[arg(long)]
    pub check: bool,

    /// Keep the core's doc comments in the bindings.
    #[arg(long)]
    pub docs: bool,

    /// Name of the core crate, for a schema that has none (a canonical schema JSON file).
    #[arg(long, value_name = "NAME")]
    pub crate_name: Option<String>,

    /// How the Swift stores are observed: `observation` (`@Observable`, iOS 17 and later) or
    /// `observable-object` (`ObservableObject` with `@Published`, iOS 15 and later). Overrides
    /// `[bindings] swift_observation`, which defaults to what `[ios] deployment_target` allows.
    #[arg(long, value_name = "MODE")]
    pub swift_observation: Option<String>,

    /// Generate the Swift for this iOS floor (`15.0`, `16.0`, `17.0`) instead of `[ios]
    /// deployment_target`: for CI that checks an app's bindings also work at an older iOS.
    #[arg(long, value_name = "VERSION")]
    pub ios_deployment_target: Option<String>,
}

/// Arguments of `undra schema`.
#[derive(Args, Debug)]
pub struct SchemaArgs {
    /// What to do with schemas.
    #[command(subcommand)]
    pub command: SchemaCommand,
}

/// The subcommands of `undra schema`.
#[derive(Subcommand, Debug)]
pub enum SchemaCommand {
    /// What changed in the public API between two schemas: one line per change, each breaking or additive.
    #[command(
        long_about = "Compares two schemas (the JSON `undra schema export` writes, or any file `undra bindgen --schema` reads) and \
prints one line per difference an app can notice: records, fields, enums and their cases, objects, stores and their \
signals, constructors, methods, functions, ports, callbacks, queries and mutations. Each line is marked `breaking` \
(code written against the old bindings can stop compiling in Swift, Kotlin or TypeScript: a removed or changed \
signature, an added enum case, an added record field, even one with a default, which a TypeScript object literal \
must name) or `additive` (it cannot: an added function, method, store, object or record). The rules are in \
docs/SPEC.md 2.6. The order of the output is fixed: types, objects and stores, \
functions, ports, callbacks, queries, by name, so the same two schemas print the same text. Doc comments and wire ids are \
not compared.\n\n\
With --against the old side is FILE as committed at a git ref (`git show <ref>:<path>`; nothing is built) and the new \
side is FILE in the working tree. FILE is `schema.json` in the project directory unless named. Since nothing is built, \
the file is checked against the project's generated bindings, whose headers name the schema hash they were made from: \
where the two disagree, in the working tree or at the ref, a warning says the file is stale (and --exit-code fails on a \
stale working tree). Without --against give the old and the new file, in that order.\n\n\
The output goes to stdout and the exit status is 0 whatever the diff says, so a reviewer can run it freely; --exit-code \
makes it 1 when any line is breaking, for a CI gate.",
        after_long_help = "\
EXAMPLES
    undra schema diff --against origin/main            the working tree's schema.json against origin/main's
    undra schema diff --against HEAD~3 api/schema.json a file in another place
    undra schema diff old.json new.json                two files
    undra schema diff --against origin/main --exit-code    fail CI on a breaking change

OUTPUT
    breaking  field Todo.title: type changed from String to Option<String>
    additive  function archive: added (fn archive(id: TodoId) -> Result<(), TodoError>)
    2 breaking, 3 additive.

NO schema.json YET
    undra schema export -o schema.json        build the core, read its schema, write the file; commit it"
    )]
    Diff(SchemaDiffArgs),
    /// Write the core's schema (the API) as a JSON file, for `undra schema diff` and for review.
    #[command(
        long_about = "Builds the core as a host library, loads it and writes its schema as JSON, every type and every small definition on one \
line so a changed field is one changed line: the same document `undra bindgen` generates from, without the doc comments unless you ask for them (so fixing a comment is not an API change). Commit the file; a \
pull request then shows the API change as a few lines of JSON, `undra schema diff --against <base>` says what they mean, and \
a CI step `undra schema export --check` keeps it as current as `undra bindgen --check` keeps the bindings (it builds the \
core and fails when the file is not what it would write). Without -o the JSON goes to stdout.",
        after_long_help = "\
EXAMPLES
    undra schema export -o schema.json           write the schema to schema.json
    undra schema export > schema.json            the same through the shell
    undra schema export --docs -o docs/schema.json    with the doc comments
    undra schema export --check                  fail when schema.json is not the core's schema (CI)"
    )]
    Export(SchemaExportArgs),
}

/// Arguments of `undra schema diff`.
#[derive(Args, Debug)]
pub struct SchemaDiffArgs {
    /// Compare FILE as committed at this git ref (a branch, tag or commit) with FILE in the working tree.
    #[arg(long, value_name = "REF")]
    pub against: Option<String>,

    /// Exit with status 1 when any line is breaking, or when --against finds the schema file stale
    /// against the bindings (for CI); the output is the same.
    #[arg(long)]
    pub exit_code: bool,

    /// Two schema files, old then new; with --against, at most one: the schema file (default `schema.json` in the project directory).
    #[arg(value_name = "OLD NEW | FILE")]
    pub files: Vec<PathBuf>,
}

/// Arguments of `undra drift`.
#[derive(Args, Debug)]
pub struct DriftArgs {
    /// The schema that names the ids and decodes the bytes (default: `schema.json` in the project directory, else the
    /// project's core is built and asked; outside a project, ids and bytes are compared).
    #[arg(long, value_name = "FILE")]
    pub schema: Option<PathBuf>,

    /// Leave this out of the comparison (repeatable): `Todos.add.title`, `Http.request.req.headers`, `Todos.add`
    /// (the whole arguments), `Todos.todos` (a signal's final value).
    #[arg(long, value_name = "PATH")]
    pub ignore: Vec<String>,

    /// Exit with status 1 when any divergence was found (for CI); the output is the same.
    #[arg(long)]
    pub exit_code: bool,

    /// The reference recording, then every recording to compare with it (`undra dev --record FILE`).
    #[arg(value_name = "REFERENCE OTHER", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,
}

/// Arguments of `undra schema export`.
#[derive(Args, Debug)]
pub struct SchemaExportArgs {
    /// Write the schema to this file instead of standard output (relative paths are relative to `-C`).
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Keep the core's doc comments in the file.
    #[arg(long)]
    pub docs: bool,

    /// Extract the schema from a release build of the core.
    #[arg(long)]
    pub release: bool,

    /// Write nothing: fail when the file (-o, or `schema.json` in the project directory) is not
    /// exactly what this command would write (for CI, as `undra bindgen --check` is for the bindings).
    #[arg(long)]
    pub check: bool,
}

/// Arguments of `undra build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// What to build for: ios, android, web, host, rn (React Native: ios and android plus the
    /// UndraCore pod) (comma separated; default every platform in undra.toml).
    #[arg(long, visible_alias = "platforms", value_name = "LIST")]
    pub platform: Option<String>,

    /// Optimized build (LTO, one codegen unit) instead of a debug build. The web build is always
    /// optimized. iOS and Android release builds are tuned for size (the `release-mobile` profile:
    /// `opt-level = "s"`, and a panic still unwinds so the core can contain it); `opt_level` in
    /// `[ios]` / `[android]` of undra.toml chooses `"z"` (smallest) or `"3"` (fastest) instead.
    #[arg(long)]
    pub release: bool,

    /// Do not write the symbol files of a release build (`build/symbols`: the unstripped Android libraries, the web debug
    /// modules and function map, the manifest). The shipped copies are the same either way, and the shim is built without
    /// debug info, as before ADR-046.
    #[arg(long)]
    pub no_symbols: bool,

    /// An Xcode build configuration, as the Run Script phase of the generated Xcode project passes
    /// it (`$CONFIGURATION`): `Release` and names that contain it build release, any other name
    /// debug. After an iOS build it writes the stamp that tells Xcode which configuration the
    /// XCFramework is for.
    #[arg(long, value_name = "NAME", conflicts_with = "release")]
    pub configuration: Option<String>,
}

/// Arguments of `undra symbolicate`.
#[derive(Args, Debug)]
pub struct SymbolicateArgs {
    /// A panic report as JSON (a file, or `-` for standard input) and/or addresses (`0x20e5f`, `6699`, `wasm-function[41]:0x1a2b`).
    #[arg(value_name = "REPORT_OR_ADDRESS")]
    pub inputs: Vec<String>,

    /// Only the artefacts of this platform: ios, android, web, host (needed for bare addresses without `--image-id`).
    #[arg(long, value_name = "PLATFORM")]
    pub platform: Option<String>,

    /// The image the addresses are in (the report's `imageId`): an ELF build id, a Mach-O UUID or the SHA-256 of a wasm module.
    #[arg(long, value_name = "ID")]
    pub image_id: Option<String>,

    /// The directory with `manifest.json` and the symbol files (default: `build/symbols` of the project).
    #[arg(long, value_name = "DIR")]
    pub symbols: Option<PathBuf>,

    /// The dSYM of the iOS (or macOS) app that crashed: the bundle, or the DWARF file inside it. Spotlight looks for it by UUID when not given.
    #[arg(long, value_name = "PATH")]
    pub dsym: Option<PathBuf>,

    /// The architecture of a dSYM with several slices, when the report names no image id (`arm64`, `x86_64`).
    #[arg(long, value_name = "ARCH")]
    pub arch: Option<String>,
}

/// Arguments of `undra dev`.
#[derive(Args, Debug)]
pub struct DevArgs {
    /// Address to listen on (port 0 picks a free one).
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:7443")]
    pub addr: String,

    /// Build once and serve; do not rebuild when the code changes.
    #[arg(long)]
    pub no_watch: bool,

    /// Log records below this level (0 trace .. 5 fatal) are not printed.
    #[arg(long, value_name = "LEVEL", default_value_t = 1)]
    pub log_level: u8,

    /// Run `adb reverse` for the server's port on every attached Android device or emulator (only the one in ANDROID_SERIAL, when set).
    #[arg(long)]
    pub android: bool,

    /// Start every rebuilt core from fresh state instead of carrying the running core's state over (the escape hatch for "my logic changed under the restored state").
    #[arg(long)]
    pub no_keep_state: bool,

    /// Record the session (every call, reply, change-set, port call and the core's clock and random readings) into FILE as an
    /// `undra.recording` (docs/TESTING.md). A reload starts a new core and so a new file: FILE, then `NAME-2.json`, `NAME-3.json`.
    #[arg(long, value_name = "FILE")]
    pub record: Option<PathBuf>,

    /// Keep `SecureStore` values (tokens, passwords) in the recording. By default the calls of the `SecureStore` port are recorded with empty arguments and replies, so the file can be shared; HTTP headers and bodies, `Kv` values and files are recorded as they are either way.
    #[arg(long, requires = "record")]
    pub record_secrets: bool,

    /// Serve the devtools page (store viewer, change-set timeline with time travel, port and query logs, counters) at /devtools: `auto` on a loopback address only, `on` always, `off` never. Every request needs the token in the printed address.
    #[arg(long, value_enum, value_name = "MODE", default_value = "auto")]
    pub devtools: DevtoolsMode,
}

/// When `undra dev` serves its devtools page (`--devtools`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DevtoolsMode {
    /// On a loopback address only.
    Auto,
    /// Always, whatever the address: the token is still required.
    On,
    /// Never.
    Off,
}

/// Arguments of `undra doctor`.
#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Check only these platforms (ios, android, web; comma separated).
    #[arg(long, value_name = "LIST")]
    pub platform: Option<String>,

    /// Print the commands that close the gaps as one block to paste into a shell (nothing is run).
    #[arg(long, conflicts_with = "json")]
    pub fix: bool,

    /// Print the report as JSON: every finding with its state, observed value, fix commands and
    /// documentation anchor.
    #[arg(long)]
    pub json: bool,
}

/// Arguments of `undra upgrade`.
#[derive(Args, Debug)]
pub struct UpgradeArgs {
    /// Show the lines that would change and the migration notes; write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Move the pins but do not regenerate the bindings afterwards.
    #[arg(long)]
    pub no_bindgen: bool,

    /// Keep the core's doc comments in the regenerated bindings (`undra bindgen --docs`).
    #[arg(long)]
    pub docs: bool,
}

/// Arguments of `undra adopt`.
#[derive(Args, Debug)]
pub struct AdoptArgs {
    /// The app repository (default: the current directory).
    pub path: Option<PathBuf>,

    /// Only these platforms (ios, android, web; comma separated); default every one detected.
    #[arg(long, value_name = "LIST")]
    pub platform: Option<String>,

    /// Name of the core (default: the repository directory's name).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Use the crates and runtimes of this checkout of the Undra repository.
    #[arg(long, value_name = "DIR", env = "UNDRA_PATH")]
    pub undra_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn init_defaults_and_aliases() {
        let cli = Cli::try_parse_from(["undra", "init", "todo"]).unwrap();
        let Command::Init(args) = cli.command else {
            panic!("not init")
        };
        assert_eq!(args.name, "todo");
        assert_eq!(args.platforms, "ios,android,web");

        let cli = Cli::try_parse_from([
            "undra",
            "init",
            "todo",
            "--targets",
            "web",
            "--undra-path",
            "/k",
        ])
        .unwrap();
        let Command::Init(args) = cli.command else {
            panic!("not init")
        };
        assert_eq!(args.platforms, "web");
        assert_eq!(args.undra_path, Some(PathBuf::from("/k")));
    }

    #[test]
    fn build_accepts_the_spec_flags() {
        let cli =
            Cli::try_parse_from(["undra", "build", "--platform", "ios,web", "--release"]).unwrap();
        let Command::Build(args) = cli.command else {
            panic!("not build")
        };
        assert_eq!(args.platform.as_deref(), Some("ios,web"));
        assert!(args.release);
    }

    #[test]
    fn build_takes_an_xcode_configuration_instead_of_release() {
        let cli = Cli::try_parse_from([
            "undra",
            "build",
            "--platform",
            "ios",
            "--configuration",
            "Release",
        ])
        .unwrap();
        let Command::Build(args) = cli.command else {
            panic!("not build")
        };
        assert_eq!(args.configuration.as_deref(), Some("Release"));
        assert!(!args.release);
        // The two say the same thing in different words: only one of them.
        assert!(
            Cli::try_parse_from(["undra", "build", "--release", "--configuration", "Debug"])
                .is_err()
        );
    }

    #[test]
    fn doctor_takes_fix_or_json_but_not_both() {
        let cli = Cli::try_parse_from(["undra", "doctor", "--fix"]).unwrap();
        let Command::Doctor(args) = cli.command else {
            panic!("not doctor")
        };
        assert!(args.fix && !args.json);
        let cli = Cli::try_parse_from(["undra", "doctor", "--json", "--platform", "web"]).unwrap();
        let Command::Doctor(args) = cli.command else {
            panic!("not doctor")
        };
        assert!(args.json && !args.fix);
        assert_eq!(args.platform.as_deref(), Some("web"));
        assert!(Cli::try_parse_from(["undra", "doctor", "--fix", "--json"]).is_err());
    }

    #[test]
    fn build_can_skip_the_symbol_files() {
        let cli = Cli::try_parse_from(["undra", "build", "--release", "--no-symbols"]).unwrap();
        let Command::Build(args) = cli.command else {
            panic!("not build")
        };
        assert!(args.release && args.no_symbols);
        let cli = Cli::try_parse_from(["undra", "build", "--release"]).unwrap();
        let Command::Build(args) = cli.command else {
            panic!("not build")
        };
        assert!(!args.no_symbols);
    }

    #[test]
    fn symbolicate_takes_a_report_or_addresses() {
        let cli = Cli::try_parse_from([
            "undra",
            "symbolicate",
            "report.json",
            "0x10",
            "--platform",
            "android",
            "--image-id",
            "ab12",
            "--symbols",
            "/s",
            "--dsym",
            "App.dSYM",
        ])
        .unwrap();
        let Command::Symbolicate(args) = cli.command else {
            panic!("not symbolicate")
        };
        assert_eq!(args.inputs, ["report.json", "0x10"]);
        assert_eq!(args.platform.as_deref(), Some("android"));
        assert_eq!(args.image_id.as_deref(), Some("ab12"));
        assert_eq!(args.symbols, Some(PathBuf::from("/s")));
        assert_eq!(args.dsym, Some(PathBuf::from("App.dSYM")));
    }

    #[test]
    fn dev_defaults_to_loopback() {
        let cli = Cli::try_parse_from(["undra", "dev"]).unwrap();
        let Command::Dev(args) = cli.command else {
            panic!("not dev")
        };
        assert_eq!(args.addr, "127.0.0.1:7443");
        assert!(!args.no_watch);
        assert_eq!(args.log_level, 1);
        assert!(!args.android);
    }

    #[test]
    fn dev_android_asks_for_adb_reverse() {
        let cli =
            Cli::try_parse_from(["undra", "dev", "--android", "--addr", "127.0.0.1:0"]).unwrap();
        let Command::Dev(args) = cli.command else {
            panic!("not dev")
        };
        assert!(args.android);
    }

    #[test]
    fn schema_diff_takes_two_files_or_a_ref_and_a_gate() {
        let cli = Cli::try_parse_from(["undra", "schema", "diff", "old.json", "new.json"]).unwrap();
        let Command::Schema(SchemaArgs {
            command: SchemaCommand::Diff(args),
        }) = cli.command
        else {
            panic!("not schema diff")
        };
        assert_eq!(
            args.files,
            [PathBuf::from("old.json"), PathBuf::from("new.json")]
        );
        assert!(args.against.is_none() && !args.exit_code);

        let cli = Cli::try_parse_from([
            "undra",
            "schema",
            "diff",
            "--against",
            "origin/main",
            "--exit-code",
            "api/schema.json",
        ])
        .unwrap();
        let Command::Schema(SchemaArgs {
            command: SchemaCommand::Diff(args),
        }) = cli.command
        else {
            panic!("not schema diff")
        };
        assert_eq!(args.against.as_deref(), Some("origin/main"));
        assert!(args.exit_code);
        assert_eq!(args.files, [PathBuf::from("api/schema.json")]);
        // With nothing but a ref it is the default file: no positional is needed.
        assert!(Cli::try_parse_from(["undra", "schema", "diff", "--against", "HEAD"]).is_ok());
    }

    #[test]
    fn schema_export_writes_to_a_file_or_standard_output() {
        let cli = Cli::try_parse_from(["undra", "schema", "export", "-o", "schema.json", "--docs"])
            .unwrap();
        let Command::Schema(SchemaArgs {
            command: SchemaCommand::Export(args),
        }) = cli.command
        else {
            panic!("not schema export")
        };
        assert_eq!(args.output, Some(PathBuf::from("schema.json")));
        assert!(args.docs && !args.release && !args.check);
        let cli = Cli::try_parse_from(["undra", "schema", "export", "--check"]).unwrap();
        let Command::Schema(SchemaArgs {
            command: SchemaCommand::Export(args),
        }) = cli.command
        else {
            panic!("not schema export")
        };
        assert!(args.check && args.output.is_none());
        assert!(
            Cli::try_parse_from(["undra", "schema"]).is_err(),
            "a subcommand is needed"
        );
    }

    #[test]
    fn the_schema_subcommands_teach_too() {
        let command = Cli::command();
        let schema = command
            .find_subcommand("schema")
            .expect("schema is a command");
        for sub in schema.get_subcommands() {
            assert!(
                sub.get_long_about()
                    .is_some_and(|t| t.to_string().len() > 120),
                "`undra schema {}` needs a real --help text",
                sub.get_name()
            );
            assert!(
                sub.get_after_long_help()
                    .is_some_and(|h| h.to_string().contains("undra schema ")),
                "`undra schema {}` needs examples",
                sub.get_name()
            );
        }
    }

    #[test]
    fn drift_takes_a_reference_and_others_with_a_schema_ignores_and_a_gate() {
        let cli = Cli::try_parse_from([
            "undra",
            "drift",
            "--schema",
            "s.json",
            "--ignore",
            "Todos.add.title",
            "--ignore",
            "Http.request.req.headers",
            "--exit-code",
            "ios.json",
            "android.json",
            "web.json",
        ])
        .unwrap();
        let Command::Drift(args) = cli.command else {
            panic!("not drift")
        };
        assert_eq!(args.schema, Some(PathBuf::from("s.json")));
        assert_eq!(args.ignore, ["Todos.add.title", "Http.request.req.headers"]);
        assert!(args.exit_code);
        assert_eq!(
            args.files,
            [
                PathBuf::from("ios.json"),
                PathBuf::from("android.json"),
                PathBuf::from("web.json")
            ]
        );
        let cli = Cli::try_parse_from(["undra", "drift", "a.json", "b.json"]).unwrap();
        let Command::Drift(args) = cli.command else {
            panic!("not drift")
        };
        assert!(args.schema.is_none() && args.ignore.is_empty() && !args.exit_code);
        assert!(
            Cli::try_parse_from(["undra", "drift"]).is_err(),
            "a file is needed"
        );
    }

    #[test]
    fn the_project_dir_is_global() {
        let cli = Cli::try_parse_from(["undra", "doctor", "-C", "/p"]).unwrap();
        assert_eq!(cli.project_dir, Some(PathBuf::from("/p")));
    }

    #[test]
    fn unknown_commands_fail() {
        assert!(Cli::try_parse_from(["undra", "frobnicate"]).is_err());
        assert!(Cli::try_parse_from(["undra"]).is_err());
    }

    #[test]
    fn every_command_has_teaching_help() {
        let command = Cli::command();
        for sub in command.get_subcommands() {
            let long = sub
                .get_long_about()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(
                long.len() > 120,
                "`undra {}` needs a real --help text",
                sub.get_name()
            );
            assert!(
                sub.get_after_long_help()
                    .is_some_and(|h| h.to_string().contains("undra ")),
                "`undra {}` needs examples",
                sub.get_name()
            );
        }
    }
}
