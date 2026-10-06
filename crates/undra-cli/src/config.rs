//! `undra.toml`: the file that makes a directory an Undra project.
//!
//! ```toml
//! [project]
//! name = "todo"
//! id = "com.example.todo"
//! platforms = ["ios", "android", "web"]
//!
//! [core]
//! path = "core"
//! namespace = "todo"   # optional: names the core's symbol and libraries (ADR-044)
//! ```
//!
//! Every key but `project.name` and `project.id` has a default, and an unknown key is an error
//! (a typo must not silently fall back to a default). [`ProjectConfig::render`] writes the file
//! `undra init` creates, with comments that say what each section is for.

use std::path::Path;

use undra_bindgen::SwiftObservation;

use crate::error::{CliError, Code, Result};
use crate::toml_lite::{self, Document, Entry, Value, quote};

/// A platform an app is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// iOS: an XCFramework and a SwiftUI app.
    Ios,
    /// Android: `jniLibs/` and a Compose app.
    Android,
    /// The web: a wasm module and a React app.
    Web,
}

impl Platform {
    /// Every platform, in the order they are printed.
    pub const ALL: [Platform; 3] = [Platform::Ios, Platform::Android, Platform::Web];

    /// The name used in `undra.toml` and on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Android => "android",
            Platform::Web => "web",
        }
    }

    /// Parses a platform name.
    ///
    /// # Errors
    ///
    /// Lists the valid names.
    pub fn parse(text: &str) -> Result<Platform> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ios" => Ok(Platform::Ios),
            "android" => Ok(Platform::Android),
            "web" | "wasm" => Ok(Platform::Web),
            other => Err(CliError::bad_argument(
                format!("`{other}` is not a platform Undra builds for"),
                "a platform decides which toolchain builds the core and which app shell is created",
                "use one of: ios, android, web",
            )),
        }
    }

    /// Parses a comma-separated list such as `ios,android`.
    ///
    /// # Errors
    ///
    /// The first invalid name, or an empty list.
    pub fn parse_list(text: &str) -> Result<Vec<Platform>> {
        let mut out = Vec::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let platform = Platform::parse(part)?;
            if !out.contains(&platform) {
                out.push(platform);
            }
        }
        if out.is_empty() {
            return Err(CliError::bad_argument(
                "the platform list is empty",
                "an app without a platform has nothing to build",
                "list at least one of: ios, android, web",
            ));
        }
        out.sort();
        Ok(out)
    }
}

/// Whether `undra bindgen` writes the lint exclusion files beside the generated trees: `[bindings] lint_exclusions` of
/// `undra.toml` (ADR-061).
///
/// A repository that lints everything needs the generated bindings left out of its linters. The default writes the files each
/// linter reads beside the trees ([`crate::lint`] lists them); a repository that configures its linters in one place says
/// `none` and adds the same exclusions to that configuration (the "Linters over the whole tree" section of the Bazel guide
/// lists the lines). The `@file:Suppress` line at the top of every generated Kotlin file is not one of these files: it is part
/// of the file and stays whatever this says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LintExclusions {
    /// `"beside"`, the default: the exclusion files are written beside the trees and are part of their manifest, so `--check`
    /// compares them and a later run removes them with the rest.
    #[default]
    Beside,
    /// `"none"`: no exclusion file is written. A file an earlier run wrote is removed with the rest of the manifest's
    /// leftovers, so switching the setting leaves nothing stale.
    None,
}

impl LintExclusions {
    /// The value as `undra.toml` spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LintExclusions::Beside => "beside",
            LintExclusions::None => "none",
        }
    }
}

impl std::str::FromStr for LintExclusions {
    type Err = String;

    /// Reads `beside` or `none`.
    ///
    /// # Errors
    ///
    /// Names the text and the two values.
    fn from_str(text: &str) -> std::result::Result<LintExclusions, String> {
        match text {
            "beside" => Ok(LintExclusions::Beside),
            "none" => Ok(LintExclusions::None),
            other => Err(format!(
                "`{other}` is not a lint exclusions setting (`beside` or `none`)"
            )),
        }
    }
}

/// Names of the generated bindings, overriding what `undra-bindgen` derives from the crate name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BindingsConfig {
    /// The Swift module (`Sources/<module>`); default `<Pascal>Core`.
    pub swift_module: Option<String>,
    /// The Kotlin package; default `<project id>.core`.
    pub kotlin_package: Option<String>,
    /// The npm scope, without `@`; default `app`.
    pub ts_scope: Option<String>,
    /// The npm package name without the scope; default the core's package name.
    pub ts_package: Option<String>,
    /// Map `i64`/`u64` to `number` instead of `bigint` in TypeScript.
    pub ts_js_number: bool,
    /// Emit `throws(E)` on Swift port requirements (calls always use plain
    /// `throws`, ADR-032); `true` unless turned off.
    pub swift_typed_throws: Option<bool>,
    /// How generated Swift stores are observed (ADR-045): `observation` (`@Observable`, iOS 17 and later) or
    /// `observable-object` (`ObservableObject` with `@Published`, iOS 15 and later). Absent: chosen by
    /// `[ios] deployment_target`, `observable-object` below 17.0.
    pub swift_observation: Option<SwiftObservation>,
    /// Whether the lint exclusion files are written beside the generated trees ([`LintExclusions`]); `beside` unless
    /// `lint_exclusions = "none"`.
    pub lint_exclusions: LintExclusions,
}

/// iOS build settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosConfig {
    /// The minimum iOS version (`IPHONEOS_DEPLOYMENT_TARGET`): 15.0 or later. Below 17.0 the generated Swift
    /// stores are `ObservableObject`s and not `@Observable` (ADR-045, [`ProjectConfig::swift_observation`]).
    pub deployment_target: String,
    /// Simulator architectures: `arm64` and/or `x86_64`.
    pub simulator_archs: Vec<String>,
    /// What a release build optimises for (`opt_level`, [`NativeOptLevel`]).
    pub opt_level: NativeOptLevel,
}

impl Default for IosConfig {
    fn default() -> Self {
        IosConfig {
            deployment_target: "17.0".to_owned(),
            simulator_archs: vec!["arm64".to_owned()],
            opt_level: NativeOptLevel::default(),
        }
    }
}

/// Android build settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidConfig {
    /// The ABIs to build: `arm64-v8a`, `x86_64`, `armeabi-v7a`, `x86`.
    pub abis: Vec<String>,
    /// The minimum API level (`cargo ndk --platform`).
    pub min_sdk: u32,
    /// What a release build optimises for (`opt_level`, [`NativeOptLevel`]).
    pub opt_level: NativeOptLevel,
}

impl Default for AndroidConfig {
    fn default() -> Self {
        AndroidConfig {
            abis: vec!["arm64-v8a".to_owned(), "x86_64".to_owned()],
            min_sdk: 26,
            opt_level: NativeOptLevel::default(),
        }
    }
}

