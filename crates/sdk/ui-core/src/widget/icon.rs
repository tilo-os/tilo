//! `IconWidget` — a leaf that paints one themed vector glyph from the
//! shared [`crate::icons::Icon`] set, at one of the theme's three icon
//! sizes ([`IconSize`]).
//!
//! Wraps the immediate-mode [`crate::icons::Icon::draw`] helper in the
//! [`Widget`] contract so an icon can be dropped straight into a
//! `Row`/`Column`/`Container` like any other widget, sized by the layout
//! pass, and tinted from the active [`Theme`].
//!
//! Defaults to [`Theme::text_primary`] and the lucide 1.5px stroke; see
//! [`IconWidget::with_color`], [`IconWidget::dim`], [`IconWidget::with_stroke`].

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Where the glyph colour comes from.
#[derive(Clone, Copy, Debug)]
enum Tint {
    Primary,
    Dim,
    Accent,
    Fixed(Color),
}

/// Which of the theme's icon sizes a glyph takes: `metrics.icon_sm`,
/// `icon_md` or `icon_lg` (14, 20 and 24 by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSize {
    /// Beside a caption or inside a small control.
    Small,
    /// Beside a row's label; the default.
    #[default]
    Regular,
    /// A header mark, a glyph beside a title.
    Large,
}

impl IconSize {
    /// The edge length this size resolves to in `theme`.
    #[must_use]
    pub fn px(self, theme: &Theme) -> f32 {
        let metrics = &theme.tokens.metrics;
        match self {
            Self::Small => metrics.icon_sm,
            Self::Regular => metrics.icon_md,
            Self::Large => metrics.icon_lg,
        }
    }
}

/// Themed glyph widget — see module docs.
pub struct IconWidget {
    icon: Icon,
    /// The square the glyph is centred in, resolved from the theme at
    /// layout.
    size: IconSize,
    tint: Tint,
    stroke_px: f64,
    rect: Rect,
}

impl IconWidget {
    /// `icon` at [`IconSize::Regular`].
    #[must_use]
    pub fn new(icon: Icon) -> Self {
        Self {
            icon,
            size: IconSize::Regular,
            tint: Tint::Primary,
            stroke_px: LUCIDE_DEFAULT_STROKE_PX,
            rect: Rect::ZERO,
        }
    }

    /// Take one of the theme's icon sizes.
    #[must_use]
    pub fn sized(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.tint = Tint::Fixed(color);
        self
    }

    /// For secondary icons that should recede next to their label.
    #[must_use]
    pub fn dim(mut self) -> Self {
        self.tint = Tint::Dim;
        self
    }

    /// Resolve the glyph through the active theme's primary accent. Prefer
    /// this over capturing `Theme::dark().tokens.colors.intent.inverted` at construction so
    /// light, custom, and high-contrast themes remain truthful.
    #[must_use]
    pub fn accent(mut self) -> Self {
        self.tint = Tint::Accent;
        self
    }

    #[must_use]
    pub fn with_stroke(mut self, stroke_px: f64) -> Self {
        self.stroke_px = stroke_px;
        self
    }

    pub fn icon(&self) -> Icon {
        self.icon
    }

    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.tint {
            Tint::Primary => theme.text_primary,
            Tint::Dim => theme.text_dim,
            Tint::Accent => theme.tokens.colors.intent.inverted,
            Tint::Fixed(c) => c,
        }
    }
}

impl Widget for IconWidget {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let edge = self.size.px(theme);
        let style = Style {
            size: Size {
                width: Dimension::length(edge),
                height: Dimension::length(edge),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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

    fn name(&self) -> &'static str {
        "IconWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A leaf sized by the theme: layout is settled once the first
    /// non-zero rect lands.
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
        let edge = f64::from(w.size.px(&theme));
        w.rect = Rect::new(0.0, 0.0, edge, edge);
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    #[test]
    fn icon_widget_paints_resolved_glyph_or_nothing() {
        /// A laid-out icon widget must emit at least one stroke into the scene.
        fn icon_widget_paints_glyph() {
            let mut w = IconWidget::new(Icon::Home).sized(IconSize::Large);
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
        fn icon_widget_zero_rect_is_noop() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            let mut w = IconWidget::new(Icon::Home).sized(IconSize::Large);
            // rect stays ZERO
            w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let tags = scene.encoding().draw_tags.len();
            assert_eq!(tags, 0, "no draw work for a zero-size icon");
        }

        /// The colour builders pick the documented theme slots.
        fn icon_widget_color_resolution() {
            let theme = Theme::dark();
            let primary = IconWidget::new(Icon::Star);
            assert_eq!(primary.resolve_color(&theme), theme.text_primary);

            let dim = IconWidget::new(Icon::Star).dim();
            assert_eq!(dim.resolve_color(&theme), theme.text_dim);

            let fixed = IconWidget::new(Icon::Star).with_color(Color::from_rgb8(1, 2, 3));
            assert_eq!(fixed.resolve_color(&theme), Color::from_rgb8(1, 2, 3));
        }

        /// Each size resolves from the theme's icon metrics.
        fn icon_widget_sizes_follow_the_theme() {
            let theme = Theme::dark();
            let metrics = &theme.tokens.metrics;
            assert_eq!(IconSize::Small.px(&theme), metrics.icon_sm);
            assert_eq!(IconSize::Regular.px(&theme), metrics.icon_md);
            assert_eq!(IconSize::Large.px(&theme), metrics.icon_lg);
            assert_eq!(IconWidget::new(Icon::Star).size, IconSize::Regular);
        }

        icon_widget_paints_glyph();
        icon_widget_sizes_follow_the_theme();
        icon_widget_zero_rect_is_noop();
        icon_widget_color_resolution();
    }
}
