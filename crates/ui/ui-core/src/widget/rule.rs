//! `Rule`: a horizontal rule of a size the view gives it, with a tick at each end, for what is
//! measured rather than written — a map's scale bar.
//!
//! A rule that fills its row is a `divider`. This one is sized: the view measures the thing it
//! stands for (a map's scale at the camera it shows) and asks for that many logical pixels, so
//! the line's length means the distance its label names. The ticks at the ends read as a
//! measurement's ends, as a map's scale bar's do.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

#[path = "rule_component.rs"]
mod component;
pub use component::RuleProps;

/// How far a tick reaches from the rule, in logical pixels.
const TICK: f32 = 4.0;

/// The rule's thickness when the view does not say otherwise, in logical pixels.
const THICKNESS: f32 = 1.5;

/// A horizontal rule of a given length, ticked at each end.
pub struct Rule {
    rect: Rect,
    /// How long it draws, in logical pixels; `None` fills the row it stands in.
    length: Option<f64>,
    /// A tick stands down at each end.
    ticks: bool,
    thickness: f32,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            length: None,
            ticks: false,
            thickness: THICKNESS,
        }
    }
}

impl Rule {
    /// A rule `length` logical pixels long, ticked at its ends.
    #[must_use]
    pub fn sized(length: f64) -> Self {
        Self {
            length: Some(length.max(0.0)),
            ticks: true,
            ..Self::default()
        }
    }

    /// A rule that fills the row it stands in.
    #[must_use]
    pub fn filling() -> Self {
        Self::default()
    }

    /// Whether the ends carry a tick.
    #[must_use]
    pub fn with_ticks(mut self, ticks: bool) -> Self {
        self.ticks = ticks;
        self
    }

    /// The ink it draws in: the person's own text ink, which reads on every surface and theme.
    fn ink(theme: &Theme) -> vello::peniko::Color {
        theme.tokens.colors.text_primary
    }
}

impl Widget for Rule {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let width = self
            .length
            .map_or(taffy::style_helpers::percent(1.0_f32), |length| {
                taffy::style_helpers::length(length as f32)
            });
        let height = if self.ticks {
            self.thickness + 2.0 * TICK
        } else {
            self.thickness
        };
        let style = Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width,
                height: taffy::style_helpers::length(height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        draw_rule(
            scene,
            transform,
            theme,
            self.rect,
            self.thickness,
            self.ticks,
        );
    }

    fn name(&self) -> &'static str {
        "Rule"
    }
}

/// Paint a rule across `rect`'s middle, its ticks reaching down from each end.
fn draw_rule(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    thickness: f32,
    ticks: bool,
) {
    let ink = Rule::ink(theme);
    let middle = rect.center().y;
    let stroke = f64::from(thickness);
    let line = Rect::new(
        rect.x0,
        middle - stroke / 2.0,
        rect.x1,
        middle + stroke / 2.0,
    );
    scene.fill(vello::peniko::Fill::NonZero, transform, ink, None, &line);
    if ticks {
        for x in [rect.x0, rect.x1 - stroke] {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                ink,
                None,
                &Rect::new(x, middle, x + stroke, middle + f64::from(TICK)),
            );
        }
    }
}
