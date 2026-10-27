//! The Quick Settings tile and its level gauge: a control that is on is
//! filled with the on colour, the half under the pointer lifts, and a tile
//! with a section of its own sets its chevron apart with a hairline.

use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// How far the half under the pointer lifts on a tile that is on: the on
/// mark's colour at this share.
const ON_HOVER_LIFT: f32 = 0.12;
/// A tile that is on letters its second line in the on mark at this share.
const ON_SECONDARY_INK: f32 = 0.82;
/// The hairline before a tile's chevron area: the on mark's colour on a
/// tile that is on, the primary ink on one that is off, at these shares.
const ON_DIVIDER: f32 = 0.35;
const OFF_DIVIDER: f32 = 0.12;

/// A Quick Settings tile's ground: its switch half (`main`) and, when it
/// has a section, its chevron half (`expand`).
#[derive(Clone, Copy, Debug)]
pub struct QuickTileFace {
    /// The whole tile.
    pub rect: Rect,
    /// The switch half.
    pub main: Rect,
    /// The chevron half, when the tile has a section.
    pub expand: Option<Rect>,
    pub on: bool,
    /// The half under the pointer, if any.
    pub hovered: Option<Rect>,
    /// The tile's corner radius.
    pub radius: f64,
}

impl QuickTileFace {
    /// Paint the on fill (a tile that is off keeps the control ground its
    /// host painted) and lift the half under the pointer: by the on mark on
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
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.on_state,
                None,
                &shape,
            );
        }
        if let Some(half) = self.hovered {
            let lift = if self.on {
                colors.intent.on_state_mark.multiply_alpha(ON_HOVER_LIFT)
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

    /// The tile's primary and secondary ink.
    #[must_use]
    pub fn ink(&self, theme: &Theme) -> (Color, Color) {
        let colors = &theme.tokens.colors;
        if self.on {
            let mark = colors.intent.on_state_mark;
            (mark, mark.multiply_alpha(ON_SECONDARY_INK))
        } else {
            (colors.text_primary, colors.text_dim)
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
            colors.intent.on_state_mark.multiply_alpha(ON_DIVIDER)
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
