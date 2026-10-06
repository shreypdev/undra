//! `undra upgrade`: move a project to the version of this `undra`.
//!
//! The planning is in [`crate::upgrade`] (every pin, and the edits), the notes are in
//! [`crate::migrations`]; this module reads the project, prints the plan, writes the files,
//! regenerates the bindings and prints the notes of every release the project crosses.

use std::path::{Path, PathBuf};

use crate::cli::{BindgenArgs, UpgradeArgs};
use crate::error::{CliError, Code, Result};
use crate::migrations::{self, Migration};
use crate::project::Project;
use crate::semver::Semver;
use crate::upgrade::{self, FileEdit, Plan, Why};

use super::Env;

/// Runs `undra upgrade`.
///
/// # Errors
///
/// `C0001` outside a project, `C0014` when the project is on a newer release than this `undra`,
/// `C0005` when the core is missing (nothing is written), `C0010` when a file cannot be written
/// (nothing is written), and whatever `undra bindgen` reports (the pins are already moved then, and
/// the message says so).
pub fn run(env: &Env<'_>, args: &UpgradeArgs) -> Result<()> {
    let project = Project::discover(&env.start_dir()?)?;
    let ui = env.ui;
    let target = Semver::parse(crate::version::SEMVER)
        .expect("the CLI's own version is a version: a test checks it");
    // ADR-063: the pins name the release where the environment says (GitHub unless a mirror is set).
    let dist = crate::dist::Dist::announced_for_upgrade(env.sys, &env.ui)?;
    let plan = upgrade::plan(&project.root, &project.generated_dir(), &target, &dist);

    // A dependency on a checkout of the repository is the version of that checkout.
    if plan.on_a_checkout() {
        ui.line(&path_message(&project, &plan));
        return Ok(());
    }
    if let Some((file, pin)) = plan.ahead() {
        let installed = std::env::current_exe()
            .ok()
            .map(|exe| exe.canonicalize().unwrap_or(exe))
            .and_then(|exe| {
                channel_of(
                    &exe,
                    env.sys.home().as_deref(),
                    env.sys.env("UNDRA_HOME"),
                    env.sys.env("CARGO_HOME"),
                )
            });
        return Err(ahead(&project, file, &pin.shown, &target, installed));
    }

    let current = plan.current();
    ui.line(&format!(
        "{} {} is on Undra {}; this undra is {target}",
        ui.bold_out("undra upgrade:"),
        project.config.name,
        current.as_ref().map_or_else(
            || "an unreleased commit (no pin names a version)".to_owned(),
            ToString::to_string
        ),
    ));
    for (file, unmovable) in &plan.unmovable {
        match unmovable.why {
            Why::Fork => ui.warn(&format!(
                "{}:{}: depends on a fork of Undra, which has no release tags to move to; left as it is ({})",
                file.display(),
                unmovable.line,
                unmovable.text
            )),
            Why::NotALiteral => ui.warn(&format!(
                "{}:{}: the Undra version is not written out here, so it is left as it is; move it where it is defined ({})",
                file.display(),
                unmovable.line,
                unmovable.text
            )),
            Why::Path => {}
        }
    }

    if plan.files.is_empty() {
        ui.line(&format!(
            "Every pin already names {target} (or the release line it belongs to): nothing to change."
        ));
        if plan.pins.is_empty() {
            ui.hint("no pin of an Undra version was found (core/Cargo.toml, undra.toml, the apps' runtime references and the workflow are where `undra init` writes them)");
        }
        return Ok(());
    }

    ui.line("");
    for edit in &plan.files {
        ui.line(&render_edit(edit));
    }
    ui.line(&format!(
        "{} line{} in {} file{}.",
        plan.change_count(),
        if plan.change_count() == 1 { "" } else { "s" },
        plan.files.len(),
        if plan.files.len() == 1 { "" } else { "s" }
    ));

    if args.dry_run {
        ui.line("");
        print_notes(env, current.as_ref(), &target);
        ui.line("");
        ui.line("--dry-run: nothing was written. Run `undra upgrade` to apply it.");
        return Ok(());
    }

    if !args.no_bindgen && !project.core_manifest().is_file() {
        // The bindings could not be regenerated after the pins moved: say so before moving any.
        return Err(CliError::new(
            Code::BadCore,
            format!(
                "there is no core crate at {}; nothing was written",
                project.core_manifest().display()
            ),
            "`undra upgrade` moves the pins and then regenerates the bindings from the core, and without the core the second half cannot happen",
            "fix `[core] path` in undra.toml (or restore the core), then run `undra upgrade` again; `--no-bindgen` moves the pins alone",
        ));
    }
    write_all(&project.root, &plan.files)?;
    ui.line(&format!(
        "Updated {} file{}.",
        plan.files.len(),
        if plan.files.len() == 1 { "" } else { "s" }
    ));
    for hint in lock_file_hints(&plan) {
        ui.hint(&hint);
    }
    if plan.gradle_repository_missing {
        if let Some(lines) = dist.gradle_repository("        ", true) {
            ui.warn(&format!(
                "the Kotlin runtime now comes from {} (ADR-063), and no Gradle settings script of the project could be given it: add it to the repositories your Android build resolves from (the `dependencyResolutionManagement {{ repositories {{ }} }}` block of settings.gradle.kts, or `allprojects {{ repositories {{ }} }}` of an older build):\n{lines}",
                dist.maven_repo.as_deref().unwrap_or_default()
            ));
        }
    }

    if args.no_bindgen {
        ui.hint("bindings not regenerated (--no-bindgen): run `undra bindgen` before you build");
    } else {
        ui.line("");
        ui.step("Regenerating the bindings (undra bindgen)");
        let bindgen_args = BindgenArgs {
            schema: None,
            library: None,
            out: None,
            release: false,
            platforms: None,
            check: false,
            docs: args.docs,
            crate_name: None,
            swift_observation: None,
            ios_deployment_target: None,
        };
        if let Err(e) = super::bindgen::run(env, &bindgen_args) {
            ui.warn("the pins above are already moved; fix what `undra bindgen` reports below and run it again");
            return Err(e);
        }
    }
    ui.line("");
    print_notes(env, current.as_ref(), &target);
    ui.line("");
    ui.line("Review with `git diff`, then build.");
    Ok(())
}

