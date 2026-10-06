//! Where a project made by a released `undra` gets Undra from (ADR-063): one GitHub repository at one
//! release tag, `v<version>`, with no registry account on either side.
//!
//! | What | From | In a project |
//! |---|---|---|
//! | the crates | the repository, by tag | `undra = { git = "https://github.com/shreypdev/undra", tag = "v1.0.0" }` |
//! | the Swift runtime | the package at the repository's root | `.package(url: "https://github.com/shreypdev/undra", from: "1.0.0")` |
//! | the Kotlin runtime | JitPack, built from the tag ([`MAVEN`]) | `com.github.shreypdev.undra:runtime:v1.0.0` |
//! | the npm packages | the GitHub Release's assets | `https://github.com/shreypdev/undra/releases/download/v1.0.0/undra-runtime-1.0.0.tgz` |
//!
//! Three environment variables replace the addresses for a rehearsal of a release against a local copy
//! (`packaging/rehearse-launch.sh`) or for a company mirror: [`ENV_GIT_URL`], [`ENV_RELEASE_URL`] and
//! [`ENV_MAVEN_REPO`]. They are read by the commands that write dependency lines (`undra init`, `undra adopt`,
//! `undra bindgen`, `undra upgrade`), each of which warns when one is set, and are not stored in the project as such:
//! what they replaced is written into its files. `undra bindgen` in a project names the repository its core's `undra`
//! dependency comes from (`core/Cargo.toml`) rather than the environment's ([`Dist::follow_repository`]), so a project
//! made against a mirror stays on it and `undra bindgen --check` does not depend on the shell it runs in.

use crate::error::{CliError, Code, Result};
use crate::sys::Sys;
use crate::ui::Ui;

/// The Undra repository: the crates (by tag) and the Swift package (the manifest at its root).
pub const REPO_URL: &str = "https://github.com/shreypdev/undra";

/// Where the assets of a GitHub Release are downloaded: `<RELEASES_URL>/v<version>/<file>`.
pub const RELEASES_URL: &str = "https://github.com/shreypdev/undra/releases/download";

/// Where a Maven artifact of Undra's Kotlin runtime is published, and how it is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MavenChannel {
    /// The group of every module.
    pub group: &'static str,
    /// What comes before the release version in an artifact's version (JitPack names a build by its tag: `v`).
    pub version_prefix: &'static str,
    /// The repository an app adds for the group; `None` for one Gradle already knows (Maven Central).
    pub repository: Option<&'static str>,
}

/// The one place the Kotlin coordinates are decided (ADR-063, section 4): JitPack builds the modules from the release tag.
/// Moving to Maven Central is this line (`group: "dev.undra"`, no prefix, no repository); `undra upgrade` moves the pins.
pub const MAVEN: MavenChannel = MavenChannel {
    group: "com.github.shreypdev.undra",
    version_prefix: "v",
    repository: Some("https://jitpack.io"),
};

/// Every group an Undra Kotlin artifact is named by: the release channel's and the repository's own (`dev.undra`, what a
/// checkout's composite build substitutes and the eventual Maven Central name). `undra upgrade` recognises both.
pub const MAVEN_GROUPS: [&str; 2] = ["com.github.shreypdev.undra", "dev.undra"];

/// The Kotlin modules an app can depend on (each is published at every release).
pub const MAVEN_MODULES: [&str; 6] = [
    "runtime",
    "android-adapters",
    "android-work",
    "undra-compose",
    "okhttp-adapters",
    "testkit",
];

/// The npm packages attached to every release, with the file stem `npm pack` gives each (`<stem>-<version>.tgz`).
pub const NPM_PACKAGES: [(&str, &str); 3] = [
    ("@undra/runtime", "undra-runtime"),
    ("@undra/react-native", "undra-react-native"),
    ("@undra/testkit", "undra-testkit"),
];

