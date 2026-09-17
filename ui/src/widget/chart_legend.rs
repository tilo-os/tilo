//! Shared responsive legend layout for comparative charts.

use vello::kurbo::Rect;

pub const ROW_HEIGHT: f64 = 24.0;
const SWATCH_AND_GAP: f64 = 22.0;
const ITEM_GAP: f64 = 12.0;
const APPROX_GLYPH_WIDTH: f64 = 7.0;
const MAX_LABEL_WIDTH: f64 = 160.0;

fn item_width(label: &str) -> f64 {
    let glyphs = u32::try_from(label.chars().count()).unwrap_or(u32::MAX);
    (f64::from(glyphs) * APPROX_GLYPH_WIDTH).clamp(24.0, MAX_LABEL_WIDTH) + SWATCH_AND_GAP
}

/// Place legend items left-to-right and wrap before the chart edge.
pub fn layout<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    left: f64,
    right: f64,
    top: f64,
) -> Vec<Rect> {
    let available = (right - left).max(1.0);
    let mut x = left;
    let mut y = top;
    labels
        .into_iter()
        .map(|label| {
            let width = item_width(label).min(available);
            if x > left && x + width > right {
                x = left;
                y += ROW_HEIGHT;
            }
            let rect = Rect::new(x, y, x + width, y + ROW_HEIGHT);
            x += width + ITEM_GAP;
            rect
        })
        .collect()
}

pub fn rows<'a>(labels: impl IntoIterator<Item = &'a str>, left: f64, right: f64) -> usize {
    layout(labels, left, right, 0.0).last().map_or(0, |rect| {
        crate::num::usize_of_f64(rect.y0 / ROW_HEIGHT).saturating_add(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legends_wrap_without_crossing_the_available_width() {
        let items = layout(
            ["First long series", "Second long series", "Third"],
            10.0,
            210.0,
            20.0,
        );
        assert!(items.iter().all(|item| item.x0 >= 10.0 && item.x1 <= 210.0));
        assert!(items[1].y0 > items[0].y0);
        assert_eq!(
            rows(["First long series", "Second long series"], 10.0, 210.0),
            2
        );
    }
}
