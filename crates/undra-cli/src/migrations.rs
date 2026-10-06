//! What changed for app authors between releases: the notes `undra upgrade` prints.
//!
//! One [`Migration`] per release that an app author has to know about, newest last. `undra upgrade`
//! prints the notes of every release it crosses (newer than the version the project was on, up to
//! the version of this `undra`), each note marked as something to do ([`Kind::Action`]), something
//! that behaves differently ([`Kind::Changed`]) or something new ([`Kind::New`]).
//!
//! **When a release is cut**, add its entry here (`docs/RELEASING.md`): an upgrade across a release
//! that has no entry prints nothing for it, which tells the author nothing changed.

use crate::semver::Semver;

/// What kind of thing a note is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The author has to do something (rebuild, add a `try`, edit a file).
    Action,
    /// Something behaves differently from before.
    Changed,
    /// Something new that existing projects do not get by themselves.
    New,
}

impl Kind {
    /// The marker printed in front of a note of this kind.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Kind::Action => "do",
            Kind::Changed => "changed",
            Kind::New => "new",
        }
    }
}

/// One thing an app author should know.
#[derive(Clone, Copy, Debug)]
pub struct Note {
    /// What kind of thing it is.
    pub kind: Kind,
    /// The text: one paragraph, wrapped by the printer.
    pub text: &'static str,
}

/// The notes of one release.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The release these notes arrive with (`1.1.0`).
    pub version: &'static str,
    /// A one-line summary.
    pub title: &'static str,
    /// The notes, in the order to read them.
    pub notes: &'static [Note],
}

impl Migration {
    /// The release as a [`Semver`].
    ///
    /// # Panics
    ///
    /// When the table has a malformed version (a test makes sure it has none).
    #[must_use]
    pub fn semver(&self) -> Semver {
        Semver::parse(self.version).expect("the migration table has only valid versions")
    }
}

