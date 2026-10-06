//! `undra build`: the core, built for each platform and packaged where the app shells look for it.
//!
//! | Target | Result below `build/` | How |
//! |---|---|---|
//! | `host` | `host/lib<ns>.{dylib,so}` | the shim as a cdylib, with the JNI shim (Kotlin on the JVM); on macOS its install name is `@rpath/lib<ns>.dylib`, not a path into `target/` |
//! | `ios` | `ios/<Ns>Core.xcframework` | the shim as a staticlib for device and simulator, each slice prelinked into one object (`lib<ns>.a`), with `<ns>_undra.h`, `xcodebuild -create-xcframework` |
//! | `android` | `android/jniLibs/<abi>/lib<ns>.so` | Cargo cross-build per ABI, linked with the NDK's clang (ADR-065), 16 KB page aligned; a release build strips the shipped copy |
//! | `web` | `web/<ns>.wasm` | the wasm profile of SPEC 7, then `wasm-opt -Oz` when present; the shipped module has no names or DWARF |
//! | `rn` | `ios/<Ns>Core.xcframework` + `ios/<Ns>Core.podspec`, `android/jniLibs/` | the iOS and Android builds and the pod React Native apps link (ADR-038) |
//!
//! `<ns>` is the core's namespace (`[core] namespace`, ADR-044) and `<Ns>Core` its `CoreNames::bundle`.
//!
//! A release build also writes the **symbol files** that resolve a crash report's addresses to
//! `file:line` (`symbols/`, ADR-046; `--no-symbols` skips them): see [`crate::symbols`].
//! Every target prints the size of what it made, next to the budget of the blueprint.

pub(crate) mod android;
pub(crate) mod gradle;
pub(crate) mod host;
pub(crate) mod ios;
pub(crate) mod ndk;
pub(crate) mod rn;
pub(crate) mod web;
pub(crate) mod xcode;

use std::path::PathBuf;

use crate::config::Platform;
use crate::error::{CliError, Code, Result};
use crate::fsutil::human_size;
use crate::session::Session;
use crate::symbols::Symbols;
use crate::ui::table;

/// Something `undra build` can build for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// This machine: a cdylib for `undra bindgen`, the JVM and tests.
    Host,
    /// iOS.
    Ios,
    /// Android.
    Android,
    /// The web.
    Web,
    /// React Native: the iOS and Android cores and the pod that vendors the iOS one (ADR-038).
    ReactNative,
}

impl Target {
    /// The name on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Target::Host => "host",
            Target::Ios => "ios",
            Target::Android => "android",
            Target::Web => "web",
            Target::ReactNative => "rn",
        }
    }

    /// Parses a target name.
    ///
    /// # Errors
    ///
    /// `C0009` listing the valid names.
    pub fn parse(text: &str) -> Result<Target> {
        match text.trim().to_ascii_lowercase().as_str() {
            "host" => Ok(Target::Host),
            "ios" => Ok(Target::Ios),
            "android" => Ok(Target::Android),
            "web" | "wasm" => Ok(Target::Web),
            "rn" | "react-native" => Ok(Target::ReactNative),
            other => Err(CliError::bad_argument(
                format!("`{other}` is not a build target"),
                "a target decides which toolchain builds the core and what is produced",
                "use one of: host, ios, android, web, rn",
            )),
        }
    }

    /// The target of a platform.
    #[must_use]
    pub fn of(platform: Platform) -> Target {
        match platform {
            Platform::Ios => Target::Ios,
            Platform::Android => Target::Android,
            Platform::Web => Target::Web,
        }
    }
}

/// A file or directory a build produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// What it is, for the summary (`android arm64-v8a`).
    pub label: String,
    /// Where it is.
    pub path: PathBuf,
    /// Its size in bytes (a directory's is the sum of its files).
    pub size: u64,
    /// The blueprint budget this size is compared with, in words, when there is one.
    pub budget: Option<String>,
    /// More to say about it (`gzip 88.1 KB`).
    pub note: Option<String>,
}

