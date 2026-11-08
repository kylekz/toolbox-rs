//! Win32 half of [`crate::background`]: push a window behind every other
//! window without activating it, keep it out of the taskbar and Alt+Tab, and
//! stop both Windows and a click from ever activating it.

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, SetLastError, WPARAM};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, HWND_BOTTOM, IsWindow, MA_NOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowLongPtrW,
    SetWindowPos, WINDOW_LONG_PTR_INDEX, WM_MOUSEACTIVATE, WM_NCDESTROY, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_SYSMENU,
};

/// The extended style of a background window: a tool window (no taskbar
/// button, not in Alt+Tab) rather than an app window, and one Windows never
/// activates on its own. Without `WS_EX_NOACTIVATE`, Windows may hand the
/// window the foreground when the person minimises or closes their own
/// foreground window.
fn background_ex_style(ex_style: isize) -> isize {
    (ex_style & !(WS_EX_APPWINDOW as isize)) | (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize
}

/// The style of a background window: without a system menu. DWM draws a tool
/// window's small caption close glyph at the top-right of its frame, and a
/// window with a translucent backdrop (Mica, Acrylic) that draws its own title
/// bar shows that glyph through it, in screenshots too. The glyph exists only
/// with `WS_SYSMENU`, and a background window never takes the keyboard focus,
/// so it has no use for the system menu either.
fn background_style(style: isize) -> isize {
    style & !(WS_SYSMENU as isize)
}

/// Identifies [`no_activate_on_click`] among the window's subclasses.
const NO_ACTIVATE_SUBCLASS: usize = 1;

/// Answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE`, so a click on the window
/// doesn't activate it. `WS_EX_NOACTIVATE` alone doesn't stop that: gpui's
/// window procedure answers `MA_ACTIVATE` itself instead of leaving the
/// message to `DefWindowProc`. The click itself still reaches gpui.
unsafe extern "system" fn no_activate_on_click(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        // SAFETY: the window's last message, on its own thread; a subclass
        // must be removed before its window goes away.
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(no_activate_on_click), id);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        },
        // SAFETY: forwards a message this subclass received, unchanged.
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// Make `hwnd` a tool window that can't be activated and move it to the
/// bottom of the z-order, all without activating it. Its position, size and
/// visibility are unchanged. Must run on the thread that created the window,
/// which subclassing requires.
///
/// `SetWindowPos` sends `WM_WINDOWPOSCHANGING`/`CHANGED` and, for the frame
/// change, `WM_NCCALCSIZE` synchronously; none of them resizes or moves the
/// window, so gpui's window procedure never needs the app for them, and this
/// may run while gpui is building the window.
pub(crate) fn send_to_back(hwnd: isize) -> Result<()> {
    let hwnd = hwnd as HWND;
    // SAFETY: `IsWindow` accepts any value.
    if unsafe { IsWindow(hwnd) } == 0 {
        bail!("the window no longer exists");
    }
    update_style(hwnd, GWL_EXSTYLE, background_ex_style)
        .map_err(|err| anyhow::anyhow!("setting the tool-window style failed: {err}"))?;
    update_style(hwnd, GWL_STYLE, background_style)
        .map_err(|err| anyhow::anyhow!("removing the system menu failed: {err}"))?;
    // SAFETY: a live window; the flags keep its position, size and
    // activation, change only its z-order and re-read the frame style.
    let ok = unsafe {
        SetWindowPos(
            hwnd,
            HWND_BOTTOM,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        )
    };
    if ok == 0 {
        bail!("SetWindowPos failed: {}", std::io::Error::last_os_error());
    }
    // For a window sent back twice this only updates the subclass's data.
    // SAFETY: a live window of this thread; the procedure is a plain function
    // that removes itself on WM_NCDESTROY.
    let ok =
        unsafe { SetWindowSubclass(hwnd, Some(no_activate_on_click), NO_ACTIVATE_SUBCLASS, 0) };
    if ok == 0 {
        bail!("subclassing the window failed");
    }
    Ok(())
}

