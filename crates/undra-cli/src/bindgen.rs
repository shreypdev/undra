//! `undra bindgen`: the schema in, the Swift, Kotlin and TypeScript bindings out.
//!
//! The work is [`plan_files`], a pure function from a schema and a [`Plan`] to the files of the
//! three trees, so it is tested without a build. [`apply`] writes them and removes the files an
//! earlier run wrote that no longer exist (tracked in `.undra-generated`, so nothing the user put
//! next to them is ever deleted); [`check`] compares instead of writing, for CI.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use undra_bindgen::{BindgenError, GeneratedFile, Generator, SwiftObservation, provenance};
use undra_meta::Schema;

use crate::config::{LintExclusions, Platform, ProjectConfig, check_ios_target};
use crate::error::{CliError, Code, Result};
use crate::fsutil;
use crate::names::{portable, relative_path};
use crate::runtimes::{RuntimeRef, Runtimes};

/// The file listing what a previous run wrote, inside the output directory.
pub const MANIFEST: &str = ".undra-generated";

/// The attributes file at the root of each generated tree (`swift/`, `kotlin/`, `ts/`): it marks the
/// tree `linguist-generated` so GitHub collapses it in review (ADR-062). Its text is
/// [`provenance::GITATTRIBUTES`].
pub const ATTRIBUTES_FILE: &str = ".gitattributes";

/// Everything that decides what is generated besides the schema.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The generator configuration (module, package and scope names).
    pub generator: Generator,
    /// The languages to generate: iOS is Swift, Android is Kotlin, the web is TypeScript.
    pub platforms: Vec<Platform>,
    /// Where the runtimes are.
    pub runtimes: Runtimes,
    /// The output directory, absolute with symlinks resolved (relative paths to the runtimes are
    /// computed from it).
    pub out: PathBuf,
    /// Whether the lint exclusion files are written beside the trees (`[bindings] lint_exclusions`).
    pub lint_exclusions: LintExclusions,
}

impl Plan {
    /// The Swift package directory (`<out>/swift`).
    #[must_use]
    pub fn swift_dir(&self) -> PathBuf {
        self.out.join("swift")
    }
}

/// Points the Swift generator at the project's iOS floor (ADR-045): `[ios] deployment_target` (or
/// `ios_target`, the command line's override) decides how a store is observed and which type the wire
/// `Duration` is; `[bindings] swift_observation` (or `observation`, the command line's) overrides the mode.
///
/// # Errors
///
/// A deployment target below iOS 15, or `observation` for a floor below iOS 17: the settings are named in
/// the message.
pub fn configure_swift(
    generator: &mut Generator,
    config: &ProjectConfig,
    observation: Option<SwiftObservation>,
    ios_target: Option<&str>,
) -> Result<()> {
    let mut config = config.clone();
    if let Some(target) = ios_target {
        check_ios_target(target)?;
        config.ios.deployment_target = target.to_owned();
    }
    generator.swift_min_ios = config.ios_floor();
    generator.swift_observation = config.swift_observation(observation)?;
    Ok(())
}

/// Canonicalizes `path` even when its tail does not exist yet: the longest existing ancestor is
/// resolved and the rest appended.
#[must_use]
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = absolute.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            let mut out = resolved;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

