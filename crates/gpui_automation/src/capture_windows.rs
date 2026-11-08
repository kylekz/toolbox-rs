//! Win32 window capture.
//!
//! GPUI presents through DirectComposition, so a plain `BitBlt` of the
//! window DC sees nothing: `PrintWindow(PW_RENDERFULLCONTENT)` asks DWM for
//! the composed frame instead, which also works while the window is covered
//! by other windows. If that comes back empty (remote desktop, odd drivers),
//! fall back to copying the window's client rect from the screen.
//!
//! The composed frame includes a DWM system backdrop (Mica, measured equal to
//! the screen pixel for pixel, active or inactive) but not the blur-behind
//! accent gpui uses for `WindowBackgroundAppearance::Blurred` (Acrylic):
//! translucent areas over it come out composited over black.
//! [`CaptureSource::Screen`] copies the client rect from the desktop
//! instead, blur and all, after checking that the window is in front so the
//! copy can't show another window.
//!
//! The HWND comes from the gpui window's raw-window-handle, never from
//! "the first visible window of this process", so multi-window apps capture
//! the window that was asked for.
//!
//! Sizes are physical pixels. The capture thread switches itself to
//! per-monitor-v2 DPI awareness for the duration so client rects are never
//! DPI-virtualised, whatever the process manifest says; on the 150% dev
//! display a 1440x900 logical window captures as 2160x1350.

use std::ffi::c_void;

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACKNESS, BitBlt, ClientToScreen,
    CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    GetDIBits, HBITMAP, HDC, HGDIOBJ, PatBlt, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClientRect, GetForegroundWindow, IsIconic, IsWindow,
    PW_RENDERFULLCONTENT, WindowFromPoint,
};

use crate::capture::CaptureSource;

/// Tightly packed 8-bit RGBA pixels, alpha forced opaque.
pub(crate) struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Capture `hwnd`'s client area from `source`.
pub(crate) fn capture_client_area(hwnd: isize, source: CaptureSource) -> Result<RgbaImage> {
    let hwnd = hwnd as HWND;
    // SAFETY: `IsWindow` accepts any value and only reports whether it names
    // a live window.
    if unsafe { IsWindow(hwnd) } == 0 {
        bail!("the window no longer exists");
    }
    let _dpi = ThreadDpiAwareness::per_monitor_v2();

    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window (checked above); `rect` is a valid
    // out-pointer to a local.
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        bail!("GetClientRect failed");
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        bail!("the window has an empty client area ({w}x{h}); is it minimised?");
    }

    // SAFETY: `hwnd` is a live window.
    let window_dc = unsafe { GetDC(hwnd) };
    if window_dc.is_null() {
        bail!("GetDC(hwnd) failed");
    }
    let window_dc = WindowDc {
        hwnd,
        dc: window_dc,
    };

    // SAFETY: `window_dc.dc` is a valid, just-obtained DC.
    let mem_dc = unsafe { CreateCompatibleDC(window_dc.dc) };
    if mem_dc.is_null() {
        bail!("CreateCompatibleDC failed");
    }
    let mem_dc = MemDc(mem_dc);

    // Compatible with the *window* DC: a bitmap compatible with the fresh
    // memory DC would be 1-bit monochrome.
    // SAFETY: `window_dc.dc` is valid and `w`/`h` are positive.
    let bitmap = unsafe { CreateCompatibleBitmap(window_dc.dc, w, h) };
    if bitmap.is_null() {
        bail!("CreateCompatibleBitmap failed");
    }
    let bitmap = Bitmap(bitmap);

    let mut pixels = match source {
        CaptureSource::Window => match print_window(hwnd, &mem_dc, &bitmap, w, h)? {
            Some(pixels) => pixels,
            None => {
                log::warn!(
                    "automation: PrintWindow returned no content, copying from the screen instead"
                );
                copy_from_screen(hwnd, &mem_dc, &bitmap, w, h)?
            }
        },
        CaptureSource::Screen => {
            ensure_in_front(hwnd, w, h)?;
            copy_from_screen(hwnd, &mem_dc, &bitmap, w, h)?
        }
    };

    // BGRA -> RGBA. Alpha from a composed frame means nothing once it is
    // flattened into a screenshot, so force it opaque.
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    Ok(RgbaImage {
        width: w as u32,
        height: h as u32,
        rgba: pixels,
    })
}

