//! Shared immediate-mode disclosure/section header.
//!
//! A disclosure header is one control with one contract: its chevron, label,
//! collapsed summary, hover/focus feedback, hit target, expanded state, and
//! semantic bounds must never drift into separate app-local implementations.

use std::borrow::Cow;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::button::ButtonInteract;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

#[derive(Clone, Debug)]
pub struct DisclosureHeaderFace<'a> {
    rect: Rect,
    target_rect: Rect,
    indicator_rect: Option<Rect>,
    label: Cow<'a, str>,
    summary: Option<Cow<'a, str>>,
    expanded: bool,
    interact: ButtonInteract,
}

/// Disclosure labels are interactive section controls, not tertiary
/// metadata. Keep them at the standard control-label size so apps do not
/// need to hand-tune typography to make the affordance discoverable.
const TEXT_SIZE: f32 = 13.0;

#[allow(
    clippy::cast_possible_truncation,
    reason = "logical widget extents are bounded far below f32::MAX"
)]
fn px(value: f64) -> f32 {
    value as f32
}

impl<'a> DisclosureHeaderFace<'a> {
    pub fn new(rect: Rect, label: impl Into<Cow<'a, str>>, expanded: bool) -> Self {
        Self {
            rect,
            target_rect: rect,
            indicator_rect: None,
            label: label.into(),
            summary: None,
            expanded,
            interact: ButtonInteract::default(),
        }
    }

    #[must_use]
    pub fn summary(mut self, summary: impl Into<Cow<'a, str>>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Override the interaction/semantic target while keeping the full header
    /// as visual geometry. Rich tree rows use this when only their leading
    /// disclosure indicator toggles expansion and the rest of the row has a
    /// different primary action.
    #[must_use]
    pub fn target_rect(mut self, rect: Rect) -> Self {
        self.target_rect = rect;
        self
    }

    /// Place the disclosure glyph explicitly inside a rich header. Plain
    /// section headers use the compact leading position by default.
    #[must_use]
    pub fn indicator_rect(mut self, rect: Rect) -> Self {
        self.indicator_rect = Some(rect);
        self
    }

    #[must_use]
    pub fn optional_summary(mut self, summary: Option<impl Into<Cow<'a, str>>>) -> Self {
        self.summary = summary.map(Into::into);
        self
    }

    #[must_use]
    pub fn hovered(mut self, value: bool) -> Self {
        self.interact.hovered = value;
        self
    }

    #[must_use]
    pub fn focused(mut self, value: bool) -> Self {
        self.interact.focused = value;
        self
    }

    #[must_use]
    pub fn disabled(mut self, value: bool) -> Self {
        self.interact.disabled = value;
        self
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn hit_test(&self, point: Point) -> bool {
        !self.interact.disabled && self.target_rect.contains(point)
    }

    pub fn accessibility_with_key(&self, key: u64, children: Vec<AccessNode>) -> AccessNode {
        AccessNode::new(Role::Group)
            .with_name(self.label.as_ref())
            .with_optional_value(
                (!self.expanded)
                    .then(|| self.summary.as_deref().map(str::to_string))
                    .flatten(),
            )
            .with_key(key)
            .with_bounds(self.target_rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.interact.focused)
                    .disabled(self.interact.disabled)
                    .expanded(self.expanded),
            )
            .with_children(children)
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(self.rect, 5.0);
        if self.interact.hovered && !self.interact.disabled {
            scene.fill(Fill::NonZero, transform, theme.bg_hover, None, &shape);
        }
        if self.interact.focused && !self.interact.disabled {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        }

        self.paint_indicator(scene, transform, theme);

        let label_x = self.rect.x0 + 18.0;
        let (_, label_h) = text_renderer.measure_weighted(
            self.label.as_ref(),
            TEXT_SIZE,
            None,
            TextWeight::Semibold,
        );
        let label_y = self.rect.center().y - f64::from(label_h) / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            self.label.as_ref(),
            TEXT_SIZE,
            colors.text_dim,
            transform.pre_translate(Vec2::new(label_x, label_y)),
            None,
            TextWeight::Semibold,
        );

        if self.expanded || self.interact.disabled {
            return;
        }
        let Some(summary) = &self.summary else { return };
        let (label_w, _) = text_renderer.measure_weighted(
            self.label.as_ref(),
            TEXT_SIZE,
            None,
            TextWeight::Semibold,
        );
        let available = px((self.rect.x1 - label_x - f64::from(label_w) - 16.0).max(0.0));
        let shown = text_renderer.elide_to_width(summary.as_ref(), TEXT_SIZE, available);
        let (summary_w, summary_h) = text_renderer.measure(&shown, TEXT_SIZE, None);
        let summary_y = self.rect.center().y - f64::from(summary_h) / 2.0;
        text_renderer.draw_text(
            scene,
            &shown,
            TEXT_SIZE,
            colors.intent.inverted,
            transform.pre_translate(Vec2::new(self.rect.x1 - f64::from(summary_w), summary_y)),
            None,
        );
    }

    /// Paint only the standard disclosure indicator for a rich header whose
    /// avatar, labels, badges, or secondary actions are domain content.
    pub fn paint_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let icon = self.indicator_rect.unwrap_or_else(|| {
            Rect::from_center_size(
                Point::new(self.rect.x0 + 6.5, self.rect.center().y),
                (11.0, 11.0),
            )
        });
        crate::icons::draw_icon(
            scene,
            transform,
            icon,
            if self.expanded {
                crate::icons::Icon::ChevronDown
            } else {
                crate::icons::Icon::ChevronRight
            },
            if self.interact.disabled {
                colors.text_tertiary
            } else {
                colors.text_dim
            },
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_face_owns_hit_bounds_and_expanded_semantics() {
        let rect = Rect::new(10.0, 20.0, 210.0, 52.0);
        let collapsed = DisclosureHeaderFace::new(rect, "Geometry", false)
            .summary("cropped · animated")
            .focused(true);
        assert!(collapsed.hit_test(rect.center()));
        let node = collapsed.accessibility_with_key(7, Vec::new());
        assert_eq!(node.name.as_deref(), Some("Geometry"));
        assert_eq!(node.value.as_deref(), Some("cropped · animated"));
        assert_eq!(node.state.expanded, Some(false));
        assert!(node.state.focused);
        assert_eq!(
            node.bounds.map(crate::a11y::AccessBounds::to_rect),
            Some(rect)
        );

        let expanded = DisclosureHeaderFace::new(rect, "Geometry", true)
            .summary("must not be announced while open")
            .accessibility_with_key(7, Vec::new());
        assert_eq!(expanded.value, None);
        assert_eq!(expanded.state.expanded, Some(true));

        let indicator_target = Rect::new(10.0, 20.0, 42.0, 64.0);
        let rich =
            DisclosureHeaderFace::new(rect, "Work account", false).target_rect(indicator_target);
        assert!(rich.hit_test(indicator_target.center()));
        assert!(!rich.hit_test(Point::new(rect.x1 - 1.0, rect.center().y)));
        let node = rich.accessibility_with_key(9, Vec::new());
        assert_eq!(
            node.bounds.map(crate::a11y::AccessBounds::to_rect),
            Some(indicator_target)
        );
    }
}
