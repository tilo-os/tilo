//! The strip's own box: the bands of wallpaper above and below a tiled row,
//! which keep its windows' borders and corners showing, and the gutter that
//! stands before its first window.

use super::{GroupRow, STRIP_EDGE_GUTTER_PX, Style, WindowGroup};
use ui::widget::{STRIP_BOTTOM_GUTTER_PX, STRIP_TOP_GAP_PX};

/// The shown strip's node style over the row's `own`: tiled windows keep
/// wallpaper above and below them, so the strip reads as its own row under
/// the bar and their borders and corners show; a window filling the strip
/// or taken across the display takes both bands instead.
pub(super) fn strip_style(row: &GroupRow, own: Style) -> Style {
    // A window taken across the display has both bands: it stands on no
    // wallpaper, from the bar's band down to the screen's bottom edge.
    let fills = row.is_fullscreen() || row.groups.iter().any(|group| group.maximized);
    let bottom = if fills { 0.0 } else { STRIP_BOTTOM_GUTTER_PX };
    // The bar keeps the strip clear of itself: a window's top border and
    // rounded corners stand on wallpaper, not against the bar's band.
    let top = if fills { 0.0 } else { STRIP_TOP_GAP_PX };
    let mut style = own;
    style.margin.top = ui::taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(top));
    style.margin.bottom = ui::taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(bottom));
    style
}

/// A strip's row with the wallpaper gutter before its first window, as the
/// native desktop keeps it.
pub(super) fn strip_row(groups: Vec<WindowGroup>) -> GroupRow {
    GroupRow::new(groups).with_leading_inset(STRIP_EDGE_GUTTER_PX)
}
