//! The CI workflow `undra init` writes: `.github/workflows/undra.yml`.
//!
//! One job for the core (format, clippy, tests, `undra bindgen --check`) and one per app of the
//! project: web (Node 24, `npm ci`, test, build), Android (`assembleDebug` on Ubuntu with NDK r27)
//! and iOS (`xcodebuild` on macOS 14). Every job installs `undra` with the install script of
//! `docs/RELEASING.md`, at the version in the `UNDRA_VERSION` variable at the top of the file, and
//! the apps' build systems run `undra build` themselves (see `builds::xcode`, the Gradle task and
//! the Vite plugin), so no job builds the core by hand. On a push, each app's job also makes the
//! release build whose symbol files a crash reporter needs and uploads them as an artifact named
//! after the core's namespace (`<namespace>-android-symbols`, `-ios-symbols`, `-web-symbols`,
//! ADR-046).
//!
//! The file is assembled from the snippets in `templates/ci/`, so a project without an iOS app
//! has no iOS job.

use crate::builds::{android, ios};
use crate::config::{Platform, ProjectConfig};
use crate::names::Names;
use crate::render::Vars;

/// Where the workflow is written, relative to the project root.
pub const WORKFLOW_PATH: &str = ".github/workflows/undra.yml";

const HEADER: &str = include_str!("../templates/ci/header.yml");
const INSTALL_UNDRA: &str = include_str!("../templates/ci/install-undra.yml");
const CORE: &str = include_str!("../templates/ci/core.yml");
const WEB: &str = include_str!("../templates/ci/web.yml");
const ANDROID: &str = include_str!("../templates/ci/android.yml");
const IOS: &str = include_str!("../templates/ci/ios.yml");

/// The workflow of a project: the jobs of the core and of each platform in `config`.
///
/// `version` is the full Undra version (`0.1.0`) the workflow installs; it is what
/// `UNDRA_VERSION` says at the top of the file.
#[must_use]
pub fn workflow(config: &ProjectConfig, names: &Names, version: &str) -> String {
    let android_targets = config
        .android
        .abis
        .iter()
        .map(|abi| android::triple_of(abi))
        .collect::<Vec<_>>()
        .join(", ");
    let mut ios_targets = vec![ios::DEVICE_TRIPLE.to_owned()];
    for arch in &config.ios.simulator_archs {
        let triple = ios::simulator_triple(arch).to_owned();
        if !ios_targets.contains(&triple) {
            ios_targets.push(triple);
        }
    }
    let platforms = config
        .platforms
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ");
    let vars = Vars::new()
        .with("NAME", config.name.clone())
        .with(
            "NAMESPACE",
            config
                .core_namespace
                .clone()
                .unwrap_or_else(|| names.core_lib.clone()),
        )
        .with("APP", names.pascal.clone())
        .with("PLATFORM_LIST", platforms)
        .with("UNDRA_FULL_VERSION", version)
        .with("INSTALL_UNDRA", INSTALL_UNDRA)
        .with("ANDROID_TARGETS", android_targets)
        .with("IOS_TARGETS", ios_targets.join(", "));
    let mut out = String::new();
    let mut snippets = vec![HEADER, CORE];
    for platform in &config.platforms {
        snippets.push(match platform {
            Platform::Web => WEB,
            Platform::Android => ANDROID,
            Platform::Ios => IOS,
        });
    }
    for snippet in snippets {
        out.push_str(
            &vars
                .render(snippet)
                .expect("the CI templates only use placeholders this function sets"),
        );
    }
    out
}

