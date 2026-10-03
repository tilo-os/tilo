//! Countdown ring — a custom `Widget` that strokes an arc into the Vello
//! scene to show how much of the current TOTP period is left.
//!
//! The ring paints a faint full-circle track and, on top of it, an arc
//! spanning `fraction × 2π` starting from 12 o'clock and sweeping
//! clockwise. As the period drains the arc shortens, so the "head"
//! recedes back toward the top. The whole-seconds-remaining count is
//! drawn centred inside. When ≤ 5 seconds remain the arc and number
//! shift to the theme's danger colour as an at-a-glance "about to
//! rotate" cue.
//!
//! It implements the full `Widget` trait so it is a first-class toolkit
//! widget, but [`crate::list::AccountList`] drives it imperatively (one
//! shared instance, repositioned per row) via [`CountdownRing::place`] and
//! [`CountdownRing::draw_arc`] rather than through the taffy tree — the
//! list needs a different fraction/colour per row each frame.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, Stroke};
use vello::peniko::Color;

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

/// Smoothness of the stroked arc. 96 line segments around a ≤ 22px-radius
/// ring is visually indistinguishable from a true curve.
const ARC_STEPS: usize = 96;

/// Below this many seconds remaining, the ring switches to the danger
/// colour.
const URGENT_SECONDS: u32 = 5;

pub struct CountdownRing {
    center: Point,
    radius: f64,
    thickness: f64,
    /// 0..1 of the period still remaining.
    fraction: f64,
    /// Whole seconds left, drawn in the middle.
    seconds: u32,
    /// Base accent colour (the account's colour). Overridden by the
    /// theme danger colour when `seconds <= URGENT_SECONDS`.
    base_color: Color,
    /// Whether to render the seconds count in the centre.
    show_seconds: bool,
    rect: Rect,
}

impl Default for CountdownRing {
    fn default() -> Self {
        Self::new()
    }
}

impl CountdownRing {
    pub fn new() -> Self {
        Self {
            center: Point::ZERO,
            radius: 18.0,
            thickness: 3.0,
            fraction: 1.0,
            seconds: 30,
            base_color: Color::WHITE,
            show_seconds: true,
            rect: Rect::ZERO,
        }
    }

    /// Position the ring at `center` with the given `radius`. Updates the
    /// bounding rect so `get_rect`/culling stay correct.
    pub fn place(&mut self, center: Point, radius: f64) -> &mut Self {
        self.center = center;
        self.radius = radius;
        self.thickness = (radius * 0.16).clamp(2.5, 4.0);
        self.rect = Rect::new(
            center.x - radius,
            center.y - radius,
            center.x + radius,
            center.y + radius,
        );
        self
    }

    pub fn set_fraction(&mut self, fraction: f64) -> &mut Self {
        self.fraction = fraction.clamp(0.0, 1.0);
        self
    }

    pub fn set_seconds(&mut self, seconds: u32) -> &mut Self {
        self.seconds = seconds;
        self
    }

    pub fn set_color(&mut self, color: Color) -> &mut Self {
        self.base_color = color;
        self
    }

    pub fn set_show_seconds(&mut self, show: bool) -> &mut Self {
        self.show_seconds = show;
        self
    }

    /// Effective arc colour: danger when the period is about to roll over.
    fn arc_color(&self, theme: &Theme) -> Color {
        if self.seconds <= URGENT_SECONDS {
            theme.tokens.colors.semantic.danger
        } else {
            self.base_color
        }
    }

    /// The public draw entry point used by [`crate::list::AccountList`].
    /// Paints the track, the remaining arc, and (optionally) the seconds
    /// count, all in absolute coordinates offset by `transform`.
    pub fn draw_arc(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.radius <= 0.0 {
            return;
        }
        let stroke = Stroke::new(self.thickness)
            .with_caps(Cap::Round)
            .with_join(Join::Round);

        // Faint full-circle track, derived against the raised card the ring
        // sits on (HIG §9.5) at the panel-border strength so a 3 px arc reads
        // as a track rather than a hairline. A declared `border_standard` was
        // three shades from `raised` on the dark preset and left the arc
        // orphaned; a white overlay would vanish on a light card.
        let track = Circle::new(self.center, self.radius);
        scene.stroke(
            &Stroke::new(self.thickness),
            transform,
            ui::surface::edge_on_with(
                theme.tokens.colors.surfaces.raised,
                ui::chrome::PANEL_BORDER_ALPHA,
            ),
            None,
            &track,
        );

        // Remaining arc, top → clockwise.
        if self.fraction > 0.0 {
            let sweep = self.fraction * TAU;
            let start = -FRAC_PI_2;
            let mut path = BezPath::new();
            for i in 0..=ARC_STEPS {
                let t = i as f64 / ARC_STEPS as f64;
                let ang = start + sweep * t;
                let x = self.center.x + self.radius * ang.cos();
                let y = self.center.y + self.radius * ang.sin();
                if i == 0 {
                    path.move_to((x, y));
                } else {
                    path.line_to((x, y));
                }
            }
            scene.stroke(&stroke, transform, self.arc_color(theme), None, &path);
        }

        // Centre seconds count. The radius already carries the text scale
        // (the row measures it as `RING_R × text_scale`), and the renderer
        // applies that scale again while shaping, so the label's design size
        // goes through `renderer_input_style` once — otherwise the digits
        // render at 4× under 200% text and spill past the ring.
        if self.show_seconds {
            let label = self.seconds.to_string();
            let size = text_renderer
                .renderer_input_style(&TextStyle::new((self.radius * 0.95) as f32))
                .size;
            let color = if self.seconds <= URGENT_SECONDS {
                theme.tokens.colors.semantic.danger
            } else {
                theme.tokens.colors.text_dim
            };
            let (w, h) = text_renderer.measure_weighted(&label, size, None, TextWeight::Semibold);
            let tx = self.center.x - f64::from(w) / 2.0;
            let ty = self.center.y - f64::from(h) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &label,
                size,
                color,
                transform * Affine::translate((tx, ty)),
                None,
                TextWeight::Semibold,
            );
        }
    }
}

impl Widget for CountdownRing {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let d = self.radius as f32 * 2.0;
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(d),
                    height: Dimension::length(d),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        let w = f64::from(layout.size.width);
        let h = f64::from(layout.size.height);
        self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
        self.center = self.rect.center();
        self.radius = (w.min(h) / 2.0).max(0.0);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_arc(scene, transform, text_renderer, theme);
    }

    fn name(&self) -> &'static str {
        "CountdownRing"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The ring animates every frame (the fraction sweeps continuously),
    /// so it must keep the host pumping redraws like the Spinner does.
    fn needs_animation_frame(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}
