# Architect: Bazel-first integration (1.1), 2026-10-05

ADRs: ADR-064 (committed bindings and `undra_bindings_test`), ADR-065 (the Android build drives the NDK directly; the
NDK as a declared input), ADR-061 amendment (Bazel range, ruleset minimums, Android in CI, lint exclusions by choice).

**Boundaries.** Nothing here touches the wire, the ABI, the schema or the runtime model. The CLI gains knowledge
`cargo-ndk` used to hold (NDK toolchain paths per host, the API-level suffix); `undra_core` gains an `ndk` attribute and
loses its dependence on `extra_path` and `--action_env` for Android; `undra_bindings_test` is a macro over
`bazel_lib`'s `write_source_files`; bindgen gains one project setting for the lint exclusion files.

**The Android target under Bazel.** ADR-061's implementation note records that the Android core stopped at analysis
because rules_rust wanted a C++ toolchain for the Android platforms. The build is a Cargo cross-build inside the action,
so the target must not transition to the Android platform at all: it runs in the host configuration with the host Rust
toolchain and the NDK input. That is the fix, not a stub C++ toolchain.

**Equivalence of the two bindings models.** `undra_bindings_test` compares exactly what `undra bindgen --check`
compares. Any normalisation lives in one place (the CLI) and the rule reuses it.

**The recipe's shape.** Ingest on `undra_runtime::spawn_blocking` into an app-owned structure behind its own lock; the
core lock is taken only by the small transaction that publishes the derived window and aggregates. A bench row measures
the lock hold during a 1 MB snapshot apply against a same-job baseline (no absolute machine-speed assertion).

**Failure modes watched.** A ruleset minimum that lacks an API the rules use (declare the lowest working version, say
why); Bazel 9 removing an API (fix or record); the NDK archive download in CI (repository cache, retries); the two
bindings models drifting (the test in the example is the proof).
