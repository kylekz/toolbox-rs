//! Watching the foreground window during background runs (tests/e2e/README.md
//! "Background runs"): a background app must never take the foreground from
//! whatever the person at the machine is using.
//!
//! On Windows a thread hooks `EVENT_SYSTEM_FOREGROUND` for the whole desktop
//! and records the owning process of every window that becomes the
//! foreground, so even a brief activation between two steps is caught. A
//! scenario fails when one of its app processes owned the foreground; the
//! person switching between their own windows is only noted in the
//! transcript. Elsewhere the watch records nothing.

use std::fmt;

/// A top-level window as the transcript and failure messages describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
}

impl fmt::Display for WindowInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.hwnd == 0 {
            return f.write_str("no window");
        }
        write!(
            f,
            "{:?} (hwnd {:#x}, pid {})",
            self.title, self.hwnd, self.pid
        )
    }
}

/// A position in the watch's event log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mark(usize);

pub use platform::Watch;

#[cfg(windows)]
mod platform {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::JoinHandle;

    use anyhow::{Context as _, Result, anyhow};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW,
        GetWindowTextW, GetWindowThreadProcessId, MSG, PostThreadMessageW, WINEVENT_OUTOFCONTEXT,
        WM_QUIT,
    };

    use super::{Mark, WindowInfo};

    /// Foreground changes recorded by the hook, in order.
    type Log = Arc<Mutex<Vec<WindowInfo>>>;

    /// Where the hook callback, which gets no context pointer, appends. Only
    /// one watch runs per process.
    static LOG: OnceLock<Log> = OnceLock::new();

    pub struct Watch {
        log: Log,
        thread: Option<(u32, JoinHandle<()>)>,
    }

    impl Watch {
        /// Start watching. Failing to install the hook degrades to polling
        /// the foreground window at each check.
        pub fn start() -> Self {
            let log = LOG.get_or_init(Log::default).clone();
            let thread = match spawn_hook_thread() {
                Ok(thread) => Some(thread),
                Err(err) => {
                    eprintln!("e2e: not watching foreground changes ({err:#}); polling instead");
                    None
                }
            };
            Self { log, thread }
        }

        pub fn mark(&self) -> Mark {
            Mark(self.lock().len())
        }

        /// The first window since `mark` that belonged to one of `pids`, plus
        /// every other foreground window seen since then. Without the hook,
        /// only the current foreground window is checked.
        pub fn since(&self, mark: Mark, pids: &[u32]) -> (Option<WindowInfo>, Vec<WindowInfo>) {
            if self.thread.is_none() {
                let now = current();
                return (pids.contains(&now.pid).then_some(now), Vec::new());
            }
            let events: Vec<WindowInfo> = {
                let log = self.lock();
                log[mark.0.min(log.len())..].to_vec()
            };
            let ours = events
                .iter()
                .find(|window| pids.contains(&window.pid))
                .cloned();
            let others = events
                .into_iter()
                .filter(|window| !pids.contains(&window.pid))
                .collect();
            (ours, others)
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, Vec<WindowInfo>> {
            // A panic while holding the lock leaves plain data behind.
            self.log
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            if let Some((thread_id, handle)) = self.thread.take() {
                // SAFETY: posting to a thread id this watch started; a thread
                // that already exited just makes the post fail.
                unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
                let _ = handle.join();
            }
        }
    }

    /// The current foreground window.
    pub fn current() -> WindowInfo {
        // SAFETY: no preconditions.
        describe(unsafe { GetForegroundWindow() })
    }

    fn describe(hwnd: HWND) -> WindowInfo {
        if hwnd.is_null() {
            return WindowInfo {
                hwnd: 0,
                pid: 0,
                title: String::new(),
            };
        }
        let mut pid = 0u32;
        let mut title = [0u16; 256];
        // SAFETY: both calls accept any window handle (a stale one yields 0)
        // and write within the given buffers.
        let len = unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32)
        };
        WindowInfo {
            hwnd: hwnd as isize,
            pid,
            title: String::from_utf16_lossy(&title[..len.max(0) as usize]),
        }
    }

    unsafe extern "system" fn on_foreground(
        _hook: HWINEVENTHOOK,
        _event: u32,
        hwnd: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if let Some(log) = LOG.get() {
            let window = describe(hwnd);
            log.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(window);
        }
    }

    /// A thread that owns the hook and pumps messages, which out-of-context
    /// hooks need to be called.
    fn spawn_hook_thread() -> Result<(u32, JoinHandle<()>)> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let handle = std::thread::Builder::new()
            .name("e2e-foreground".into())
            .spawn(move || {
                // SAFETY: a process-wide out-of-context hook with a callback
                // that only touches a mutex; unhooked before the thread ends.
                let hook = unsafe {
                    SetWinEventHook(
                        EVENT_SYSTEM_FOREGROUND,
                        EVENT_SYSTEM_FOREGROUND,
                        std::ptr::null_mut(),
                        Some(on_foreground),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                if hook.is_null() {
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                    return;
                }
                // SAFETY: no preconditions.
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let mut msg = MSG::default();
                // SAFETY: a valid out-pointer; GetMessageW returns 0 on
                // WM_QUIT and -1 on error, both of which end the loop.
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    // SAFETY: a message just retrieved on this thread.
                    unsafe { DispatchMessageW(&msg) };
                }
                // SAFETY: the hook installed above.
                unsafe { UnhookWinEvent(hook) };
            })
            .context("spawning the foreground watch")?;
        let thread_id = ready_rx
            .recv()
            .context("the foreground watch thread died")?
            .map_err(|err| anyhow!("SetWinEventHook failed: {err}"))?;
        Ok((thread_id, handle))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Mark, WindowInfo};

    /// Nothing to watch: other platforms have no foreground-stealing
    /// activation path in gpui to guard against.
    pub struct Watch;

    impl Watch {
        pub fn start() -> Self {
            Self
        }

        pub fn mark(&self) -> Mark {
            Mark::default()
        }

        pub fn since(&self, _mark: Mark, _pids: &[u32]) -> (Option<WindowInfo>, Vec<WindowInfo>) {
            (None, Vec::new())
        }
    }

    pub fn current() -> WindowInfo {
        WindowInfo {
            hwnd: 0,
            pid: 0,
            title: String::new(),
        }
    }
}

pub use platform::current;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_describe_themselves() {
        let window = WindowInfo {
            hwnd: 0x1234,
            pid: 42,
            title: "Slide".into(),
        };
        assert_eq!(window.to_string(), "\"Slide\" (hwnd 0x1234, pid 42)");
        let none = WindowInfo {
            hwnd: 0,
            pid: 0,
            title: String::new(),
        };
        assert_eq!(none.to_string(), "no window");
    }

    #[test]
    fn a_watch_attributes_nothing_to_an_unknown_process() {
        let watch = Watch::start();
        let mark = watch.mark();
        let (ours, _) = watch.since(mark, &[u32::MAX]);
        assert_eq!(ours, None);
    }
}
