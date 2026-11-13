//! `IconWidget` — a leaf that paints one themed vector glyph from the shared
//! [`crate::icons::Icon`] set, at one of the theme's three icon sizes ([`IconSize`]).

use std::collections::HashMap;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

#[path = "icon_component.rs"]
mod component;
pub use component::IconProps;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tint {
    Primary,
    Dim,
    Accent,
    Tone(crate::widget::Tone),
    Fixed(Color),
}

/// Which of the theme's icon sizes a glyph takes: `metrics.icon_sm`, `icon_md` or `icon_lg`
/// (14, 20 and 24 by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSize {
    Small,
    #[default]
    Regular,
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
    size: IconSize,
    tint: Tint,
    stroke_px: f64,
    description: Option<String>,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
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
            description: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
        }
    }

    /// What the glyph means, where it says something on its own ("Mail: the server could
    /// not be reached"): read on hover, as a control's tooltip is.
    #[must_use]
    pub fn describing(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    fn set_description(&mut self, description: Option<String>) {
        if self.description != description {
            self.description = description;
            self.dirty = true;
        }
    }

    fn set_glyph(&mut self, icon: Icon, size: IconSize, tint: Tint) {
        if self.icon == icon && self.size == size && self.tint == tint {
            return;
        }
        self.icon = icon;
        self.size = size;
        self.tint = tint;
        self.dirty = true;
        self.layout_dirty = true;
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

    /// Resolve the glyph through the active theme's primary accent.
    #[must_use]
    pub fn accent(mut self) -> Self {
        self.tint = Tint::Accent;
        self
    }

    /// Resolve the glyph through a status's colour in the active theme: a passed check in
    /// success, a warning sign in warning.
    #[must_use]
    pub fn tone(mut self, tone: crate::widget::Tone) -> Self {
        self.tint = Tint::Tone(tone);
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
            Tint::Accent => theme.tokens.colors.intent.accent_ink,
            Tint::Tone(tone) => tone.colour(theme),
            Tint::Fixed(c) => c,
        }
    }
}

impl Widget for IconWidget {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let edge = self.size.px(theme);
        let style = Style {
            size: Size::from_lengths(edge, edge),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!(measured);

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
        if self.icon.is_brand() {
            self.icon.draw_filled(scene, transform, self.rect, color);
        } else {
            let stroke = if (self.stroke_px - LUCIDE_DEFAULT_STROKE_PX).abs() < f64::EPSILON {
                f64::from(theme.tokens.icon_stroke_px)
            } else {
                self.stroke_px
            };
            self.icon
                .draw_with_stroke(scene, transform, self.rect, color, stroke);
        }
    }

    fn name(&self) -> &'static str {
        "IconWidget"
    }

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.description
            .as_deref()
            .filter(|it| !it.is_empty())
            .map(std::borrow::Cow::Borrowed)
    }

    crate::redraw_flag!();
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

        fn icon_widget_zero_rect_is_noop() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            let mut w = IconWidget::new(Icon::Home).sized(IconSize::Large);
            w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let tags = scene.encoding().draw_tags.len();
            assert_eq!(tags, 0, "no draw work for a zero-size icon");
        }

        fn icon_widget_color_resolution() {
            let theme = Theme::dark();
            let primary = IconWidget::new(Icon::Star);
            assert_eq!(primary.resolve_color(&theme), theme.text_primary);

            let dim = IconWidget::new(Icon::Star).dim();
            assert_eq!(dim.resolve_color(&theme), theme.text_dim);

            let fixed = IconWidget::new(Icon::Star).with_color(Color::from_rgb8(1, 2, 3));
            assert_eq!(fixed.resolve_color(&theme), Color::from_rgb8(1, 2, 3));
        }

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

    #[test]
    fn an_icon_says_what_it_means_on_hover() {
        let mut w = IconWidget::new(Icon::WifiOff).describing("Mail: no route to host");
        w.rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        assert_eq!(w.tooltip().as_deref(), Some("Mail: no route to host"));
        assert_eq!(
            w.tooltip_target_at(vello::kurbo::Point::new(10.0, 10.0)),
            Some((w.rect, "Mail: no route to host".to_owned()))
        );

        let mut plain = IconWidget::new(Icon::WifiOff);
        plain.rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        assert_eq!(plain.tooltip(), None);
        assert_eq!(
            plain.tooltip_target_at(vello::kurbo::Point::new(10.0, 10.0)),
            None
        );
    }
}
