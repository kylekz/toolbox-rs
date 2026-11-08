//! [`ProcessError`]: why running a program failed.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Why a run, a spawn or a write to a running process failed. Each names the program as the
/// [`crate::Command`] did.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// It never ran: the program isn't there or can't be executed, or its tree couldn't be set
    /// up (nothing of it is left running).
    #[error("couldn't start {}: {source}", program.display())]
    Start {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// It ran past the timeout and was stopped, with everything it started.
    #[error("{} didn't finish within {} s and was stopped", program.display(), timeout.as_secs_f32())]
    TimedOut { program: PathBuf, timeout: Duration },
    /// The [`crate::CancelToken`] was cancelled; it was stopped, or never started.
    #[error("{} was cancelled", program.display())]
    Cancelled { program: PathBuf },
    /// Its exit couldn't be waited for, so its exit status is unknown.
    #[error("couldn't wait for {}: {source}", program.display())]
    Wait {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A write to its stdin found it closed: the process has ended, is ending, or closed its
    /// stdin, or [`crate::Handle::close_stdin`] closed it.
    #[error("{} has ended or closed its stdin", program.display())]
    Ended { program: PathBuf },
    /// A write to its stdin failed for another reason.
    #[error("couldn't write to {}: {source}", program.display())]
    Write {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
}
