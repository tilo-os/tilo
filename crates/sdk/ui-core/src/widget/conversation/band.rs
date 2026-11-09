//! The band a conversation divides its lines with: a caption centred in a band, ruled out to
//! both sides.
//!
//! A day's divider and the "New messages" divider are one family: one band, a list row tall, the
//! label centred with an `s3` gap between it and each rule, so the two read as the same mark
//! where a conversation changes from one day to the next and where the reader's unread lines
//! start. They differ in ink alone — the day in dimmed text over a hairline, the place reading
//! starts again in the accent.

use vello::kurbo::Rect;
use vello::peniko::Color;

use crate::chrome::nav_row_height;
use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The height a divider takes: one list row, at every text scale.
#[must_use]
pub fn height(theme: &Theme) -> f64 {
    nav_row_height(theme)
}

/// The ink of each divider of the family: its label and the rules beside it.
#[derive(Clone, Copy)]
pub struct Ink {
    /// The label's words.
    pub label: Color,
    /// The rules running out to the column's edges.
    pub rule: Color,
}

/// Paint `label` centred in `band`, its rules running from `leading` to the words and from them
/// to `trailing` (the message column's edges, not the window's), each stopping an `s3` gap short
/// of the label.
pub fn paint(
    paint: &mut PaintCtx<'_>,
    band: Rect,
    leading: f64,
    trailing: f64,
    label: &str,
    ink: Ink,
) {
    let theme = paint.theme;
    let style = theme.text(TextRole::Caption);
    let gap = f64::from(theme.tokens.spacing.s3);
    let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
    // A label wider than the band (a long date at 1.5× in a narrow window) is elided rather than
    // drawn over the column's edges.
    let shown = paint.text.elide_to_width_style(label, room, &style);
    let width = f64::from(paint.text.measure_style(&shown, None, &style).0);
    let centre = band.center().x;
    let left_edge = centre - width / 2.0 - gap;
    let right_edge = centre + width / 2.0 + gap;
    let rule_y = band.center().y;
    if left_edge > leading {
        crate::surface::separator(
            paint.scene,
            paint.transform,
            leading,
            left_edge,
            rule_y,
            0.0,
            ink.rule,
        );
    }
    if trailing > right_edge {
        crate::surface::separator(
            paint.scene,
            paint.transform,
            right_edge,
            trailing,
            rule_y,
            0.0,
            ink.rule,
        );
    }
    if label.is_empty() {
        return;
    }
    paint.text_role_in_band(
        &shown,
        TextRole::Caption,
        ink.label,
        centre - width / 2.0,
        band,
        None,
    );
}
