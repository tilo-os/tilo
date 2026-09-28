//! Badge widget — a small pill-shaped indicator typically anchored over
//! another element (avatar, icon, button). Three variants:
//!
//! - [`BadgeContent::Dot`] — a tiny filled circle, no text.
//! - [`BadgeContent::Count`] — `0` renders nothing; values `>= 100` clamp to
//!   `"99+"` so the pill never grows past the visual budget reserved for it.
//! - [`BadgeContent::Text`] — an arbitrary short label ("NEW", "β").
//!
//! Positioning is the caller's responsibility — `Badge` does not anchor
//! itself to another widget.

use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Color;

/// See module docs for variant semantics.
#[derive(Clone, Debug)]
pub enum BadgeContent {
    Dot,
    Count(u32),
    Text(String),
}

/// Theme-relative semantic treatment for status pills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeTone {
    Success,
    Warning,
    Neutral,
    Accent,
}

#[derive(Clone, Copy, Debug)]
enum BadgeTint {
    Default,
    Fixed(Color),
    Semantic(BadgeTone),
}

pub struct Badge {
    content: BadgeContent,
    rect: Rect,
    cached: vello::Scene,
    dirty: bool,
    cache_rect: Rect,
    /// Notification default, fixed caller colour, or a theme-relative status
    /// tone.
    tint: BadgeTint,
}

impl Badge {
    pub fn dot() -> Self {
        Self {
            content: BadgeContent::Dot,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: BadgeTint::Default,
        }
    }

    pub fn count(n: u32) -> Self {
        Self {
            content: BadgeContent::Count(n),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: BadgeTint::Default,
        }
    }

    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: BadgeContent::Text(s.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: BadgeTint::Default,
        }
    }

    /// A labeled "kind tag" pill where the colour carries meaning, unlike
    /// [`Badge::text`]'s notification-red default. Ink is derived against
    /// `tint` so a pale caller-supplied colour still gets readable text.
    pub fn pill(label: impl Into<String>, tint: Color) -> Self {
        Self {
            content: BadgeContent::Text(label.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: BadgeTint::Fixed(tint),
        }
    }

    /// A status pill whose colour follows the active theme.
    pub fn semantic_pill(label: impl Into<String>, tone: BadgeTone) -> Self {
        Self {
            content: BadgeContent::Text(label.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: BadgeTint::Semantic(tone),
        }
    }

    /// Pairs with [`Badge::count`] / [`Badge::text`] without switching to [`Badge::pill`].
    #[must_use]
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = BadgeTint::Fixed(tint);
        self
    }

    /// Exposed so tests can pin the kind-tag colour without rendering.
    pub fn background(&self, theme: &Theme) -> Color {
        match self.tint {
            BadgeTint::Default => theme.tokens.colors.semantic.danger,
            BadgeTint::Fixed(color) => color,
            BadgeTint::Semantic(BadgeTone::Success) => theme.status_success,
            BadgeTint::Semantic(BadgeTone::Warning) => theme.tokens.colors.semantic.warning,
            BadgeTint::Semantic(BadgeTone::Neutral) => theme.text_dim,
            BadgeTint::Semantic(BadgeTone::Accent) => theme.accent_primary,
        }
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

    /// Immediate-mode counterpart to the retained badge widget. Shell
    /// overlays commonly own their own row geometry, so this preserves the
    /// shared badge's tint, readable ink, and text metrics without forcing a
    /// throwaway Taffy node for every frame.
    pub fn paint_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        label: &str,
        tint: Color,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 || label.is_empty() {
            return;
        }
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.full));
        scene.fill(vello::peniko::Fill::NonZero, transform, tint, None, &shape);
        let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);
        let (tw, th) = text_renderer.measure(label, size, None);
        text_renderer.draw_text(
            scene,
            label,
            size,
            theme.tokens.colors.ink_on_any(
                &[tint],
                theme.tokens.colors.text_primary,
                crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST,
            ),
            translated(
                transform,
                rect.center().x - f64::from(tw) / 2.0,
                rect.center().y - f64::from(th) / 2.0,
            ),
            None,
        );
    }
}

