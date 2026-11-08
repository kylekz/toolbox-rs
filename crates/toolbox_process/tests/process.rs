//! `toolbox_process` against its test child (tests/support/child.rs): pipes, input, the
//! environment, timeouts, cancels, graceful stops, whole-tree kills, streaming and `Override`.

use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use toolbox_process::{
    CancelToken, Command, Event, LocateError, NO_TIMEOUT, Origin, Override, ProcessError, Stop,
    Stream, run, run_lines, spawn_streaming,
};

const LONG: Duration = Duration::from_secs(20);
const CHILD: &str = env!("CARGO_BIN_EXE_toolbox_process_child");

fn child(steps: &[&str]) -> Command {
    Command::new(CHILD).args(steps)
}

/// The pids the child printed as `<key>=PID` lines.
fn pids(stdout: &[u8], key: &str) -> Vec<u32> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|pid| pid.parse().unwrap())
        .collect()
}

/// Waits up to 5 s for each pid to be gone (an orphan is reaped by init or launchd).
#[cfg(unix)]
fn assert_all_ended(pids: &[u32]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    for &pid in pids {
        // SAFETY: signal 0 only checks the pid.
        while unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            assert!(Instant::now() < deadline, "pid {pid} is still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(windows)]
fn assert_all_ended(pids: &[u32]) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    for &pid in pids {
        // SAFETY: a plain call; the result is checked.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            continue;
        }
        // SAFETY: the handle was just opened, and is closed once here.
        let waited = unsafe { WaitForSingleObject(handle, 5_000) };
        unsafe { CloseHandle(handle) };
        assert_eq!(waited, WAIT_OBJECT_0, "pid {pid} is still running");
    }
}

#[test]
fn run_collects_both_pipes_byte_for_byte() {
    let output = run(
        &child(&[
            "out=카리나 Karina\\n",
            "err=WARNING: slow\\r\\n",
            "out=no newline",
            "exit=3",
        ]),
        LONG,
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(output.stdout.clone()).unwrap(),
        "카리나 Karina\nno newline"
    );
    assert_eq!(output.stderr, b"WARNING: slow\r\n");
    assert_eq!(output.stderr_text(), "WARNING: slow");
}

#[test]
fn run_collects_more_than_a_pipe_holds() {
    let output = run(&child(&["big=3000000"]), LONG, &CancelToken::new()).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 3_000_000);
}

#[test]
fn run_lines_hands_over_lines_without_their_endings() {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = run_lines(
        &child(&[
            "out=one\\ntwo\\n",
            "err=ERROR: a\\r\\nWARNING: b\\n",
            "out=last",
        ]),
        LONG,
        &CancelToken::new(),
        |stream, line| {
            let line = String::from_utf8(line.to_vec()).unwrap();
            match stream {
                Stream::Stdout => stdout.push(line),
                Stream::Stderr => stderr.push(line),
            }
        },
    )
    .unwrap();
    assert!(status.success());
    assert_eq!(stdout, ["one", "two", "last"]);
    assert_eq!(stderr, ["ERROR: a", "WARNING: b"]);
}

