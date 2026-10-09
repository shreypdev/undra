//! The commands: one module per `undra <command>`.

pub(crate) mod adopt;
pub(crate) mod bindgen;
pub(crate) mod build;
pub(crate) mod dev;
pub(crate) mod doctor;
pub(crate) mod drift;
pub(crate) mod init;
pub(crate) mod schema;
pub(crate) mod symbolicate;
pub(crate) mod upgrade;

use std::path::PathBuf;

use crate::error::{CliError, Result};
use crate::project::Project;
use crate::session::Session;
use crate::sys::Sys;
use crate::ui::Ui;

/// What every command runs against.
pub struct Env<'a> {
    /// The machine.
    pub sys: &'a dyn Sys,
    /// Output.
    pub ui: Ui,
    /// `-C <dir>`: where to start looking for the project (default: the current directory).
    pub project_dir: Option<PathBuf>,
}

impl<'a> Env<'a> {
    /// The directory the command starts from.
    ///
    /// # Errors
    ///
    /// `C0010` when the current directory cannot be determined.
    pub fn start_dir(&self) -> Result<PathBuf> {
        match &self.project_dir {
            Some(dir) => Ok(dir.clone()),
            None => std::env::current_dir().map_err(|e| {
                CliError::io("find the current directory", std::path::Path::new("."), &e)
            }),
        }
    }

    /// Finds the project and starts a session on it.
    ///
    /// # Errors
    ///
    /// `C0001` outside a project; see [`Project::discover`].
    pub fn session(&self) -> Result<Session<'a>> {
        let project = Project::discover(&self.start_dir()?)?;
        Ok(Session::new(project, self.sys, self.ui))
    }
}
