//! Errors that teach: what happened, why it matters, and what to do about it.
//!
//! Every failure the CLI reports has the shape of the macro diagnostics in `docs/SPEC.md`
//! section 12 (constitution R8):
//!
//! ```text
//! error[undra::C0001]: no undra.toml found in /work/app or any parent directory
//!   = note: `undra build` works on an Undra project, and a project is a directory with an undra.toml
//!   = help: run it inside a project, or create one with `undra init <name>`
//!   = docs: https://shreypdev.github.io/undra/docs/errors.html#C0001
//! ```
//!
//! The codes are stable; the catalogue is [`Code`].

use core::fmt;
use std::path::Path;

/// The stable code of a CLI diagnostic (`error[undra::C00NN]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// `C0001`: not inside an Undra project (no `undra.toml`).
    NoProject,
    /// `C0002`: `undra.toml` (or a schema file) cannot be read or is invalid.
    BadConfig,
    /// `C0003`: a tool the command needs is not installed.
    MissingTool,
    /// `C0004`: a build tool ran and failed.
    ToolFailed,
    /// `C0005`: the core crate is missing, or is not an Undra core.
    BadCore,
    /// `C0006`: the schema could not be extracted from the built core.
    Schema,
    /// `C0007`: the schema cannot be turned into bindings (bindgen diagnostics).
    Bindgen,
    /// `C0008`: the command would overwrite something it must not.
    WouldOverwrite,
    /// `C0009`: an argument has a value the command cannot use.
    BadArgument,
    /// `C0010`: a file or directory operation failed.
    Io,
    /// `C0011`: the Rust toolchain lacks a compilation target.
    MissingTarget,
    /// `C0012`: the platform is not available on this machine.
    Unsupported,
    /// `C0013`: the dev server failed to start or crashed.
    Dev,
    /// `C0014`: the project's Undra does not match: the core's comes from elsewhere, or it is newer.
    ///
    /// The core and the project disagree about where Undra comes from, or the project is on a newer
    /// Undra than this `undra` (`undra upgrade` moves a project forward only).
    UndraMismatch,
}

impl Code {
    /// The code as printed, e.g. `"C0001"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NoProject => "C0001",
            Code::BadConfig => "C0002",
            Code::MissingTool => "C0003",
            Code::ToolFailed => "C0004",
            Code::BadCore => "C0005",
            Code::Schema => "C0006",
            Code::Bindgen => "C0007",
            Code::WouldOverwrite => "C0008",
            Code::BadArgument => "C0009",
            Code::Io => "C0010",
            Code::MissingTarget => "C0011",
            Code::Unsupported => "C0012",
            Code::Dev => "C0013",
            Code::UndraMismatch => "C0014",
        }
    }
}

/// A failure of an `undra` command: a stable [`Code`], what went wrong, why it matters, and how to
/// fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    /// The stable diagnostic code.
    pub code: Code,
    /// What went wrong, in one line.
    pub what: String,
    /// Why it matters or why it happened.
    pub why: String,
    /// What to do about it. May span several lines; the first line is the command or step.
    pub fix: String,
    /// Output of the failed tool, printed after the diagnostic when there is any.
    pub detail: Option<String>,
}

/// The result type of every CLI operation.
pub type Result<T> = core::result::Result<T, CliError>;

impl CliError {
    /// A diagnostic with all three parts.
    #[must_use]
    pub fn new(
        code: Code,
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> CliError {
        CliError {
            code,
            what: what.into(),
            why: why.into(),
            fix: fix.into(),
            detail: None,
        }
    }

    /// Attaches the output of a failed tool.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> CliError {
        self.detail = Some(detail.into());
        self
    }

    /// `C0001`: no `undra.toml` from `start` up to the filesystem root.
    #[must_use]
    pub fn no_project(start: &Path) -> CliError {
        CliError::new(
            Code::NoProject,
            format!(
                "no undra.toml found in {} or any parent directory",
                start.display()
            ),
            "this command works on an Undra project, and a project is the directory that holds undra.toml",
            "run it inside a project, point at one with `-C <dir>`, or create one with `undra init <name>`",
        )
    }