/// What an iOS or Android release build optimises for: `opt_level` in `[ios]` and in `[android]` of
/// `undra.toml` (ADR-052, amendment "native size gates"). Each is the profile of the generated crate
/// that `undra build --release` builds for that platform; a debug build is cargo's dev profile
/// whatever this says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NativeOptLevel {
    /// `"s"`, the default: the `release-mobile` profile, `opt-level = "s"` with the crates of the call
    /// path (`undra-wire`, `undra-signals`, `undra-runtime`, `undra-ffi`) at 3.
    #[default]
    Small,
    /// `"z"`: the `release-mobile-z` profile, `opt-level = "z"` for every crate, the call path
    /// included: the smallest library, and a synchronous call about half again as slow.
    Smallest,
    /// `"3"`: the `release` profile, `opt-level = 3` for every crate: the fastest and the largest
    /// (what every native release build was before the size-tuned profile).
    Fastest,
}

impl NativeOptLevel {
    /// The value as `undra.toml` spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NativeOptLevel::Small => "s",
            NativeOptLevel::Smallest => "z",
            NativeOptLevel::Fastest => "3",
        }
    }

    /// Reads `"s"`, `"z"` or `"3"` (also the integer `3`).
    fn from_value(value: &Value) -> Option<NativeOptLevel> {
        match value {
            Value::Str(s) if s == "s" => Some(NativeOptLevel::Small),
            Value::Str(s) if s == "z" => Some(NativeOptLevel::Smallest),
            Value::Str(s) if s == "3" => Some(NativeOptLevel::Fastest),
            Value::Int(3) => Some(NativeOptLevel::Fastest),
            _ => None,
        }
    }
}

/// Web build settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebConfig {
    /// `z` (smallest) or `s` (small): the `opt-level` of the wasm build (SPEC 7: "measured").
    pub opt_level: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            opt_level: "z".to_owned(),
        }
    }
}

/// Where the platform runtimes are found, when not derived from `[undra] path`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RuntimesConfig {
    /// The directory of the `UndraRuntime` Swift package.
    pub swift: Option<String>,
    /// The directory of the Kotlin runtime Gradle build (`runtimes/kotlin/undra-runtime`).
    pub kotlin: Option<String>,
    /// The directory of the `@undra/runtime` npm package.
    pub ts: Option<String>,
}

/// The contents of `undra.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfig {
    /// The project name (`todo-app`).
    pub name: String,
    /// The application id / bundle identifier (`com.example.todo`).
    pub id: String,
    /// The platforms the app targets.
    pub platforms: Vec<Platform>,
    /// The core crate's directory, relative to the project.
    pub core_path: String,
    /// The core's Cargo package name, when it cannot be found by looking at the directory.
    pub core_package: Option<String>,
    /// The core's namespace (ADR-044): it names the core's one C export (`<namespace>_undra_api`),
    /// its libraries (`lib<namespace>.so`, `<namespace>.wasm`, ...) and the generated entry point
    /// (`Undra<Namespace>`). `None`: the core's package name in snake case.
    pub core_namespace: Option<String>,
    /// Where `undra bindgen` writes, relative to the project.
    pub generated: String,
    /// Where `undra build` writes artifacts, relative to the project.
    pub build: String,
    /// A checkout of the Undra repository the crates and runtimes come from (relative to the
    /// project); absent when they come from a release.
    pub undra_path: Option<String>,
    /// The Undra release the project's dependencies name (`1.0.0`; a two-part value an older `undra init` wrote means
    /// `<line>.0`).
    pub undra_version: String,
    /// Binding names.
    pub bindings: BindingsConfig,
    /// iOS settings.
    pub ios: IosConfig,
    /// Android settings.
    pub android: AndroidConfig,
    /// Web settings.
    pub web: WebConfig,
    /// Runtime locations.
    pub runtimes: RuntimesConfig,
}

/// The guide to the lint exclusion files and to the root-configuration lines that replace them (`lint_exclusions = "none"`).
pub const LINT_DOCS: &str = "https://shreypdev.github.io/undra/docs/bazel.html#lint-everything";

/// The Undra release this CLI belongs to, in full (`1.0.0`): what `undra init` pins every dependency of a project to and
/// writes as `[undra] version` (ADR-063: the crates' tag, the Swift package, the Kotlin artifacts and the npm tarballs all
/// name this one release). It is the workspace version, so a release never leaves a scaffold asking for the previous one.
pub const UNDRA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The git tag of this CLI's release, `v<version>`: what `undra init` pins the core's `undra`
/// dependency to, so the project uses the crates the CLI was released with.
pub const UNDRA_RELEASE_TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));

impl ProjectConfig {
    /// A configuration with every default, for a project called `name`.
    #[must_use]
    pub fn new(name: &str, id: &str, platforms: Vec<Platform>) -> ProjectConfig {
        ProjectConfig {
            name: name.to_owned(),
            id: id.to_owned(),
            platforms,
            core_path: "core".to_owned(),
            core_package: None,
            core_namespace: None,
            generated: "generated".to_owned(),
            build: "build".to_owned(),
            undra_path: None,
            undra_version: UNDRA_VERSION.to_owned(),
            bindings: BindingsConfig::default(),
            ios: IosConfig::default(),
            android: AndroidConfig::default(),
            web: WebConfig::default(),
            runtimes: RuntimesConfig::default(),
        }
    }

