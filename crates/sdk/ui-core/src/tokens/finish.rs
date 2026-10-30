//! Finishes: what each surface role is made of, how its edges are drawn
//! and how it stands off what is below it (`docs/architecture/theming.md`,
//! "Material, shape and depth").
//!
//! A theme document sets one [`Finish`] per surface role
//! ([`RoleFinishes`]: the window ground, raised controls, floating chrome
//! and bars), one [`Border`] and one [`Depth`]. The toolkit paints every
//! surface of a role through the role's finish (`crate::surface`,
//! `crate::backdrop`), so a theme makes everything flat and blocky or
//! everything glass without an app or widget knowing.
//!
//! Legibility never depends on a finish: whatever lies below a translucent
//! or glass surface is held inside a band around the role's ground before
//! the tint is laid over it (`crate::backdrop::material_band`), so ink keeps
//! the contrast it has on the opaque ground over the worst backdrop
//! (`crate::theme::ThemeDocument::findings`).

use serde::{Deserialize, Serialize};

/// What one surface role is made of: the painter set that draws it and that
/// set's parameters.
///
/// In a document a role names its painter set (`"painter"`) beside the
/// set's own parameters, so the same field names a built-in set today and an
/// installed one later (`docs/architecture/theming.md`, "Part painters"):
///
/// | Document | Finish |
/// |---|---|
/// | `{ "painter": "block" }` | [`Finish::Flat`] |
/// | `{ "painter": "default" }` | [`Finish::Solid`] |
/// | `{ "painter": "default", "blur": 24, "tint": 0.72, "saturation": 1 }` | [`Finish::Translucent`] |
/// | `{ "painter": "glass", "blur": …, "tint": …, "saturation": …, "refraction": …, "edge_highlight": …, "specular": … }` | [`Finish::Glass`] |
///
/// A set this build does not have is not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "PainterRef", into = "PainterRef")]
pub enum Finish {
    /// The role's ground, opaque, with no edge light and no hairline of
    /// its own: a printed surface. Borders come from [`Border`] alone.
    Flat,
    /// The role's ground, opaque, with the hairline the design language
    /// gives the role.
    Solid,
    /// What lies below, blurred by `blur` logical pixels and its colour
    /// scaled by `saturation`, under the role's ground at `tint` opacity.
    Translucent {
        blur: f32,
        tint: f32,
        saturation: f32,
    },
    /// [`Self::Translucent`], with the backdrop bent at the edges like a
    /// thick lens (`refraction`, logical pixels of displacement at the rim),
    /// a lit rim along the top edge (`edge_highlight`, its opacity) and a
    /// soft sheen across the upper body (`specular`, its opacity).
    Glass {
        blur: f32,
        tint: f32,
        saturation: f32,
        refraction: f32,
        edge_highlight: f32,
        specular: f32,
    },
}

/// The built-in painter sets a role can name, with their parameters, as a
/// document spells them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "painter", rename_all = "snake_case", deny_unknown_fields)]
enum PainterRef {
    /// Flat, square-edged, borders only: the Block set.
    Block {},
    /// The design language's painters: opaque, or translucent when given a
    /// blur or a tint.
    Default {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blur: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saturation: Option<f32>,
    },
    /// The Glass set.
    Glass {
        blur: f32,
        tint: f32,
        saturation: f32,
        refraction: f32,
        edge_highlight: f32,
        specular: f32,
    },
}

impl From<PainterRef> for Finish {
    fn from(painter: PainterRef) -> Self {
        match painter {
            PainterRef::Block {} => Self::Flat,
            PainterRef::Default {
                blur: None,
                tint: None,
                saturation: None,
            } => Self::Solid,
            PainterRef::Default {
                blur,
                tint,
                saturation,
            } => Self::Translucent {
                blur: blur.unwrap_or(0.0),
                tint: tint.unwrap_or(1.0),
                saturation: saturation.unwrap_or(1.0),
            },
            PainterRef::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            } => Self::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            },
        }
    }
}

