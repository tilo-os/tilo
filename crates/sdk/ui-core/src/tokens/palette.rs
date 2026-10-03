//! What a theme document says about colour (`docs/design-language.md`,
//! "Colour" and "Theming").
//!
//! A [`Palette`] is a temperature (one OKLCH hue and a chroma at or below
//! [`NEUTRAL_MAX_CHROMA`]), a lightness ladder for the six surfaces, three
//! ink tiers and four muted status tones. [`Emphasis`] decides how primary
//! actions, selection and "on" states show. [`Tone`] sets the lightness and
//! chroma of the presence palette, whose twelve hues never move. Everything
//! else ([`crate::tokens::Colors`]) is derived from these numbers in one
//! place, so a palette can only say what the design language lets it say.

use serde::{Deserialize, Serialize};

/// Ceiling on the neutral material's chroma. The neutrals carry a
/// temperature, never a colour.
pub const NEUTRAL_MAX_CHROMA: f32 = 0.010;

/// Ceiling on status chroma: danger, warning and success are muted.
pub const STATUS_MAX_CHROMA: f32 = 0.13;

/// Temperature of a tinted palette when nothing chose one: a cool blue-grey.
/// A workspace's wallpaper replaces it while that workspace is shown.
pub const DEFAULT_NEUTRAL_HUE: f32 = 255.0;

/// Status hues, in OKLCH degrees: danger, warning, success, info. A theme
/// moves their lightness and chroma, never their meaning.
pub const STATUS_HUES: [f32; 4] = [25.0, 75.0, 150.0, 245.0];

/// OKLCH lightness of every surface, `0` black to `1` white.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLadder {
    /// The desktop ground behind tiles.
    pub base: f32,
    /// The window ground every app paints on.
    pub content: f32,
    /// The navigation region of a window.
    pub sidebar: f32,
    /// The detail / inspector region of a window.
    pub inspector: f32,
    /// Fields, wells and objects inside content.
    pub raised: f32,
    /// Floating chrome: menus, popovers, the palette, sheets.
    pub overlay: f32,
}

impl SurfaceLadder {
    /// Every rung, in declaration order, named as the docs spell them.
    #[must_use]
    pub fn rungs(&self) -> [(&'static str, f32); 6] {
        [
            ("base", self.base),
            ("content", self.content),
            ("sidebar", self.sidebar),
            ("inspector", self.inspector),
            ("raised", self.raised),
            ("overlay", self.overlay),
        ]
    }
}

/// OKLCH lightness of the three ink tiers, defined on the content surface.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InkLadder {
    pub primary: f32,
    pub secondary: f32,
    pub tertiary: f32,
}

/// An OKLCH lightness and chroma at a hue the role fixes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tone {
    pub lightness: f32,
    pub chroma: f32,
}

impl Tone {
    #[must_use]
    pub const fn new(lightness: f32, chroma: f32) -> Self {
        Self { lightness, chroma }
    }
}

/// The four status tones. Their hues are [`STATUS_HUES`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusTones {
    pub danger: Tone,
    pub warning: Tone,
    pub success: Tone,
    pub info: Tone,
}

impl StatusTones {
    /// Danger, warning, success, info: the order of [`STATUS_HUES`].
    #[must_use]
    pub fn in_hue_order(&self) -> [Tone; 4] {
        [self.danger, self.warning, self.success, self.info]
    }
}

/// The neutral material and its inks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    /// Ink is light on these surfaces.
    pub dark: bool,
    /// Every text pairing holds WCAG AAA, floating chrome is opaque, and
    /// content surfaces are asked to force their colours.
    #[serde(default)]
    pub high_contrast: bool,
    /// The OKLCH hue every neutral shares, in degrees.
    pub hue: f32,
    /// The neutrals' chroma, `0` (pure grey) to [`NEUTRAL_MAX_CHROMA`]. At
    /// `0` a workspace's temperature has nothing to tint.
    pub chroma: f32,
    pub surfaces: SurfaceLadder,
    pub ink: InkLadder,
    pub status: StatusTones,
}