    /// Parses `text`, read from `file` (used in messages).
    ///
    /// # Errors
    ///
    /// A syntax error, an unknown table or key, a value of the wrong type, or a missing
    /// required key.
    pub fn parse(text: &str, file: &Path) -> Result<ProjectConfig> {
        let doc = toml_lite::parse(text).map_err(|e| {
            CliError::bad_config(file, e.to_string(), "fix the line named above; undra.toml is TOML with tables, strings, booleans, integers and arrays")
        })?;
        let reader = Reader { doc: &doc, file };
        reader.check_tables(&[
            "project", "core", "paths", "undra", "bindings", "ios", "android", "web", "runtimes",
        ])?;
        if let Some(root) = doc.tables.get("").filter(|t| !t.is_empty()) {
            let (key, entry) = root.iter().next().expect("not empty");
            return Err(CliError::bad_config(
                file,
                format!("line {}: `{key}` is outside any table", entry.line),
                "put it under the right [table], for example `name` belongs under [project]",
            ));
        }

        reader.check_keys("project", &["name", "id", "platforms"])?;
        let name = reader.require_str("project", "name")?;
        let id = reader.require_str("project", "id")?;
        let platforms = match reader.get("project", "platforms") {
            Some(entry) => {
                let list = reader.as_str_list(entry, "project", "platforms")?;
                Platform::parse_list(&list.join(",")).map_err(|e| {
                    CliError::bad_config(file, format!("line {}: {}", entry.line, e.what), e.fix)
                })?
            }
            None => Platform::ALL.to_vec(),
        };

        let mut cfg = ProjectConfig::new(&name, &id, platforms);

        reader.check_keys("core", &["path", "package", "namespace"])?;
        if let Some(v) = reader.opt_str("core", "path")? {
            cfg.core_path = v;
        }
        cfg.core_package = reader.opt_str("core", "package")?;
        if let Some(entry) = reader.get("core", "namespace") {
            let Value::Str(namespace) = &entry.value else {
                return Err(CliError::bad_config(
                    file,
                    format!("line {}: namespace must be a string", entry.line),
                    "write it in quotes: namespace = \"acme_pay\"",
                ));
            };
            if let Err(why) = check_namespace(namespace) {
                return Err(CliError::bad_config(
                    file,
                    format!(
                        "line {}: `{namespace}` is not a core namespace: {why}",
                        entry.line
                    ),
                    "use lowercase letters, digits and `_`, starting with a letter, at most 32 characters (for example \"acme_pay\")",
                ));
            }
            cfg.core_namespace = Some(namespace.clone());
        }

        reader.check_keys("paths", &["generated", "build"])?;
        if let Some(v) = reader.opt_str("paths", "generated")? {
            cfg.generated = v;
        }
        if let Some(v) = reader.opt_str("paths", "build")? {
            cfg.build = v;
        }

        reader.check_keys("undra", &["path", "version"])?;
        cfg.undra_path = reader.opt_str("undra", "path")?;
        if let Some(v) = reader.opt_str("undra", "version")? {
            cfg.undra_version = v;
        }

        reader.check_keys(
            "bindings",
            &[
                "swift_module",
                "kotlin_package",
                "ts_scope",
                "ts_package",
                "ts_js_number",
                "swift_typed_throws",
                "swift_observation",
                "lint_exclusions",
            ],
        )?;
        cfg.bindings = BindingsConfig {
            swift_module: reader.opt_str("bindings", "swift_module")?,
            kotlin_package: reader.opt_str("bindings", "kotlin_package")?,
            ts_scope: reader.opt_str("bindings", "ts_scope")?,
            ts_package: reader.opt_str("bindings", "ts_package")?,
            ts_js_number: reader
                .opt_bool("bindings", "ts_js_number")?
                .unwrap_or(false),
            swift_typed_throws: reader.opt_bool("bindings", "swift_typed_throws")?,
            swift_observation: match reader.get("bindings", "swift_observation") {
                None => None,
                Some(entry) => match &entry.value {
                    Value::Str(text) => Some(text.parse::<SwiftObservation>().map_err(|e| {
                        CliError::bad_config(
                            file,
                            format!("line {}: {e}", entry.line),
                            "write swift_observation = \"observation\" (`@Observable`, iOS 17 and later) or \"observable-object\" (`ObservableObject`, iOS 15 and later), or leave it out to follow [ios] deployment_target",
                        )
                    })?),
                    _ => {
                        return Err(CliError::bad_config(
                            file,
                            format!("line {}: swift_observation must be a string", entry.line),
                            "write it in quotes: swift_observation = \"observable-object\"",
                        ));
                    }
                },
            },
            lint_exclusions: match reader.get("bindings", "lint_exclusions") {
                None => LintExclusions::default(),
                Some(entry) => match &entry.value {
                    Value::Str(text) => text.parse::<LintExclusions>().map_err(|e| {
                        CliError::new(
                            Code::BadConfig,
                            format!("{}: line {}: {e}", file.display(), entry.line),
                            "`undra bindgen` writes the files each linter reads to exclude the generated bindings (.editorconfig, .swiftlint.yml, .eslintrc.json, eslint.config.undra.mjs) beside the trees, or none of them when your linters are configured at the repository root; any other value would make it guess which",
                            format!("write lint_exclusions = \"beside\" (the default) or lint_exclusions = \"none\" in [bindings]; with \"none\", add the exclusions to your root linter configuration: {LINT_DOCS}"),
                        )
                    })?,
                    _ => {
                        return Err(CliError::new(
                            Code::BadConfig,
                            format!("{}: line {}: lint_exclusions must be a string", file.display(), entry.line),
                            "the setting decides whether the files that keep your linters out of the generated bindings are written, `beside` or `none`, and a number or a boolean names neither",
                            format!("write it in quotes: lint_exclusions = \"none\" ({LINT_DOCS})"),
                        ));
                    }
                },
            },
        };

        reader.check_keys(
            "ios",
            &["deployment_target", "simulator_archs", "opt_level"],
        )?;
        if let Some(level) = reader.native_opt_level("ios")? {
            cfg.ios.opt_level = level;
        }
        if let Some(entry) = reader.get("ios", "deployment_target") {
            let Value::Str(target) = &entry.value else {
                return Err(CliError::bad_config(
                    file,
                    format!("line {}: deployment_target must be a string", entry.line),
                    "write it in quotes: deployment_target = \"15.0\"",
                ));
            };
            check_ios_target(target)
                .map_err(|e| in_file(file, format!("line {}: {}", entry.line, e.what), e))?;
            cfg.ios.deployment_target.clone_from(target);
        }
        // `observation` with a floor below iOS 17 would generate code that does not compile (ADR-045).
        cfg.swift_observation(None)
            .map_err(|e| in_file(file, e.what.clone(), e))?;
        if let Some(entry) = reader.get("ios", "simulator_archs") {
            let archs = reader.as_str_list(entry, "ios", "simulator_archs")?;
            for arch in &archs {
                if arch != "arm64" && arch != "x86_64" {
                    return Err(CliError::bad_config(
                        file,
                        format!(
                            "line {}: `{arch}` is not an iOS simulator architecture",
                            entry.line
                        ),
                        "use \"arm64\" and/or \"x86_64\"",
                    ));
                }
            }
            if archs.is_empty() {
                return Err(CliError::bad_config(
                    file,
                    format!("line {}: simulator_archs is empty", entry.line),
                    "list at least one of \"arm64\", \"x86_64\"",
                ));
            }
            cfg.ios.simulator_archs = archs;
        }

        reader.check_keys("android", &["abis", "min_sdk", "opt_level"])?;
        if let Some(level) = reader.native_opt_level("android")? {
            cfg.android.opt_level = level;
        }
        if let Some(entry) = reader.get("android", "abis") {
            let abis = reader.as_str_list(entry, "android", "abis")?;
            for abi in &abis {
                if !["arm64-v8a", "x86_64", "armeabi-v7a", "x86"].contains(&abi.as_str()) {
                    return Err(CliError::bad_config(
                        file,
                        format!("line {}: `{abi}` is not an Android ABI", entry.line),
                        "use \"arm64-v8a\", \"x86_64\", \"armeabi-v7a\" or \"x86\"",
                    ));
                }
            }
            if abis.is_empty() {
                return Err(CliError::bad_config(
                    file,
                    format!("line {}: abis is empty", entry.line),
                    "list at least one ABI, for example \"arm64-v8a\"",
                ));
            }
            cfg.android.abis = abis;
        }
        if let Some(entry) = reader.get("android", "min_sdk") {
            match entry.value {
                Value::Int(n) if (21..=99).contains(&n) => {
                    cfg.android.min_sdk = u32::try_from(n).expect("in range")
                }
                _ => {
                    return Err(CliError::bad_config(
                        file,
                        format!(
                            "line {}: min_sdk must be an integer API level (21 or more)",
                            entry.line
                        ),
                        "Undra targets API 26 and up (SPEC 0); use 26 unless you know your users need less",
                    ));
                }
            }
        }

        reader.check_keys("web", &["opt_level"])?;
        if let Some(entry) = reader.get("web", "opt_level") {
            match &entry.value {
                Value::Str(s) if s == "z" || s == "s" => cfg.web.opt_level.clone_from(s),
                _ => {
                    return Err(CliError::bad_config(
                        file,
                        format!("line {}: opt_level must be \"z\" or \"s\"", entry.line),
                        "SPEC 7 allows opt-level z (smallest) or s (small); measure both with `undra build --platform web`",
                    ));
                }
            }
        }

        reader.check_keys("runtimes", &["swift", "kotlin", "ts"])?;
        cfg.runtimes = RuntimesConfig {
            swift: reader.opt_str("runtimes", "swift")?,
            kotlin: reader.opt_str("runtimes", "kotlin")?,
            ts: reader.opt_str("runtimes", "ts")?,
        };
        Ok(cfg)
    }

