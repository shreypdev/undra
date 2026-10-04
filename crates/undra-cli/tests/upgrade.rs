//! `undra upgrade` on fixture projects: every pin moves in lockstep, comments and formatting
//! survive, a checkout is left alone, a project ahead of the CLI is refused, and the migration
//! notes of the releases crossed are printed.
//!
//! The fixtures are in `tests/fixtures/upgrade/`: `released-0.0.9` is what `undra init` wrote at
//! an older release (every kind of pin), the others are the shapes `init` does not write but real
//! projects have (a commit pin, registry versions in a workspace, a checkout, a fork).
//!
//! The last test regenerates the bindings for real (it builds the core from a local clone of this
//! repository standing in for GitHub) and runs where `UNDRA_TEST_UPGRADE_E2E=1` or
//! `UNDRA_REQUIRE_TOOLCHAINS=1` says so.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, flag, run_err, run_ok, undra};

const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Where the npm packages of a release are downloaded (ADR-063).
const ASSETS: &str = "https://github.com/shreypdev/undra/releases/download";

/// The release the first migration notes are filed under (`crates/undra-cli/src/migrations.rs`). It is not always this
/// `undra`'s: scripts/bump-version.sh files notes kept under a version that was never released under the new one, and
/// leaves them under a released one (`1.0.0-rc.1` when `1.0.0` is cut), so the version pull requests stay green.
fn first_notes_release() -> String {
    let text = include_str!("../src/migrations.rs");
    let at = text
        .find("version: \"")
        .expect("the first migration's version")
        + "version: \"".len();
    text[at..at + text[at..].find('"').unwrap()].to_owned()
}

/// The `@undra/runtime` dependency of a project on the current release.
fn runtime_url() -> String {
    format!("{ASSETS}/v{CURRENT}/undra-runtime-{CURRENT}.tgz")
}

/// What `undra init` writes into `dependencyResolutionManagement { repositories { } }` (ADR-063).
const JITPACK: &str = "        // Undra's Kotlin runtime, built from the release tag (ADR-063). Only its group is looked up here.\n        maven {\n            url = uri(\"https://jitpack.io\")\n            content { includeGroup(\"com.github.shreypdev.undra\") }\n        }\n";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/upgrade")
        .join(name)
}

