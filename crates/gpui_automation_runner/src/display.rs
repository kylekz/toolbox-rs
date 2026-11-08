//! The display the app's windows open on (tests/e2e/README.md "Linux").
//!
//! On Linux the runner starts an Xvfb display of its own for the run (`Xvfb :<n> -screen 0
//! 2560x1600x24`, the first `n` from 90 no live server holds) and starts every app on it under
//! X11 alone (`DISPLAY=:<n>`, no `WAYLAND_DISPLAY`), so no window ever reaches the desktop of
//! the person at the machine (WSLg's, under WSL) and screenshots are X11 `GetImage`s of a
//! display nothing covers. The server ends with the run: asked to (`SIGTERM`, so it removes its
//! lock file and socket), then killed if it hasn't within a few seconds; and it dies with the
//! runner however the runner ends (`PR_SET_PDEATHSIG`). `--host-display` keeps the display the
//! runner was started with instead: an explicit exploratory look on a real compositor, whose
//! windows do appear on that desktop. Elsewhere the app opens on the desktop as it is,
//! background mode keeping it out of the way.

use std::path::Path;

use anyhow::Result;

/// Where the app's windows go for this run.
pub struct Display {
    /// What the run's banner says about it.
    pub description: String,
    /// The environment every app process gets on top of the runner's: a value to set, or
    /// `None` to remove.
    env: Vec<(&'static str, Option<String>)>,
    /// The Xvfb server this run started, ended with the run.
    #[cfg(target_os = "linux")]
    server: Option<std::process::Child>,
}

impl Display {
    /// The display for this run: the host's when `host` (or off Linux), else (Linux) a fresh
    /// Xvfb whose output goes to `log`.
    pub fn start(host: bool, log: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        if !host {
            return xvfb::start(log);
        }
        let _ = (host, log);
        Ok(Self {
            description: "the desktop".to_string(),
            env: Vec::new(),
            #[cfg(target_os = "linux")]
            server: None,
        })
    }

    /// What points an app process at this display (`run::Config::display_env`).
    pub fn env(&self) -> Vec<(&'static str, Option<String>)> {
        self.env.clone()
    }
}

#[cfg(target_os = "linux")]
impl Drop for Display {
    fn drop(&mut self) {
        if let Some(mut server) = self.server.take() {
            xvfb::stop(&mut server);
        }
    }
}

#[cfg(target_os = "linux")]
mod xvfb {
    use std::fs::File;
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail};

    use super::Display;

    /// The display numbers tried, the first no live server holds taken; well clear of a
    /// desktop's `:0`.
    const NUMBERS: std::ops::RangeInclusive<u32> = 90..=189;
    /// The screen: room for the largest window a scenario asks for, at 24-bit colour.
    const SCREEN: &str = "2560x1600x24";
    /// How long Xvfb may take to say it listens.
    const READY: Duration = Duration::from_secs(15);
    /// How long Xvfb may take to end once asked, before it is killed.
    const STOP: Duration = Duration::from_secs(5);

    /// Starts Xvfb on the first display number no live server holds (module docs). A lock a
    /// dead server left is no obstacle: Xvfb takes the number over itself, and a number taken
    /// meanwhile makes it end without listening, after which the next is tried.
    pub fn start(log: &Path) -> Result<Display> {
        for number in NUMBERS {
            if held(number) {
                continue;
            }
            let stderr =
                File::create(log).with_context(|| format!("creating {}", log.display()))?;
            let mut command = Command::new("Xvfb");
            // `-displayfd 1`: Xvfb writes the display number to its stdout once it listens.
            command
                .arg(format!(":{number}"))
                .args([
                    "-screen",
                    "0",
                    SCREEN,
                    "-nolisten",
                    "tcp",
                    "-displayfd",
                    "1",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(stderr);
            // SAFETY: the closure runs in the child between fork and exec and calls only
            // `prctl`, which is async-signal-safe and touches no memory of this process.
            unsafe {
                command.pre_exec(|| {
                    // Ended with the runner's thread that starts it (the main one), however
                    // the runner ends.
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut server = match command.spawn() {
                Ok(server) => server,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
                    "Xvfb isn't installed (the e2e runner opens the app on a display of its own \
                     on Linux; apt's xvfb has it), or pass --host-display to use this session's"
                ),
                Err(error) => return Err(error).context("starting Xvfb"),
            };
            let stdout = server
                .stdout
                .take()
                .context("Xvfb's stdout was not piped")?;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let _ = BufReader::new(stdout).read_line(&mut line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(READY) {
                Ok(line) if line.trim() == number.to_string() => {
                    return Ok(Display {
                        description: format!("Xvfb :{number}"),
                        env: vec![
                            ("DISPLAY", Some(format!(":{number}"))),
                            ("WAYLAND_DISPLAY", None),
                        ],
                        server: Some(server),
                    });
                }
                // It ended without listening: another server took the number meanwhile.
                Ok(_) => stop(&mut server),
                Err(_) => {
                    stop(&mut server);
                    bail!(
                        "Xvfb :{number} didn't start within {} s; see {}",
                        READY.as_secs(),
                        log.display()
                    );
                }
            }
        }
        bail!(
            "no free X display between :{} and :{} for Xvfb",
            NUMBERS.start(),
            NUMBERS.end()
        )
    }

    /// Ends `server`: `SIGTERM`, on which an X server removes its lock file and socket, then
    /// `SIGKILL` if it is still there after [`STOP`].
    pub fn stop(server: &mut Child) {
        if let Ok(Some(_)) = server.try_wait() {
            return;
        }
        if let Ok(pid) = libc::pid_t::try_from(server.id()) {
            // SAFETY: `kill` takes no pointers; `pid` is this child's, not yet reaped (so not
            // reused).
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let deadline = Instant::now() + STOP;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = server.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = server.kill();
        let _ = server.wait();
    }

    /// Whether a live server holds display `number`: its lock file names a process that runs.
    fn held(number: u32) -> bool {
        match std::fs::read_to_string(format!("/tmp/.X{number}-lock")) {
            Ok(text) => lock_pid(&text).is_none_or(runs),
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// The process an X lock file names: its decimal pid, space-padded (`"   1234\n"`).
    pub(super) fn lock_pid(text: &str) -> Option<libc::pid_t> {
        text.trim().parse().ok().filter(|pid| *pid > 0)
    }

    /// Whether process `pid` exists (one of another user's included).
    pub(super) fn runs(pid: libc::pid_t) -> bool {
        // SAFETY: `kill` with signal 0 sends nothing and takes no pointers.
        let sent = unsafe { libc::kill(pid, 0) };
        sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::xvfb::{lock_pid, runs};

    #[test]
    fn a_lock_names_its_server() {
        assert_eq!(lock_pid("      1234\n"), Some(1234));
        assert_eq!(lock_pid("1234"), Some(1234));
        assert_eq!(lock_pid(""), None);
        assert_eq!(lock_pid("x11\n"), None);
        assert_eq!(lock_pid("0\n"), None);
    }

    #[test]
    fn a_dead_server_holds_nothing() {
        let me = libc::pid_t::try_from(std::process::id()).unwrap();
        assert!(runs(me));
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let pid = libc::pid_t::try_from(gone.id()).unwrap();
        gone.wait().unwrap();
        assert!(!runs(pid), "a reaped child");
    }
}
