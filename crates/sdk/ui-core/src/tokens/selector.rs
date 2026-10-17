//! The selector finish: what a docked strip of selectors (the window
//! group's tab strip) stands on, and the lit rim and hover film of the items
//! in it. A theme sets it in its document (`material.selector`); every
//! selector paints through `crate::chrome::paint_selector`.

use serde::{Deserialize, Serialize};
use vello::peniko::Color;

/// What a docked selector strip's body is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripMaterial {
    /// The floating material: the desktop behind the strip blurred and
    /// tinted with the panel ground, as menus and the palette are.
    Floating,
    /// The opaque panel ground.
    Opaque,
}

/// How selectors are finished. Selection itself (which state lifts how
/// far) is not the theme's to change: the active item is always one surface
/// step of its ground; this only says what the strip is made of, how the
/// light catches an active item and how much of the lift a hover shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectorFinish {
    pub strip: StripMaterial,
    /// How far down an active item its lit rim reaches before it is gone,
    /// as a share of the item's height. A rim lit all the way round is a
    /// halo, not light.
    pub rim_span: f32,
    /// Opacity of the lit rim where it is brightest, at the item's top.
    /// `0` paints no rim.
    pub rim_alpha: f32,
    /// Opacity of the hover film: the active lift laid over the strip at
    /// this strength, so the strip still shows through a hovered item.
    pub hover_alpha: f32,
}

impl SelectorFinish {
    /// The stock finish: a frosted strip, a rim that fades over the top
    /// half, and a hover strong enough to clear the selection step on an
    /// opaque ground (`crate::chrome::SELECTION_MIN_STEP`).
    pub const STOCK: Self = Self {
        strip: StripMaterial::Floating,
        rim_span: 0.55,
        rim_alpha: 0.22,
        hover_alpha: 0.75,
    };

    /// High contrast: a solid strip and no rim. Its hover is composited
    /// onto the ground it stands on, so it is opaque too.
    pub const OPAQUE: Self = Self {
        strip: StripMaterial::Opaque,
        rim_span: 0.0,
        rim_alpha: 0.0,
        hover_alpha: Self::STOCK.hover_alpha,
    };

    /// Rim span band a document may state.
    pub const RIM_SPAN_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=1.0;
    /// Rim opacity band a document may state: past it the rim reads as a
    /// second edge.
    pub const RIM_ALPHA_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=0.5;
    /// Hover opacity band a document may state: below it a hovered item no
    /// longer clears the selection step on the lightest ground.
    pub const HOVER_ALPHA_ADMITTED: std::ops::RangeInclusive<f32> = 0.7..=1.0;

    /// Whether the hover film and the strip may let what is behind them
    /// through.
    #[must_use]
    pub fn translucent(&self) -> bool {
        self.strip == StripMaterial::Floating
    }

    /// The lit rim's colour at its brightest, or `None` without a rim. Light,
    /// whichever way the palette lifts: it is the light landing on the
    /// item, not a shade of the item.
    #[must_use]
    pub fn rim(&self) -> Option<Color> {
        (self.rim_alpha > 0.0 && self.rim_span > 0.0)
            .then(|| Color::WHITE.with_alpha(self.rim_alpha.clamp(0.0, 1.0)))
    }

    /// What a frosted strip of `ground` tinted at `opacity` can look like at
    /// its darkest and its lightest: the tint over black and over white.
    /// Ink on the strip holds against both, whatever the desktop is.
    #[must_use]
    pub fn frosted_extremes(ground: Color, opacity: f32) -> [Color; 2] {
        [Color::BLACK, Color::WHITE]
            .map(|backdrop| crate::tokens::colors::over(backdrop, ground, opacity))
    }

    /// The finish actually painted: where nothing may be translucent (forced
    /// colours, reduced transparency) a solid strip with no rim, whatever the
    /// theme says; the hover keeps its strength, composited.
    #[must_use]
    pub fn effective(self, opaque: bool) -> Self {
        if opaque {
            Self {
                strip: StripMaterial::Opaque,
                rim_alpha: 0.0,
                ..self
            }
        } else {
            self
        }
    }
}
