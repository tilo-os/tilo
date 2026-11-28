//! Shared responsive legend layout for comparative charts.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};

use tilo_ui_base::text_render::{TextRenderer, TextStyle};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegendEntry {
    pub width: f64,
    pub height: f64,
}

/// One entry measured against the width its row gives it: its name broken over as many lines as
/// that width holds, and the box the entry takes.
#[derive(Clone, Debug)]
pub struct LegendLines {
    /// The name's lines as they read.
    pub lines: Vec<String>,
    pub size: LegendEntry,
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

/// Measure each label as an entry broken over the lines `width` holds: its swatch, gap and name,
/// the name wrapped at the words' own breaks so a long one reads whole rather than elided. A
/// single word wider than `width` is elided, since a word never splits.
pub fn measure_lines<'a>(
    text: &mut TextRenderer,
    theme: &Theme,
    labels: impl IntoIterator<Item = &'a str>,
    width: f64,
) -> Vec<LegendLines> {
    let style = theme.text(TextRole::Caption);
    let (dot, gap) = swatch(theme);
    let line = f64::from(text.measure_style("Ag", None, &style).1);
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let spare = tilo_ui_base::num::f32_of_f64((width - dot - gap).max(1.0));
    labels
        .into_iter()
        .map(|label| {
            let lines = break_lines(text, label, &style, spare);
            let widest = lines
                .iter()
                .map(|line| f64::from(text.measure_style(line, None, &style).0))
                .fold(0.0, f64::max);
            LegendLines {
                size: LegendEntry {
                    width: (dot + gap + widest).ceil(),
                    height: (tilo_ui_base::num::f64_of_usize(lines.len()) * line + tight).ceil(),
                },
                lines,
            }
        })
        .collect()
}