/// The Undra version `undra init` writes into the workflow: this CLI's own.
#[must_use]
pub fn current_version() -> &'static str {
    crate::version::SEMVER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Platform;

    fn project(platforms: &[Platform]) -> (ProjectConfig, Names) {
        (
            ProjectConfig::new("todo-app", "com.example.todoapp", platforms.to_vec()),
            Names::derive("todo-app"),
        )
    }

    #[test]
    fn a_job_per_platform_after_the_core() {
        let (config, names) = project(&Platform::ALL);
        let text = workflow(&config, &names, "1.2.3");
        // The keys of `env:` and `concurrency:` are not jobs: look under `jobs:`.
        let after = text.split("\njobs:\n").nth(1).unwrap();
        let in_jobs: Vec<&str> = after
            .lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("   ") && l.trim_end().ends_with(':'))
            .map(|l| l.trim().trim_end_matches(':'))
            .collect();
        assert_eq!(in_jobs, ["core", "ios", "android", "web"]);
        assert!(!text.contains("@@"), "{text}");
    }

    #[test]
    fn a_project_without_an_ios_app_has_no_ios_job() {
        let (config, names) = project(&[Platform::Android, Platform::Web]);
        let text = workflow(&config, &names, "1.2.3");
        assert!(
            !text.contains("macos-14") && !text.contains("xcodebuild"),
            "{text}"
        );
        assert!(text.contains("android:") && text.contains("web:") && text.contains("core:"));
        let (config, names) = project(&[Platform::Web]);
        let text = workflow(&config, &names, "1.2.3");
        assert!(
            !text.contains("setup-java") && !text.contains("ndk;27"),
            "{text}"
        );
        assert!(
            text.contains("platforms") || text.contains("(web)"),
            "{text}"
        );
    }

    #[test]
    fn the_version_is_pinned_once_at_the_top() {
        let (config, names) = project(&Platform::ALL);
        let text = workflow(&config, &names, "1.2.3");
        let first_env = text.lines().position(|l| l == "env:").unwrap();
        let first_job = text.lines().position(|l| l == "jobs:").unwrap();
        assert!(first_env < first_job);
        assert_eq!(text.matches("UNDRA_VERSION: ").count(), 1, "{text}");
        assert!(text.contains("\n  UNDRA_VERSION: \"1.2.3\"\n"), "{text}");
        // Every job installs undra the way docs/RELEASING.md says users will.
        assert_eq!(
            text.matches(
                "curl -fsSL \"https://raw.githubusercontent.com/shreypdev/undra/v${UNDRA_VERSION}/site/install.sh\" | sh"
            )
            .count(),
            4,
            "{text}"
        );
        // Not the site's latest installer: the one of the pinned release.
        assert!(
            !text.contains("shreypdev.github.io/undra/install.sh"),
            "{text}"
        );
        assert_eq!(
            text.matches("echo \"$HOME/.undra/bin\" >> \"$GITHUB_PATH\"")
                .count(),
            4
        );
    }

    #[test]
    fn the_jobs_run_what_the_brief_names() {
        let (config, names) = project(&Platform::ALL);
        let text = workflow(&config, &names, "0.1.0");
        for needle in [
            "cargo fmt --all --check",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo test --workspace",
            "undra bindgen --check",
            "node-version: 24",
            "npm ci",
            "npm test --if-present",
            "npm run build",
            "./gradlew assembleDebug",
            "ndk;27.2.12479018",
            "ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018",
            "runs-on: macos-14",
            "-project ios/TodoApp.xcodeproj",
            "-scheme TodoApp",
            "-sdk iphonesimulator",
            "targets: aarch64-linux-android, x86_64-linux-android",
            "targets: aarch64-apple-ios, aarch64-apple-ios-sim",
            "targets: wasm32-unknown-unknown",
        ] {
            assert!(text.contains(needle), "no {needle:?}:\n{text}");
        }
        // ADR-044: the Swift runtime has no link-time stand-in any more, so nothing switches it off.
        assert!(!text.contains("UNDRA_LINK_CORE"), "{text}");
    }

    #[test]
    fn the_rust_targets_follow_the_project() {
        let (mut config, names) = project(&Platform::ALL);
        config.android.abis = vec!["arm64-v8a".into(), "armeabi-v7a".into()];
        config.ios.simulator_archs = vec!["arm64".into(), "x86_64".into()];
        let text = workflow(&config, &names, "0.1.0");
        assert!(
            text.contains("targets: aarch64-linux-android, armv7-linux-androideabi\n"),
            "{text}"
        );
        assert!(
            text.contains("targets: aarch64-apple-ios, aarch64-apple-ios-sim, x86_64-apple-ios\n"),
            "{text}"
        );
    }

    #[test]
    fn a_push_uploads_the_symbol_files_of_each_app_named_after_the_core() {
        let (config, names) = project(&Platform::ALL);
        let text = workflow(&config, &names, "0.1.0");
        for needle in [
            "name: todo_app_core-android-symbols",
            "name: todo_app_core-ios-symbols",
            "name: todo_app_core-web-symbols",
            "undra build --platform android --release",
            "-configuration Release",
            "build/xcode/Build/Products/Release-iphonesimulator/*.dSYM",
        ] {
            assert!(text.contains(needle), "no {needle:?}:\n{text}");
        }
        // Three uploads, from three jobs, each from `build/symbols/`, and only on a push: a pull request
        // (a fork's included) builds the debug app and uploads nothing.
        assert_eq!(
            text.matches("actions/upload-artifact@v4").count(),
            3,
            "{text}"
        );
        assert_eq!(text.matches("path: build/symbols/").count(), 2, "{text}");
        assert_eq!(
            text.matches("if: github.event_name == 'push'").count(),
            5,
            "{text}"
        );
        // The crash reporters' side is said once per platform, in a comment.
        assert!(
            text.contains("Crashlytics")
                && text.contains("Sentry")
                && text.contains("Play Console"),
            "{text}"
        );
        // The namespace is the project's own when it sets one.
        let (mut config, names) = project(&[Platform::Web]);
        config.core_namespace = Some("acme_pay".to_owned());
        assert!(workflow(&config, &names, "0.1.0").contains("name: acme_pay-web-symbols"));
        // A project without an app has no upload for it.
        let (config, names) = project(&[Platform::Android]);
        let text = workflow(&config, &names, "0.1.0");
        assert!(
            !text.contains("ios-symbols") && !text.contains("web-symbols"),
            "{text}"
        );
    }

    #[test]
    fn the_version_is_the_cli_s_own() {
        assert_eq!(current_version(), env!("CARGO_PKG_VERSION"));
    }
}