impl From<Finish> for PainterRef {
    fn from(finish: Finish) -> Self {
        match finish {
            Finish::Flat => Self::Block {},
            Finish::Solid => Self::Default {
                blur: None,
                tint: None,
                saturation: None,
            },
            Finish::Translucent {
                blur,
                tint,
                saturation,
            } => Self::Default {
                blur: Some(blur),
                tint: Some(tint),
                saturation: Some(saturation),
            },
            Finish::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            } => Self::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            },
        }
    }
}

impl Finish {
    /// The painter set that draws this finish, as a document names it.
    #[must_use]
    pub const fn painter_set(self) -> &'static str {
        match self {
            Self::Flat => "block",
            Self::Solid | Self::Translucent { .. } => "default",
            Self::Glass { .. } => "glass",
        }
    }

    /// Blur band a document may state, logical pixels.
    pub const BLUR_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=64.0;
    /// Blur band a theme is built at.
    pub const BLUR_USABLE: std::ops::RangeInclusive<f32> = 0.0..=48.0;
    /// Tint band: any. Legibility is the backdrop band's, not the tint's.
    pub const TINT_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=1.0;
    /// Saturation band: `0` grey, `1` as it is, `2` twice as vivid.
    pub const SATURATION_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;
    /// Refraction band, logical pixels of displacement at the rim.
    pub const REFRACTION_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=32.0;
    /// Edge highlight and specular band: past half they read as a second
    /// surface, not light on this one.
    pub const LIGHT_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.5;

    /// The stock floating material: the design language's blurred panel.
    pub const STOCK_FLOATING: Self = Self::Translucent {
        blur: crate::tokens::colors::MATERIAL_BLUR_PX,
        tint: crate::tokens::colors::MATERIAL_ALPHA,
        saturation: 1.0,
    };

    /// Whether the role lets what is below it through.
    #[must_use]
    pub const fn translucent(self) -> bool {
        matches!(self, Self::Translucent { .. } | Self::Glass { .. })
    }

    /// The backdrop blur, logical pixels; `0` for an opaque finish.
    #[must_use]
    pub const fn blur(self) -> f32 {
        match self {
            Self::Translucent { blur, .. } | Self::Glass { blur, .. } => blur,
            Self::Flat | Self::Solid => 0.0,
        }
    }

    /// The tint's opacity over the backdrop; `1` for an opaque finish.
    #[must_use]
    pub const fn tint(self) -> f32 {
        match self {
            Self::Translucent { tint, .. } | Self::Glass { tint, .. } => tint,
            Self::Flat | Self::Solid => 1.0,
        }
    }

    /// The backdrop's saturation; `1` for an opaque finish.
    #[must_use]
    pub const fn saturation(self) -> f32 {
        match self {
            Self::Translucent { saturation, .. } | Self::Glass { saturation, .. } => saturation,
            Self::Flat | Self::Solid => 1.0,
        }
    }

    /// How light bends through the body and catches on it, for glass.
    #[must_use]
    pub const fn optics(self) -> Optics {
        match self {
            Self::Glass {
                refraction,
                edge_highlight,
                specular,
                saturation,
                ..
            } => Optics {
                refraction,
                edge_highlight,
                specular,
                saturation,
            },
            Self::Translucent { saturation, .. } => Optics {
                saturation,
                ..Optics::NONE
            },
            Self::Flat | Self::Solid => Optics::NONE,
        }
    }

    /// Whether the role draws the hairline the design language gives it.
    #[must_use]
    pub const fn hairline(self) -> bool {
        !matches!(self, Self::Flat)
    }

    /// The finish actually painted where nothing may be translucent
    /// (reduced transparency, forced colours, high contrast): solid, or
    /// flat if it was flat.
    #[must_use]
    pub const fn opaque(self) -> Self {
        match self {
            Self::Flat => Self::Flat,
            _ => Self::Solid,
        }
    }

    /// Structural check: every number finite and in its band.
    pub fn validate(self) -> Result<(), &'static str> {
        let within = |value: f32, band: &std::ops::RangeInclusive<f32>| {
            value.is_finite() && band.contains(&value)
        };
        match self {
            Self::Flat | Self::Solid => Ok(()),
            Self::Translucent {
                blur,
                tint,
                saturation,
            } => {
                if !within(blur, &Self::BLUR_ADMITTED) {
                    return Err("material blur outside 0..=64");
                }
                if !within(tint, &Self::TINT_ADMITTED) {
                    return Err("material tint outside 0..=1");
                }
                if !within(saturation, &Self::SATURATION_ADMITTED) {
                    return Err("material saturation outside 0..=2");
                }
                Ok(())
            }
            Self::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            } => {
                Self::Translucent {
                    blur,
                    tint,
                    saturation,
                }
                .validate()?;
                if !within(refraction, &Self::REFRACTION_ADMITTED) {
                    return Err("glass refraction outside 0..=32");
                }
                if !within(edge_highlight, &Self::LIGHT_ADMITTED) {
                    return Err("glass edge highlight outside 0..=0.5");
                }
                if !within(specular, &Self::LIGHT_ADMITTED) {
                    return Err("glass specular outside 0..=0.5");
                }
                Ok(())
            }
        }
    }

    /// This finish with its blur held to the usable band.
    #[must_use]
    pub fn usable(self) -> Self {
        let clamp = |blur: f32| blur.clamp(*Self::BLUR_USABLE.start(), *Self::BLUR_USABLE.end());
        match self {
            Self::Translucent {
                blur,
                tint,
                saturation,
            } => Self::Translucent {
                blur: clamp(blur),
                tint,
                saturation,
            },
            Self::Glass {
                blur,
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            } => Self::Glass {
                blur: clamp(blur),
                tint,
                saturation,
                refraction,
                edge_highlight,
                specular,
            },
            other => other,
        }
    }
}

