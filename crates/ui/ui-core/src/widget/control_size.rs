//! How big a control is — the one size knob a control takes.

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
    Small,
    /// The default control.
    #[default]
    Regular,
    Large,
}

crate::choice!(ControlSize, "ControlSize", {
    Small => "small",
    Regular => "regular",
    Large => "large",
});

impl ControlSize {
    #[must_use]
    pub const fn text_role(self) -> TextRole {
        match self {
            Self::Small | Self::Regular => TextRole::Label,
            Self::Large => TextRole::Body,
        }
    }

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