    /// The major version of `[ios] deployment_target`: the floor the generated Swift is written for (ADR-045).
    #[must_use]
    pub fn ios_floor(&self) -> u32 {
        ios_major(&self.ios.deployment_target).unwrap_or(SwiftObservation::OBSERVATION_MIN_IOS)
    }

    /// How the generated Swift stores are observed: `requested` (the command line's
    /// `--swift-observation`), else `[bindings] swift_observation`, else what the deployment target allows
    /// (`@Observable` from iOS 17, `ObservableObject` below).
    ///
    /// # Errors
    ///
    /// `observation` with a deployment target below iOS 17.0: Observation does not exist there, so the
    /// generated stores would not compile. The message names both settings.
    pub fn swift_observation(
        &self,
        requested: Option<SwiftObservation>,
    ) -> Result<SwiftObservation> {
        let floor = self.ios_floor();
        let chosen = requested
            .or(self.bindings.swift_observation)
            .unwrap_or_else(|| SwiftObservation::for_ios(floor));
        if chosen == SwiftObservation::Observation && floor < SwiftObservation::OBSERVATION_MIN_IOS
        {
            let from = if requested.is_some() {
                "`--swift-observation observation`"
            } else {
                "`swift_observation = \"observation\"` in [bindings]"
            };
            return Err(CliError::bad_argument(
                format!(
                    "{from} needs iOS 17, but `deployment_target = \"{}\"` in [ios] is lower",
                    self.ios.deployment_target
                ),
                "`@Observable` is Observation, which is iOS 17.0 and later; the generated stores would not compile for an older iOS",
                "raise [ios] deployment_target to \"17.0\", or use swift_observation = \"observable-object\" (the default below 17.0: `ObservableObject` stores with `@Published` properties, which work from iOS 15, docs/IOS_15_16.md)",
            ));
        }
        Ok(chosen)
    }

    /// The text `undra init` writes: every setting that matters, with comments.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let platforms = self
            .platforms
            .iter()
            .map(|p| quote(p.name()))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "# undra.toml: this file makes the directory an Undra project (`undra --help` for the commands).\n\
             \n\
             [project]\n\
             name = {}\n\
             # The Android applicationId, the iOS bundle identifier and the base of the Kotlin package.\n\
             id = {}\n\
             platforms = [{platforms}]\n\
             \n\
             [core]\n\
             # The crate `undra bindgen`, `undra build` and `undra dev` work on.\n\
             path = {}",
            quote(&self.name),
            quote(&self.id),
            quote(&self.core_path),
        );
        if let Some(package) = &self.core_package {
            let _ = writeln!(out, "package = {}", quote(package));
        }
        match &self.core_namespace {
            Some(namespace) => {
                let _ = writeln!(
                    out,
                    "# Names the core's symbol, libraries and generated entry point; unique per app.\n\
                     namespace = {}",
                    quote(namespace)
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "# namespace = \"todo\"   # names the core's symbol, libraries and entry point; default: the crate name"
                );
            }
        }
        let _ = writeln!(
            out,
            "\n[paths]\n\
             # Written by `undra bindgen` (Swift package, Gradle module, npm package). Commit it or\n\
             # regenerate it in CI with `undra bindgen --check`.\n\
             generated = {}\n\
             # Written by `undra build` (XCFramework, jniLibs, wasm). Do not commit.\n\
             build = {}",
            quote(&self.generated),
            quote(&self.build),
        );
        let _ = writeln!(
            out,
            "\n[undra]\n\
             # Where Undra comes from. `path` is a checkout of the Undra repository (crates and\n\
             # runtimes are used from there); without it, the release `version` names is used: the\n\
             # crates by git tag (see core/Cargo.toml), the runtimes from the same release (ADR-063).\n\
             version = {}",
            quote(&self.undra_version)
        );
        if let Some(path) = &self.undra_path {
            let _ = writeln!(out, "path = {}", quote(path));
        }
        let _ = writeln!(
            out,
            "\n[bindings]\n\
             # Names of the generated code. Defaults are derived from the core crate's name.\n\
             # swift_module = \"TodoCore\"\n\
             # kotlin_package = \"com.example.todo.core\"\n\
             # ts_scope = \"app\"\n\
             # ts_js_number = false        # i64/u64 as `number` instead of `bigint`\n\
             # swift_typed_throws = true   # port requirements: `throws(E)`; false emits plain `throws`\n\
             # swift_observation = \"observable-object\"   # Swift stores: \"observation\" (@Observable, iOS 17+) or \"observable-object\" (iOS 15+); default follows [ios] deployment_target\n\
             # lint_exclusions = \"none\"   # \"beside\" (the default) writes .editorconfig, .swiftlint.yml and the ESLint files beside the trees; \"none\" writes none: add the lines to your root linter config ({LINT_DOCS})"
        );
        if let Some(v) = &self.bindings.swift_module {
            let _ = writeln!(out, "swift_module = {}", quote(v));
        }
        if let Some(v) = &self.bindings.kotlin_package {
            let _ = writeln!(out, "kotlin_package = {}", quote(v));
        }
        if let Some(v) = &self.bindings.ts_scope {
            let _ = writeln!(out, "ts_scope = {}", quote(v));
        }
        if let Some(v) = &self.bindings.ts_package {
            let _ = writeln!(out, "ts_package = {}", quote(v));
        }
        if self.bindings.ts_js_number {
            let _ = writeln!(out, "ts_js_number = true");
        }
        if let Some(v) = self.bindings.swift_typed_throws {
            let _ = writeln!(out, "swift_typed_throws = {v}");
        }
        if let Some(v) = self.bindings.swift_observation {
            let _ = writeln!(out, "swift_observation = {}", quote(v.as_str()));
        }
        if self.bindings.lint_exclusions != LintExclusions::default() {
            let _ = writeln!(
                out,
                "lint_exclusions = {}",
                quote(self.bindings.lint_exclusions.as_str())
            );
        }
        let archs = self
            .ios
            .simulator_archs
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(", ");
        let abis = self
            .android
            .abis
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(", ");
        // `opt_level` of a mobile platform: the line is written commented out while it is the default, so
        // the file shows the choice without making it.
        let native_opt_level = |level: NativeOptLevel| {
            format!(
                "{}opt_level = {}   # release builds: \"s\" small (the default), \"z\" smallest (calls about 1.5x slower), \"3\" fastest",
                if level == NativeOptLevel::default() {
                    "# "
                } else {
                    ""
                },
                quote(level.as_str())
            )
        };
        let _ = writeln!(
            out,
            "\n[ios]\n\
             # The lowest iOS the app runs on, 15.0 or later. From 17.0 the generated Swift stores are `@Observable`;\n\
             # below it they are `ObservableObject`s (docs/IOS_15_16.md).\n\
             deployment_target = {}\n\
             # Simulator slices of the XCFramework. Add \"x86_64\" for Intel Macs.\n\
             simulator_archs = [{archs}]\n\
             {}\n\
             \n\
             [android]\n\
             abis = [{abis}]\n\
             min_sdk = {}\n\
             {}\n\
             \n\
             [web]\n\
             # \"z\" is smallest, \"s\" is small and a little faster: measure both.\n\
             opt_level = {}",
            quote(&self.ios.deployment_target),
            native_opt_level(self.ios.opt_level),
            self.android.min_sdk,
            native_opt_level(self.android.opt_level),
            quote(&self.web.opt_level),
        );
        let runtimes = [
            ("swift", &self.runtimes.swift),
            ("kotlin", &self.runtimes.kotlin),
            ("ts", &self.runtimes.ts),
        ];
        if runtimes.iter().any(|(_, v)| v.is_some()) {
            let _ = writeln!(
                out,
                "\n[runtimes]\n# Overrides for where each platform runtime lives (relative to this file)."
            );
            for (key, value) in runtimes {
                if let Some(v) = value {
                    let _ = writeln!(out, "{key} = {}", quote(v));
                }
            }
        }
        out
    }
}

