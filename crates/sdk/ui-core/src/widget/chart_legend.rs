//! Shared responsive legend layout for comparative charts.
//!
//! [`measure`], [`wrap`] and [`paint_entry`] lay out and paint a legend
//! from the theme: each entry is a series swatch and its `Caption` label,
//! measured by the renderer so long labels never overlap. [`layout`] is the
//! older renderer-free estimate.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// One measured legend entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegendEntry {
    pub width: f64,
    pub height: f64,
}

fn swatch(theme: &Theme) -> (f64, f64) {
    let spacing = &theme.tokens.spacing;
    (f64::from(spacing.s2), f64::from(spacing.s2))
}

/// Measure each label as an entry: swatch, gap, label; one `Caption` line
/// plus a tight step tall.
pub fn measure<'a>(
    text: &mut TextRenderer,
    theme: &Theme,
    labels: impl IntoIterator<Item = &'a str>,
) -> Vec<LegendEntry> {
    let style = theme.text(TextRole::Caption);
    let (dot, gap) = swatch(theme);
    let line = f64::from(text.measure_style("Ag", None, &style).1);
    let height = (line + f64::from(theme.tokens.spacing.rhythm.tight)).ceil();
    labels
        .into_iter()
        .map(|label| LegendEntry {
            width: (dot + gap + f64::from(text.measure_style(label, None, &style).0)).ceil(),
            height,
        })
        .collect()
}

/// Place measured entries left to right from `top`, wrapping before
/// `right`, a section step apart. An entry wider than the space is clamped
/// to it (its label elides when painted).
pub fn wrap(entries: &[LegendEntry], left: f64, right: f64, top: f64, theme: &Theme) -> Vec<Rect> {
    let gap = f64::from(theme.tokens.spacing.rhythm.section);
    let available = (right - left).max(1.0);
    let mut x = left;
    let mut y = top;
    entries
        .iter()
        .map(|entry| {
            let width = entry.width.min(available);
            if x > left && x + width > right {
                x = left;
                y += entry.height;
            }
            let rect = Rect::new(x, y, x + width, y + entry.height);
            x += width + gap;
            rect
        })
        .collect()
}

/// Paint one entry into `rect`: the series swatch, then its label in
/// secondary ink, elided to the rect.
pub fn paint_entry(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    colour: Color,
    label: &str,
) {
    let style = theme.text(TextRole::Caption);
    let (dot, gap) = swatch(theme);
    let cy = rect.center().y;
    scene.fill(
        Fill::NonZero,
        transform,
        colour,
        None,
        &Circle::new(Point::new(rect.x0 + dot / 2.0, cy), dot / 2.0),
    );
    let room = crate::num::f32_of_f64((rect.width() - dot - gap).max(0.0));
    let label = text.elide_to_width_style(label, room, &style);
    let h = f64::from(text.measure_style(&label, None, &style).1);
    text.draw_style(
        scene,
        &label,
        theme.text_dim,
        transform * Affine::translate((rect.x0 + dot + gap, cy - h / 2.0)),
        None,
        &style,
    );
}

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
