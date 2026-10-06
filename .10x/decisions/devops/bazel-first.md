# DevOps: Bazel-first integration (1.1), 2026-10-05

**CI additions.** The Bazel example runs on Bazel 8.8.1 and on 9.x (`USE_BAZEL_VERSION` for the second run) and once at
the rules' minimum ruleset versions; a Linux job builds the example's Android core and `undra_android_library` with the
SDK and the NDK (`scripts/android-sdk-install.sh`, repository cache, the existing `dl.google.com` retry); the symbolicate
test and `undra_bindings_test` run in the example's `bazel test //...`. Every job stays inside the Gate.

**Cost.** The Gate grows by one Linux job and one more Bazel run; the NDK archive is cached between runs.

**Release.** 1.1.0 follows `docs/RELEASING.md` unchanged: `scripts/bump-version.sh 1.1.0-rc.1` (it already sets the
Bazel module versions), the Release and Release smoke workflows on the candidate, then `1.1.0`. The Release smoke
workflow gains nothing; the Bazel path is proven by the example's CI jobs on every change.

**Rollback.** Each piece is a squash commit on `main`; a revert is one pull request. The rules are versioned with the
repository, so an adopter pins the tag that works for them.
