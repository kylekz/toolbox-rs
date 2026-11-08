//! Running other programs (docs/PLAN.md §5a, docs/process.md): the one way the apps start a
//! process, so every one gets the same guarantees.
//!
//! - **What runs:** a [`Command`]: a program, never through a shell, its arguments,
//!   environment changes and working directory. On Windows it never opens a console window.
//! - **To its end:** [`run`] collects the output, [`run_lines`] hands it over line by line;
//!   both under a timeout and a [`CancelToken`]. stdin is empty, or [`Command::input`].
//! - **Talking to it:** [`spawn_streaming`] delivers its output to a sink as [`Event`]s and
//!   returns a [`Handle`] that writes its stdin, stops it, and kills it when dropped.
//! - **Finding it:** [`Override`]: an environment variable names the executable, otherwise
//!   `PATH`.
//!
//! **The process tree.** Each process runs in a tree of its own, which takes everything it
//! starts with it:
//! - On Unix it leads a process group of its own. A graceful [`Stop`] sends the group SIGINT,
//!   then SIGKILL after the grace period; a kill sends SIGKILL at once.
//! - On Windows it starts suspended (`CREATE_SUSPENDED | CREATE_NO_WINDOW`), joins a Job
//!   Object of its own that kills what it holds when it closes, and only then runs, so nothing
//!   it starts can escape the job. Stopping it ends the job. The job's handle is this
//!   process's, so the tree also dies with the app, however the app ends.
//!
//! When the process itself exits, whatever it left running in its tree is killed too. Its
//! exit is seen before it is reaped, so on Unix the group's id can't have been reused by then.
//! A process that leaves its tree on purpose (`setsid`, a job breakaway) is beyond reach; a
//! pipe it still holds delays a run's return by at most a second.
//!
//! **Dying with the app.** On Windows a tree's job dies with the app. A child started some
//! other way (a raw `std::process::Command`, from [`Command::to_std`]) can join the app's own
//! job instead through [`tie_to_this_process`]. On Unix nothing kills a child when the app
//! dies.

mod command;
mod error;
mod locate;
mod pipes;
mod run;
mod streaming;
mod tree;
#[cfg(windows)]
mod windows;

#[cfg(all(test, windows))]
mod windows_tests;

pub use command::{Command, DEFAULT_GRACE, Stop};
pub use error::ProcessError;
pub use locate::{LocateError, Located, Origin, Override};
pub use pipes::{LINE_MAX, Stream};
pub use run::{CancelToken, NO_TIMEOUT, Output, run, run_lines};
pub use streaming::{Event, Handle, spawn_streaming};

/**
 * Puts `child`, started outside this crate's trees, in this process's own job on Windows: a
 * Job Object created on first use and never closed, which the OS closes when this process
 * ends, however it ends, killing what it holds. An error when the job couldn't be made or
 * `child` couldn't join it; the child runs on regardless. Elsewhere it does nothing.
 *
 * Processes run through [`run`] or [`spawn_streaming`] don't need it: their own job already
 * dies with this process.
 */
pub fn tie_to_this_process(child: &std::process::Child) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows::app_job()
            .map_err(std::io::Error::other)?
            .assign(child)
    }
    #[cfg(not(windows))]
    {
        let _ = child;
        Ok(())
    }
}
