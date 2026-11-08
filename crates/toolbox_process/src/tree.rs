//! A started process and its tree (the crate docs' "The process tree").
//!
//! [`Process`] keeps the `Child`, its tree and whether it has exited under one lock, so a
//! signal never reaches a group whose leader was reaped: on Unix the waiter sees the exit
//! without reaping (`waitid` with `WNOWAIT`), kills what is left of the group, and only then
//! reaps, all while holding the lock; a kill takes the lock and does nothing once the exit was
//! seen. A wait that fails for another reason kills nothing, since the group's id may then
//! belong to someone else.

use std::io;
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::Stop;
use crate::command::Command;
use crate::pipes::{self, Mode, Msg, QUEUE, Stream};

/// A running (or exited, not yet dropped) process and its tree.
pub(crate) struct Process {
    state: Mutex<State>,
    /// Notified once `State::exited` is set.
    exit_seen: Condvar,
    pid: u32,
    /// The `Child`'s own handle, which the waiter waits on without the lock: open while
    /// `state` holds the `Child`, which is while this `Process` lives.
    #[cfg(windows)]
    handle: usize,
}

struct State {
    child: Child,
    exited: bool,
    /// The process's job, which holds it and everything it starts.
    #[cfg(windows)]
    job: crate::windows::Job,
}

/**
 * What [`start`] hands back: the process, its stdin when piped, its output as [`Msg`]s, and a
 * sender into that channel (the streaming handle's wake-up).
 */
pub(crate) struct Started {
    pub process: Arc<Process>,
    pub stdin: Option<ChildStdin>,
    pub rx: Receiver<Msg>,
    pub tx: SyncSender<Msg>,
}

/**
 * Starts `command` in a tree of its own, with a reader thread per output pipe (`modes` for
 * stdout and stderr) and a waiter thread, all sending to the one bounded channel in
 * [`Started::rx`]: a consumer that falls behind makes the process wait on its pipes. `stdin`
 * piped or null. On an error nothing is left running.
 */
