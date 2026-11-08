//! Window materials and what the platform can show right now (docs/appearance.md "Materials").
//!
//! [`MaterialSupport::probe`] asks the OS:
//! - **Windows:** Mica needs build 22621 or later (gpui sets it through
//!   `DWMWA_SYSTEMBACKDROP_TYPE`, which older builds lack, and does nothing there); Acrylic needs
//!   Windows 11 (build 22000). Both need Settings › Personalization › Colors › "Transparency
//!   effects" on. Windows 10 shows Opaque.
//! - **macOS:** vibrancy, unless Accessibility › Display › "Reduce transparency" is on.
//! - **Linux:** Opaque.

use gpui::WindowBackgroundAppearance;

/// The first Windows 11 build.
pub const WINDOWS_11_BUILD: u32 = 22000;
/// The first build with `DWMWA_SYSTEMBACKDROP_TYPE`, which gpui's Mica uses.
pub const MICA_BUILD: u32 = 22621;

/// What shows behind a window's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Material {
    /// Windows 11's Mica: the wallpaper, blurred and tinted; the Windows default.
    Mica,
    /// Windows' Acrylic: whatever is behind the window, blurred. It can lag while the window is
    /// dragged on some builds, which is why Mica is the default.
    Acrylic,
    /// macOS's vibrancy: an `NSVisualEffectView` behind the window's content, styled by
    /// [`Vibrancy`](crate::Vibrancy).
    Vibrancy,
    /// Nothing: the app paints solid surfaces.
    Opaque,
}

impl Material {
    pub const ALL: [Material; 4] = [Self::Mica, Self::Acrylic, Self::Vibrancy, Self::Opaque];

    /// `"mica"`, `"acrylic"`, `"vibrancy"` or `"opaque"`, as settings files and automation state
    /// spell it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Mica => "mica",
            Self::Acrylic => "acrylic",
            Self::Vibrancy => "vibrancy",
            Self::Opaque => "opaque",
        }
    }

    /// The name a settings screen shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mica => "Mica",
            Self::Acrylic => "Acrylic",
            Self::Vibrancy => "Vibrancy",
            Self::Opaque => "Opaque",
        }
    }

    /// The material a [`key`](Self::key) names.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|material| material.key() == key)
    }

    /// Whether the OS material shows through where the app paints nothing.
    pub fn is_translucent(self) -> bool {
        self != Self::Opaque
    }

    /// The material a setting defaults to on this platform: Mica on Windows, vibrancy on macOS,
    /// Opaque on Linux. [`MaterialSupport::resolve`] still narrows it to what the machine can show.
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            Self::Mica
        } else if cfg!(target_os = "macos") {
            Self::Vibrancy
        } else {
            Self::Opaque
        }
    }

    /// What gpui's `WindowOptions::window_background` takes for this material. Vibrancy is
    /// `Transparent`: the crate's own effect view does the blurring, not gpui's `Blurred`, which
    /// shows no blur on macOS 27.
    pub fn window_background(self, backing: Backing) -> WindowBackgroundAppearance {
        match self {
            Self::Mica => WindowBackgroundAppearance::MicaBackdrop,
            Self::Acrylic => WindowBackgroundAppearance::Blurred,
            Self::Vibrancy => WindowBackgroundAppearance::Transparent,
            Self::Opaque => match backing {
                Backing::Solid => WindowBackgroundAppearance::Opaque,
                Backing::SeeThrough => WindowBackgroundAppearance::Transparent,
            },
        }
    }
}

/// What [`Material::Opaque`] means for one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Backing {
    /// A solid window: gpui's `Opaque`.
    #[default]
    Solid,
    /// A window that must stay see-through where the app paints nothing, even without a
    /// material: gpui's `Transparent`. fc's player shows mpv's child window through a hole in
    /// its Windows frame, which gpui would clear to white over an opaque window.
    SeeThrough,
}

/// Which translucent materials the platform can show right now. Opaque always works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialSupport {
    pub mica: bool,
    pub acrylic: bool,
    pub vibrancy: bool,
}

impl MaterialSupport {
    /// Opaque only: Linux, Windows 10, transparency effects off, tests.
    pub const OPAQUE_ONLY: Self = Self {
        mica: false,
        acrylic: false,
        vibrancy: false,
    };
    /// Every material, whatever the platform: tests.
    pub const ALL: Self = Self {
        mica: true,
        acrylic: true,
        vibrancy: true,
    };

    /// What this machine can show now. Cheap: a build number and a registry value on Windows,
    /// one AppKit property on macOS.
    pub fn probe() -> Self {
        #[cfg(windows)]
        {
            Self::for_windows(
                crate::windows::build_number(),
                crate::windows::transparency_enabled(),
            )
        }
        #[cfg(target_os = "macos")]
        {
            Self::for_macos(crate::macos::reduce_transparency())
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            Self::OPAQUE_ONLY
        }
    }

    /// Windows' support for its `build` (unknown: none) with "Transparency effects" on or off.
    pub fn for_windows(build: Option<u32>, transparency: bool) -> Self {
        let build = build.unwrap_or(0);
        Self {
            mica: transparency && build >= MICA_BUILD,
            acrylic: transparency && build >= WINDOWS_11_BUILD,
            vibrancy: false,
        }
    }