/// How a glass body treats the light through it and on it. All zero (and
/// saturation one) for every other finish, so a painter applies it
/// unconditionally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Optics {
    /// Displacement of the backdrop at the rim, logical pixels; it eases to
    /// nothing a rim's width in.
    pub refraction: f32,
    /// Opacity of the lit rim along the top edge.
    pub edge_highlight: f32,
    /// Opacity of the sheen across the upper body.
    pub specular: f32,
    /// The backdrop's saturation.
    pub saturation: f32,
}

impl Optics {
    pub const NONE: Self = Self {
        refraction: 0.0,
        edge_highlight: 0.0,
        specular: 0.0,
        saturation: 1.0,
    };

    /// Whether the backdrop pass has anything to do beyond the blur.
    #[must_use]
    pub fn bends(&self) -> bool {
        self.refraction > 0.0 || (self.saturation - 1.0).abs() > f32::EPSILON
    }
}

/// The surface roles a theme finishes. Each is a level of the design
/// language (`docs/design-language.md`, "Surfaces").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceRole {
    /// The window ground every app paints on, and its regions.
    Window,
    /// A raised control: a button's face, a field's well, a segmented
    /// track, an object's card.
    Control,
    /// Floating chrome: menus, popovers, the palette, sheets, Quick
    /// Settings, notifications, tooltips.
    Floating,
    /// A bar: the top bar and the window group's tab strip.
    Bar,
}

/// One finish per surface role: the theme document's `material` role
/// (`crate::theme::ThemeMaterial`) as the toolkit paints it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoleFinishes {
    /// The window ground. Windows never overlap, so a translucent window
    /// shows the desktop behind it.
    pub window: Finish,
    /// Raised controls, over the window ground.
    pub control: Finish,
    /// Floating chrome.
    pub floating: Finish,
    /// The top bar's body. `None`: the bar has no body of its own and
    /// stands on the desktop, as the stock bar does.
    pub bar: Option<Finish>,
}

impl RoleFinishes {
    /// The design language's finishes: opaque windows and controls, the
    /// blurred floating material, a bar with no body.
    pub const STOCK: Self = Self {
        window: Finish::Solid,
        control: Finish::Solid,
        floating: Finish::STOCK_FLOATING,
        bar: None,
    };