/// Replaces [`REPO_URL`]: the repository the crates and the Swift package are fetched from.
pub const ENV_GIT_URL: &str = "UNDRA_DIST_GIT_URL";
/// Replaces [`RELEASES_URL`]: where `v<version>/<asset>` is downloaded.
pub const ENV_RELEASE_URL: &str = "UNDRA_DIST_RELEASE_URL";
/// Replaces the Maven repository of [`MAVEN`]: where the Kotlin artifacts are resolved.
pub const ENV_MAVEN_REPO: &str = "UNDRA_DIST_MAVEN_REPO";

/// The addresses a released project's dependencies are written with: GitHub's, or the mirror the environment names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dist {
    /// The repository of the crates and the Swift package.
    pub git_url: String,
    /// The base of the release assets' URLs (`<release_url>/v<version>/<file>`).
    pub release_url: String,
    /// The Maven repository an app adds for [`MAVEN`]'s group, if any.
    pub maven_repo: Option<String>,
}

impl Default for Dist {
    fn default() -> Self {
        Dist::github()
    }
}

impl Dist {
    /// GitHub's addresses: what a project gets when no override is set.
    #[must_use]
    pub fn github() -> Dist {
        Dist {
            git_url: REPO_URL.to_owned(),
            release_url: RELEASES_URL.to_owned(),
            maven_repo: MAVEN.repository.map(ToOwned::to_owned),
        }
    }

    /// The addresses after the overrides of the environment.
    ///
    /// # Errors
    ///
    /// `C0009` naming the variable whose value is not a URL Undra can write into a project.
    pub fn from_sys(sys: &dyn Sys) -> Result<Dist> {
        Dist::from_lookup(|key| sys.env(key))
    }

    /// [`Dist::from_sys`], with a warning for every override in effect: a project written with one fetches Undra from
    /// somewhere other than GitHub, and whoever runs the command sees that before it writes anything.
    ///
    /// # Errors
    ///
    /// As [`Dist::from_sys`].
    pub fn announced(sys: &dyn Sys, ui: &Ui) -> Result<Dist> {
        let dist = Dist::from_sys(sys)?;
        for warning in override_warnings(|key| sys.env(key)) {
            ui.warn(&warning);
        }
        Ok(dist)
    }

    /// [`Dist::announced`] for `undra upgrade`, whose warning about the git URL says what the upgrade does with it
    /// ([`upgrade_override_warnings`]): it moves pins in place and keeps the repository each dependency names.
    ///
    /// # Errors
    ///
    /// As [`Dist::from_sys`].
    pub fn announced_for_upgrade(sys: &dyn Sys, ui: &Ui) -> Result<Dist> {
        let dist = Dist::from_sys(sys)?;
        for warning in upgrade_override_warnings(|key| sys.env(key)) {
            ui.warn(&warning);
        }
        Ok(dist)
    }

    /// Names the repository a project's core takes its `undra` crates from (`url`, from `core/Cargo.toml`) for the
    /// Swift package too, so the bindings a project generates stay on the source the project was made with, whatever
    /// the environment says. GitHub's repository, in any spelling (`.git`, `ssh`), stays [`REPO_URL`].
    pub fn follow_repository(&mut self, url: &str) {
        self.git_url = if crate::upgrade::same_repository(url, REPO_URL) {
            REPO_URL.to_owned()
        } else {
            url.trim().trim_end_matches('/').to_owned()
        };
    }

    /// [`Dist::from_sys`] over any lookup (the tests pass a map).
    ///
    /// # Errors
    ///
    /// As [`Dist::from_sys`].
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Dist> {
        let mut dist = Dist::github();
        if let Some(value) = get(ENV_GIT_URL) {
            dist.git_url = checked(ENV_GIT_URL, &value, true)?;
        }
        if let Some(value) = get(ENV_RELEASE_URL) {
            dist.release_url = checked(ENV_RELEASE_URL, &value, false)?;
        }
        if let Some(value) = get(ENV_MAVEN_REPO) {
            dist.maven_repo = Some(checked(ENV_MAVEN_REPO, &value, false)?);
        }
        Ok(dist)
    }