/// Generates every file of the trees for `plan.platforms`, paths relative to `plan.out`.
///
/// # Errors
///
/// `C0007` with bindgen's own diagnostics when the schema cannot be turned into bindings.
pub fn plan_files(schema: &Schema, plan: &Plan) -> Result<Vec<GeneratedFile>> {
    let mut files = Vec::new();
    let prefixed = |dir: &str, generated: Vec<GeneratedFile>| -> Vec<GeneratedFile> {
        generated
            .into_iter()
            .map(|f| GeneratedFile {
                path: format!("{dir}/{}", f.path),
                contents: f.contents,
            })
            .collect()
    };
    // Each tree says it is generated to the code host (ADR-062): a `.gitattributes` at its root, so a
    // pull request collapses the tree and the reviewer reads the schema diff instead.
    let attributes = |dir: &str| GeneratedFile {
        path: format!("{dir}/{ATTRIBUTES_FILE}"),
        contents: provenance::GITATTRIBUTES.to_owned(),
    };
    if plan.platforms.contains(&Platform::Ios) {
        files.push(attributes("swift"));
        files.extend(prefixed(
            "swift",
            plan.generator.swift(schema).map_err(bindgen_failure)?,
        ));
        files.push(GeneratedFile {
            path: "swift/Package.swift".into(),
            contents: swift_package(
                &plan.generator.swift_module,
                &plan.generator.core_names().ffi_module(),
                &plan.runtimes.swift,
                &plan.swift_dir(),
                plan.generator.swift_min_ios,
            ),
        });
    }
    if plan.platforms.contains(&Platform::Android) {
        files.push(attributes("kotlin"));
        files.extend(prefixed(
            "kotlin",
            plan.generator.kotlin(schema).map_err(bindgen_failure)?,
        ));
        files.push(GeneratedFile {
            path: "kotlin/build.gradle.kts".into(),
            contents: kotlin_build(&plan.runtimes.kotlin),
        });
        files.push(GeneratedFile {
            path: "kotlin/.gitignore".into(),
            contents: KOTLIN_GITIGNORE.into(),
        });
    }
    if plan.platforms.contains(&Platform::Web) {
        files.push(attributes("ts"));
        files.extend(prefixed(
            "ts",
            ts_generator(&plan.generator, &plan.runtimes.ts)
                .typescript(schema)
                .map_err(bindgen_failure)?,
        ));
    }
    // The exclusions the repository's linters read, beside each tree (ADR-061), unless the project configures its linters
    // centrally (`lint_exclusions = "none"`). The `@file:Suppress` line of every Kotlin file is part of that file and stays.
    if plan.lint_exclusions == LintExclusions::Beside {
        files.extend(crate::lint::fragments(
            &plan.platforms,
            &plan.generator.swift_module,
            &plan.generator.core_names().ffi_module(),
        ));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The generator of the TypeScript tree. A released project's package asks for the runtime of the release it pins
/// (`[undra] version`, ADR-063), as its Swift package and Kotlin module do, so what `undra bindgen --check` compares does
/// not depend on which `undra` generated it; a checkout's asks for this generator's own release, the checkout's.
fn ts_generator<'a>(
    generator: &'a Generator,
    runtime: &RuntimeRef,
) -> std::borrow::Cow<'a, Generator> {
    match runtime {
        RuntimeRef::Path(_) => std::borrow::Cow::Borrowed(generator),
        RuntimeRef::Release { version, .. } => {
            let mut released = generator.clone();
            released.ts_runtime_range = format!("^{version}");
            std::borrow::Cow::Owned(released)
        }
    }
}

fn bindgen_failure(errors: Vec<BindgenError>) -> CliError {
    let detail = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CliError::new(
        Code::Bindgen,
        format!(
            "the schema cannot be turned into bindings ({} problem{})",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        ),
        "the public surface of the core has to be expressible in Swift, Kotlin and TypeScript, and these items are not",
        "change the items named below in the core (each message says what to do), then run `undra bindgen` again",
    )
    .with_detail(detail)
}