/// Writes every edit or none: a project is never left with some pins moved and others not.
///
/// Before the first write each file is checked to be unchanged since it was read and writable
/// (opened for writing, not truncated). A write that fails after that (a full disk) puts the files
/// already written back as they were.
///
/// # Errors
///
/// `C0010` naming the file, saying that nothing was written (or, should restoring fail too, which
/// files were left changed).
fn write_all(root: &Path, files: &[FileEdit]) -> Result<()> {
    let nothing = |path: &Path, what: String| {
        CliError::new(
            Code::Io,
            format!(
                "could not write {}: {what}; nothing was written",
                path.display()
            ),
            "`undra upgrade` moves every pin of the project or none, so the project is never half upgraded",
            "make the file writable (or close what holds it) and run `undra upgrade` again",
        )
    };
    for edit in files {
        let path = root.join(&edit.path);
        match std::fs::read_to_string(&path) {
            Ok(now) if now == edit.before => {}
            Ok(_) => {
                return Err(nothing(
                    &edit.path,
                    "it changed while `undra upgrade` ran".to_owned(),
                ));
            }
            Err(e) => return Err(nothing(&edit.path, e.to_string())),
        }
        if let Err(e) = std::fs::OpenOptions::new().write(true).open(&path) {
            return Err(nothing(&edit.path, e.to_string()));
        }
    }
    for (i, edit) in files.iter().enumerate() {
        let path = root.join(&edit.path);
        if let Err(e) = std::fs::write(&path, &edit.after) {
            let mut left = Vec::new();
            for done in &files[..i] {
                if std::fs::write(root.join(&done.path), &done.before).is_err() {
                    left.push(done.path.display().to_string());
                }
            }
            if left.is_empty() {
                return Err(nothing(&edit.path, e.to_string()));
            }
            return Err(CliError::new(
                Code::Io,
                format!(
                    "could not write {}: {e}, and could not put back {}",
                    edit.path.display(),
                    left.join(", ")
                ),
                "`undra upgrade` moves every pin or none, and restoring the files it had already written failed too",
                "`git checkout -- .` (or your editor's undo) returns the project to where it was; then run `undra upgrade` again",
            ));
        }
    }
    Ok(())
}

