# ADR-065: The Android build drives the NDK directly, and the NDK is a declared input under Bazel

Status: **Accepted** (2026-10-05). Part of the 1.1 Bazel-first design (`.10x/specs/2026-10-05-bazel-first-design.md`).
Changes how `undra build --platform android` invokes Cargo and what `undra_core` takes as inputs; the produced libraries
(`jniLibs/<abi>/lib<ns>.so`, 16 KB page alignment, GNU build id, symbol files) are unchanged.

## Context

`undra build --platform android` has called `cargo ndk`, a Cargo subcommand that points Cargo at the NDK's clang and
linker for each target triple. It works, and it costs every Android developer one more tool (`cargo install cargo-ndk`,
checked by `undra doctor` as C0003). Under Bazel (ADR-061) the Android build therefore reached outside the action for
two things: `cargo-ndk` on the machine's `PATH` (through `extra_path`) and the NDK through `ANDROID_NDK_HOME`
(through `--action_env`). Neither is a declared input, so the action is not reproducible by Bazel's definition and a
remote cache cannot trust it. The guide said so ("the Apple and Android toolchains are the machine's").

What `cargo ndk` does is small and well understood: set the linker, the C compiler and the archiver for a triple to the
NDK's LLVM prebuilts for a given API level, and pass the flags the libraries need. Rust 1.68 and later handle the libgcc
and libunwind detail that once made the tool necessary.

## Decision

1. **The CLI sets up the NDK itself.** For each 64-bit ABI (`arm64-v8a`, `x86_64`) `undra build` runs plain `cargo build
   --target <triple>` with the environment Cargo reads: `CARGO_TARGET_<TRIPLE>_LINKER` pointing at the NDK's
   `<triple><api>-clang`, `CC_<triple>` and `AR_<triple>` at the same toolchain's clang and `llvm-ar`, and the existing
   link arguments (16 KB `max-page-size`, the GNU build id). The API level is the project's `minSdk`. The output layout,
   the alignment check and the build-id check are unchanged.
2. **`cargo-ndk` is no longer required.** `undra doctor` stops checking for it; the NDK check (C0003) stays and names the
   two places the NDK is looked for: `ANDROID_NDK_HOME`, then `ANDROID_HOME/ndk/<version>`.
3. **Under Bazel the NDK is a label.** `undra_core` gains `ndk`, a label naming the NDK's root directory as a declared
   input (a repository rule's tree: an `http_archive` of the NDK pinned by checksum, or `rules_android_ndk`'s
   repository). The action sets `ANDROID_NDK_HOME` to that input's path itself; `extra_path` and `--action_env` are not
   involved for Android any more. The example shows the archive form.
4. **CI builds Android.** A Linux job builds the example's Android core and `undra_android_library` with the SDK and the
   NDK, so a break is found here first.
5. **iOS is the documented exception.** Xcode cannot be a label; `DEVELOPER_DIR` through `--action_env` stays until native
   mode (a later ADR) builds the Apple slices with Bazel's own toolchain.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep `cargo-ndk`, build it from source inside the rules like the CLI | Keeps a tool nobody needs for a job the CLI can do in a dozen environment variables |
| Native mode now (`rust_shared_library` through a Bazel NDK toolchain) | The right end state; weeks of work and a second build pipeline to keep byte-identical with the CLI's. Scheduled as its own ADR |
| Document `--action_env` and leave it | Fails the managed-toolchain policies common in monorepos; the Android action stays uncacheable |

## Consequences

* Positive: one tool fewer for every Android developer; the Bazel Android action has only declared inputs and is
  cacheable; Android is built in CI for the first time.
* Negative: the CLI owns a few lines of NDK knowledge (toolchain paths per host OS, the API-level suffix) that `cargo-ndk`
  used to own. They are covered by the build-systems tests and `undra doctor`'s C0003 message.
* Risk: the NDK archive is around 600 MB; CI uses the repository cache and the existing retry on `dl.google.com`.

Amendment (2026-10-06): the first cut set only what Cargo links with (the linker, `CC`/`CXX`, `AR`/`RANLIB`,
`ANDROID_NDK_HOME`), so a dependency whose build script runs `bindgen` over C headers failed with `'stdio.h' file not
found` where 1.0 built through `cargo-ndk`. The build now also exports, per ABI, what `cargo-ndk` gave build scripts:
`BINDGEN_EXTRA_CLANG_ARGS_<triple>` (`--sysroot=<sysroot> --target=<triple><api> -I<sysroot>/usr/include/<triple>`),
`CLANG_PATH` (the NDK's clang), `ANDROID_ABI`, `ANDROID_PLATFORM`, and `CMAKE_TOOLCHAIN_FILE_<triple>` naming a file in the
target directory that sets `ANDROID_ABI` and `ANDROID_PLATFORM` and includes the NDK's `android.toolchain.cmake` (the NDK's
file reads both only as CMake variables, so pointing the `cmake` crate at it alone builds `armeabi-v7a`). A value the user
exported for any of these, or a name its reader takes first or instead, is kept. `CFLAGS_<triple>` and `CARGO_NDK_*` are
not set: the C compiler is the NDK's wrapper of the API level, which carries its target. Proven by a scratch dependency
using `cc`, `cmake` and `bindgen` over `<stdio.h>` and `<android/log.h>`, built debug and release for both ABIs.