/// What to build.
#[derive(Clone, Debug)]
pub struct Options {
    /// The targets, in order.
    pub targets: Vec<Target>,
    /// Release builds (LTO, optimized) instead of debug.
    pub release: bool,
    /// Write the symbol files of a release build (`false` for `--no-symbols`): the unstripped
    /// twins, the web debug modules and the manifest (ADR-046).
    pub symbols: bool,
}

/// Builds every target of `options`, then prints what was made and how big it is.
///
/// # Errors
///
/// The first failure; targets already built stay built.
pub fn run(session: &Session<'_>, options: &Options) -> Result<Vec<Artifact>> {
    let mut all = Vec::new();
    let symbols = Symbols::new(session, options.symbols, options.release);
    for target in &options.targets {
        session.ui.step(&format!(
            "Building the core for {} ({})",
            target.name(),
            if *target == Target::Web || options.release {
                "release"
            } else {
                "debug"
            }
        ));
        let produced = match target {
            Target::Host => host::package(session, options.release, &symbols)?,
            Target::Ios => ios::build(session, options.release, &symbols)?,
            Target::Android => android::build(session, options.release, &symbols)?,
            Target::Web => web::build(session, &symbols)?,
            Target::ReactNative => rn::build(session, options.release, &symbols)?,
        };
        all.extend(produced);
    }
    Ok(all)
}

/// Advice after a build, one line each: a debug Android core is tens of megabytes per ABI, and
/// `--release` is what gets packaged.
#[must_use]
pub fn hints(session: &Session<'_>, options: &Options, artifacts: &[Artifact]) -> Vec<String> {
    let mut out = Vec::new();
    if !options.release
        && (options.targets.contains(&Target::Android)
            || options.targets.contains(&Target::ReactNative))
    {
        out.extend(android::hint(session, artifacts));
        out.extend(android::debugger_hint(session));
    }
    out
}

/// The summary table of a build: one row per artifact, with its budget where the blueprint has one.
#[must_use]
pub fn summary(project_root: &std::path::Path, artifacts: &[Artifact]) -> String {
    let mut rows = vec![vec![
        "artifact".to_owned(),
        "size".to_owned(),
        "where".to_owned(),
    ]];
    for a in artifacts {
        let mut size = human_size(a.size);
        if let Some(note) = &a.note {
            size.push_str(&format!(" ({note})"));
        }
        if let Some(budget) = &a.budget {
            size.push_str(&format!("  [budget {budget}]"));
        }
        let shown = a.path.strip_prefix(project_root).unwrap_or(&a.path);
        rows.push(vec![a.label.clone(), size, shown.display().to_string()]);
    }
    table(&rows)
}

/// `C0012`: the platform cannot be built on this machine.
pub(crate) fn unsupported(platform: &str, why: &str, fix: &str) -> CliError {
    CliError::new(
        Code::Unsupported,
        format!("{platform} cannot be built on this machine"),
        why,
        fix,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse_and_map_from_platforms() {
        assert_eq!(Target::parse("Host").unwrap(), Target::Host);
        assert_eq!(Target::parse("wasm").unwrap(), Target::Web);
        assert_eq!(Target::parse("rn").unwrap(), Target::ReactNative);
        assert_eq!(Target::parse("React-Native").unwrap(), Target::ReactNative);
        assert_eq!(Target::ReactNative.name(), "rn");
        assert!(
            Target::parse("tv")
                .unwrap_err()
                .fix
                .contains("host, ios, android, web, rn")
        );
        assert_eq!(Target::of(Platform::Android), Target::Android);
    }

    #[test]
    fn the_summary_shows_sizes_notes_and_budgets() {
        let a = Artifact {
            label: "web".into(),
            path: PathBuf::from("/p/build/web/todo_core.wasm"),
            size: 241_000,
            budget: Some("120 KB gzip".into()),
            note: Some("gzip 88.1 KB".into()),
        };
        let text = summary(std::path::Path::new("/p"), &[a]);
        assert!(
            text.contains("241.0 KB (gzip 88.1 KB)  [budget 120 KB gzip]"),
            "{text}"
        );
        assert!(text.contains("build/web/todo_core.wasm"), "{text}");
    }
}
