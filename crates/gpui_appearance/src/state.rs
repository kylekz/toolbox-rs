//! The app-wide appearance: what the system shows, what the app asks for, what automation pins,
//! and what that resolves to (docs/appearance.md "State").
//!
//! [`AppearanceState`] is a gpui global. Every change that alters it notifies its observers once
//! (`cx.observe_global::<AppearanceState>`), and nothing else does, so an app's theme can
//! reinstall itself from [`AppearanceState::resolved`] in an observer. Windows follow it through
//! [`crate::track_window`].

use gpui::{App, Global};

use crate::{Appearance, Material, MaterialSupport, Mode, Vibrancy};

/// What the app asks for, usually from its settings file, already narrowed by anything the app
/// knows that the crate doesn't (a palette that has no light variant asks for [`Mode::Dark`]; one
/// that is opaque only asks for [`Material::Opaque`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Preferences {
    pub mode: Mode,
    pub material: Material,
}

impl Default for Preferences {
    /// Follow the system, on the platform's default material.
    fn default() -> Self {
        Self {
            mode: Mode::System,
            material: Material::platform_default(),
        }
    }
}

/// What windows show now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resolved {
    pub appearance: Appearance,
    pub material: Material,
}

/// What [`init`] starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    /// What the platform can show: [`MaterialSupport::probe`] in the app, a constant in tests.
    pub support: MaterialSupport,
    pub preferences: Preferences,
    /// The vibrancy view's style, the app's design choice (macOS).
    pub vibrancy: Vibrancy,
}

/// The app-wide appearance (module docs). Read it with [`state`], change it with the free
/// functions of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearanceState {
    system: Appearance,
    support: MaterialSupport,
    preferences: Preferences,
    pinned: Option<Appearance>,
    vibrancy: Vibrancy,
}

impl Global for AppearanceState {}

impl AppearanceState {
    /// The system's light/dark setting, as last reported.
    pub fn system(&self) -> Appearance {
        self.system
    }

    /// What the platform can show, as last probed.
    pub fn support(&self) -> MaterialSupport {
        self.support
    }

    pub fn preferences(&self) -> Preferences {
        self.preferences
    }

    /// The automation override ([`pin`]), which beats the mode and the system.
    pub fn pinned(&self) -> Option<Appearance> {
        self.pinned
    }

    pub fn vibrancy(&self) -> Vibrancy {
        self.vibrancy
    }

    /// The appearance windows are told to show whatever the system does: the pin, else a fixed
    /// mode. `None` lets each window follow the system itself, live.
    pub fn forced(&self) -> Option<Appearance> {
        self.pinned.or(self.preferences.mode.fixed())
    }

    pub fn resolved(&self) -> Resolved {
        Resolved {
            appearance: self.forced().unwrap_or(self.system),
            material: self.support.resolve(self.preferences.material),
        }
    }
}

/// Starts tracking the appearance. Call once at startup, before the first window opens, so
/// [`crate::window_background`] and the app's theme already see the resolved values.
pub fn init(setup: Setup, cx: &mut App) {
    let Setup {
        support,
        preferences,
        vibrancy,
    } = setup;
    log::info!("window materials available: {}", describe(support));
    cx.set_global(AppearanceState {
        system: Appearance::of_window(cx.window_appearance()),
        support,
        preferences,
        pinned: None,
        vibrancy,
    });
}

/// The app-wide appearance. Panics if [`init`] hasn't run, which is a startup bug.
pub fn state(cx: &App) -> &AppearanceState {
    cx.global::<AppearanceState>()
}

/// What windows show now. Panics if [`init`] hasn't run.
pub fn resolved(cx: &App) -> Resolved {
    state(cx).resolved()
}

/// The app's settings changed.
pub fn set_preferences(preferences: Preferences, cx: &mut App) {
    update(cx, |state| state.preferences = preferences);
}

/// Restyles the vibrancy view of every tracked window.
pub fn set_vibrancy(vibrancy: Vibrancy, cx: &mut App) {
    update(cx, |state| state.vibrancy = vibrancy);
}

/// The automation override: scenarios pin light or dark whatever the mode and the system say,
/// without touching the app's settings; `None` lifts it.
pub fn pin(appearance: Option<Appearance>, cx: &mut App) {
    update(cx, |state| state.pinned = appearance);
}

/// The system's light/dark setting changed. Tracked windows report it themselves; tests and
/// scenarios call it to stand in for the system.
pub fn set_system(appearance: Appearance, cx: &mut App) {
    update(cx, |state| state.system = appearance);
}

/// Re-reads what the platform can show: the transparency switches may have flipped while the app
/// was in the background. Tracked windows call it whenever they are activated.
pub fn refresh_support(cx: &mut App) {
    set_support(MaterialSupport::probe(), cx);
}