    /// macOS's support with "Reduce transparency" on or off.
    pub fn for_macos(reduce_transparency: bool) -> Self {
        Self {
            vibrancy: !reduce_transparency,
            ..Self::OPAQUE_ONLY
        }
    }

    pub fn supports(self, material: Material) -> bool {
        match material {
            Material::Mica => self.mica,
            Material::Acrylic => self.acrylic,
            Material::Vibrancy => self.vibrancy,
            Material::Opaque => true,
        }
    }

    /// The materials that can be shown, in [`Material::ALL`] order; Opaque is always last.
    pub fn available(self) -> Vec<Material> {
        Material::ALL
            .into_iter()
            .filter(|material| self.supports(*material))
            .collect()
    }

    /// The material on screen for the setting `wanted`: itself when it can be shown, otherwise
    /// Opaque. Never another translucent material: an app's tokens are tuned per material, and
    /// Opaque is the one every app designs to look finished on its own.
    pub fn resolve(self, wanted: Material) -> Material {
        if self.supports(wanted) {
            wanted
        } else {
            Material::Opaque
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_support_follows_the_build_and_the_transparency_switch() {
        // Windows 10.
        assert_eq!(
            MaterialSupport::for_windows(Some(19045), true),
            MaterialSupport::OPAQUE_ONLY
        );
        // Windows 11 before DWMWA_SYSTEMBACKDROP_TYPE: Acrylic only.
        assert_eq!(
            MaterialSupport::for_windows(Some(WINDOWS_11_BUILD), true).available(),
            [Material::Acrylic, Material::Opaque]
        );
        assert_eq!(
            MaterialSupport::for_windows(Some(MICA_BUILD - 1), true).available(),
            [Material::Acrylic, Material::Opaque]
        );
        assert_eq!(
            MaterialSupport::for_windows(Some(26200), true).available(),
            [Material::Mica, Material::Acrylic, Material::Opaque]
        );
        // Transparency effects off: Opaque, whatever the build.
        assert_eq!(
            MaterialSupport::for_windows(Some(26200), false),
            MaterialSupport::OPAQUE_ONLY
        );
        assert_eq!(
            MaterialSupport::for_windows(None, true),
            MaterialSupport::OPAQUE_ONLY
        );
    }

    #[test]
    fn macos_support_follows_reduce_transparency() {
        assert_eq!(
            MaterialSupport::for_macos(false).available(),
            [Material::Vibrancy, Material::Opaque]
        );
        assert_eq!(
            MaterialSupport::for_macos(true),
            MaterialSupport::OPAQUE_ONLY
        );
    }

    #[test]
    fn an_unsupported_material_resolves_to_opaque() {
        let acrylic_only = MaterialSupport::for_windows(Some(WINDOWS_11_BUILD), true);
        assert_eq!(acrylic_only.resolve(Material::Mica), Material::Opaque);
        assert_eq!(acrylic_only.resolve(Material::Acrylic), Material::Acrylic);
        assert_eq!(acrylic_only.resolve(Material::Vibrancy), Material::Opaque);
        let mac = MaterialSupport::for_macos(false);
        assert_eq!(mac.resolve(Material::Mica), Material::Opaque);
        assert_eq!(mac.resolve(Material::Vibrancy), Material::Vibrancy);
        assert_eq!(MaterialSupport::OPAQUE_ONLY.available(), [Material::Opaque]);
    }

    #[test]
    fn keys_round_trip_and_map_onto_gpui() {
        for material in Material::ALL {
            assert_eq!(Material::from_key(material.key()), Some(material));
        }
        assert_eq!(Material::from_key("Mica"), None);
        let solid = Backing::Solid;
        assert_eq!(
            Material::Mica.window_background(solid),
            WindowBackgroundAppearance::MicaBackdrop
        );
        assert_eq!(
            Material::Acrylic.window_background(solid),
            WindowBackgroundAppearance::Blurred
        );
        assert_eq!(
            Material::Vibrancy.window_background(solid),
            WindowBackgroundAppearance::Transparent
        );
        assert_eq!(
            Material::Opaque.window_background(solid),
            WindowBackgroundAppearance::Opaque
        );
        assert_eq!(
            Material::Opaque.window_background(Backing::SeeThrough),
            WindowBackgroundAppearance::Transparent
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_spells_the_keys() {
        for material in Material::ALL {
            assert_eq!(
                serde_json::to_value(material).unwrap(),
                serde_json::json!(material.key())
            );
            assert_eq!(
                serde_json::from_value::<Material>(serde_json::json!(material.key())).unwrap(),
                material
            );
        }
    }

    #[test]
    fn the_platform_default_is_shown_where_the_platform_can() {
        let default = Material::platform_default();
        if cfg!(windows) {
            assert_eq!(default, Material::Mica);
        } else if cfg!(target_os = "macos") {
            assert_eq!(default, Material::Vibrancy);
        } else {
            assert_eq!(default, Material::Opaque);
        }
    }
}
