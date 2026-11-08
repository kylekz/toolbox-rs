//! Applying the appearance to windows (docs/appearance.md "Windows").
//!
//! A window opens with [`window_background`] in its `WindowOptions` and its root view calls
//! [`track_window`] once. From then on the window follows [`AppearanceState`]:
//! - **light/dark:** macOS sets the window's own `NSAppearance` (aqua or darkAqua, or none to
//!   follow the system), which tints the title bar, the traffic lights and the vibrancy material.
//!   gpui's `App::set_window_appearance` isn't used: it sets the application's appearance,
//!   after which `App::window_appearance` reports the forced value and the system's own setting
//!   can no longer be followed. Windows sets `DWMWA_USE_IMMERSIVE_DARK_MODE`, which tints the
//!   frame and the Mica/Acrylic backdrop. Linux leaves the window alone.
//! - **material:** gpui's `set_background_appearance` only adds its own effect, so the previous
//!   material is cleared first (going from Acrylic to Mica would otherwise leave both on), and
//!   macOS's vibrancy is the crate's own effect view (`crate::macos`).
//!
//! The tracker also reports the system's light/dark setting as the window sees it change, and
//! re-probes the materials when the window is activated.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, Context, Subscription, Window, WindowBackgroundAppearance};

use crate::state::{AppearanceState, refresh_support, set_system, state};
use crate::{Appearance, Backing, Material, Vibrancy};

/// What `WindowOptions::window_background` takes for a window about to open, so its first frame
/// already shows the resolved material. Panics if [`crate::init`] hasn't run.
pub fn window_background(backing: Backing, cx: &App) -> WindowBackgroundAppearance {
    state(cx).resolved().material.window_background(backing)
}

/// What the crate last applied to one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Applied {
    /// The appearance the window shows.
    pub appearance: Appearance,
    /// What the window was told to show whatever the system does; `None` follows the system.
    pub forced: Option<Appearance>,
    pub material: Material,
    /// The vibrancy view's style, while [`Applied::material`] is vibrancy.
    pub vibrancy: Option<Vibrancy>,
}

impl Applied {
    fn of(state: &AppearanceState) -> Self {
        let resolved = state.resolved();
        Self {
            appearance: resolved.appearance,
            forced: state.forced(),
            material: resolved.material,
            vibrancy: (resolved.material == Material::Vibrancy).then(|| state.vibrancy()),
        }
    }
}

/// Keeps one window in step with [`AppearanceState`]; the window's root view holds it for the
/// window's lifetime.
pub struct WindowTracker {
    applied: Rc<Cell<Applied>>,
    _subscriptions: [Subscription; 3],
}

impl WindowTracker {
    /// What the crate last applied to the window.
    pub fn applied(&self) -> Applied {
        self.applied.get()
    }
}

/// Applies the current appearance to `window` and keeps it applied. Call from the window's root
/// view (`V`) as it is built, and keep the tracker in that view. `backing` says what Opaque means
/// for this window, as in [`window_background`].
pub fn track_window<V: 'static>(
    backing: Backing,
    window: &mut Window,
    cx: &mut Context<V>,
) -> WindowTracker {
    set_system(Appearance::of_window(cx.window_appearance()), cx);
    let applied = Rc::new(Cell::new(apply(None, backing, window, cx)));
    let subscriptions = [
        cx.observe_global_in::<AppearanceState>(window, {
            let applied = applied.clone();
            move |_, window, cx| applied.set(apply(Some(applied.get()), backing, window, cx))
        }),
        cx.observe_window_appearance(window, {
            let applied = applied.clone();
            move |_, window, cx| {
                /*
                 * The application's appearance, not the window's: a window with a forced
                 * appearance reports that, not the system's. The change itself reaches the
                 * window through the global observer above.
                 */
                set_system(Appearance::of_window(cx.window_appearance()), cx);
                /*
                 * On Windows gpui sets the frame's dark mode to the system's right after this
                 * observer runs; the app's may differ, so it is put back on the next frame.
                 * Only there: on macOS setting the window's appearance is itself an appearance
                 * change, which would bring this observer back every frame.
                 */
                if cfg!(windows) {
                    let applied = applied.clone();
                    window.on_next_frame(move |window, _| {
                        let applied = applied.get();
                        set_window_appearance(window, applied.forced, applied.appearance);
                    });
                }
            }
        }),
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                refresh_support(cx);
            }
        }),
    ];
    WindowTracker {
        applied,
        _subscriptions: subscriptions,
    }
}