    /// `C0002`: a configuration or schema file is unusable.
    #[must_use]
    pub fn bad_config(file: &Path, what: impl Into<String>, fix: impl Into<String>) -> CliError {
        CliError::new(
            Code::BadConfig,
            format!("{}: {}", file.display(), what.into()),
            "the file drives the build, so a value the CLI cannot understand would make it guess",
            fix,
        )
    }

    /// `C0003`: a tool is not installed.
    #[must_use]
    pub fn missing_tool(tool: &str, needed_for: &str, install: &str) -> CliError {
        CliError::new(
            Code::MissingTool,
            format!("`{tool}` was not found"),
            format!("{needed_for} needs `{tool}`, and it is not on your PATH"),
            format!("{install}\nrun `undra doctor` to check the rest of the toolchain"),
        )
    }

    /// `C0004`: a build tool exited with a failure.
    #[must_use]
    pub fn tool_failed(tool: &str, doing: &str, status: &str) -> CliError {
        CliError::new(
            Code::ToolFailed,
            format!("`{tool}` failed while {doing} ({status})"),
            "the tool's own output above says what it objected to; Undra cannot continue without its result",
            "fix the problem the tool reported and run the command again; `undra doctor` checks the toolchain",
        )
    }

    /// `C0010`: an I/O operation failed.
    #[must_use]
    pub fn io(doing: &str, path: &Path, error: &std::io::Error) -> CliError {
        CliError::new(
            Code::Io,
            format!("could not {doing} {}: {error}", path.display()),
            "the command needs to read or write this path and the operating system refused",
            "check that the path exists and that you may access it, then run the command again",
        )
    }

    /// `C0009`: an argument is unusable.
    #[must_use]
    pub fn bad_argument(
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> CliError {
        CliError::new(Code::BadArgument, what, why, fix)
    }

    /// The documentation URL of the code.
    #[must_use]
    pub fn docs_url(&self) -> String {
        format!(
            "https://shreypdev.github.io/undra/docs/errors.html#{}",
            self.code.as_str()
        )
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[undra::{}]: {}", self.code.as_str(), self.what)?;
        write!(f, "\n  = note: {}", self.why)?;
        let mut fix = self.fix.lines();
        if let Some(first) = fix.next() {
            write!(f, "\n  = help: {first}")?;
            for line in fix {
                write!(f, "\n          {line}")?;
            }
        }
        write!(f, "\n  = docs: {}", self.docs_url())?;
        if let Some(detail) = &self.detail {
            write!(f, "\n\n{detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_has_the_four_parts_in_order() {
        let e = CliError::new(Code::MissingTool, "what", "why", "first\nsecond");
        assert_eq!(
            e.to_string(),
            "error[undra::C0003]: what\n  = note: why\n  = help: first\n          second\n  = docs: https://shreypdev.github.io/undra/docs/errors.html#C0003"
        );
    }

    #[test]
    fn codes_are_unique() {
        let all = [
            Code::NoProject,
            Code::BadConfig,
            Code::MissingTool,
            Code::ToolFailed,
            Code::BadCore,
            Code::Schema,
            Code::Bindgen,
            Code::WouldOverwrite,
            Code::BadArgument,
            Code::Io,
            Code::MissingTarget,
            Code::Unsupported,
            Code::Dev,
            Code::UndraMismatch,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in all {
            assert!(seen.insert(code.as_str()), "{code:?} repeats a code");
        }
    }

    #[test]
    fn detail_follows_the_diagnostic() {
        let e = CliError::new(Code::ToolFailed, "w", "y", "f").with_detail("compiler output");
        assert!(e.to_string().ends_with("\n\ncompiler output"));
    }

    #[test]
    fn missing_tool_teaches_the_install() {
        let e = CliError::missing_tool(
            "adb",
            "listing devices",
            "install it with `sdkmanager \"platform-tools\"`",
        );
        let text = e.to_string();
        assert!(text.contains("`adb` was not found"), "{text}");
        assert!(text.contains("sdkmanager"), "{text}");
        assert!(text.contains("undra doctor"), "{text}");
    }
}
