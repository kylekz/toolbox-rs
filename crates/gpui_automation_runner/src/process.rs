//! Child processes (the app under test): line-oriented stdout with deadlines,
//! stderr captured to a log file, and guaranteed cleanup. On Windows every
//! child is also placed in a kill-on-close job object, so even a runner
//! killed outright takes its children with it.

use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};

/// Set by the ctrl-c handler; every wait polls it.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

const POLL: Duration = Duration::from_millis(100);

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Why a line could not be read.
#[derive(Debug)]
pub enum ReadError {
    Timeout,
    Interrupted,
    /// The child closed stdout (exited or crashed).
    Closed,
}

pub struct ChildProcess {
    pub label: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    pub stderr_log: PathBuf,
}

impl ChildProcess {
    /// Spawn `command` with piped stdio. Stderr is appended to `stderr_log`
    /// by a background thread so the child can never block on a full pipe.
    pub fn spawn(label: &'static str, mut command: Command, stderr_log: &Path) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let program = command.get_program().to_string_lossy().into_owned();
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {label} ({program})"))?;
        job::assign(&child);

        let stdout = child.stdout.take().context("child stdout was not piped")?;
        let stderr = child.stderr.take().context("child stderr was not piped")?;
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("e2e-{label}-stdout"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .context("spawning a stdout reader")?;
        let log_path = stderr_log.to_path_buf();
        std::thread::Builder::new()
            .name(format!("e2e-{label}-stderr"))
            .spawn(move || copy_to_file(stderr, &log_path))
            .context("spawning a stderr reader")?;

        Ok(Self {
            label,
            stdin: child.stdin.take(),
            child,
            lines,
            stderr_log: stderr_log.to_path_buf(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send_line(&mut self, line: &str) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("{} stdin is closed", self.label))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .with_context(|| format!("writing to {} (did it exit?)", self.label))
    }

    /// Next stdout line before `deadline`.
    pub fn read_line(&mut self, deadline: Instant) -> Result<String, ReadError> {
        loop {
            if interrupted() {
                return Err(ReadError::Interrupted);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ReadError::Timeout);
            }
            match self.lines.recv_timeout((deadline - now).min(POLL)) {
                Ok(line) => return Ok(line),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(ReadError::Closed),
            }
        }
    }

    pub fn try_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait up to `timeout` for the child to exit on its own.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_status() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Close stdin (the automation driver's "hang up"), give the child a
    /// moment to exit, then kill it.
    pub fn shutdown(&mut self, grace: Duration) -> Option<ExitStatus> {
        self.stdin = None;
        if let Some(status) = self.wait_exit(grace) {
            return Some(status);
        }
        self.kill();
        None
    }

    pub fn kill(&mut self) {
        if self.try_status().is_none() {
            if let Err(err) = self.child.kill() {
                eprintln!("e2e: killing {} (pid {}): {err}", self.label, self.pid());
            }
            let _ = self.child.wait();
        }
    }

    /// The last `lines` lines of the captured stderr.
    pub fn stderr_tail(&self, lines: usize) -> String {
        let text = std::fs::read_to_string(&self.stderr_log).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn copy_to_file(mut from: impl Read, path: &Path) {
    let Ok(mut file) = std::fs::File::create(path) else {
        // Still drain the pipe so the child never blocks on it.
        let _ = std::io::copy(&mut from, &mut std::io::sink());
        return;
    };
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if file.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = file.flush();
            }
        }
    }
}

/// Install the ctrl-c handler: flag the interruption so the current
/// scenario unwinds, kills its children and prints a summary.
pub fn install_interrupt_handler() -> Result<()> {
    job::install_ctrl_handler()
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering;

    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::core::BOOL;

    /// The job handle, stored as an integer so it can live in a static. It
    /// is deliberately never closed: the OS closes it when the runner exits,
    /// which is exactly when the children must die.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        let handle = JOB.get_or_init(|| {
            // SAFETY: null attributes and name create an anonymous job.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                eprintln!("e2e: CreateJobObject failed; children rely on explicit kills");
                return None;
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `job` is a fresh job handle and `info` is the struct
            // the information class expects, with its exact size.
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                eprintln!(
                    "e2e: configuring the job object failed; children rely on explicit kills"
                );
                return None;
            }
            Some(job as isize)
        });
        handle.map(|job| job as HANDLE)
    }

    pub fn assign(child: &Child) {
        if let Some(job) = job() {
            // SAFETY: both handles are live; the child handle is owned by
            // `child` for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
            if ok == 0 {
                eprintln!(
                    "e2e: could not put pid {} in the job object; it relies on explicit kills",
                    child.id()
                );
            }
        }
    }

    unsafe extern "system" fn on_ctrl(ctrl_type: u32) -> BOOL {
        if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
            super::INTERRUPTED.store(true, Ordering::Relaxed);
            1
        } else {
            // Console close, logoff, shutdown: take the default exit; the job
            // object then kills the children.
            0
        }
    }

    pub fn install_ctrl_handler() -> Result<()> {
        // SAFETY: `on_ctrl` matches PHANDLER_ROUTINE and only touches an
        // atomic, which is safe from the handler thread.
        if unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } == 0 {
            bail!("SetConsoleCtrlHandler failed");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod job {
    use std::process::Child;

    use anyhow::Result;

    /// Children share the runner's process group, so a terminal ctrl-c
    /// reaches them directly; explicit kills cover every other exit path.
    pub fn assign(_child: &Child) {}

    pub fn install_ctrl_handler() -> Result<()> {
        Ok(())
    }
}

/// Keeps the display awake while it is held: the whole run.
///
/// macOS stops drawing a window nobody can see, a sleeping display's included (gpui's display
/// link stops while a window is occluded), so a scenario waiting for a frame would time out
/// once the display idles off. `caffeinate -d -u -w <runner>` wakes the display, keeps it from
/// idling and ends with the runner. It changes no setting; elsewhere this does nothing.
pub struct DisplayAwake(Option<Child>);

impl DisplayAwake {
    pub fn hold() -> Self {
        if !cfg!(target_os = "macos") {
            return Self(None);
        }
        let child = Command::new("/usr/bin/caffeinate")
            .args(["-d", "-u", "-w", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(child) => Self(Some(child)),
            Err(err) => {
                eprintln!(
                    "e2e: couldn't start caffeinate ({err}); a display that sleeps stops the \
                     app drawing"
                );
                Self(None)
            }
        }
    }
}

impl Drop for DisplayAwake {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
