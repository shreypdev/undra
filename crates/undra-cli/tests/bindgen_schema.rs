//! `undra bindgen --schema`: a schema file in, the three trees out, nothing built.

mod common;

use std::path::Path;

use common::{TempDir, run_err, run_ok, undra};

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stores.schema.json")
}

/// The files compared with the goldens: one of each language, the package manifests the CLI adds and the
/// `.gitattributes` of each tree (ADR-062).
const GOLDEN_FILES: &[&str] = &[
    "swift/.gitattributes",
    "swift/Package.swift",
    "swift/Sources/GoldenStores/Generated/Stores.swift",
    "kotlin/build.gradle.kts",
    "kotlin/.gitattributes",
    "kotlin/.gitignore",
    "kotlin/src/main/kotlin/dev/undra/generated/golden_stores/Stores.kt",
    "ts/.gitattributes",
    "ts/package.json",
    "ts/src/stores.ts",
    // The lint exclusions beside each tree (ADR-061).
    "kotlin/.editorconfig",
    "swift/.swiftlint.yml",
    "ts/.eslintrc.json",
    "ts/eslint.config.undra.mjs",
    "ts/tsconfig.json",
];

fn golden_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/stores")
}

#[test]
fn a_schema_file_generates_the_three_trees() {
    let out = TempDir::new("schema-out");
    let result = run_ok(
        undra()
            .args(["bindgen", "--schema"])
            .arg(fixture())
            .arg("--out")
            .arg(out.path())
            .current_dir(out.path()),
    );
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(
        text.contains("Generated bindings for golden-stores")
            && text.contains("swift")
            && text.contains("kotlin")
            && text.contains("ts"),
        "{text}"
    );

    let updating = std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    for file in GOLDEN_FILES {
        let actual = std::fs::read_to_string(out.path().join(file))
            .unwrap_or_else(|_| panic!("{file} was not generated"));
        // The manifests name the release this `undra` belongs to (ADR-063): the golden says `<version>`, so a release's
        // version bump does not change it.
        let actual = if file.ends_with("Package.swift") || file.ends_with("build.gradle.kts") {
            actual.replace(env!("CARGO_PKG_VERSION"), "<version>")
        } else {
            actual
        };
        let golden = golden_dir().join(file);
        if updating {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden)
            .unwrap_or_else(|_| panic!("golden {file} is missing; run with UPDATE_GOLDEN=1"));
        assert_eq!(
            actual, expected,
            "{file} differs from its golden (UPDATE_GOLDEN=1 to refresh after reviewing)"
        );
    }
}

#[test]
fn generation_is_idempotent_and_check_agrees() {
    let out = TempDir::new("schema-idem");
    let mut generate = undra();
    generate
        .args(["bindgen", "--schema"])
        .arg(fixture())
        .arg("--out")
        .arg(out.path());
    run_ok(&mut generate);
    let second = run_ok(&mut generate);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("0 written"),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );

    let check = run_ok(
        undra()
            .args(["bindgen", "--check", "--schema"])
            .arg(fixture())
            .arg("--out")
            .arg(out.path()),
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("up to date"));

    // A hand edit makes --check fail and name the file.
    let edited = out.path().join("ts/src/stores.ts");
    let mut text = std::fs::read_to_string(&edited).unwrap();
    text.push_str("// edited\n");
    std::fs::write(&edited, text).unwrap();
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--check", "--schema"])
            .arg(fixture())
            .arg("--out")
            .arg(out.path()),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("out of date") && stderr.contains("ts/src/stores.ts differs"),
        "{stderr}"
    );
    // Regenerating repairs it.
    run_ok(&mut generate);
    run_ok(
        undra()
            .args(["bindgen", "--check", "--schema"])
            .arg(fixture())
            .arg("--out")
            .arg(out.path()),
    );
}

#[test]
fn files_of_an_earlier_run_are_removed_and_the_users_are_not() {
    let out = TempDir::new("schema-stale");
    let mut generate = undra();
    generate
        .args(["bindgen", "--schema"])
        .arg(fixture())
        .arg("--out")
        .arg(out.path());
    run_ok(&mut generate);
    std::fs::write(out.path().join("ts/src/mine.ts"), "// mine\n").unwrap();
    // Pretend the previous run also wrote a file the schema no longer generates.
    std::fs::write(out.path().join("ts/src/gone.ts"), "// old\n").unwrap();
    let manifest = out.path().join(".undra-generated");
    let mut listed = std::fs::read_to_string(&manifest).unwrap();
    listed.push_str("ts/src/gone.ts\n");
    std::fs::write(&manifest, listed).unwrap();

    let again = run_ok(&mut generate);
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("1 removed"),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
    assert!(!out.path().join("ts/src/gone.ts").exists());
    assert!(
        out.path().join("ts/src/mine.ts").exists(),
        "files the CLI did not write are never touched"
    );
}

