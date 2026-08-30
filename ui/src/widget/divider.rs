//! A standalone themed 1px rule, horizontal or vertical, usable as a
//! free-standing element inside a `Column` / `Row` (unlike
//! `Toolbar::push_separator`, which is toolbar-scoped). Sizes itself along
//! its cross-axis and stretches to fill the main axis.
//!
//! [`Divider::horizontal`] fills the parent's width, 1px tall, for stacked
//! sections in a `Column`; [`Divider::vertical`] fills the parent's height,
//! 1px wide, for side-by-side panes in a `Row`. Colour comes from
//! `theme.border_standard`, overridable via [`Divider::with_color`].

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
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DividerAxis {
    Horizontal,
    Vertical,
}

pub struct Divider {
    axis: DividerAxis,
    thickness: f32,
    /// Optional explicit colour. `None` ⇒ `theme.border_standard`.
    color: Option<vello::peniko::Color>,
    rect: Rect,
}

impl Divider {
    pub fn horizontal() -> Self {
        Self {
            axis: DividerAxis::Horizontal,
            thickness: 1.0,
            color: None,
            rect: Rect::ZERO,
        }
    }

    pub fn vertical() -> Self {
        Self {
            axis: DividerAxis::Vertical,
            thickness: 1.0,
            color: None,
            rect: Rect::ZERO,
        }
    }

    /// Logical pixels.
    pub fn with_thickness(mut self, px: f32) -> Self {
        self.thickness = px.max(0.0);
        self
    }

    /// Pass a theme token (e.g. a fainter `theme.tokens.colors.neutral[..]`)
    /// so the choice still tracks the active theme.
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
        self
    }

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
            DividerAxis::Horizontal => (percent(1.0_f32), length(self.thickness)),
            DividerAxis::Vertical => (length(self.thickness), percent(1.0_f32)),
        };
        let node = taffy.leaf_node(Style {
            // Never shrink the 1px cross-axis away under tight budgets.
            flex_shrink: 0.0,
            size: taffy::Size {
                width: w,
                height: h,
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &self.rect,
        );
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

    fn name(&self) -> &'static str {
        "Divider"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn lay_out(mut d: Divider) -> (Divider, Rect) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        // Wrap in a fixed-size flex parent so percent() resolves.
        let child = d.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy.parent_node(
            Style {
                size: taffy::Size {
                    width: length(400.0_f32),
                    height: length(200.0_f32),
                },
                ..Default::default()
            },
            &[child],
        );
        taffy
            .compute_layout(parent, taffy::Size::max_content())
            .unwrap();
        d.apply_layout(&taffy, child, Point::ZERO);
        let r = d.get_rect();
        (d, r)
    }

    #[test]
    fn horizontal_is_one_px_tall_and_fills_width() {
        let (_d, r) = lay_out(Divider::horizontal());
        assert!(
            (r.height() - 1.0).abs() < 0.5,
            "1px tall, got {}",
            r.height()
        );
        assert!(r.width() > 100.0, "fills parent width, got {}", r.width());
    }

    #[test]
    fn vertical_is_one_px_wide_and_fills_height() {
        let (_d, r) = lay_out(Divider::vertical());
        assert!((r.width() - 1.0).abs() < 0.5, "1px wide, got {}", r.width());
        assert!(
            r.height() > 100.0,
            "fills parent height, got {}",
            r.height()
        );
    }

    #[test]
    fn with_thickness_overrides_cross_axis() {
        let (_d, r) = lay_out(Divider::horizontal().with_thickness(3.0));
        assert!(
            (r.height() - 3.0).abs() < 0.5,
            "3px tall, got {}",
            r.height()
        );
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