/// `Package.swift` of the generated Swift package: the bindings as a library that depends on the
/// `UndraRuntime` package and on `ffi_module`, the C target that declares the core's entry
/// (`<namespace>_undra_api`, ADR-044; the function itself is linked into the app with the core).
#[must_use]
pub fn swift_package(
    module: &str,
    ffi_module: &str,
    runtime: &RuntimeRef,
    package_dir: &Path,
    min_ios: u32,
) -> String {
    let (dependency, package_id) = match runtime {
        RuntimeRef::Path(dir) => {
            let rel = relative_path(package_dir, dir).unwrap_or_else(|| dir.clone());
            (
                format!(".package(path: \"{}\")", portable(&rel)),
                dir.file_name().map_or_else(
                    || "UndraRuntime".to_owned(),
                    |n| n.to_string_lossy().into_owned(),
                ),
            )
        }
        // ADR-063: the package at the root of the Undra repository, from the project's release on (SwiftPM reads the
        // `v<version>` tags); SwiftPM names it by the identity it derives from the URL.
        RuntimeRef::Release { version, dist } => (
            format!(".package(url: \"{}\", from: \"{version}\")", dist.git_url),
            dist.swift_package_identity(),
        ),
    };
    // The package's platform floor: iOS 17 / macOS 14 as ever, or the iOS 15 / 16 floor of ADR-045 (with the
    // macOS version that has the same APIs: Swift.Duration and the clocks are iOS 16 / macOS 13).
    let platforms = match min_ios {
        0..=15 => ".iOS(.v15), .macOS(.v12)",
        16 => ".iOS(.v16), .macOS(.v13)",
        _ => ".iOS(.v17), .macOS(.v14)",
    };
    format!(
        "// swift-tools-version: 6.0\n\
         // Generated by `undra bindgen`: do not edit, it is rewritten with the bindings.\n\
         import PackageDescription\n\
         \n\
         let package = Package(\n\
         \x20   name: \"{module}\",\n\
         \x20   platforms: [{platforms}],\n\
         \x20   products: [.library(name: \"{module}\", targets: [\"{module}\"])],\n\
         \x20   dependencies: [{dependency}],\n\
         \x20   targets: [\n\
         \x20       // Declares the core's entry point; the core itself is linked into the app.\n\
         \x20       .target(name: \"{ffi_module}\", path: \"Sources/{ffi_module}\"),\n\
         \x20       .target(\n\
         \x20           name: \"{module}\",\n\
         \x20           dependencies: [\n\
         \x20               \"{ffi_module}\",\n\
         \x20               .product(name: \"UndraRuntime\", package: \"{package_id}\"),\n\
         \x20           ],\n\
         \x20           path: \"Sources/{module}\"\n\
         \x20       ),\n\
         \x20   ],\n\
         \x20   swiftLanguageModes: [.v6]\n\
         )\n"
    )
}

/// `build.gradle.kts` of the generated Kotlin module: a JVM library that depends on the runtime.
/// The Android project includes it (`settings.gradle.kts`) and depends on it.
#[must_use]
pub fn kotlin_build(runtime: &RuntimeRef) -> String {
    let dependency = match runtime {
        RuntimeRef::Path(_) => "dev.undra:runtime:0.1.0-SNAPSHOT".to_owned(),
        RuntimeRef::Release { version, .. } => crate::dist::maven_coordinate("runtime", version),
    };
    format!(
        "// Generated by `undra bindgen`: do not edit, it is rewritten with the bindings.\n\
         //\n\
         // A plain JVM library (the bindings use no Android API). The Android project includes it\n\
         // as a module and depends on it; the runtime comes from a composite build of the Undra\n\
         // checkout or from the project's Undra release (ADR-063).\n\
         plugins {{\n\
         \x20   id(\"org.jetbrains.kotlin.jvm\")\n\
         }}\n\
         \n\
         java {{\n\
         \x20   sourceCompatibility = JavaVersion.VERSION_11\n\
         \x20   targetCompatibility = JavaVersion.VERSION_11\n\
         }}\n\
         \n\
         kotlin {{\n\
         \x20   compilerOptions {{\n\
         \x20       jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_11)\n\
         \x20   }}\n\
         }}\n\
         \n\
         dependencies {{\n\
         \x20   api(\"{dependency}\")\n\
         }}\n"
    )
}

/// `.gitignore` of the generated Kotlin module, written with it.
///
/// The module is a Gradle project, so building the app fills it with Gradle's own output
/// (`build/`, `.gradle/`, `.kotlin/`), which does not belong in the committed `generated/` tree.
/// The patterns are anchored to the module: an unanchored `build/` would also hide a package
/// directory called `build` (an app id like `com.acme.build.app`).
///
/// The CLI owns this file, not the Kotlin generator: the generator writes the sources of the
/// bindings, and the module around them (`build.gradle.kts`, this file) is what `undra` adds.
pub const KOTLIN_GITIGNORE: &str = "\
# Generated by `undra bindgen`: do not edit, it is rewritten with the bindings.
# Gradle's own output, from building the app that includes this module.
/build/
/.gradle/
/.kotlin/
";

