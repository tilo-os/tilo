//! A standalone themed 1px rule, horizontal or vertical, usable as a
//! free-standing element inside a `Column` / `Row` (unlike
//! `Toolbar::push_separator`, which is toolbar-scoped). Sizes itself along
//! its cross-axis and stretches to fill the main axis.
//!
//! [`Divider::horizontal`] fills the parent's width, 1px tall, for stacked
//! sections in a `Column`; [`Divider::vertical`] fills the parent's height,
//! 1px wide, for side-by-side panes in a `Row`. Colour comes from
//! `theme.border_standard` — a rule on `surfaces.base` (HIG §9.5). A rule
//! inside a raised or overlay surface names that surface with
//! [`Divider::on`], which derives the colour so the line stays visible on
//! the dark preset; [`Divider::with_color`] is the explicit override.

use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

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
    /// Optional explicit colour. `None` ⇒ derived from [`Self::surface`]
    /// when set, else `theme.border_standard`.
    color: Option<vello::peniko::Color>,
    /// The surface the rule is painted on, when it is not `surfaces.base`;
    /// the colour is then `surface::edge_on(surface)`.
    surface: Option<vello::peniko::Color>,
    rect: Rect,
}

impl Divider {
    pub fn horizontal() -> Self {
        Self {
            axis: DividerAxis::Horizontal,
            thickness: 1.0,
            color: None,
            surface: None,
            rect: Rect::ZERO,
        }
    }

    pub fn vertical() -> Self {
        Self {
            axis: DividerAxis::Vertical,
            thickness: 1.0,
            color: None,
            surface: None,
            rect: Rect::ZERO,
        }
    }

    /// Logical pixels.
    #[must_use]
    pub fn with_thickness(mut self, px: f32) -> Self {
        self.thickness = px.max(0.0);
        self
    }

    /// Pass a theme token so the choice still tracks the active theme.
    /// Prefer [`Self::on`] for a rule inside a raised or overlay surface.
    #[must_use]
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
        self
    }

    /// A rule inside `surface` (a card, popover or panel fill): the colour
    /// becomes [`crate::surface::edge_on`] measured against that surface,
    /// so it registers on every theme where a declared `border_standard`
    /// would sit within rounding distance of the fill (HIG §9.5).
    #[must_use]
    pub fn on(mut self, surface: vello::peniko::Color) -> Self {
        self.surface = Some(surface);
        self
    }

    pub fn axis(&self) -> DividerAxis {
        self.axis
    }

    /// The colour `draw` paints: an explicit [`Self::with_color`] wins, then
    /// a surface named with [`Self::on`] derives its edge, else the theme's
    /// rule on `surfaces.base`.
    pub fn rule_color(&self, theme: &Theme) -> vello::peniko::Color {
        self.color.unwrap_or_else(|| {
            self.surface
                .map_or(theme.border_standard, crate::surface::edge_on)
        })
    }
}

impl Widget for Divider {
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
        let style = Style {
            // Never shrink the 1px cross-axis away under tight budgets.
            flex_shrink: 0.0,
            size: taffy::Size {
                width: w,
                height: h,
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

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
        let color = self.rule_color(theme);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &self.rect,
        );
    }

    fn name(&self) -> &'static str {
        "Divider"
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
                size: taffy::Size::from_lengths(400.0_f32, 200.0_f32),
                ..Default::default()
            },
            &[child],
        );
        taffy
            .compute_layout(parent, taffy::Size::max_content())
            .unwrap();
        d.apply_layout(&taffy, child, vello::kurbo::Point::ZERO);
        let r = d.get_rect();
        (d, r)
    }

    #[test]
    fn divider_axis_thickness_and_rule_colour() {
        fn horizontal_is_one_px_tall_and_fills_width() {
            let (_d, r) = lay_out(Divider::horizontal());
            assert!(
                (r.height() - 1.0).abs() < 0.5,
                "1px tall, got {}",
                r.height()
            );
            assert!(r.width() > 100.0, "fills parent width, got {}", r.width());
        }

        fn vertical_is_one_px_wide_and_fills_height() {
            let (_d, r) = lay_out(Divider::vertical());
            assert!((r.width() - 1.0).abs() < 0.5, "1px wide, got {}", r.width());
            assert!(
                r.height() > 100.0,
                "fills parent height, got {}",
                r.height()
            );
        }

        fn with_thickness_overrides_cross_axis() {
            let (_d, r) = lay_out(Divider::horizontal().with_thickness(3.0));
            assert!(
                (r.height() - 3.0).abs() < 0.5,
                "3px tall, got {}",
                r.height()
            );
        }

        /// The colour `draw` resolves (`rule_color` is what `draw` paints):
        /// default is the theme's rule on `surfaces.base`; `.on(surface)`
        /// derives an edge that registers against that surface on every preset;
        /// an explicit `.with_color` wins over both.
        fn rule_color_follows_with_color_then_on_then_default() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let raised = theme.tokens.colors.surfaces.raised;
                let name = &theme.name;

                let default = Divider::horizontal().rule_color(&theme);
                assert_eq!(default, theme.border_standard, "{name}: default rule");

                let on_raised = Divider::horizontal().on(raised).rule_color(&theme);
                assert_eq!(
                    on_raised,
                    crate::surface::edge_on(raised),
                    "{name}: `.on(raised)` derives the edge"
                );
                assert_ne!(
                    on_raised, theme.border_standard,
                    "{name}: `.on` changes the colour"
                );
                assert!(
                    crate::surface::edge_delta(on_raised, raised)
                        >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{name}: a divider on `raised` must register against it"
                );

                let explicit = theme.tokens.colors.intent.accent;
                assert_eq!(
                    Divider::horizontal()
                        .on(raised)
                        .with_color(explicit)
                        .rule_color(&theme),
                    explicit,
                    "{name}: `.with_color` wins over `.on`"
                );
            }
        }

        fn on_surface_still_emits_exactly_one_fill() {
            let theme = Theme::dark();
            let raised = theme.tokens.colors.surfaces.raised;
            let (mut d, _r) = lay_out(Divider::horizontal().on(raised));
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
            assert_eq!(fills, 1, "a divider on a surface is still one fill");
        }

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

        horizontal_is_one_px_tall_and_fills_width();
        vertical_is_one_px_wide_and_fills_height();
        with_thickness_overrides_cross_axis();
        rule_color_follows_with_color_then_on_then_default();
        on_surface_still_emits_exactly_one_fill();
        draw_emits_a_fill();
    }
}
