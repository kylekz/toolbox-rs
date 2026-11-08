//! How [`Material::Vibrancy`](crate::Material::Vibrancy) looks: the `NSVisualEffectView`'s
//! material, blending mode and state (docs/appearance.md "Vibrancy"). The app picks these as a
//! design decision; they are not a user setting. Defined on every platform, so settings and
//! automation code needs no `cfg`; only macOS shows them.

/// `NSVisualEffectMaterial`, the semantic materials (the deprecated `Light`/`Dark`/`MediumLight`/
/// `UltraDark`/`AppearanceBased` are left out). Each follows the window's light/dark appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum VibrancyMaterial {
    Titlebar,
    Selection,
    Menu,
    Popover,
    Sidebar,
    HeaderView,
    Sheet,
    WindowBackground,
    HudWindow,
    FullScreenUi,
    ToolTip,
    ContentBackground,
    /// The default: the material AppKit puts under a window's content.
    #[default]
    UnderWindowBackground,
    UnderPageBackground,
}

impl VibrancyMaterial {
    pub const ALL: [VibrancyMaterial; 14] = [
        Self::Titlebar,
        Self::Selection,
        Self::Menu,
        Self::Popover,
        Self::Sidebar,
        Self::HeaderView,
        Self::Sheet,
        Self::WindowBackground,
        Self::HudWindow,
        Self::FullScreenUi,
        Self::ToolTip,
        Self::ContentBackground,
        Self::UnderWindowBackground,
        Self::UnderPageBackground,
    ];

    /// The kebab-case name automation state and an app's config spell it with.
    pub fn key(self) -> &'static str {
        match self {
            Self::Titlebar => "titlebar",
            Self::Selection => "selection",
            Self::Menu => "menu",
            Self::Popover => "popover",
            Self::Sidebar => "sidebar",
            Self::HeaderView => "header-view",
            Self::Sheet => "sheet",
            Self::WindowBackground => "window-background",
            Self::HudWindow => "hud-window",
            Self::FullScreenUi => "full-screen-ui",
            Self::ToolTip => "tool-tip",
            Self::ContentBackground => "content-background",
            Self::UnderWindowBackground => "under-window-background",
            Self::UnderPageBackground => "under-page-background",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|material| material.key() == key)
    }

    /// AppKit's `NSVisualEffectMaterial` value.
    pub fn raw(self) -> isize {
        match self {
            Self::Titlebar => 3,
            Self::Selection => 4,
            Self::Menu => 5,
            Self::Popover => 6,
            Self::Sidebar => 7,
            Self::HeaderView => 10,
            Self::Sheet => 11,
            Self::WindowBackground => 12,
            Self::HudWindow => 13,
            Self::FullScreenUi => 15,
            Self::ToolTip => 17,
            Self::ContentBackground => 18,
            Self::UnderWindowBackground => 21,
            Self::UnderPageBackground => 22,
        }
    }

    /// The material an AppKit value stands for; `None` for the deprecated ones.
    pub fn from_raw(raw: isize) -> Option<Self> {
        Self::ALL.into_iter().find(|material| material.raw() == raw)
    }
}

/// `NSVisualEffectBlendingMode`: what the view blurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum Blending {
    /// The desktop and other windows behind this one: the default, and the only mode that shows
    /// anything behind a whole-window background, since gpui draws the content above it.
    #[default]
    BehindWindow,
    /// The window's own content under the view: here, nothing but the material's tint.
    WithinWindow,
}

impl Blending {
    pub const ALL: [Blending; 2] = [Self::BehindWindow, Self::WithinWindow];

    pub fn key(self) -> &'static str {
        match self {
            Self::BehindWindow => "behind-window",
            Self::WithinWindow => "within-window",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|blending| blending.key() == key)
    }

    pub fn raw(self) -> isize {
        match self {
            Self::BehindWindow => 0,
            Self::WithinWindow => 1,
        }
    }

    pub fn from_raw(raw: isize) -> Option<Self> {
        Self::ALL.into_iter().find(|blending| blending.raw() == raw)
    }
}

/// `NSVisualEffectState`: when the material is live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum VibrancyState {
    /// Live while the window is active, flat while it isn't, as every macOS window does: the
    /// default. A background automation run's window is never active, so it shows flat.
    #[default]
    FollowsWindow,
    Active,
    Inactive,
}

impl VibrancyState {
    pub const ALL: [VibrancyState; 3] = [Self::FollowsWindow, Self::Active, Self::Inactive];

    pub fn key(self) -> &'static str {
        match self {
            Self::FollowsWindow => "follows-window",
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.key() == key)
    }

    pub fn raw(self) -> isize {
        match self {
            Self::FollowsWindow => 0,
            Self::Active => 1,
            Self::Inactive => 2,
        }
    }

    pub fn from_raw(raw: isize) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.raw() == raw)
    }
}

/// The whole style of the vibrancy view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vibrancy {
    pub material: VibrancyMaterial,
    pub blending: Blending,
    pub state: VibrancyState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_raw_values_round_trip() {
        for material in VibrancyMaterial::ALL {
            assert_eq!(VibrancyMaterial::from_key(material.key()), Some(material));
            assert_eq!(VibrancyMaterial::from_raw(material.raw()), Some(material));
        }
        for blending in Blending::ALL {
            assert_eq!(Blending::from_key(blending.key()), Some(blending));
            assert_eq!(Blending::from_raw(blending.raw()), Some(blending));
        }
        for state in VibrancyState::ALL {
            assert_eq!(VibrancyState::from_key(state.key()), Some(state));
            assert_eq!(VibrancyState::from_raw(state.raw()), Some(state));
        }
        // AppKit's deprecated `Dark`.
        assert_eq!(VibrancyMaterial::from_raw(2), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_spells_the_keys() {
        for material in VibrancyMaterial::ALL {
            assert_eq!(
                serde_json::to_value(material).unwrap(),
                serde_json::json!(material.key())
            );
        }
        for blending in Blending::ALL {
            assert_eq!(
                serde_json::to_value(blending).unwrap(),
                serde_json::json!(blending.key())
            );
        }
        for state in VibrancyState::ALL {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.key())
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn raw_values_are_appkits() {
        use objc2_app_kit::{
            NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        };
        assert_eq!(
            VibrancyMaterial::UnderWindowBackground.raw(),
            NSVisualEffectMaterial::UnderWindowBackground.0
        );
        assert_eq!(
            VibrancyMaterial::HudWindow.raw(),
            NSVisualEffectMaterial::HUDWindow.0
        );
        assert_eq!(
            VibrancyMaterial::UnderPageBackground.raw(),
            NSVisualEffectMaterial::UnderPageBackground.0
        );
        assert_eq!(
            Blending::WithinWindow.raw(),
            NSVisualEffectBlendingMode::WithinWindow.0
        );
        assert_eq!(
            VibrancyState::Inactive.raw(),
            NSVisualEffectState::Inactive.0
        );
    }
}