#[test]
fn applies_the_environment_and_working_dir() {
    let cwd = tempfile::tempdir().unwrap();
    let command = child(&["env=TBX_MARK", "env=TBX_GONE", "cwd"])
        .env("TBX_MARK", "1")
        .env("TBX_GONE", "1")
        .env_remove("TBX_GONE")
        .current_dir(cwd.path());
    let output = run(&command, LONG, &CancelToken::new()).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[..2], ["TBX_MARK=1", "TBX_GONE unset"]);
    assert_eq!(
        std::fs::canonicalize(lines[2]).unwrap(),
        std::fs::canonicalize(cwd.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn a_cleared_environment_has_only_what_is_set_after() {
    let command = child(&["env=PATH", "env=TBX_MARK"])
        .env("TBX_EARLY", "1")
        .env_clear()
        .env("TBX_MARK", "2");
    let output = run(&command, LONG, &CancelToken::new()).unwrap();
    assert_eq!(output.stdout, b"PATH unset\nTBX_MARK=2\n");
}

#[test]
fn input_is_written_then_stdin_closes() {
    let input: Vec<u8> = (0..=255).cycle().take(1_000_000).collect();
    let output = run(
        &child(&["cat"]).input(input.clone()),
        LONG,
        &CancelToken::new(),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, input);

    let empty = run(&child(&["cat"]), LONG, &CancelToken::new()).unwrap();
    assert!(empty.status.success());
    assert_eq!(empty.stdout, b"");
}

#[test]
fn a_missing_program_doesnt_start() {
    let dir = tempfile::tempdir().unwrap();
    let result = run(
        &Command::new(dir.path().join("missing")),
        LONG,
        &CancelToken::new(),
    );
    assert!(
        matches!(result, Err(ProcessError::Start { .. })),
        "{result:?}"
    );
}

#[test]
fn a_cancelled_token_starts_nothing() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = run(&child(&["out=ran"]), LONG, &cancel);
    assert!(
        matches!(result, Err(ProcessError::Cancelled { .. })),
        "{result:?}"
    );
}

#[test]
fn no_timeout_waits_for_the_end() {
    let output = run(
        &child(&["sleep=100", "out=done"]),
        NO_TIMEOUT,
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(output.stdout, b"done");
}

/** A timeout ends the process and the one it started, which holds its pipes. */
#[test]
fn a_timeout_ends_the_whole_tree() {
    let started = Instant::now();
    let mut stdout = Vec::new();
    let result = run_lines(
        &child(&["spawn=sleep=20000", "sleep=20000"]),
        Duration::from_millis(500),
        &CancelToken::new(),
        |_, line| stdout.extend_from_slice(&[line, b"\n"].concat()),
    );
    assert!(
        matches!(result, Err(ProcessError::TimedOut { .. })),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let children = pids(&stdout, "child");
    assert_eq!(children.len(), 1, "{}", String::from_utf8_lossy(&stdout));
    assert_all_ended(&children);
}

/**
 * What a process leaves running when it exits goes with it. On Windows this is also the
 * suspended start: the child is started before its parent prints anything.
 */
#[test]
fn leftovers_end_with_the_process() {
    let started = Instant::now();
    let output = run(
        &child(&["spawn=sleep=20000", "out=done\\n"]),
        LONG,
        &CancelToken::new(),
    )
    .unwrap();
    assert!(output.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let children = pids(&output.stdout, "child");
    assert_eq!(children.len(), 1);
    assert_all_ended(&children);
}

/** A run that unwinds (a panic in `on_line`) still takes the tree down with it. */
#[test]
fn a_panicking_line_handler_ends_the_tree() {
    let mut seen = Vec::new();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_lines(
            &child(&["spawn=sleep=20000", "out=ready\\n", "sleep=20000"]),
            LONG,
            &CancelToken::new(),
            |_, line| {
                seen.extend_from_slice(&[line, b"\n"].concat());
                if line == b"ready" {
                    panic!("the handler gave up");
                }
            },
        )
    }));
    assert!(unwound.is_err());
    let children = pids(&seen, "child");
    assert_eq!(children.len(), 1, "{}", String::from_utf8_lossy(&seen));
    assert_all_ended(&children);
}

/** A cancel is SIGINT first, which a CLI tool answers by cleaning up and exiting. */
#[cfg(unix)]
#[test]
fn a_cancel_interrupts_the_group() {
    let cancel = CancelToken::new();
    let mut lines = Vec::new();
    /* Cancelled once it is running, so its handler is in place. */
    let result = run_lines(
        &child(&["trap=exit", "out=started\\n", "sleep=20000"]),
        LONG,
        &cancel,
        |_, line| {
            cancel.cancel();
            lines.push(String::from_utf8_lossy(line).into_owned());
        },
    );
    assert!(
        matches!(result, Err(ProcessError::Cancelled { .. })),
        "{result:?}"
    );
    assert_eq!(lines, ["started", "interrupted"]);
}

/** A process that ignores SIGINT gets SIGKILL after the grace period, with its tree. */
#[cfg(unix)]
#[test]
fn sigkill_follows_an_unanswered_sigint() {
    let grace = Duration::from_millis(600);
    let ignoring = child(&[
        "trap=ignore",
        "spawn=trap=ignore,sleep=20000",
        "sleep=20000",
    ])
    .stop(Stop::Graceful(grace));
    let cancel = CancelToken::new();
    let mut stdout = Vec::new();
    let started = Instant::now();
    let result = run_lines(&ignoring, LONG, &cancel, |_, line| {
        cancel.cancel();
        stdout.extend_from_slice(&[line, b"\n"].concat());
    });
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ProcessError::Cancelled { .. })),
        "{result:?}"
    );
    assert!(elapsed >= grace && elapsed < grace * 3, "{elapsed:?}");
    assert_all_ended(&pids(&stdout, "child"));

    let started = Instant::now();
    let result = run(&ignoring, Duration::from_millis(200), &CancelToken::new());
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(ProcessError::TimedOut { .. })),
        "{result:?}"
    );
    assert!(elapsed >= grace && elapsed < grace * 3, "{elapsed:?}");
}

