//! The Quick Settings tile and its level gauge: a control that is on wears
//! the person's accent as a tint over its own floor and its glyph takes the
//! accent as ink, the half under the pointer lifts, and a tile with a
//! section of its own sets its chevron apart with a hairline.

use crate::theme::Theme;
use crate::tokens::colors::{Colors, NON_TEXT_MIN_CONTRAST};
use crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// How far a tile that is on is lifted off its own floor, in surface steps:
/// the same step a selected row takes.
const ON_LIFT: f32 = 0.5;
/// How much of the accent is washed into that lift.
const ON_TINT: f32 = 0.20;
const ON_HOVER_LIFT: f32 = 0.10;
/// The hairline before a tile's chevron area on a tile that is on; one that
/// is off keeps the primary ink at [`OFF_DIVIDER`].
const ON_DIVIDER: u8 = 44;
const OFF_DIVIDER: f32 = 0.12;

/// A Quick Settings tile's ground: its switch half (`main`) and, when it
/// has a section, its chevron half (`expand`).
#[derive(Clone, Copy, Debug)]
pub struct QuickTileFace {
    pub rect: Rect,
    pub main: Rect,
    pub expand: Option<Rect>,
    pub on: bool,
    pub hovered: Option<Rect>,
    pub ground: Color,
    pub radius: f64,
}

impl QuickTileFace {
    #[must_use]
    pub fn fill(&self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        if self.on {
            colors.lift_tinted_on(self.ground, ON_LIFT, ON_TINT)
        } else {
            self.ground
        }
    }

    /// Paint the on fill (a tile that is off keeps the control ground its
    /// host painted) and lift the half under the pointer: by the accent on
    /// a tile that is on, by `off_hover` on one that is off.
    pub fn paint_ground(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        off_hover: Color,
    ) {
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(self.rect, self.radius);
        if self.on {
            scene.fill(Fill::NonZero, transform, self.fill(theme), None, &shape);
        }
        if let Some(half) = self.hovered {
            let lift = if self.on {
                colors.intent.accent.multiply_alpha(ON_HOVER_LIFT)
            } else {
                off_hover
            };
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &shape,
            );
            scene.fill(Fill::NonZero, transform, lift, None, &half);
            scene.pop_layer();
        }
    }

    #[must_use]
    pub fn ink(&self, theme: &Theme) -> (Color, Color) {
        let colors = &theme.tokens.colors;
        if self.on {
            let fill = self.fill(theme);
            (
                colors.ink_on(fill, colors.text_primary, BODY_TEXT_MIN_CONTRAST),
                colors.ink_on(fill, colors.text_dim, BODY_TEXT_MIN_CONTRAST),
            )
        } else {
            (colors.text_primary, colors.text_dim)
        }
    }

    #[must_use]
    pub fn glyph_ink(&self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        if self.on {
            colors.ink_on(
                self.fill(theme),
                colors.intent.accent_ink,
                NON_TEXT_MIN_CONTRAST,
            )
        } else {
            colors.text_primary
        }
    }

    /// The hairline that sets the chevron half apart, `inset_y` in from the
    /// tile's top and bottom.
    pub fn paint_divider(&self, scene: &mut Scene, transform: Affine, theme: &Theme, inset_y: f64) {
        let Some(expand) = self.expand else {
            return;
        };
        let colors = &theme.tokens.colors;
        let divider = if self.on {
            Colors::hairline_on(self.fill(theme), ON_DIVIDER)
        } else {
            colors.text_primary.multiply_alpha(OFF_DIVIDER)
        };
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        scene.fill(
            Fill::NonZero,
            transform,
            divider,
            None,
            &Rect::new(
                expand.x0,
                expand.y0 + inset_y,
                expand.x0 + rule,
                expand.y1 - inset_y,
            ),
        );
    }
}

/// A charge gauge at `rect`: a rounded rim in the secondary ink, filled to
/// `level` (0 to 1) in the primary ink.
pub fn paint_level_gauge(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    level: f64,
    theme: &Theme,
) {
    let colors = &theme.tokens.colors;
    let radius = rect.height() / 4.0;
    scene.stroke(
        &Stroke::new(1.5),
        transform,
        colors.text_dim,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    let inner = rect.inset(-3.0);
    let filled = Rect::new(
        inner.x0,
        inner.y0,
        inner.x0 + inner.width() * level.clamp(0.0, 1.0),
        inner.y1,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.text_primary,
        None,
        &RoundedRect::from_rect(filled, radius / 2.0),
    );
}
