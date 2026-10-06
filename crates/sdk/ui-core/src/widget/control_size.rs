//! How big a control is — the one size knob a control takes.
//!
//! A control never takes a font size or a pixel height from an app. It takes
//! a [`ControlSize`], and the size decides both the label's [`TextRole`] and
//! the control's height from the theme, so a small button and a small
//! segmented control agree without either caller restating a number.

use crate::text_render::TextStyle;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// Small, regular or large: a control's height and label role together.
///
/// | Size | Height at 1× | Label |
/// |---|---|---|
/// | `Small` | 28 (`metrics.bar_control_height`) | `Label` |
/// | `Regular` | 32 (`metrics.min_pointer_target`) | `Label` |
/// | `Large` | 40 (`metrics.bar_height`) | `Body` |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ControlSize {
    /// Dense chrome: toolbars, inline row actions.
    Small,
    /// The default control.
    #[default]
    Regular,
    /// A prominent action: onboarding, a sheet's confirmation.
    Large,
}

impl ControlSize {
    /// The role a control of this size sets its label in.
    #[must_use]
    pub const fn text_role(self) -> TextRole {
        match self {
            Self::Small | Self::Regular => TextRole::Label,
            Self::Large => TextRole::Body,
        }
    }

    /// The resolved label style for this size in `theme`.
    #[must_use]
    pub fn label_style(self, theme: &Theme) -> TextStyle {
        theme.text(self.text_role())
    }

    /// The control's height in `theme`, already grown by its UI scale.
    ///
    /// At a large text scale the height grows with the label's line once the
    /// line and `spacing.s1` above and below it outgrow the metric, so a
    /// label never overflows its control; on a stock theme, and under a
    /// uniform UI scale, it is exactly the metric.
    #[must_use]
    pub fn height(self, theme: &Theme) -> f64 {
        let metrics = &theme.tokens.metrics;
        let height = f64::from(match self {
            Self::Small => metrics.bar_control_height,
            Self::Regular => metrics.min_pointer_target,
            Self::Large => metrics.bar_height,
        });
        let grown = f64::from(self.label_style(theme).line_height_px())
            + 2.0 * f64::from(theme.tokens.spacing.s1);
        if grown > height { grown.ceil() } else { height }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Controls of one size share one height when they sit side by side,
    /// whatever kind they are: buttons, a dropdown, a number field and a
    /// segmented control line up in a toolbar or a form row.
    #[test]
    fn every_control_of_a_size_is_that_tall() {
        use crate::headless::{FrameHarness, InspectionNode};
        use crate::widget::toggle_group::Segment;
        use crate::widget::{
            Button, ButtonRole, Dropdown, NumberInput, Row, SearchField, ToggleGroup,
        };
        fn collect(node: &InspectionNode, out: &mut Vec<(String, f64)>) {
            if [
                "Button",
                "Dropdown",
                "NumberInput",
                "ToggleGroup",
                "SearchField",
            ]
            .contains(&node.widget.as_str())
            {
                out.push((node.widget.clone(), node.rect[3]));
            }
            for child in &node.children {
                collect(child, out);
            }
        }
        let theme = Theme::dark();
        for size in [ControlSize::Small, ControlSize::Regular, ControlSize::Large] {
            let row = Row::new()
                .push(Box::new(
                    Button::new("p", "Save")
                        .role(ButtonRole::Primary)
                        .control_size(size),
                ))
                .push(Box::new(Button::new("s", "Discard").control_size(size)))
                .push(Box::new(
                    Dropdown::new(vec!["Medium".into()], "d").control_size(size),
                ))
                .push(Box::new(NumberInput::new(1.0).control_size(size)))
                .push(Box::new(
                    ToggleGroup::new(vec![Segment::new("List"), Segment::new("Grid")], 0)
                        .control_size(size),
                ))
                .push(Box::new(SearchField::new().control_size(size)));
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(row),
                (1200, 200),
                theme.clone(),
            );
            let mut heights = Vec::new();
            collect(&harness.inspection().root, &mut heights);
            assert_eq!(heights.len(), 6, "{size:?}: {heights:?}");
            let want = size.height(&theme);
            for (widget, height) in &heights {
                assert!(
                    (height - want).abs() < 0.5,
                    "{size:?}: {widget} is {height} tall, not {want}: {heights:?}"
                );
            }
        }
    }

    #[test]
    fn sizes_order_heights_and_pick_label_roles() {
        let theme = Theme::dark();
        assert_eq!(ControlSize::Small.height(&theme), 28.0);
        assert_eq!(ControlSize::Regular.height(&theme), 32.0);
        assert_eq!(ControlSize::Large.height(&theme), 40.0);
        assert_eq!(ControlSize::Small.text_role(), TextRole::Label);
        assert_eq!(ControlSize::Regular.text_role(), TextRole::Label);
        assert_eq!(ControlSize::Large.text_role(), TextRole::Body);
        let scaled = Theme::dark().with_ui_scale(1.5);
        assert_eq!(ControlSize::Regular.height(&scaled), 48.0);
        assert_eq!(
            ControlSize::Large.label_style(&scaled),
            scaled.text(TextRole::Body)
        );
    }
}
