//! Shared immediate-mode keyframe control.
//!
//! A keyframe diamond is a button, not canvas decoration. This face keeps its
//! three-state visual language, target, tooltip, disabled treatment, and
//! accessibility value together while an animation editor owns the mutation.

use std::borrow::Cow;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyframeState {
    /// The parameter has no animation curve.
    Off,
    /// The parameter is animated, but no key sits at the current instant.
    Between,
    /// A key sits at the current instant.
    OnKey,
}

impl KeyframeState {
    pub const fn value(self) -> &'static str {
        match self {
            Self::Off => "Not animated",
            Self::Between => "Animated; no keyframe at the playhead",
            Self::OnKey => "Keyframe at the playhead",
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Off => "Add",
            Self::Between | Self::OnKey => "Edit",
        }
    }

    pub const fn tooltip(self) -> &'static str {
        match self {
            Self::Off => "Add keyframe at playhead",
            Self::Between | Self::OnKey => "Keyframe options",
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyframeFace<'a> {
    rect: Rect,
    label: Cow<'a, str>,
    state: KeyframeState,
    hovered: bool,
    focused: bool,
    disabled: bool,
}

impl<'a> KeyframeFace<'a> {
    pub fn new(rect: Rect, label: impl Into<Cow<'a, str>>, state: KeyframeState) -> Self {
        Self {
            rect,
            label: label.into(),
            state,
            hovered: false,
            focused: false,
            disabled: false,
        }
    }

    #[must_use]
    pub fn hovered(mut self, value: bool) -> Self {
        self.hovered = value;
        self
    }

    #[must_use]
    pub fn focused(mut self, value: bool) -> Self {
        self.focused = value;
        self
    }

    #[must_use]
    pub fn disabled(mut self, value: bool) -> Self {
        self.disabled = value;
        self
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn hit_test(&self, point: Point) -> bool {
        !self.disabled && self.rect.contains(point)
    }

    pub fn spoken_name(&self) -> String {
        format!("{} keyframe for {}", self.state.verb(), self.label)
    }

    pub fn tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        self.hit_test(point)
            .then(|| (self.rect, self.state.tooltip().to_string()))
    }

    pub fn accessibility_with_key(&self, key: u64) -> AccessNode {
        AccessNode::new(Role::Button)
            .with_name(self.spoken_name())
            .with_value(self.state.value())
            .with_key(key)
            .with_bounds(self.rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.focused)
                    .disabled(self.disabled),
            )
    }

    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        if self.hovered && !self.disabled {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.bg_hover,
                None,
                &RoundedRect::from_rect(self.rect, 5.0),
            );
        }
        if self.focused && !self.disabled {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(self.rect, 5.0),
            );
        }
        let (radius, color, filled) = if self.disabled {
            (4.0, colors.text_tertiary, false)
        } else {
            match self.state {
                KeyframeState::Off => (4.0, colors.text_tertiary, false),
                KeyframeState::Between => (5.5, colors.intent.inverted, false),
                KeyframeState::OnKey => (5.5, colors.intent.inverted, true),
            }
        };
        let center = self.rect.center();
        let mut diamond = BezPath::new();
        diamond.move_to((center.x, center.y - radius));
        diamond.line_to((center.x + radius, center.y));
        diamond.line_to((center.x, center.y + radius));
        diamond.line_to((center.x - radius, center.y));
        diamond.close_path();
        if filled {
            scene.fill(Fill::NonZero, transform, color, None, &diamond);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                color.multiply_alpha(0.5),
                None,
                &Circle::new(center, radius + 3.0),
            );
        } else {
            scene.stroke(&Stroke::new(1.4), transform, color, None, &diamond);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_face_drives_semantics_hit_test_and_state_values() {
        fn semantics_tooltip_and_hit_test_share_one_face() {
            let rect = Rect::new(8.0, 10.0, 40.0, 42.0);
            let face = KeyframeFace::new(rect, "Opacity", KeyframeState::Off);
            assert!(face.hit_test(rect.center()));
            assert_eq!(
                face.tooltip_target(rect.center()),
                Some((rect, "Add keyframe at playhead".to_string()))
            );
            let node = face.accessibility_with_key(42);
            assert_eq!(node.name.as_deref(), Some("Add keyframe for Opacity"));
            assert_eq!(node.value.as_deref(), Some("Not animated"));
            assert_eq!(
                node.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(rect)
            );
        }

        fn all_three_states_have_distinct_spoken_values() {
            let rect = Rect::ZERO;
            let off = KeyframeFace::new(rect, "Scale", KeyframeState::Off);
            let between = KeyframeFace::new(rect, "Scale", KeyframeState::Between);
            let on = KeyframeFace::new(rect, "Scale", KeyframeState::OnKey);
            assert_ne!(off.state.value(), between.state.value());
            assert_ne!(between.state.value(), on.state.value());
            assert_eq!(between.spoken_name(), "Edit keyframe for Scale");
        }

        semantics_tooltip_and_hit_test_share_one_face();
        all_three_states_have_distinct_spoken_values();
    }
}
