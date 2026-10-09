//! Compiles the generated Swift of every golden case against the real Swift runtime
//! (`runtimes/swift/UndraRuntime`) in the Swift 6 language mode, which is strict concurrency,
//! and runs the execution checks of `tests/fixtures/swift-run` against it.
//!
//! This is also what proves the standard library contract of ADR-024 (amended): the stdlib case
//! refers to `HttpRequest`, `HttpError`, `NetKind`, ... and declares none of them, so it only
//! builds if the runtime exports every one of them as public API with the conformances generated
//! code uses (`UndraRecord`, `UndraError`, `Codable`, public initializers).
//!
//! Every case becomes one target of a single scratch SwiftPM package below the target
//! directory, so that one `swift build` type-checks all of them and nothing is written
//! into the repository. The default mode builds for the host (macOS 14, which has the same API
//! availability as iOS 17); the iOS 15 / 16 mode of ADR-045 (`ObservableObject` stores,
//! `UndraDuration` below iOS 16) is built once more for each floor, for the iOS simulator at that
//! version (`--triple arm64-apple-ios15.0-simulator`), so a use of an API newer than the floor
//! fails here; without the iOS SDK it is built for the macOS version with the same availability. Each case also brings the C module bindgen generates for its core,
//! `<Namespace>CoreFFI` (the declaration of `<namespace>_undra_api`, ADR-044), which its Swift
//! target depends on; an execution check, which is linked, gets a C target that defines that
//! function as returning NULL (the checks never load a core in process). A compiler error fails the test; warnings do not (the generator
//! still escapes a few keyword argument labels that Swift accepts bare). Skipped, with a
//! message on stderr, when `swift` is not on the path or the host is not macOS (the
//! runtime builds against the Apple SDKs); `UNDRA_REQUIRE_TOOLCHAINS=1` turns the skip
//! into a failure and `UNDRA_SKIP_SWIFT=1` skips this test on purpose.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{manifest_dir, on_path, repo_root, scratch, skip};
use undra_bindgen::{GeneratedFile, SwiftObservation};

/// One way to build the generated Swift: which mode and floor it is generated for and which
/// platform floor the package declares.
struct Variant {
    /// The scratch directory and the package name.
    name: &'static str,
    observation: SwiftObservation,
    /// The major iOS version of the floor (`Generator::swift_min_ios`).
    min_ios: u32,
    /// The package's `platforms:` in the iOS build and in the host build used without the iOS SDK.
    ios_platforms: &'static str,
    host_platforms: &'static str,
    /// The simulator triple of the iOS build.
    triple: &'static str,
    /// Whether the execution checks run (they need the host).
    checks: bool,
}

const DEFAULT: Variant = Variant {
    name: "swift-generated",
    observation: SwiftObservation::Observation,
    min_ios: 17,
    ios_platforms: ".iOS(.v17), .macOS(.v14)",
    host_platforms: ".macOS(.v14)",
    triple: "",
    checks: true,
};

const IOS15: Variant = Variant {
    name: "swift-generated-ios15",
    observation: SwiftObservation::ObservableObject,
    min_ios: 15,
    ios_platforms: ".iOS(.v15), .macOS(.v12)",
    host_platforms: ".macOS(.v12)",
    triple: "arm64-apple-ios15.0-simulator",
    checks: false,
};

const IOS16: Variant = Variant {
    name: "swift-generated-ios16",
    observation: SwiftObservation::ObservableObject,
    min_ios: 16,
    ios_platforms: ".iOS(.v16), .macOS(.v13)",
    host_platforms: ".macOS(.v13)",
    triple: "arm64-apple-ios16.0-simulator",
    checks: false,
};

/// The SwiftPM target a case is compiled as: `records` is `GoldenRecords`.
fn target_name(case: &str) -> String {
    let mut chars = case.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase());
    format!("Golden{}{}", first.unwrap_or_default(), chars.as_str())
}

