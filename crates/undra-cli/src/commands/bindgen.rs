//! `undra bindgen`.

use std::path::{Path, PathBuf};

use undra_bindgen::Generator;
use undra_meta::Schema;

use crate::bindgen::{self, Plan, canonicalize_lenient};
use crate::builds::host;
use crate::cli::BindgenArgs;
use crate::config::{Platform, ProjectConfig};
use crate::error::{CliError, Code, Result};
use crate::names::Names;
use crate::project::Project;
use crate::runtimes::Runtimes;
use crate::schema::{self, parse_schema_json};
use crate::session::Session;

use super::Env;

/// Runs `undra bindgen`.
///
/// # Errors
///
/// See the modules it drives: [`crate::schema`], [`crate::bindgen`], [`crate::builds`].
pub fn run(env: &Env<'_>, args: &BindgenArgs) -> Result<()> {
    let ui = env.ui;
    // ADR-063: a released project's packages name the release where the environment says (GitHub unless a mirror is
    // set, which is announced below); a malformed setting stops the command before the core is built.
    let dist = crate::dist::Dist::from_sys(env.sys)?;
    let project = match Project::discover(&env.start_dir()?) {
        Ok(project) => Some(project),
        // `--schema` needs no project: generating from a file is the fallback of SPEC 13.
        Err(e) if e.code == Code::NoProject && args.schema.is_some() => None,
        Err(e) => return Err(e),
    };
    let session = project.map(|p| Session::new(p, env.sys, ui));
    // The Swift package follows the repository the core takes Undra from, whatever the environment says (below), and
    // the warning about the git URL override says which one it names.
    let core_repository = session.as_ref().and_then(|s| match s.core() {
        Ok(crate::cargo::CoreInfo {
            undra: crate::cargo::UndraSource::Git { url, .. },
            ..
        }) => Some(url.clone()),
        _ => None,
    });
    for warning in
        crate::dist::bindgen_override_warnings(|key| env.sys.env(key), core_repository.as_deref())
    {
        ui.warn(&warning);
    }

    let crate_name = crate_name(session.as_ref(), args);
    let schema = match (&args.schema, &args.library, &session) {
        (Some(file), _, _) => read_schema_file(file, &crate_name)?,
        (None, Some(library), Some(session)) => {
            schema_from_library(session, library, &crate_name, args.docs)?
        }
        (None, None, Some(session)) => schema_from_core(session, args.release, args.docs)?,
        (None, _, None) => return Err(CliError::no_project(&env.start_dir()?)),
    };
    if schema_is_empty(&schema) {
        ui.warn("the schema is empty: the core has no `#[undra::api]` items that are `pub`, or its registrations were not linked");
    }

    let plan = plan(session.as_ref(), args, &schema, &env.start_dir()?, &dist)?;
    let files = bindgen::plan_files(&schema, &plan)?;
    // R7: the bindings are this `undra`'s; a released project's runtimes are the release it pins.
    let other_release = session
        .as_ref()
        .and_then(|s| release_mismatch(&s.project.config, crate::config::UNDRA_VERSION));
    if let Some(pinned) = &other_release {
        let cli = crate::config::UNDRA_VERSION;
        ui.warn(&format!(
            "this project is on Undra {pinned} (undra.toml) and this undra is {cli}: the bindings it writes are {cli}'s, and the project's runtimes are {pinned}'s. {}",
            release_advice(pinned, cli)
        ));
    }

    if args.check {
        let problems = bindgen::check(&plan.out, &files);
        if problems.is_empty() {
            ui.line(&format!(
                "The bindings in {} are up to date (schema hash {:#018x}).",
                plan.out.display(),
                schema.hash()
            ));
            return Ok(());
        }
        let (why, fix) = match &other_release {
            Some(pinned) => (
                format!(
                    "they are generated from the core's schema by `undra`, and this undra is {} while the project is on Undra {pinned}: another release generates other bindings (or the core changed, or they were edited by hand)",
                    crate::config::UNDRA_VERSION
                ),
                release_advice(pinned, crate::config::UNDRA_VERSION),
            ),
            None => (
                "they are generated from the core's schema and the project's undra.toml, and one of them changed (or they were edited by hand)".to_owned(),
                "run `undra bindgen` and commit the result".to_owned(),
            ),
        };
        return Err(CliError::new(
            Code::Bindgen,
            format!(
                "the generated bindings in {} are out of date",
                plan.out.display()
            ),
            why,
            fix,
        )
        .with_detail(
            problems
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }

    let applied = bindgen::apply(&plan.out, &files)?;
    let shown = |p: &Path| {
        session
            .as_ref()
            .and_then(|s| p.strip_prefix(&s.project.root).ok())
            .unwrap_or(p)
            .display()
            .to_string()
    };
    ui.line(&format!(
        "Generated bindings for {} (schema hash {:#018x}) in {}:",
        schema.crate_name,
        schema.hash(),
        shown(&plan.out)
    ));
    for platform in &plan.platforms {
        let (dir, what) = match platform {
            Platform::Ios => (
                "swift",
                format!("Swift package `{}`", plan.generator.swift_module),
            ),
            Platform::Android => (
                "kotlin",
                format!("Kotlin module, package `{}`", plan.generator.kotlin_package),
            ),
            Platform::Web => (
                "ts",
                format!("TypeScript package `{}`", plan.generator.ts_package_name()),
            ),
        };
        let count = files
            .iter()
            .filter(|f| f.path.starts_with(&format!("{dir}/")))
            .count();
        ui.line(&format!("  {dir:<7} {what} ({count} files)"));
    }
    ui.line(&format!(
        "  {} written, {} unchanged, {} removed",
        applied.written.len(),
        applied.unchanged,
        applied.removed.len()
    ));
    Ok(())
}

/// The release a released project pins (`[undra] version`, in full) when it is not `cli`'s; `None` for a project on a
/// checkout (`[undra] path`), whose runtimes are the checkout's.
fn release_mismatch(config: &ProjectConfig, cli: &str) -> Option<String> {
    if config.undra_path.is_some() {
        return None;
    }
    let pinned = crate::dist::full_version(&config.undra_version);
    (pinned != cli).then_some(pinned)
}

/// What to do about a project on release `pinned` worked on with an `undra` of release `cli`: use the project's release
/// (what its CI installs), or move the project forward when this `undra` is newer (`undra upgrade` never moves one back).
fn release_advice(pinned: &str, cli: &str) -> String {
    let install = format!(
        "use undra {pinned}, the project's release and what its CI installs (`curl -fsSL https://shreypdev.github.io/undra/install.sh | UNDRA_VERSION={pinned} sh`)"
    );
    match (
        crate::semver::Semver::parse(pinned),
        crate::semver::Semver::parse(cli),
    ) {
        (Some(p), Some(c)) if p < c => format!(
            "Run `undra upgrade` to move the project to {cli} (and commit what it changes), or {install}"
        ),
        _ => format!("{}{}", install[..1].to_uppercase(), &install[1..]),
    }
}

fn schema_is_empty(schema: &Schema) -> bool {
    schema.records.is_empty()
        && schema.enums.is_empty()
        && schema.objects.is_empty()
        && schema.functions.is_empty()
        && schema.ports.is_empty()
        && schema.queries.is_empty()
}

/// The crate name that labels a schema with none: `--crate-name`, else the project's core.
fn crate_name(session: Option<&Session<'_>>, args: &BindgenArgs) -> String {
    if let Some(name) = &args.crate_name {
        return name.clone();
    }
    match session {
        Some(s) => s
            .project
            .config
            .core_package
            .clone()
            .unwrap_or_else(|| Names::derive(&s.project.config.name).core_package),
        None => "core".to_owned(),
    }
}

fn read_schema_file(file: &Path, crate_name: &str) -> Result<Schema> {
    let text = std::fs::read_to_string(file).map_err(|e| CliError::io("read", file, &e))?;
    parse_schema_json(&text, crate_name)
        .map_err(|e| CliError::bad_config(file, e.what.clone(), e.fix.clone()))
}

/// Builds the core as a host library and asks it for its schema.
///
/// The library's schema carries the doc comments (the same JSON the dev runner prints, ADR-050),
/// so `--docs` only decides whether the bindings keep them: without it they are dropped, which is
/// what generated bindings have always been by default.
pub(crate) fn schema_from_core(session: &Session<'_>, release: bool, docs: bool) -> Result<Schema> {
    session
        .ui
        .step("Building the core for this machine to read its schema");
    let library = host::cdylib(session, release)?;
    let core = session.core()?;
    let namespace = session.namespace()?;
    session.ui.step("Reading the schema from the built library");
    schema::load_from_library(&library, &namespace, &core.package, docs)
}

/// Reads the schema from a host library that was built already: no Cargo, no build. The library is the one
/// `undra build --platform host` writes (or a build system's copy of it), and the entry it exports is named by the
/// project's namespace.
fn schema_from_library(
    session: &Session<'_>,
    library: &Path,
    crate_name: &str,
    docs: bool,
) -> Result<Schema> {
    let library = canonicalize_lenient(library);
    let namespace = session.namespace()?;
    session.ui.step("Reading the schema from the built library");
    schema::load_from_library(&library, &namespace, crate_name, docs)
}

/// The generator configuration and output locations.
fn plan(
    session: Option<&Session<'_>>,
    args: &BindgenArgs,
    schema: &Schema,
    cwd: &Path,
    dist: &crate::dist::Dist,
) -> Result<Plan> {
    let mut generator = Generator::for_crate(&schema.crate_name);
    // The iOS floor and the Swift observation mode (ADR-045): the project's, and what the command line says.
    let observation = match &args.swift_observation {
        Some(text) => Some(text.parse().map_err(|e: String| {
            CliError::bad_argument(
                format!("--swift-observation: {e}"),
                "the Swift stores are either `@Observable` (iOS 17 and later) or `ObservableObject`s (iOS 15 and later)",
                "use --swift-observation observation, or --swift-observation observable-object",
            )
        })?),
        None => None,
    };
    let standalone = ProjectConfig::new("standalone", "com.example.standalone", Vec::new());
    let config = session.map_or(&standalone, |s| &s.project.config);
    bindgen::configure_swift(
        &mut generator,
        config,
        observation,
        args.ios_deployment_target.as_deref(),
    )?;
    let mut platforms = Platform::ALL.to_vec();
    let mut runtimes = Runtimes::released(crate::config::UNDRA_VERSION, dist);
    let mut default_out = cwd.join("generated");
    let mut lint_exclusions = crate::config::LintExclusions::default();
    if let Some(session) = session {
        let project = &session.project;
        let cfg = &project.config.bindings;
        generator.swift_module = project.swift_module();
        generator.kotlin_package = project.kotlin_package();
        // The namespace names the generated entry point (ADR-044). From a schema file, without a
        // core Cargo can describe, the default is derived from the schema's crate name.
        if project.config.core_namespace.is_some() || session.core().is_ok() {
            generator.namespace = session.namespace()?;
        }
        if let Some(scope) = &cfg.ts_scope {
            generator.ts_scope.clone_from(scope);
        }
        if let Some(package) = &cfg.ts_package {
            generator.ts_package.clone_from(package);
        }
        generator.ts_js_number = cfg.ts_js_number;
        lint_exclusions = cfg.lint_exclusions;
        if let Some(typed) = cfg.swift_typed_throws {
            generator.swift_typed_throws = typed;
        }
        platforms.clone_from(&project.config.platforms);
        // ADR-063: the Swift package names the repository the core's `undra` crates come from (core/Cargo.toml), the
        // project's own record of where it gets Undra, so a project made against a mirror stays on it whatever the
        // environment of this run says.
        let mut dist = dist.clone();
        if let Ok(crate::cargo::CoreInfo {
            undra: crate::cargo::UndraSource::Git { url, .. },
            ..
        }) = session.core()
        {
            dist.follow_repository(url);
        }
        runtimes = Runtimes::for_project(project, &dist);
        default_out = project.generated_dir();
    }
    if let Some(list) = &args.platforms {
        platforms = Platform::parse_list(list)?;
    }
    let out: PathBuf = match &args.out {
        Some(out) if out.is_absolute() => out.clone(),
        Some(out) => cwd.join(out),
        None => default_out,
    };
    Ok(Plan {
        generator,
        platforms,
        runtimes,
        out: canonicalize_lenient(&out),
        lint_exclusions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(version: &str, path: Option<&str>) -> ProjectConfig {
        let mut config = ProjectConfig::new("demo", "com.example.demo", Vec::new());
        config.undra_version = version.to_owned();
        config.undra_path = path.map(ToOwned::to_owned);
        config
    }

    #[test]
    fn a_released_project_on_another_release_is_named_and_a_checkout_is_not() {
        assert_eq!(release_mismatch(&config("1.0.0", None), "1.0.0"), None);
        // A two-part version an older `undra init` wrote is the first release of its line.
        assert_eq!(release_mismatch(&config("1.0", None), "1.0.0"), None);
        assert_eq!(
            release_mismatch(&config("1.0.0", None), "1.0.1").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            release_mismatch(&config("0.9.0", Some("../undra")), "1.0.0"),
            None
        );
    }

    #[test]
    fn the_advice_moves_a_project_forward_only() {
        // This undra is newer: upgrade the project, or use its release.
        let newer = release_advice("1.0.0", "1.0.1");
        assert!(
            newer.starts_with("Run `undra upgrade` to move the project to 1.0.1")
                && newer.contains("UNDRA_VERSION=1.0.0 sh"),
            "{newer}"
        );
        // This undra is older: `undra upgrade` would refuse (C0014), so only the project's release is offered.
        let older = release_advice("1.0.1", "1.0.0");
        assert!(
            older.starts_with("Use undra 1.0.1") && !older.contains("undra upgrade"),
            "{older}"
        );
    }
}