/// Composes `transform` with a translation by `(dx, dy)`. `kurbo::Affine`'s
/// `Mul` impl trips `arithmetic_side_effects` like any operator overload,
/// but composing two f64-based affine transforms cannot meaningfully
/// overflow — same reasoning clippy already exempts bare `f64` arithmetic
/// for.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine::mul composes f64 transform matrices; cannot meaningfully overflow"
)]
fn translated(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

impl Widget for Badge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Height is twice the dot radius, so the dot fits perfectly inside a
        // single-character pill. Text/count width is a rough approximation —
        // the real width is recomputed in `draw`, so this is just a layout hint.
        let dot_radius = f64::from(theme.tokens.spacing.s2);
        let pad_x = f64::from(theme.tokens.spacing.s2);
        let height = dot_radius * 2.0;
        let width = match &self.content {
            BadgeContent::Dot => height,
            BadgeContent::Count(0) => 0.0,
            _ => {
                let s = self.display_text().unwrap_or_default();
                let approx_char_w = 7.0_f64;
                // A badge label is a handful of characters ("99+", a short
                // word), so the char count is well within f64 precision.
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "badge label length is a tiny UI count, well within f64 precision"
                )]
                let char_count = s.chars().count() as f64;
                (char_count * approx_char_w + pad_x * 2.0).max(height)
            }
        };
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
            out_scene.append(&self.cached, Some(translated(out_transform, dx, dy)));
            return;
        }
        // mem::take dance — `display_text` borrows &self while `cached` needs &mut self.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        let transform = Affine::IDENTITY;
        let bg = self.background(theme);

        match &self.content {
            BadgeContent::Dot => {
                let radius = f64::from(theme.tokens.spacing.s2);
                let circle = Circle::new(self.rect.center(), radius);
                cached.fill(vello::peniko::Fill::NonZero, transform, bg, None, &circle);
            }
            BadgeContent::Count(_) | BadgeContent::Text(_) => {
                let radius = f64::from(theme.tokens.radii.full);
                let pill = RoundedRect::from_rect(self.rect, radius);
                cached.fill(vello::peniko::Fill::NonZero, transform, bg, None, &pill);

                if let Some(label) = self.display_text() {
                    let text_size =
                        renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);
                    let (tw, th) = text_renderer.measure(&label, text_size, None);
                    let tx = self.rect.center().x - f64::from(tw) / 2.0;
                    let ty = self.rect.center().y - f64::from(th) / 2.0;
                    text_renderer.draw_text(
                        &mut cached,
                        &label,
                        text_size,
                        theme.tokens.colors.ink_on_any(
                            &[bg],
                            theme.tokens.colors.text_primary,
                            crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST,
                        ),
                        translated(transform, tx, ty),
                        None,
                    );
                }
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
    fn badge_paints_count_dot_and_pill_variants() {
        /// Asserting via the Vello encoding is the strongest possible check: if
        /// any code path leaks a stray fill (pill, dot, glyph), it's caught.
        fn badge_count_zero_paints_nothing() {
            let mut b = Badge::count(0);
            let fills = color_fill_count(&mut b);
            assert_eq!(
                fills, 0,
                "Badge::count(0) must not emit any COLOR draw tags (got {fills})",
            );
        }

        fn badge_count_one_hundred_paints_99plus() {
            assert_eq!(
                Badge::count(100).display_text().as_deref(),
                Some("99+"),
                "values >= 100 must clamp to the \"99+\" overflow string",
            );
            // A larger value, so a future "only 100 is special" off-by-one doesn't slip by.
            assert_eq!(Badge::count(9_999).display_text().as_deref(), Some("99+"),);
            // The boundary just below: `99` must NOT clamp.
            assert_eq!(Badge::count(99).display_text().as_deref(), Some("99"),);
        }

        /// Exactly one fill — the circle. No pill background, no glyphs, no shadow.
        fn badge_dot_paints_filled_circle() {
            let mut b = Badge::dot();
            let fills = color_fill_count(&mut b);
            assert_eq!(
                fills, 1,
                "Badge::dot() must emit exactly one COLOR draw tag (the \
             filled circle); got {fills}",
            );
        }

        fn immediate_pill_uses_the_shared_badge_painter() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            Badge::paint_at(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 72.0, 20.0),
                "Owner",
                theme.tokens.colors.accent[5],
                &mut tr,
                &theme,
            );
            assert!(
                scene.encoding().draw_tags.contains(&DrawTag::COLOR),
                "the immediate badge bridge must paint its pill rather than becoming a no-op"
            );
        }

        badge_count_zero_paints_nothing();
        badge_count_one_hundred_paints_99plus();
        badge_dot_paints_filled_circle();
        immediate_pill_uses_the_shared_badge_painter();
    }

    #[test]
    fn badge_semantics_and_theme_colours() {
        fn accessibility_reports_static_text_role_and_value() {
            use crate::a11y::Role;
            let b = Badge::count(3);
            let node = Widget::accessibility(&b).expect("badge is not presentational");
            assert_eq!(node.role, Role::StaticText);
            assert_eq!(node.value.as_deref(), Some("3"));
            assert!(!node.state.focusable, "a badge is not focusable");

            // Overflow clamps in the value too.
            let over = Badge::count(250);
            assert_eq!(
                Widget::accessibility(&over).unwrap().value.as_deref(),
                Some("99+")
            );

            // Dot variant carries no text → no accessible value.
            let dot = Badge::dot();
            assert_eq!(Widget::accessibility(&dot).unwrap().value, None);
        }

        fn pill_uses_tint_text_uses_danger_default() {
            let theme = Theme::dark();
            let tint = Color::from_rgb8(10, 200, 90);
            let pill = Badge::pill("ONLINE", tint);
            assert_eq!(pill.background(&theme), tint, "pill must paint its tint");
            assert_eq!(pill.display_text().as_deref(), Some("ONLINE"));

            let plain = Badge::text("NEW");
            assert_eq!(
                plain.background(&theme),
                theme.tokens.colors.semantic.danger,
                "plain text badge keeps the notification-red default",
            );

            // `with_tint` overrides a count badge's colour too.
            let counted = Badge::count(3).with_tint(tint);
            assert_eq!(counted.background(&theme), tint);
        }

        accessibility_reports_static_text_role_and_value();
        pill_uses_tint_text_uses_danger_default();
    }
}
