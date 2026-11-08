//! [`Override`]: an environment variable names the executable, otherwise it is found on
//! `PATH` (dv's `DV_GH`; tests and scenarios point such a variable at a mock).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// A program found by name on `PATH` unless the variable [`Override::var`] names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    var: String,
    name: OsString,
}

/// Where [`Override::locate`] found the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The variable named it.
    Var,
    /// It was found on `PATH`.
    Path,
}

/// The program [`Override::locate`] found: an absolute path to an executable file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub program: PathBuf,
    pub origin: Origin,
}

/// Why [`Override::locate`] found nothing.
#[derive(Debug, thiserror::Error)]
pub enum LocateError {
    /// The variable is set, but what it names isn't an executable.
    #[error("{var} names {}, which isn't a program that can run", value.display())]
    BadVar {
        var: String,
        value: OsString,
        #[source]
        source: which::Error,
    },
    /// The variable isn't set and no executable of that name is on `PATH`.
    #[error("{} isn't on PATH (or set {var} to its path)", name.display())]
    NotFound {
        var: String,
        name: OsString,
        #[source]
        source: which::Error,
    },
}

impl Override {
    /// `name` on `PATH`, unless `var` is set and not empty.
    pub fn new(var: impl Into<String>, name: impl Into<OsString>) -> Self {
        Self {
            var: var.into(),
            name: name.into(),
        }
    }

    pub fn var(&self) -> &str {
        &self.var
    }

    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// The program in this process's environment and working directory ([`Override::locate_with`]).
    pub fn locate(&self) -> Result<Located, LocateError> {
        let cwd = std::env::current_dir().unwrap_or_default();
        self.locate_with(|name| std::env::var_os(name), &cwd)
    }

    /**
     * The program, with variables read through `env` (`PATH` included) and relative paths
     * resolved against `cwd`. The variable's value may be a path or a name (found on `PATH`
     * in turn); either way it must be an executable file. On Windows a name is tried with
     * each of `PATHEXT`'s extensions.
     */
    pub fn locate_with(
        &self,
        env: impl Fn(&str) -> Option<OsString>,
        cwd: &Path,
    ) -> Result<Located, LocateError> {
        let path = env("PATH");
        match env(&self.var).filter(|value| !value.is_empty()) {
            Some(value) => match which::which_in(&value, path, cwd) {
                Ok(program) => Ok(Located {
                    program,
                    origin: Origin::Var,
                }),
                Err(source) => Err(LocateError::BadVar {
                    var: self.var.clone(),
                    value,
                    source,
                }),
            },
            None => match which::which_in(&self.name, path, cwd) {
                Ok(program) => Ok(Located {
                    program,
                    origin: Origin::Path,
                }),
                Err(source) => Err(LocateError::NotFound {
                    var: self.var.clone(),
                    name: self.name.clone(),
                    source,
                }),
            },
        }
    }
}
