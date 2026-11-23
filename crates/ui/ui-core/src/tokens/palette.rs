//! What a theme document says about colour (`docs/design-language.md`, "Colour" and
//! "Theming"). A [`Palette`] is a temperature (one OKLCH hue and a chroma at or below
//! [`NEUTRAL_MAX_CHROMA`]), a lightness ladder for the six surfaces, three ink tiers and
//! four muted status tones. [`Tone`] sets the lightness and chroma of the presence
//! palette, whose twelve hues never move.

use serde::{Deserialize, Serialize};

/// Ceiling on the neutral material's chroma. The neutrals carry a temperature you can
/// name (slate, paper, moss, dusk), never a colour that competes with content.
pub const NEUTRAL_MAX_CHROMA: f32 = 0.032;

/// Ceiling on status chroma: danger, warning and success are muted.
pub const STATUS_MAX_CHROMA: f32 = 0.13;

/// Status hues, in OKLCH degrees: danger, warning, success, info. A theme moves their
/// lightness and chroma, never their meaning.
pub const STATUS_HUES: [f32; 4] = [25.0, 75.0, 150.0, 245.0];

/// OKLCH lightness of every surface, `0` black to `1` white.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceLadder {
    pub base: f32,
    pub content: f32,
    pub sidebar: f32,
    pub inspector: f32,
    pub raised: f32,
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
    pub dark: bool,
    #[serde(default)]
    pub high_contrast: bool,
    /// The OKLCH hue every neutral shares, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    /// The neutrals' chroma, `0` (pure grey) to [`NEUTRAL_MAX_CHROMA`]: how much
    /// temperature the material takes once it has a hue.
    pub chroma: f32,
    pub surfaces: SurfaceLadder,
    pub ink: InkLadder,
    pub status: StatusTones,
}

/// Which hue the accent takes: primary actions, the selected row's lift tint, links, the
/// focus ring, the caret and the text selection (`docs/design-language.md`, "Colour
/// roles"). An "on" state is never the accent; it is the on-state green
/// ([`crate::tokens::Intents::on_state`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Emphasis {
    #[default]
    You,
    Tinted {
        hue: f32,
    },
    Inverted,
}

/// Chroma of the selection lift under an accent ([`Emphasis::You`] and
/// [`Emphasis::Tinted`]).
pub const TINTED_LIFT_CHROMA: f32 = 0.035;

/// The light a surface catches: the specular sheen across a glass body and the rim lit
/// along its edge ([`crate::tokens::Colors::light`], read by the Glass painter set).
/// Daylight, at no chroma: light is not a shade of what it lands on, so a theme's or a
/// workspace's temperature never tints it, and it is white on every polarity.
pub const SURFACE_LIGHT: Tone = Tone::new(1.0, 0.0);

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

/// Tilo's own temperature: slate, cool enough to read as a colour of its own on dark
/// surfaces and as clean daylight on light ones.
pub const STOCK_HUE: f32 = 255.0;

/// The five levels of a dark material (`docs/design-language.md`, "Surfaces"): ground,
/// window, region (recessed, so sidebar and inspector alike), raised and floating. Each
/// level is a step the eye registers on its own; a child never paints its parent's level.
const fn dark_ladder(base: f32) -> SurfaceLadder {
    SurfaceLadder {
        base,
        content: base + 0.043,
        sidebar: base + 0.023,
        inspector: base + 0.023,
        raised: base + 0.087,
        overlay: base + 0.075,
    }
}

/// The light material: a tinted desktop under near-white windows, regions a step down and
/// wells a step further.
const fn light_ladder(base: f32, content: f32) -> SurfaceLadder {
    SurfaceLadder {
        base,
        content,
        sidebar: content - 0.024,
        inspector: content - 0.024,
        raised: content - 0.043,
        overlay: if content + 0.008 > 1.0 {
            1.0
        } else {
            content + 0.008
        },
    }
}

const DARK_INK: InkLadder = InkLadder {
    primary: 0.955,
    secondary: 0.775,
    tertiary: 0.665,
};

const LIGHT_INK: InkLadder = InkLadder {
    primary: 0.18,
    secondary: 0.43,
    tertiary: 0.555,
};

impl Palette {
    /// A dark material at `hue` and `chroma` whose ground is `base`.
    const fn dark_at(hue: f32, chroma: f32, base: f32) -> Self {
        Self {
            dark: true,
            high_contrast: false,
            hue: Some(hue),
            chroma,
            surfaces: dark_ladder(base),
            ink: DARK_INK,
            status: DARK_STATUS,
        }
    }

