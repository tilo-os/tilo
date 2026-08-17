//! Badge widget — a small pill-shaped indicator typically anchored over
//! another element (avatar, icon, button) to signal new activity or a
//! count of unread items. Three variants exist:
//!
//! - [`BadgeContent::Dot`] — a tiny filled circle, no text. Used for
//!   "has activity" hints where the count itself isn't important.
//! - [`BadgeContent::Count`] — a numeric badge. `0` is treated as a
//!   semantic "nothing to show" and renders nothing; values `>= 100`
//!   clamp to the `"99+"` overflow string so the pill never grows
//!   wider than the visual budget the caller reserved.
//! - [`BadgeContent::Text`] — an arbitrary short text label (typically
//!   1–4 characters), used for things like "NEW" or "β".
//!
//! Positioning is the caller's responsibility — `Badge` does not anchor
//! itself to another widget. The taffy node it returns is a leaf with
//! a fixed measured size, so a caller can drop it into a stack /
//! container that does the positioning.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Color;
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// Body content of a [`Badge`]. See module docs for variant semantics.
#[derive(Clone, Debug)]
pub enum BadgeContent {
    /// Small filled circle, no text. Caller sizes the dot via the
    /// rendering theme (radius = `theme.tokens.spacing.s2`).
    Dot,
    /// Numeric badge. `0` paints nothing (semantic: nothing to show);
    /// values `>= 100` render as the overflow string `"99+"` so the
    /// pill never exceeds the visual budget callers reserve for it.
    Count(u32),
    /// Arbitrary short text (typically 1–4 characters, e.g. `"NEW"`).
    Text(String),
}

pub struct Badge {
    content: BadgeContent,
    rect: Rect,
    cached: vello::Scene,
    dirty: bool,
    cache_rect: Rect,
    /// Optional background tint override. `None` keeps the default
    /// notification treatment (`theme.tokens.colors.semantic.danger`)
    /// for count/dot badges; a labeled "kind tag" pill sets this to a
    /// caller-supplied accent (e.g. a category colour). Stored as a
    /// [`Color`] so it survives across themes — callers wanting a
    /// theme-relative tint pass a resolved theme colour.
    tint: Option<Color>,
}

impl Badge {
    /// Construct a [`BadgeContent::Dot`] badge.
    pub fn dot() -> Self {
        Self {
            content: BadgeContent::Dot,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: None,
        }
    }

    /// Construct a [`BadgeContent::Count`] badge.
    pub fn count(n: u32) -> Self {
        Self {
            content: BadgeContent::Count(n),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: None,
        }
    }

    /// Construct a [`BadgeContent::Text`] badge.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: BadgeContent::Text(s.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: None,
        }
    }

    /// Construct a labeled "kind tag" pill — a [`BadgeContent::Text`] badge
    /// with a caller-supplied background `tint`. Use for category / status
    /// chips ("DRAFT", "Rust", "online") where the colour carries meaning,
    /// as opposed to [`Badge::text`] which uses the notification-red
    /// default. The label renders in `Color::WHITE` on the tint.
    pub fn pill(label: impl Into<String>, tint: Color) -> Self {
        Self {
            content: BadgeContent::Text(label.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            tint: Some(tint),
        }
    }

    /// Builder: override the background tint of any badge variant. Pairs
    /// with [`Badge::count`] / [`Badge::text`] when the caller wants a
    /// non-default colour without switching to [`Badge::pill`].
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = Some(tint);
        self
    }

    /// The background colour this badge will paint with, given `theme`.
    /// Defaults to the semantic danger colour unless a [`tint`] override
    /// was supplied. Exposed so tests can pin the kind-tag colour without
    /// rendering.
    pub fn background(&self, theme: &Theme) -> Color {
        self.tint.unwrap_or(theme.tokens.colors.semantic.danger)
    }

    /// The string that `draw` will hand to the text renderer for this
    /// badge, or `None` when no text should be drawn at all (the dot
    /// variant, or the zero-count "nothing to show" case).
    ///
    /// Centralising the clamp / "99+" / suppression logic here lets the
    /// tests pin the contract without rendering into a Vello scene.
    pub fn display_text(&self) -> Option<String> {
        match &self.content {
            BadgeContent::Dot => None,
            BadgeContent::Count(0) => None,
            BadgeContent::Count(n) if *n >= 100 => Some("99+".to_string()),
            BadgeContent::Count(n) => Some(n.to_string()),
            BadgeContent::Text(s) => Some(s.clone()),
        }
    }
}