/// Brings `window` from `previous` (`None`: unknown, so everything) to the current appearance.
fn apply(previous: Option<Applied>, backing: Backing, window: &Window, cx: &App) -> Applied {
    let next = Applied::of(state(cx));
    if previous.map(|previous| (previous.forced, previous.appearance))
        != Some((next.forced, next.appearance))
    {
        set_window_appearance(window, next.forced, next.appearance);
    }
    if previous.map(|previous| previous.material) != Some(next.material) {
        set_material(window, next.material, backing);
    }
    #[cfg(target_os = "macos")]
    if let Some(vibrancy) = next.vibrancy
        && previous.and_then(|previous| previous.vibrancy) != Some(vibrancy)
    {
        crate::macos::install_vibrancy(window, vibrancy);
    }
    next
}

fn set_window_appearance(window: &Window, forced: Option<Appearance>, appearance: Appearance) {
    #[cfg(target_os = "macos")]
    {
        let _ = appearance;
        crate::macos::set_appearance(window, forced);
    }
    #[cfg(windows)]
    {
        let _ = forced;
        crate::windows::set_dark_mode(window, appearance.is_dark());
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, forced, appearance);
}

fn set_material(window: &Window, material: Material, backing: Backing) {
    // Opaque turns gpui's own effects off: the accent policy (Acrylic) and its blurred view.
    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    #[cfg(windows)]
    if material != Material::Mica {
        crate::windows::clear_system_backdrop(window);
    }
    #[cfg(target_os = "macos")]
    if material != Material::Vibrancy {
        crate::macos::remove_vibrancy(window);
    }
    let background = material.window_background(backing);
    if background != WindowBackgroundAppearance::Opaque {
        window.set_background_appearance(background);
    }
}

/// What the OS itself reports for a window, read back rather than remembered: what scenarios
/// assert to prove the crate's changes reached AppKit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Native {
    /// The `NSWindow`'s own appearance; `None` follows the application's, so the system's.
    pub forced: Option<Appearance>,
    /// `NSWindow.isOpaque`.
    pub opaque: bool,
    /// Whether the crate's effect view is in the window.
    pub vibrancy_view: bool,
    /// That view's style; `None` without the view or with a value the crate doesn't set.
    pub vibrancy: Option<Vibrancy>,
}

/// What the OS reports for `window`: macOS only; `None` elsewhere, where the crate has nothing it
/// can read back.
pub fn native(window: &Window) -> Option<Native> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::native(window)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Render, TestAppContext, VisualTestContext, div};

    use super::*;
    use crate::{MaterialSupport, Mode, Preferences, Setup, pin, set_preferences, set_vibrancy};

    struct Root {
        tracker: WindowTracker,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            div()
        }
    }

    fn open(cx: &mut TestAppContext) -> (gpui::Entity<Root>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::init(
                Setup {
                    support: MaterialSupport::ALL,
                    preferences: Preferences {
                        mode: Mode::Dark,
                        material: Material::Acrylic,
                    },
                    vibrancy: Vibrancy::default(),
                },
                cx,
            )
        });
        cx.add_window_view(|window, cx| Root {
            tracker: track_window(Backing::Solid, window, cx),
        })
    }

    fn applied(root: &gpui::Entity<Root>, cx: &mut VisualTestContext) -> Applied {
        cx.run_until_parked();
        root.read_with(cx, |root, _| root.tracker.applied())
    }

    #[gpui::test]
    fn a_window_follows_the_state(cx: &mut TestAppContext) {
        let (root, cx) = open(cx);
        assert_eq!(
            applied(&root, cx),
            Applied {
                appearance: Appearance::Dark,
                forced: Some(Appearance::Dark),
                material: Material::Acrylic,
                vibrancy: None,
            }
        );
        cx.update(|_, cx| {
            set_preferences(
                Preferences {
                    mode: Mode::System,
                    material: Material::Vibrancy,
                },
                cx,
            );
            crate::set_system(Appearance::Light, cx);
        });
        assert_eq!(
            applied(&root, cx),
            Applied {
                appearance: Appearance::Light,
                forced: None,
                material: Material::Vibrancy,
                vibrancy: Some(Vibrancy::default()),
            }
        );
        let sidebar = Vibrancy {
            material: crate::VibrancyMaterial::Sidebar,
            ..Vibrancy::default()
        };
        cx.update(|_, cx| {
            set_vibrancy(sidebar, cx);
            pin(Some(Appearance::Dark), cx);
        });
        let now = applied(&root, cx);
        assert_eq!(now.vibrancy, Some(sidebar));
        assert_eq!(
            (now.appearance, now.forced),
            (Appearance::Dark, Some(Appearance::Dark))
        );
    }

    #[gpui::test]
    fn the_first_frame_already_has_the_material(cx: &mut TestAppContext) {
        let (_root, cx) = open(cx);
        cx.update(|_, cx| {
            assert_eq!(
                window_background(Backing::Solid, cx),
                WindowBackgroundAppearance::Blurred
            );
            crate::set_support(MaterialSupport::OPAQUE_ONLY, cx);
            assert_eq!(
                window_background(Backing::SeeThrough, cx),
                WindowBackgroundAppearance::Transparent
            );
        });
    }
}
