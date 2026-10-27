//! The place of earlier lines a conversation cannot get.
//!
//! A conversation kept in parts reads earlier ones back from the room as
//! its reader scrolls up, showing them arriving meanwhile. When the room's
//! hosts sent everything they hold without them, or no host is connected,
//! nothing more is coming: the place says so, quietly and with nothing
//! moving, where the earlier lines would be.

use vello::kurbo::Rect;

use crate::paint::PaintCtx;
use crate::tokens::TextRole;

/// "Earlier messages aren't available on this device's hosts", centred in
/// the band the earlier lines would take.
pub struct EarlierUnavailableFace;

impl EarlierUnavailableFace {
    /// What the place says.
    #[must_use]
    pub fn label() -> String {
        super::t("conversation-earlier-unavailable")
    }

    /// Paint the label centred across `band`, within `leading..trailing`
    /// (the message column's edges).
    pub fn paint(paint: &mut PaintCtx<'_>, band: Rect, leading: f64, trailing: f64) {
        let theme = paint.theme;
        let label = Self::label();
        let style = theme.text(TextRole::Caption);
        let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
        let shown = paint.text.elide_to_width_style(&label, room, &style);
        let width = f64::from(paint.text.measure_style(&shown, None, &style).0);
        let x = leading + ((trailing - leading) - width).max(0.0) / 2.0;
        paint.text_role_in_band(&shown, TextRole::Caption, theme.text_dim, x, band, None);
    }
}
