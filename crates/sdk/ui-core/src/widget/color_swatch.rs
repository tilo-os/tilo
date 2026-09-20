//! Immediate-mode colour swatch face.
//!
//! Dense toolbars and custom canvases can keep their own layout while sharing
//! one declaration for swatch paint, hit-testing, tooltip help, and radio-style
//! accessibility semantics.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{Color, Fill};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct ColorSwatchFace {
    rect: Rect,
    target_rect: Rect,
    color: Color,
    name: String,
    marker: Option<String>,
    selected: bool,
    hovered: bool,
}

impl ColorSwatchFace {
    pub fn new(rect: Rect, color: Color, name: impl Into<String>) -> Self {
        Self {
            rect,
            target_rect: rect,
            color,
            name: name.into(),
            marker: None,
            selected: false,
            hovered: false,
        }
    }

    #[must_use]
    pub fn marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn target_rect(mut self, target_rect: Rect) -> Self {
        self.target_rect = target_rect;
        self
    }

    pub fn hit_test(&self, point: Point) -> bool {
        self.target_rect.contains(point)
    }

    pub fn tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        self.hit_test(point).then(|| (self.rect, self.name.clone()))
    }

    pub fn accessibility(&self, action_target: impl Into<String>) -> AccessNode {
        AccessNode::new(Role::RadioButton)
            .with_name(self.name.clone())
            .with_action_target(action_target)
            .with_bounds(self.target_rect)
            .with_state(AccessState::new().focusable(true).checked(self.selected))
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let shape = RoundedRect::from_rect(self.rect, 4.0);
        scene.fill(Fill::NonZero, transform, self.color, None, &shape);
        let border = if self.selected {
            Some((2.0, theme.tokens.colors.border_focus))
        } else if self.hovered {
            Some((
                1.5,
                theme.tokens.colors.ink_on_any(
                    &[self.color],
                    theme.tokens.colors.text_primary,
                    3.0,
                ),
            ))
        } else {
            None
        };
        if let Some((width, color)) = border {
            scene.stroke(
                &vello::kurbo::Stroke::new(width),
                transform,
                color,
                None,
                &shape,
            );
        }
        if let Some(marker) = &self.marker {
            let size = 12.0;
            let (width, _) = text.measure(marker, size, None);
            let ink = theme
                .tokens
                .colors
                .ink_on_any(&[self.color], theme.text_primary, 4.5);
            text.draw_text(
                scene,
                marker,
                size,
                ink,
                transform.pre_translate(Vec2::new(
                    self.rect.center().x - f64::from(width) / 2.0,
                    self.rect.center().y - 7.0,
                )),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_face_drives_hit_help_and_checked_semantics() {
        let visual = Rect::new(10.0, 10.0, 30.0, 30.0);
        let target = Rect::new(6.0, 4.0, 34.0, 36.0);
        let face = ColorSwatchFace::new(visual, Color::WHITE, "Text color #FFFFFF")
            .selected(true)
            .target_rect(target);
        assert!(face.hit_test(Point::new(7.0, 20.0)));
        assert_eq!(
            face.tooltip_target(Point::new(7.0, 20.0)),
            Some((visual, "Text color #FFFFFF".to_owned())),
        );
        let node = face.accessibility("text-color:ffffff");
        assert_eq!(node.role, Role::RadioButton);
        assert_eq!(node.state.checked, Some(true));
        assert_eq!(node.bounds.expect("target bounds").to_rect(), target);
    }
}