#[test]
fn a_kill_stops_at_once() {
    let started = Instant::now();
    let result = run(
        &child(&["sleep=20000"]).stop(Stop::Kill),
        Duration::from_millis(200),
        &CancelToken::new(),
    );
    assert!(
        matches!(result, Err(ProcessError::TimedOut { .. })),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/** A process that left the tree on purpose holds the run up by the drain, no longer. */
#[cfg(unix)]
#[test]
fn an_escaped_process_holds_up_the_run_by_a_second_at_most() {
    let started = Instant::now();
    let output = run(
        &child(&["escape=sleep=20000", "out=done\\n"]),
        LONG,
        &CancelToken::new(),
    )
    .unwrap();
    let elapsed = started.elapsed();
    let escaped = pids(&output.stdout, "escaped");
    for &pid in &escaped {
        // SAFETY: the test's own grandchild, still running.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    assert!(output.status.success());
    assert_eq!(escaped.len(), 1);
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );
}

/// Every event a streaming process sends, until its exit or 10 s.
fn collect(events: &mpsc::Receiver<Event>) -> Vec<Event> {
    let mut collected = Vec::new();
    while let Ok(event) = events.recv_timeout(Duration::from_secs(10)) {
        let exit = matches!(event, Event::Exit(_));
        collected.push(event);
        if exit {
            break;
        }
    }
    collected
}

fn stdout_of(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Stdout(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

#[test]
fn a_streaming_process_reads_what_it_is_written() {
    let (sender, events) = mpsc::channel();
    let handle = spawn_streaming(
        &child(&["err=warming up\\r\\n", "cat", "exit=4"]).input("first\n"),
        move |event| drop(sender.send(event)),
    )
    .unwrap();
    assert_ne!(handle.pid(), 0);
    handle.write(b"second\n").unwrap();
    handle.close_stdin();
    assert!(
        matches!(handle.write(b"late"), Err(ProcessError::Ended { .. })),
        "a closed stdin takes nothing"
    );
    let events = collect(&events);
    assert_eq!(stdout_of(&events), b"first\nsecond\n");
    assert!(events.contains(&Event::Stderr(b"warming up".to_vec())));
    match events.last() {
        Some(Event::Exit(Some(status))) => assert_eq!(status.code(), Some(4)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_write_after_the_exit_finds_it_ended() {
    let (sender, events) = mpsc::channel();
    let handle =
        spawn_streaming(&child(&["exit=0"]), move |event| drop(sender.send(event))).unwrap();
    assert!(matches!(
        collect(&events).last(),
        Some(Event::Exit(Some(_)))
    ));
    let result = handle.write(b"anyone?\n");
    assert!(
        matches!(result, Err(ProcessError::Ended { .. })),
        "{result:?}"
    );
}

/** Dropping the handle kills the process and its tree, and drops the sink. */
#[test]
fn dropping_the_handle_kills_the_tree_and_drops_the_sink() {
    let (sender, events) = mpsc::channel();
    let handle = spawn_streaming(
        &child(&["spawn=sleep=20000", "out=ready\\n", "sleep=20000"]),
        move |event| drop(sender.send(event)),
    )
    .unwrap();
    let mut stdout = Vec::new();
    while !stdout.ends_with(b"ready\n") {
        match events.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Stdout(bytes) => stdout.extend(bytes),
            event => panic!("{event:?}"),
        }
    }
    let pid = handle.pid();
    drop(handle);
    /* The sink, and with it the sender, is gone: no exit is delivered after the drop. */
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    let mut tree = pids(&stdout, "child");
    tree.push(pid);
    assert_all_ended(&tree);
}

/** A handle's graceful stop asks first: the process gets to say it was interrupted. */
#[cfg(unix)]
#[test]
fn a_handle_stops_gracefully() {
    let (sender, events) = mpsc::channel();
    let handle = spawn_streaming(
        &child(&["trap=exit", "out=ready\\n", "sleep=20000"]),
        move |event| drop(sender.send(event)),
    )
    .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)).unwrap(),
        Event::Stdout(b"ready\n".to_vec())
    );
    handle.stop(Stop::Graceful(Duration::from_secs(10)));
    let events = collect(&events);
    assert!(events.contains(&Event::Stderr(b"interrupted".to_vec())));
    match events.last() {
        Some(Event::Exit(Some(status))) => assert_eq!(status.code(), Some(1)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_handle_kill_reports_the_exit() {
    let (sender, events) = mpsc::channel();
    let handle = spawn_streaming(&child(&["sleep=20000"]), move |event| {
        drop(sender.send(event))
    })
    .unwrap();
    handle.stop(Stop::Kill);
    match collect(&events).last() {
        Some(Event::Exit(Some(status))) => assert!(!status.success()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_child_started_elsewhere_can_be_tied_to_this_process() {
    let mut child = std::process::Command::new(CHILD)
        .arg("sleep=20000")
        .spawn()
        .unwrap();
    let tied = toolbox_process::tie_to_this_process(&child);
    child.kill().unwrap();
    child.wait().unwrap();
    tied.unwrap();
}

/// The test child's folder and file name, as `PATH` and a name to look up.
fn child_on_path() -> (OsString, OsString) {
    let path = Path::new(CHILD);
    (
        path.parent().unwrap().as_os_str().to_owned(),
        path.file_stem().unwrap().to_owned(),
    )
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

#[test]
fn an_override_names_the_program_or_path_finds_it() {
    let (dir, name) = child_on_path();
    let cwd = std::env::current_dir().unwrap();
    let finder = Override::new("TBX_TOOL", &name);
    let env = |vars: Vec<(&'static str, OsString)>| {
        move |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        }
    };

    let found = finder
        .locate_with(env(vec![("PATH", dir.clone())]), &cwd)
        .unwrap();
    assert_eq!(found.origin, Origin::Path);
    assert!(same_file(&found.program, Path::new(CHILD)), "{found:?}");

    let found = finder
        .locate_with(
            env(vec![("PATH", dir.clone()), ("TBX_TOOL", OsString::new())]),
            &cwd,
        )
        .unwrap();
    assert_eq!(found.origin, Origin::Path, "an empty variable is unset");

    let found = finder
        .locate_with(env(vec![("TBX_TOOL", CHILD.into())]), &cwd)
        .unwrap();
    assert_eq!(found.origin, Origin::Var);
    assert!(same_file(&found.program, Path::new(CHILD)), "{found:?}");

    let missing = tempfile::tempdir().unwrap();
    let result = finder.locate_with(
        env(vec![
            ("PATH", dir.clone()),
            ("TBX_TOOL", missing.path().join("nothing").into()),
        ]),
        &cwd,
    );
    assert!(
        matches!(result, Err(LocateError::BadVar { .. })),
        "{result:?}"
    );

    let result = finder.locate_with(env(vec![("PATH", missing.path().into())]), &cwd);
    match result {
        Err(error @ LocateError::NotFound { .. }) => assert!(
            error.to_string().contains("set TBX_TOOL to its path"),
            "{error}"
        ),
        other => panic!("{other:?}"),
    }
}