/// What to say about a project that depends on a checkout of the repository.
fn path_message(project: &Project, plan: &Plan) -> String {
    let mut out = format!(
        "{} {} depends on a checkout of the Undra repository, so its version is whatever that checkout is:\n",
        "undra upgrade:", project.config.name
    );
    for (file, u) in plan.unmovable.iter().filter(|(_, u)| u.why == Why::Path) {
        out.push_str(&format!("  {}:{}  {}\n", file.display(), u.line, u.text));
    }
    out.push_str("Nothing was changed. Update the checkout itself (`git pull` there, then `undra bindgen` here) to move the project; to follow releases instead, create a project without `--undra-path`.");
    out
}

/// How the running `undra` was installed, as far as its path tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    /// The installer (`install.sh`): `$UNDRA_HOME/bin`, by default `~/.undra/bin`.
    Installer,
    /// `cargo install`: `$CARGO_HOME/bin`, by default `~/.cargo/bin`.
    Cargo,
}

impl Channel {
    /// The command that updates `undra` through this channel.
    fn update_command(self) -> &'static str {
        match self {
            Channel::Installer => "curl -fsSL https://shreypdev.github.io/undra/install.sh | sh",
            Channel::Cargo => {
                "cargo install --locked --force --git https://github.com/shreypdev/undra undra-cli"
            }
        }
    }
}

/// The channel that installed the binary at `exe` (symlinks resolved), when its directory says so; `None` for anything
/// else (a build in a checkout's `target/`, a copy somewhere).
fn channel_of(
    exe: &Path,
    home: Option<&Path>,
    undra_home: Option<String>,
    cargo_home: Option<String>,
) -> Option<Channel> {
    let dir = exe.parent()?;
    let bin = |root: Option<PathBuf>| root.is_some_and(|root| dir == root.join("bin"));
    if bin(undra_home.map(PathBuf::from)) || bin(home.map(|h| h.join(".undra"))) {
        return Some(Channel::Installer);
    }
    if bin(cargo_home.map(PathBuf::from)) || bin(home.map(|h| h.join(".cargo"))) {
        return Some(Channel::Cargo);
    }
    None
}

/// What to run to update `undra`: the channel's command when it is known, else the installer's, and cargo's for a
/// binary cargo built (a path that says nothing, such as a package manager's directory, gets the installer).
fn update_help(installed: Option<Channel>) -> String {
    match installed {
        Some(channel) => format!("`{}`", channel.update_command()),
        None => format!(
            "`{}`, or `{}` if you installed it with cargo",
            Channel::Installer.update_command(),
            Channel::Cargo.update_command()
        ),
    }
}

/// `C0014`: the project is ahead of this `undra`.
fn ahead(
    project: &Project,
    file: &Path,
    shown: &str,
    target: &Semver,
    installed: Option<Channel>,
) -> CliError {
    CliError::new(
        Code::UndraMismatch,
        format!(
            "{} pins Undra at {shown}, newer than this undra ({target})",
            file.display()
        ),
        format!(
            "`undra upgrade` moves a project forward only, and a CLI older than the project's runtimes would generate bindings they do not match ({} is {})",
            project.config.name,
            project.root.display()
        ),
        format!(
            "update this `undra` first ({}) and run `undra upgrade` again",
            update_help(installed)
        ),
    )
}

/// One file's edits as a diff: the lines that go and the lines that come.
fn render_edit(edit: &FileEdit) -> String {
    let mut out = format!("{}\n", edit.path.display());
    for change in &edit.changes {
        out.push_str(&format!("  line {}  ({})\n", change.line, change.what));
        if !change.old.is_empty() {
            out.push_str(&format!("    - {}\n", change.old));
        }
        if !change.new.is_empty() {
            out.push_str(&format!("    + {}\n", change.new));
        }
    }
    out.trim_end().to_owned()
}

/// What to run after the pins moved so the lock files follow.
fn lock_file_hints(plan: &Plan) -> Vec<String> {
    let mut hints = Vec::new();
    if plan.files.iter().any(|f| f.path.ends_with("package.json")) {
        hints.push("`npm install` in the web app refreshes its package-lock.json".to_owned());
    }
    if plan.files.iter().any(|f| f.path.ends_with("Cargo.toml")) {
        hints.push("Cargo.lock follows at the next build (it fetches the new tag, so it needs the network)".to_owned());
    }
    hints
}