impl Widget for Badge {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The pill height is twice the dot radius — i.e. the dot fits
        // perfectly inside a single-character pill. For the text/count
        // variants we widen by a tight character-width approximation;
        // the actual on-screen text width is recomputed in `draw` via
        // the text renderer, so this is just a layout hint.
        let dot_radius = theme.tokens.spacing.s2 as f64;
        let pad_x = theme.tokens.spacing.s2 as f64;
        let height = dot_radius * 2.0;
        let width = match &self.content {
            BadgeContent::Dot => height,
            BadgeContent::Count(0) => 0.0,
            _ => {
                // Approximate character advance at the badge text size.
                // The draw pass overlays text using the actual measured
                // width; this hint only determines the pill rect taffy
                // hands us in `apply_layout`.
                let s = self.display_text().unwrap_or_default();
                let approx_char_w = 7.0_f64;
                (s.chars().count() as f64 * approx_char_w + pad_x * 2.0).max(height)
            }
        };
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(width as f32),
                    height: Dimension::length(height as f32),
                },
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
            out_scene.append(&self.cached, Some(out_transform * Affine::translate((dx, dy))));
            return;
        }
        // mem::take dance — `display_text` borrows &self while
        // `scene` borrows `&mut self.cached`.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        let transform = Affine::IDENTITY;
        let bg = self.background(theme);

        match &self.content {
            BadgeContent::Dot => {
                let radius = theme.tokens.spacing.s2 as f64;
                let circle = Circle::new(self.rect.center(), radius);
                cached.fill(vello::peniko::Fill::NonZero, transform, bg, None, &circle);
            }
            BadgeContent::Count(_) | BadgeContent::Text(_) => {
                let radius = theme.tokens.radii.full as f64;
                let pill = RoundedRect::from_rect(self.rect, radius);
                cached.fill(vello::peniko::Fill::NonZero, transform, bg, None, &pill);

                if let Some(label) = self.display_text() {
                    let text_size = theme.tokens.typography.scale.xs;
                    let (tw, _th) = text_renderer.measure(&label, text_size, None);
                    let tx = self.rect.center().x - tw as f64 / 2.0;
                    // Optically centre the label in the pill so it sits level
                    // with the box middle, not ~0.2 em low (shared helper).
                    let ty = crate::text_render::centered_text_y(self.rect.y0, self.rect.height(), text_size);
                    text_renderer.draw_text(
                        &mut cached,
                        &label,
                        text_size,
                        Color::WHITE,
                        transform * Affine::translate((tx, ty)),
                        None,
                    );
                }
            }
        }
        out_scene.append(&cached, Some(out_transform));
        self.cached = cached;
        self.cache_rect = self.rect;
    }

    fn needs_redraw(&self) -> bool { self.dirty }
    fn mark_drawn(&mut self) { self.dirty = false; }
    fn is_layout_dirty(&self) -> bool { self.cache_rect == Rect::ZERO }

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
        // Badges are purely informational — no input handling.
    }

    fn name(&self) -> &str { "Badge" }

    fn get_rect(&self) -> Rect { self.rect }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // A badge is a small, non-focusable status label. Its visible text
        // (count / "99+" / label) is the accessible value; the dot variant and
        // the zero-count "nothing to show" case have no text and so no value.
        Some(AccessNode::new(Role::StaticText).with_optional_value(self.display_text()))
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use vello_encoding::DrawTag;

    /// Count COLOR draw tags emitted by `draw`. COLOR is Vello's tag for
    /// a solid-colour fill — circles, rounded rects, and glyph runs all
    /// emit it, so a delta of zero proves nothing was painted at all.
    /// Mirrors the walk used in `tabs.rs` (search "DrawTag::COLOR").
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

    /// `Badge::count(0)` must paint absolutely nothing — semantically,
    /// "zero unread" should never show a badge over the parent element.
    /// Asserting via the Vello encoding (no COLOR draw tags) is the
    /// strongest possible check: if any code path leaks a stray fill
    /// (pill, dot, glyph), the encoding picks it up.
    #[test]
    fn badge_count_zero_paints_nothing() {
        let mut b = Badge::count(0);
        let fills = color_fill_count(&mut b);
        assert_eq!(
            fills, 0,
            "Badge::count(0) must not emit any COLOR draw tags (got {fills})",
        );
    }

    /// `Badge::count(100)` (and anything beyond) must render the
    /// overflow string `"99+"` so the badge stays inside its visual
    /// budget. Pinning via `display_text` keeps the contract visible
    /// without rendering — and any refactor that recomputes the clamp
    /// in `draw` directly has to also drop this accessor, which makes
    /// the regression impossible to miss in review.
    #[test]
    fn badge_count_one_hundred_paints_99plus() {
        assert_eq!(
            Badge::count(100).display_text().as_deref(),
            Some("99+"),
            "values >= 100 must clamp to the \"99+\" overflow string",
        );
        // Also pin a larger value so a future "only 100 is special"
        // off-by-one regression flips this red.
        assert_eq!(
            Badge::count(9_999).display_text().as_deref(),
            Some("99+"),
        );
        // And the boundary just below — `99` itself must NOT clamp,
        // otherwise the threshold has drifted.
        assert_eq!(
            Badge::count(99).display_text().as_deref(),
            Some("99"),
        );
    }

    /// A count badge reports the `StaticText` role and surfaces its visible
    /// text as the accessible value; the dot variant has no value.
    #[test]
    fn accessibility_reports_static_text_role_and_value() {
        use crate::a11y::Role;
        let b = Badge::count(3);
        let node = Widget::accessibility(&b).expect("badge is not presentational");
        assert_eq!(node.role, Role::StaticText);
        assert_eq!(node.value.as_deref(), Some("3"));
        assert!(!node.state.focusable, "a badge is not focusable");

        // Overflow clamps in the value too.
        let over = Badge::count(250);
        assert_eq!(Widget::accessibility(&over).unwrap().value.as_deref(), Some("99+"));

        // Dot variant carries no text → no accessible value.
        let dot = Badge::dot();
        assert_eq!(Widget::accessibility(&dot).unwrap().value, None);
    }

    /// A `Badge::pill` carries a caller-supplied tint while a plain
    /// `Badge::text` falls back to the semantic danger colour. The
    /// `background` accessor pins this without rendering, and the pill's
    /// label still resolves through `display_text`.
    #[test]
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

    /// The count / text label is optically vertically centred in the pill.
    /// The draw path places the label top at
    /// `centered_text_y(rect.y0, rect.height, xs)`; assert that puts its
    /// optical centre on the pill centre (within tolerance) and lifts it above
    /// the naive box-centre that read low.
    #[test]
    fn label_is_optically_vertically_centered() {
        use crate::text_render::{centered_text_y, OPTICAL_LIFT};
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 24.0, 16.0); // a representative count pill
        let size = theme.tokens.typography.scale.xs;
        let top = centered_text_y(rect.y0, rect.height(), size);
        let optical_center = top + size as f64 * (0.5 + OPTICAL_LIFT);
        assert!(
            (optical_center - rect.center().y).abs() < 0.5,
            "badge label optical centre {optical_center} must sit on the pill centre {}",
            rect.center().y,
        );
        let naive = rect.y0 + (rect.height() - size as f64) / 2.0;
        assert!(top < naive, "badge label must lift above the naive-low baseline");
    }

    /// `Badge::dot()` must emit exactly one fill — the filled circle.
    /// No pill background, no glyphs, no shadow. Inspecting via
    /// `vello_encoding::DrawTag::COLOR` is the same walk `tabs.rs`
    /// uses to pin pill-chrome paint counts.
    #[test]
    fn badge_dot_paints_filled_circle() {
        let mut b = Badge::dot();
        let fills = color_fill_count(&mut b);
        assert_eq!(
            fills, 1,
            "Badge::dot() must emit exactly one COLOR draw tag (the \
             filled circle); got {fills}",
        );
    }
}