/// What [`apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Files written (new or changed).
    pub written: Vec<String>,
    /// Files that were already up to date.
    pub unchanged: usize,
    /// Files removed because an earlier run wrote them and this one did not.
    pub removed: Vec<String>,
}

/// Writes `files` below `out` and removes what an earlier run wrote that this one did not.
///
/// # Errors
///
/// `C0010` when a file cannot be written.
pub fn apply(out: &Path, files: &[GeneratedFile]) -> Result<Applied> {
    fsutil::create_dir_all(out)?;
    let previous = read_manifest(out);
    let mut applied = Applied::default();
    for file in files {
        if fsutil::write_if_changed(&out.join(&file.path), &file.contents)? {
            applied.written.push(file.path.clone());
        } else {
            applied.unchanged += 1;
        }
    }
    let current: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    for old in previous.iter().filter(|p| !current.contains(p.as_str())) {
        let path = out.join(old);
        if path.is_file() && std::fs::remove_file(&path).is_ok() {
            applied.removed.push(old.clone());
            prune_empty_parents(out, &path);
        }
    }
    let manifest: Vec<&str> = current.into_iter().collect();
    fsutil::write_if_changed(&out.join(MANIFEST), &(manifest.join("\n") + "\n"))?;
    Ok(applied)
}