/// The window's composed frame through `PrintWindow`, or `None` when it
/// rendered nothing.
fn print_window(
    hwnd: HWND,
    mem_dc: &MemDc,
    bitmap: &Bitmap,
    w: i32,
    h: i32,
) -> Result<Option<Vec<u8>>> {
    let printed = {
        let _selected = Selection::new(mem_dc.0, bitmap.0);
        // A fresh bitmap holds undefined pixels; clear it so a PrintWindow
        // that "succeeds" without rendering is caught by the blank check.
        // SAFETY: `bitmap` is selected into `mem_dc`, sized `w`x`h`.
        if unsafe { PatBlt(mem_dc.0, 0, 0, w, h, BLACKNESS) } == 0 {
            bail!("PatBlt failed clearing the capture bitmap");
        }
        // SAFETY: `hwnd` is live; `mem_dc` has a client-sized bitmap selected.
        unsafe { PrintWindow(hwnd, mem_dc.0, PW_CLIENTONLY | PW_RENDERFULLCONTENT) }
    }; // deselected here: GetDIBits requires the bitmap not be selected
    if printed == 0 {
        return Ok(None);
    }
    let pixels = read_bgra(mem_dc.0, bitmap.0, w, h)?;
    Ok((!is_blank(&pixels)).then_some(pixels))
}

/// The window's client rect as the screen shows it, whatever is on top.
fn copy_from_screen(
    hwnd: HWND,
    mem_dc: &MemDc,
    bitmap: &Bitmap,
    w: i32,
    h: i32,
) -> Result<Vec<u8>> {
    let origin = client_to_screen(hwnd, 0, 0)?;
    // SAFETY: a null window asks for the whole-screen DC, a documented use.
    let screen_dc = unsafe { GetDC(std::ptr::null_mut()) };
    if screen_dc.is_null() {
        bail!("GetDC(NULL) failed");
    }
    let screen_dc = WindowDc {
        hwnd: std::ptr::null_mut(),
        dc: screen_dc,
    };
    {
        let _selected = Selection::new(mem_dc.0, bitmap.0);
        // SAFETY: both DCs are valid and the bitmap is `w`x`h`.
        let copied = unsafe {
            BitBlt(
                mem_dc.0,
                0,
                0,
                w,
                h,
                screen_dc.dc,
                origin.x,
                origin.y,
                SRCCOPY,
            )
        };
        if copied == 0 {
            bail!("BitBlt from the screen failed");
        }
    }
    read_bgra(mem_dc.0, bitmap.0, w, h)
}

/// Fails unless the screen at the window's client area shows the window: it
/// must be the foreground window, not minimised, and the window under a 3x3
/// grid of points across the client area must be it.
fn ensure_in_front(hwnd: HWND, w: i32, h: i32) -> Result<()> {
    // SAFETY: `hwnd` is live; the calls only query window state.
    let (root, foreground, iconic) = unsafe {
        (
            GetAncestor(hwnd, GA_ROOT),
            GetAncestor(GetForegroundWindow(), GA_ROOT),
            IsIconic(hwnd) != 0,
        )
    };
    if iconic {
        bail!("a screen capture needs the window on screen, and it is minimised");
    }
    if foreground != root {
        bail!(
            "a screen capture needs the window in front, but another window has the foreground \
             (the person at the machine switched windows, or Windows kept the app from taking \
             the foreground); retry, or use the default window source"
        );
    }
    for fy in [1, 3, 5] {
        for fx in [1, 3, 5] {
            let point = client_to_screen(hwnd, w * fx / 6, h * fy / 6)?;
            // SAFETY: `WindowFromPoint` takes any point and returns the
            // window there or null; `GetAncestor` accepts null.
            let under = unsafe { WindowFromPoint(point) };
            if under.is_null() {
                bail!(
                    "a screen capture needs the whole window on screen, but its client area runs \
                     off the screen at ({}, {})",
                    point.x,
                    point.y
                );
            }
            // SAFETY: as above.
            let under = unsafe { GetAncestor(under, GA_ROOT) };
            if under != root {
                bail!(
                    "a screen capture needs the window uncovered, but another window is in front \
                     of it at screen point ({}, {}); retry, or use the default window source",
                    point.x,
                    point.y
                );
            }
        }
    }
    Ok(())
}