/// How primary actions, selection and "on" states show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Emphasis {
    /// A primary action is primary ink with the content surface as its
    /// label; a selection is a colourless lift. The default.
    #[default]
    Inverted,
    /// One hue for people who want an accent: primary actions, switches
    /// that are on and the selection lift take it.
    Tinted { hue: f32 },
}

/// Chroma of the selection lift under [`Emphasis::Tinted`].
pub const TINTED_LIFT_CHROMA: f32 = 0.035;

const DARK_STATUS: StatusTones = StatusTones {
    danger: Tone::new(0.70, 0.13),
    warning: Tone::new(0.80, 0.12),
    success: Tone::new(0.74, 0.12),
    info: Tone::new(0.72, 0.11),
};

const LIGHT_STATUS: StatusTones = StatusTones {
    danger: Tone::new(0.52, 0.13),
    warning: Tone::new(0.56, 0.12),
    success: Tone::new(0.52, 0.12),
    info: Tone::new(0.52, 0.11),
};

const DARK_LADDER: SurfaceLadder = SurfaceLadder {
    base: 0.16,
    content: 0.215,
    sidebar: 0.195,
    inspector: 0.235,
    raised: 0.265,
    overlay: 0.285,
};

const DARK_INK: InkLadder = InkLadder {
    primary: 0.96,
    secondary: 0.76,
    tertiary: 0.62,
};

