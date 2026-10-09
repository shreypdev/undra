//! The command line as a user meets it: help text, usage errors, and the shape of the errors.

mod common;

use common::{TempDir, run_err, run_ok, undra};

#[test]
fn top_level_help_teaches_the_workflow() {
    let out = run_ok(undra().arg("--help"));
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "undra init",
        "undra dev",
        "undra build",
        "undra bindgen",
        "undra adopt",
        "undra doctor",
        "undra upgrade",
        "undra drift",
        "error[undra::C00NN]",
    ] {
        assert!(text.contains(needle), "--help lacks {needle:?}:\n{text}");
    }
}

#[test]
fn every_command_has_a_help_with_examples() {
    for command in [
        "init", "bindgen", "build", "dev", "doctor", "adopt", "upgrade", "drift",
    ] {
        let out = run_ok(undra().args([command, "--help"]));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("EXAMPLES"),
            "undra {command} --help has no examples:\n{text}"
        );
        assert!(
            text.contains(&format!("undra {command}")),
            "undra {command} --help does not show its own usage:\n{text}"
        );
    }
}

#[test]
fn specific_help_texts_say_what_matters() {
    let build =
        String::from_utf8_lossy(&run_ok(undra().args(["build", "--help"])).stdout).into_owned();
    assert!(
        build.contains("<Namespace>Core.xcframework")
            && build.contains("jniLibs/<abi>/lib<namespace>.so")
            && build.contains("<namespace>.wasm")
            && build.contains("[core] namespace"),
        "{build}"
    );
    assert!(build.contains("no -force_load"), "{build}");
    let dev = String::from_utf8_lossy(&run_ok(undra().args(["dev", "--help"])).stdout).into_owned();
    assert!(
        dev.contains("127.0.0.1:7443") && dev.contains("no authentication"),
        "{dev}"
    );
    let bindgen =
        String::from_utf8_lossy(&run_ok(undra().args(["bindgen", "--help"])).stdout).into_owned();
    assert!(
        bindgen.contains("--schema") && bindgen.contains("undra_schema_json"),
        "{bindgen}"
    );
}

#[test]
fn the_version_is_printed() {
    // `undra <semver> (<short sha>)`: the sha comes from UNDRA_BUILD_SHA at build time, `unknown`
    // for a build without it.
    let out = run_ok(undra().arg("--version"));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text.trim_end();
    let prefix = format!("undra {} (", env!("CARGO_PKG_VERSION"));
    assert!(line.starts_with(&prefix) && line.ends_with(')'), "{line:?}");
    let sha = &line[prefix.len()..line.len() - 1];
    assert!(
        sha == "unknown" || (sha.len() == 7 && sha.chars().all(|c| c.is_ascii_hexdigit())),
        "the build sha is `unknown` or seven hex digits, got {sha:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_ok(undra().arg("-V")).stdout),
        text,
        "-V and --version agree"
    );
}

#[test]
fn usage_errors_exit_with_2() {
    let (code, stderr) = run_err(undra().arg("frobnicate"));
    assert_eq!(code, 2, "{stderr}");
    let (code, _) = run_err(&mut undra());
    assert_eq!(code, 2, "no command is a usage error");
    let (code, stderr) = run_err(undra().args(["build", "--platform"]));
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn outside_a_project_the_error_says_what_why_and_how_to_fix() {
    let dir = TempDir::new("noproject");
    let (code, stderr) = run_err(undra().args(["build"]).current_dir(dir.path()));
    assert_eq!(code, 1);
    assert!(
        stderr.starts_with("error[undra::C0001]: no undra.toml found"),
        "{stderr}"
    );
    assert!(
        stderr.contains("= note:") && stderr.contains("= help:") && stderr.contains("undra init"),
        "{stderr}"
    );
    assert!(
        stderr.contains("https://shreypdev.github.io/undra/docs/errors.html#C0001"),
        "{stderr}"
    );
}

#[test]
fn a_bad_project_name_suggests_a_good_one() {
    let dir = TempDir::new("badname");
    let (code, stderr) = run_err(
        undra()
            .args(["init", "My App!"])
            .arg("--dir")
            .arg(dir.path()),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("error[undra::C0009]") && stderr.contains("my-app"),
        "{stderr}"
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "nothing was created"
    );
}

#[test]
fn an_invalid_undra_toml_names_the_line() {
    let dir = TempDir::new("badtoml");
    std::fs::write(
        dir.path().join("undra.toml"),
        "[project]\nname = \"x\"\nid = \"com.example.x\"\nplatfroms = []\n",
    )
    .unwrap();
    let (_, stderr) = run_err(undra().args(["build"]).current_dir(dir.path()));
    assert!(
        stderr.contains("error[undra::C0002]")
            && stderr.contains("line 4")
            && stderr.contains("platfroms"),
        "{stderr}"
    );
}

#[test]
fn doctor_reports_each_area_and_gives_an_exit_status() {
    let dir = TempDir::new("doctor");
    let out = undra()
        .arg("doctor")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("undra doctor: no project here"), "{text}");
    for section in ["Rust", "iOS", "Android", "Web"] {
        assert!(text.contains(section), "no {section} section:\n{text}");
    }
    let failed = text.contains("FAIL");
    assert_eq!(
        out.status.success(),
        !failed,
        "the exit status follows the failures:\n{text}"
    );
    // A gap always comes with its fix.
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("FAIL") {
            let next = text.lines().nth(i + 1).unwrap_or_default();
            assert!(
                next.trim_start().starts_with("fix:"),
                "FAIL without a fix: {line}\n{text}"
            );
        }
    }
}

#[test]
fn doctor_can_be_limited_to_a_platform() {
    let dir = TempDir::new("doctor-web");
    let out = undra()
        .args(["doctor", "--platform", "web"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Web") && !text.contains("\nAndroid\n") && !text.contains("\niOS\n"),
        "{text}"
    );
    let (code, stderr) = run_err(
        undra()
            .args(["doctor", "--platform", "tv"])
            .current_dir(dir.path()),
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("C0009"), "{stderr}");
}
