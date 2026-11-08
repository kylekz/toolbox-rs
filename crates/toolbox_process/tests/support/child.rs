//! The program the crate's tests run (tests/process.rs): it plays the steps its arguments
//! name, in order, flushing after each.
//!
//! - `out=TEXT`, `err=TEXT`: write TEXT to stdout or stderr (`\n` and `\r` spelled as escapes);
//! - `big=N`: write N bytes of `x` to stdout;
//! - `sleep=MS`, `exit=CODE`;
//! - `env=NAME`: write `NAME=value` or `NAME unset`, and a newline;
//! - `cwd`: write the working directory and a newline;
//! - `cat`: copy stdin to stdout until stdin ends;
//! - `spawn=STEPS`: start this program with STEPS (separated by `,`) sharing this one's stdio,
//!   without waiting for it, and write `child=PID` and a newline;
//! - `escape=STEPS` (Unix): the same in a process group of its own, out of the tree's reach,
//!   writing `escaped=PID`;
//! - `trap=exit` (Unix): from now on SIGINT writes `interrupted` to stderr and exits 1;
//!   `trap=ignore`: SIGINT does nothing.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    for step in std::env::args().skip(1) {
        let (name, value) = step.split_once('=').unwrap_or((&step, ""));
        let mut stdout = std::io::stdout().lock();
        match name {
            "out" => stdout.write_all(unescape(value).as_bytes()).unwrap(),
            "err" => std::io::stderr()
                .write_all(unescape(value).as_bytes())
                .unwrap(),
            "big" => stdout
                .write_all(&vec![b'x'; value.parse().unwrap()])
                .unwrap(),
            "sleep" => sleep(Duration::from_millis(value.parse().unwrap())),
            "exit" => {
                stdout.flush().unwrap();
                std::process::exit(value.parse().unwrap());
            }
            "env" => match std::env::var(value) {
                Ok(set) => writeln!(stdout, "{value}={set}").unwrap(),
                Err(_) => writeln!(stdout, "{value} unset").unwrap(),
            },
            "cwd" => writeln!(stdout, "{}", std::env::current_dir().unwrap().display()).unwrap(),
            "cat" => {
                let mut stdin = std::io::stdin().lock();
                let mut buffer = [0; 4096];
                loop {
                    let read = stdin.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    stdout.write_all(&buffer[..read]).unwrap();
                    stdout.flush().unwrap();
                }
            }
            "spawn" => writeln!(stdout, "child={}", start(this(value))).unwrap(),
            #[cfg(unix)]
            "escape" => {
                use std::os::unix::process::CommandExt as _;
                let mut command = this(value);
                command.process_group(0);
                writeln!(stdout, "escaped={}", start(command)).unwrap();
            }
            #[cfg(unix)]
            "trap" => trap(value),
            _ => panic!("unknown step {step}"),
        }
        stdout.flush().unwrap();
    }
}

fn this(steps: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(steps.split(','))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

/// Starts `command` and returns its pid, never waiting for it: it is meant to outlive this
/// process, so the tests see what becomes of it.
fn start(mut command: Command) -> u32 {
    #[expect(
        clippy::zombie_processes,
        reason = "it outlives this process on purpose"
    )]
    let child = command.spawn().unwrap();
    child.id()
}

fn unescape(text: &str) -> String {
    text.replace("\\n", "\n").replace("\\r", "\r")
}

/// Sleeps, but answers a trapped SIGINT within 10 ms.
fn sleep(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        #[cfg(unix)]
        if INTERRUPTED.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("interrupted");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn trap(how: &str) {
    extern "C" fn on_sigint(_: libc::c_int) {
        INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let handler = match how {
        "exit" => on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t,
        "ignore" => libc::SIG_IGN,
        _ => panic!("unknown trap {how}"),
    };
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe { libc::signal(libc::SIGINT, handler) };
}