    /// Everything opaque: high contrast and forced colours.
    pub const OPAQUE: Self = Self {
        window: Finish::Solid,
        control: Finish::Solid,
        floating: Finish::Solid,
        bar: None,
    };

    /// The finish of `role`; a bar without a body reads as flat.
    #[must_use]
    pub const fn of(&self, role: SurfaceRole) -> Finish {
        match role {
            SurfaceRole::Window => self.window,
            SurfaceRole::Control => self.control,
            SurfaceRole::Floating => self.floating,
            SurfaceRole::Bar => match self.bar {
                Some(finish) => finish,
                None => Finish::Flat,
            },
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.window.validate()?;
        self.control.validate()?;
        self.floating.validate()?;
        if let Some(bar) = self.bar {
            bar.validate()?;
        }
        Ok(())
    }

    /// These finishes with every blur held to the usable band.
    #[must_use]
    pub fn usable(self) -> Self {
        Self {
            window: self.window.usable(),
            control: self.control.usable(),
            floating: self.floating.usable(),
            bar: self.bar.map(Finish::usable),
        }
    }

    /// These finishes where nothing may be translucent.
    #[must_use]
    pub fn opaque(self) -> Self {
        Self {
            window: self.window.opaque(),
            control: self.control.opaque(),
            floating: self.floating.opaque(),
            bar: self.bar.map(Finish::opaque),
        }
    }
}

impl Default for RoleFinishes {
    fn default() -> Self {
        Self::STOCK
    }
}

/// Which ink a border is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderInk {
    /// The stock edges: each surface keeps the hairline the design language
    /// gives it (a window's edge, a floating panel's inner hairline, a
    /// card's rest edge) and controls draw none.
    Hairline,
    /// Every surface of every role outlined in `border_standard`.
    Standard,
    /// Every surface of every role outlined in the strong ink: the tertiary
    /// text tier, which clears 3:1 on every surface (WCAG 1.4.11).
    Strong,
}

/// How surfaces are outlined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Border {
    /// Width of an outline, logical pixels. A hairline is always 1.
    pub width: f32,
    pub ink: BorderInk,
}

impl Border {
    pub const STOCK: Self = Self {
        width: 1.0,
        ink: BorderInk::Hairline,
    };

    /// Width band a document may state.
    pub const WIDTH_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=4.0;

    /// Whether every surface carries an outline of its own.
    #[must_use]
    pub fn outlines(&self) -> bool {
        self.ink != BorderInk::Hairline && self.width > 0.0
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !(self.width.is_finite() && Self::WIDTH_ADMITTED.contains(&self.width)) {
            return Err("border width outside 0..=4");
        }
        Ok(())
    }
}

impl Default for Border {
    fn default() -> Self {
        Self::STOCK
    }
}

/// How a surface stands off what is below it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Depth {
    /// Soft shadows, the design language's elevation, their strength
    /// scaled by `strength` (`1` stock).
    Shadow { strength: f32 },
    /// No shadow; a surface that would cast one is outlined instead, in
    /// the border's ink (the strong ink where the border is a hairline).
    Outline,
    /// No shadow and no outline: surfaces stand off by tone alone.
    None,
}

impl Depth {
    pub const STOCK: Self = Self::Shadow { strength: 1.0 };

    /// Shadow strength band a document may state.
    pub const STRENGTH_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;

    /// The factor every shadow's opacity is scaled by.
    #[must_use]
    pub const fn shadow_strength(self) -> f32 {
        match self {
            Self::Shadow { strength } => strength,
            Self::Outline | Self::None => 0.0,
        }
    }

    pub fn validate(self) -> Result<(), &'static str> {
        if let Self::Shadow { strength } = self
            && !(strength.is_finite() && Self::STRENGTH_ADMITTED.contains(&strength))
        {
            return Err("shadow strength outside 0..=2");
        }
        Ok(())
    }
}

impl Default for Depth {
    fn default() -> Self {
        Self::STOCK
    }
}