    /// The identity SwiftPM gives the package at [`Dist::git_url`]: its last path component without `.git`, lowercased
    /// (`undra`). A `.product(name:package:)` names the package by it.
    #[must_use]
    pub fn swift_package_identity(&self) -> String {
        let last = self
            .git_url
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .unwrap_or("undra");
        last.trim_end_matches(".git").to_ascii_lowercase()
    }

    /// The URL of an npm package's tarball at a release: `<release_url>/v<version>/<stem>-<version>.tgz`.
    #[must_use]
    pub fn npm_url(&self, stem: &str, version: &str) -> String {
        format!("{}/v{version}/{stem}-{version}.tgz", self.release_url)
    }

    /// The Gradle lines that add [`Dist::maven_repo`] for [`MAVEN`]'s group, indented by `indent` (none when there is no
    /// repository to add). `kotlin_dsl` picks `settings.gradle.kts` or Groovy's `settings.gradle`.
    #[must_use]
    pub fn gradle_repository(&self, indent: &str, kotlin_dsl: bool) -> Option<String> {
        let url = self.maven_repo.as_deref()?;
        let group = MAVEN.group;
        Some(if kotlin_dsl {
            format!(
                "{indent}// Undra's Kotlin runtime, built from the release tag (ADR-063). Only its group is looked up here.\n\
                 {indent}maven {{\n\
                 {indent}    url = uri(\"{url}\")\n\
                 {indent}    content {{ includeGroup(\"{group}\") }}\n\
                 {indent}}}\n"
            )
        } else {
            format!(
                "{indent}// Undra's Kotlin runtime, built from the release tag (ADR-063). Only its group is looked up here.\n\
                 {indent}maven {{\n\
                 {indent}    url '{url}'\n\
                 {indent}    content {{ includeGroup '{group}' }}\n\
                 {indent}}}\n"
            )
        })
    }
}

/// One warning per override the environment sets (ADR-063): the variable, its value and the GitHub address it replaces.
#[must_use]
pub fn override_warnings(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    [
        (ENV_GIT_URL, REPO_URL),
        (ENV_RELEASE_URL, RELEASES_URL),
        (ENV_MAVEN_REPO, MAVEN.repository.unwrap_or("Maven Central")),
    ]
    .into_iter()
    .filter_map(|(var, github)| {
        let value = get(var)?;
        Some(format!(
            "{var} is set: the dependency lines this command writes name `{}` instead of {github} (a rehearsal or a mirror, ADR-063); unset it for a project that builds from GitHub",
            value.trim()
        ))
    })
    .collect()
}

/// [`override_warnings`] for `undra bindgen` of a project whose core takes Undra from the git repository `core_repository`
/// (`None` for a core that does not, or without a project): the Swift package then names that repository, not the git URL
/// override ([`Dist::follow_repository`]), and the warning says so.
#[must_use]
pub fn bindgen_override_warnings(
    get: impl Fn(&str) -> Option<String>,
    core_repository: Option<&str>,
) -> Vec<String> {
    let git = get(ENV_GIT_URL);
    override_warnings(get)
        .into_iter()
        .map(|warning| match (&git, core_repository) {
            (Some(value), Some(core)) if warning.starts_with(ENV_GIT_URL) => format!(
                "{ENV_GIT_URL} is set, but the Swift package this command writes names the repository core/Cargo.toml takes Undra from, `{}`, not `{}` (ADR-063)",
                core.trim(),
                value.trim()
            ),
            _ => warning,
        })
        .collect()
}

/// [`override_warnings`] as `undra upgrade` reads them. The release URL and the Maven repository are written as they say;
/// the git URL is not: an upgrade moves the tag of an Undra dependency and keeps the repository the dependency names
/// (`core/Cargo.toml` keeps its `git` URL, and the regenerated Swift package follows the core). The stand-in only makes a
/// dependency on it count as Undra's, and replaces the Swift package URL of before ADR-063 (`undra-swift`).
#[must_use]
pub fn upgrade_override_warnings(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let git = get(ENV_GIT_URL);
    override_warnings(get)
        .into_iter()
        .map(|warning| match &git {
            Some(value) if warning.starts_with(ENV_GIT_URL) => format!(
                "{ENV_GIT_URL} is set: `undra upgrade` keeps the repository each Undra dependency names (the `git` URL of core/Cargo.toml stays as it is, only its tag moves), counts a dependency on `{}` as Undra's, and writes `{0}` only in place of the old `undra-swift` package URL (ADR-063)",
                value.trim()
            ),
            _ => warning,
        })
        .collect()
}

