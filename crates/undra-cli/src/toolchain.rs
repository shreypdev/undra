//! Finding the toolchains a build needs when the environment does not say where they are.
//!
//! A developer machine rarely has everything on `PATH` the way a CI image does: rustup's
//! `~/.cargo/bin` is missing from a GUI-launched shell, `xcode-select` still points at the
//! command line tools although Xcode is installed, `ANDROID_HOME` was never exported. Instead of
//! failing with a message the developer has to decode, [`Toolchain::detect`] looks in the usual
//! places and hands child processes an environment that works, and `undra doctor` says what it
//! found so nothing is magic.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CliError, Code};
use crate::sys::{Os, Sys};

/// Where Xcode is installed by default.
pub const XCODE_DEVELOPER_DIR: &str = "/Applications/Xcode.app/Contents/Developer";

/// The platform a toolchain note is about, so a build says only what concerns the platform being
/// built: an iOS build has no use for where the Android NDK was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concern {
    /// Xcode and Apple's tools (iOS).
    Apple,
    /// The Android SDK and NDK.
    Android,
}

/// One non-obvious decision of [`Toolchain::detect`], in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// What the decision is about.
    pub concern: Concern,
    /// The sentence.
    pub text: String,
}

/// The environment child processes (cargo, xcodebuild, the NDK's tools) are started with, on top of
/// the CLI's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toolchain {
    /// Directories appended to `PATH` (tools that exist but are not on it).
    pub path_dirs: Vec<PathBuf>,
    /// Variables to set for child processes: only ones the user has not set themselves.
    pub env: Vec<(String, String)>,
    /// One entry per non-obvious decision; a build prints the ones of its own platform
    /// ([`Toolchain::notes_for`]).
    pub notes: Vec<Note>,
    /// The Android SDK directory, if one was found.
    pub android_sdk: Option<PathBuf>,
    /// The Android NDK directory, if one was found.
    pub android_ndk: Option<PathBuf>,
    /// The NDK variable the user set to a path that is not a directory, if they did: then
    /// `android_ndk` is `None` (no other NDK is chosen behind the variable's back) and an Android
    /// build stops with [`UnusableNdk::error`].
    pub android_ndk_unusable: Option<UnusableNdk>,
}

/// The variables that name the Android NDK, in the order they are read: the first one set decides.
pub const NDK_VARIABLES: [&str; 3] = ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"];

/// An NDK variable ([`NDK_VARIABLES`]) the user set to a path that is not a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusableNdk {
    /// The variable, `ANDROID_NDK_HOME` for example.
    pub variable: &'static str,
    /// The path it holds.
    pub path: PathBuf,
}

impl UnusableNdk {
    /// The `C0003` an Android build stops with: the variable chose an NDK that is not there, and
    /// building with another one would hide that.
    #[must_use]
    pub fn error(&self) -> CliError {
        let variable = self.variable;
        CliError::new(
            Code::MissingTool,
            format!(
                "{variable} is {}, which is not a directory",
                self.path.display()
            ),
            format!(
                "{variable} names the Android NDK the core is compiled and linked with; undra does not fall back to another NDK (such as the newest in the SDK's ndk/ directory) that the variable did not choose"
            ),
            format!(
                "point {variable} at an installed NDK, r27 or newer (`sdkmanager \"ndk;27.2.12479018\"` installs one under $ANDROID_HOME/ndk/), or unset it to use the newest NDK of the SDK"
            ),
        )
    }
}

impl Toolchain {
    /// Looks for toolchains in their usual places.
    #[must_use]
    pub fn detect(sys: &dyn Sys) -> Toolchain {
        let mut tc = Toolchain::default();
        tc.detect_cargo_home(sys);
        tc.detect_xcode(sys);
        tc.detect_android(sys);
        tc
    }

    fn detect_cargo_home(&mut self, sys: &dyn Sys) {
        let Some(home) = sys.home() else { return };
        let bin = sys
            .env("CARGO_HOME")
            .map_or_else(|| home.join(".cargo"), PathBuf::from)
            .join("bin");
        if sys.is_dir(&bin) {
            self.path_dirs.push(bin);
        }
    }

    fn detect_xcode(&mut self, sys: &dyn Sys) {
        if sys.os() != Os::Macos || sys.env("DEVELOPER_DIR").is_some() {
            return;
        }
        let xcode = Path::new(XCODE_DEVELOPER_DIR);
        if !sys.is_dir(xcode) {
            return;
        }
        let selected = sys
            .which("xcode-select", &[PathBuf::from("/usr/bin")])
            .and_then(|tool| sys.run(&tool, &["-p"], &[]))
            .map(|out| out.stdout.trim().to_owned())
            .unwrap_or_default();
        if selected.is_empty() || selected.ends_with("CommandLineTools") {
            self.env
                .push(("DEVELOPER_DIR".to_owned(), XCODE_DEVELOPER_DIR.to_owned()));
            self.note(Concern::Apple, format!(
                "xcode-select points at {}, so DEVELOPER_DIR={XCODE_DEVELOPER_DIR} is used for Xcode tools (fix for good: `sudo xcode-select -s {XCODE_DEVELOPER_DIR}`)",
                if selected.is_empty() { "nothing" } else { selected.as_str() }
            ));
        }
    }