/// `label` packed greedily into lines no wider than `spare`, at the words' own breaks.
fn break_lines(text: &mut TextRenderer, label: &str, style: &TextStyle, spare: f32) -> Vec<String> {
    let fits =
        |text: &mut TextRenderer, line: &str| text.measure_style(line, None, style).0 <= spare;
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in label.split_whitespace() {
        let word = if fits(text, word) {
            word.to_owned()
        } else {
            text.elide_to_width_style(word, spare, style)
        };
        let candidate = if current.is_empty() {
            word.clone()
        } else {
            format!("{current} {word}")
        };
        if current.is_empty() || fits(text, &candidate) {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current = word;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Place measured entries left to right from `top`, wrapping before
/// `right`, a section step apart. An entry wider than the space is clamped
/// to it (its label elides when painted). A line of entries advances past
/// the tallest of them.
pub fn wrap(entries: &[LegendEntry], left: f64, right: f64, top: f64, theme: &Theme) -> Vec<Rect> {
    let gap = f64::from(theme.tokens.spacing.rhythm.section);
    let available = (right - left).max(1.0);
    let mut x = left;
    let mut y = top;
    let mut row = 0.0_f64;
    entries
        .iter()
        .map(|entry| {
            let width = entry.width.min(available);
            if x > left && x + width > right {
                x = left;
                y += row;
                row = 0.0;
            }
            let rect = Rect::new(x, y, x + width, y + entry.height);
            x += width + gap;
            row = row.max(entry.height);
            rect
        })
        .collect()
}

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
    let spare = tilo_ui_base::num::f32_of_f64((rect.width() - dot - gap).max(0.0));
    let label = text.elide_to_width_style(label, spare, &style);
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

/// The ink a legend entry's name and its value take on the chart's ground: the theme's primary
/// and dim text, the dim one only where it clears the small-text floor there, so a chart on a
/// ground the dim tier is too light for still reads its values.
pub fn entry_inks(theme: &Theme) -> (Color, Color) {
    let ground = theme.tokens.colors.surfaces.base;
    let dim = theme.tokens.colors.text_dim;
    let name = theme
        .tokens
        .colors
        .ink_on(ground, theme.tokens.colors.text_primary, 4.5);
    let value = if tilo_ui_base::a11y::contrast_ratio(dim, ground) >= 4.5 {
        dim
    } else {
        theme.tokens.colors.text_primary
    };
    (name, value)
}

/// An entry [`measure_lines`] measured, placed at `rect` with the value that follows its name.
#[derive(Clone, Copy, Debug)]
pub struct PlacedEntry<'a> {
    pub rect: Rect,
    pub lines: &'a [String],
    pub value: Option<&'a str>,
}

/// Paint a placed entry: its swatch on the first line, its name's lines under each other, and
/// its value under them where it has one.
pub fn paint_lines(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    colour: Color,
    entry: PlacedEntry<'_>,
) {
    let style = theme.text(TextRole::Caption);
    let (dot, gap) = swatch(theme);
    let line = f64::from(text.measure_style("Ag", None, &style).1);
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let (name_ink, value_ink) = entry_inks(theme);
    let rect = entry.rect;
    scene.fill(
        Fill::NonZero,
        transform,
        colour,
        None,
        &Circle::new(
            Point::new(rect.x0 + dot / 2.0, rect.y0 + line / 2.0),
            dot / 2.0,
        ),
    );
    let x = rect.x0 + dot + gap;
    for (index, text_line) in entry.lines.iter().enumerate() {
        let y = rect.y0 + tilo_ui_base::num::f64_of_usize(index) * line;
        text.draw_style(
            scene,
            text_line,
            name_ink,
            transform * Affine::translate((x, y)),
            None,
            &style,
        );
    }
    if let Some(value) = entry.value {
        let y = rect.y0 + tilo_ui_base::num::f64_of_usize(entry.lines.len()) * line + tight;
        text.draw_style(
            scene,
            value,
            value_ink,
            transform * Affine::translate((x, y)),
            None,
            &style,
        );
    }
}

/// The series a legend has hidden, where the layout placed each entry, and
/// the latest visibility change for the host to read.
#[derive(Debug, Default)]
pub struct SeriesToggles {
    hidden: HashSet<String>,
    /// Each entry's place, by series id, in the order the legend lays them out.
    placed: Vec<(String, Rect)>,
    changed: Option<(String, bool)>,
    /// The series the person is on, and whether they are on its legend entry (which brings the
    /// series forward) rather than on one of its marks (which names it in the legend).
    focus: Option<(String, bool)>,
}

impl SeriesToggles {
    pub(crate) fn is_hidden(&self, id: &str) -> bool {
        self.hidden.contains(id)
    }

    pub(crate) fn retain(&mut self, known: impl Fn(&str) -> bool) {
        self.hidden.retain(|id| known(id));
    }

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

    pub(crate) fn set_focus(&mut self, focus: Option<(String, bool)>) {
        self.focus = focus;
    }

    /// The series the person is on, by its legend entry or by one of its marks.
    pub(crate) fn focus(&self) -> Option<&str> {
        self.focus.as_ref().map(|(id, _)| id.as_str())
    }

    /// Whether series `id` stands back while the person points at another's legend entry.
    pub(crate) fn dims(&self, id: &str) -> bool {
        self.focus
            .as_ref()
            .is_some_and(|(focus, on_legend)| *on_legend && focus != id)
    }

    /// `colour` as series `id` wears it now: faded while another's legend entry is pointed at.
    pub(crate) fn ink(&self, id: &str, colour: Color) -> Color {
        if self.dims(id) {
            tilo_ui_base::paint::with_alpha(colour, 56)
        } else {
            colour
        }
    }

    /// Place each `(id, name)` entry across `area` from its top, in the order
    /// the legend paints them; `None` when the legend shows nothing. The
    /// layout runs this, so a press lands on an entry before the legend has
    /// painted and after the series changed.
    pub(crate) fn place<'a>(
        &mut self,
        entries: impl IntoIterator<Item = (&'a str, &'a str)>,
        area: Option<Rect>,
    ) {
        let entries: Vec<(&str, &str)> = entries.into_iter().collect();
        self.placed = area.map_or_else(Vec::new, |area| {
            layout(
                entries.iter().map(|(_, name)| *name),
                area.x0,
                area.x1,
                area.y0,
            )
            .into_iter()
            .zip(entries)
            .map(|(rect, (id, _))| (id.to_owned(), rect))
            .collect()
        });
    }

    pub(crate) fn entry(&self, id: &str) -> Option<Rect> {
        self.placed
            .iter()
            .find(|(placed, _)| placed == id)
            .map(|(_, rect)| *rect)
    }

    /// Paint the entries the layout placed: each `(id, name, colour)`
    /// entry's swatch and name, faded while hidden. Painting adds no places.
    pub(crate) fn paint<'a>(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        entries: impl IntoIterator<Item = (&'a str, &'a str, Color)>,
    ) {
        for (id, name, colour) in entries {
            if let Some(item) = self.entry(id) {
                paint_series(
                    scene,
                    transform,
                    text,
                    theme,
                    item,
                    (name, colour, self.is_hidden(id)),
                    self.focus() == Some(id),
                );
            }
        }
    }

    pub(crate) fn hit(&self, point: Point) -> Option<String> {
        self.placed
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| id.clone())
    }

    pub(crate) fn access_node(&self, id: &str, name: &str) -> AccessNode {
        AccessNode::new(Role::ToggleButton)
            .with_action_target(target(id))
            .with_name(tilo_ui_base::i18n::widget_format(
                "widget-chart-series",
                &[("name", name.into())],
            ))
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .checked(!self.is_hidden(id)),
            )
    }

    pub(crate) fn activated<'a>(
        ids: impl IntoIterator<Item = &'a str>,
        key: u64,
    ) -> Option<String> {
        ids.into_iter()
            .find(|id| tilo_ui_base::a11y::hash_key(&target(id)) == key)
            .map(str::to_owned)
    }
}

fn target(id: &str) -> String {
    format!("legend:{id}")
}

/// One series' entry in a legend [`layout`] placed at `item`: the `(name, colour, hidden)` of
/// the series, as its swatch and its name, faded while the series is hidden.
pub(crate) fn paint_series(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    item: Rect,
    (name, colour, hidden): (&str, Color, bool),
    emphasised: bool,
) {
    let fade = |colour: Color, alpha: u8| {
        if hidden {
            tilo_ui_base::paint::with_alpha(colour, alpha)
        } else {
            colour
        }
    };
    // The swatch and the name share the entry's centre line, as [`paint_entry`]'s do.
    let cy = item.center().y;
    scene.fill(
        Fill::NonZero,
        transform,
        fade(colour, 65),
        None,
        &Circle::new((item.x0 + 4.0, cy), 4.0),
    );
    let style = theme.text(TextRole::Caption);
    let h = f64::from(text.measure_style(name, None, &style).1);
    let ink = if emphasised {
        theme.tokens.colors.text_primary
    } else {
        theme.tokens.colors.text_dim
    };
    text.draw_style(
        scene,
        name,
        fade(ink, 90),
        transform * Affine::translate((item.x0 + 14.0, cy - h / 2.0)),
        Some(tilo_ui_base::num::f32_of_f64(
            (item.width() - 18.0).max(1.0),
        )),
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
        tilo_ui_base::num::usize_of_f64(rect.y0 / ROW_HEIGHT).saturating_add(1)
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