/// The version of a Kotlin module at a release (`v1.0.0` on JitPack).
#[must_use]
pub fn maven_version(version: &str) -> String {
    format!("{}{version}", MAVEN.version_prefix)
}

/// `group:module:version` of a Kotlin module at a release.
#[must_use]
pub fn maven_coordinate(module: &str, version: &str) -> String {
    format!("{}:{module}:{}", MAVEN.group, maven_version(version))
}

/// The release version an asset URL names, when it is `<anything>/v<version>/<stem>-<version>.tgz` for `stem`.
#[must_use]
pub fn npm_url_version<'a>(url: &'a str, stem: &str) -> Option<&'a str> {
    let (dir, file) = url.rsplit_once('/')?;
    let version = file
        .strip_prefix(stem)?
        .strip_prefix('-')?
        .strip_suffix(".tgz")?;
    let tag = dir.rsplit('/').next()?;
    (tag.strip_prefix('v') == Some(version) && !version.is_empty()).then_some(version)
}

/// A release version in full: a two-part `[undra] version` an older `undra init` wrote (`"1.0"`) means `1.0.0`.
#[must_use]
pub fn full_version(text: &str) -> String {
    let text = text.trim();
    let core = text.split(['-', '+']).next().unwrap_or(text);
    if core.split('.').count() == 2 {
        format!("{core}.0{}", &text[core.len()..])
    } else {
        text.to_owned()
    }
}