    fn detect_android(&mut self, sys: &dyn Sys) {
        self.android_sdk = find_android_sdk(sys);
        if let Some(sdk) = &self.android_sdk {
            if sys.env("ANDROID_HOME").is_none() && sys.env("ANDROID_SDK_ROOT").is_none() {
                self.env.push((
                    "ANDROID_HOME".to_owned(),
                    sdk.to_string_lossy().into_owned(),
                ));
                self.note(
                    Concern::Android,
                    format!(
                        "ANDROID_HOME is not set; using the SDK found at {}",
                        sdk.display()
                    ),
                );
            }
        }
        match find_android_ndk(sys, self.android_sdk.as_deref()) {
            Ok(ndk) => self.android_ndk = ndk,
            Err(unusable) => self.android_ndk_unusable = Some(unusable),
        }
        if let Some(ndk) = &self.android_ndk {
            let set = NDK_VARIABLES.iter().any(|k| sys.env(k).is_some());
            if !set {
                self.env.push((
                    "ANDROID_NDK_HOME".to_owned(),
                    ndk.to_string_lossy().into_owned(),
                ));
                self.note(
                    Concern::Android,
                    format!(
                        "ANDROID_NDK_HOME is not set; using the NDK found at {}",
                        ndk.display()
                    ),
                );
            }
        }
    }

    fn note(&mut self, concern: Concern, text: String) {
        self.notes.push(Note { concern, text });
    }

    /// The notes about `concern`, for the build of that platform to print.
    pub fn notes_for(&self, concern: Concern) -> impl Iterator<Item = &str> {
        self.notes
            .iter()
            .filter(move |n| n.concern == concern)
            .map(|n| n.text.as_str())
    }

    /// Applies the environment to a command about to be run.
    pub fn apply(&self, command: &mut Command) {
        if !self.path_dirs.is_empty() {
            let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default();
            for dir in &self.path_dirs {
                if !paths.contains(dir) {
                    paths.push(dir.clone());
                }
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("PATH", joined);
            }
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
    }

    /// The same environment as `(key, value)` pairs for [`Sys::run`], `PATH` included.
    #[must_use]
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = self.env.clone();
        if !self.path_dirs.is_empty() {
            let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default();
            paths.extend(self.path_dirs.iter().cloned());
            if let Ok(joined) = std::env::join_paths(paths) {
                pairs.push(("PATH".to_owned(), joined.to_string_lossy().into_owned()));
            }
        }
        pairs
    }

    /// Finds `program` on `PATH` or in the extra directories.
    #[must_use]
    pub fn which(&self, sys: &dyn Sys, program: &str) -> Option<PathBuf> {
        sys.which(program, &self.path_dirs)
    }
}

/// The places an Android SDK is usually installed.
fn android_sdk_candidates(sys: &dyn Sys) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(v) = sys.env(key) {
            out.push(PathBuf::from(v));
        }
    }
    if let Some(home) = sys.home() {
        out.push(home.join("Library/Android/sdk"));
        out.push(home.join("Android/Sdk"));
    }
    out.push(PathBuf::from(
        "/opt/homebrew/share/android-commandlinetools",
    ));
    out.push(PathBuf::from("/usr/local/share/android-commandlinetools"));
    out.push(PathBuf::from("/opt/android-sdk"));
    out.push(PathBuf::from("/usr/lib/android-sdk"));
    out
}

/// The Android SDK directory: `ANDROID_HOME` / `ANDROID_SDK_ROOT` if they name a directory,
/// else the first usual location that exists.
#[must_use]
pub fn find_android_sdk(sys: &dyn Sys) -> Option<PathBuf> {
    android_sdk_candidates(sys)
        .into_iter()
        .find(|p| sys.is_dir(p))
}