/// The execution checks: a case and the `main.swift` (in `tests/fixtures/swift-run`) that is built
/// against its generated module as the executable `<target>Check`.
const CHECKS: &[(&str, &str)] = &[
    ("recursive", "recursive.swift"),
    ("callbacks", "callbacks.swift"),
    ("newtypes", "newtypes.swift"),
    ("generic_functions", "generic_functions.swift"),
];

/// The executable target of a check.
fn check_name(case: &str) -> String {
    format!("{}Check", target_name(case))
}

/// The host side of a case, as an app writes it against the generated code (ADR-066): the files of
/// `tests/fixtures/swift-isolation`, compiled as the target `<Target>App` of the default build and,
/// under default main-actor isolation, of the pass below.
const APPS: &[(&str, &[&str])] = &[("ports", &["ports-protocol.swift", "ports-closures.swift"])];

/// The target of a case's app files.
fn app_name(case: &str) -> String {
    format!("{}App", target_name(case))
}

/// The C module of a case's core (`GoldenRecordsCoreFFI`, `PlaygroundCoreFFI`), as generated.
fn ffi_module(case: &str) -> String {
    common::generator_for(case, &common::case(case))
        .core_names()
        .ffi_module()
}

/// The C target that stands in for a check's core at link time.
fn stub_name(case: &str) -> String {
    format!("{}CoreStub", check_name(case))
}

/// The source of `stub_name(case)`: the core's entry point, returning no table.
fn stub_source(case: &str) -> String {
    let symbol = common::generator_for(case, &common::case(case))
        .core_names()
        .api_symbol();
    format!(
        "/* A stand-in for the core's entry point: the check never loads the core in process. */\n\
         const void *{symbol}(void);\n\
         const void *{symbol}(void) {{ return 0; }}\n"
    )
}

/// Why the Swift toolchain cannot be used, or `None` when it can.
fn unavailable() -> Option<&'static str> {
    if std::env::var("UNDRA_SKIP_SWIFT").is_ok_and(|v| v == "1") {
        Some("UNDRA_SKIP_SWIFT=1")
    } else if !cfg!(target_os = "macos") {
        Some("the Swift runtime builds on macOS only")
    } else if !on_path("swift") {
        Some("no `swift` found on the path")
    } else {
        None
    }
}

/// What one scratch package holds: the generated cases, the execution checks, the app files of
/// `APPS`, and how the manifest compiles them.
struct Layout<'a> {
    cases: &'a [&'a str],
    checks: &'a [(&'a str, &'a str)],
    apps: &'a [(&'a str, &'a [&'a str])],
    /// What follows the dependencies of every app target: nothing, or `, swiftSettings: [..]`.
    app_settings: &'a str,
    /// The manifest's `swift-tools-version`.
    tools: &'a str,
}

/// The manifest of the scratch package: per case, the C module of its core and its Swift target;
/// per check, the stand-in for the core and the executable; per app, its target on the case's
/// module; all on the real runtime.
fn manifest(runtime: &Path, layout: &Layout, platforms: &str) -> String {
    let runtime_product = ".product(name: \"UndraRuntime\", package: \"UndraRuntime\")";
    let mut targets: String = layout
        .cases
        .iter()
        .map(|case| {
            let ffi = ffi_module(case);
            format!(
                "        .target(name: \"{ffi}\", path: \"Sources/{ffi}\", publicHeadersPath: \"include\"),\n        \
                 .target(name: \"{}\", dependencies: [\"{ffi}\", {runtime_product}]),\n",
                target_name(case)
            )
        })
        .collect();
    for (case, _) in layout.checks {
        targets.push_str(&format!(
            "        .target(name: \"{stub}\", path: \"Sources/{stub}\"),\n        \
             .executableTarget(name: \"{}\", dependencies: [\"{}\", {runtime_product}, \"{stub}\"]),\n",
            check_name(case),
            target_name(case),
            stub = stub_name(case)
        ));
    }
    for (case, _) in layout.apps {
        targets.push_str(&format!(
            "        .target(name: \"{}\", dependencies: [\"{}\", {runtime_product}]{}),\n",
            app_name(case),
            target_name(case),
            layout.app_settings
        ));
    }
    format!(
        "// swift-tools-version: {}\n\
         import PackageDescription\n\n\
         let package = Package(\n    \
             name: \"GoldenSwift\",\n    \
             platforms: [{platforms}],\n    \
             dependencies: [.package(path: \"{}\")],\n    \
             targets: [\n{targets}    ],\n    \
             swiftLanguageModes: [.v6]\n\
         )\n",
        layout.tools,
        runtime.display()
    )
}