pub(crate) fn start(
    command: &Command,
    stdin_piped: bool,
    modes: (Mode, Mode),
) -> io::Result<Started> {
    let mut std = command.to_std();
    std.stdin(if stdin_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut process = Process::spawn(std)?;
    let child = &mut process
        .state
        .get_mut()
        .unwrap_or_else(PoisonError::into_inner)
        .child;
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let process = Arc::new(process);
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let started = (|| {
        let (stdin, Some(stdout), Some(stderr)) = pipes else {
            return Err(io::Error::other("its pipes weren't created"));
        };
        pipes::start_reader(Stream::Stdout, modes.0, stdout, tx.clone())?;
        pipes::start_reader(Stream::Stderr, modes.1, stderr, tx.clone())?;
        pipes::start_waiter(Arc::clone(&process), tx.clone())?;
        Ok(stdin)
    })();
    match started {
        Ok(stdin) => Ok(Started {
            process,
            stdin,
            rx,
            tx,
        }),
        Err(error) => {
            /* The waiter is the last to start, so no thread reaps it: this one does. */
            process.kill();
            let _ = process.lock().child.wait();
            Err(error)
        }
    }
}

impl Process {
    #[cfg(unix)]
    fn spawn(mut command: std::process::Command) -> io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        let child = command.process_group(0).spawn()?;
        Ok(Self {
            pid: child.id(),
            state: Mutex::new(State {
                child,
                exited: false,
            }),
            exit_seen: Condvar::new(),
        })
    }

    /**
     * Created suspended, put in a job of its own, and only then resumed: a process that
     * starts a child at once (PyInstaller's yt-dlp does within ~100 ms) can't have it escape
     * a job joined after a plain spawn.
     */
    #[cfg(windows)]
    fn spawn(mut command: std::process::Command) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use std::os::windows::process::CommandExt as _;

        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let joined = crate::windows::Job::new().and_then(|job| {
            job.assign(&child)?;
            crate::windows::resume(&child)?;
            Ok(job)
        });
        match joined {
            Ok(job) => Ok(Self {
                pid: child.id(),
                handle: child.as_raw_handle() as usize,
                state: Mutex::new(State {
                    child,
                    exited: false,
                    job,
                }),
                exit_seen: Condvar::new(),
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the waiter has seen the process exit.
    pub(crate) fn has_exited(&self) -> bool {
        self.lock().exited
    }

    /// Kills the process and its tree, unless it has exited (then the waiter ended its tree).
    pub(crate) fn kill(&self) {
        let state = self.lock();
        if !state.exited {
            state.end_tree();
        }
    }

    /**
     * Stops the process as `stop` says, without blocking: a graceful stop asks now and leaves
     * the kill to a thread of its own, which gives up once the process has exited.
     */
    pub(crate) fn stop(self: &Arc<Self>, stop: Stop) {
        let grace = match stop {
            Stop::Kill => return self.kill(),
            Stop::Graceful(grace) => grace,
        };
        {
            let state = self.lock();
            if state.exited {
                return;
            }
            state.interrupt();
        }
        let process = Arc::clone(self);
        let escalate = std::thread::Builder::new()
            .name("tbx-proc-stop".into())
            .spawn(move || {
                let state = process.lock();
                let (state, _) = process
                    .exit_seen
                    .wait_timeout_while(state, grace, |state| !state.exited)
                    .unwrap_or_else(PoisonError::into_inner);
                if !state.exited {
                    state.end_tree();
                }
            });
        if escalate.is_err() {
            self.kill();
        }
    }

    /**
     * Blocks until the process exits, kills what is left of its tree, reaps it and returns
     * its status (the waiter thread's work, module docs).
     */
    pub(crate) fn wait(&self) -> io::Result<ExitStatus> {
        let exited = self.block_until_exited();
        let mut state = self.lock();
        state.exited = true;
        self.exit_seen.notify_all();
        exited?;
        /* Whatever it left behind in its group or job goes with it. */
        state.end_tree();
        state.child.wait()
    }

    /// Blocks until the process has exited, leaving it unreaped.
    #[cfg(unix)]
    fn block_until_exited(&self) -> io::Result<()> {
        loop {
            // SAFETY: an all-zero `siginfo_t` is a valid value of this plain C struct, which
            // `waitid` overwrites.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            /*
             * SAFETY: waits for this process's own child to exit; `WNOWAIT` leaves it
             * unreaped, so its pid stays taken until `Child::wait` reaps it. A pid is positive
             * and fits every platform's `id_t`.
             */
            let waited = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if waited == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    #[cfg(windows)]
    fn block_until_exited(&self) -> io::Result<()> {
        crate::windows::wait(self.handle as windows_sys::Win32::Foundation::HANDLE)
    }

    /// The process's handle and its job's (tests).
    #[cfg(all(test, windows))]
    pub(crate) fn handles(
        &self,
    ) -> (
        windows_sys::Win32::Foundation::HANDLE,
        windows_sys::Win32::Foundation::HANDLE,
    ) {
        (
            self.handle as windows_sys::Win32::Foundation::HANDLE,
            self.lock().job.handle(),
        )
    }
}

impl State {
    /// The first request to stop: SIGINT to the group, or the job's end.
    fn interrupt(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGINT);
        #[cfg(windows)]
        self.job.terminate();
    }

    /// Kills everything in the tree.
    fn end_tree(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        #[cfg(windows)]
        self.job.terminate();
    }

    #[cfg(unix)]
    fn signal(&self, signal: libc::c_int) {
        if let Ok(group) = libc::pid_t::try_from(self.child.id()) {
            /*
             * SAFETY: a plain syscall. The group's leader is unreaped while the lock is held
             * and `exited` is unset, or (in `Process::wait`) until after this call, so the id
             * is still this group's.
             */
            unsafe { libc::killpg(group, signal) };
        }
    }
}