/// The Android NDK: the directory the first variable of [`NDK_VARIABLES`] that is set names, else
/// the newest `<sdk>/ndk/<version>`, else the legacy `<sdk>/ndk-bundle`.
///
/// # Errors
///
/// [`UnusableNdk`] when that variable is set to a path that is not a directory: the user chose an
/// NDK, and building with another one would hide that it is not there.
pub fn find_android_ndk(sys: &dyn Sys, sdk: Option<&Path>) -> Result<Option<PathBuf>, UnusableNdk> {
    if let Some((variable, value)) = NDK_VARIABLES
        .iter()
        .find_map(|key| sys.env(key).map(|value| (*key, value)))
    {
        let path = PathBuf::from(value);
        return if sys.is_dir(&path) {
            Ok(Some(path))
        } else {
            Err(UnusableNdk { variable, path })
        };
    }
    Ok(sdk.and_then(|sdk| newest_sdk_ndk(sys, sdk)))
}

/// The newest NDK of the SDK at `sdk`: `ndk/<version>`, else the legacy `ndk-bundle`.
#[must_use]
pub fn newest_sdk_ndk(sys: &dyn Sys, sdk: &Path) -> Option<PathBuf> {
    let ndk_dir = sdk.join("ndk");
    let mut versions: Vec<(Vec<u32>, String)> = sys
        .list_dir(&ndk_dir)
        .into_iter()
        .filter(|name| sys.is_dir(&ndk_dir.join(name)))
        .map(|name| (ndk_version(&name), name))
        .collect();
    versions.sort();
    if let Some((_, name)) = versions.last() {
        return Some(ndk_dir.join(name));
    }
    let bundle = sdk.join("ndk-bundle");
    sys.is_dir(&bundle).then_some(bundle)
}

