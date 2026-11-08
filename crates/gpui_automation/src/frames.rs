//! Waiting on the window's real frame loop instead of sleeping.
//!
//! gpui runs `on_next_frame` callbacks at the *start* of a frame, before that
//! frame draws. So one hop only proves a frame began; a callback registered
//! from inside that first callback runs at the start of the following frame,
//! by which time the first frame has drawn and presented. Two nested hops
//! ("a drawn frame") therefore guarantee that every change made before the
//! wait is on screen and in the hit-test tree, which is what fixes clicks
//! landing on a stale frame.

use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use gpui::{AnyWindowHandle, App, AppContext as _, AsyncApp, FutureExt as _, Window};

/// Longest wait for a single frame before assuming the window will not draw
/// (minimised, or on a display that stopped vsyncing).
pub(crate) const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `f` at the start of the next frame of `window` and return its result.
pub(crate) async fn at_next_frame<T: 'static>(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App) -> T + 'static,
) -> Result<T> {
    let (tx, rx) = oneshot::channel();
    cx.update_window(window, |_, window, _| {
        window.on_next_frame(move |window, cx| {
            tx.send(f(window, cx)).ok();
        })
    })?;
    receive(rx, cx).await
}

/// Wait until `window` has drawn and presented a frame that started after
/// this call. With `refresh`, force that frame to redraw everything (and
/// bypass cached views) even if nothing was invalidated.
pub(crate) async fn drawn_frame(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    refresh: bool,
) -> Result<()> {
    between_frames(window, cx, refresh, |_, _| (), |_, _| ()).await
}

/// Run `first` at the start of the next frame and `second` at the start of
/// the frame after it (so exactly one draw happens between them) and return
/// `second`'s result. Both are registered up front; nothing can slip between
/// the two hops.
pub(crate) async fn between_frames<T: 'static>(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    refresh: bool,
    first: impl FnOnce(&mut Window, &mut App) + 'static,
    second: impl FnOnce(&mut Window, &mut App) -> T + 'static,
) -> Result<T> {
    let (tx, rx) = oneshot::channel();
    cx.update_window(window, |_, window, _| {
        if refresh {
            window.refresh();
        }
        window.on_next_frame(move |window, cx| {
            first(window, cx);
            window.on_next_frame(move |window, cx| {
                tx.send(second(window, cx)).ok();
            });
        });
    })?;
    receive(rx, cx).await
}

async fn receive<T>(rx: oneshot::Receiver<T>, cx: &mut AsyncApp) -> Result<T> {
    // Two hops can legitimately take two frame timeouts' worth on a
    // throttled inactive window, so allow for it.
    match rx
        .with_timeout(FRAME_TIMEOUT * 2, cx.background_executor())
        .await
    {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(oneshot::Canceled)) => Err(anyhow!("the window closed while waiting for a frame")),
        Err(_) => Err(anyhow!(
            "no frame within {}s: is the window minimised or hidden?",
            (FRAME_TIMEOUT * 2).as_secs()
        )),
    }
}

/// Wait for `n` frame starts (not necessarily draws) of `window`.
pub(crate) async fn frames(window: AnyWindowHandle, cx: &mut AsyncApp, n: u32) -> Result<()> {
    for _ in 0..n {
        at_next_frame(window, cx, |_, _| ()).await?;
    }
    Ok(())
}

/// Sleep on the app's timer. Used for pacing (`step_ms`, `hold_ms`); frame
/// synchronisation uses the functions above.
pub(crate) async fn sleep(cx: &mut AsyncApp, duration: Duration) {
    if !duration.is_zero() {
        cx.background_executor().timer(duration).await;
    }
}