/// How far, as a contrast ratio, a translucent surface may stand off its
/// ground anywhere, whatever is behind it. Every ink drawn on a surface is
/// chosen to clear 7:1 on the opaque ground (`SMALL_TEXT_MIN_CONTRAST`; a
/// disabled row's dim ink included), so on the material it still clears
/// 7 / 1.5 ≈ 4.7:1, above body text's 4.5:1 with room for 8-bit rounding.
pub const MATERIAL_SPREAD: f64 = 1.5;

/// How far what shows through a translucent surface of `ground` may stand
/// off it: [`MATERIAL_SPREAD`], narrowed to what the theme's inks can
/// afford. Contrast ratios on one side of a ground divide exactly, so ink
/// with ratio `r` on the ground keeps `r / spread` on the band's far end;
/// the spread is the least of `r / floor` over the three ink tiers (body
/// text for primary and secondary, large text for tertiary, AAA text on a
/// high-contrast palette), never below `1` (an opaque surface).
#[must_use]
pub fn material_spread(colors: &crate::tokens::Colors, ground: vello::peniko::Color) -> f64 {
    use crate::tokens::contrast::{BODY_TEXT_MIN_CONTRAST, HIGH_CONTRAST_TEXT_MIN_CONTRAST};
    let text = if colors.palette.high_contrast {
        HIGH_CONTRAST_TEXT_MIN_CONTRAST
    } else {
        BODY_TEXT_MIN_CONTRAST
    };
    let large = if colors.palette.high_contrast {
        HIGH_CONTRAST_TEXT_MIN_CONTRAST
    } else {
        crate::tokens::colors::NON_TEXT_MIN_CONTRAST
    };
    [
        (colors.text_primary, text),
        (colors.text_dim, text),
        (colors.text_tertiary, large),
    ]
    .into_iter()
    // A hair inside, so the binding ink lands on its floor, not below it.
    .map(|(ink, floor)| crate::color::contrast_ratio(ink, ground) / floor * (1.0 - 1e-6))
    .fold(MATERIAL_SPREAD, f64::min)
    .max(1.0)
}

/// The darkest and the lightest colour a translucent surface of `ground`
/// shows, whatever lies below it: the ground stepped toward each end of the
/// theme's neutral ramp until it stands [`material_spread`] off it (or
/// reaches the end). The backdrop pass holds what shows through inside this
/// band before the tint is laid over it (`crate::backdrop`), so these are
/// the worst backdrops ink on the material is measured against.
#[must_use]
pub fn material_band(
    colors: &crate::tokens::Colors,
    ground: vello::peniko::Color,
) -> (vello::peniko::Color, vello::peniko::Color) {
    use vello::peniko::Color;
    let spread = material_spread(colors, ground);
    let toward = |end: Color| {
        let at = |t: f32| {
            let [r, g, b, _] = ground.components;
            let [er, eg, eb, _] = end.components;
            Color::new([r + (er - r) * t, g + (eg - g) * t, b + (eb - b) * t, 1.0])
        };
        if crate::color::contrast_ratio(at(1.0), ground) <= spread {
            return at(1.0);
        }
        let (mut within, mut beyond) = (0.0_f32, 1.0_f32);
        for _ in 0..24 {
            let mid = (within + beyond) / 2.0;
            if crate::color::contrast_ratio(at(mid), ground) <= spread {
                within = mid;
            } else {
                beyond = mid;
            }
        }
        at(within)
    };
    let stops = &colors.neutral.stops;
    let (first, last) = (toward(stops[0]), toward(stops[10]));
    if crate::color::relative_luminance(first) <= crate::color::relative_luminance(last) {
        (first, last)
    } else {
        (last, first)
    }
}

/// The opaque ground a surface of `role` is tinted with.
#[must_use]
pub fn role_ground(colors: &crate::tokens::Colors, role: SurfaceRole) -> vello::peniko::Color {
    match role {
        SurfaceRole::Window => colors.surfaces.content,
        SurfaceRole::Control => colors.surfaces.raised,
        SurfaceRole::Floating | SurfaceRole::Bar => colors.surfaces.overlay,
    }
}

