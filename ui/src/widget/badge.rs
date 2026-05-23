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
    last_rect: Rect,
}

impl Badge {
    /// Construct a [`BadgeContent::Dot`] badge.
    pub fn dot() -> Self {
        Self {
            content: BadgeContent::Dot,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            last_rect: Rect::ZERO,
        }
    }

    /// Construct a [`BadgeContent::Count`] badge.
    pub fn count(n: u32) -> Self {
        Self {
            content: BadgeContent::Count(n),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            last_rect: Rect::ZERO,
        }
    }

    /// Construct a [`BadgeContent::Text`] badge.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: BadgeContent::Text(s.into()),
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            last_rect: Rect::ZERO,
        }
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
        if self.rect != self.last_rect {
            self.dirty = true;
            self.last_rect = self.rect;
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
        if !self.dirty {
            out_scene.append(&self.cached, Some(out_transform));
            return;
        }
        // mem::take dance — `display_text` borrows &self while
        // `scene` borrows `&mut self.cached`.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        let transform = Affine::IDENTITY;
        let bg = theme.tokens.colors.semantic.danger;

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
                    let (tw, th) = text_renderer.measure(&label, text_size, None);
                    let tx = self.rect.center().x - tw as f64 / 2.0;
                    let ty = self.rect.center().y - th as f64 / 2.0;
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
    }

    fn needs_redraw(&self) -> bool { self.dirty }
    fn mark_drawn(&mut self) { self.dirty = false; }
    fn is_layout_dirty(&self) -> bool { self.rect == Rect::ZERO }

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
