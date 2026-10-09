//! `undra drift REFERENCE OTHER...`: where recordings of one flow, made on different platforms,
//! diverged at the boundary (`undra_testkit::drift`).

use std::path::{Path, PathBuf};

use undra_meta::Schema;
use undra_testkit::Recording;
use undra_testkit::decode::SchemaIndex;
use undra_testkit::drift::{Compared, Report, Rules, Session as DriftSession, compare};

use crate::cli::DriftArgs;
use crate::error::{CliError, Result};
use crate::project::Project;
use crate::schema::parse_schema_json;

use super::Env;

/// Runs `undra drift`. `Ok(false)` is `--exit-code` finding a divergence: the exit status is 1,
/// and the report has been printed.
///
/// # Errors
///
/// `C0009` for fewer than two files, a file that is not a recording, or a recording of another
/// schema than the reference's (or than `--schema`'s); `C0010` for a file that cannot be read;
/// `C0002` for a `--schema` file that is not a schema; and, when the schema has to come from the
/// project's core, whatever building it raises.
pub fn run(env: &Env<'_>, args: &DriftArgs) -> Result<bool> {
    let start = env.start_dir()?;
    if args.files.len() < 2 {
        return Err(CliError::bad_argument(
            format!(
                "`undra drift` compares a reference recording with at least one other, and {} {} given",
                args.files.len(),
                if args.files.len() == 1 { "was" } else { "were" }
            ),
            "the first file is the reference and every other file is compared with it, so one file compares with nothing",
            "give the reference and then the others (`undra drift ios.json android.json web.json`); \
             `undra dev --record FILE` records a session",
        ));
    }
    let recordings = args
        .files
        .iter()
        .map(|file| read_recording(&start, file))
        .collect::<Result<Vec<_>>>()?;
    let schema = schema(env, &start, args)?;

    // Every file has to belong to the same core as the reference (and the schema, when given):
    // ids and bytes of another schema compare as noise.
    let (reference_file, reference) = &recordings[0];
    for (file, recording) in &recordings[1..] {
        if recording.schema_hash != reference.schema_hash {
            return Err(other_schema(
                file,
                recording.schema_hash,
                &format!("the reference {reference_file}"),
                reference.schema_hash,
            ));
        }
    }
    if let Some(schema) = &schema
        && schema.hash() != reference.schema_hash
    {
        return Err(other_schema(
            reference_file,
            reference.schema_hash,
            "the schema",
            schema.hash(),
        ));
    }

    let rules = Rules::new(schema.as_ref().map(SchemaIndex::new)).with_ignores(&args.ignore);
    let session = |file: &str, recording: &Recording| {
        let session = DriftSession::from_recording(recording);
        if recording.platform.is_none() {
            session.with_label(file)
        } else {
            session
        }
    };
    let reference_session = session(reference_file, reference);
    let compared = recordings[1..]
        .iter()
        .map(|(file, recording)| {
            let other = session(file, recording);
            Compared {
                file: file.clone(),
                label: other.label.clone(),
                divergences: compare(&reference_session, &other, &rules),
            }
        })
        .collect();
    let report = Report {
        reference: reference_file.clone(),
        schema: schema.as_ref().map(Schema::hash),
        user_ignores: rules.user_ignores(),
        compared,
    };
    for line in report.render().lines() {
        env.ui.line(line);
    }
    Ok(!(args.exit_code && report.total() > 0))
}

/// `C0009`: `file` (schema `hash`) is not a recording of `other` (schema `other_hash`).
fn other_schema(file: &str, hash: u64, other: &str, other_hash: u64) -> CliError {
    CliError::bad_argument(
        format!(
            "{file} (schema {hash:#018x}) is not a recording of the same core as {other} (schema {other_hash:#018x})"
        ),
        "a recording holds the ids and bytes of one schema, and compared across schemas they are noise, not drift",
        "record every platform on the same build of the core (`undra dev --record FILE`, one server per platform), \
         and give the schema of that build with --schema (or run inside the project)",
    )
}

/// Reads `file` (relative to `start`) as a recording, labelled as the user named it.
fn read_recording(start: &Path, file: &Path) -> Result<(String, Recording)> {
    let path = start.join(file);
    let shown = file.display().to_string();
    let text = std::fs::read_to_string(&path).map_err(|e| CliError::io("read", &path, &e))?;
    let recording = Recording::from_json(&text).map_err(|e| {
        CliError::bad_argument(
            format!("{shown} is not a recording: {e}"),
            "`undra drift` compares `undra.recording` files (docs/TESTING.md), as `undra dev --record` and the \
             platform kits' recorders write them",
            "give recordings of the flow, one per platform (`undra dev --record ios.json`)",
        )
    })?;
    Ok((shown, recording))
}

/// The schema that names and decodes the recordings: `--schema FILE`, else the project's
/// `schema.json`, else the project's core built and asked (as `undra schema export` does), else
/// none (ids and bytes are compared, and the report says so).
fn schema(env: &Env<'_>, start: &Path, args: &DriftArgs) -> Result<Option<Schema>> {
    if let Some(file) = &args.schema {
        let path = start.join(file);
        let text = std::fs::read_to_string(&path).map_err(|e| CliError::io("read", &path, &e))?;
        return parse(&text, &file.display().to_string()).map(Some);
    }
    let Ok(project) = Project::discover(start) else {
        return Ok(None);
    };
    let file: PathBuf = project.root.join(super::schema::DEFAULT_FILE);
    if let Ok(text) = std::fs::read_to_string(&file) {
        return parse(&text, super::schema::DEFAULT_FILE).map(Some);
    }
    let session = env.session()?;
    super::bindgen::schema_from_core(&session, false, false).map(Some)
}

/// Parses `text`, naming `label` in the error.
fn parse(text: &str, label: &str) -> Result<Schema> {
    parse_schema_json(text, "core").map_err(|mut e| {
        e.what = format!("{label}: {}", e.what);
        e
    })
}

#[cfg(test)]
mod tests {
    use crate::error::Code;

    use super::*;

    #[test]
    fn another_schema_names_both_hashes_and_the_fix() {
        let e = other_schema("android.json", 0x11, "the reference ios.json", 0x22);
        assert_eq!(e.code, Code::BadArgument);
        assert!(
            e.what.contains("android.json (schema 0x0000000000000011)")
                && e.what
                    .contains("the reference ios.json (schema 0x0000000000000022)"),
            "{e}"
        );
        assert!(e.fix.contains("--schema"), "{e}");
    }

    #[test]
    fn a_file_that_is_not_a_recording_names_itself() {
        let dir = std::env::temp_dir().join(format!("undra-drift-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not.json");
        std::fs::write(&file, "{\"format\":\"other\"}").unwrap();
        let e = read_recording(&dir, Path::new("not.json")).unwrap_err();
        assert_eq!(e.code, Code::BadArgument);
        assert!(e.what.starts_with("not.json is not a recording: "), "{e}");
        let e = read_recording(&dir, Path::new("missing.json")).unwrap_err();
        assert_eq!(e.code, Code::Io);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
