# Product: Bazel-first integration (1.1), 2026-10-05

**Who:** a platform team adopting Undra inside a Bazel monorepo that already has its rulesets pinned, lints every file,
commits generated code, and allows only managed toolchains in actions.

**Scope (the spec's table is binding):** Bazel 8.8 and 9.x; ruleset minimums; `undra_bindings_test`; the NDK as a
declared input and no `cargo-ndk` anywhere; Android built in CI; lint exclusions configurable; symbol files and
`undra symbolicate` proven under Bazel; a high-frequency cookbook recipe (a leaderboard, 100,000 rows); the docs that make
the Bazel path findable and the dev loop fast; React Native 0.86 verified; the debugger path from a `rules_apple` app.

**Success:** a team can put `undra_core`, `undra_bindings`, `undra_bindings_test`, `undra_swift_library` and
`undra_android_library` into their graph for host, iOS and Android on day one, with a remote cache hit on an unchanged
core, `undra_bindings_test` green, their linters green, and no environment variable the action depends on except
`DEVELOPER_DIR` for iOS (documented; removed by native mode).

**Out of scope:** native mode; a `crate_universe` hub shared with the app; placement of Undra into any specific domain.