/// Replaces what the platform can show: [`refresh_support`], tests and scenarios.
pub fn set_support(support: MaterialSupport, cx: &mut App) {
    if state(cx).support != support {
        log::info!("window materials available now: {}", describe(support));
    }
    update(cx, |state| state.support = support);
}

/// Applies `change`, and sets (so notifies) the global only when something changed.
fn update(cx: &mut App, change: impl FnOnce(&mut AppearanceState)) {
    let mut next = *state(cx);
    change(&mut next);
    if next != *state(cx) {
        cx.set_global(next);
    }
}

/// `"vibrancy, opaque"`, for the log.
fn describe(support: MaterialSupport) -> String {
    support
        .available()
        .iter()
        .map(|material| material.key())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{TestAppContext, WindowAppearance};

    use super::*;

    fn setup(cx: &mut TestAppContext, support: MaterialSupport) {
        cx.update(|cx| {
            init(
                Setup {
                    support,
                    preferences: Preferences {
                        mode: Mode::System,
                        material: Material::Mica,
                    },
                    vibrancy: Vibrancy::default(),
                },
                cx,
            )
        });
    }

    #[gpui::test]
    fn the_system_is_read_at_init(cx: &mut TestAppContext) {
        let system = cx.update(|cx| Appearance::of_window(cx.window_appearance()));
        setup(cx, MaterialSupport::ALL);
        cx.update(|cx| {
            assert_eq!(state(cx).system(), system);
            assert_eq!(resolved(cx).appearance, system);
        });
    }

    #[gpui::test]
    fn system_mode_follows_and_a_fixed_mode_does_not(cx: &mut TestAppContext) {
        setup(cx, MaterialSupport::ALL);
        cx.update(|cx| {
            set_system(Appearance::Light, cx);
            assert_eq!(resolved(cx).appearance, Appearance::Light);
            assert_eq!(state(cx).forced(), None);
            set_system(Appearance::of_window(WindowAppearance::VibrantDark), cx);
            assert_eq!(resolved(cx).appearance, Appearance::Dark);

            set_preferences(
                Preferences {
                    mode: Mode::Light,
                    material: Material::Mica,
                },
                cx,
            );
            set_system(Appearance::Dark, cx);
            assert_eq!(resolved(cx).appearance, Appearance::Light);
            assert_eq!(state(cx).forced(), Some(Appearance::Light));
        });
    }

    #[gpui::test]
    fn a_pin_beats_the_mode_and_the_system(cx: &mut TestAppContext) {
        setup(cx, MaterialSupport::ALL);
        cx.update(|cx| {
            set_preferences(
                Preferences {
                    mode: Mode::Light,
                    material: Material::Mica,
                },
                cx,
            );
            pin(Some(Appearance::Dark), cx);
            assert_eq!(resolved(cx).appearance, Appearance::Dark);
            assert_eq!(state(cx).forced(), Some(Appearance::Dark));
            pin(None, cx);
            assert_eq!(resolved(cx).appearance, Appearance::Light);
        });
    }

    #[gpui::test]
    fn the_platform_narrows_the_material(cx: &mut TestAppContext) {
        setup(cx, MaterialSupport::OPAQUE_ONLY);
        cx.update(|cx| {
            assert_eq!(resolved(cx).material, Material::Opaque, "transparency off");
            assert_eq!(state(cx).preferences().material, Material::Mica, "kept");
            set_support(MaterialSupport::ALL, cx);
            assert_eq!(resolved(cx).material, Material::Mica);
        });
    }

    #[gpui::test]
    fn observers_hear_only_real_changes(cx: &mut TestAppContext) {
        setup(cx, MaterialSupport::ALL);
        let calls = Rc::new(Cell::new(0));
        let _subscription = cx.update(|cx| {
            let calls = calls.clone();
            cx.observe_global::<AppearanceState>(move |_| calls.set(calls.get() + 1))
        });
        cx.run_until_parked();
        let other = match cx.update(|cx| state(cx).system()) {
            Appearance::Light => Appearance::Dark,
            Appearance::Dark => Appearance::Light,
        };
        cx.update(|cx| set_system(other, cx));
        cx.run_until_parked();
        assert_eq!(calls.get(), 1);
        cx.update(|cx| set_system(other, cx));
        cx.update(|cx| set_support(MaterialSupport::ALL, cx));
        cx.run_until_parked();
        assert_eq!(calls.get(), 1, "nothing changed");
        cx.update(|cx| pin(Some(Appearance::Dark), cx));
        cx.run_until_parked();
        assert_eq!(calls.get(), 2);
    }
}
