//! `window_control` on Windows: what the window answers to `WM_NCHITTEST` where the pointer is.
//!
//! An app that hides the OS title bar draws its own and marks its parts with gpui's
//! `WindowControlArea`: the drag strip and the caption buttons. Windows asks the window what is
//! under the pointer with `WM_NCHITTEST`, and gpui answers from the hitboxes under the last mouse
//! position, so the driver first moves its pointer there, then sends the message itself.

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, IsWindow, SendMessageW, WM_NCHITTEST,
};

/**
 * The window part at `(x, y)`, physical pixels in `hwnd`'s client area: `caption` (the drag
 * region), `min`, `max`, `close`, `client`, or `ht <code>` for anything else (an edge).
 *
 * Must not run inside a gpui window update: the window procedure calls back into the app.
 */
pub(crate) fn hit_test(hwnd: isize, x: i32, y: i32) -> Result<String> {
    let hwnd = hwnd as HWND;
    // SAFETY: `IsWindow` accepts any value.
    if unsafe { IsWindow(hwnd) } == 0 {
        bail!("the window no longer exists");
    }
    let mut point = POINT { x, y };
    // SAFETY: a live window and a valid in/out pointer to a local.
    if unsafe { ClientToScreen(hwnd, &mut point) } == 0 {
        bail!("ClientToScreen failed");
    }
    /* WM_NCHITTEST's lParam packs the screen point's x and y as signed 16-bit words. */
    let lparam = ((point.y as u16 as u32) << 16 | point.x as u16 as u32) as LPARAM;
    // SAFETY: a live window; WM_NCHITTEST takes no pointers.
    let code = unsafe { SendMessageW(hwnd, WM_NCHITTEST, 0, lparam) } as u32;
    Ok(match code {
        HTCAPTION => "caption".to_owned(),
        HTMINBUTTON => "min".to_owned(),
        HTMAXBUTTON => "max".to_owned(),
        HTCLOSE => "close".to_owned(),
        HTCLIENT => "client".to_owned(),
        other => format!("ht {other}"),
    })
}

/**
 * Sends `hwnd` the `WM_DPICHANGED` Windows sends when the window's display changes scale (a
 * move to another display, a new scale in Settings), at `dpi`, with the rectangle Windows
 * would suggest: the window's own, scaled from `current` (the DPI the window last took, not
 * the system's, which this message doesn't change) about its top-left corner.
 *
 * Must not run inside a gpui window update: gpui's handler resizes the window, which sends
 * `WM_SIZE` back into the app.
 */
pub(crate) fn dpi_changed(hwnd: isize, current: u32, dpi: u32) -> Result<()> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, WM_DPICHANGED};

    let hwnd = hwnd as HWND;
    // SAFETY: `IsWindow` accepts any value.
    if unsafe { IsWindow(hwnd) } == 0 {
        bail!("the window no longer exists");
    }
    let mut rect = RECT::default();
    // SAFETY: a live window and a valid out-pointer to a local.
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        bail!("GetWindowRect failed");
    }
    let scale =
        |length: i32| (i64::from(length) * i64::from(dpi) / i64::from(current.max(1))) as i32;
    let mut suggested = RECT {
        left: rect.left,
        top: rect.top,
        right: rect.left + scale(rect.right - rect.left),
        bottom: rect.top + scale(rect.bottom - rect.top),
    };
    /* wParam carries the new DPI twice (x in the low word, y in the high); lParam the RECT. */
    let wparam = (dpi << 16 | dpi) as usize;
    // SAFETY: a live window; `suggested` outlives the synchronous call.
    unsafe {
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            wparam,
            &mut suggested as *mut RECT as LPARAM,
        )
    };
    Ok(())
}
