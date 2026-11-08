//! macOS: the crate's own vibrancy view and the window's `NSAppearance` (docs/appearance.md
//! "Vibrancy").
//!
//! gpui's `Blurred` is an `NSVisualEffectView` subclass with its tint (`CAChameleonLayer`) and
//! saturation filter stripped, which shows no blur at all on macOS 27: the window is merely
//! clear. The crate adds a plain `NSVisualEffectView` instead, keeping AppKit's own layers, as
//! the bottom subview of the window's content view, the view gpui adds its own `GPUIView` to:
//! `addSubview:positioned:NSWindowBelow relativeTo:nil`, sized to the content view and resized
//! with it, as gpui's own blurred view is. The window itself is gpui's `Transparent` (not opaque,
//! a near-clear background colour and a transparent Metal layer), so whatever gpui leaves
//! unpainted shows the view. Events still reach gpui: its view is above, covering the window.
//!
//! The view is found again by its `identifier`, so the crate keeps no per-window state. Every
//! call needs the main thread (gpui's) and a real AppKit window; elsewhere (gpui's test
//! platform) it does nothing.

use gpui::Window;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization as _, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAppearanceNameVibrantDark, NSAppearanceNameVibrantLight, NSAutoresizingMaskOptions,
    NSUserInterfaceItemIdentification as _, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    NSWorkspace,
};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{Appearance, Blending, Native, Vibrancy, VibrancyMaterial, VibrancyState};

/// The `identifier` of the crate's effect view.
const VIEW_ID: &str = "toolbox-rs.gpui_appearance.vibrancy";

/// Accessibility › Display › "Reduce transparency".
pub fn reduce_transparency() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

/// Puts the effect view behind gpui's content, or restyles the one already there.
pub fn install_vibrancy(window: &Window, vibrancy: Vibrancy) {
    let Some((mtm, content)) = content_view(window) else {
        return;
    };
    let view = effect_view(&content).unwrap_or_else(|| {
        let view = NSVisualEffectView::initWithFrame(mtm.alloc(), content.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        view.setIdentifier(Some(&NSString::from_str(VIEW_ID)));
        content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        view
    });
    view.setMaterial(NSVisualEffectMaterial(vibrancy.material.raw()));
    view.setBlendingMode(NSVisualEffectBlendingMode(vibrancy.blending.raw()));
    view.setState(NSVisualEffectState(vibrancy.state.raw()));
}

/// Takes the effect view out of the window, if it is there.
pub fn remove_vibrancy(window: &Window) {
    if let Some((_, content)) = content_view(window)
        && let Some(view) = effect_view(&content)
    {
        view.removeFromSuperview();
    }
}

/// Sets the window's own appearance: aqua or darkAqua, or none to follow the application's,
/// which follows the system.
pub fn set_appearance(window: &Window, forced: Option<Appearance>) {
    let Some((_, content)) = content_view(window) else {
        return;
    };
    let Some(ns_window) = content.window() else {
        return;
    };
    let appearance = forced.and_then(|forced| {
        /* SAFETY: AppKit's appearance-name constants, which it defines for the process's life. */
        let name = unsafe {
            match forced {
                Appearance::Light => NSAppearanceNameAqua,
                Appearance::Dark => NSAppearanceNameDarkAqua,
            }
        };
        NSAppearance::appearanceNamed(name)
    });
    ns_window.setAppearance(appearance.as_deref());
}

/// What AppKit reports for the window.
pub fn native(window: &Window) -> Option<Native> {
    let (_, content) = content_view(window)?;
    let ns_window = content.window()?;
    let forced = ns_window.appearance().map(|appearance| {
        let name = appearance.name();
        /* SAFETY: as in `set_appearance`. */
        let dark = unsafe {
            name.isEqualToString(NSAppearanceNameDarkAqua)
                || name.isEqualToString(NSAppearanceNameVibrantDark)
        };
        /* SAFETY: as in `set_appearance`. */
        debug_assert!(
            dark || unsafe {
                name.isEqualToString(NSAppearanceNameAqua)
                    || name.isEqualToString(NSAppearanceNameVibrantLight)
            }
        );
        if dark {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    });
    let view = effect_view(&content);
    Some(Native {
        forced,
        opaque: ns_window.isOpaque(),
        vibrancy_view: view.is_some(),
        vibrancy: view.and_then(|view| {
            Some(Vibrancy {
                material: VibrancyMaterial::from_raw(view.material().0)?,
                blending: Blending::from_raw(view.blendingMode().0)?,
                state: VibrancyState::from_raw(view.state().0)?,
            })
        }),
    })
}

/// The window's content view, the superview of gpui's `GPUIView`, on the main thread.
fn content_view(window: &Window) -> Option<(MainThreadMarker, Retained<NSView>)> {
    let mtm = MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    /*
     * SAFETY: gpui's AppKit handle points at the window's live `GPUIView`, an `NSView`; retaining
     * it keeps it alive for as long as this borrow is used. `superview` is safe to call on the
     * main thread, which `mtm` proves this is.
     */
    let content = unsafe {
        let view = Retained::retain(handle.ns_view.as_ptr().cast::<NSView>())?;
        view.superview()
    }?;
    Some((mtm, content))
}

fn effect_view(content: &NSView) -> Option<Retained<NSVisualEffectView>> {
    let id = NSString::from_str(VIEW_ID);
    content
        .subviews()
        .iter()
        .find(|view| {
            view.identifier()
                .is_some_and(|each| each.isEqualToString(&id))
        })
        .and_then(|view| view.downcast::<NSVisualEffectView>().ok())
}