/// `27.2.12479018` to `[27, 2, 12479018]`; non-numeric parts sort first.
#[must_use]
pub fn ndk_version(name: &str) -> Vec<u32> {
    name.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The major version of an NDK directory (its last path component), if it has the usual name.
#[must_use]
pub fn ndk_major(ndk: &Path) -> Option<u32> {
    let name = ndk.file_name()?.to_string_lossy().into_owned();
    let first = name.split('.').next()?;
    first.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeSys;

    #[test]
    fn xcode_is_used_when_xcode_select_points_at_command_line_tools() {
        let sys = FakeSys::macos()
            .with_dir(XCODE_DEVELOPER_DIR)
            .with_tool("xcode-select", "/usr/bin/xcode-select")
            .with_output(
                "xcode-select",
                "-p",
                "/Library/Developer/CommandLineTools\n",
            );
        let tc = Toolchain::detect(&sys);
        assert!(
            tc.env
                .contains(&("DEVELOPER_DIR".into(), XCODE_DEVELOPER_DIR.into())),
            "{tc:?}"
        );
        assert!(
            tc.notes_for(Concern::Apple)
                .any(|n| n.contains("xcode-select -s")),
            "{tc:?}"
        );
    }

    #[test]
    fn a_correct_xcode_selection_or_developer_dir_is_left_alone() {
        let selected = FakeSys::macos()
            .with_dir(XCODE_DEVELOPER_DIR)
            .with_tool("xcode-select", "/usr/bin/xcode-select")
            .with_output("xcode-select", "-p", &format!("{XCODE_DEVELOPER_DIR}\n"));
        assert!(Toolchain::detect(&selected).env.is_empty());

        let explicit = FakeSys::macos()
            .with_dir(XCODE_DEVELOPER_DIR)
            .with_env("DEVELOPER_DIR", "/Somewhere/Else");
        assert!(Toolchain::detect(&explicit).env.is_empty());

        let linux = FakeSys::linux().with_dir(XCODE_DEVELOPER_DIR);
        assert!(Toolchain::detect(&linux).env.is_empty());
    }

    #[test]
    fn finds_the_sdk_and_the_newest_ndk() {
        let sdk = "/opt/homebrew/share/android-commandlinetools";
        let sys = FakeSys::macos()
            .with_dir(&format!("{sdk}/ndk/26.1.10909125"))
            .with_dir(&format!("{sdk}/ndk/27.2.12479018"))
            .with_dir(&format!("{sdk}/ndk/9.0.1"));
        let tc = Toolchain::detect(&sys);
        assert_eq!(tc.android_sdk.as_deref(), Some(Path::new(sdk)));
        assert_eq!(
            tc.android_ndk.as_deref(),
            Some(Path::new(&format!("{sdk}/ndk/27.2.12479018")))
        );
        assert!(tc.env.iter().any(|(k, _)| k == "ANDROID_HOME"));
        assert!(
            tc.env
                .iter()
                .any(|(k, v)| k == "ANDROID_NDK_HOME" && v.ends_with("27.2.12479018"))
        );
    }

    #[test]
    fn notes_belong_to_one_platform_each() {
        let sdk = "/opt/homebrew/share/android-commandlinetools";
        let sys = FakeSys::macos()
            .with_dir(XCODE_DEVELOPER_DIR)
            .with_tool("xcode-select", "/usr/bin/xcode-select")
            .with_output(
                "xcode-select",
                "-p",
                "/Library/Developer/CommandLineTools\n",
            )
            .with_dir(&format!("{sdk}/ndk/27.2.12479018"));
        let tc = Toolchain::detect(&sys);

        let apple: Vec<&str> = tc.notes_for(Concern::Apple).collect();
        assert_eq!(apple.len(), 1, "{apple:?}");
        assert!(apple[0].contains("DEVELOPER_DIR"), "{apple:?}");
        assert!(
            apple.iter().all(|n| !n.contains("ANDROID")),
            "an iOS build must not hear about the Android SDK: {apple:?}"
        );

        let android: Vec<&str> = tc.notes_for(Concern::Android).collect();
        assert_eq!(android.len(), 2, "{android:?}");
        assert!(
            android.iter().all(|n| n.contains("ANDROID")),
            "an Android build must not hear about Xcode: {android:?}"
        );
    }

    #[test]
    fn explicit_android_variables_win_and_are_not_overridden() {
        let sys = FakeSys::linux()
            .with_dir("/sdk")
            .with_dir("/sdk/ndk/25.0.0")
            .with_dir("/custom/ndk")
            .with_env("ANDROID_HOME", "/sdk")
            .with_env("ANDROID_NDK_HOME", "/custom/ndk");
        let tc = Toolchain::detect(&sys);
        assert_eq!(tc.android_ndk.as_deref(), Some(Path::new("/custom/ndk")));
        assert!(tc.env.is_empty(), "{tc:?}");
    }

    #[test]
    fn an_explicit_ndk_that_is_not_a_directory_stops_the_build_instead_of_another_ndk() {
        let sys = FakeSys::linux()
            .with_dir("/sdk")
            .with_dir("/sdk/ndk/27.2.12479018")
            .with_env("ANDROID_HOME", "/sdk")
            .with_env("ANDROID_NDK_HOME", "/typo/ndk");
        let tc = Toolchain::detect(&sys);
        assert_eq!(
            tc.android_ndk, None,
            "the SDK's NDK is not used behind the variable"
        );
        assert_eq!(
            tc.android_ndk_unusable,
            Some(UnusableNdk {
                variable: "ANDROID_NDK_HOME",
                path: PathBuf::from("/typo/ndk"),
            })
        );
        assert!(
            tc.notes_for(Concern::Android).all(|n| !n.contains("NDK")),
            "{tc:?}"
        );
        let text = tc.android_ndk_unusable.unwrap().error().to_string();
        assert!(
            text.starts_with(
                "error[undra::C0003]: ANDROID_NDK_HOME is /typo/ndk, which is not a directory"
            ),
            "{text}"
        );
        assert!(
            text.contains("= note: ANDROID_NDK_HOME names the Android NDK"),
            "{text}"
        );
        assert!(
            text.contains("= help: point ANDROID_NDK_HOME at an installed NDK"),
            "{text}"
        );
        assert!(text.contains("errors.html#C0003"), "{text}");

        // The first variable that is set decides, even when a later one names a real NDK.
        let root = FakeSys::linux()
            .with_dir("/real/ndk")
            .with_env("ANDROID_NDK_ROOT", "/gone")
            .with_env("NDK_HOME", "/real/ndk");
        assert_eq!(
            find_android_ndk(&root, None),
            Err(UnusableNdk {
                variable: "ANDROID_NDK_ROOT",
                path: PathBuf::from("/gone"),
            })
        );
        // Unset, the SDK's newest NDK is used, as before.
        let unset = FakeSys::linux().with_dir("/sdk/ndk/27.2.12479018");
        assert_eq!(
            find_android_ndk(&unset, Some(Path::new("/sdk"))),
            Ok(Some(PathBuf::from("/sdk/ndk/27.2.12479018")))
        );
    }

    #[test]
    fn no_sdk_means_nothing_is_invented() {
        let tc = Toolchain::detect(&FakeSys::linux());
        assert_eq!(tc.android_sdk, None);
        assert_eq!(tc.android_ndk, None);
    }

    #[test]
    fn cargo_home_is_added_to_path_when_it_exists() {
        let sys = FakeSys::macos().with_dir("/Users/dev/.cargo/bin");
        assert_eq!(
            Toolchain::detect(&sys).path_dirs,
            vec![PathBuf::from("/Users/dev/.cargo/bin")]
        );
    }

    #[test]
    fn ndk_versions_order_numerically() {
        assert!(ndk_version("27.2.12479018") > ndk_version("27.0.9"));
        assert!(ndk_version("27.0.0") > ndk_version("9.9.9"));
        assert_eq!(ndk_major(Path::new("/x/ndk/27.2.12479018")), Some(27));
        assert_eq!(ndk_major(Path::new("/x/ndk-bundle")), None);
    }
}