/// Every release with notes, oldest first.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "1.0.0-rc.1",
        title: "Since v1.0: the UNDR wire, frame-coalesced delivery, typed call errors, reconnecting apps",
        notes: &[
            Note {
                kind: Kind::Action,
                text: "Rebuild the core and every app together. The envelope's four magic bytes are now `UNDR` (55 4E 44 52, ADR-033), where they were the working name's. A core and a runtime from either side of the change refuse each other's frames with a typed bad-magic error. `undra upgrade` moves every pin in step and regenerates the bindings, so one build of each is all it takes.",
            },
            Note {
                kind: Kind::Changed,
                text: "The platform mirrors apply what the core produces once per display frame, merged (ADR-031): a burst of transactions (a firehose of events, a stream, a burst of port completions) shows as one update per frame with its entries folded per signal, and the backlog is bounded (65,536 entries or 16 MiB, after which the store is observed again). A reply, `callSync` and `observe` are never delayed, so after `await store.method()` the mirror shows the change. A screen that needs every intermediate value of a signal sees the latest one per frame. `UndraCore.stats()` counts what was merged, and every runtime has a drain listener (`addDrainListener`).",
            },
            Note {
                kind: Kind::Action,
                text: "Swift: a generated call that returns a value or has an error type now `throws` (untyped, ADR-032): add `try`, and keep catching the method's own error (`catch let error as TodoError`). Two more things can arrive: `CancellationError` when the calling task was cancelled, and the new `UndraCallError` for the failure of the call itself (a panic in the core, a refusal, a core that was shut down, a lost connection, a reply that does not decode). A method that returns nothing and has no error type stays non-throwing: its failure goes to `LoadOptions.onError` and to the log. Generated Swift no longer traps on the outcome of a call.",
            },
            Note {
                kind: Kind::Action,
                text: "Kotlin and TypeScript have the same closed set (ADR-032 amendment A, docs/ERRORS.md): a generated call fails with the method's own error, a cancellation, or `UndraCallError` (`CancelledByCore`, `Panicked`, `Refused`, `Unavailable`, `Malformed`; Kotlin `sealed class UndraCallError : UndraException`, TypeScript `UndraCallError extends UndraError`), never with the runtime's raw `UndraReplyException` / `UndraTransportException` / `UndraReplyError` / `UndraTransportError`. Catch `UndraCallError` where you caught those around a generated call (a `catch (e: UndraException)` or `instanceof UndraError` still matches). A command (a method that returns nothing and has no error type) no longer throws or rejects: its failure goes to the `onError` of `UndraCore.load` and to the log.",
            },
            Note {
                kind: Kind::Action,
                text: "Core code: a write to a store's signal from a thread that does not hold that store's runtime (a `spawn_blocking` worker, a host thread, another runtime) is refused in every build now, release included (ADR-035, error E0065): it panics with the teaching message, or `Signal::try_set` / `try_update` return `WriteError::OffCore`. Send the value to the core instead (return it from `spawn_blocking`, `ctx.spawn`, a call), or write under `ctx.with_core(|| ..)` from a host thread.",
            },
            Note {
                kind: Kind::Changed,
                text: "Core code: a store, task or subscriber that keeps a `Ctx` keeps its runtime alive; keep a `WeakCtx` (`ctx.downgrade()`, `upgrade()` per use) for anything that outlives a call, as the E0001 help says (ADR-034). An event subscriber receives the `Ctx` as its first argument. A computed that panics no longer fails its whole store: it alone is held back and reported once, and the write that made it fail succeeds (the ADR-019 amendment).",
            },
            Note {
                kind: Kind::New,
                text: "A stream can end with its typed error part-way: return `impl Stream<Item = Result<T, E>>` and the platforms deliver the items, then `E` as the generated error type (ADR-036). A stream the core ends itself (a restore, a shutdown, a panicking producer) ends with `UndraCallError` (`CancelledByCore`, `Panicked`), never with the stream's own error.",
            },
            Note {
                kind: Kind::New,
                text: "Apps reconnect to `undra dev` by themselves on all three platforms (ADR-051): backoff with jitter, every observed store observed again after the handshake, `core.connection` (TypeScript), `connectionState` (Kotlin, Swift) for a status bar, and the server keeps a dropped client's objects for ten minutes. A rebuild of the core still starts the app over on the new core; carrying state across a rebuild is planned. The Kotlin runtime has the WebSocket transport now, so Android runs against `undra dev` too (`./gradlew -PundraDevUrl=ws://10.0.2.2:7443 :app:installDebug`, `undra dev --android`).",
            },
            Note {
                kind: Kind::Changed,
                text: "`undra bindgen --docs` keeps the core's doc comments in the generated code: the schema a built core exports is the whole schema now (ADR-050). A core built before that change exports only the canonical form, and `--docs` on it is error C0006: rebuild the core.",
            },
            Note {
                kind: Kind::New,
                text: "A project made by `undra init` builds its core from the app's own build system (a Gradle `undraBuild` task, an Xcode \"Build the Undra core\" phase, the `undra()` Vite plugin) and has a CI workflow (`.github/workflows/undra.yml`). `undra upgrade` does not edit your app's project files: to get these in an older project copy the pieces from a fresh `undra init` (the Xcode project's build phase and `ios/Config/*.xcfilelist`, `android/app/build.gradle.kts`, `web/vite.config.ts`). `undra doctor --fix` prints the commands that close every gap of this machine as one block.",
            },
            Note {
                kind: Kind::New,
                text: "An app can support iOS 15 and 16 (ADR-045, docs/IOS_15_16.md): set `[ios] deployment_target = \"15.0\"` in undra.toml (it must be 15.0 or later) and run `undra bindgen`, and the generated Swift stores and query handles are `ObservableObject`s with `@Published` properties instead of `@Observable` classes (the default from 17.0 on, unchanged), and the wire `Duration` is the runtime's `UndraDuration` below iOS 16. Views observe a store with `@ObservedObject` (`@StateObject` to own it) and the dev status bar reads `core.connectionObject`; `undra init --ios-deployment-target 15.0` writes an app that does. `[bindings] swift_observation` (and `undra bindgen --swift-observation`) overrides the choice, and `observation` below iOS 17.0 is refused. Nothing changes for a project that stays on 17.0.",
            },
        ],
    },
    Migration {
        version: "1.1.0",
        title: "1.1: Bazel-first integration, no cargo-ndk, lint exclusions by choice",
        notes: &[
            Note {
                kind: Kind::Changed,
                text: "`undra build --platform android` links with the NDK's clang itself: `cargo-ndk` is no longer needed, nor checked by `undra doctor`, and the API level is `[android] min_sdk` of undra.toml (26 when it says nothing). The `cargo install cargo-ndk` line of a workflow `undra init` wrote (`.github/workflows/undra.yml`) can be deleted; leaving it costs a few minutes of CI and nothing else. `undra doctor --json` has no `android.cargo-ndk` finding any more: a tool that reads the JSON should drop that id.",
            },
            Note {
                kind: Kind::Changed,
                text: "For the build scripts of crates that wrap C libraries, `undra build --platform android` exports what `cargo-ndk` did, per ABI: `BINDGEN_EXTRA_CLANG_ARGS_<triple>` (the NDK's sysroot, `--target=<triple><min_sdk>` and the ABI's include directory, so `bindgen` finds `stdio.h`), `CLANG_PATH` (the NDK's clang), `ANDROID_ABI` and `ANDROID_PLATFORM`, and `CMAKE_TOOLCHAIN_FILE_<triple>`, a toolchain file in the target directory that sets the ABI and API level and includes the NDK's `android.toolchain.cmake` (the `cmake` crate reads it). A value you export yourself is kept. `CFLAGS_<triple>` and `CARGO_NDK_*` are not set: the compiler is the NDK's wrapper of the API level, which knows its target.",
            },
            Note {
                kind: Kind::New,
                text: "`[bindings] lint_exclusions = \"beside\" | \"none\"` in undra.toml: `beside` (the default) writes the lint exclusion files next to the generated trees as before; `none` writes none, for a repository that configures its linters in one place (the Bazel guide lists the root-config lines).",
            },
            Note {
                kind: Kind::New,
                text: "Bazel: `undra_bindings_test` guards committed bindings; `undra_core(ndk = <label>)` makes the NDK a declared input, and `extra_path` reaches the iOS core alone (an `extra_path` that named `cargo-ndk`'s directory for an Android core is ignored: that core needs `ndk`); `undra_core(keep_debug_objects = True)` keeps the iOS archives a debug map names; the `symbols` output group now holds the host library's debug symbols too, under `<target>.symbols/symbols/` and `host/` (a path that read the old flat layout must change). The rules support Bazel 8.8 and 9.x.",
            },
        ],
    },
];

