//! The Windows tree from inside: a process runs in a job of its own, a tied child is in the
//! app's job, and a job's end kills with [`windows::KILLED_CODE`].

use std::os::windows::io::AsRawHandle as _;
use std::process::Stdio;
use std::time::Duration;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;

use crate::pipes::{Mode, Msg};
use crate::{Command, tree, windows};

/// A process that runs for about 30 s, printing to stdout, as the test child does elsewhere
/// (whose path only integration tests are given).
fn pinging() -> Command {
    Command::new("ping").args(["-n", "30", "127.0.0.1"])
}

/// Whether `process` is in `job`.
fn in_job(process: HANDLE, job: HANDLE) -> bool {
    let mut inside = 0;
    // SAFETY: both handles are open for the call; `inside` is a BOOL to write.
    let asked = unsafe { IsProcessInJob(process, job, &mut inside) };
    assert_ne!(asked, 0, "{}", std::io::Error::last_os_error());
    inside != 0
}

#[test]
fn a_process_runs_in_a_job_of_its_own_and_its_end_kills_it() {
    let started = tree::start(&pinging(), false, (Mode::Chunks, Mode::Chunks)).unwrap();
    let (process, job) = started.process.handles();
    assert!(in_job(process, job), "the process is in its job");
    let mut other = pinging().to_std().stdout(Stdio::null()).spawn().unwrap();
    let other_inside = in_job(other.as_raw_handle(), job);
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!other_inside, "another process isn't in its job");

    started.process.kill();
    let exited = loop {
        if let Msg::Exited(status) = started
            .rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the exit within 10 s")
        {
            break status.unwrap();
        }
    };
    assert_eq!(exited.code(), Some(windows::KILLED_CODE as i32));
}

#[test]
fn a_tied_child_is_in_the_apps_job() {
    let mut child = pinging().to_std().stdout(Stdio::null()).spawn().unwrap();
    crate::tie_to_this_process(&child).unwrap();
    let job = windows::app_job().unwrap().handle();
    let inside = in_job(child.as_raw_handle(), job);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(inside, "the child is in the app's job");
}
