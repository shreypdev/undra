# SDE: xcodeproj-debug: debugging the Rust core from a Bazel-built iOS app (ADR-061 follow-up), 2026-10-05

Branch `wt/xcodeproj-debug`, from `main` `58e373a`. Goal: a `rules_apple` `ios_application` in `examples/bazel`, its Xcode project from
`rules_xcodeproj`, run on the iOS simulator, with a breakpoint hit in a Rust frame of the core and the Swift frames above it; then the
verified steps in the Bazel guide.

## Result

Hit. `breakpoint set --name hello_core::greeting` stops at `lib.rs:145:5` in the app built by `xcodebuild` from the generated project (a
build through Bazel), with `__undra_dispatch_fn_greeting` (lib.rs:144) under it, the core's entry `undra_ffi::native::undra_call_sync`
(native.rs:321, frame 29), the Swift runtime's `InprocTransport.callSync` (InprocTransport.swift:193), the generated
`greeting(name:ctx:)` (Objects.swift:11) and `HelloApp.start()` (HelloApp.swift:23:24, frame 47). Verified on the iOS simulator only
(iPhone 17, iOS 26.5): a physical device was not tried, and Xcode's own editor and debugger UI was not driven (LLDB from the command
line, attached to the process `simctl launch --wait-for-debugger` started).

