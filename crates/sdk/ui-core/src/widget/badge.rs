//! Badge widget — the non-interactive member of the chip family: a small
//! pill typically anchored over another element (avatar, icon, row). Three
//! contents:
//!
//! - [`BadgeContent::Dot`] — a small filled circle, no text.
//! - [`BadgeContent::Count`] — `0` renders nothing; values `>= 100` clamp to
//!   `"99+"` so the pill never grows past the visual budget reserved for it.
//! - [`BadgeContent::Text`] — a short status ("Owner", "Beta").
//!
//! A badge takes a [`ChipStyle`], never a colour. A count or a dot is
//! [`ChipStyle::Live`] by default (something new happened); a text badge is
//! [`ChipStyle::Neutral`]. A status reads as `ChipStyle::Tinted(Tone::…)`.
//! Canvas hosts paint the same pill with [`BadgeFace`].
//!
//! Positioning is the caller's responsibility — `Badge` does not anchor
//! itself to another widget.

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::chip::{ChipLead, ChipStyle, PillPaint, paint_pill};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

/// See module docs for content semantics.
#[derive(Clone, Debug)]
pub enum BadgeContent {
    Dot,
    Count(u32),
    Text(String),
}

/// A badge's geometry in one theme.
#[derive(Clone, Debug, PartialEq)]
struct BadgeMetrics {
    /// The caption line plus `spacing.s0` a side: a pill as tight as a
    /// badge can be and still hold its text at any text scale.
    height: f64,
    pad_x: f64,
    dot: f64,
    caption: crate::text_render::TextStyle,
}

impl BadgeMetrics {
    fn of(theme: &Theme) -> Self {
        let line = f64::from(theme.text(TextRole::Caption).line_height_px());
        let height = (line + 2.0 * f64::from(theme.tokens.spacing.s0)).ceil();
        Self {
            height,
            pad_x: f64::from(theme.tokens.spacing.s2).max((height / 3.0).round()),
            dot: f64::from(theme.tokens.spacing.s2),
            caption: theme.text(TextRole::Caption),
        }
    }

    fn width(&self, label: &str, text: &mut TextRenderer) -> f64 {
        if label.is_empty() {
            return self.dot;
        }
        let (w, _) = text.measure_style(label, None, &self.caption);
        (f64::from(w) + 2.0 * self.pad_x).ceil().max(self.height)
    }
}

/// An immediate-mode badge for hosts that own their row geometry (shell
/// overlays, painted lists): the [`Badge`] look without a layout node.
pub struct BadgeFace<'a> {
    rect: Rect,
    label: &'a str,
    style: ChipStyle,
}

impl<'a> BadgeFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            style: ChipStyle::Neutral,
        }
    }
    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }
    /// The badge height in `theme`.
    pub fn height(theme: &Theme) -> f64 {
        BadgeMetrics::of(theme).height
    }
    /// The width a badge needs for `label` in `theme`.
    pub fn intrinsic_width(label: &str, text: &mut TextRenderer, theme: &Theme) -> f64 {
        BadgeMetrics::of(theme).width(label, text)
    }
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.label.is_empty() {
            return;
        }
        paint_badge_pill(
            scene,
            transform,
            self.rect,
            self.label,
            self.style,
            text_renderer,
            theme,
        );
    }
}

fn paint_badge_pill(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    style: ChipStyle,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    paint_pill(
        &PillPaint {
            rect,
            label,
            lead: ChipLead::None,
            style,
            selected: false,
            hovered: false,
            remove: None,
            bare: false,
            opacity: 1.0,
            role: TextRole::Caption,
            centred: true,
        },
        scene,
        transform,
        text_renderer,
        theme,
    );
}

pub struct Badge {
    content: BadgeContent,
    style: ChipStyle,
    rect: Rect,
    cached: vello::Scene,
    dirty: bool,
    cache_rect: Rect,
    /// The geometry seen in `build_layout`, for the measured width.
    metrics: Option<BadgeMetrics>,
}

impl Badge {
    fn with_content(content: BadgeContent, style: ChipStyle) -> Self {
        Self {
            content,
            style,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            metrics: None,
        }
    }

    /// Something new, without a number.
    pub fn dot() -> Self {
        Self::with_content(BadgeContent::Dot, ChipStyle::Live)
    }

    /// How many new things there are.
    pub fn count(n: u32) -> Self {
        Self::with_content(BadgeContent::Count(n), ChipStyle::Live)
    }

    /// A short status ("Owner", "Beta").
    pub fn text(s: impl Into<String>) -> Self {
        Self::with_content(BadgeContent::Text(s.into()), ChipStyle::Neutral)
    }

    /// What the badge is: neutral, tinted with a tone, or live.
    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }

    /// `None` when no text should be drawn (the dot variant, or the
    /// zero-count "nothing to show" case). Centralising the clamp/"99+"
    /// logic here lets tests pin the contract without rendering.
    pub fn display_text(&self) -> Option<String> {
        match &self.content {
            BadgeContent::Dot | BadgeContent::Count(0) => None,
            BadgeContent::Count(n) if *n >= 100 => Some("99+".to_string()),
            BadgeContent::Count(n) => Some(n.to_string()),
            BadgeContent::Text(s) => Some(s.clone()),
        }
    }
}

