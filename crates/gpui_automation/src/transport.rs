//! Where requests come from and where replies go: stdin/stdout by default,
//! or a named pipe (see [`crate::pipe`]) that drivers attach to while the
//! app runs. Either way the command loop sees one stream of [`Inbound`]
//! messages, each tagged with the client that sent it, and answers through
//! an [`Output`] that routes the reply back to that client.

use std::io::{BufRead as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::{Context as _, Result};
use futures::channel::mpsc;
use serde_json::Value;

/// Numbers connections in the order they were made, starting at 1. Only one
/// client is connected at a time.
pub(crate) type ClientId = u64;

/// Stdin/stdout is a single client for the life of the process.
pub(crate) const STDIO_CLIENT: ClientId = 1;

/// Queued for the command loop when stdin reaches EOF (`quit_on_eof`).
pub(crate) const EOF_QUIT: &str = r#"{"cmd":"quit"}"#;

/// Longest request line a pipe client may send before it is disconnected.
pub(crate) const MAX_LINE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Inbound {
    /// A driver connected; it is greeted with the `ready` event.
    Connected(ClientId),
    /// A request line from a driver.
    Line(ClientId, String),
}

/// Book-keeping shared by the reader threads and the command loop, so a
/// long wait can tell that nobody will ever read its reply.
///
/// Reading a redirected file is effectively instantaneous, so stdin's EOF is
/// usually seen before the first command even runs; bailing out of waits on
/// EOF alone would cut short every script's first `wait_ready`. A wait is
/// abandoned only when its client has hung up *and* nothing that client sent
/// is still queued behind it, i.e. the wait is genuinely the last thing that
/// client will ever see.
#[derive(Debug, Default)]
pub(crate) struct Clients {
    /// Messages sent to the command loop and not yet taken.
    pending: AtomicUsize,
    /// The client of the most recently queued message.
    last_queued: AtomicU64,
    /// Every client up to and including this one has hung up.
    hung_up_through: AtomicU64,
}

impl Clients {
    /// Count a message before it is sent, so the loop never takes a message
    /// that isn't reflected in `pending` yet.
    pub fn queue(&self, client: ClientId) {
        self.last_queued.store(client, Ordering::SeqCst);
        self.pending.fetch_add(1, Ordering::SeqCst);
    }

    /// The command loop took a message.
    pub fn dequeued(&self) {
        self.pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            })
            .ok();
    }

    pub fn hung_up(&self, client: ClientId) {
        self.hung_up_through.fetch_max(client, Ordering::SeqCst);
    }

    /// Whether a wait on behalf of `client` should give up.
    pub fn abandoned(&self, client: ClientId) -> bool {
        self.hung_up_through.load(Ordering::SeqCst) >= client
            && (self.pending.load(Ordering::SeqCst) == 0
                || self.last_queued.load(Ordering::SeqCst) != client)
    }
}

/// Where replies and events go.
#[derive(Clone)]
pub(crate) enum Output {
    Stdout,
    #[cfg(windows)]
    Pipe(crate::pipe::Writer),
}

impl Output {
    /// Send one JSON line to `client`. Replies to a client that has since
    /// disconnected are dropped: there is nobody to tell.
    pub fn send(&self, client: ClientId, value: &Value) {
        let mut line = value.to_string().into_bytes();
        line.push(b'\n');
        match self {
            Self::Stdout => {
                let mut out = std::io::stdout().lock();
                // A failed write means the driver is gone.
                let _ = out.write_all(&line);
                let _ = out.flush();
            }
            #[cfg(windows)]
            Self::Pipe(writer) => writer.send(client, line),
        }
        #[cfg(not(windows))]
        let _ = client;
    }

    /// Wait until everything sent so far has been written, so a `quit`
    /// reply isn't lost when the process exits right after it.
    pub fn flush(&self) {
        match self {
            // `send` already flushed.
            Self::Stdout => {}
            #[cfg(windows)]
            Self::Pipe(writer) => writer.flush(),
        }
    }
}

/// A request line as received: `None` for lines that carry nothing.
/// PowerShell pipes can prefix a BOM to the first line.
pub(crate) fn clean_line(line: &str) -> Option<&str> {
    let line = line.trim_start_matches('\u{feff}').trim();
    (!line.is_empty()).then_some(line)
}