impl Palette {
    /// The default: the dark ladder in pure neutral grey.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            dark: true,
            high_contrast: false,
            hue: DEFAULT_NEUTRAL_HUE,
            chroma: 0.0,
            surfaces: DARK_LADDER,
            ink: DARK_INK,
            status: DARK_STATUS,
        }
    }

    /// The dark ladder, cool.
    #[must_use]
    pub const fn dark_cool() -> Self {
        Self {
            hue: 255.0,
            chroma: 0.008,
            ..Self::dark()
        }
    }

    /// The dark ladder, warm graphite.
    #[must_use]
    pub const fn dark_graphite() -> Self {
        Self {
            hue: 60.0,
            chroma: 0.006,
            ..Self::dark()
        }
    }

    /// A lifted dark ladder, faintly violet: lower contrast between ground
    /// and ink for long sessions. The tertiary tier rises with the surfaces
    /// so it still reads as body text on content.
    #[must_use]
    pub const fn dark_soft() -> Self {
        Self {
            hue: 270.0,
            chroma: 0.003,
            surfaces: SurfaceLadder {
                base: 0.19,
                content: 0.25,
                sidebar: 0.228,
                inspector: 0.27,
                raised: 0.30,
                overlay: 0.32,
            },
            ink: InkLadder {
                primary: 0.96,
                secondary: 0.78,
                tertiary: 0.65,
            },
            ..Self::dark()
        }
    }

    /// The light ladder. It carries chroma, so it takes each workspace's
    /// temperature.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            dark: false,
            high_contrast: false,
            hue: DEFAULT_NEUTRAL_HUE,
            chroma: NEUTRAL_MAX_CHROMA,
            surfaces: SurfaceLadder {
                base: 0.90,
                content: 0.995,
                sidebar: 0.965,
                inspector: 0.975,
                raised: 1.0,
                overlay: 1.0,
            },
            ink: InkLadder {
                primary: 0.18,
                secondary: 0.43,
                tertiary: 0.56,
            },
            status: LIGHT_STATUS,
        }
    }

    /// Black ground under near-white ink: every text pairing at WCAG AAA.
    #[must_use]
    pub const fn high_contrast_dark() -> Self {
        Self {
            dark: true,
            high_contrast: true,
            hue: DEFAULT_NEUTRAL_HUE,
            chroma: 0.0,
            surfaces: SurfaceLadder {
                base: 0.0,
                content: 0.0,
                sidebar: 0.0,
                inspector: 0.0,
                raised: 0.0,
                overlay: 0.0,
            },
            ink: InkLadder {
                primary: 1.0,
                secondary: 0.94,
                tertiary: 0.88,
            },
            status: StatusTones {
                danger: Tone::new(0.80, 0.13),
                warning: Tone::new(0.86, 0.13),
                success: Tone::new(0.84, 0.13),
                info: Tone::new(0.82, 0.12),
            },
        }
    }

    /// White ground under black ink: every text pairing at WCAG AAA.
    #[must_use]
    pub const fn high_contrast_light() -> Self {
        Self {
            dark: false,
            high_contrast: true,
            hue: DEFAULT_NEUTRAL_HUE,
            chroma: 0.0,
            surfaces: SurfaceLadder {
                base: 1.0,
                content: 1.0,
                sidebar: 1.0,
                inspector: 1.0,
                raised: 1.0,
                overlay: 1.0,
            },
            ink: InkLadder {
                primary: 0.0,
                secondary: 0.30,
                tertiary: 0.40,
            },
            status: StatusTones {
                danger: Tone::new(0.44, 0.13),
                warning: Tone::new(0.46, 0.11),
                success: Tone::new(0.44, 0.12),
                info: Tone::new(0.44, 0.12),
            },
        }
    }

    /// The high-contrast palette of this palette's polarity.
    #[must_use]
    pub const fn high_contrast_of(&self) -> Self {
        if self.dark {
            Self::high_contrast_dark()
        } else {
            Self::high_contrast_light()
        }
    }

    /// The presence tone this polarity ships with.
    #[must_use]
    pub const fn stock_presence(&self) -> Tone {
        match (self.dark, self.high_contrast) {
            (true, false) => Tone::new(0.74, 0.14),
            (false, false) => Tone::new(0.58, 0.15),
            (true, true) => Tone::new(0.82, 0.13),
            (false, true) => Tone::new(0.56, 0.15),
        }
    }

    /// Lightness of the modal scrim.
    pub(crate) fn scrim(&self) -> f32 {
        if self.high_contrast {
            0.0
        } else if self.dark {
            self.surfaces.base * 0.5
        } else {
            0.18
        }
    }

    /// Lightness of the hairline between two equal surfaces.
    pub(crate) fn border(&self) -> f32 {
        match (self.dark, self.high_contrast) {
            (true, true) => 1.0,
            (false, true) => 0.0,
            (true, false) => (self.surfaces.raised + 0.065).min(1.0),
            (false, false) => (self.surfaces.base - 0.02).max(0.0),
        }
    }

    /// The neutral ramp's ends: `neutral[0]` toward the ground side,
    /// `neutral[10]` the primary ink.
    pub(crate) fn ramp(&self) -> (f32, f32) {
        if self.dark {
            ((self.surfaces.base - 0.06).max(0.0), self.ink.primary)
        } else {
            (1.0, self.ink.primary)
        }
    }

    /// Signed lightness step of a selection lift: one surface step away
    /// from the ground, lighter on dark palettes and darker on light ones.
    pub(crate) fn lift(&self) -> f32 {
        match (self.dark, self.high_contrast) {
            (true, false) => 0.10,
            (false, false) => -0.09,
            (true, true) => 0.30,
            (false, true) => -0.15,
        }
    }

    /// Structural check: finite numbers in range and a ladder whose ink sits
    /// on the right side of every surface. Legibility is checked on the
    /// materialized colours (`crate::tokens::contrast::audit`).
    ///
    /// # Errors
    ///
    /// A static message naming the first field out of range.
    pub fn validate(&self) -> Result<(), &'static str> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !self.hue.is_finite() {
            return Err("invalid palette hue");
        }
        if !self.chroma.is_finite() || !(0.0..=NEUTRAL_MAX_CHROMA).contains(&self.chroma) {
            return Err("palette chroma outside 0..=0.010");
        }
        if !self.surfaces.rungs().iter().all(|(_, l)| unit(*l)) {
            return Err("invalid surface lightness");
        }
        let ink = [self.ink.primary, self.ink.secondary, self.ink.tertiary];
        if !ink.iter().all(|l| unit(*l)) {
            return Err("invalid ink lightness");
        }
        let on_the_ink_side = |l: f32| {
            self.surfaces
                .rungs()
                .iter()
                .all(|(_, s)| if self.dark { l > *s } else { l < *s })
        };
        if !ink.iter().all(|l| on_the_ink_side(*l)) {
            return Err(
                "ink must be lighter than every surface on a dark palette and darker on a light one",
            );
        }
        for tone in self.status.in_hue_order() {
            if !unit(tone.lightness)
                || !tone.chroma.is_finite()
                || !(0.0..=STATUS_MAX_CHROMA).contains(&tone.chroma)
            {
                return Err("invalid status tone");
            }
        }
        Ok(())
    }
}

