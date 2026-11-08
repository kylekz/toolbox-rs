//! [`crate::Options::pipe`]: the same JSONL protocol over a Windows named
//! pipe (`\\.\pipe\<name>`), so a driver can attach to an app that is
//! already running, e.g. one started with `cargo run`.
//!
//! One client at a time (a second one gets `ERROR_PIPE_BUSY`). Each client
//! is greeted with the `ready` event. Disconnecting does not quit the app;
//! the pipe waits for the next client. The pipe is created with
//! `FILE_FLAG_OVERLAPPED` because a reader blocked in `ReadFile` on a
//! synchronous handle would stall every `WriteFile` on it, and replies must
//! flow while the next request is being read. Replies are written by their
//! own thread so a client that stops reading never blocks the UI thread.

use anyhow::{Result, bail};

/// Longest pipe path Windows accepts.
const MAX_PATH_LEN: usize = 256;
const PREFIX: &str = r"\\.\pipe\";

/// The full pipe path for `name`, which may be a bare name (`slide`) or
/// already a pipe path (`\\.\pipe\slide`).
pub(crate) fn pipe_path(name: &str) -> Result<String> {
    let bare = match name.get(..PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(PREFIX) => &name[PREFIX.len()..],
        _ => name,
    };
    if bare.is_empty() {
        bail!("the automation pipe needs a name");
    }
    if bare.contains('\\') || bare.chars().any(char::is_control) {
        bail!("pipe name {name:?} may not contain backslashes or control characters");
    }
    let path = format!("{PREFIX}{bare}");
    if path.encode_utf16().count() > MAX_PATH_LEN {
        bail!("pipe name {name:?} is too long");
    }
    Ok(path)
}

#[cfg(windows)]
pub(crate) use imp::{Writer, serve};