/// Every file below `dir` with its text, keyed by path relative to `dir`.
fn read_tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap_or_default(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A copy of the fixture in a temporary directory.
fn project(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(name);
    let root = dir.path().join("demo");
    for (rel, text) in read_tree(&fixture(name)) {
        let path = root.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (dir, root)
}

fn upgrade(root: &Path, args: &[&str]) -> (String, String) {
    let out = run_ok(undra().arg("-C").arg(root).arg("upgrade").args(args));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// What the released fixture's files become: the old pins replaced by the current release's, from GitHub (ADR-063).
fn upgraded(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = tree.clone();
    for (path, text) in &mut out {
        if path.starts_with("generated/") || path == "core/src/lib.rs" {
            continue;
        }
        *text = text
            .replace("tag = \"v0.0.9\"", &format!("tag = \"v{CURRENT}\""))
            .replace("version = \"0.0\"\n", &format!("version = \"{CURRENT}\"\n"))
            .replace("\"^0.0.9\"", &format!("\"{}\"", runtime_url()))
            .replace(
                "dev.undra:runtime:0.0.9",
                &format!("com.github.shreypdev.undra:runtime:v{CURRENT}"),
            )
            .replace(
                "dev.undra:android-adapters:0.0.9",
                &format!("com.github.shreypdev.undra:android-adapters:v{CURRENT}"),
            )
            .replace(
                "minimumVersion = 0.0.9;",
                &format!("minimumVersion = {CURRENT};"),
            )
            .replace(
                "repositoryURL = \"https://github.com/shreypdev/undra-swift\";",
                "repositoryURL = \"https://github.com/shreypdev/undra\";",
            )
            .replace(
                "UNDRA_VERSION: \"0.0.9\"",
                &format!("UNDRA_VERSION: \"{CURRENT}\""),
            );
        if path == "android/settings.gradle.kts" {
            *text = text.replace(
                "        mavenCentral()\n    }\n}",
                &format!("        mavenCentral()\n{JITPACK}    }}\n}}"),
            );
        }
    }
    out
}

#[test]
fn a_dry_run_shows_every_line_that_would_change_and_writes_nothing() {
    let (_dir, root) = project("released-0.0.9");
    let before = read_tree(&root);
    let (stdout, _) = upgrade(&root, &["--dry-run"]);
    assert_eq!(read_tree(&root), before, "nothing was written");
    assert!(
        stdout.contains(&format!(
            "undra upgrade: demo is on Undra 0.0.9; this undra is {CURRENT}"
        )),
        "{stdout}"
    );
    for file in [
        "core/Cargo.toml",
        "undra.toml",
        "web/package.json",
        "android/app/build.gradle.kts",
        "android/settings.gradle.kts",
        "ios/Demo.xcodeproj/project.pbxproj",
        ".github/workflows/undra.yml",
    ] {
        assert!(
            stdout.contains(&format!("\n{file}\n")),
            "no {file}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains(
            "    - undra = { git = \"https://github.com/shreypdev/undra\", tag = \"v0.0.9\" }"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "    + undra = {{ git = \"https://github.com/shreypdev/undra\", tag = \"v{CURRENT}\" }}"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("    - \"@undra/runtime\": \"^0.0.9\","),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("    + \"@undra/runtime\": \"{}\",", runtime_url())),
        "{stdout}"
    );
    assert!(
        stdout.contains("    + maven { url = uri(\"https://jitpack.io\") } (for com.github.shreypdev.undra only)"),
        "{stdout}"
    );
    assert!(stdout.contains("9 lines in 7 files."), "{stdout}");
    // The notes of the release it crosses, and the way out.
    assert!(
        stdout.contains(&format!("Migration notes, 0.0.9 to {CURRENT}")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{}  Since v1.0:", first_notes_release()))
            && stdout.contains("[do] Rebuild the core and every app together"),
        "{stdout}"
    );
    assert!(
        stdout.contains("--dry-run: nothing was written. Run `undra upgrade` to apply it."),
        "{stdout}"
    );
}

#[test]
fn every_pin_moves_in_lockstep_and_nothing_else_changes() {
    let (_dir, root) = project("released-0.0.9");
    let before = read_tree(&root);
    let (stdout, stderr) = upgrade(&root, &["--no-bindgen"]);
    let after = read_tree(&root);
    assert_eq!(after, upgraded(&before), "{stdout}");
    // The bindings are the generator's: untouched here, and said so.
    assert_eq!(
        after["generated/ts/package.json"],
        before["generated/ts/package.json"]
    );
    assert_eq!(after["core/src/lib.rs"], before["core/src/lib.rs"]);
    assert!(stdout.contains("Updated 7 files."), "{stdout}");
    assert!(
        stderr.contains("bindings not regenerated (--no-bindgen): run `undra bindgen`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`npm install` in the web app refreshes its package-lock.json"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Cargo.lock follows at the next build"),
        "{stderr}"
    );
    assert!(stdout.contains("Review with `git diff`"), "{stdout}");
    // The pins agree with each other and with the CLI.
    assert!(after["core/Cargo.toml"].contains(&format!("tag = \"v{CURRENT}\"")));
    assert!(
        after[".github/workflows/undra.yml"].contains(&format!("UNDRA_VERSION: \"{CURRENT}\""))
    );
}

#[test]
fn a_second_upgrade_has_nothing_to_do() {
    let (_dir, root) = project("released-0.0.9");
    upgrade(&root, &["--no-bindgen"]);
    let settled = read_tree(&root);
    let (stdout, stderr) = upgrade(&root, &[]);
    assert_eq!(read_tree(&root), settled);
    assert!(stdout.contains("nothing to change"), "{stdout}");
    assert!(!stdout.contains("Migration notes"), "{stdout}");
    assert!(
        !stderr.contains("bindgen"),
        "it does not regenerate what did not change:\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("is on Undra {CURRENT}")),
        "{stdout}"
    );
}

#[test]
fn an_upgraded_project_is_the_project_init_would_have_made() {
    // A fresh project (no `--undra-path`, outside any checkout), aged back to 0.0.9 pin by pin, then upgraded.
    let dir = TempDir::new("lockstep");
    run_ok(undra().args(["init", "lockstep", "--dir"]).arg(dir.path()));
    let root = dir.path().join("lockstep");
    let fresh = read_tree(&root);
    // Each file aged by the pins it holds, as a project made before ADR-063 had them.
    let age = |file: &str, text: &str| -> String {
        match file {
            "undra.toml" => {
                text.replace(&format!("version = \"{CURRENT}\"\n"), "version = \"0.0\"\n")
            }
            "core/Cargo.toml" => text.replace(&format!("tag = \"v{CURRENT}\""), "tag = \"v0.0.9\""),
            "web/package.json" => text.replace(&runtime_url(), "^0.0.9"),
            "android/app/build.gradle.kts" => text
                .replace("com.github.shreypdev.undra:", "dev.undra:")
                .replace(&format!(":v{CURRENT}\""), ":0.0.9\""),
            "android/settings.gradle.kts" => text.replace(JITPACK, ""),
            "ios/Lockstep.xcodeproj/project.pbxproj" => text
                .replace(
                    &format!("minimumVersion = {CURRENT};"),
                    "minimumVersion = 0.0.9;",
                )
                .replace(
                    "repositoryURL = \"https://github.com/shreypdev/undra\";",
                    "repositoryURL = \"https://github.com/shreypdev/undra-swift\";",
                ),
            ".github/workflows/undra.yml" => text.replace(
                &format!("UNDRA_VERSION: \"{CURRENT}\""),
                "UNDRA_VERSION: \"0.0.9\"",
            ),
            _ => unreachable!(),
        }
    };
    let pinned = [
        "undra.toml",
        "core/Cargo.toml",
        "web/package.json",
        "android/app/build.gradle.kts",
        "android/settings.gradle.kts",
        "ios/Lockstep.xcodeproj/project.pbxproj",
        ".github/workflows/undra.yml",
    ];
    for file in pinned {
        let aged = age(file, &fresh[file]);
        assert_ne!(
            aged,
            fresh[file],
            "{file} has no pin to age: {}",
            &fresh[file][..200.min(fresh[file].len())]
        );
        std::fs::write(root.join(file), aged).unwrap();
    }
    upgrade(&root, &["--no-bindgen"]);
    let after = read_tree(&root);
    for file in pinned {
        assert_eq!(
            after[file], fresh[file],
            "{file}: an upgrade must leave what `undra init` writes"
        );
    }
}

#[test]
fn a_commit_pin_becomes_the_release_tag_and_the_rest_of_the_line_survives() {
    let (_dir, root) = project("pinned-by-rev");
    let (stdout, _) = upgrade(&root, &["--no-bindgen"]);
    let cargo = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
    assert!(
        cargo.contains(&format!("undra = {{ git = \"https://github.com/shreypdev/undra\", tag = \"v{CURRENT}\", features = [\"extra\"] }} # pinned\n")),
        "{cargo}"
    );
    assert!(
        cargo.contains("# The Undra release this core is built on: a commit of the repository.\n")
            && cargo.contains("serde = \"1\"\n"),
        "{cargo}"
    );
    let web = std::fs::read_to_string(root.join("web/package.json")).unwrap();
    assert!(
        web.contains(&format!("\"@undra/runtime\": \"{}\"", runtime_url()))
            && web.contains("\"react\": \"^19.0.0\""),
        "{web}"
    );
    // The oldest pin that names a version is where the project is said to be; the commit names none.
    assert!(stdout.contains("is on Undra 0.0.5"), "{stdout}");
}

#[test]
fn registry_versions_workspace_tables_and_dotted_sections_are_moved() {
    let (_dir, root) = project("registry");
    upgrade(&root, &["--no-bindgen"]);
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(workspace.contains(&format!("undra = \"{CURRENT}\"\nundra-ffi = {{ version = \"={CURRENT}\", features = [\"jni\"] }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\n")), "{workspace}");
    let core = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
    assert!(
        core.contains("undra.workspace = true\n")
            && core.contains("shared = { path = \"../crates/shared\" }\n"),
        "{core}"
    );
    assert!(
        core.contains(&format!("undra-wire = \"^{CURRENT}\"\n")),
        "{core}"
    );
    let shared = std::fs::read_to_string(root.join("crates/shared/Cargo.toml")).unwrap();
    assert!(
        shared.contains(&format!(
            "[dependencies.undra-meta]\nversion = \"={CURRENT}\"\ndefault-features = false\n"
        )),
        "{shared}"
    );
}

#[test]
fn a_project_on_a_checkout_is_left_alone_and_told_so() {
    let (_dir, root) = project("checkout");
    let before = read_tree(&root);
    let (stdout, _) = upgrade(&root, &[]);
    assert_eq!(read_tree(&root), before);
    assert!(stdout.contains("depends on a checkout of the Undra repository, so its version is whatever that checkout is"), "{stdout}");
    assert!(
        stdout.contains("core/Cargo.toml:")
            && stdout.contains("undra = { path = \"../../undra/crates/undra\" }"),
        "{stdout}"
    );
    assert!(stdout.contains("Nothing was changed."), "{stdout}");
    assert!(!stdout.contains("Migration notes"), "{stdout}");
}

#[test]
fn a_project_ahead_of_this_undra_is_refused_with_the_way_out() {
    let (_dir, root) = project("ahead");
    let before = read_tree(&root);
    let (code, stderr) = run_err(undra().arg("-C").arg(&root).arg("upgrade"));
    assert_eq!(code, 1);
    assert_eq!(read_tree(&root), before);
    assert!(stderr.starts_with("error[undra::C0014]: "), "{stderr}");
    assert!(
        stderr.contains("pins Undra at")
            && stderr.contains(&format!("newer than this undra ({CURRENT})")),
        "{stderr}"
    );
    assert!(
        stderr.contains("update this `undra` first")
            && stderr.contains("install.sh")
            && stderr.contains("cargo install")
            && !stderr.contains("brew"),
        "{stderr}"
    );
    assert!(
        stderr.contains("https://shreypdev.github.io/undra/docs/errors.html#C0014"),
        "{stderr}"
    );
}

#[test]
fn a_fork_is_reported_and_left_while_the_other_pins_move() {
    let (_dir, root) = project("fork");
    let (_, stderr) = upgrade(&root, &["--no-bindgen"]);
    assert!(
        stderr.contains("depends on a fork of Undra, which has no release tags to move to"),
        "{stderr}"
    );
    let cargo = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
    assert!(
        cargo.contains("https://github.com/someone/undra\", tag = \"v0.0.9\""),
        "{cargo}"
    );
    let web = std::fs::read_to_string(root.join("web/package.json")).unwrap();
    assert!(!web.contains("^0.0.9"), "{web}");
}

/// A file the upgrade cannot write stops it before anything is written: no half-upgraded tree.
#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_written_leaves_every_file_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, root) = project("released-0.0.9");
    // The last file in the order the edits are written: the five before it would have been written.
    let locked = root.join("web/package.json");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = read_tree(&root);
    let (code, stderr) = run_err(
        undra()
            .arg("-C")
            .arg(&root)
            .args(["upgrade", "--no-bindgen"]),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(read_tree(&root), before, "a half-upgraded tree:\n{stderr}");
    assert!(stderr.contains("error[undra::C0010]"), "{stderr}");
    assert!(stderr.contains("web/package.json"), "{stderr}");
    assert!(stderr.contains("nothing was written"), "{stderr}");
}

/// A project whose core is missing cannot have its bindings regenerated: refused before any pin moves.
#[test]
fn a_project_without_its_core_is_refused_before_anything_is_written() {
    let (_dir, root) = project("released-0.0.9");
    std::fs::remove_file(root.join("core/Cargo.toml")).unwrap();
    let before = read_tree(&root);
    let (code, stderr) = run_err(undra().arg("-C").arg(&root).arg("upgrade"));
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        read_tree(&root),
        before,
        "pins moved for bindings that cannot be made:\n{stderr}"
    );
    assert!(
        stderr.contains("error[undra::C0005]") && stderr.contains("nothing was written"),
        "{stderr}"
    );
    // --no-bindgen moves the pins it finds: there is nothing to regenerate.
    upgrade(&root, &["--no-bindgen"]);
    assert_ne!(read_tree(&root), before);
}

/// A Gradle version held in a variable is not a pin `undra init` writes: left, and said.
#[test]
fn a_gradle_version_variable_is_reported_and_left() {
    let (_dir, root) = project("released-0.0.9");
    let gradle = root.join("android/app/build.gradle.kts");
    let text = std::fs::read_to_string(&gradle)
        .unwrap()
        .replace("dev.undra:runtime:0.0.9", "dev.undra:runtime:$undraVersion");
    std::fs::write(&gradle, &text).unwrap();
    let (_, stderr) = upgrade(&root, &["--no-bindgen"]);
    assert!(
        stderr.contains("android/app/build.gradle.kts")
            && stderr.contains("not written out")
            && stderr.contains("$undraVersion"),
        "{stderr}"
    );
    let after = std::fs::read_to_string(&gradle).unwrap();
    assert!(after.contains("dev.undra:runtime:$undraVersion"), "{after}");
    assert!(
        after.contains(&format!(
            "com.github.shreypdev.undra:android-adapters:v{CURRENT}"
        )),
        "{after}"
    );
}

#[test]
fn outside_a_project_it_says_where_to_run_it() {
    let dir = TempDir::new("upgrade-none");
    let (code, stderr) = run_err(undra().arg("upgrade").current_dir(dir.path()));
    assert_eq!(code, 1);
    assert!(
        stderr.starts_with("error[undra::C0001]: no undra.toml found"),
        "{stderr}"
    );
}

#[test]
fn the_help_teaches_what_it_moves_and_what_it_does_not() {
    let out = run_ok(undra().args(["upgrade", "--help"]));
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "EXAMPLES",
        "--dry-run",
        "--no-bindgen",
        "--docs",
        "[undra] version",
        "@undra/runtime",
        "UNDRA_VERSION",
        "dev.undra:android-adapters",
        "checkout of the Undra repository",
        "C0014",
        "migration notes",
    ] {
        assert!(text.contains(needle), "--help lacks {needle:?}:\n{text}");
    }
}

#[test]
fn upgrade_regenerates_the_bindings_for_real() {
    let require = flag("UNDRA_REQUIRE_TOOLCHAINS");
    if !require && !flag("UNDRA_TEST_UPGRADE_E2E") {
        eprintln!(
            "skipped: it builds the core from a local clone of the repository (minutes); set UNDRA_TEST_UPGRADE_E2E=1, or UNDRA_REQUIRE_TOOLCHAINS=1 to require it"
        );
        return;
    }
    if !common::has_tool("git", "--version") {
        assert!(
            !require,
            "git is not installed (UNDRA_REQUIRE_TOOLCHAINS=1)"
        );
        eprintln!("skipped: git is not installed");
        return;
    }
    // A repository built from this checkout's tree stands in for GitHub: git is told to fetch it
    // instead (`insteadOf`), and Cargo to use git's own fetch, which honours that; the tags v0.0.9
    // and v<current> are its one commit. It is a fresh repository rather than a clone because CI
    // checks out shallowly, and Cargo refuses to fetch a tag whose root is shallow.
    let dir = TempDir::new("upgrade-e2e");
    let clone = dir.path().join("undra-source");
    std::fs::create_dir_all(&clone).unwrap();
    run_ok(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&clone),
    );
    run_ok(Command::new("sh").arg("-c").arg(format!(
        "git -C '{}' archive --format=tar HEAD | tar -x -C '{}'",
        common::repo_root().display(),
        clone.display()
    )));
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args([
            "-c",
            "user.name=undra-test",
            "-c",
            "user.email=undra-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(&clone);
        c
    };
    run_ok(&mut git(&["add", "-A"]));
    run_ok(&mut git(&[
        "commit",
        "--quiet",
        "-m",
        "the tree under test",
    ]));
    for tag in ["v0.0.9", &format!("v{CURRENT}")] {
        run_ok(&mut git(&["tag", tag]));
    }
    let root = dir.path().join("regen");
    run_ok(
        undra()
            .args(["init", "regen", "--platforms", "web", "--dir"])
            .arg(dir.path()),
    );
    let cargo = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
    std::fs::write(
        root.join("core/Cargo.toml"),
        cargo.replace(&format!("v{CURRENT}"), "v0.0.9"),
    )
    .unwrap();
    let toml = std::fs::read_to_string(root.join("undra.toml")).unwrap();
    std::fs::write(
        root.join("undra.toml"),
        toml.replace(&format!("version = \"{CURRENT}\""), "version = \"0.0\""),
    )
    .unwrap();
    let out = run_ok(
        undra()
            .arg("-C")
            .arg(&root)
            .arg("upgrade")
            .env("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.file://{}.insteadOf", clone.display()),
            )
            .env("GIT_CONFIG_VALUE_0", "https://github.com/shreypdev/undra"),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("--- undra upgrade, with the bindings regenerated\n{stdout}\n{stderr}");
    assert!(
        stderr.contains("Regenerating the bindings (undra bindgen)"),
        "{stderr}"
    );
    assert!(
        stdout.contains("Generated bindings for regen-core"),
        "{stdout}"
    );
    assert!(
        std::fs::read_to_string(root.join("core/Cargo.toml"))
            .unwrap()
            .contains(&format!("tag = \"v{CURRENT}\""))
    );
    let ts = std::fs::read_to_string(root.join("generated/ts/package.json")).unwrap();
    assert!(
        ts.contains(&format!("\"@undra/runtime\": \"^{CURRENT}\"")),
        "the regenerated package names the new runtime:\n{ts}"
    );
}
