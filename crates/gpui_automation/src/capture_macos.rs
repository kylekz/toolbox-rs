//! macOS window capture, through Core Graphics' `CGWindowListCreateImage` at
//! the display's own resolution (`BestResolution`: 2x on a Retina display, so
//! a 1440x900 window captures as 2880x1800), without the window's shadow.
//!
//! - [`capture_window`] (the `window` source) is the one window by its window
//!   number. The window server keeps every window's own backing store, so it
//!   works while other windows cover the window. A translucent window's
//!   own image holds nothing of what is behind it: translucent surfaces
//!   come out over black, as with Acrylic on Windows.
//! - [`capture_screen`] (the `screen` source) is the window's rectangle as the
//!   screen composes the window and everything under it (what shows through
//!   a translucent window included), any window above it left out.
//!
//! Both need the Screen Recording permission for the process that runs them
//! (over SSH, the session's). Core Graphics marks the call deprecated in
//! favour of ScreenCaptureKit, whose capture is asynchronous and block-based;
//! this one still works on macOS 27 and is a single synchronous call, so it
//! stays until it doesn't.
//!
//! The image is drawn into an sRGB bitmap with no alpha, so the PNG holds sRGB
//! colours (the display's own space, Display P3 on most Macs, converted).

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use core_graphics::base::{kCGBitmapByteOrder32Big, kCGImageAlphaNoneSkipLast};
use core_graphics::color_space::{CGColorSpace, kCGColorSpaceSRGB};
use core_graphics::context::CGContext;
use core_graphics::display::CGDisplay;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window::{
    CGWindowListOption, create_image, kCGWindowImageBestResolution,
    kCGWindowImageBoundsIgnoreFraming, kCGWindowListOptionIncludingWindow,
    kCGWindowListOptionOnScreenBelowWindow,
};
use gpui::Window;
use objc2::encode::{Encode, Encoding};
use objc2::msg_send;
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// AppKit's `NSPoint`, `NSSize` and `NSRect` (`CGPoint`, `CGSize`, `CGRect`), for reading
/// `-[NSWindow frame]` through `msg_send!`.
#[repr(C)]
#[derive(Clone, Copy)]
struct NsPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NsSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    origin: NsPoint,
    size: NsSize,
}

// SAFETY: each is `#[repr(C)]` with exactly the fields its encoding names, in that order.
unsafe impl Encode for NsPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: as above.
unsafe impl Encode for NsSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: as above.
unsafe impl Encode for NsRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[NsPoint::ENCODING, NsSize::ENCODING]);
}

/// `window`'s window number (its `CGWindowID`), its frame in Core Graphics' global coordinates
/// (points, origin at the main display's top-left, y down, whichever display the window is on)
/// and its backing scale.
/// AppKit's `frame` is in its own global coordinates (origin at the main display's bottom-left,
/// y up), so the frame is flipped against the main display's height. Read on the main thread
/// (it borrows the window).
pub(crate) fn window_target(window: &Window) -> Result<(u32, [f64; 4], f64)> {
    // SAFETY: `windowNumber`, `frame` and `backingScaleFactor` are plain NSWindow getters,
    // `frame` returning an NSRect.
    let (number, frame, scale): (isize, NsRect, f64) =
        with_ns_window(window, |ns_window| unsafe {
            (
                msg_send![ns_window, windowNumber],
                msg_send![ns_window, frame],
                msg_send![ns_window, backingScaleFactor],
            )
        })?;
    let number = u32::try_from(number)
        .with_context(|| format!("window number {number} isn't a CGWindowID"))?;
    let main_height = CGDisplay::main().bounds().size.height;
    Ok((number, cg_frame(frame, main_height), scale))
}

/// `window`'s smallest content size in points (`contentMinSize`; 0x0 when it has none).
pub(crate) fn content_min_size(window: &Window) -> Result<(f32, f32)> {
    // SAFETY: `contentMinSize` is a plain NSWindow getter returning an NSSize.
    let size: NsSize = with_ns_window(window, |ns_window| unsafe {
        msg_send![ns_window, contentMinSize]
    })?;
    Ok((size.width as f32, size.height as f32))
}

/**
 * Runs `f` with the NSWindow gpui draws `window` into. Call it on the main thread, where gpui
 * hands out its windows.
 */
pub(crate) fn with_ns_window<R>(window: &Window, f: impl FnOnce(&AnyObject) -> R) -> Result<R> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|err| anyhow::anyhow!("window has no native handle: {err}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        bail!("unexpected native window handle {:?}", handle.as_raw());
    };
    // SAFETY: `ns_view` is the live NSView gpui draws the window into (the window is borrowed,
    // on the main thread), and `window` is NSView's getter for the NSWindow holding it.
    let ns_window: *mut AnyObject = unsafe {
        let view: &AnyObject = handle.ns_view.cast::<AnyObject>().as_ref();
        msg_send![view, window]
    };
    // SAFETY: a non-null NSWindow, alive while its view is (the borrow of `window` above).
    let Some(ns_window) = (unsafe { ns_window.as_ref() }) else {
        bail!("the window's view is in no NSWindow");
    };
    Ok(f(ns_window))
}

