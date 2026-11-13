//! The place of earlier lines a conversation cannot get.

use vello::kurbo::Rect;

use crate::paint::PaintCtx;
use crate::tokens::TextRole;

/// "Earlier messages aren't on this device", centred in
/// the band the earlier lines would take.
pub struct EarlierUnavailableFace;

impl EarlierUnavailableFace {
    #[must_use]
    pub fn label() -> String {
        super::t("conversation-earlier-unavailable")
    }

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