/// Writes the generated Swift of `layout.cases`, the checks and the app files as the targets of one
/// package in `root`.
fn lay_out(root: &Path, runtime: &Path, layout: &Layout, variant: &Variant, platforms: &str) {
    for case in layout.cases {
        let schema = common::case(case);
        let mut generator = common::generator_for(case, &schema);
        generator.swift_observation = variant.observation;
        generator.swift_min_ios = variant.min_ios;
        let files = generator.swift(&schema).unwrap();
        // The generated Swift never names its own module, so a case compiles under any target
        // name; the golden path's module (`PlaygroundCore`, `GoldenRecords`, ...) is replaced by
        // the case's own. The C module of the core keeps its generated name and layout: the
        // Swift imports it by that name.
        let ffi = format!("Sources/{}/", ffi_module(case));
        let renamed: Vec<GeneratedFile> = files
            .into_iter()
            .map(|f| {
                if f.path.starts_with(&ffi) {
                    return f;
                }
                let file = f.path.rsplit('/').next().unwrap_or(&f.path).to_owned();
                GeneratedFile {
                    path: format!("Sources/{}/Generated/{file}", target_name(case)),
                    contents: f.contents,
                }
            })
            .collect();
        assert!(
            renamed.iter().any(|f| f.path.starts_with(&ffi)),
            "`{case}` generates no C module {ffi}"
        );
        GeneratedFile::write_all(&renamed, root).unwrap();
    }
    for (case, fixture) in layout.checks {
        let stub = root.join("Sources").join(stub_name(case));
        fs::create_dir_all(stub.join("include")).unwrap();
        fs::write(stub.join("core_stub.c"), stub_source(case)).unwrap();
        let dir = root.join("Sources").join(check_name(case));
        fs::create_dir_all(&dir).unwrap();
        fs::copy(
            manifest_dir()
                .join("tests/fixtures/swift-run")
                .join(fixture),
            dir.join("main.swift"),
        )
        .unwrap();
    }
    for (case, files) in layout.apps {
        let dir = root.join("Sources").join(app_name(case));
        fs::create_dir_all(&dir).unwrap();
        for file in *files {
            fs::copy(
                manifest_dir()
                    .join("tests/fixtures/swift-isolation")
                    .join(file),
                dir.join(file),
            )
            .unwrap();
        }
    }
    fs::write(
        root.join("Package.swift"),
        manifest(runtime, layout, platforms),
    )
    .unwrap();
}