fn read_manifest(out: &Path) -> Vec<String> {
    std::fs::read_to_string(out.join(MANIFEST))
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.contains("..") && !l.starts_with('/'))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Removes directories left empty by deleting `file`, up to (not including) `root`.
fn prune_empty_parents(root: &Path, file: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Compares `files` with what is below `out`; returns one line per difference (empty: up to
/// date). Stale files of an earlier run count as differences.
#[must_use]
pub fn check(out: &Path, files: &[GeneratedFile]) -> Vec<String> {
    let mut problems = Vec::new();
    for file in files {
        match std::fs::read_to_string(out.join(&file.path)) {
            Ok(existing) if existing == file.contents => {}
            Ok(_) => problems.push(format!(
                "{} differs from what the schema generates",
                file.path
            )),
            Err(_) => problems.push(format!("{} is missing", file.path)),
        }
    }
    let current: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    for old in read_manifest(out) {
        if !current.contains(old.as_str()) && out.join(&old).is_file() {
            problems.push(format!(
                "{old} is stale (the schema no longer generates it)"
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use undra_meta::{FieldDef, RecordDef, TypeRef, ids};

    use super::*;
    use crate::dist::Dist;

    fn schema() -> Schema {
        let mut schema = Schema::new("demo-core");
        schema.records.push(RecordDef {
            name: "Todo".into(),
            type_id: ids::type_id("Todo"),
            fields: vec![FieldDef {
                name: "title".into(),
                ty: TypeRef::String,
                default: false,
                docs: String::new(),
            }],
            transparent: false,
            docs: String::new(),
        });
        schema
    }

    fn plan(platforms: Vec<Platform>, runtimes: Runtimes) -> Plan {
        let mut generator = Generator::for_crate("demo-core");
        generator.kotlin_package = "com.example.demo.core".into();
        Plan {
            generator,
            platforms,
            runtimes,
            out: PathBuf::from("/proj/generated"),
            lint_exclusions: LintExclusions::default(),
        }
    }

    #[test]
    fn each_platform_gets_its_tree_and_manifest() {
        let files = plan_files(
            &schema(),
            &plan(
                Platform::ALL.to_vec(),
                Runtimes::released("0.1.0", &Dist::github()),
            ),
        )
        .unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"swift/Package.swift"), "{paths:?}");
        assert!(
            paths.contains(&"swift/Sources/DemoCore/Generated/Types.swift"),
            "{paths:?}"
        );
        assert!(paths.contains(&"kotlin/build.gradle.kts"), "{paths:?}");
        assert!(
            paths.contains(&"kotlin/.gitignore"),
            "Gradle output must not land in the committed tree: {paths:?}"
        );
        assert!(
            paths.contains(&"kotlin/src/main/kotlin/com/example/demo/core/Types.kt"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"ts/src/types.ts") && paths.contains(&"ts/package.json"),
            "{paths:?}"
        );
        for lint in [
            "kotlin/.editorconfig",
            "swift/.swiftlint.yml",
            "ts/.eslintrc.json",
            "ts/eslint.config.undra.mjs",
        ] {
            assert!(
                paths.contains(&lint),
                "the lint exclusion {lint} is written with its tree: {paths:?}"
            );
        }
        assert!(paths.windows(2).all(|w| w[0] <= w[1]), "sorted");
    }

    #[test]
    fn each_tree_is_marked_generated_for_the_code_host() {
        // ADR-062: one `.gitattributes` per tree, the text bindgen owns, written (and checked, and
        // removed) with the rest.
        let files = plan_files(
            &schema(),
            &plan(
                Platform::ALL.to_vec(),
                Runtimes::released("0.1.0", &Dist::github()),
            ),
        )
        .unwrap();
        for tree in ["swift", "kotlin", "ts"] {
            let attributes = files
                .iter()
                .find(|f| f.path == format!("{tree}/.gitattributes"))
                .unwrap_or_else(|| panic!("{tree} has no .gitattributes"));
            assert_eq!(attributes.contents, provenance::GITATTRIBUTES);
        }
        assert!(
            provenance::GITATTRIBUTES.contains("linguist-generated=true"),
            "the host collapses what this marks"
        );
        // Only the chosen platforms' trees get one.
        let web = plan_files(
            &schema(),
            &plan(
                vec![Platform::Web],
                Runtimes::released("0.1.0", &Dist::github()),
            ),
        )
        .unwrap();
        let attributes: Vec<&str> = web
            .iter()
            .map(|f| f.path.as_str())
            .filter(|p| p.ends_with(".gitattributes"))
            .collect();
        assert_eq!(attributes, ["ts/.gitattributes"]);
    }

    #[test]
    fn the_kotlin_gitignore_is_anchored_to_the_module() {
        // `build/` alone would also ignore a package directory named `build`.
        let rules: Vec<&str> = KOTLIN_GITIGNORE
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect();
        assert_eq!(rules, ["/build/", "/.gradle/", "/.kotlin/"]);
    }

    #[test]
    fn only_the_chosen_platforms_are_generated() {
        let files = plan_files(
            &schema(),
            &plan(
                vec![Platform::Web],
                Runtimes::released("0.1.0", &Dist::github()),
            ),
        )
        .unwrap();
        assert!(files.iter().all(|f| f.path.starts_with("ts/")), "{files:?}");
    }

    #[test]
    fn the_swift_package_points_at_the_runtime_relative_to_itself() {
        let runtimes = Runtimes::in_repo(Path::new("/src/undra"));
        let text = swift_package(
            "DemoCore",
            "DemoCoreFFI",
            &runtimes.swift,
            Path::new("/proj/generated/swift"),
            17,
        );
        assert!(
            text.contains(".package(path: \"../../../src/undra/runtimes/swift/UndraRuntime\")"),
            "{text}"
        );
        assert!(text.contains("package: \"UndraRuntime\""), "{text}");
        assert!(text.contains("path: \"Sources/DemoCore\""), "{text}");
        // ADR-044: the C target declaring the core's entry, which the bindings depend on.
        assert!(
            text.contains(".target(name: \"DemoCoreFFI\", path: \"Sources/DemoCoreFFI\")"),
            "{text}"
        );
    }

    #[test]
    fn the_swift_package_declares_the_ios_floor_it_was_generated_for() {
        let runtimes = Runtimes::in_repo(Path::new("/src/undra"));
        let platforms = |min_ios: u32| {
            let text = swift_package(
                "DemoCore",
                "DemoCoreFFI",
                &runtimes.swift,
                Path::new("/proj/generated/swift"),
                min_ios,
            );
            text.lines()
                .find(|l| l.trim_start().starts_with("platforms:"))
                .unwrap()
                .trim()
                .to_owned()
        };
        assert_eq!(platforms(17), "platforms: [.iOS(.v17), .macOS(.v14)],");
        assert_eq!(platforms(18), "platforms: [.iOS(.v17), .macOS(.v14)],");
        assert_eq!(platforms(16), "platforms: [.iOS(.v16), .macOS(.v13)],");
        assert_eq!(platforms(15), "platforms: [.iOS(.v15), .macOS(.v12)],");
    }

    #[test]
    fn the_swift_floor_follows_the_deployment_target_and_the_overrides() {
        let config = |target: &str, mode: Option<SwiftObservation>| {
            let mut config = ProjectConfig::new("demo", "com.example.demo", vec![Platform::Ios]);
            config.ios.deployment_target = target.to_owned();
            config.bindings.swift_observation = mode;
            config
        };
        let configured = |config: &ProjectConfig, flag, target: Option<&str>| {
            let mut generator = Generator::for_crate("demo-core");
            configure_swift(&mut generator, config, flag, target)
                .map(|()| (generator.swift_observation, generator.swift_min_ios))
        };
        use SwiftObservation::{ObservableObject, Observation};
        assert_eq!(
            configured(&config("17.0", None), None, None).unwrap(),
            (Observation, 17)
        );
        assert_eq!(
            configured(&config("16.4", None), None, None).unwrap(),
            (ObservableObject, 16)
        );
        assert_eq!(
            configured(&config("15.0", None), None, None).unwrap(),
            (ObservableObject, 15)
        );
        // The mode can be asked for on an iOS 17 floor, in undra.toml and on the command line.
        let by_toml = config("17.0", Some(ObservableObject));
        assert_eq!(
            configured(&by_toml, None, None).unwrap(),
            (ObservableObject, 17)
        );
        assert_eq!(
            configured(&by_toml, Some(Observation), None).unwrap(),
            (Observation, 17)
        );
        // The command line's floor replaces the project's.
        assert_eq!(
            configured(&config("17.0", None), None, Some("15.0")).unwrap(),
            (ObservableObject, 15)
        );
        // `observation` below iOS 17 names both settings; so does a floor Undra does not support.
        let conflict = configured(&config("16.0", Some(Observation)), None, None).unwrap_err();
        assert!(
            conflict.what.contains("swift_observation")
                && conflict.what.contains("deployment_target"),
            "{conflict:?}"
        );
        assert!(
            conflict.fix.contains("observable-object") && conflict.fix.contains("17.0"),
            "{conflict:?}"
        );
        let flag = configured(&config("16.0", None), Some(Observation), None).unwrap_err();
        assert!(flag.what.contains("--swift-observation"), "{flag:?}");
        assert!(configured(&config("17.0", None), None, Some("14.0")).is_err());
        assert!(configured(&config("17.0", None), None, Some("seventeen")).is_err());
    }

    #[test]
    fn a_released_projects_typescript_package_asks_for_its_own_release() {
        // ADR-063: every manifest names the release `[undra] version` pins, whichever `undra` generated it.
        let package = |runtimes: Runtimes| -> serde_json::Value {
            let files = plan_files(&schema(), &plan(vec![Platform::Web], runtimes)).unwrap();
            let file = files.iter().find(|f| f.path == "ts/package.json").unwrap();
            serde_json::from_str(&file.contents).unwrap()
        };
        let released = package(Runtimes::released("7.8.9", &Dist::github()));
        assert_eq!(released["peerDependencies"]["@undra/runtime"], "^7.8.9");
        assert_eq!(released["devDependencies"]["@undra/runtime"], "^7.8.9");
        // A checkout's package asks for this generator's release: the checkout's own.
        let checkout = package(Runtimes {
            swift: RuntimeRef::Path(PathBuf::from("/k/swift")),
            kotlin: RuntimeRef::Path(PathBuf::from("/k/kotlin")),
            ts: RuntimeRef::Path(PathBuf::from("/k/ts")),
        });
        assert_eq!(
            checkout["peerDependencies"]["@undra/runtime"],
            undra_bindgen::RUNTIME_RANGE
        );
    }

    #[test]
    fn a_released_project_names_the_release_on_github() {
        let release = RuntimeRef::Release {
            version: "1.2.3".into(),
            dist: Dist::github(),
        };
        let swift = swift_package("DemoCore", "DemoCoreFFI", &release, Path::new("/x"), 17);
        assert!(
            swift
                .contains(".package(url: \"https://github.com/shreypdev/undra\", from: \"1.2.3\")")
                && swift.contains(".product(name: \"UndraRuntime\", package: \"undra\")"),
            "{swift}"
        );
        assert!(!swift.contains("undra-swift"), "{swift}");
        let kotlin = kotlin_build(&release);
        assert!(
            kotlin.contains("api(\"com.github.shreypdev.undra:runtime:v1.2.3\")"),
            "{kotlin}"
        );
        assert!(!kotlin.contains("Maven Central"), "{kotlin}");
        assert!(
            kotlin_build(&RuntimeRef::Path(PathBuf::from("/k")))
                .contains("api(\"dev.undra:runtime:0.1.0-SNAPSHOT\")")
        );
        // A mirror (ADR-063): the package's URL and identity follow it.
        let mirror = RuntimeRef::Release {
            version: "1.2.3".into(),
            dist: Dist {
                git_url: "file:///tmp/remote/undra.git".into(),
                ..Dist::github()
            },
        };
        let swift = swift_package("DemoCore", "DemoCoreFFI", &mirror, Path::new("/x"), 17);
        assert!(
            swift.contains(".package(url: \"file:///tmp/remote/undra.git\", from: \"1.2.3\")")
                && swift.contains("package: \"undra\")"),
            "{swift}"
        );
    }

    #[test]
    fn invalid_schemas_are_reported_with_bindgens_diagnostics() {
        let mut bad = schema();
        bad.records.push(bad.records[0].clone()); // a duplicate type name: E0050
        let e = plan_files(
            &bad,
            &plan(
                vec![Platform::Web],
                Runtimes::released("0.1.0", &Dist::github()),
            ),
        )
        .unwrap_err();
        assert_eq!(e.code, Code::Bindgen);
        assert!(
            e.detail.as_deref().unwrap_or_default().contains("E0050"),
            "{e}"
        );
    }

    /// The lint exclusion files of the three trees: what `lint_exclusions = "beside"` writes and `"none"` does not.
    const LINT_FILES: [&str; 4] = [
        "kotlin/.editorconfig",
        "swift/.swiftlint.yml",
        "ts/.eslintrc.json",
        "ts/eslint.config.undra.mjs",
    ];

    fn all_paths(lint_exclusions: LintExclusions) -> Vec<String> {
        let mut plan = plan(
            Platform::ALL.to_vec(),
            Runtimes::released("0.1.0", &Dist::github()),
        );
        plan.lint_exclusions = lint_exclusions;
        plan_files(&schema(), &plan)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect()
    }

    #[test]
    fn beside_writes_the_lint_exclusions_and_none_writes_nothing_else_missing() {
        let beside = all_paths(LintExclusions::Beside);
        let none = all_paths(LintExclusions::None);
        for file in LINT_FILES {
            assert!(beside.iter().any(|p| p == file), "{file} not in {beside:?}");
            assert!(!none.iter().any(|p| p == file), "{file} written with none");
        }
        // `none` removes exactly the four exclusion files: every other file is written as before, the `.gitattributes`
        // of each tree (ADR-062) and the Kotlin `.gitignore` included.
        let rest: Vec<&String> = beside
            .iter()
            .filter(|p| !LINT_FILES.contains(&p.as_str()))
            .collect();
        assert_eq!(rest, none.iter().collect::<Vec<_>>());
        for kept in [
            "swift/.gitattributes",
            "kotlin/.gitattributes",
            "ts/.gitattributes",
            "kotlin/.gitignore",
        ] {
            assert!(none.iter().any(|p| p == kept), "{kept} in {none:?}");
        }
        // The default of the setting is `beside`: a plan that says nothing writes what it always did.
        assert_eq!(LintExclusions::default(), LintExclusions::Beside);
    }

    #[test]
    fn the_kotlin_suppress_line_is_not_an_exclusion_file_and_stays_with_none() {
        let mut plan = plan(
            vec![Platform::Android],
            Runtimes::released("0.1.0", &Dist::github()),
        );
        plan.lint_exclusions = LintExclusions::None;
        let files = plan_files(&schema(), &plan).unwrap();
        let kotlin: Vec<&GeneratedFile> =
            files.iter().filter(|f| f.path.ends_with(".kt")).collect();
        assert!(!kotlin.is_empty());
        for file in kotlin {
            assert!(
                file.contents
                    .contains("@file:Suppress(\"ALL\", \"ktlint\")"),
                "{} lost its suppression",
                file.path
            );
        }
    }

    #[test]
    fn switching_to_none_removes_the_files_an_earlier_run_wrote_and_check_names_them_until_then() {
        let out = fsutil::unique_temp_dir("bindgen-lint-switch");
        let mut plan = plan(
            Platform::ALL.to_vec(),
            Runtimes::released("0.1.0", &Dist::github()),
        );
        let beside = plan_files(&schema(), &plan).unwrap();
        apply(&out, &beside).unwrap();
        assert!(check(&out, &beside).is_empty());
        for file in LINT_FILES {
            assert!(out.join(file).is_file(), "{file}");
        }

        plan.lint_exclusions = LintExclusions::None;
        let none = plan_files(&schema(), &plan).unwrap();
        let problems = check(&out, &none);
        assert_eq!(problems.len(), LINT_FILES.len(), "{problems:?}");
        assert!(
            problems.iter().all(|p| p.contains("is stale")),
            "{problems:?}"
        );

        let applied = apply(&out, &none).unwrap();
        assert_eq!(
            applied.removed.len(),
            LINT_FILES.len(),
            "{:?}",
            applied.removed
        );
        for file in LINT_FILES {
            assert!(!out.join(file).exists(), "{file} survived");
        }
        assert!(check(&out, &none).is_empty());
        // And back: the files return, and `--check` is clean again.
        plan.lint_exclusions = LintExclusions::Beside;
        let again = plan_files(&schema(), &plan).unwrap();
        apply(&out, &again).unwrap();
        assert!(check(&out, &again).is_empty());
        let _ = std::fs::remove_dir_all(out);
    }

    #[test]
    fn apply_writes_then_prunes_only_what_it_wrote() {
        let out = fsutil::unique_temp_dir("bindgen-apply");
        let file = |path: &str, contents: &str| GeneratedFile {
            path: path.into(),
            contents: contents.into(),
        };
        let first = apply(&out, &[file("ts/a.ts", "a"), file("ts/sub/b.ts", "b")]).unwrap();
        assert_eq!(first.written, ["ts/a.ts", "ts/sub/b.ts"]);
        // The user's own file next to the generated ones must survive.
        std::fs::write(out.join("ts/mine.ts"), "mine").unwrap();

        let second = apply(&out, &[file("ts/a.ts", "a")]).unwrap();
        assert_eq!(second.unchanged, 1);
        assert_eq!(second.removed, ["ts/sub/b.ts"]);
        assert!(
            !out.join("ts/sub").exists(),
            "the emptied directory is pruned"
        );
        assert!(out.join("ts/mine.ts").exists());
        assert!(check(&out, &[file("ts/a.ts", "a")]).is_empty());
        assert_eq!(check(&out, &[file("ts/a.ts", "changed")]).len(), 1);
        assert_eq!(
            check(&out, &[file("ts/a.ts", "a"), file("ts/new.ts", "n")]),
            ["ts/new.ts is missing"]
        );
        let _ = std::fs::remove_dir_all(out);
    }

    #[test]
    fn a_manifest_cannot_name_files_outside_the_output() {
        let out = fsutil::unique_temp_dir("bindgen-manifest");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(MANIFEST), "../escape\n/abs\nok.txt\n").unwrap();
        assert_eq!(read_manifest(&out), ["ok.txt"]);
        let _ = std::fs::remove_dir_all(out);
    }

    #[test]
    fn lenient_canonicalization_handles_paths_that_do_not_exist_yet() {
        let base = fsutil::unique_temp_dir("canon");
        std::fs::create_dir_all(&base).unwrap();
        let resolved = canonicalize_lenient(&base.join("a/b/c"));
        assert_eq!(resolved, base.canonicalize().unwrap().join("a/b/c"));
        let _ = std::fs::remove_dir_all(base);
    }
}