/// `problem` (a diagnostic about a setting, with its own why and fix) as a `C0002` about `file`: the file's name in front
/// of what is wrong, and the why and the fix kept (`CliError::bad_config` would replace the why with its general one).
fn in_file(file: &Path, what: String, problem: CliError) -> CliError {
    CliError::new(
        Code::BadConfig,
        format!("{}: {what}", file.display()),
        problem.why,
        problem.fix,
    )
}

/// The lowest iOS version the Swift runtime supports (ADR-045), as `[ios] deployment_target`.
pub const MIN_IOS_TARGET: &str = "15.0";

/// The major version of an iOS deployment target (`"15.0"` is 15, `"17"` is 17), or `None` when the text is
/// not `<major>[.<minor>[.<patch>]]`.
#[must_use]
pub fn ios_major(target: &str) -> Option<u32> {
    // Digits only: `u32::from_str` also takes a leading `+`, which Xcode and cargo's IPHONEOS_DEPLOYMENT_TARGET do not.
    let numbers: Option<Vec<u32>> = target
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect();
    let numbers = numbers?;
    if numbers.len() > 3 {
        return None;
    }
    numbers.first().copied()
}

/// Checks `[ios] deployment_target`: a version, iOS 15.0 or later (the floor of the Swift runtime, ADR-045).
///
/// # Errors
///
/// What is wrong with it and the fix.
pub fn check_ios_target(target: &str) -> Result<()> {
    match ios_major(target) {
        None => Err(CliError::bad_argument(
            format!("`{target}` is not an iOS version"),
            "the deployment target is passed to Xcode and to the core's build as IPHONEOS_DEPLOYMENT_TARGET",
            "write the version as in Xcode: \"15.0\", \"16.0\", \"17.0\"",
        )),
        Some(major) if major < SwiftObservation::RUNTIME_MIN_IOS => Err(CliError::bad_argument(
            format!("iOS {target} is below the lowest iOS Undra supports, {MIN_IOS_TARGET}"),
            "the Swift runtime uses concurrency and the Combine framework, and the generated stores need ObservableObject at the very least (ADR-045)",
            format!("set deployment_target = \"{MIN_IOS_TARGET}\" (or later) in [ios]"),
        )),
        Some(_) => Ok(()),
    }
}

/// The longest core namespace (ADR-044): it is part of a C symbol and of file names.
pub const MAX_NAMESPACE_LEN: usize = 32;

/// Checks a core namespace (`[core] namespace`, ADR-044): a lowercase C identifier of 1 to 32
/// bytes that starts with a letter. Lowercase only, so that `lib<namespace>.so` and
/// `<namespace>.wasm` never differ by case alone on a case-insensitive file system. Its generated
/// entry point, `Undra<Namespace>`, must not be a name the runtimes or the generated bindings
/// declare (`core` would make it `UndraCore`; `undra_bindgen::naming::RESERVED_ENTRIES`).
///
/// # Errors
///
/// What is wrong with it, in words.
pub fn check_namespace(namespace: &str) -> std::result::Result<(), String> {
    if namespace.is_empty() {
        return Err("it is empty".to_owned());
    }
    if namespace.len() > MAX_NAMESPACE_LEN {
        return Err(format!(
            "it is {} characters long, and a namespace has at most {MAX_NAMESPACE_LEN}",
            namespace.len()
        ));
    }
    if !namespace.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err("it must start with a lowercase letter".to_owned());
    }
    if let Some(c) = namespace
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_'))
    {
        return Err(format!(
            "`{c}` is not allowed (lowercase letters, digits and `_` only)"
        ));
    }
    let names = undra_bindgen::naming::CoreNames::new(namespace);
    if names.entry_is_reserved() {
        return Err(format!(
            "its generated entry point would be `{}`, a name the Undra runtimes or the generated \
             bindings already declare",
            names.entry()
        ));
    }
    Ok(())
}

struct Reader<'a> {
    doc: &'a Document,
    file: &'a Path,
}

