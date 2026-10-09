# DevOps - index

Local toolchain on Shrey's Mac (installed 2026-09-30): rustup stable 1.98.1 with
aarch64-apple-ios(-sim), aarch64/x86_64-linux-android, wasm32-unknown-unknown; JDK 17
(brew, keg-only), Kotlin 2.4.20, Gradle 9.8, binaryen 133; kotlinx-coroutines-core-jvm
1.6.4 at ../.tools/lib. Full Xcode.app present; xcode-select needs sudo so env.sh routes
via DEVELOPER_DIR. Missing until the playground step: Android commandlinetools + NDK r27
+ AVD, cargo-ndk. CI workflows: none yet (piece 7).

* [dist.md](dist.md) (2026-09-30): the release workflow, the npm / Homebrew / curl / cargo
  channels, `scripts/bump-version.sh`, the `undra --version` and `undra init` pinning changes;
  what is verified locally and what only the first dry run can show. Runbook:
  `docs/RELEASING.md`.
- `pr-gate.md` - the merge gate as GitHub enforces it: the "All green" roll-up per workflow, the four required checks, Site on every pull request, `wt.sh merge` through a pull request (2026-10-02).
- `bazel-first.md` - 1.1 CI additions (Bazel range, ruleset minimums, Android in CI, the symbolicate and bindings tests), their cost, and that the release process is unchanged (2026-10-05).
- `ci-wall-time.md` - the Gate's critical path was the macOS runner pool and four serial jobs; each is two jobs side by side (Bazel recent and minimums on Linux and macOS, Rust workspace and CLI, Swift runtime and Xcode projects, Two cores Debug and Release), the Bazel jobs over a disk cache, macOS jobs queued first; every job still runs on every change (2026-10-09).