/// An AppKit frame (origin bottom-left of the main display, y up) in Core Graphics' global
/// coordinates (origin top-left of the main display, y down), `[x, y, width, height]`.
fn cg_frame(frame: NsRect, main_height: f64) -> [f64; 4] {
    [
        frame.origin.x,
        main_height - (frame.origin.y + frame.size.height),
        frame.size.width,
        frame.size.height,
    ]
}

/// Tightly packed 8-bit RGBA pixels, alpha forced opaque.
pub(crate) struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/**
 * Capture the window whose window number (`CGWindowID`) is `window` once its image is its
 * `frame` at `scale`. A window that has just opened is still in AppKit's open animation, which
 * draws it scaled down and then with a few pixels of outline: a scenario's first screenshot came
 * out 1428x924 points, then 1442x934, for a 1440x932 frame. A full-size image that keeps an
 * outline (a dark window's can) is taken once `OUTLINE_SETTLE` has passed.
 */
pub(crate) fn capture_window(window: u32, frame: [f64; 4], scale: f64) -> Result<RgbaImage> {
    let [_, _, width, height] = frame;
    let (width, height) = ((width * scale).round(), (height * scale).round());
    let started = Instant::now();
    loop {
        let image = window_image(window)?;
        let (w, h) = (f64::from(image.width), f64::from(image.height));
        let waited = started.elapsed();
        if (w == width && h == height)
            || (w >= width && h >= height && waited >= OUTLINE_SETTLE)
            || waited >= OPEN_ANIMATION
        {
            return Ok(image);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Longer than AppKit's window open animation, which lasts a fraction of a second.
const OPEN_ANIMATION: Duration = Duration::from_secs(2);
/// How long a full-size image with an outline gets to lose it.
const OUTLINE_SETTLE: Duration = Duration::from_millis(750);

fn window_image(window: u32) -> Result<RgbaImage> {
    // `CGRectNull`: the window's own bounds.
    let null = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0., 0.),
    );
    to_rgba(image(null, kCGWindowListOptionIncludingWindow, window)?)
}

/// Capture `frame` (points, Core Graphics' global top-left coordinates: the
/// window `window`'s frame) as the screen shows the window and what is under it.
pub(crate) fn capture_screen(window: u32, frame: [f64; 4]) -> Result<RgbaImage> {
    let [x, y, w, h] = frame;
    let rect = CGRect::new(&CGPoint::new(x, y), &CGSize::new(w, h));
    to_rgba(image(
        rect,
        kCGWindowListOptionOnScreenBelowWindow | kCGWindowListOptionIncludingWindow,
        window,
    )?)
}

fn image(rect: CGRect, list: CGWindowListOption, window: u32) -> Result<CGImage> {
    let Some(image) = create_image(
        rect,
        list,
        window,
        kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution,
    ) else {
        bail!(
            "CGWindowListCreateImage returned nothing for window {window}: is it on screen, and \
             does this process (or the SSH session) have the Screen Recording permission?"
        );
    };
    Ok(image)
}

/// `image` drawn over black into sRGB RGBA.
fn to_rgba(image: CGImage) -> Result<RgbaImage> {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        bail!("the window has an empty image ({width}x{height})");
    }
    // SAFETY: a constant CFString Core Graphics exports, read once.
    let srgb = unsafe { kCGColorSpaceSRGB };
    let space = match CGColorSpace::create_with_name(srgb) {
        Some(space) => space,
        None => CGColorSpace::create_device_rgb(),
    };
    let mut context = CGContext::create_bitmap_context(
        None,
        width,
        height,
        8,
        width * 4,
        &space,
        kCGImageAlphaNoneSkipLast | kCGBitmapByteOrder32Big,
    );
    context.draw_image(
        CGRect::new(
            &CGPoint::new(0., 0.),
            &CGSize::new(width as f64, height as f64),
        ),
        &image,
    );
    let stride = context.bytes_per_row();
    let mut rgba = Vec::with_capacity(width * height * 4);
    // A bitmap context's rows run top to bottom in memory.
    for row in context.data().chunks_exact(stride).take(height) {
        for &[red, green, blue, _] in row[..width * 4].as_chunks::<4>().0 {
            rgba.extend_from_slice(&[red, green, blue, 0xff]);
        }
    }
    Ok(RgbaImage {
        width: u32::try_from(width)?,
        height: u32::try_from(height)?,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NsRect {
        NsRect {
            origin: NsPoint { x, y },
            size: NsSize { width, height },
        }
    }

    #[test]
    fn appkit_frames_flip_against_the_main_displays_height() {
        // A 1440x900 window 100 from the left and 200 below the top of a 1440-high main display.
        assert_eq!(
            cg_frame(rect(100., 340., 1440., 900.), 1440.),
            [100., 200., 1440., 900.]
        );
        // On a display above the main one (AppKit y beyond its height): a negative y.
        assert_eq!(
            cg_frame(rect(0., 1500., 800., 600.), 1440.),
            [0., -660., 800., 600.]
        );
        // On a display to the left: a negative x, kept.
        assert_eq!(
            cg_frame(rect(-1920., 0., 800., 600.), 1440.),
            [-1920., 840., 800., 600.]
        );
    }
}