impl Default for Palette {
    /// The default theme's palette: pure neutral dark.
    fn default() -> Self {
        Self::dark()
    }
}

impl Default for Tone {
    /// The default theme's presence tone.
    fn default() -> Self {
        Palette::dark().stock_presence()
    }
}

impl Emphasis {
    /// # Errors
    ///
    /// A non-finite tint hue.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Inverted => Ok(()),
            Self::Tinted { hue } if hue.is_finite() => Ok(()),
            Self::Tinted { .. } => Err("invalid emphasis hue"),
        }
    }
}

/// The presence palette's usable band: bright enough to read as colour,
/// never so loud that one participant shouts.
pub const PRESENCE_LIGHTNESS: std::ops::RangeInclusive<f32> = 0.35..=0.90;
/// See [`PRESENCE_LIGHTNESS`].
pub const PRESENCE_CHROMA: std::ops::RangeInclusive<f32> = 0.04..=0.20;

impl Tone {
    /// Structural check for a presence tone.
    ///
    /// # Errors
    ///
    /// A lightness or chroma outside the presence band.
    pub fn validate_presence(&self) -> Result<(), &'static str> {
        if !self.lightness.is_finite() || !PRESENCE_LIGHTNESS.contains(&self.lightness) {
            return Err("presence lightness outside 0.35..=0.90");
        }
        if !self.chroma.is_finite() || !PRESENCE_CHROMA.contains(&self.chroma) {
            return Err("presence chroma outside 0.04..=0.20");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_palettes_are_structurally_sound_and_follow_the_design_language() {
        for palette in [
            Palette::dark(),
            Palette::dark_cool(),
            Palette::dark_graphite(),
            Palette::dark_soft(),
            Palette::light(),
            Palette::high_contrast_dark(),
            Palette::high_contrast_light(),
        ] {
            assert_eq!(palette.validate(), Ok(()), "{palette:?}");
            assert_eq!(palette.stock_presence().validate_presence(), Ok(()));
        }
        assert!(
            Palette::dark().chroma.abs() < f32::EPSILON,
            "dark is neutral"
        );
        assert!((Palette::light().chroma - 0.010).abs() < f32::EPSILON);
        let soft = Palette::dark_soft().surfaces;
        assert_eq!(
            [
                soft.base,
                soft.content,
                soft.sidebar,
                soft.raised,
                soft.overlay
            ],
            [0.19, 0.25, 0.228, 0.30, 0.32]
        );
    }

    #[test]
    fn a_palette_out_of_band_or_with_crossed_ink_is_refused() {
        let mut loud = Palette::dark();
        loud.chroma = 0.05;
        assert!(loud.validate().is_err());
        let mut crossed = Palette::dark();
        crossed.ink.tertiary = 0.2;
        assert!(crossed.validate().is_err());
        let mut status = Palette::light();
        status.status.danger.chroma = 0.3;
        assert!(status.validate().is_err());
        assert!(Emphasis::Tinted { hue: f32::NAN }.validate().is_err());
        assert!(Tone::new(0.95, 0.14).validate_presence().is_err());
    }

    #[test]
    fn emphasis_round_trips_as_tagged_json() {
        let tinted = serde_json::to_value(Emphasis::Tinted { hue: 150.0 }).unwrap();
        assert_eq!(tinted, serde_json::json!({"kind": "tinted", "hue": 150.0}));
        let back: Emphasis =
            serde_json::from_value(serde_json::json!({"kind": "inverted"})).unwrap();
        assert_eq!(back, Emphasis::Inverted);
    }
}