#[test]
fn platforms_can_be_selected() {
    let out = TempDir::new("schema-platforms");
    run_ok(
        undra()
            .args(["bindgen", "--platforms", "web", "--schema"])
            .arg(fixture())
            .arg("--out")
            .arg(out.path()),
    );
    assert!(out.path().join("ts/src/index.ts").is_file());
    assert!(!out.path().join("swift").exists() && !out.path().join("kotlin").exists());
}

#[test]
fn a_broken_schema_file_is_explained() {
    let out = TempDir::new("schema-broken");
    let bad = out.path().join("bad.json");
    std::fs::write(&bad, "{\"records\": 3}").unwrap();
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--schema"])
            .arg(&bad)
            .arg("--out")
            .arg(out.path()),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("error[undra::C0002]")
            && stderr.contains("bad.json")
            && stderr.contains("expected shape"),
        "{stderr}"
    );
    let (_, stderr) = run_err(
        undra()
            .args(["bindgen", "--schema"])
            .arg(out.path().join("missing.json"))
            .arg("--out")
            .arg(out.path()),
    );
    assert!(stderr.contains("error[undra::C0010]"), "{stderr}");
}

#[test]
fn a_schema_bindgen_rejects_reports_its_diagnostics() {
    let out = TempDir::new("schema-invalid");
    // The fixture with its first record duplicated: E0050.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let first = value["records"][0].clone();
    value["records"].as_array_mut().unwrap().push(first);
    let bad = out.path().join("dup.json");
    std::fs::write(&bad, serde_json::to_string(&value).unwrap()).unwrap();
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--schema"])
            .arg(&bad)
            .arg("--out")
            .arg(out.path()),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("error[undra::C0007]") && stderr.contains("E0050"),
        "{stderr}"
    );
}

/// The `--check` command of the generated tree, for the options `extra`.
fn check_command(schema: &Path, out: &Path, extra: &[&str]) -> std::process::Command {
    let mut check = undra();
    check
        .args(["bindgen", "--check", "--schema"])
        .arg(schema)
        .arg("--out")
        .arg(out)
        .args(extra);
    check
}

/// ADR-045: the Swift observation mode and the iOS floor are part of the generated files, so a project whose mode or floor
/// changed (`[ios] deployment_target`, `[bindings] swift_observation`, or the flags that say the same) without running
/// `undra bindgen` again has stale bindings, and `--check` says so and names the files.
#[test]
fn a_swift_mode_or_floor_switch_without_regeneration_is_stale_bindings() {
    let out = TempDir::new("schema-mode");
    let generate = |extra: &[&str]| {
        run_ok(
            undra()
                .args(["bindgen", "--schema"])
                .arg(fixture())
                .arg("--out")
                .arg(out.path())
                .args(extra),
        )
    };
    let stale = |extra: &[&str]| {
        let (code, stderr) = run_err(&mut check_command(&fixture(), out.path(), extra));
        assert_eq!(code, 1, "{extra:?}");
        assert!(
            stderr.contains("out of date")
                && stderr.contains("swift/Sources/GoldenStores/Generated/Stores.swift differs"),
            "{extra:?}: {stderr}"
        );
        stderr
    };
    let current = |extra: &[&str]| {
        let result = run_ok(&mut check_command(&fixture(), out.path(), extra));
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("up to date"),
            "{extra:?}"
        );
    };

    // Generated for iOS 17 (the default): current for itself, stale for every other mode or floor.
    generate(&[]);
    current(&[]);
    current(&["--ios-deployment-target", "17.0"]);
    stale(&["--swift-observation", "observable-object"]);
    stale(&["--ios-deployment-target", "15.0"]);
    stale(&["--ios-deployment-target", "16.0"]);

    // Regenerated for iOS 15: the header says which mode it is, and now the default is the stale one.
    generate(&["--ios-deployment-target", "15.0"]);
    let stores = std::fs::read_to_string(
        out.path()
            .join("swift/Sources/GoldenStores/Generated/Stores.swift"),
    )
    .unwrap();
    assert!(
        stores.lines().nth(1).is_some_and(
            |l| l.starts_with("// Swift mode: observable-object") && l.contains("iOS 15")
        ),
        "{stores}"
    );
    current(&["--ios-deployment-target", "15.0"]);
    current(&[
        "--swift-observation",
        "observable-object",
        "--ios-deployment-target",
        "15.0",
    ]);
    stale(&[]);
    // 16.0 keeps the mode but changes the wire Duration (UndraDuration to Duration) and the package's platforms.
    let stderr = stale(&["--ios-deployment-target", "16.0"]);
    assert!(stderr.contains("swift/Package.swift differs"), "{stderr}");
    // Back to the default: current again.
    generate(&[]);
    current(&[]);
}