impl Widget for Badge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = BadgeMetrics::of(theme);
        let (width, height) = match &self.content {
            BadgeContent::Dot => (metrics.dot, metrics.dot),
            BadgeContent::Count(0) => (0.0, 0.0),
            // The measured label width lands in `adjust_layout_with_measure`.
            _ => (metrics.height, metrics.height),
        };
        if self.metrics.as_ref() != Some(&metrics) {
            self.dirty = true;
        }
        self.metrics = Some(metrics);
        // Layout dimensions are bounded logical-pixel measurements; the
        // f64->f32 narrowing for taffy's `Dimension` cannot lose meaningful
        // precision at UI scale.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel layout size; f32 precision is sufficient"
        )]
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(width as f32),
                height: Dimension::length(height as f32),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// A text or count badge is as wide as its measured label.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let (Some(label), Some(metrics)) = (self.display_text(), self.metrics.as_ref()) else {
            return false;
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel width; f32 precision is sufficient"
        )]
        let width = Dimension::length(metrics.width(&label, text) as f32);
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        if style.size.width == width {
            return false;
        }
        let mut style = style.clone();
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Exact equality is the point — this is change detection for the
        // paint cache, not a numerical comparison that needs a tolerance.
        #[allow(
            clippy::float_cmp,
            reason = "exact-change detection for the paint cache, not numerical comparison"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        if matches!(self.content, BadgeContent::Count(0)) {
            return;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            out_scene.append(
                &self.cached,
                Some(out_transform.pre_translate((dx, dy).into())),
            );
            return;
        }
        // mem::take dance — `display_text` borrows &self while `cached` needs &mut self.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        let transform = Affine::IDENTITY;
        match self.display_text() {
            None => {
                let colors =
                    self.style
                        .colors(theme, theme.tokens.colors.surfaces.content, false, false);
                let radius = BadgeMetrics::of(theme).dot / 2.0;
                let circle = Circle::new(self.rect.center(), radius);
                cached.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    colors.fill,
                    None,
                    &circle,
                );
            }
            Some(label) => {
                paint_badge_pill(
                    &mut cached,
                    transform,
                    self.rect,
                    &label,
                    self.style,
                    text_renderer,
                    theme,
                );
            }
        }
        out_scene.append(&cached, Some(out_transform));
        self.cached = cached;
        self.cache_rect = self.rect;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Badge"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The dot variant and zero-count case have no text and so no value.
        Some(AccessNode::new(Role::StaticText).with_optional_value(self.display_text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::widget::Tone;
    use vello_encoding::DrawTag;

    /// COLOR is Vello's tag for a solid-colour fill — circles, rounded
    /// rects, and glyph runs all emit it, so a delta of zero proves nothing
    /// was painted at all.
    fn color_fill_count(badge: &mut Badge) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        badge.rect = Rect::new(0.0, 0.0, 24.0, 16.0);
        badge.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn badge_paints_count_dot_and_text_variants() {
        let mut zero = Badge::count(0);
        assert_eq!(
            color_fill_count(&mut zero),
            0,
            "a zero count paints nothing"
        );

        assert_eq!(Badge::count(100).display_text().as_deref(), Some("99+"));
        assert_eq!(Badge::count(9_999).display_text().as_deref(), Some("99+"));
        assert_eq!(Badge::count(99).display_text().as_deref(), Some("99"));

        let mut dot = Badge::dot();
        assert_eq!(color_fill_count(&mut dot), 1, "a dot is one filled circle");

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        BadgeFace::new(Rect::new(0.0, 0.0, 72.0, 20.0), "Owner")
            .style(ChipStyle::Tinted(Tone::Info))
            .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(scene.encoding().draw_tags.contains(&DrawTag::COLOR));
    }

    #[test]
    fn a_text_badge_is_as_wide_as_its_label_at_every_text_scale() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(crate::widget::Row::new().push(Box::new(
                    Badge::text("Maintainer").style(ChipStyle::Tinted(Tone::Success)),
                ))),
                (600, 200),
                theme.clone(),
            );
            let rect = harness.inspection().root.children[0].rect;
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            let (label, line) =
                text.measure_style("Maintainer", None, &theme.text(TextRole::Caption));
            assert!(rect[2] > f64::from(label), "{scale}x: {rect:?}");
            assert!(rect[3] >= f64::from(line), "{scale}x: {rect:?}");
        }
    }

    #[test]
    fn accessibility_reports_static_text_role_and_value() {
        use crate::a11y::Role;
        let b = Badge::count(3);
        let node = Widget::accessibility(&b).expect("badge is not presentational");
        assert_eq!(node.role, Role::StaticText);
        assert_eq!(node.value.as_deref(), Some("3"));
        assert!(!node.state.focusable, "a badge is not focusable");
        assert_eq!(
            Widget::accessibility(&Badge::count(250))
                .unwrap()
                .value
                .as_deref(),
            Some("99+")
        );
        assert_eq!(Widget::accessibility(&Badge::dot()).unwrap().value, None);
    }
}
