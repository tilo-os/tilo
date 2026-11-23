//! The names a theme is chosen by: the looks Tilo ships, each designed for light and dark, the
//! presets those make, and the spacing scale a person picks. The palettes and documents they stand
//! for are the toolkit's.

use serde::{Deserialize, Serialize};

/// `appearance` key: the custom theme documents a `custom:<id>` names.
pub const CUSTOM_THEMES_KEY: &str = "custom_themes";

/// The spacing scale, as a multiple of the stock 4 px scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDensity {
    pub spacing: f32,
}

impl ThemeDensity {
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=2.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 0.75..=1.5;
    /// The compact density Settings offers.
    pub const COMPACT: f32 = 0.85;
    pub const REGULAR: f32 = 1.0;
    /// The large density Settings offers.
    pub const LARGE: f32 = 1.2;
}

/// A look Tilo ships: one temperature designed twice, for light and for dark, with the
/// accent it comes with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    /// Slate; the accent is the person's own colour.
    Tilo,
    /// Warm stock and charcoal, with a fountain-pen blue.
    Paper,
    /// Sage and forest night, with amber.
    Moss,
    /// Lilac and deep indigo, with rose.
    Dusk,
    /// True greys, black and white for emphasis, no accent.
    Mono,
}

impl Look {
    pub const ALL: [Self; 5] = [Self::Tilo, Self::Paper, Self::Moss, Self::Dusk, Self::Mono];

    /// The stored id of the look (`tilo`, `paper`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Tilo => "tilo",
            Self::Paper => "paper",
            Self::Moss => "moss",
            Self::Dusk => "dusk",
            Self::Mono => "mono",
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tilo => "Tilo",
            Self::Paper => "Paper",
            Self::Moss => "Moss",
            Self::Dusk => "Dusk",
            Self::Mono => "Mono",
        }
    }

    /// The preset of this look for the polarity `dark` names.
    #[must_use]
    pub const fn preset(self, dark: bool) -> Preset {
        match (self, dark) {
            (Self::Tilo, true) => Preset::Dark,
            (Self::Tilo, false) => Preset::Light,
            (Self::Paper, true) => Preset::PaperDark,
            (Self::Paper, false) => Preset::PaperLight,
            (Self::Moss, true) => Preset::MossDark,
            (Self::Moss, false) => Preset::MossLight,
            (Self::Dusk, true) => Preset::DuskDark,
            (Self::Dusk, false) => Preset::DuskLight,
            (Self::Mono, true) => Preset::MonoDark,
            (Self::Mono, false) => Preset::MonoLight,
        }
    }

    /// A look by id or name, loosely: case and punctuation are ignored.
    #[must_use]
    pub fn find(key: &str) -> Option<Self> {
        let key: String = key
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        Self::ALL.into_iter().find(|look| look.id() == key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Tilo, dark: the default.
    Dark,
    /// Tilo, light.
    Light,
    PaperDark,
    PaperLight,
    MossDark,
    MossLight,
    DuskDark,
    DuskLight,
    MonoDark,
    MonoLight,
    HighContrast,
    HighContrastLight,
}

impl Preset {
    pub const ALL: [Self; 12] = [
        Self::Dark,
        Self::Light,
        Self::PaperDark,
        Self::PaperLight,
        Self::MossDark,
        Self::MossLight,
        Self::DuskDark,
        Self::DuskLight,
        Self::MonoDark,
        Self::MonoLight,
        Self::HighContrast,
        Self::HighContrastLight,
    ];

    /// The stored id (`appearance.dark_theme`, `appearance.light_theme`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Dark => "tilo-dark",
            Self::Light => "tilo-light",
            Self::PaperDark => "paper-dark",
            Self::PaperLight => "paper-light",
            Self::MossDark => "moss-dark",
            Self::MossLight => "moss-light",
            Self::DuskDark => "dusk-dark",
            Self::DuskLight => "dusk-light",
            Self::MonoDark => "mono-dark",
            Self::MonoLight => "mono-light",
            Self::HighContrast => "high-contrast",
            Self::HighContrastLight => "high-contrast-light",
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dark => "Tilo dark",
            Self::Light => "Tilo light",
            Self::PaperDark => "Paper dark",
            Self::PaperLight => "Paper light",
            Self::MossDark => "Moss dark",
            Self::MossLight => "Moss light",
            Self::DuskDark => "Dusk dark",
            Self::DuskLight => "Dusk light",
            Self::MonoDark => "Mono dark",
            Self::MonoLight => "Mono light",
            Self::HighContrast => "High contrast",
            Self::HighContrastLight => "High contrast light",
        }
    }

    /// The look this preset is one side of; high contrast belongs to none.
    #[must_use]
    pub const fn look(self) -> Option<Look> {
        Some(match self {
            Self::Dark | Self::Light => Look::Tilo,
            Self::PaperDark | Self::PaperLight => Look::Paper,
            Self::MossDark | Self::MossLight => Look::Moss,
            Self::DuskDark | Self::DuskLight => Look::Dusk,
            Self::MonoDark | Self::MonoLight => Look::Mono,
            Self::HighContrast | Self::HighContrastLight => return None,
        })
    }

    /// A preset by id or name, loosely (`"paper-light"`, `"Paper light"`); a look's name
    /// alone (`"paper"`) is its dark side, and `dark` and `light` are Tilo's.
    #[must_use]
    pub fn find(key: &str) -> Option<Self> {
        let key: String = key
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if let Some(preset) = Self::ALL.into_iter().find(|preset| {
            preset
                .id()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .eq(key.chars())
        }) {
            return Some(preset);
        }
        Some(match key.as_str() {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "highcontrast" | "contrast" | "highcontrastdark" => Self::HighContrast,
            other => Look::find(other)?.preset(true),
        })
    }
}
