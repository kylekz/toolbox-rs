//! Light/dark and window materials for gpui apps (docs/appearance.md; PLAN §5a).
//!
//! gpui: **yes**. One app-wide [`AppearanceState`] holds:
//! - the system's light/dark setting, followed live;
//! - what the app asks for ([`Preferences`]: a [`Mode`] of System / Light / Dark and a
//!   [`Material`]);
//! - an automation override ([`pin`]) that scenarios use to pin light or dark;
//! - what the platform can show ([`MaterialSupport::probe`]).
//!
//! It resolves all of them into the [`Resolved`] appearance and material, which the app's theme
//! reads to pick its tokens and which [`track_window`] applies to each window: Mica and Acrylic
//! on Windows 11, the crate's own `NSVisualEffectView` on macOS ([`Vibrancy`]), and the frame's
//! light/dark on both.
//!
//! ```ignore
//! // startup, before the first window:
//! gpui_appearance::init(Setup { support: MaterialSupport::probe(), preferences, vibrancy }, cx);
//! // the app's settings changed:
//! gpui_appearance::set_preferences(Preferences { mode, material }, cx);
//! // the theme follows:
//! cx.observe_global::<AppearanceState>(|cx| theme::install(gpui_appearance::resolved(cx), cx));
//! // each window:
//! WindowOptions { window_background: gpui_appearance::window_background(Backing::Solid, cx), .. }
//! // in its root view's constructor, kept in the view:
//! let tracker = gpui_appearance::track_window(Backing::Solid, window, cx);
//! ```

mod appearance;
#[cfg(target_os = "macos")]
mod macos;
mod material;
mod state;
mod vibrancy;
mod window;
#[cfg(windows)]
mod windows;

pub use appearance::{Appearance, Mode};
pub use material::{Backing, MICA_BUILD, Material, MaterialSupport, WINDOWS_11_BUILD};
pub use state::{
    AppearanceState, Preferences, Resolved, Setup, init, pin, refresh_support, resolved,
    set_preferences, set_support, set_system, set_vibrancy, state,
};
pub use vibrancy::{Blending, Vibrancy, VibrancyMaterial, VibrancyState};
pub use window::{Applied, Native, WindowTracker, native, track_window, window_background};