#[cfg(not(windows))]
pub(crate) fn serve(
    name: &str,
    _clients: std::sync::Arc<crate::transport::Clients>,
) -> Result<(
    futures::channel::mpsc::UnboundedReceiver<crate::transport::Inbound>,
    crate::transport::Output,
)> {
    pipe_path(name)?;
    bail!("the automation pipe is only available on Windows (named pipes)")
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::sync::mpsc as std_mpsc;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use anyhow::{Context as _, Result, bail};
    use futures::channel::mpsc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    };
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::CreateEventW;

    use crate::transport::{ClientId, Clients, Inbound, LineBuffer, Output};

    const BUFFER: u32 = 64 * 1024;
    /// How long a flush (before quitting) waits for queued replies.
    const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

    /// Sends replies to the writer thread.
    #[derive(Clone)]
    pub(crate) struct Writer(std_mpsc::Sender<Write>);

    enum Write {
        Line(ClientId, Vec<u8>),
        Flush(std_mpsc::SyncSender<()>),
    }

    impl Writer {
        pub fn send(&self, client: ClientId, line: Vec<u8>) {
            // Fails only once the writer thread has died, already logged.
            let _ = self.0.send(Write::Line(client, line));
        }

        pub fn flush(&self) {
            let (tx, rx) = std_mpsc::sync_channel(1);
            if self.0.send(Write::Flush(tx)).is_ok() && rx.recv_timeout(FLUSH_TIMEOUT).is_err() {
                log::warn!("automation: replies still unwritten after {FLUSH_TIMEOUT:?}");
            }
        }
    }

    /// The server end of the pipe. The handle stays open for the life of the
    /// process; clients come and go through `ConnectNamedPipe` /
    /// `DisconnectNamedPipe`.
    struct Pipe {
        handle: HANDLE,
        /// The client whose replies may be written; `None` between clients.
        /// The writer holds this lock while writing, so a reply can never
        /// reach the client that connected after its requester left.
        current: Mutex<Option<ClientId>>,
    }

    // SAFETY: a pipe handle may be used from any thread, and it was opened
    // for overlapped I/O precisely so reads and writes can run concurrently.
    unsafe impl Send for Pipe {}
    // SAFETY: as above; `current` is the only mutable state and is locked.
    unsafe impl Sync for Pipe {}

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this struct and closed once.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    /// A manual-reset event for one thread's overlapped operations.
    struct Event(HANDLE);

    impl Event {
        fn new() -> io::Result<Self> {
            // SAFETY: null attributes and name create an anonymous event.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(event))
        }

        fn overlapped(&self) -> OVERLAPPED {
            OVERLAPPED {
                hEvent: self.0,
                ..Default::default()
            }
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            // SAFETY: owned by this struct and closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn last_error_is(err: &io::Error, code: u32) -> bool {
        err.raw_os_error() == Some(code as i32)
    }

    impl Pipe {
        fn lock(&self) -> MutexGuard<'_, Option<ClientId>> {
            self.current.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Finish an overlapped operation started on this handle.
        fn wait(&self, overlapped: &OVERLAPPED) -> io::Result<u32> {
            let mut transferred = 0;
            // SAFETY: `overlapped` belongs to an operation started on this
            // handle and outlives the (blocking) wait.
            if unsafe { GetOverlappedResult(self.handle, overlapped, &mut transferred, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(transferred)
        }

        /// Block until a client connects.
        fn connect(&self, event: &Event) -> io::Result<()> {
            loop {
                let mut overlapped = event.overlapped();
                // SAFETY: a live pipe handle and an OVERLAPPED that stays in
                // place until `wait` returns.
                if unsafe { ConnectNamedPipe(self.handle, &mut overlapped) } != 0 {
                    return Ok(());
                }
                let err = io::Error::last_os_error();
                if last_error_is(&err, ERROR_PIPE_CONNECTED) {
                    // The client connected before we started listening.
                    return Ok(());
                }
                if last_error_is(&err, ERROR_NO_DATA) {
                    // It connected and already left; reset and listen again.
                    self.disconnect();
                    continue;
                }
                if !last_error_is(&err, ERROR_IO_PENDING) {
                    return Err(err);
                }
                return self.wait(&overlapped).map(drop);
            }
        }

        fn disconnect(&self) {
            // SAFETY: a live pipe handle; disconnecting an idle pipe is harmless.
            unsafe {
                DisconnectNamedPipe(self.handle);
            }
        }

        fn read(&self, event: &Event, buf: &mut [u8]) -> io::Result<u32> {
            let mut overlapped = event.overlapped();
            let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            // SAFETY: `buf` and `overlapped` stay alive and in place until
            // `wait` has returned.
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr(),
                    len,
                    std::ptr::null_mut(),
                    &mut overlapped,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if !last_error_is(&err, ERROR_IO_PENDING) {
                    return Err(err);
                }
            }
            self.wait(&overlapped)
        }

        fn write_all(&self, event: &Event, mut bytes: &[u8]) -> io::Result<()> {
            while !bytes.is_empty() {
                let mut overlapped = event.overlapped();
                let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                // SAFETY: `bytes` and `overlapped` stay alive and in place
                // until `wait` has returned.
                let ok = unsafe {
                    WriteFile(
                        self.handle,
                        bytes.as_ptr(),
                        len,
                        std::ptr::null_mut(),
                        &mut overlapped,
                    )
                };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    if !last_error_is(&err, ERROR_IO_PENDING) {
                        return Err(err);
                    }
                }
                let written = self.wait(&overlapped)? as usize;
                if written == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                bytes = &bytes[written.min(bytes.len())..];
            }
            Ok(())
        }
    }

    /// Create the pipe and start serving it. Fails if the name is taken, so
    /// two apps never share one pipe.
    pub(crate) fn serve(
        name: &str,
        clients: Arc<Clients>,
    ) -> Result<(mpsc::UnboundedReceiver<Inbound>, Output)> {
        let path = super::pipe_path(name)?;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string; null security
        // attributes give the default DACL (the creator and admins may
        // write, others may only read).
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            if last_error_is(&err, ERROR_ACCESS_DENIED) || last_error_is(&err, ERROR_PIPE_BUSY) {
                bail!("{path} is already in use (is another instance running?)");
            }
            return Err(err).with_context(|| format!("creating {path}"));
        }
        let pipe = Arc::new(Pipe {
            handle,
            current: Mutex::new(None),
        });

        let (tx, rx) = mpsc::unbounded();
        let (write_tx, write_rx) = std_mpsc::channel();
        std::thread::Builder::new()
            .name("automation-pipe".into())
            .spawn({
                let pipe = pipe.clone();
                move || accept_loop(&pipe, &tx, &clients)
            })
            .context("spawning the automation pipe thread")?;
        std::thread::Builder::new()
            .name("automation-pipe-writer".into())
            .spawn(move || write_loop(&pipe, write_rx))
            .context("spawning the automation pipe writer")?;
        log::info!("automation: listening on {path}");
        Ok((rx, Output::Pipe(Writer(write_tx))))
    }

    /// Serve clients one after another until the command loop goes away.
    fn accept_loop(pipe: &Pipe, tx: &mpsc::UnboundedSender<Inbound>, clients: &Clients) {
        let event = match Event::new() {
            Ok(event) => event,
            Err(err) => {
                log::error!("automation: creating a pipe event failed: {err}");
                return;
            }
        };
        let mut next: ClientId = 1;
        loop {
            if let Err(err) = pipe.connect(&event) {
                log::error!("automation: waiting for a pipe client failed: {err}");
                return;
            }
            let client = next;
            next += 1;
            *pipe.lock() = Some(client);
            log::info!("automation: pipe client {client} connected");
            clients.queue(client);
            let serving = tx.unbounded_send(Inbound::Connected(client)).is_ok()
                && read_client(pipe, &event, client, tx, clients);
            clients.hung_up(client);
            // Taking the lock waits out a reply being written to this client.
            *pipe.lock() = None;
            pipe.disconnect();
            log::info!("automation: pipe client {client} disconnected");
            if !serving {
                return;
            }
        }
    }

    /// Queue one client's lines until it hangs up. `false` once the command
    /// loop is gone (the app is quitting).
    fn read_client(
        pipe: &Pipe,
        event: &Event,
        client: ClientId,
        tx: &mpsc::UnboundedSender<Inbound>,
        clients: &Clients,
    ) -> bool {
        let send = |line: String| {
            clients.queue(client);
            tx.unbounded_send(Inbound::Line(client, line)).is_ok()
        };
        let mut lines = LineBuffer::default();
        let mut buf = vec![0u8; BUFFER as usize];
        loop {
            match pipe.read(event, &mut buf) {
                Ok(n) => match lines.push(&buf[..n as usize]) {
                    Ok(complete) => {
                        for line in complete {
                            if !send(line) {
                                return false;
                            }
                        }
                    }
                    Err(len) => {
                        log::error!(
                            "automation: pipe client {client} sent a {len}-byte line; disconnecting it"
                        );
                        return true;
                    }
                },
                Err(err) => {
                    if !last_error_is(&err, ERROR_BROKEN_PIPE) {
                        log::warn!("automation: reading from pipe client {client} failed: {err}");
                    }
                    break;
                }
            }
        }
        // Like stdin, a last line without a newline still counts.
        lines.finish().is_none_or(send)
    }

    fn write_loop(pipe: &Pipe, rx: std_mpsc::Receiver<Write>) {
        let event = match Event::new() {
            Ok(event) => event,
            Err(err) => {
                log::error!("automation: creating a pipe event failed: {err}");
                return;
            }
        };
        for write in rx {
            match write {
                Write::Line(client, bytes) => {
                    let current = pipe.lock();
                    if *current != Some(client) {
                        // That driver has gone; nobody will read this.
                        continue;
                    }
                    if let Err(err) = pipe.write_all(&event, &bytes) {
                        log::debug!("automation: writing to pipe client {client} failed: {err}");
                    }
                    drop(current);
                }
                Write::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;
        use std::fs::{File, OpenOptions};
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::time::Instant;

        fn unique_name(tag: &str) -> String {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            format!(
                "gpui-automation-test-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
        }

        /// Open the client end, retrying while the server is between clients.
        fn connect(path: &str) -> File {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match OpenOptions::new().read(true).write(true).open(path) {
                    Ok(file) => return file,
                    Err(err) if Instant::now() < deadline => {
                        assert!(
                            last_error_is(&err, ERROR_PIPE_BUSY)
                                || err.kind() == io::ErrorKind::NotFound,
                            "{err}"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("connecting to {path}: {err}"),
                }
            }
        }

        fn next(rx: &mut mpsc::UnboundedReceiver<Inbound>, clients: &Clients) -> Inbound {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(message) = rx.try_recv() {
                    clients.dequeued();
                    return message;
                }
                assert!(Instant::now() < deadline, "no message from the pipe");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn read_line(reader: &mut BufReader<File>) -> String {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            line
        }

        #[test]
        fn clients_connect_send_and_reconnect() {
            let name = unique_name("serve");
            let path = super::super::pipe_path(&name).unwrap();
            let clients = Arc::new(Clients::default());
            let (mut rx, output) = serve(&name, clients.clone()).unwrap();

            let mut first = connect(&path);
            assert_eq!(next(&mut rx, &clients), Inbound::Connected(1));
            first
                .write_all(b"{\"cmd\": \"state\"}\r\n\n{\"cmd\": \"win")
                .unwrap();
            first.write_all(b"dows\"}\n").unwrap();
            assert_eq!(
                next(&mut rx, &clients),
                Inbound::Line(1, r#"{"cmd": "state"}"#.into())
            );
            assert_eq!(
                next(&mut rx, &clients),
                Inbound::Line(1, r#"{"cmd": "windows"}"#.into())
            );
            output.send(1, &serde_json::json!({"id": 1, "ok": true}));
            let mut reader = BufReader::new(first.try_clone().unwrap());
            assert_eq!(read_line(&mut reader), "{\"id\":1,\"ok\":true}\n");

            // One client at a time.
            let busy = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap_err();
            assert!(last_error_is(&busy, ERROR_PIPE_BUSY), "{busy}");

            // A last line without a newline still arrives when the client leaves.
            first.write_all(b"{\"cmd\": \"quit\"}").unwrap();
            drop(reader);
            drop(first);
            assert_eq!(
                next(&mut rx, &clients),
                Inbound::Line(1, r#"{"cmd": "quit"}"#.into())
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while !clients.abandoned(1) {
                assert!(Instant::now() < deadline, "client 1 never hung up");
                std::thread::sleep(Duration::from_millis(5));
            }

            // Replies to a client that left are dropped, not sent to the next.
            output.send(1, &serde_json::json!({"id": "stale"}));
            let second = connect(&path);
            assert_eq!(next(&mut rx, &clients), Inbound::Connected(2));
            output.send(1, &serde_json::json!({"id": "stale"}));
            output.send(2, &serde_json::json!({"event": "ready"}));
            output.flush();
            let mut reader = BufReader::new(second);
            assert_eq!(read_line(&mut reader), "{\"event\":\"ready\"}\n");
            assert!(!clients.abandoned(2));
        }

        #[test]
        fn names_in_use_are_refused() {
            let name = unique_name("taken");
            let clients = Arc::new(Clients::default());
            let _first = serve(&name, clients.clone()).unwrap();
            let err = serve(&name, clients)
                .err()
                .expect("a second server must fail");
            assert!(err.to_string().contains("already in use"), "{err:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_paths_are_normalised_and_validated() {
        assert_eq!(pipe_path("slide").unwrap(), r"\\.\pipe\slide");
        assert_eq!(pipe_path(r"\\.\pipe\slide-2").unwrap(), r"\\.\pipe\slide-2");
        assert_eq!(pipe_path(r"\\.\PIPE\x").unwrap(), r"\\.\pipe\x");
        for bad in ["", r"\\.\pipe\", r"a\b", "tab\there"] {
            assert!(pipe_path(bad).is_err(), "{bad:?}");
        }
        assert!(pipe_path(&"x".repeat(300)).is_err());
    }
}