/// Pump stdin lines into a channel from a dedicated thread (stdin reads
/// block, and must never block the UI thread).
pub(crate) fn spawn_stdin_pump(
    clients: Arc<Clients>,
    quit_on_eof: bool,
) -> Result<mpsc::UnboundedReceiver<Inbound>> {
    let (tx, rx) = mpsc::unbounded();
    // The only stdio client is there from the start and gets `ready` once
    // the window has drawn.
    clients.queue(STDIO_CLIENT);
    tx.unbounded_send(Inbound::Connected(STDIO_CLIENT))
        .context("queueing the ready event")?;
    std::thread::Builder::new()
        .name("automation-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let Some(line) = clean_line(&line) else {
                    continue;
                };
                clients.queue(STDIO_CLIENT);
                if tx
                    .unbounded_send(Inbound::Line(STDIO_CLIENT, line.to_string()))
                    .is_err()
                {
                    return;
                }
            }
            clients.hung_up(STDIO_CLIENT);
            if quit_on_eof {
                // Deliberately not counted: it isn't something the driver
                // sent, so it mustn't keep an earlier wait alive.
                tx.unbounded_send(Inbound::Line(STDIO_CLIENT, EOF_QUIT.into()))
                    .ok();
            }
        })
        .context("spawning the automation stdin thread")?;
    Ok(rx)
}

/// Splits a byte stream into request lines, like `BufRead::lines` but fed
/// in arbitrary pieces.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct LineBuffer {
    partial: Vec<u8>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl LineBuffer {
    /// Add bytes and return every line they complete (cleaned, empty lines
    /// skipped). Fails once an unfinished line grows past [`MAX_LINE`].
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, usize> {
        let mut lines = Vec::new();
        let mut rest = bytes;
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            self.partial.extend_from_slice(&rest[..end]);
            rest = &rest[end + 1..];
            let line = std::mem::take(&mut self.partial);
            if let Some(line) = clean_line(&String::from_utf8_lossy(&line)) {
                lines.push(line.to_string());
            }
        }
        self.partial.extend_from_slice(rest);
        if self.partial.len() > MAX_LINE {
            return Err(self.partial.len());
        }
        Ok(lines)
    }

    /// The last line when the stream ends without a newline.
    pub fn finish(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.partial);
        clean_line(&String::from_utf8_lossy(&line)).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn eof_only_abandons_the_last_command() {
        let clients = Clients::default();
        clients.queue(STDIO_CLIENT);
        clients.queue(STDIO_CLIENT);
        clients.hung_up(STDIO_CLIENT);
        clients.dequeued();
        assert!(
            !clients.abandoned(STDIO_CLIENT),
            "one real command still queued"
        );
        clients.dequeued();
        assert!(clients.abandoned(STDIO_CLIENT));
        clients.dequeued();
        assert_eq!(
            clients.pending.load(Ordering::SeqCst),
            0,
            "saturates at zero"
        );
    }

    #[test]
    fn a_connected_client_is_never_abandoned() {
        let clients = Clients::default();
        assert!(!clients.abandoned(1));
        clients.queue(1);
        clients.dequeued();
        assert!(!clients.abandoned(1));
    }

    #[test]
    fn a_newer_client_does_not_keep_a_gone_one_waiting() {
        let clients = Clients::default();
        clients.queue(1);
        clients.dequeued();
        clients.hung_up(1);
        clients.queue(2);
        clients.queue(2);
        assert!(clients.abandoned(1), "client 1's wait ends");
        assert!(!clients.abandoned(2), "client 2 is still connected");
        clients.hung_up(2);
        assert!(!clients.abandoned(2), "client 2 still has lines queued");
        clients.dequeued();
        clients.dequeued();
        assert!(clients.abandoned(2));
    }

    #[test]
    fn lines_are_split_across_pieces() {
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(b"{\"cmd\":").unwrap(), Vec::<String>::new());
        assert_eq!(
            buffer
                .push(b" \"state\"}\r\n\n  \n\xef\xbb\xbf{\"cmd\": \"windows\"}\n{\"cmd\"")
                .unwrap(),
            [r#"{"cmd": "state"}"#, r#"{"cmd": "windows"}"#]
        );
        assert_eq!(buffer.finish().as_deref(), Some(r#"{"cmd""#));
        assert_eq!(buffer.finish(), None);
        assert_eq!(buffer.push(b"   \r\n").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn overlong_lines_are_refused() {
        let mut buffer = LineBuffer::default();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut result = Ok(Vec::new());
        for _ in 0..=MAX_LINE / chunk.len() {
            result = buffer.push(&chunk);
            if result.is_err() {
                break;
            }
        }
        assert!(result.is_err());
    }

    #[test]
    fn clean_lines_drop_bom_and_blank() {
        assert_eq!(
            clean_line("\u{feff} {\"cmd\":\"x\"} "),
            Some("{\"cmd\":\"x\"}")
        );
        assert_eq!(clean_line(" \t "), None);
    }
}