/// Replaces the window's `index` style (`GWL_STYLE` or `GWL_EXSTYLE`) with
/// `change` applied to it. The frame re-reads styles on the next
/// `SetWindowPos(SWP_FRAMECHANGED)`.
fn update_style(
    hwnd: HWND,
    index: WINDOW_LONG_PTR_INDEX,
    change: fn(isize) -> isize,
) -> std::io::Result<()> {
    // SAFETY: a live window; both indices are valid for any window.
    let style = unsafe { GetWindowLongPtrW(hwnd, index) };
    // SetWindowLongPtrW returns the previous value, which may legitimately be
    // zero, so failure is only visible through the last error.
    // SAFETY: plain thread-local error bookkeeping, then a style update on a
    // live window this thread created.
    let failed = unsafe {
        SetLastError(0);
        SetWindowLongPtrW(hwnd, index, change(style)) == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(0)
    };
    if failed {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GW_HWNDNEXT, GetWindow, HTCLIENT, MA_ACTIVATE,
        SendMessageW, WM_LBUTTONDOWN, WS_OVERLAPPEDWINDOW,
    };

    #[test]
    fn background_windows_are_tool_windows() {
        let app_window = WS_EX_APPWINDOW as isize | 0x100;
        let style = background_ex_style(app_window);
        assert_eq!(style & WS_EX_APPWINDOW as isize, 0);
        assert_ne!(style & WS_EX_TOOLWINDOW as isize, 0);
        assert_ne!(style & WS_EX_NOACTIVATE as isize, 0);
        assert_ne!(style & 0x100, 0, "other bits are kept");
    }

    #[test]
    fn background_windows_have_no_system_menu() {
        let style = background_style(WS_OVERLAPPEDWINDOW as isize);
        assert_eq!(style & WS_SYSMENU as isize, 0);
        assert_eq!(
            style,
            (WS_OVERLAPPEDWINDOW & !WS_SYSMENU) as isize,
            "other bits are kept"
        );
    }

    #[test]
    fn dead_windows_are_reported() {
        let err = send_to_back(0).unwrap_err();
        assert!(err.to_string().contains("no longer exists"), "{err}");
    }

    /// A hidden top-level window of the system "STATIC" class, destroyed on drop.
    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: a system class, no parent, never shown.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_APPWINDOW,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    200,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "CreateWindowExW failed");
            Self(hwnd)
        }

        /// Whether `other` comes after this window in the z-order.
        fn is_above(&self, other: &TestWindow) -> bool {
            let mut next = self.0;
            loop {
                // SAFETY: walking the z-order from a live window; null ends it.
                next = unsafe { GetWindow(next, GW_HWNDNEXT) };
                if next.is_null() {
                    return false;
                }
                if next == other.0 {
                    return true;
                }
            }
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: created by this test on this thread.
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn windows_move_behind_the_others_as_tool_windows() {
        let first = TestWindow::new();
        // Newer windows start above older ones.
        let second = TestWindow::new();
        assert!(second.is_above(&first));
        send_to_back(second.0 as isize).unwrap();
        assert!(first.is_above(&second), "moved behind the older window");
        // SAFETY: a live window.
        let ex_style = unsafe { GetWindowLongPtrW(second.0, GWL_EXSTYLE) };
        assert_ne!(ex_style & WS_EX_TOOLWINDOW as isize, 0);
        assert_ne!(ex_style & WS_EX_NOACTIVATE as isize, 0);
        assert_eq!(ex_style & WS_EX_APPWINDOW as isize, 0);
        // SAFETY: a live window.
        let style = unsafe { GetWindowLongPtrW(second.0, GWL_STYLE) };
        assert_eq!(style & WS_SYSMENU as isize, 0);
    }

    /// Stands in for gpui's window procedure, which answers `MA_ACTIVATE`
    /// itself rather than leaving `WM_MOUSEACTIVATE` to `DefWindowProc`.
    unsafe extern "system" fn activates_on_click(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        match msg {
            WM_MOUSEACTIVATE => MA_ACTIVATE as LRESULT,
            // SAFETY: forwards a message this subclass received, unchanged.
            _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
        }
    }

    const ACTIVATES_ON_CLICK: usize = 7;

    /// What the window answers when the left button goes down in its client
    /// area.
    fn click_activation(window: &TestWindow) -> u32 {
        let lparam = ((WM_LBUTTONDOWN << 16) | HTCLIENT) as LPARAM;
        // SAFETY: a live window of this thread; WM_MOUSEACTIVATE carries only
        // the top-level window and the hit test.
        unsafe { SendMessageW(window.0, WM_MOUSEACTIVATE, window.0 as WPARAM, lparam) as u32 }
    }

    #[test]
    fn clicks_do_not_activate_background_windows() {
        let window = TestWindow::new();
        // SAFETY: a live window of this thread and a plain function.
        let ok =
            unsafe { SetWindowSubclass(window.0, Some(activates_on_click), ACTIVATES_ON_CLICK, 0) };
        assert_ne!(ok, 0, "SetWindowSubclass failed");
        assert_eq!(click_activation(&window), MA_ACTIVATE);

        send_to_back(window.0 as isize).unwrap();
        assert_eq!(click_activation(&window), MA_NOACTIVATE);
        send_to_back(window.0 as isize).unwrap();
        assert_eq!(click_activation(&window), MA_NOACTIVATE, "sent back twice");

        // SAFETY: removes the subclass installed above before the window is
        // destroyed (the one `send_to_back` installed removes itself).
        unsafe { RemoveWindowSubclass(window.0, Some(activates_on_click), ACTIVATES_ON_CLICK) };
    }
}
