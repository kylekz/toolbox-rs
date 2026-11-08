//! Running a program to its end: [`run`] and [`run_lines`], under a timeout and a
//! [`CancelToken`].
//!
//! The calling thread consumes the channel the readers and the waiter fill (`crate::tree`),
//! checks the deadline and the token, and returns once the process has exited and both pipes
//! have ended, or [`DRAIN`] after the exit when a process that escaped the tree still holds a
//! pipe (its reader thread then ends when it closes the pipe or exits).

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::error::ProcessError;
use crate::pipes::{self, Mode, Msg, Stream};
use crate::tree::{self, Process};

/// A timeout that never passes: the run waits for the process however long it takes (the
/// token can still cancel it).
pub const NO_TIMEOUT: Duration = Duration::MAX;

/// How long after the exit its pipes may stay open before the run returns anyway.
pub(crate) const DRAIN: Duration = Duration::from_secs(1);

/// How often the calling thread looks at the token and the clock while nothing arrives.
const POLL: Duration = Duration::from_millis(50);

/// Asks a running program to stop; clones share the request.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A finished run's exit status and everything it wrote.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// stderr as text, trimmed at the end, for an error message.
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim_end().to_owned()
    }
}

/**
 * Runs `command` to its end and returns its exit status and everything it wrote, byte for
 * byte. Once `timeout` passes ([`NO_TIMEOUT`]: never) or `cancel` is cancelled, the process
 * and everything it started is stopped as [`Command::stop`] says, and the run fails with
 * [`ProcessError::TimedOut`] or [`ProcessError::Cancelled`]. A token cancelled beforehand
 * starts nothing. A non-zero exit is not an error: the caller reads [`Output::status`].
 */
pub fn run(
    command: &Command,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Output, ProcessError> {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = drive(
        command,
        timeout,
        cancel,
        Mode::Chunks,
        |stream, data| match stream {
            Stream::Stdout => stdout.extend_from_slice(&data),
            Stream::Stderr => stderr.extend_from_slice(&data),
        },
    )?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/**
 * [`run`], handing each line to `on_line` as it arrives instead of collecting the output: a
 * line without its `\n` or `\r\n`, in the order each pipe gave them (the two pipes interleave
 * as they arrive). A line longer than [`crate::LINE_MAX`] arrives in pieces. `on_line` runs on
 * the calling thread; while it works, the process waits once the queue is full.
 */
pub fn run_lines(
    command: &Command,
    timeout: Duration,
    cancel: &CancelToken,
    mut on_line: impl FnMut(Stream, &[u8]),
) -> Result<ExitStatus, ProcessError> {
    drive(command, timeout, cancel, Mode::Lines, |stream, line| {
        on_line(stream, &line);
    })
}

fn drive(
    command: &Command,
    timeout: Duration,
    cancel: &CancelToken,
    mode: Mode,
    mut on_data: impl FnMut(Stream, Vec<u8>),
) -> Result<ExitStatus, ProcessError> {
    let program = || command.get_program().to_path_buf();
    if cancel.is_cancelled() {
        return Err(ProcessError::Cancelled { program: program() });
    }
    let start_error = |source| ProcessError::Start {
        program: program(),
        source,
    };
    let input = command.get_input();
    let started = tree::start(command, input.is_some(), (mode, mode)).map_err(start_error)?;
    let process = KillOnReturn(started.process);
    /* Only the threads send from here, so the channel disconnects once they are all done. */
    drop(started.tx);
    if let (Some(stdin), Some(input)) = (started.stdin, input) {
        pipes::start_writer(stdin, input.to_vec()).map_err(start_error)?;
    }

    let deadline = Instant::now().checked_add(timeout);
    let mut stopped: Option<ProcessError> = None;
    let mut open_pipes = 2;
    let mut exit: Option<(Instant, std::io::Result<ExitStatus>)> = None;
    loop {
        if let Some((at, _)) = &exit
            && (open_pipes == 0 || at.elapsed() >= DRAIN)
        {
            break;
        }
        match started.rx.recv_timeout(POLL) {
            Ok(Msg::Data(stream, data)) => on_data(stream, data),
            Ok(Msg::End) => open_pipes -= 1,
            Ok(Msg::Exited(status)) => exit = Some((Instant::now(), status)),
            Ok(Msg::Stop) | Err(RecvTimeoutError::Timeout) => {}
            /* Every sender is gone: both pipes ended and the waiter reported the exit. */
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if exit.is_some() || stopped.is_some() {
            continue;
        }
        stopped = if cancel.is_cancelled() {
            Some(ProcessError::Cancelled { program: program() })
        } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Some(ProcessError::TimedOut {
                program: program(),
                timeout,
            })
        } else {
            continue;
        };
        process.0.stop(command.get_stop());
    }
    if let Some(error) = stopped {
        return Err(error);
    }
    match exit {
        Some((_, Ok(status))) => Ok(status),
        Some((_, Err(source))) => Err(ProcessError::Wait {
            program: program(),
            source,
        }),
        None => Err(ProcessError::Wait {
            program: program(),
            source: std::io::Error::other("its exit was never reported"),
        }),
    }
}

/**
 * Kills the process's tree when the run returns, however it returns (an early error, a panic
 * in `on_line`); a no-op once the process has exited. The waiter then reaps it.
 */
struct KillOnReturn(Arc<Process>);

impl Drop for KillOnReturn {
    fn drop(&mut self) {
        self.0.kill();
    }
}