/// A point in `hwnd`'s client area, in physical screen coordinates.
fn client_to_screen(hwnd: HWND, x: i32, y: i32) -> Result<POINT> {
    let mut point = POINT { x, y };
    // SAFETY: `hwnd` is live; `point` is a valid in/out pointer.
    if unsafe { ClientToScreen(hwnd, &mut point) } == 0 {
        bail!("ClientToScreen failed");
    }
    Ok(point)
}

/// `true` when every colour channel is zero: nothing was rendered. Alpha is
/// ignored because PrintWindow's alpha is unreliable.
fn is_blank(bgra: &[u8]) -> bool {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .all(|[b, g, r, _]| *b == 0 && *g == 0 && *r == 0)
}

/// Read `w`x`h` top-down 32-bit BGRA pixels. `bitmap` must not be selected
/// into a DC (a documented `GetDIBits` requirement).
fn read_bgra(dc: HDC, bitmap: HBITMAP, w: i32, h: i32) -> Result<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // negative: top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    // SAFETY: `dc`/`bitmap` are valid GDI handles; `buf` holds exactly
    // `w * h` 32-bit pixels as described by `info`.
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        bail!("GetDIBits returned no scan lines");
    }
    Ok(buf)
}

/// Sets the calling thread's DPI awareness and restores it on drop.
struct ThreadDpiAwareness(DPI_AWARENESS_CONTEXT);

impl ThreadDpiAwareness {
    fn per_monitor_v2() -> Self {
        // SAFETY: the constant is a valid awareness context; the call only
        // affects this thread and returns the previous context (null on
        // failure, which `drop` then leaves alone).
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for ThreadDpiAwareness {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: restores the context this thread had before.
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

/// Releases a DC from `GetDC` on drop.
struct WindowDc {
    hwnd: HWND,
    dc: HDC,
}

impl Drop for WindowDc {
    fn drop(&mut self) {
        // SAFETY: `dc` came from `GetDC(self.hwnd)` and is released once.
        unsafe {
            ReleaseDC(self.hwnd, self.dc);
        }
    }
}

/// Deletes a memory DC from `CreateCompatibleDC` on drop.
struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: owned solely by this guard.
        unsafe {
            DeleteDC(self.0);
        }
    }
}

/// Deletes a bitmap from `CreateCompatibleBitmap` on drop. `Selection`
/// guards are always dropped first, so the bitmap is never still selected
/// (deleting a selected bitmap fails and leaks).
struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: owned solely by this guard and no longer selected.
        unsafe {
            DeleteObject(self.0);
        }
    }
}

/// `SelectObject` that restores the previous object on every exit path.
struct Selection {
    dc: HDC,
    old: HGDIOBJ,
}

impl Selection {
    fn new(dc: HDC, object: HGDIOBJ) -> Self {
        // SAFETY: callers pass a valid DC and a compatible GDI object.
        let old = unsafe { SelectObject(dc, object) };
        Self { dc, old }
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        // SAFETY: `old` is what `SelectObject` reported for this live DC.
        unsafe {
            SelectObject(self.dc, self.old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_detection_ignores_alpha() {
        assert!(is_blank(&[0, 0, 0, 255, 0, 0, 0, 0]));
        assert!(!is_blank(&[0, 0, 1, 0]));
    }

    #[test]
    fn dead_windows_are_reported() {
        let err = capture_client_area(0, CaptureSource::Window)
            .err()
            .expect("null hwnd must fail");
        assert!(err.to_string().contains("no longer exists"), "{err}");
    }
}