    /// A light material at `hue` and `chroma`: a desktop at `base` under windows at `content`.
    const fn light_at(hue: f32, chroma: f32, base: f32, content: f32) -> Self {
        Self {
            dark: false,
            high_contrast: false,
            hue: Some(hue),
            chroma,
            surfaces: light_ladder(base, content),
            ink: LIGHT_INK,
            status: LIGHT_STATUS,
        }
    }

    /// Tilo, dark: slate.
    #[must_use]
    pub const fn dark() -> Self {
        Self::dark_at(STOCK_HUE, 0.016, 0.175)
    }

    /// Tilo, light: clean daylight with a cool desktop.
    #[must_use]
    pub const fn light() -> Self {
        // No hue of its own: white paper, which a workspace's temperature tints.
        Self {
            hue: None,
            ..Self::light_at(STOCK_HUE, 0.008, 0.935, 0.995)
        }
    }

    /// Paper, dark: warm charcoal, like lamplight on a dark page.
    #[must_use]
    pub const fn paper_dark() -> Self {
        Self::dark_at(70.0, 0.014, 0.17)
    }

    /// Paper, light: warm stock, never cream.
    #[must_use]
    pub const fn paper_light() -> Self {
        Self {
            ink: InkLadder {
                primary: 0.235,
                secondary: 0.46,
                tertiary: 0.545,
            },
            ..Self::light_at(80.0, 0.011, 0.915, 0.988)
        }
    }

    /// Moss, dark: olive-black, the bark under moss. Yellow-green rather than green, so
    /// its ochre accent and the ochre selection on it are of one earthy family.
    #[must_use]
    pub const fn moss_dark() -> Self {
        Self::dark_at(125.0, 0.016, 0.165)
    }

    /// Moss, light: sage paper, an off-white that leans olive and never mint.
    #[must_use]
    pub const fn moss_light() -> Self {
        Self::light_at(125.0, 0.013, 0.92, 0.988)
    }

    /// Dusk, dark: deep indigo.
    #[must_use]
    pub const fn dusk_dark() -> Self {
        Self::dark_at(285.0, 0.030, 0.16)
    }

    /// Dusk, light: lilac grey.
    #[must_use]
    pub const fn dusk_light() -> Self {
        Self::light_at(295.0, 0.018, 0.92, 0.99)
    }

    /// Mono, dark: true greys, no temperature at all.
    #[must_use]
    pub const fn mono_dark() -> Self {
        Self {
            hue: None,
            chroma: 0.0,
            ink: InkLadder {
                primary: 0.97,
                secondary: 0.78,
                tertiary: 0.66,
            },
            ..Self::dark_at(0.0, 0.0, 0.13)
        }
    }

    /// Mono, light: white windows on a grey desktop, black ink.
    #[must_use]
    pub const fn mono_light() -> Self {
        Self {
            hue: None,
            chroma: 0.0,
            ink: InkLadder {
                primary: 0.15,
                secondary: 0.43,
                tertiary: 0.55,
            },
            ..Self::light_at(0.0, 0.0, 0.94, 1.0)
        }
    }

