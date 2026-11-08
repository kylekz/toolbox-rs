//! Light or dark, and the app's choice between them (docs/appearance.md "Light and dark").

use gpui::WindowAppearance;

/// What a window shows: light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Appearance {
    Light,
    #[default]
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 2] = [Self::Light, Self::Dark];

    /// `"light"` or `"dark"`, as settings files and automation state spell it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The appearance a [`key`](Self::key) names.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|appearance| appearance.key() == key)
    }

    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }

    /// What gpui's appearance stands for; the vibrant variants are macOS's names for the same two.
    pub fn of_window(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
        }
    }
}

/// The app's light/dark setting: follow the system live, or always one appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Self::System, Self::Light, Self::Dark];

    /// `"system"`, `"light"` or `"dark"`, as settings files and automation state spell it.
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The mode a [`key`](Self::key) names.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    /// The appearance this mode asks for while the system shows `system`.
    pub fn resolve(self, system: Appearance) -> Appearance {
        self.fixed().unwrap_or(system)
    }

    /// The appearance this mode always shows, or `None` for [`Mode::System`].
    pub fn fixed(self) -> Option<Appearance> {
        match self {
            Self::System => None,
            Self::Light => Some(Appearance::Light),
            Self::Dark => Some(Appearance::Dark),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for appearance in Appearance::ALL {
            assert_eq!(Appearance::from_key(appearance.key()), Some(appearance));
        }
        for mode in Mode::ALL {
            assert_eq!(Mode::from_key(mode.key()), Some(mode));
        }
        assert_eq!(Mode::from_key("Dark"), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_spells_the_keys() {
        for mode in Mode::ALL {
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::json!(mode.key())
            );
            assert_eq!(
                serde_json::from_value::<Mode>(serde_json::json!(mode.key())).unwrap(),
                mode
            );
        }
        for appearance in Appearance::ALL {
            assert_eq!(
                serde_json::to_value(appearance).unwrap(),
                serde_json::json!(appearance.key())
            );
        }
    }

    #[test]
    fn system_follows_and_the_others_ignore_it() {
        assert_eq!(Mode::System.resolve(Appearance::Light), Appearance::Light);
        assert_eq!(Mode::System.resolve(Appearance::Dark), Appearance::Dark);
        assert_eq!(Mode::Dark.resolve(Appearance::Light), Appearance::Dark);
        assert_eq!(Mode::Light.resolve(Appearance::Dark), Appearance::Light);
    }

    #[test]
    fn vibrant_window_appearances_are_the_plain_ones() {
        assert_eq!(
            Appearance::of_window(WindowAppearance::VibrantDark),
            Appearance::Dark
        );
        assert_eq!(
            Appearance::of_window(WindowAppearance::VibrantLight),
            Appearance::Light
        );
    }
}