/// The compiler diagnostics that are errors, one per line, for a readable failure.
fn errors(output: &str) -> String {
    output
        .lines()
        .filter(|l| l.contains(": error:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the build left the compiled module `target` (SwiftPM 6 keeps them in `Modules/`).
fn has_module(root: &Path, target: &str) -> bool {
    let debug = root.join(".build/debug");
    [debug.join("Modules"), debug]
        .iter()
        .any(|dir| dir.join(format!("{target}.swiftmodule")).exists())
}

/// The iPhone Simulator SDK, when Xcode is installed.
fn simulator_sdk() -> Option<String> {
    let output = Command::new("xcrun")
        .args(["--sdk", "iphonesimulator", "--show-sdk-path"])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && Path::new(&path).exists()).then_some(path)
}

/// `swift build` of the scratch package; for the iOS simulator at `triple` when `sdk` is given.
fn build(root: &Path, ios: Option<(&str, &str)>) -> Result<String, String> {
    let mut command = Command::new("swift");
    if let Some((sdk, triple)) = ios {
        command.args(["build", "--sdk", sdk, "--triple", triple]);
    } else {
        command.arg("build");
    }
    let output = command
        .arg("--package-path")
        .arg(root)
        .arg("--scratch-path")
        .arg(root.join(".build"))
        // The manifest is ours; a nested sandbox (CI inside a container, an agent) would only
        // make the manifest evaluation fail.
        .arg("--disable-sandbox")
        .output()
        .expect("swift runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// Generates every case for `variant`, builds the package and checks that every case compiled
/// into a module of its own; runs the execution checks when the variant has them.
fn compile(variant: &Variant) {
    if let Some(why) = unavailable() {
        skip(&format!("no Swift toolchain: {why}"));
        return;
    }
    // The floors are built for the iOS simulator at their own version; without Xcode's iOS SDK for
    // the macOS version with the same API availability.
    let sdk = if variant.triple.is_empty() {
        None
    } else {
        simulator_sdk()
    };
    let (platforms, ios) = match &sdk {
        Some(sdk) => (variant.ios_platforms, Some((sdk.as_str(), variant.triple))),
        None => (variant.host_platforms, None),
    };
    let runtime: PathBuf = repo_root()
        .join("runtimes/swift/UndraRuntime")
        .canonicalize()
        .expect("the Swift runtime package exists");
    let root = scratch(variant.name);
    // The checks and the app files need the host (the floors build for the simulator).
    let layout = Layout {
        cases: common::CASES,
        checks: if variant.checks { CHECKS } else { &[] },
        apps: if variant.checks { APPS } else { &[] },
        app_settings: "",
        tools: "6.0",
    };
    lay_out(&root, &runtime, &layout, variant, platforms);
    match build(&root, ios) {
        Ok(output) => {
            assert!(
                output.contains("Build complete"),
                "swift build did not report a complete build:\n{output}"
            );
            // Every case was compiled into a module of its own, and every app too.
            let missing: Vec<String> = common::CASES
                .iter()
                .map(|case| target_name(case))
                .chain(layout.apps.iter().map(|(case, _)| app_name(case)))
                .filter(|target| !has_module(&root, target))
                .collect();
            assert!(missing.is_empty(), "no compiled module for {missing:?}");
        }
        Err(output) => panic!(
            "the generated Swift ({}) does not compile ({} golden cases as one package):\n{}\n\nfull output:\n{output}",
            variant.name,
            common::CASES.len(),
            errors(&output)
        ),
    }
    if !variant.checks {
        return;
    }
    for (case, _) in CHECKS {
        let output = Command::new(root.join(".build/debug").join(check_name(case)))
            .output()
            .expect("the check executable runs");
        assert!(
            output.status.success(),
            "the generated Swift of `{case}` fails its execution test:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn every_case_compiles_and_the_generated_swift_behaves() {
    compile(&DEFAULT);
}

/// ADR-045: the `ObservableObject` stores and `UndraDuration` build against the runtime at iOS 15.
#[test]
fn every_case_compiles_at_the_ios_15_floor() {
    compile(&IOS15);
}

/// ADR-045: from iOS 16 the wire `Duration` is `Swift.Duration` again, with `ObservableObject` stores.
#[test]
fn every_case_compiles_at_the_ios_16_floor() {
    compile(&IOS16);
}

#[test]
fn the_scratch_package_names_one_target_per_case_and_one_executable_per_check() {
    // Needs no Swift toolchain: the layout is plain text.
    let layout = Layout {
        cases: common::CASES,
        checks: CHECKS,
        apps: APPS,
        app_settings: ", swiftSettings: [.defaultIsolation(MainActor.self)]",
        tools: "6.2",
    };
    let manifest = manifest(Path::new("/runtime"), &layout, DEFAULT.host_platforms);
    for case in common::CASES {
        let ffi = ffi_module(case);
        assert!(manifest.contains(&format!(
            ".target(name: \"{}\", dependencies: [\"{ffi}\", ",
            target_name(case)
        )));
        assert!(manifest.contains(&format!(
            ".target(name: \"{ffi}\", path: \"Sources/{ffi}\", publicHeadersPath: \"include\")"
        )));
    }
    assert!(
        manifest.contains("\"PlaygroundCoreFFI\""),
        "the full case keeps its core's module name"
    );
    for (case, fixture) in CHECKS {
        assert!(manifest.contains(&format!(
            ".executableTarget(name: \"{}\", dependencies: [\"{}\"",
            check_name(case),
            target_name(case)
        )));
        assert!(manifest.contains(&format!("\"{}\"]", stub_name(case))));
        assert!(stub_source(case).contains("_undra_api(void) { return 0; }"));
        assert!(
            manifest_dir()
                .join("tests/fixtures/swift-run")
                .join(fixture)
                .exists(),
            "{fixture} is missing"
        );
        assert!(common::CASES.contains(case), "{case} is not a golden case");
    }
    for (case, files) in APPS {
        assert!(manifest.contains(&format!(
            ".target(name: \"{}\", dependencies: [\"{}\", .product(name: \"UndraRuntime\", package: \"UndraRuntime\")], swiftSettings: [.defaultIsolation(MainActor.self)])",
            app_name(case),
            target_name(case)
        )));
        for file in *files {
            assert!(
                manifest_dir()
                    .join("tests/fixtures/swift-isolation")
                    .join(file)
                    .exists(),
                "{file} is missing"
            );
        }
        assert!(common::CASES.contains(case), "{case} is not a golden case");
    }
    assert!(manifest.starts_with("// swift-tools-version: 6.2\n"));
    assert!(manifest.contains("swiftLanguageModes: [.v6]"));
    assert!(manifest.contains(".package(path: \"/runtime\")"));
    assert!(manifest.contains("platforms: [.macOS(.v14)]"));
}

/// `swift --version`'s `Swift version 6.3.3` as (major, minor), when it can be read.
fn swift_version() -> Option<(u32, u32)> {
    let output = Command::new("swift").arg("--version").output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let rest = text.split("Swift version ").nth(1)?;
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .take(2)
        .map(|part| part.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

/// ADR-066: against the `ports` golden as SwiftPM builds it, a plain class per port and a closure
/// registration per port (`tests/fixtures/swift-isolation`) compile in an app target under default
/// main-actor isolation (`.defaultIsolation(MainActor.self)`: Swift 6.2 and later, what Xcode 26
/// sets for a new project). The default build above compiles the same two files without the setting.
#[test]
fn the_ports_golden_is_implementable_under_default_main_actor_isolation() {
    if let Some(why) = unavailable() {
        skip(&format!("no Swift toolchain: {why}"));
        return;
    }
    match swift_version() {
        Some(version) if version >= (6, 2) => {}
        version => {
            eprintln!(
                "skipping the default main-actor isolation pass: it needs Swift 6.2 or newer, found {version:?}"
            );
            return;
        }
    }
    let runtime: PathBuf = repo_root()
        .join("runtimes/swift/UndraRuntime")
        .canonicalize()
        .expect("the Swift runtime package exists");
    let root = scratch("swift-generated-main-actor");
    let layout = Layout {
        cases: &["ports"],
        checks: &[],
        apps: APPS,
        app_settings: ", swiftSettings: [.defaultIsolation(MainActor.self)]",
        tools: "6.2",
    };
    lay_out(&root, &runtime, &layout, &DEFAULT, DEFAULT.host_platforms);
    match build(&root, None) {
        Ok(output) => {
            assert!(
                output.contains("Build complete"),
                "swift build did not report a complete build:\n{output}"
            );
            assert!(
                has_module(&root, &app_name("ports")),
                "no compiled module for the app"
            );
        }
        Err(output) => panic!(
            "the host side of the `ports` golden does not compile under default main-actor isolation:\n{}\n\nfull output:\n{output}",
            errors(&output)
        ),
    }
}