/// Checks the value of an override: a URL with a scheme Undra can write, without the characters that would end or
/// escape the strings it is written into (TOML, JSON, Kotlin, Groovy, an Xcode project).
fn checked(var: &str, value: &str, git: bool) -> Result<String> {
    let value = value.trim();
    let schemes: &[&str] = if git {
        &["https://", "http://", "file://", "ssh://"]
    } else {
        &["https://", "http://", "file://"]
    };
    let scheme = schemes.iter().find(|s| value.starts_with(**s));
    let rest = scheme.map(|s| value[s.len()..].trim_end_matches('/'));
    let bad_char = value
        .chars()
        .find(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '`' | '$' | '{' | '}'));
    let problem = match (rest, bad_char) {
        (_, Some(c)) => Some(format!("it contains {c:?}")),
        (None, None) => Some(format!("it does not start with {}", schemes.join(", "))),
        (Some(""), None) => Some("it has a scheme and nothing after it".to_owned()),
        (Some(_), None) => None,
    };
    match (problem, scheme, rest) {
        (None, Some(scheme), Some(rest)) => Ok(format!("{scheme}{rest}")),
        (problem, _, _) => Err(CliError::new(
            Code::BadArgument,
            format!(
                "{var} is `{value}`, which is not a URL Undra can write into a project: {}",
                problem.unwrap_or_default()
            ),
            format!(
                "{var} replaces a GitHub address in the dependency lines `undra init`, `undra adopt`, `undra bindgen` and `undra upgrade` write (a rehearsal of a release against a local copy, or a mirror; ADR-063), and the value is written into Cargo, Gradle, Xcode and npm files as it is"
            ),
            format!(
                "set it to {} URL without spaces or quotes (a rehearsal uses file:///absolute/path), or unset it to use GitHub",
                if git {
                    "an https://, http://, ssh:// or file://"
                } else {
                    "an https://, http:// or file://"
                }
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dist(vars: &[(&str, &str)]) -> Result<Dist> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Dist::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn without_overrides_everything_is_on_github() {
        let d = dist(&[]).unwrap();
        assert_eq!(d, Dist::github());
        assert_eq!(d.git_url, "https://github.com/shreypdev/undra");
        assert_eq!(d.swift_package_identity(), "undra");
        assert_eq!(
            d.npm_url("undra-runtime", "1.0.0"),
            "https://github.com/shreypdev/undra/releases/download/v1.0.0/undra-runtime-1.0.0.tgz"
        );
        assert_eq!(d.maven_repo.as_deref(), Some("https://jitpack.io"));
        assert_eq!(
            maven_coordinate("runtime", "1.0.0"),
            "com.github.shreypdev.undra:runtime:v1.0.0"
        );
    }

    #[test]
    fn overrides_replace_each_address_and_lose_a_trailing_slash() {
        let d = dist(&[
            (ENV_GIT_URL, "file:///tmp/rehearsal/remote/undra.git"),
            (ENV_RELEASE_URL, "http://127.0.0.1:8123/releases/"),
            (ENV_MAVEN_REPO, " file:///tmp/rehearsal/m2 "),
        ])
        .unwrap();
        assert_ne!(d, Dist::github());
        assert_eq!(d.git_url, "file:///tmp/rehearsal/remote/undra.git");
        assert_eq!(d.swift_package_identity(), "undra");
        assert_eq!(
            d.npm_url("undra-react-native", "1.0.0-rc.1"),
            "http://127.0.0.1:8123/releases/v1.0.0-rc.1/undra-react-native-1.0.0-rc.1.tgz"
        );
        assert_eq!(d.maven_repo.as_deref(), Some("file:///tmp/rehearsal/m2"));
        let ssh = dist(&[(ENV_GIT_URL, "ssh://git@mirror.example/team/Undra.git")]).unwrap();
        assert_eq!(ssh.swift_package_identity(), "undra");
    }

    #[test]
    fn every_override_is_announced_and_none_without_one() {
        let map: HashMap<&str, &str> = [
            (ENV_GIT_URL, "file:///tmp/remote/undra.git"),
            (ENV_MAVEN_REPO, "https://maven.example/undra"),
        ]
        .into_iter()
        .collect();
        let warnings = override_warnings(|key| map.get(key).map(|v| (*v).to_owned()));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].starts_with("UNDRA_DIST_GIT_URL is set")
                && warnings[0].contains(
                    "`file:///tmp/remote/undra.git` instead of https://github.com/shreypdev/undra"
                ),
            "{warnings:?}"
        );
        assert!(
            warnings[1].starts_with("UNDRA_DIST_MAVEN_REPO is set")
                && warnings[1].contains("instead of https://jitpack.io"),
            "{warnings:?}"
        );
        assert!(override_warnings(|_| None).is_empty());
    }

    #[test]
    fn bindgen_says_the_swift_package_follows_the_core() {
        let get =
            |key: &str| (key == ENV_GIT_URL).then(|| "file:///tmp/remote/undra.git".to_owned());
        let following = bindgen_override_warnings(get, Some(REPO_URL));
        assert_eq!(following.len(), 1);
        assert!(
            following[0].contains("names the repository core/Cargo.toml takes Undra from, `https://github.com/shreypdev/undra`, not `file:///tmp/remote/undra.git`"),
            "{following:?}"
        );
        // A core that names no git repository: the override is what the package names.
        assert_eq!(bindgen_override_warnings(get, None), override_warnings(get));
    }

    #[test]
    fn the_upgrade_says_it_keeps_the_repository_a_dependency_names() {
        let map: HashMap<&str, &str> = [
            (ENV_GIT_URL, "file:///tmp/remote/undra.git"),
            (ENV_RELEASE_URL, "http://127.0.0.1:8123/releases"),
        ]
        .into_iter()
        .collect();
        let warnings = upgrade_override_warnings(|key| map.get(key).map(|v| (*v).to_owned()));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0]
                .starts_with("UNDRA_DIST_GIT_URL is set: `undra upgrade` keeps the repository")
                && warnings[0].contains("the `git` URL of core/Cargo.toml stays as it is")
                && warnings[0].contains("`file:///tmp/remote/undra.git`")
                && !warnings[0].contains("instead of"),
            "{warnings:?}"
        );
        // The release URL is written into package.json as it says.
        assert!(
            warnings[1].starts_with("UNDRA_DIST_RELEASE_URL is set")
                && warnings[1].contains("instead of"),
            "{warnings:?}"
        );
        assert!(upgrade_override_warnings(|_| None).is_empty());
    }

    #[test]
    fn a_project_keeps_the_repository_its_core_comes_from() {
        // A project made against a mirror: its core names the mirror, and so does the Swift package bindgen writes, with
        // or without the environment that made it.
        let mut d = Dist::github();
        d.follow_repository("file:///srv/mirror/undra.git/");
        assert_eq!(d.git_url, "file:///srv/mirror/undra.git");
        assert_eq!(d.swift_package_identity(), "undra");
        // GitHub's repository in another spelling is still GitHub's, written as `undra init` writes it.
        for spelling in [
            "https://github.com/shreypdev/undra.git",
            "https://github.com/shreypdev/undra/",
            "ssh://git@github.com/shreypdev/undra.git",
        ] {
            let mut d = dist(&[(ENV_GIT_URL, "file:///tmp/elsewhere")]).unwrap();
            d.follow_repository(spelling);
            assert_eq!(d.git_url, REPO_URL, "{spelling}");
        }
    }

    #[test]
    fn a_malformed_override_is_refused_with_the_variable_the_reason_and_the_fix() {
        for (var, value, reason) in [
            (ENV_GIT_URL, "github.com/me/undra", "does not start with"),
            (
                ENV_RELEASE_URL,
                "ssh://host/releases",
                "does not start with",
            ),
            (
                ENV_MAVEN_REPO,
                "https://repo.example/my maven",
                "contains ' '",
            ),
            (ENV_MAVEN_REPO, "https://repo.example/\"x", "contains '\"'"),
            (ENV_GIT_URL, "https://host/$HOME/undra", "contains '$'"),
            (ENV_RELEASE_URL, "file://", "nothing after it"),
        ] {
            let e = dist(&[(var, value)]).unwrap_err();
            assert_eq!(e.code, Code::BadArgument, "{e:?}");
            assert!(e.what.starts_with(var) && e.what.contains(reason), "{e:?}");
            assert!(
                e.why.contains("ADR-063") && e.fix.contains("unset it"),
                "{e:?}"
            );
        }
    }

    #[test]
    fn the_gradle_repository_is_for_the_undra_group_only() {
        let kts = Dist::github().gradle_repository("        ", true).unwrap();
        assert!(kts.contains("        maven {\n            url = uri(\"https://jitpack.io\")\n            content { includeGroup(\"com.github.shreypdev.undra\") }\n        }\n"), "{kts}");
        let groovy = Dist::github().gradle_repository("  ", false).unwrap();
        assert!(
            groovy.contains("url 'https://jitpack.io'")
                && groovy.contains("includeGroup 'com.github.shreypdev.undra'"),
            "{groovy}"
        );
        let none = Dist {
            maven_repo: None,
            ..Dist::github()
        };
        assert_eq!(none.gradle_repository("", true), None);
    }

    #[test]
    fn asset_urls_name_their_version() {
        let url =
            "https://github.com/shreypdev/undra/releases/download/v1.2.3/undra-runtime-1.2.3.tgz";
        assert_eq!(npm_url_version(url, "undra-runtime"), Some("1.2.3"));
        assert_eq!(npm_url_version(url, "undra-react-native"), None);
        assert_eq!(
            npm_url_version("https://x/v1.2.3/undra-runtime-1.2.4.tgz", "undra-runtime"),
            None
        );
    }

    #[test]
    fn a_release_line_is_a_full_version() {
        assert_eq!(full_version("1.0"), "1.0.0");
        assert_eq!(full_version("1.0.2"), "1.0.2");
        assert_eq!(full_version("1.0.0-rc.1"), "1.0.0-rc.1");
        assert_eq!(full_version(" 0.1 "), "0.1.0");
    }
}
