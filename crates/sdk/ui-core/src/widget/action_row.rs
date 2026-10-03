//! Shared immediate-mode face for a clickable list row.
//!
//! Use this when a row's content is domain-specific but its interaction
//! surface is not. The face keeps background feedback, hit-testing, focus,
//! disabled state, bounds, and button semantics together while the caller
//! remains free to paint rich row content inside [`ActionRowFace::rect`].

use crate::a11y::{AccessNode, AccessState, Role};
use crate::theme::Theme;
use crate::widget::button::ButtonInteract;
use std::borrow::Cow;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};

#[derive(Clone, Debug)]
pub struct ActionRowFace<'a> {
    rect: Rect,
    name: Cow<'a, str>,
    value: Option<Cow<'a, str>>,
    interact: ButtonInteract,
    radius: f64,
    accented: bool,
    outlined: bool,
}

impl<'a> ActionRowFace<'a> {
    pub fn new(rect: Rect, name: impl Into<Cow<'a, str>>) -> Self {
        Self {
            rect,
            name: name.into(),
            value: None,
            interact: ButtonInteract::default(),
            radius: 8.0,
            accented: false,
            outlined: false,
        }
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.value = Some(value.into());
        self
    }
    #[must_use]
    pub fn hovered(mut self, value: bool) -> Self {
        self.interact.hovered = value;
        self
    }
    #[must_use]
    pub fn pressed(mut self, value: bool) -> Self {
        self.interact.pressed = value;
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
    #[must_use]
    pub fn radius(mut self, value: f64) -> Self {
        self.radius = value;
        self
    }
    /// Give the recommended row the standard accent surface. Rich choice
    /// lists use this instead of recreating primary-card colors per app.
    #[must_use]
    pub fn accented(mut self, value: bool) -> Self {
        self.accented = value;
        self
    }
    /// Use a quiet base fill with the standard border for a secondary choice.
    #[must_use]
    pub fn outlined(mut self, value: bool) -> Self {
        self.outlined = value;
        self
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn hit_test(&self, point: Point) -> bool {
        !self.interact.disabled && self.rect.contains(point)
    }

    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let rest = if self.outlined {
            colors.surfaces.content
        } else {
            colors.surfaces.raised
        };
        let background = if self.interact.disabled {
            colors.surfaces.content
        } else if self.interact.pressed {
            colors.lift_on(rest, 1.0)
        } else if self.accented && self.interact.hovered {
            theme.tokens.colors.intent.inverted_hover
        } else if self.accented {
            theme.tokens.colors.intent.inverted
        } else if self.interact.hovered {
            colors.lift_on(rest, 0.5)
        } else {
            rest
        };
        let shape = RoundedRect::from_rect(self.rect, self.radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            background,
            None,
            &shape,
        );
        if self.outlined {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &shape,
            );
        }
        self.paint_focus(scene, transform, theme);
    }

    /// Paint only the standard focus treatment. Domain-rich rows can retain
    /// selected/unread backgrounds and custom content while delegating the
    /// interaction outline to the shared row contract.
    pub fn paint_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.interact.focused && !self.interact.disabled {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(self.rect, self.radius),
            );
        }
    }

    pub fn accessibility(&self, action_target: impl Into<String>) -> AccessNode {
        let mut node = AccessNode::new(Role::Button)
            .with_name(self.name.as_ref())
            .with_action_target(action_target)
            .with_bounds(self.rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.interact.focused)
                    .disabled(self.interact.disabled),
            );
        if let Some(value) = &self.value {
            node = node.with_value(value.as_ref());
        }
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_pairs_hit_testing_and_semantics() {
        let rect = Rect::new(10.0, 20.0, 210.0, 72.0);
        let face = ActionRowFace::new(rect, "Open result").value("42");
        assert!(face.hit_test(rect.center()));
        assert!(!face.hit_test(Point::ZERO));
        let node = face.accessibility("history:0");
        assert_eq!(node.name.as_deref(), Some("Open result"));
        assert_eq!(node.value.as_deref(), Some("42"));
        assert_eq!(node.action_target.as_deref(), Some("history:0"));
        assert_eq!(
            node.bounds.map(crate::a11y::AccessBounds::to_rect),
            Some(rect)
        );
    }
}
