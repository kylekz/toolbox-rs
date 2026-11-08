//! A long-running process the caller talks to: [`spawn_streaming`] and its [`Handle`] (a
//! language server, which reads requests on stdin and answers on stdout).
//!
//! Four threads serve it: the two readers and the waiter fill the channel (`crate::tree`),
//! and a delivery thread hands it to the sink in order, the exit last: once the process has
//! exited and both pipes have ended, or [`DRAIN`] after the exit when a process that escaped
//! the tree still holds a pipe.

use std::fmt;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::{ChildStdin, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::command::{Command, Stop};
use crate::error::ProcessError;
use crate::pipes::{Mode, Msg, Stream};
use crate::run::DRAIN;
use crate::tree::{self, Process};

/// What a streaming process did, as its sink hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The next bytes of its stdout as the pipe gave them: a message can span chunks.
    Stdout(Vec<u8>),
    /**
     * One line of its stderr without its `\n` or `\r\n` (a line longer than
     * [`crate::LINE_MAX`] arrives in pieces).
     */
    Stderr(Vec<u8>),
    /**
     * It ended: always the last event, after everything it printed (unless something it
     * started escaped its tree and holds its output open past [`DRAIN`]). `None` when its
     * exit couldn't be waited for. A process killed through its tree reads a signal on Unix
     * (no code) and exit code 1 on Windows.
     */
    Exit(Option<ExitStatus>),
}

/**
 * Starts `command` with stdin piped, its stdout delivered in chunks and its stderr in lines
 * to `sink` on a thread of the process's own ([`Event`]). `sink` should return quickly (hand
 * the event to a channel): the next event waits for it, and the process waits once the
 * queue is full. [`Command::input`], if any, is written before this returns. Dropping the
 * [`Handle`] kills the process and its tree.
 */
pub fn spawn_streaming(
    command: &Command,
    sink: impl FnMut(Event) + Send + 'static,
) -> Result<Handle, ProcessError> {
    let program = command.get_program().to_path_buf();
    let started = tree::start(command, true, (Mode::Chunks, Mode::Lines)).map_err(|source| {
        ProcessError::Start {
            program: program.clone(),
            source,
        }
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let delivery = std::thread::Builder::new()
        .name("tbx-proc-sink".into())
        .spawn({
            let stop = Arc::clone(&stop);
            move || deliver(&started.rx, &stop, sink)
        });
    let delivery = match delivery {
        Ok(delivery) => delivery,
        Err(source) => {
            started.process.kill();
            return Err(ProcessError::Start { program, source });
        }
    };
    let handle = Handle {
        program,
        process: started.process,
        stdin: Mutex::new(started.stdin),
        stop,
        wake: started.tx,
        delivery: Some(delivery),
    };
    if let Some(input) = command.get_input() {
        /* A process that ended before reading it says so through its exit. */
        let _ = handle.write(input);
    }
    Ok(handle)
}

/**
 * A running process from [`spawn_streaming`]. Dropping it kills the process and everything
 * it started, and stops the delivery: once `drop` returns, the sink is never called again
 * and has itself been dropped (except when the handle is dropped from inside the sink, where
 * delivery stops after the sink returns).
 */
pub struct Handle {
    program: PathBuf,
    process: Arc<Process>,
    /// `None` once closed. Its own lock, so a write blocked on a full pipe never holds up a
    /// stop.
    stdin: Mutex<Option<ChildStdin>>,
    stop: Arc<AtomicBool>,
    /// Wakes the delivery thread when the handle is dropped.
    wake: SyncSender<Msg>,
    delivery: Option<JoinHandle<()>>,
}

impl Handle {
    /// The process's id.
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /**
     * Writes `bytes` to the process's stdin and flushes them, returning once they are in the
     * pipe. Writes from one thread arrive in order. [`ProcessError::Ended`] once the process
     * has ended or its stdin is closed.
     */
    pub fn write(&self, bytes: &[u8]) -> Result<(), ProcessError> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(self.ended());
        };
        pipe.write_all(bytes)
            .and_then(|()| pipe.flush())
            .map_err(|source| {
                /* Windows reports a pipe whose reader is gone with its own error codes. */
                if source.kind() == io::ErrorKind::BrokenPipe || self.process.has_exited() {
                    self.ended()
                } else {
                    ProcessError::Write {
                        program: self.program.clone(),
                        source,
                    }
                }
            })
    }

    /// Closes the process's stdin: it reads the end of its input.
    pub fn close_stdin(&self) {
        drop(lock(&self.stdin).take());
    }

    /**
     * Stops the process and everything it started, as `stop` says, without blocking; the sink
     * hears its exit. Does nothing once it has exited.
     */
    pub fn stop(&self, stop: Stop) {
        self.process.stop(stop);
    }

    fn ended(&self) -> ProcessError {
        ProcessError::Ended {
            program: self.program.clone(),
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.process.kill();
        self.close_stdin();
        /* A full queue needs no wake-up: the delivery thread sees `stop` at its next message. */
        let _ = self.wake.try_send(Msg::Stop);
        if let Some(thread) = self.delivery.take()
            /*
             * Dropped from inside the sink, the thread is this one: it stops once the sink
             * returns and sees `stop`, and joining it here would deadlock.
             */
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("program", &self.program)
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The delivery thread (module docs), until the handle is dropped.
fn deliver(rx: &Receiver<Msg>, stop: &AtomicBool, mut sink: impl FnMut(Event)) {
    let mut open_pipes = 2;
    let mut exit: Option<Option<ExitStatus>> = None;
    let mut drain_until: Option<Instant> = None;
    while exit.is_none() || open_pipes > 0 {
        let msg = match drain_until {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        let msg = match msg {
            Ok(msg) => msg,
            /* A pipe still open after the drain: the exit goes out anyway. */
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stop.load(Ordering::Acquire) {
            return;
        }
        match msg {
            Msg::Data(Stream::Stdout, bytes) => sink(Event::Stdout(bytes)),
            Msg::Data(Stream::Stderr, line) => sink(Event::Stderr(line)),
            Msg::End => open_pipes -= 1,
            Msg::Exited(status) => {
                exit = Some(status.ok());
                drain_until = Some(Instant::now() + DRAIN);
            }
            Msg::Stop => return,
        }
    }
    if let Some(status) = exit
        && !stop.load(Ordering::Acquire)
    {
        sink(Event::Exit(status));
    }
}
