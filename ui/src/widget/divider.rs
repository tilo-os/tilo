//! `Divider` — a standalone themed 1px rule, horizontal or vertical.
//!
//! Until now the only separator in the toolkit was `Toolbar::push_separator`,
//! which is toolbar-scoped and not usable as a free-standing element inside a
//! `Column` / `Row`. Settings (and any list/section layout) needs a real
//! widget it can `.push()` between sections. `Divider` is that widget: a
//! single themed line that sizes itself along its cross-axis and stretches to
//! fill the main axis.
//!
//! * [`Divider::horizontal`] — a full-width 1px rule (fills its parent's
//!   width, 1px tall). The common case between stacked sections in a `Column`.
//! * [`Divider::vertical`] — a full-height 1px rule (1px wide, fills its
//!   parent's height). For separating side-by-side panes in a `Row`.
//!
//! The colour comes from `theme.border_standard` (overridable via
//! [`Divider::with_color`] for callers that want a fainter inset rule). The
//! thickness defaults to 1px but can be bumped for emphasis.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Orientation of the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DividerAxis {
    /// A horizontal rule: fills the parent's width, `thickness` px tall.
    Horizontal,
    /// A vertical rule: `thickness` px wide, fills the parent's height.
    Vertical,
}

/// A themed 1px separator line.
pub struct Divider {
    axis: DividerAxis,
    thickness: f32,
    /// Optional explicit colour. `None` ⇒ `theme.border_standard`.
    color: Option<vello::peniko::Color>,
    rect: Rect,
}

impl Divider {
    /// A full-width horizontal rule (the section-separator default).
    pub fn horizontal() -> Self {
        Self { axis: DividerAxis::Horizontal, thickness: 1.0, color: None, rect: Rect::ZERO }
    }

    /// A full-height vertical rule (pane separator).
    pub fn vertical() -> Self {
        Self { axis: DividerAxis::Vertical, thickness: 1.0, color: None, rect: Rect::ZERO }
    }

    /// Override the 1px default thickness (logical pixels).
    pub fn with_thickness(mut self, px: f32) -> Self {
        self.thickness = px.max(0.0);
        self
    }

    /// Paint the rule in an explicit colour instead of `border_standard`.
    /// Pass a theme token (e.g. a fainter `theme.tokens.colors.neutral[..]`)
    /// so the choice still tracks the active theme.
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
        self
    }

    /// The axis this divider was constructed for.
    pub fn axis(&self) -> DividerAxis {
        self.axis
    }
}

impl Widget for Divider {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (w, h) = match self.axis {
            DividerAxis::Horizontal => (percent(1.0), length(self.thickness)),
            DividerAxis::Vertical => (length(self.thickness), percent(1.0)),
        };
        let node = taffy
            .new_leaf(Style {
                // Never shrink the 1px cross-axis away under tight budgets.
                flex_shrink: 0.0,
                size: taffy::Size { width: w, height: h },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let color = self.color.unwrap_or(theme.border_standard);
        scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &self.rect);
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str { "Divider" }

    fn get_rect(&self) -> Rect { self.rect }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    fn lay_out(mut d: Divider) -> (Divider, Rect) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        // Wrap in a fixed-size flex parent so percent() resolves.
        let child = d.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy
            .new_with_children(
                Style {
                    size: taffy::Size {
                        width: length(400.0),
                        height: length(200.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy.compute_layout(parent, taffy::Size::max_content()).unwrap();
        d.apply_layout(&taffy, child, Point::ZERO);
        let r = d.get_rect();
        (d, r)
    }

    #[test]
    fn horizontal_is_one_px_tall_and_fills_width() {
        let (_d, r) = lay_out(Divider::horizontal());
        assert!((r.height() - 1.0).abs() < 0.5, "1px tall, got {}", r.height());
        assert!(r.width() > 100.0, "fills parent width, got {}", r.width());
    }

    #[test]
    fn vertical_is_one_px_wide_and_fills_height() {
        let (_d, r) = lay_out(Divider::vertical());
        assert!((r.width() - 1.0).abs() < 0.5, "1px wide, got {}", r.width());
        assert!(r.height() > 100.0, "fills parent height, got {}", r.height());
    }

    #[test]
    fn with_thickness_overrides_cross_axis() {
        let (_d, r) = lay_out(Divider::horizontal().with_thickness(3.0));
        assert!((r.height() - 3.0).abs() < 0.5, "3px tall, got {}", r.height());
    }

    #[test]
    fn draw_emits_a_fill() {
        let (mut d, _r) = lay_out(Divider::horizontal());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        d.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert_eq!(fills, 1, "a laid-out divider must emit exactly one fill");
    }
}