/// What a surface of `role` can look like at its darkest and its lightest
/// under `finishes`, over any backdrop: the role's ground when it is
/// opaque; the band around it ([`material_band`]) when it shows a blurred
/// backdrop; and for a translucent control, which is never blurred, its
/// ground at its tint over the darkest and lightest window it can stand on.
#[must_use]
pub fn worst_grounds(
    colors: &crate::tokens::Colors,
    finishes: &RoleFinishes,
    role: SurfaceRole,
) -> [vello::peniko::Color; 2] {
    let ground = role_ground(colors, role);
    let finish = finishes.of(role);
    if !finish.translucent() {
        return [ground, ground];
    }
    if role == SurfaceRole::Control {
        let below = worst_grounds(colors, finishes, SurfaceRole::Window);
        return below.map(|under| crate::tokens::colors::over(under, ground, finish.tint()));
    }
    let (dark, light) = material_band(colors, ground);
    [dark, light]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glass_with(refraction: f32, specular: f32, saturation: f32) -> Finish {
        Finish::Glass {
            blur: 18.0,
            tint: 0.25,
            saturation,
            refraction,
            edge_highlight: 0.4,
            specular,
        }
    }

    #[test]
    fn every_finish_round_trips_and_out_of_band_values_are_refused() {
        let glass = Finish::Glass {
            blur: 18.0,
            tint: 0.25,
            saturation: 1.6,
            refraction: 10.0,
            edge_highlight: 0.4,
            specular: 0.15,
        };
        for finish in [
            Finish::Flat,
            Finish::Solid,
            Finish::Translucent {
                blur: 24.0,
                tint: 0.5,
                saturation: 1.2,
            },
            glass,
        ] {
            let json = serde_json::to_value(finish).unwrap();
            assert_eq!(serde_json::from_value::<Finish>(json).unwrap(), finish);
            assert_eq!(finish.validate(), Ok(()));
        }
        assert_eq!(
            serde_json::to_value(glass).unwrap()["painter"],
            serde_json::json!("glass")
        );
        assert_eq!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "block"})).unwrap(),
            Finish::Flat
        );
        assert_eq!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "default"})).unwrap(),
            Finish::Solid
        );
        assert!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "block", "blur": 3.0}))
                .is_err(),
            "the block set takes no blur"
        );
        assert!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "chrome"})).is_err(),
            "a painter set this build does not have is not admitted"
        );
        for bad in [
            Finish::Translucent {
                blur: 80.0,
                tint: 0.5,
                saturation: 1.0,
            },
            Finish::Translucent {
                blur: 8.0,
                tint: f32::NAN,
                saturation: 1.0,
            },
            glass_with(64.0, 0.1, 1.0),
            glass_with(8.0, 0.9, 1.0),
            glass_with(8.0, 0.1, 3.0),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn opaque_keeps_flat_flat_and_makes_the_rest_solid() {
        let block = RoleFinishes {
            window: Finish::Flat,
            control: Finish::Flat,
            floating: Finish::Flat,
            bar: Some(Finish::Flat),
        };
        assert_eq!(block.opaque(), block);
        assert_eq!(RoleFinishes::STOCK.opaque().floating, Finish::Solid);
        assert_eq!(RoleFinishes::STOCK.of(SurfaceRole::Bar), Finish::Flat);
    }

    #[test]
    fn depth_and_border_round_trip() {
        for depth in [Depth::STOCK, Depth::Outline, Depth::None] {
            let json = serde_json::to_value(depth).unwrap();
            assert_eq!(serde_json::from_value::<Depth>(json).unwrap(), depth);
        }
        assert!(Depth::Shadow { strength: 3.0 }.validate().is_err());
        let border = Border {
            width: 2.0,
            ink: BorderInk::Strong,
        };
        let json = serde_json::to_value(border).unwrap();
        assert_eq!(serde_json::from_value::<Border>(json).unwrap(), border);
        assert!(border.outlines());
        assert!(!Border::STOCK.outlines());
        assert!(
            Border {
                width: 9.0,
                ..border
            }
            .validate()
            .is_err()
        );
    }
}