/// R8 for the command line's spelling of the floor (`--ios-deployment-target`, `--swift-observation`): each mistake is a
/// `C0009` with what, why, a fix and the docs link, and nothing is written.
#[test]
fn the_floor_flags_explain_their_mistakes() {
    let out = TempDir::new("schema-flags");
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["--ios-deployment-target", "14.0"],
            "iOS 14.0 is below the lowest iOS Undra supports, 15.0",
            "Combine",
        ),
        (
            &["--ios-deployment-target", "latest"],
            "`latest` is not an iOS version",
            "IPHONEOS_DEPLOYMENT_TARGET",
        ),
        (
            &["--swift-observation", "combine"],
            "`combine` is not a Swift observation mode",
            "`ObservableObject`s",
        ),
        (
            &[
                "--swift-observation",
                "observation",
                "--ios-deployment-target",
                "16.4",
            ],
            "`--swift-observation observation` needs iOS 17, but `deployment_target = \"16.4\"` in [ios] is lower",
            "Observation, which is iOS 17.0 and later",
        ),
    ];
    for (extra, what, why) in cases {
        let (code, stderr) = run_err(
            undra()
                .args(["bindgen", "--schema"])
                .arg(fixture())
                .arg("--out")
                .arg(out.path())
                .args(*extra),
        );
        assert_eq!(code, 1, "{extra:?}");
        assert!(
            stderr.contains("error[undra::C0009]")
                && stderr.contains(what)
                && stderr.contains(why)
                && stderr.contains("= help: ")
                && stderr.contains("errors.html#C0009"),
            "{extra:?}: {stderr}"
        );
    }
    assert!(
        !out.path().join("swift").exists(),
        "a refused run writes nothing"
    );
    // 15.0.1 and a bare 15 are versions, and `observation` on 17.0 is fine.
    for extra in [
        &["--ios-deployment-target", "15.0.1"][..],
        &["--ios-deployment-target", "15"],
        &[
            "--swift-observation",
            "observation",
            "--ios-deployment-target",
            "17.0",
        ],
    ] {
        run_ok(
            undra()
                .args(["bindgen", "--schema"])
                .arg(fixture())
                .arg("--out")
                .arg(out.path())
                .args(extra),
        );
    }
}

/// E0051 at the floor only (ADR-045): a store member named `objectWillChange` is fine for `@Observable` and a collision
/// with `ObservableObject`'s own publisher below iOS 17, and the CLI reports it as bindgen diagnostics (`C0007`).
#[test]
fn a_store_member_named_object_will_change_is_refused_at_the_floor_only() {
    let out = TempDir::new("schema-e0051");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let todos = value["objects"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|o| o["name"] == "Todos")
        .unwrap();
    todos["store"]["signals"][0]["name"] = "object_will_change".into();
    let schema = out.path().join("will-change.json");
    std::fs::write(&schema, serde_json::to_string(&value).unwrap()).unwrap();
    // iOS 17: Observation has no such member.
    run_ok(
        undra()
            .args(["bindgen", "--schema"])
            .arg(&schema)
            .arg("--out")
            .arg(out.path().join("modern")),
    );
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--schema"])
            .arg(&schema)
            .arg("--out")
            .arg(out.path().join("floor"))
            .args(["--ios-deployment-target", "15.0"]),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("error[undra::C0007]")
            && stderr.contains("E0051")
            && stderr.contains("objectWillChange")
            && stderr.contains("ObservableObject"),
        "{stderr}"
    );
    assert!(!out.path().join("floor").join("swift").exists());
}

/// The four lint exclusion files `undra bindgen` writes beside the trees (ADR-061), as paths below the output directory.
const LINT_EXCLUSIONS: [&str; 4] = [
    "kotlin/.editorconfig",
    "swift/.swiftlint.yml",
    "ts/.eslintrc.json",
    "ts/eslint.config.undra.mjs",
];