Versions: Bazel 8.8.1, Xcode 26.6 (17F113), Rust 1.99.0, `rules_apple` 5.2.0, `rules_swift` 4.1.2, `apple_support` 2.8.4, and the new
`rules_xcodeproj` 4.1.0 (BCR; it asks for `rules_apple` 4.4.0 and `rules_swift` 3.5.0, so the example's higher ones win; no version the
example resolved before moved, and it adds six modules of its own: `rsync`, `openssl`, `lz4`, `xxhash`, `zstd`, `rules_perl`, `bazel mod
graph` on main and on the branch, reviewer's correction; the lock also gained extension entries that 8.8.1 recorded).

## What was added

| Where | What |
|---|---|
| `examples/bazel/ios/` | `HelloApp.swift` (SwiftUI: loads the core on launch, calls `greeting`), `Info.plist`, `BUILD.bazel` (`hello` = `undra_swift_library` compiled for iOS, `core_simulator`/`core_device` genrules that take one slice out of the XCFramework directory, `core` = `cc_import` selecting by the simulator constraint, `app_lib`, `app` = `ios_application`, `xcodeproj`, `dbg` config_setting, `app_dsym`, `symbols_test`), `symbols_test.sh`, `.gitignore` (the generated `HelloApp.xcodeproj`) |
| `examples/bazel/MODULE.bazel` | `bazel_dep(rules_xcodeproj 4.1.0)` in a section of its own at the end; the lock as 8.8.1 wrote it |
| `examples/bazel/BUILD.bazel` | one attribute on `//:mobile`: `keep_debug_objects = select({":dbg": True, default False})`, and the `dbg` config_setting (`-c dbg`) |
| `bazel/undra/core.bzl`, `run.sh` | `undra_core(keep_debug_objects)` (below); the iOS core target is compatible with an iOS target platform too |
| `bazel/undra/bindings.bzl` | `core` of `undra_bindings` is built for the execution platform (`cfg = "exec"`) |
| `site/docs/bazel.html`, `examples/bazel/README.md` | "Debugging the core from a Bazel-built app" |

All the new targets are `manual` and `requires-darwin`. `bazel test //...` on the example (macOS): 6 of 6 tests pass, same as before; the
iOS targets are not part of it (CI on Linux was not run; the new `load("@rules_xcodeproj//...")` in `//ios` is loaded by `//...` there
too, which could not be tried here).

## What did not work, in order, and the three changes it needed

1. **`//:mobile_ios` is incompatible with an iOS target platform** ("didn't satisfy constraint @platforms//os:osx"): `undra_core` set
   `target_compatible_with = macos` for `ios`, which is right for building it from a Mac and wrong for an `ios_application`, whose
   dependencies are configured for iOS (the rule transitions to the Apple slices itself). Now `select(macos, ios -> [], else
   incompatible)`, as `undra_swift_library` has it.
2. **`undra_bindings` under an iOS configuration** built `core_host` for the iOS platform (a `.so` where `undra build` writes a dylib).
   The bindgen action loads that library on the machine that runs it, so `core` is now `cfg = "exec"`. Cost: on a Mac the host core is
   built once more, in the exec configuration; `bazel test //...` is 36 s warm.
3. **The Rust frames did not resolve.** With a plain `-c fastbuild` build the app has no debug information at all (the link strips it;
   `nm -pa` shows no `OSO`). With `-c dbg --apple_generate_dsym` the app's debug map lists the Swift objects (`bazel-out/..`) and
   the Rust ones as `/tmp/undra-bazel-<key>/target/aarch64-apple-ios-sim/debug/libundra_core_<hash>.a(..rcgu.o)`, the 11 MB dSYM has
   Swift only, and LLDB says `Breakpoint 1: no locations (pending)` (a `continue` then runs the app to its end). The cause is ADR-044
   and the header of `builds/ios.rs`: a prelinked slice carries a debug map, not DWARF, and the objects "must still be there" when
   the app is linked; `run.sh` removes the action's directory. Keeping the directory by hand (a temporary env knob) made the dSYM 50 MB
   with `lib.rs` line tables and the breakpoint resolve: that is the proof of the cause.
   Change: `undra_core(keep_debug_objects = True)`; `run.sh` leaves only `target/*/(debug|release*)/libundra_core_*.a` (156 MB of the
   1.2 GB directory) and the owner file of the dead process, so the next build of the target replaces them. The directory is named by
   the target and the attribute (`stage_key` gets `|keep_debug_objects`), so a build without it (another configuration, the Xcode
   project's own output base) cannot remove them: that was found, not designed: `bazel test //...` and a keep=False build wiped the
   objects of the app that was being debugged, and LLDB then showed `hello_core::greeting` with no file, line or arguments.
   Test: `//ios:symbols_test` (manual; `--ios_multi_cpus=sim_arm64 -c dbg --apple_generate_dsym`): the dSYM has a line table for
   `lib.rs` and names `greeting` there. It passes with the attribute and FAILS with it off (run both ways).
   Not solved: the kept objects are not an output, so a core restored from a cache after the directory is gone has none. The design
   alternatives that would make them an output (declared object files, `-oso_prefix`) cannot give the final link a path it can use,
   because the prelink runs inside the CLI and the OSO paths must be execution-root relative: a follow-up if the cache case matters.

## Commands (all from `examples/bazel`, `source scripts/env.sh` first)

```
bazel run //ios:xcodeproj          # "Updated project at ios/HelloApp.xcodeproj" (rules_xcodeproj builds its generator in its own output base)
xcodebuild -project ios/HelloApp.xcodeproj -scheme app -configuration Debug \
  -destination 'platform=iOS Simulator,id=<udid>' -derivedDataPath $DD build        # ** BUILD SUCCEEDED **
xcrun simctl install <udid> $DD/Build/Products/Debug-iphonesimulator/bazel-out/*/bin/ios/app.app
xcrun simctl launch --wait-for-debugger <udid> dev.undra.bazel.hello.app            # pid
cd $(bazel info output_base)/rules_xcodeproj.noindex/build_output_base/execroot/_main
xcrun lldb -b -o "process attach -p <pid>" -o "breakpoint set --name hello_core::greeting" -o continue \
  -o "frame select 47" -o "bt -c 60" -o detach
```

The plain-Bazel path (`bazel build //ios:app --ios_multi_cpus=sim_arm64 -c dbg --apple_generate_dsym`, unzip `app.ipa`, install, same
LLDB) hits the same Rust frames (verified from the repository directory); the Swift lines there were not re-checked from its own
execution root.

Trimmed output of the last run: `Breakpoint 1: where = app`hello_core::greeting + 20 at lib.rs:145:5, address = 0x0000000102a6ee14`;
`frame #0: ... app`hello_core::greeting(name=String @ 0x000000016d556b10) at lib.rs:145:5`; `frame #1: ...
__undra_dispatch_fn_greeting(...) at lib.rs:144:8`; `frame #29: ... undra_ffi::native::undra_call_sync(ptr="", len=26) at
native.rs:321:5`; `frame #46: ... greeting(name="Xcode", ctx=0x...) at Objects.swift:11:28`; `frame #47: ... HelloApp.start() at
HelloApp.swift:23:24`.

## Findings that are not fixed here

* **The Rust formatters crash this LLDB.** With the `.lldbinit` of `undra init` sourced (Rust 1.99 `lldb_lookup.py`, Xcode 26.6 LLDB),
  `frame variable name` and `bt 3` work (`name="Xcode"`), `bt 6` and `bt -c 60` end the LLDB process with exit 132 (SIGILL), three of
  three runs; without the formatters the full backtrace is fine. The guide says so. A follow-up for the formatter or the template.
* **LLDB prints Swift frames without file and line after a bare `bt`**; selecting a Swift frame first (`frame select 47`) loads them
  and the backtrace then has them. Documented.
* **Running LLDB from the repository directory** resolves the Rust frames but not Swift lines; the execution root is what Xcode's session
  uses, so the guide says to start there.
* `bazel run //ios:xcodeproj` writes `ios/HelloApp.xcodeproj` into the tree (git-ignored).
* The ADR-061 text still says the iOS core is `target_compatible_with macos` and does not mention `keep_debug_objects`; an amendment
  note is for the integrator.