impl Reader<'_> {
    fn get(&self, table: &str, key: &str) -> Option<&Entry> {
        self.doc.tables.get(table).and_then(|t| t.get(key))
    }

    fn check_tables(&self, known: &[&str]) -> Result<()> {
        for name in self.doc.tables.keys() {
            if !name.is_empty() && !known.contains(&name.as_str()) {
                return Err(CliError::bad_config(
                    self.file,
                    format!("unknown table [{name}]"),
                    format!(
                        "undra.toml has these tables: {}",
                        known
                            .iter()
                            .map(|k| format!("[{k}]"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
        Ok(())
    }

    fn check_keys(&self, table: &str, known: &[&str]) -> Result<()> {
        if let Some(t) = self.doc.tables.get(table) {
            for (key, entry) in t {
                if !known.contains(&key.as_str()) {
                    return Err(CliError::bad_config(
                        self.file,
                        format!("line {}: unknown key `{key}` in [{table}]", entry.line),
                        format!("[{table}] has these keys: {}", known.join(", ")),
                    ));
                }
            }
        }
        Ok(())
    }

    fn wrong_type(&self, entry: &Entry, table: &str, key: &str, expected: &str) -> CliError {
        /// A value of the kind that was expected, as TOML.
        fn example_value(expected: &str) -> &'static str {
            match expected {
                "a string" => "\"text\"",
                "true or false" => "true",
                "an array of strings" => "[\"a\", \"b\"]",
                _ => "\"value\"",
            }
        }
        CliError::bad_config(
            self.file,
            format!(
                "line {}: `{key}` in [{table}] is {} but must be {expected}",
                entry.line,
                entry.value.describe()
            ),
            format!(
                "write it as {expected}, for example `{key} = {}`",
                example_value(expected)
            ),
        )
    }

    fn opt_str(&self, table: &str, key: &str) -> Result<Option<String>> {
        match self.get(table, key) {
            None => Ok(None),
            Some(Entry {
                value: Value::Str(s),
                ..
            }) => Ok(Some(s.clone())),
            Some(entry) => Err(self.wrong_type(entry, table, key, "a string")),
        }
    }

    /// `opt_level` of `[ios]` or `[android]`: `"s"`, `"z"` or `"3"` ([`NativeOptLevel`]).
    fn native_opt_level(&self, table: &str) -> Result<Option<NativeOptLevel>> {
        let Some(entry) = self.get(table, "opt_level") else {
            return Ok(None);
        };
        NativeOptLevel::from_value(&entry.value).map(Some).ok_or_else(|| {
            CliError::bad_config(
                self.file,
                format!(
                    "line {}: opt_level in [{table}] must be \"s\", \"z\" or \"3\"",
                    entry.line
                ),
                "\"s\" (the default) is small with the call path at full speed; \"z\" is the smallest, and a call is about \
                 half again as slow; \"3\" is the fastest and the largest (ADR-052, \"native size gates\")",
            )
        })
    }

    fn require_str(&self, table: &str, key: &str) -> Result<String> {
        self.opt_str(table, key)?.ok_or_else(|| {
            CliError::bad_config(
                self.file,
                format!("[{table}] has no `{key}`"),
                format!("add `{key} = \"...\"` under [{table}]"),
            )
        })
    }

    fn opt_bool(&self, table: &str, key: &str) -> Result<Option<bool>> {
        match self.get(table, key) {
            None => Ok(None),
            Some(Entry {
                value: Value::Bool(b),
                ..
            }) => Ok(Some(*b)),
            Some(entry) => Err(self.wrong_type(entry, table, key, "true or false")),
        }
    }

    fn as_str_list(&self, entry: &Entry, table: &str, key: &str) -> Result<Vec<String>> {
        let Value::Array(items) = &entry.value else {
            return Err(self.wrong_type(entry, table, key, "an array of strings"));
        };
        items
            .iter()
            .map(|item| match item {
                Value::Str(s) => Ok(s.clone()),
                _ => Err(self.wrong_type(entry, table, key, "an array of strings")),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Code;

    fn file() -> &'static Path {
        Path::new("undra.toml")
    }

    fn sample() -> ProjectConfig {
        let mut cfg = ProjectConfig::new(
            "todo-app",
            "com.example.todoapp",
            vec![Platform::Ios, Platform::Web],
        );
        cfg.undra_path = Some("../../undra".into());
        cfg.bindings.kotlin_package = Some("com.example.todoapp.core".into());
        cfg.bindings.swift_typed_throws = Some(false);
        cfg.ios.simulator_archs = vec!["arm64".into(), "x86_64".into()];
        cfg.android.opt_level = NativeOptLevel::Smallest;
        cfg.runtimes.ts = Some("../rt".into());
        cfg
    }

    #[test]
    fn render_and_parse_round_trip() {
        let cfg = sample();
        let text = cfg.render();
        assert_eq!(ProjectConfig::parse(&text, file()).unwrap(), cfg, "{text}");
    }

    fn with_ios(toml: &str) -> Result<ProjectConfig> {
        ProjectConfig::parse(
            &format!("[project]\nname = \"a\"\nid = \"com.example.a\"\n{toml}"),
            file(),
        )
    }

    #[test]
    fn the_ios_floor_decides_how_swift_stores_are_observed_unless_the_file_says_otherwise() {
        use SwiftObservation::{ObservableObject, Observation};
        let mode = |toml: &str| {
            let cfg = with_ios(toml).unwrap();
            (cfg.ios_floor(), cfg.swift_observation(None).unwrap())
        };
        assert_eq!(mode(""), (17, Observation));
        assert_eq!(
            mode("[ios]\ndeployment_target = \"17.2\"\n"),
            (17, Observation)
        );
        assert_eq!(
            mode("[ios]\ndeployment_target = \"16.0\"\n"),
            (16, ObservableObject)
        );
        assert_eq!(
            mode("[ios]\ndeployment_target = \"15.0\"\n"),
            (15, ObservableObject)
        );
        // The mode can be chosen on a newer floor, and a round trip keeps it.
        let cfg = with_ios("[bindings]\nswift_observation = \"observable-object\"\n").unwrap();
        assert_eq!(cfg.swift_observation(None).unwrap(), ObservableObject);
        assert_eq!(ProjectConfig::parse(&cfg.render(), file()).unwrap(), cfg);
        let floor = with_ios("[ios]\ndeployment_target = \"15.0\"\n").unwrap();
        assert_eq!(
            ProjectConfig::parse(&floor.render(), file()).unwrap(),
            floor
        );
    }

    #[test]
    fn observation_below_ios_17_is_an_error_that_names_both_settings() {
        let e = with_ios(
            "[bindings]\nswift_observation = \"observation\"\n[ios]\ndeployment_target = \"16.0\"\n",
        )
        .unwrap_err();
        let text = format!("{} {} {}", e.what, e.why, e.fix);
        assert!(
            text.contains("swift_observation") && text.contains("deployment_target"),
            "{text}"
        );
        assert!(
            text.contains("observable-object") && text.contains("17.0"),
            "{text}"
        );
        // Not an error when the floor allows it, nor when the mode is left to the floor.
        assert!(with_ios("[bindings]\nswift_observation = \"observation\"\n").is_ok());
        assert!(with_ios("[ios]\ndeployment_target = \"15.0\"\n").is_ok());
    }

    #[test]
    fn a_deployment_target_the_runtime_cannot_run_on_is_refused() {
        for (toml, needle) in [
            (
                "[ios]\ndeployment_target = \"14.0\"\n",
                "below the lowest iOS Undra supports, 15.0",
            ),
            (
                "[ios]\ndeployment_target = \"fifteen\"\n",
                "not an iOS version",
            ),
            ("[ios]\ndeployment_target = 15\n", "must be a string"),
            (
                "[bindings]\nswift_observation = \"combine\"\n",
                "not a Swift observation mode",
            ),
            ("[bindings]\nswift_observation = true\n", "must be a string"),
        ] {
            let e = with_ios(toml).unwrap_err();
            assert!(e.what.contains(needle), "{toml}: {e:?}");
        }
        assert_eq!(ios_major("15"), Some(15));
        assert_eq!(ios_major("16.4.1"), Some(16));
        assert_eq!(ios_major("16."), None);
        // Only what Xcode itself takes: digits, at most three parts, no sign, no spaces.
        for bad in [
            "+15", "-15", " 15", "15 ", "15.+1", "15.0.1.2", "", ".", "1e1", "15.x",
        ] {
            assert_eq!(ios_major(bad), None, "{bad:?}");
        }
    }

    /// R8 for the iOS floor settings, one row per input the review named: the diagnostic has a code, what is wrong
    /// (naming the setting and the value), why it matters (specific, not the general sentence of `bad_config`), a fix
    /// and the docs link, once through `undra.toml` and, for the command line's spelling, through `check_ios_target`.
    #[test]
    fn every_ios_floor_mistake_teaches_what_why_fix_and_where_to_read_more() {
        // (undra.toml text, what it must name, what the why must say, what the fix must offer)
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "[ios]\ndeployment_target = \"14.0\"\n",
                "iOS 14.0 is below the lowest iOS Undra supports, 15.0",
                "Combine",
                "deployment_target = \"15.0\"",
            ),
            (
                "[ios]\ndeployment_target = \"sixteen\"\n",
                "`sixteen` is not an iOS version",
                "IPHONEOS_DEPLOYMENT_TARGET",
                "\"16.0\"",
            ),
            (
                "[bindings]\nswift_observation = \"observation\"\n[ios]\ndeployment_target = \"16.4\"\n",
                "`swift_observation = \"observation\"` in [bindings] needs iOS 17, but `deployment_target = \"16.4\"` in [ios] is lower",
                "Observation, which is iOS 17.0 and later",
                "swift_observation = \"observable-object\"",
            ),
        ];
        for (toml, what, why, fix) in rows {
            let e = with_ios(toml).unwrap_err();
            assert_eq!(e.code, Code::BadConfig, "{toml}");
            assert!(
                e.what.contains("undra.toml") && e.what.contains(what),
                "{toml}: {e}"
            );
            assert!(e.why.contains(why), "{toml}: the why is `{}`", e.why);
            assert!(e.fix.contains(fix), "{toml}: the fix is `{}`", e.fix);
            let text = e.to_string();
            assert!(
                text.starts_with("error[undra::C0002]")
                    && text.contains("= note: ")
                    && text.contains("= help: ")
                    && text.contains("errors.html#C0002"),
                "{text}"
            );
        }
        // `observation` is not refused as such: on the first iOS that has it, it is the default and may be said outright.
        assert!(
            with_ios("[bindings]\nswift_observation = \"observation\"\n[ios]\ndeployment_target = \"17.0\"\n")
                .is_ok()
        );
        // Targets that are fine, whatever their spelling: a major alone, three parts, the first iOS with Observation.
        for ok in ["15", "15.0", "15.0.1", "16.4", "17.0", "26.0"] {
            assert!(
                with_ios(&format!("[ios]\ndeployment_target = \"{ok}\"\n")).is_ok(),
                "{ok}"
            );
        }
        // The command line says the same thing as a bad argument (C0009), with the same why.
        for (target, what) in [
            ("14.0", "below the lowest iOS"),
            ("x", "not an iOS version"),
        ] {
            let e = check_ios_target(target).unwrap_err();
            assert_eq!(e.code, Code::BadArgument);
            assert!(e.what.contains(what) && !e.why.is_empty() && !e.fix.is_empty());
            assert!(e.to_string().contains("errors.html#C0009"));
        }
        for ok in ["15", "15.0.1", "17.0"] {
            assert!(check_ios_target(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_lint_exclusions_are_written_beside_the_trees_unless_the_file_says_none() {
        let setting = |toml: &str| with_ios(toml).unwrap().bindings.lint_exclusions;
        assert_eq!(setting(""), LintExclusions::Beside);
        assert_eq!(
            setting("[bindings]\nswift_module = \"X\"\n"),
            LintExclusions::Beside
        );
        assert_eq!(
            setting("[bindings]\nlint_exclusions = \"beside\"\n"),
            LintExclusions::Beside
        );
        assert_eq!(
            setting("[bindings]\nlint_exclusions = \"none\"\n"),
            LintExclusions::None
        );
        assert_eq!(LintExclusions::Beside.as_str(), "beside");
        assert_eq!(LintExclusions::None.as_str(), "none");
    }

    #[test]
    fn the_default_is_a_comment_in_the_file_undra_init_writes_and_none_is_a_setting() {
        let default = with_ios("").unwrap();
        let text = default.render();
        assert!(
            text.contains("\n# lint_exclusions = \"none\"")
                && text.contains("\"beside\" (the default)"),
            "{text}"
        );
        assert!(!text.contains("\nlint_exclusions ="), "{text}");
        assert_eq!(ProjectConfig::parse(&text, file()).unwrap(), default);

        let none = with_ios("[bindings]\nlint_exclusions = \"none\"\n").unwrap();
        let text = none.render();
        assert!(text.contains("\nlint_exclusions = \"none\"\n"), "{text}");
        assert_eq!(ProjectConfig::parse(&text, file()).unwrap(), none);
    }

    #[test]
    fn a_wrong_lint_exclusions_value_teaches_what_why_fix_and_where_to_read_more() {
        for (toml, what) in [
            (
                "[bindings]\nlint_exclusions = \"all\"\n",
                "`all` is not a lint exclusions setting",
            ),
            (
                "[bindings]\nlint_exclusions = \"None\"\n",
                "`None` is not a lint exclusions setting",
            ),
            (
                "[bindings]\nlint_exclusions = \"\"\n",
                "`` is not a lint exclusions setting",
            ),
            ("[bindings]\nlint_exclusions = false\n", "must be a string"),
        ] {
            let e = with_ios(toml).unwrap_err();
            assert_eq!(e.code, Code::BadConfig, "{toml}");
            assert!(
                e.what.contains("undra.toml") && e.what.contains("line ") && e.what.contains(what),
                "{toml}: {e}"
            );
            assert!(e.why.contains("linter"), "{toml}: the why is `{}`", e.why);
            assert!(
                e.fix.contains("lint_exclusions = \"none\"")
                    && e.fix.contains("bazel.html#lint-everything"),
                "{toml}: the fix is `{}`",
                e.fix
            );
            let text = e.to_string();
            assert!(
                text.starts_with("error[undra::C0002]")
                    && text.contains("= note: ")
                    && text.contains("= help: ")
                    && text.contains("errors.html#C0002"),
                "{text}"
            );
        }
        // The key belongs to [bindings]: elsewhere it is an unknown key, as every typo is.
        assert!(with_ios("[paths]\nlint_exclusions = \"none\"\n").is_err());
    }

    #[test]
    fn minimal_file_gets_defaults() {
        let cfg = ProjectConfig::parse("[project]\nname = \"a\"\nid = \"com.example.a\"\n", file())
            .unwrap();
        assert_eq!(cfg.platforms, Platform::ALL.to_vec());
        assert_eq!(cfg.core_path, "core");
        assert_eq!(cfg.generated, "generated");
        assert_eq!(cfg.android.abis, ["arm64-v8a", "x86_64"]);
        assert_eq!(cfg.web.opt_level, "z");
        assert_eq!(cfg.ios.opt_level, NativeOptLevel::Small);
        assert_eq!(cfg.android.opt_level, NativeOptLevel::Small);
    }

    #[test]
    fn a_mobile_release_build_optimises_for_what_opt_level_says() {
        // ADR-052, "native size gates": "s" is the default, "z" and "3" are chosen per platform.
        let levels = |toml: &str| {
            let cfg = with_ios(toml).unwrap();
            (cfg.ios.opt_level, cfg.android.opt_level)
        };
        use NativeOptLevel::{Fastest, Small, Smallest};
        assert_eq!(levels(""), (Small, Small));
        assert_eq!(levels("[android]\nopt_level = \"z\"\n"), (Small, Smallest));
        assert_eq!(
            levels("[ios]\nopt_level = \"3\"\n[android]\nopt_level = 3\n"),
            (Fastest, Fastest)
        );
        assert_eq!(levels("[ios]\nopt_level = \"s\"\n"), (Small, Small));
        // The default is written commented out, a choice as a setting, and both read back.
        let default = with_ios("").unwrap();
        let text = default.render();
        assert!(text.contains("\n# opt_level = \"s\""), "{text}");
        assert!(!text.contains("\nopt_level = \"s\""), "{text}");
        let chosen = with_ios("[ios]\nopt_level = \"z\"\n[android]\nopt_level = \"3\"\n").unwrap();
        let text = chosen.render();
        assert!(text.contains("\nopt_level = \"z\""), "{text}");
        assert!(text.contains("\nopt_level = \"3\""), "{text}");
        assert_eq!(ProjectConfig::parse(&text, file()).unwrap(), chosen);
        assert_eq!(
            ProjectConfig::parse(&default.render(), file()).unwrap(),
            default
        );
    }

    #[test]
    fn mistakes_are_explained() {
        let cases = [
            ("[project]\nname = \"a\"\n", "no `id`"),
            (
                "[project]\nname = \"a\"\nid = \"b\"\nplatfroms = []\n",
                "unknown key `platfroms`",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[tooling]\n",
                "unknown table [tooling]",
            ),
            ("name = \"a\"\n", "outside any table"),
            (
                "[project]\nname = 3\nid = \"b\"\n",
                "is an integer but must be a string",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\nplatforms = [\"ios\", \"tv\"]\n",
                "`tv` is not a platform",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[android]\nabis = [\"mips\"]\n",
                "not an Android ABI",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[android]\nmin_sdk = 3\n",
                "min_sdk",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[web]\nopt_level = \"3\"\n",
                "opt_level",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[android]\nopt_level = \"0\"\n",
                "must be \"s\", \"z\" or \"3\"",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[ios]\nopt_level = 2\n",
                "opt_level in [ios]",
            ),
            (
                "[project]\nname = \"a\"\nid = \"b\"\n[ios]\nsimulator_archs = [\"ppc\"]\n",
                "not an iOS simulator",
            ),
            ("[project\n", "must end with `]`"),
        ];
        for (text, needle) in cases {
            let e = ProjectConfig::parse(text, file()).unwrap_err();
            assert_eq!(e.code, Code::BadConfig, "{text:?}");
            assert!(
                e.what.contains(needle) || e.fix.contains(needle),
                "{text:?}: {e}"
            );
        }
    }

    #[test]
    fn platform_lists() {
        assert_eq!(
            Platform::parse_list("web, ios,ios").unwrap(),
            vec![Platform::Ios, Platform::Web]
        );
        assert_eq!(Platform::parse("WASM").unwrap(), Platform::Web);
        assert!(Platform::parse_list("").is_err());
        assert!(Platform::parse_list("ios,tv").is_err());
    }

    #[test]
    fn namespaces_are_lowercase_c_identifiers_of_at_most_32_bytes() {
        for good in [
            "acme_pay",
            "a",
            "playground_core",
            "a1",
            &"n".repeat(32),
            "fn",
            "undra",
        ] {
            assert!(check_namespace(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            &"n".repeat(33),
            "acme-pay",
            "a.b",
            "a/b",
            "../x",
            "Acme",
            "_x",
            "1abc",
            "\u{e9}t\u{e9}",
            "a b",
            "a\0b",
        ] {
            assert!(check_namespace(bad).is_err(), "{bad:?}");
        }
    }

    /// Review (abi-table): the entry `Undra<Namespace>` is declared in every language beside the
    /// runtime's own types, so a namespace whose entry is one of them (`core` is `UndraCore`)
    /// would generate bindings that do not compile. Refused with the reason instead.
    #[test]
    fn a_namespace_whose_entry_is_a_runtime_or_generated_name_is_refused() {
        for (namespace, entry) in [
            ("core", "UndraCore"),
            ("ids", "UndraIds"),
            ("core_native", "UndraCoreNative"),
            ("core_entry", "UndraCoreEntry"),
            ("store", "UndraStore"),
            ("log", "UndraLog"),
            ("runtime", "UndraRuntime"),
            ("app", "UndraApp"),
        ] {
            let why = check_namespace(namespace).expect_err(namespace);
            assert!(why.contains(entry), "{namespace}: {why}");
        }
        assert!(check_namespace("core_app").is_ok());
    }
}
