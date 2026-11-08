//! A fixture's helper process (docs/automation.md "Runner extensions"): a mock server the
//! scenario's app talks to, started before the app's first launch and killed when the fixture
//! drops it. It is the runner's child like the app: in the kill-on-close job on Windows, its
//! stderr in a log, its stdout read for the port it announces.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::http::{self, HttpResponse};
use crate::process::{ChildProcess, ReadError};

/// A running helper process that listens on `127.0.0.1:<port>`.
pub struct Sidecar {
    /// Held for its drop, which kills the process.
    _child: ChildProcess,
    port: u16,
}

impl Sidecar {
    /**
     * Starts `command` and waits up to `timeout` for the first stdout line that is JSON, which
     * must be `{"port": N}`; earlier lines are passed on to the runner's stderr. Its own stderr
     * goes to `stderr_log`. Fails when it exits or stays silent first, or on ctrl-c.
     */
    pub fn start(
        label: &'static str,
        command: Command,
        stderr_log: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        let mut child = ChildProcess::spawn(label, command, stderr_log)?;
        let deadline = Instant::now() + timeout;
        loop {
            let line = match child.read_line(deadline) {
                Ok(line) => line,
                Err(ReadError::Timeout) => bail!(
                    "{label} did not announce {{\"port\": N}} within {}s",
                    timeout.as_secs()
                ),
                Err(ReadError::Interrupted) => bail!("interrupted"),
                Err(ReadError::Closed) => bail!(
                    "{label} exited before announcing its port; stderr:\n{}",
                    child.stderr_tail(20)
                ),
            };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                eprintln!("e2e: {label} said: {line}");
                continue;
            };
            let port = value
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| anyhow!("{label} announced {line}, expected {{\"port\": N}}"))?;
            return Ok(Self {
                _child: child,
                port,
            });
        }
    }

    /// The port it announced.
    pub fn port(&self) -> u16 {
        self.port
    }

    /**
     * `method path` (path and query) with an optional JSON `body`, the whole response read
     * within `timeout`. Fails when it can't connect, times out, the response is malformed or on
     * ctrl-c ("interrupted"); any status is a response.
     */
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<HttpResponse> {
        http::request(
            SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            method,
            path,
            body,
            Instant::now() + timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;

    use pretty_assertions::assert_eq;

    use super::*;

    /// Set for the copy of this test binary that plays the sidecar.
    const CHILD_VAR: &str = "TOOLBOX_SIDECAR_TEST_CHILD";

    /**
     * Not a test of its own: the sidecar the other tests start, this test binary run again with
     * only this test and `CHILD_VAR` set. It prints the test harness's own lines first, which
     * `start` passes over, then its port, and answers one request with the request's line.
     */
    #[test]
    fn sidecar_child() {
        let Some(mode) = std::env::var_os(CHILD_VAR) else {
            return;
        };
        if mode == "silent-exit" {
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        /* On a line of its own: the harness has printed `test … ` without a newline. */
        println!("\n{{\"port\": {}}}", listener.local_addr().unwrap().port());
        std::io::stdout().flush().unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut line = String::new();
        while line != "\r\n" {
            line.clear();
            reader.read_line(&mut line).unwrap();
        }
        let body = request_line.trim_end();
        write!(
            reader.into_inner(),
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn child(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "sidecar::tests::sidecar_child", "--nocapture"])
            .env(CHILD_VAR, mode);
        command
    }

    #[test]
    fn a_sidecar_announces_its_port_and_answers_requests() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = Sidecar::start(
            "sidecar",
            child("serve"),
            &dir.path().join("sidecar.log"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_ne!(sidecar.port(), 0);
        let response = sidecar
            .request("GET", "/__mock/state", None, Duration::from_secs(30))
            .unwrap();
        assert!(response.is_success());
        assert_eq!(response.body, b"GET /__mock/state HTTP/1.1");
    }

    #[test]
    fn a_sidecar_that_exits_before_its_port_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Sidecar::start(
            "sidecar",
            child("silent-exit"),
            &dir.path().join("sidecar.log"),
            Duration::from_secs(30),
        )
        .err()
        .unwrap();
        assert!(
            format!("{err:#}").contains("sidecar exited before announcing its port"),
            "{err:#}"
        );
    }
}
