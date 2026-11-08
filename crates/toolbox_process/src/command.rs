//! What to run: [`Command`], and how to stop it ([`Stop`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long [`Stop::Graceful`] waits by default between asking and killing: the grace fc gives
/// yt-dlp, which exits in about 0.05 s on SIGINT, leaving its `.part` file.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(3);

/// How a process is stopped: by a timeout or a cancel in [`crate::run`], or by
/// [`crate::Handle::stop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /**
     * Ask first, then kill what is left after the grace period. On Unix the request is SIGINT
     * to the process's group (Ctrl-C, which CLI tools clean up after); on Windows a windowless
     * process has no console to send Ctrl-C to, so it is killed at once.
     */
    Graceful(Duration),
    /// Kill the whole tree at once: SIGKILL to the group, or the job's end.
    Kill,
}

impl Default for Stop {
    fn default() -> Self {
        Self::Graceful(DEFAULT_GRACE)
    }
}

/**
 * A program to run, its arguments, environment and working directory. It never runs through a
 * shell, and on Windows never opens a console window (`CREATE_NO_WINDOW`), whichever way it
 * is run. A child inherits this process's environment with [`Command::env`],
 * [`Command::env_remove`] and [`Command::env_clear`] applied in order.
 */
#[derive(Clone, Debug)]
pub struct Command {
    program: PathBuf,
    args: Vec<OsString>,
    clear_env: bool,
    env: Vec<(OsString, Option<OsString>)>,
    cwd: Option<PathBuf>,
    input: Option<Vec<u8>>,
    stop: Stop,
}

impl Command {
    /**
     * `program`: a path, or a bare name the OS looks up on `PATH` when it starts
     * ([`crate::Override`] finds it beforehand, to say where it is or that it is missing).
     */
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            clear_env: false,
            env: Vec::new(),
            cwd: None,
            input: None,
            stop: Stop::default(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), Some(value.into())));
        self
    }

    pub fn env_remove(mut self, name: impl Into<OsString>) -> Self {
        self.env.push((name.into(), None));
        self
    }

    /// Starts from an empty environment: only what is set after this reaches the child.
    pub fn env_clear(mut self) -> Self {
        self.clear_env = true;
        self.env.clear();
        self
    }

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /**
     * Bytes the process reads on its stdin. [`crate::run`] and [`crate::run_lines`] write them
     * and then close stdin (without input, stdin is empty); [`crate::spawn_streaming`] writes
     * them before it returns and keeps stdin open for [`crate::Handle::write`].
     */
    pub fn input(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.input = Some(bytes.into());
        self
    }

    /// How a timeout or a cancel stops it ([`Stop::default`]: graceful, [`DEFAULT_GRACE`]).
    pub fn stop(mut self, stop: Stop) -> Self {
        self.stop = stop;
        self
    }

    pub fn get_program(&self) -> &Path {
        &self.program
    }

    pub fn get_args(&self) -> &[OsString] {
        &self.args
    }

    /// Each variable the command sets (`Some`) or removes (`None`), in the order given.
    pub fn get_envs(&self) -> impl Iterator<Item = (&OsString, Option<&OsString>)> {
        self.env.iter().map(|(name, value)| (name, value.as_ref()))
    }

    /// Whether the child starts from an empty environment ([`Command::env_clear`]).
    pub fn get_env_clear(&self) -> bool {
        self.clear_env
    }

    pub fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn get_input(&self) -> Option<&[u8]> {
        self.input.as_deref()
    }

    pub fn get_stop(&self) -> Stop {
        self.stop
    }

    /**
     * The same program, arguments, environment and working directory as a
     * `std::process::Command`, with no console window on Windows, but no tree of its own:
     * for a child whose pipes go to code that wants them raw (a transport's reader and
     * writer). Pair it with [`crate::tie_to_this_process`] so it still dies with this app on
     * Windows. Its stdio and [`Command::input`] are the caller's to set up.
     */
    pub fn to_std(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args);
        if self.clear_env {
            command.env_clear();
        }
        for (name, value) in &self.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn env_changes_keep_their_order_and_a_clear_drops_earlier_ones() {
        let command = Command::new("tool")
            .env("A", "1")
            .env_clear()
            .env("B", "2")
            .env_remove("C");
        let envs: Vec<_> = command.get_envs().collect();
        assert_eq!(
            envs,
            [
                (&OsString::from("B"), Some(&OsString::from("2"))),
                (&OsString::from("C"), None)
            ]
        );
        assert!(command.get_env_clear());

        let std = command.to_std();
        let set: Vec<_> = std
            .get_envs()
            .filter_map(|(name, value)| Some((name.to_str()?, value?.to_str()?)))
            .collect();
        assert_eq!(set, [("B", "2")]);
    }

    #[test]
    fn the_std_command_carries_everything_but_stdio() {
        let command = Command::new("tool")
            .arg("--a")
            .args(["b", "c"])
            .current_dir("/somewhere");
        let std = command.to_std();
        assert_eq!(std.get_program(), "tool");
        assert_eq!(std.get_args().collect::<Vec<_>>(), ["--a", "b", "c"]);
        assert_eq!(std.get_current_dir(), Some(Path::new("/somewhere")));
        assert_eq!(command.get_stop(), Stop::Graceful(DEFAULT_GRACE));
        assert_eq!(command.stop(Stop::Kill).get_stop(), Stop::Kill);
    }
}