/// Prints the notes of every release between `from` and `to`.
fn print_notes(env: &Env<'_>, from: Option<&Semver>, to: &Semver) {
    let ui = env.ui;
    let crossed = migrations::between(from, to);
    let heading = match from {
        Some(from) => format!("Migration notes, {from} to {to}"),
        None => format!("Migration notes, up to {to} (the project's version is not known)"),
    };
    if crossed.is_empty() {
        ui.line(&format!(
            "{heading}: none. No release in between changed anything an app author has to know."
        ));
        return;
    }
    ui.line(&ui.bold_out(&heading));
    for migration in crossed {
        ui.line(&render_migration(migration));
    }
}

/// A release's notes, wrapped for a terminal.
fn render_migration(migration: &Migration) -> String {
    let mut out = format!("\n{}  {}\n", migration.version, migration.title);
    for note in migration.notes {
        let marker = format!("  [{}] ", note.kind.marker());
        out.push_str(&wrap(note.text, &marker, 100));
    }
    out.trim_end().to_owned()
}

/// Wraps `text` at `width` columns; the first line starts with `marker`, the others are indented
/// to match.
fn wrap(text: &str, marker: &str, width: usize) -> String {
    let indent = " ".repeat(marker.chars().count());
    let mut out = String::new();
    let mut line = marker.to_owned();
    let mut empty = true;
    for word in text.split_whitespace() {
        if !empty && line.chars().count() + 1 + word.chars().count() > width {
            out.push_str(&line);
            out.push('\n');
            line = indent.clone();
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    out.push_str(&line);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{Kind, Note};

    #[test]
    fn the_update_help_names_the_channel_the_binary_came_from() {
        let home = Path::new("/Users/me");
        let of = |exe: &str| channel_of(Path::new(exe), Some(home), None, None);
        // A binary under a Homebrew prefix (from a formula of someone else's, say) is no channel of ours: it gets the
        // installer's help, like any other path that says nothing.
        assert_eq!(of("/opt/homebrew/Cellar/undra/1.0.0/bin/undra"), None);
        assert_eq!(of("/opt/homebrew/bin/undra"), None);
        assert_eq!(
            of("/home/linuxbrew/.linuxbrew/Cellar/undra/1.0.0/bin/undra"),
            None
        );
        assert_eq!(of("/Users/me/.undra/bin/undra"), Some(Channel::Installer));
        assert_eq!(of("/Users/me/.cargo/bin/undra"), Some(Channel::Cargo));
        assert_eq!(of("/Users/me/src/undra/target/debug/undra"), None);
        assert_eq!(of("/usr/local/bin/undra"), None);
        // UNDRA_HOME and CARGO_HOME move the directories.
        assert_eq!(
            channel_of(
                Path::new("/opt/tools/undra/bin/undra"),
                Some(home),
                Some("/opt/tools/undra".into()),
                None
            ),
            Some(Channel::Installer)
        );
        assert_eq!(
            channel_of(
                Path::new("/opt/cargo/bin/undra"),
                Some(home),
                None,
                Some("/opt/cargo".into())
            ),
            Some(Channel::Cargo)
        );
        assert_eq!(
            update_help(Some(Channel::Installer)),
            "`curl -fsSL https://shreypdev.github.io/undra/install.sh | sh`"
        );
        let all = update_help(None);
        for needle in ["install.sh | sh", "cargo install"] {
            assert!(all.contains(needle), "{all}");
        }
        assert!(all.starts_with("`curl"), "the installer comes first: {all}");
        for gone in ["brew", "npm"] {
            assert!(!all.contains(gone), "{all}");
        }
    }

    #[test]
    fn notes_are_wrapped_with_a_hanging_indent() {
        let text = wrap(
            "one two three four five six seven eight nine ten",
            "  [do] ",
            24,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() > 2, "{text}");
        assert!(lines[0].starts_with("  [do] one"), "{text}");
        assert!(
            lines[1].starts_with("       ") && !lines[1].starts_with("        "),
            "{text}"
        );
        assert!(lines.iter().all(|l| l.chars().count() <= 24), "{text}");
    }

    #[test]
    fn a_migration_prints_its_version_title_and_marked_notes() {
        let m = Migration {
            version: "9.9.9",
            title: "Something changed",
            notes: &[
                Note {
                    kind: Kind::Action,
                    text: "Do this.",
                },
                Note {
                    kind: Kind::New,
                    text: "A new thing.",
                },
            ],
        };
        let text = render_migration(&m);
        assert_eq!(
            text,
            "\n9.9.9  Something changed\n  [do] Do this.\n  [new] A new thing."
        );
    }
}
