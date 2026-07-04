//! `IconWidget` — a fixed-size leaf that paints one themed vector glyph
//! from the shared [`crate::icons::Icon`] set.
//!
//! The [`crate::icons`] module already exposes the glyph data and the
//! immediate-mode [`crate::icons::draw_icon`] / [`crate::icons::Icon::draw`]
//! helpers used by shell chrome that paints icons inline. This widget wraps
//! that helper in the [`Widget`] contract so an icon can be dropped straight
//! into a `Row`/`Column`/`Container` like any other widget, sized by the
//! layout pass, and tinted from the active [`Theme`].
//!
//! Colour resolution: by default the glyph is stroked in
//! [`Theme::text_primary`]. Call [`IconWidget::with_color`] for an explicit
//! tint, or [`IconWidget::dim`] to use the theme's dim text colour (for
//! secondary affordances). The stroke width defaults to the lucide 1.5 px
//! aesthetic; override via [`IconWidget::with_stroke`].

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Where the glyph colour comes from.
#[derive(Clone, Copy, Debug)]
enum Tint {
    /// Resolve to `theme.text_primary` at draw time (default).
    Primary,
    /// Resolve to `theme.text_dim` at draw time.
    Dim,
    /// A fixed colour supplied by the caller.
    Fixed(Color),
}

/// Fixed-size themed glyph widget — see module docs.
pub struct IconWidget {
    icon: Icon,
    /// Box edge length in logical pixels. The glyph is centred in this
    /// square.
    size: f32,
    tint: Tint,
    stroke_px: f64,
    rect: Rect,
}

impl IconWidget {
    /// Construct an icon widget for `icon` at `size × size` logical pixels.
    /// Defaults to the theme's primary text colour and the lucide 1.5 px
    /// stroke.
    pub fn new(icon: Icon, size: f32) -> Self {
        Self {
            icon,
            size,
            tint: Tint::Primary,
            stroke_px: LUCIDE_DEFAULT_STROKE_PX,
            rect: Rect::ZERO,
        }
    }

    /// Builder: tint the glyph with an explicit colour instead of the
    /// theme's primary text colour.
    pub fn with_color(mut self, color: Color) -> Self {
        self.tint = Tint::Fixed(color);
        self
    }

    /// Builder: tint with the theme's dim text colour — for secondary
    /// icons that should recede next to their label.
    pub fn dim(mut self) -> Self {
        self.tint = Tint::Dim;
        self
    }

    /// Builder: override the output stroke width in pixels.
    pub fn with_stroke(mut self, stroke_px: f64) -> Self {
        self.stroke_px = stroke_px;
        self
    }

    /// Which glyph this widget paints.
    pub fn icon(&self) -> Icon {
        self.icon
    }

    /// Resolve the draw colour against `theme`.
    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.tint {
            Tint::Primary => theme.text_primary,
            Tint::Dim => theme.text_dim,
            Tint::Fixed(c) => c,
        }
    }
}

impl Widget for IconWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.size),
                    height: Dimension::length(self.size),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        let color = self.resolve_color(theme);
        self.icon
            .draw_with_stroke(scene, transform, self.rect, color, self.stroke_px);
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

    fn name(&self) -> &str {
        "IconWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Fixed-size leaf: layout is settled once the first non-zero rect
    /// lands.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn paint(w: &mut IconWidget) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.rect = Rect::new(0.0, 0.0, w.size as f64, w.size as f64);
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    /// A laid-out icon widget must emit at least one stroke into the scene.
    #[test]
    fn icon_widget_paints_glyph() {
        let mut w = IconWidget::new(Icon::Home, 24.0);
        let scene = paint(&mut w);
        let color_tags = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(color_tags >= 1, "icon widget must paint its glyph");
    }

    /// A zero-size rect is a no-op (no panic, no draw).
    #[test]
    fn icon_widget_zero_rect_is_noop() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        let mut w = IconWidget::new(Icon::Home, 24.0);
        // rect stays ZERO
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let tags = scene.encoding().draw_tags.len();
        assert_eq!(tags, 0, "no draw work for a zero-size icon");
    }

    /// The colour builders pick the documented theme slots.
    #[test]
    fn icon_widget_color_resolution() {
        let theme = Theme::dark();
        let primary = IconWidget::new(Icon::Star, 16.0);
        assert_eq!(primary.resolve_color(&theme), theme.text_primary);

        let dim = IconWidget::new(Icon::Star, 16.0).dim();
        assert_eq!(dim.resolve_color(&theme), theme.text_dim);

        let fixed = IconWidget::new(Icon::Star, 16.0).with_color(Color::from_rgb8(1, 2, 3));
        assert_eq!(fixed.resolve_color(&theme), Color::from_rgb8(1, 2, 3));
    }
}
