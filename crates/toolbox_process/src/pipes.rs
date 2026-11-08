//! The threads around a process: a reader per output pipe, the waiter, and a stdin writer for
//! [`crate::Command::input`]. Readers and the waiter send to one bounded channel.

use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::process::{ChildStdin, ExitStatus};
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use crate::tree::Process;

/// How many chunks or lines wait for the consumer before the readers wait for it.
pub(crate) const QUEUE: usize = 256;

/// How much one read of a pipe takes, in [`Mode::Chunks`].
const CHUNK: usize = 64 * 1024;

/// The longest line a reader hands over in [`Mode::Lines`]; a longer one arrives in pieces of
/// this size, so a program that never ends a line can't make the reader grow without bound.
pub const LINE_MAX: usize = 1024 * 1024;

/// Which of a process's output pipes something came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What the reader and waiter threads send.
pub(crate) enum Msg {
    /// A chunk as the pipe gave it, or a line without its `\n` or `\r\n`.
    Data(Stream, Vec<u8>),
    /// The pipe reached its end (or failed).
    End,
    /// The process exited and was reaped (`Process::wait`).
    Exited(io::Result<ExitStatus>),
    /// The streaming handle was dropped: deliver nothing more.
    Stop,
}

/// How a reader splits what it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Chunks,
    Lines,
}

fn start_thread(name: &str, f: impl FnOnce() + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
}

pub(crate) fn start_reader(
    stream: Stream,
    mode: Mode,
    pipe: impl Read + Send + 'static,
    tx: SyncSender<Msg>,
) -> io::Result<()> {
    let name = match stream {
        Stream::Stdout => "tbx-proc-out",
        Stream::Stderr => "tbx-proc-err",
    };
    start_thread(name, move || {
        let send = |data| tx.send(Msg::Data(stream, data)).is_ok();
        let delivered = match mode {
            Mode::Chunks => read_chunks(pipe, send),
            Mode::Lines => read_lines(BufReader::new(pipe), send),
        };
        if delivered {
            let _ = tx.send(Msg::End);
        }
    })
}

/// The waiter thread: [`Process::wait`], then the exit.
pub(crate) fn start_waiter(process: Arc<Process>, tx: SyncSender<Msg>) -> io::Result<()> {
    start_thread("tbx-proc-wait", move || {
        let _ = tx.send(Msg::Exited(process.wait()));
    })
}

/**
 * Writes `bytes` to the process's stdin on a thread of its own, then closes it. A process may
 * exit without reading all of it; its exit status says so, so a broken pipe is no error here.
 */
pub(crate) fn start_writer(mut stdin: ChildStdin, bytes: Vec<u8>) -> io::Result<()> {
    start_thread("tbx-proc-in", move || {
        let _ = stdin.write_all(&bytes);
    })
}

/// Reads `pipe` to its end in chunks; `false` once `send` was refused (nobody listens).
fn read_chunks(mut pipe: impl Read, mut send: impl FnMut(Vec<u8>) -> bool) -> bool {
    let mut buffer = vec![0; CHUNK];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return true,
            Ok(read) => {
                if !send(buffer[..read].to_vec()) {
                    return false;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return true,
        }
    }
}

/// Reads `reader` to its end line by line ([`next_line`]); `false` once `send` was refused.
fn read_lines(mut reader: impl BufRead, mut send: impl FnMut(Vec<u8>) -> bool) -> bool {
    while let Ok(Some(line)) = next_line(&mut reader) {
        if !send(line) {
            return false;
        }
    }
    true
}

/**
 * The next line without its `\n` or `\r\n`, or the next [`LINE_MAX`] bytes of a longer one;
 * the last line needs no ending. `None` at the end.
 */
fn next_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok((!line.is_empty()).then_some(line));
        }
        let room = LINE_MAX - line.len();
        match buffer.iter().take(room).position(|&byte| byte == b'\n') {
            Some(at) => {
                line.extend_from_slice(&buffer[..at]);
                reader.consume(at + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            None => {
                let taken = buffer.len().min(room);
                line.extend_from_slice(&buffer[..taken]);
                reader.consume(taken);
                if line.len() == LINE_MAX {
                    return Ok(Some(line));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn lines(input: &[u8]) -> Vec<Vec<u8>> {
        let mut reader = BufReader::with_capacity(16, input);
        let mut lines = Vec::new();
        while let Some(line) = next_line(&mut reader).unwrap() {
            lines.push(line);
        }
        lines
    }

    #[test]
    fn lines_lose_their_endings_and_the_last_needs_none() {
        assert_eq!(lines(b""), Vec::<Vec<u8>>::new());
        assert_eq!(
            lines(b"a\nb\r\n\nlast"),
            [b"a".to_vec(), b"b".to_vec(), b"".to_vec(), b"last".to_vec()]
        );
        assert_eq!(lines(b"caf\xc3\xa9 \xff\n"), [b"caf\xc3\xa9 \xff".to_vec()]);
        assert_eq!(lines(b"one\r"), [b"one".to_vec()]);
    }

    #[test]
    fn a_long_line_arrives_in_pieces() {
        let mut input = vec![b'x'; LINE_MAX * 2 + 10];
        input.extend_from_slice(b"\nnext\n");
        let read = lines(&input);
        let sizes: Vec<_> = read.iter().map(Vec::len).collect();
        assert_eq!(sizes, [LINE_MAX, LINE_MAX, 10, 4]);
        assert_eq!(read[3], b"next");
    }

    #[test]
    fn chunks_carry_every_byte() {
        let input: Vec<u8> = (0..=255).cycle().take(CHUNK * 2 + 7).collect();
        let mut read = Vec::new();
        assert!(read_chunks(input.as_slice(), |chunk| {
            read.extend(chunk);
            true
        }));
        assert_eq!(read, input);
        assert!(!read_chunks(input.as_slice(), |_| false));
    }
}
