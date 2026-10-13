//! Shared responsive legend layout for comparative charts.
//!
//! [`measure`], [`wrap`] and [`paint_entry`] lay out and paint a legend
//! from the theme: each entry is a series swatch and its `Caption` label,
//! measured by the renderer so long labels never overlap. [`layout`] is the
//! older renderer-free estimate. [`SeriesToggles`] is the legend's
//! interactive half: which series people hid and where to click to toggle
//! them.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::a11y::{AccessNode, AccessState, Role};
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
    paint_entry_emphasised(scene, transform, text, theme, rect, colour, label, false);
}

/// [`paint_entry`] for a legend people point at: an `emphasised` entry (the
/// hovered or chosen series) reads in primary ink.
pub fn paint_entry_emphasised(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    colour: Color,
    label: &str,
    emphasised: bool,
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
        if emphasised {
            theme.text_primary
        } else {
            theme.text_dim
        },
        transform * Affine::translate((rect.x0 + dot + gap, cy - h / 2.0)),
        None,
        &style,
    );
}

/// The series a legend has hidden, where each entry was last painted, and
/// the latest visibility change for the host to read.
#[derive(Debug, Default)]
pub(crate) struct SeriesToggles {
    hidden: HashSet<String>,
    hits: Vec<(String, Rect)>,
    changed: Option<(String, bool)>,
}

impl SeriesToggles {
    pub(crate) fn is_hidden(&self, id: &str) -> bool {
        self.hidden.contains(id)
    }

    /// Forget hidden series the chart no longer has.
    pub(crate) fn retain(&mut self, known: impl Fn(&str) -> bool) {
        self.hidden.retain(|id| known(id));
    }

    /// Show or hide `id`, recording the change; false when it already was.
    pub(crate) fn set(&mut self, id: &str, visible: bool) -> bool {
        let changed = if visible {
            self.hidden.remove(id)
        } else {
            self.hidden.insert(id.to_owned())
        };
        if changed {
            self.changed = Some((id.to_owned(), visible));
        }
        changed
    }

    pub(crate) fn take_changed(&mut self) -> Option<(String, bool)> {
        self.changed.take()
    }

    pub(crate) fn clear_hits(&mut self) {
        self.hits.clear();
    }

    /// Paint a legend people click to hide and show series: each `(id,
    /// name, colour)` entry's swatch and name, laid out by [`layout`] across
    /// `area` from its top, faded while hidden. Remembers where each entry
    /// landed for [`Self::hit`].
    pub(crate) fn paint<'a>(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        entries: impl IntoIterator<Item = (&'a str, &'a str, Color)>,
        area: Rect,
    ) {
        let entries: Vec<_> = entries.into_iter().collect();
        let items = layout(
            entries.iter().map(|(_, name, _)| *name),
            area.x0,
            area.x1,
            area.y0,
        );
        let ink = theme.tokens.colors.text_dim;
        self.hits.clear();
        for ((id, name, colour), item) in entries.into_iter().zip(items) {
            let hidden = self.is_hidden(id);
            let fade = |colour: Color, alpha: u8| {
                if hidden {
                    crate::paint::with_alpha(colour, alpha)
                } else {
                    colour
                }
            };
            scene.fill(
                Fill::NonZero,
                transform,
                fade(colour, 65),
                None,
                &Circle::new((item.x0 + 4.0, item.y0 + 6.0), 4.0),
            );
            text.draw_style(
                scene,
                name,
                fade(ink, 90),
                transform * Affine::translate((item.x0 + 14.0, item.y0)),
                Some(crate::num::f32_of_f64((item.width() - 18.0).max(1.0))),
                &theme.text(TextRole::Caption),
            );
            self.hits.push((id.to_owned(), item));
        }
    }

    /// The series whose painted entry contains `point`.
    pub(crate) fn hit(&self, point: Point) -> Option<String> {
        self.hits
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| id.clone())
    }

    /// The toggle a screen reader activates in place of clicking `id`'s entry.
    pub(crate) fn access_node(&self, id: &str, name: &str) -> AccessNode {
        AccessNode::new(Role::ToggleButton)
            .with_action_target(target(id))
            .with_name(format!("{name} series"))
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .checked(!self.is_hidden(id)),
            )
    }

    /// The series among `ids` whose [`Self::access_node`] has action `key`.
    pub(crate) fn activated<'a>(
        ids: impl IntoIterator<Item = &'a str>,
        key: u64,
    ) -> Option<String> {
        ids.into_iter()
            .find(|id| crate::a11y::hash_key(&target(id)) == key)
            .map(str::to_owned)
    }
}

fn target(id: &str) -> String {
    format!("legend:{id}")
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