/// The migrations a project crosses going from `from` (the version it is on; `None` when that is
/// not known) to `to`: every release newer than `from` and not newer than `to`, oldest first.
#[must_use]
pub fn between(from: Option<&Semver>, to: &Semver) -> Vec<&'static Migration> {
    MIGRATIONS
        .iter()
        .filter(|m| {
            let v = m.semver();
            v <= *to && from.is_none_or(|from| v > *from)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Semver {
        Semver::parse(text).unwrap()
    }

    #[test]
    fn the_table_is_valid_sorted_and_not_ahead_of_this_release() {
        let mut previous: Option<Semver> = None;
        for m in MIGRATIONS {
            let this = m.semver();
            assert!(
                previous.as_ref().is_none_or(|p| *p < this),
                "{} is out of order",
                m.version
            );
            assert!(!m.notes.is_empty() && !m.title.is_empty(), "{}", m.version);
            assert!(
                this <= v(env!("CARGO_PKG_VERSION")),
                "{} has notes for a release newer than this undra ({})",
                m.version,
                env!("CARGO_PKG_VERSION")
            );
            previous = Some(this);
        }
    }

    #[test]
    fn it_starts_with_the_notes_since_v1_0() {
        // Keyed to the workspace version until the first release is cut; scripts/bump-version.sh files it under the
        // release (the outgoing version was never released), so this checks the entry, not its key.
        let first = &MIGRATIONS[0];
        assert!(first.title.starts_with("Since v1.0"), "{}", first.title);
        assert!(first.semver() <= v(env!("CARGO_PKG_VERSION")));
        // What changed since v1.0, for app authors: the wire, the mirrors, the error channel, reconnecting.
        let text: String = first
            .notes
            .iter()
            .map(|n| n.text)
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            "UNDR",
            "55 4E 44 52",
            "ADR-033",
            "ADR-031",
            "once per display frame",
            "ADR-032",
            "UndraCallError",
            "amendment A",
            "Kotlin",
            "TypeScript",
            "onError",
            "ADR-051",
            "reconnect",
            "undra upgrade",
        ] {
            assert!(
                text.contains(needle),
                "the first entry does not mention {needle}"
            );
        }
        assert!(
            first.notes.iter().any(|n| n.kind == Kind::Action),
            "it says what to do"
        );
    }

    #[test]
    fn a_project_gets_the_notes_of_the_releases_it_crosses() {
        let versions = |from: Option<&str>, to: &str| -> Vec<&'static str> {
            between(from.map(v).as_ref(), &v(to))
                .iter()
                .map(|m| m.version)
                .collect()
        };
        // The first notes' release moves when a release is cut (scripts/bump-version.sh files notes kept under a version
        // that was never released under the new one), so it is read from the table, not written here.
        let first = MIGRATIONS[0].version;
        let this = env!("CARGO_PKG_VERSION");
        assert_eq!(versions(Some("0.0.9"), first), [first]);
        assert_eq!(versions(Some("0.0.9"), this)[0], first);
        assert_eq!(
            versions(Some(first), first),
            Vec::<&str>::new(),
            "already past it"
        );
        assert_eq!(
            versions(Some("0.0.1"), "0.0.9"),
            Vec::<&str>::new(),
            "not there yet"
        );
        assert_eq!(
            versions(None, first),
            [first],
            "a project of unknown version gets everything up to the target"
        );
    }

    #[test]
    fn markers_are_distinct_words() {
        let markers = [Kind::Action, Kind::Changed, Kind::New].map(Kind::marker);
        assert_eq!(markers, ["do", "changed", "new"]);
    }
}