/// `[bindings] lint_exclusions` of `undra.toml` decides whether the four exclusion files are written: absent and `"beside"`
/// write them (as the goldens above lock), `"none"` writes none of them, drops the ones an earlier run wrote, and leaves the
/// `@file:Suppress` line of every Kotlin file alone, since that line is part of the file.
#[test]
fn lint_exclusions_none_writes_no_exclusion_file_and_beside_and_absent_write_them_all() {
    let project_with = |bindings: &str| {
        let dir = TempDir::new("schema-lint");
        std::fs::write(
            dir.path().join("undra.toml"),
            format!(
                "[project]\nname = \"golden\"\nid = \"dev.undra.golden\"\n[core]\nnamespace = \"golden_stores\"\n{bindings}"
            ),
        )
        .unwrap();
        dir
    };
    let generate = |dir: &TempDir| {
        run_ok(
            undra()
                .args(["bindgen", "--schema"])
                .arg(fixture())
                .current_dir(dir.path()),
        )
    };
    let written = |dir: &TempDir| -> Vec<String> {
        LINT_EXCLUSIONS
            .iter()
            .filter(|f| dir.path().join("generated").join(f).is_file())
            .map(|f| (*f).to_owned())
            .collect()
    };

    let absent = project_with("");
    generate(&absent);
    assert_eq!(written(&absent), LINT_EXCLUSIONS, "no key writes them all");
    let beside = project_with("[bindings]\nlint_exclusions = \"beside\"\n");
    generate(&beside);
    assert_eq!(written(&beside), LINT_EXCLUSIONS);

    let none = project_with("[bindings]\nlint_exclusions = \"none\"\n");
    generate(&none);
    assert!(written(&none).is_empty(), "{:?}", written(&none));
    let generated = none.path().join("generated");
    let manifest = std::fs::read_to_string(generated.join(".undra-generated")).unwrap();
    assert!(
        LINT_EXCLUSIONS.iter().all(|f| !manifest.contains(f)),
        "{manifest}"
    );
    // Everything else is the same set of files as with `beside`.
    let others = |dir: &TempDir| -> Vec<String> {
        std::fs::read_to_string(dir.path().join("generated/.undra-generated"))
            .unwrap()
            .lines()
            .filter(|l| !LINT_EXCLUSIONS.contains(l))
            .map(ToOwned::to_owned)
            .collect()
    };
    assert_eq!(others(&none), others(&beside));
    let kotlin_files: Vec<String> = others(&none)
        .into_iter()
        .filter(|f| f.ends_with(".kt"))
        .collect();
    assert!(!kotlin_files.is_empty());
    for file in kotlin_files {
        let kotlin = std::fs::read_to_string(generated.join(&file)).unwrap();
        assert!(
            kotlin.contains("@file:Suppress(\"ALL\", \"ktlint\")"),
            "{file}: the annotation travels with the file: {kotlin}"
        );
    }
    run_ok(
        undra()
            .args(["bindgen", "--check", "--schema"])
            .arg(fixture())
            .current_dir(none.path()),
    );

    // Switching an existing tree to `none` makes `--check` name the files, and the next run removes them.
    std::fs::write(
        beside.path().join("undra.toml"),
        std::fs::read_to_string(none.path().join("undra.toml")).unwrap(),
    )
    .unwrap();
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--check", "--schema"])
            .arg(fixture())
            .current_dir(beside.path()),
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("kotlin/.editorconfig is stale"), "{stderr}");
    // The schema did not change, the setting did: the diagnosis names both causes, not only the core.
    assert!(
        stderr.contains("the schema or undra.toml changed")
            && stderr.contains("the core's schema and the project's undra.toml"),
        "{stderr}"
    );
    generate(&beside);
    assert!(written(&beside).is_empty());
}

/// A wrong `lint_exclusions` stops the command with a teaching `C0002` and writes nothing.
#[test]
fn a_wrong_lint_exclusions_value_stops_bindgen_with_a_teaching_error() {
    let dir = TempDir::new("schema-lint-bad");
    std::fs::write(
        dir.path().join("undra.toml"),
        "[project]\nname = \"golden\"\nid = \"dev.undra.golden\"\n[bindings]\nlint_exclusions = \"everywhere\"\n",
    )
    .unwrap();
    let (code, stderr) = run_err(
        undra()
            .args(["bindgen", "--schema"])
            .arg(fixture())
            .current_dir(dir.path()),
    );
    assert_ne!(code, 0);
    for needle in [
        "error[undra::C0002]",
        "line 5",
        "`everywhere` is not a lint exclusions setting",
        "= note: ",
        "lint_exclusions = \"none\"",
        "bazel.html#lint-everything",
        "errors.html#C0002",
    ] {
        assert!(stderr.contains(needle), "{needle}: {stderr}");
    }
    assert!(!dir.path().join("generated").exists());
}