    /// Black ground under near-white ink: every text pairing at WCAG AAA.
    #[must_use]
    pub const fn high_contrast_dark() -> Self {
        Self {
            dark: true,
            high_contrast: true,
            hue: None,
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
            hue: None,
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

    /// The hue and chroma the neutrals are painted at: the palette's own when it has a
    /// hue, pure grey (chroma 0) until it has one.
    #[must_use]
    pub fn temperature(&self) -> (f32, f32) {
        match self.hue {
            Some(hue) if hue.is_finite() => (
                hue.rem_euclid(360.0),
                self.chroma.clamp(0.0, NEUTRAL_MAX_CHROMA),
            ),
            _ => (0.0, 0.0),
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

    /// How strongly a scrim dims what it covers (see `chrome::fill_scrim`).
    pub(crate) fn scrim_strength(&self) -> f32 {
        if self.dark && !self.high_contrast {
            0.6
        } else {
            0.45
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

    /// The neutral ramp's ends: `neutral[0]` toward the ground side, `neutral[10]` the
    /// primary ink.
    pub(crate) fn ramp(&self) -> (f32, f32) {
        if self.dark {
            ((self.surfaces.base - 0.06).max(0.0), self.ink.primary)
        } else {
            (1.0, self.ink.primary)
        }
    }

    /// Signed lightness step of a selection lift: one surface step away from the ground,
    /// lighter on dark palettes and darker on light ones.
    pub(crate) fn lift(&self) -> f32 {
        match (self.dark, self.high_contrast) {
            (true, false) => 0.10,
            (false, false) => -0.07,
            (true, true) => 0.30,
            (false, true) => -0.15,
        }
    }

    /// Structural check: finite numbers in range and a ladder whose ink sits on the right
    /// side of every surface. A static message naming the first field out of range.
    pub fn validate(&self) -> Result<(), &'static str> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if self.hue.is_some_and(|hue| !hue.is_finite()) {
            return Err("invalid palette hue");
        }
        if !self.chroma.is_finite() || !(0.0..=NEUTRAL_MAX_CHROMA).contains(&self.chroma) {
            return Err("palette chroma outside 0..=0.032");
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
    fn default() -> Self {
        Self::dark()
    }
}

impl Default for Tone {
    fn default() -> Self {
        Palette::dark().stock_presence()
    }
}

impl Emphasis {
    /// The emphasis a palette ships with: the person's colour as the accent, except on
    /// high contrast, where the accent is primary ink so every pairing holds at AAA.
    #[must_use]
    pub const fn stock_for(palette: &Palette) -> Self {
        if palette.high_contrast {
            Self::Inverted
        } else {
            Self::You
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Inverted | Self::You => Ok(()),
            Self::Tinted { hue } if hue.is_finite() => Ok(()),
            Self::Tinted { .. } => Err("invalid emphasis hue"),
        }
    }
}

/// The presence palette's usable band: bright enough to read as colour, never so loud
/// that one member shouts.
pub const PRESENCE_LIGHTNESS: std::ops::RangeInclusive<f32> = 0.35..=0.90;
/// See [`PRESENCE_LIGHTNESS`].
pub const PRESENCE_CHROMA: std::ops::RangeInclusive<f32> = 0.04..=0.20;

impl Tone {
    /// Structural check for a presence tone. A lightness or chroma outside the presence
    /// band.
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

    tilo_test_env::scenario! {
        /// `Palette` and `Emphasis`.
        /// Each phase was a test of its own and builds its own fixture.
        fn palettes_are_sound_or_refused_and_emphasis_round_trips;

        phase shipped_palettes_are_structurally_sound_and_follow_the_design_language;
        {
            for palette in [
                Palette::dark(),
                Palette::light(),
                Palette::paper_dark(),
                Palette::paper_light(),
                Palette::moss_dark(),
                Palette::moss_light(),
                Palette::dusk_dark(),
                Palette::dusk_light(),
                Palette::mono_dark(),
                Palette::mono_light(),
                Palette::high_contrast_dark(),
                Palette::high_contrast_light(),
            ] {
                assert_eq!(palette.validate(), Ok(()), "{palette:?}");
                assert_eq!(palette.stock_presence().validate_presence(), Ok(()));
                // Five levels the eye registers on its own: ground below region below
                // window below raised, and ink well clear of all of them.
                if !palette.high_contrast {
                    let l = palette.surfaces;
                    let order = if palette.dark {
                        [l.base, l.sidebar, l.content, l.raised]
                    } else {
                        [l.base, l.raised, l.sidebar, l.content]
                    };
                    assert!(order.windows(2).all(|w| w[0] < w[1]), "{palette:?}");
                    // A light menu is the window's white or whiter; it floats by its shadow.
                    assert!(palette.dark || l.overlay >= l.content, "{palette:?}");
                }
            }
            // Tilo's temperature: slate, on both polarities; a workspace moves its hue,
            // never its chroma.
            assert_eq!(Palette::dark().hue, Some(STOCK_HUE));
            assert_eq!(Palette::light().hue, None, "white paper until a workspace tints it");
            assert_eq!(Palette::mono_dark().temperature(), (0.0, 0.0));
            let grey = Palette {
                hue: None,
                ..Palette::light()
            };
            assert_eq!(
                grey.temperature(),
                (0.0, 0.0),
                "without a hue the ladder is pure grey"
            );
            let warm = Palette {
                hue: Some(400.0),
                chroma: 0.010,
                ..Palette::light()
            };
            assert_eq!(warm.temperature(), (40.0, 0.010));
        }

        phase a_palette_out_of_band_or_with_crossed_ink_is_refused;
        {
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

        phase emphasis_round_trips_as_tagged_json;
        {
            let tinted = serde_json::to_value(Emphasis::Tinted { hue: 150.0 }).unwrap();
            assert_eq!(tinted, serde_json::json!({"kind": "tinted", "hue": 150.0}));
            let back: Emphasis =
                serde_json::from_value(serde_json::json!({"kind": "inverted"})).unwrap();
            assert_eq!(back, Emphasis::Inverted);
            let you: Emphasis = serde_json::from_value(serde_json::json!({"kind": "you"})).unwrap();
            assert_eq!(you, Emphasis::You);
            assert_eq!(
                Emphasis::default(),
                Emphasis::You,
                "the accent is your colour"
            );
        }
    }
}
