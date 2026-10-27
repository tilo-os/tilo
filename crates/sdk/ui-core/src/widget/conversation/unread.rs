//! Where a reader's unread lines start.
//!
//! A conversation the reader comes back to marks the first line they had not
//! read when they opened it, with a rule and "New messages" over it: the
//! place to start reading again, kept while they read on, so lines arriving
//! meanwhile do not move it. The app decides which line that is (from its
//! read marks); the face is the same in every conversation.

use vello::kurbo::Rect;

use crate::chrome::nav_row_height;
use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The "New messages" divider: a rule across the column, its label at the
/// trailing end, a list row tall like a day divider
/// ([`super::DayDividerFace`]).
pub struct UnreadDividerFace;

impl UnreadDividerFace {
    /// The height a divider takes: one list row, at every text scale.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        nav_row_height(theme)
    }

    /// What the divider says.
    #[must_use]
    pub fn label() -> String {
        super::t("conversation-new-messages")
    }

    /// Paint the divider across `band`, its rule running from `leading` to
    /// the label, which ends at `trailing` (the message column's edges).
    pub fn paint(paint: &mut PaintCtx<'_>, band: Rect, leading: f64, trailing: f64) {
        let theme = paint.theme;
        let label = Self::label();
        let style = theme.text(TextRole::Caption);
        let gap = f64::from(theme.tokens.spacing.s3);
        let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
        let shown = paint.text.elide_to_width_style(&label, room, &style);
        let width = f64::from(paint.text.measure_style(&shown, None, &style).0);
        let label_x = trailing - width;
        if label_x - gap > leading {
            crate::surface::separator(
                paint.scene,
                paint.transform,
                leading,
                label_x - gap,
                band.center().y,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }
        paint.text_role_in_band(
            &shown,
            TextRole::Caption,
            theme.text_primary,
            label_x,
            band,
            None,
        );
    }
}
