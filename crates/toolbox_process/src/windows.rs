//! The Win32 calls behind a process's tree (`crate::tree`) and the app-wide job
//! (`crate::tie_to_this_process`): Job Objects that kill what they hold when they close, and
//! resuming a process created suspended.

use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::process::Child;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

/*
 * The crate's one ntdll binding (windows-sys has none for it). std keeps the first thread's
 * handle to itself, and finding the thread again takes a Toolhelp snapshot of every thread on
 * the machine, about 42 ms per spawn as fc measured (PLAN §5a). The export is undocumented
 * but stable since Windows XP.
 */
#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtResumeProcess(process: HANDLE) -> NTSTATUS;
}

/// The exit code a process killed through its job reads.
pub(crate) const KILLED_CODE: u32 = 1;

/// A Job Object that kills what it holds when terminated, and when its last handle closes:
/// at the latest when this process ends, however it ends.
#[derive(Debug)]
pub(crate) struct Job(HANDLE);

// SAFETY: a job's handle may be used and closed from any thread.
unsafe impl Send for Job {}
// SAFETY: every call on the handle is thread-safe in Win32.
unsafe impl Sync for Job {}

impl Job {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: null attributes and a null name create an anonymous job, not inheritable.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // From here dropping `job` closes the handle.
        let job = Self(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is the struct this information class names, passed with its size.
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(crate) fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are open: the job's while `self` lives, the child's while
        // `child` does.
        let assigned = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Kills every process in the job, which exit with [`KILLED_CODE`].
    pub(crate) fn terminate(&self) {
        // SAFETY: a plain call on the job's own open handle. It fails only for a job that
        // is already empty, which leaves nothing to do.
        unsafe { TerminateJobObject(self.0, KILLED_CODE) };
    }

    #[cfg(test)]
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }
}

/// Closing the last handle kills whatever is left (`KILL_ON_JOB_CLOSE`).
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is this job's own, closed once here.
        unsafe { CloseHandle(self.0) };
    }
}

/**
 * Resumes a process created with `CREATE_SUSPENDED`. An error when ntdll refuses, so a child
 * stuck suspended is never mistaken for a slow one.
 */
pub(crate) fn resume(child: &Child) -> io::Result<()> {
    // SAFETY: the child's handle is open while `child` lives, with every access right.
    let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
    if status != 0 {
        return Err(io::Error::other(format!(
            "process {} couldn't be resumed (NTSTATUS {status:#010x})",
            child.id()
        )));
    }
    Ok(())
}

/// Blocks until the process whose handle is `process` has exited.
pub(crate) fn wait(process: HANDLE) -> io::Result<()> {
    // SAFETY: the caller keeps the handle open for the length of the call.
    if unsafe { WaitForSingleObject(process, INFINITE) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/**
 * This process's own job (`crate::tie_to_this_process`): created on first use and never
 * closed, so the OS closes it, killing what it holds, when this process ends. A static is
 * never dropped.
 */
pub(crate) fn app_job() -> Result<&'static Job, &'static str> {
    static JOB: OnceLock<Result<Job, String>> = OnceLock::new();
    JOB.get_or_init(|| Job::new().map_err(|error| format!("couldn't create a job object: {error}")))
        .as_ref()
        .map_err(String::as_str)
}
