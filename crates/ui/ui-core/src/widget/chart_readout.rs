//! What a chart says about the mark a person points at or steps to: a card beside the pointer
//! that names the position and lists each value read there with its series' swatch, and the
//! marks it reads highlighted under it (a guide across the plot, a dot on each line, the column
//! of a category). Each chart kind says what it reads ([`Readout`]); the chart's shell paints it
//! over the marks in a window and composes it as the page's own elements on the web, from the
//! one geometry [`Readout::place`] works out, so both look alike.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use tilo_ui_base::paint::with_alpha;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle, text_render::TextWeight};

/// What a chart reads at the mark a person inspects.
#[derive(Clone, Debug, PartialEq)]
pub struct Readout {
    /// Where the values are read: the position on the x axis, the category, the slice. Empty
    /// for none.
    pub(crate) title: String,
    pub(crate) rows: Vec<ReadoutRow>,
    /// What the card stands beside: the pointer, or the mark the keyboard stepped to.
    pub(crate) anchor: Point,
    /// The card stays inside it: the chart's own box.
    pub(crate) bounds: Rect,
    /// Boxes the card keeps clear of where it can (a legend beside a pie).
    pub(crate) avoid: Vec<Rect>,
    /// The marks it reads, highlighted under the card.
    pub(crate) marks: Vec<Highlight>,
}

/// One value the card lists: its series' swatch, what it is and the value.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadoutRow {
    pub(crate) swatch: Option<Color>,
    pub(crate) label: String,
    /// Empty for a row of words alone (a note under the values).
    pub(crate) value: String,
    /// The value the pointer is on among the ones listed.
    pub(crate) strong: bool,
}

/// A mark the card reads, drawn over the chart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Highlight {
    /// A vertical guide at `x` across the plot, from `y0` to `y1`.
    Guide { x: f64, y0: f64, y1: f64 },
    /// A point on a line, ringed in the ground so it stands off the line.
    Dot { at: Point, color: Color },
    /// The column of a category, washed.
    Band(Rect),
    /// A mark outlined in the focus colour, at its corner radius.
    Ring { rect: Rect, radius: f64 },
}

impl Readout {
    pub(crate) fn new(title: impl Into<String>, anchor: Point, bounds: Rect) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
            anchor,
            bounds,
            avoid: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// The two lines most kinds read: what the mark is, and what it says.
    pub(crate) fn of_mark(title: &str, detail: &str, anchor: Point, bounds: Rect) -> Self {
        let mut readout = Self::new(title, anchor, bounds);
        readout.note(detail);
        readout
    }

    /// A value of a series, after its swatch.
    pub(crate) fn value(
        &mut self,
        swatch: Option<Color>,
        label: impl Into<String>,
        value: impl Into<String>,
        strong: bool,
    ) -> &mut Self {
        self.rows.push(ReadoutRow {
            swatch,
            label: label.into(),
            value: value.into(),
            strong,
        });
        self
    }

    /// Words under the values, dim.
    pub(crate) fn note(&mut self, words: impl Into<String>) -> &mut Self {
        let words = words.into();
        if !words.is_empty() {
            self.value(None, words, String::new(), false);
        }
        self
    }

    pub(crate) fn mark(&mut self, mark: Highlight) -> &mut Self {
        self.marks.push(mark);
        self
    }

    /// What it says, as one line: what assistive technology reads.
    #[cfg(test)]
    pub(crate) fn words(&self) -> String {
        let rows = self
            .rows
            .iter()
            .map(|row| match (row.label.is_empty(), row.value.is_empty()) {
                (true, _) => row.value.clone(),
                (false, true) => row.label.clone(),
                (false, false) => format!("{} {}", row.label, row.value),
            })
            .collect::<Vec<_>>()
            .join(", ");
        match (self.title.is_empty(), rows.is_empty()) {
            (true, _) => rows,
            (false, true) => self.title.clone(),
            (false, false) => format!("{}: {rows}", self.title),
        }
    }

    /// Where the card and each of its words stand, measured in `theme`'s type.
    pub(crate) fn place(&self, tr: &mut TextRenderer, theme: &Theme) -> Placed {
        let m = Metrics::of(theme);
        let styles = Styles::of(theme);
        let measure = |tr: &mut TextRenderer, text: &str, style: &TextStyle| {
            if text.is_empty() {
                0.0
            } else {
                f64::from(tr.measure_style(text, None, style).0).ceil()
            }
        };
        let title_w = measure(tr, &self.title, &styles.title);
        let widths: Vec<(f64, f64, f64)> = self
            .rows
            .iter()
            .map(|row| {
                let lead = if row.swatch.is_some() {
                    m.swatch + m.swatch_gap
                } else {
                    0.0
                };
                let label_style = if row.value.is_empty() {
                    &styles.note
                } else {
                    &styles.label
                };
                (
                    lead,
                    measure(tr, &row.label, label_style),
                    measure(tr, &row.value, &styles.value),
                )
            })
            .collect();
        let inner = widths
            .iter()
            .map(|&(lead, label, value)| {
                let gap = if label > 0.0 && value > 0.0 {
                    m.value_gap
                } else {
                    0.0
                };
                lead + label + gap + value
            })
            .fold(title_w, f64::max);
        let rows_h = tilo_ui_base::num::f64_of_usize(self.rows.len()) * m.line
            + tilo_ui_base::num::f64_of_usize(self.rows.len().saturating_sub(1)) * m.row_gap;
        let title_h = if self.title.is_empty() {
            0.0
        } else if self.rows.is_empty() {
            m.line
        } else {
            m.line + m.title_gap
        };
        let size = (
            (inner + m.pad_x * 2.0).min(self.bounds.width().max(1.0)),
            title_h + rows_h + m.pad_y * 2.0,
        );
        let card = self.card_at(size, m.offset);
        let content = Rect::new(
            card.x0 + m.pad_x,
            card.y0 + m.pad_y,
            card.x1 - m.pad_x,
            card.y1 - m.pad_y,
        );
        let title = (!self.title.is_empty())
            .then(|| Rect::new(content.x0, content.y0, content.x1, content.y0 + m.line));
        let mut y = content.y0 + title_h;
        let rows = widths
            .iter()
            .zip(&self.rows)
            .map(|(&(lead, label_w, value_w), row)| {
                let line = Rect::new(content.x0, y, content.x1, y + m.line);
                y += m.line + m.row_gap;
                let swatch = row.swatch.map(|_| {
                    let cy = line.center().y;
                    Rect::new(
                        line.x0,
                        cy - m.swatch / 2.0,
                        line.x0 + m.swatch,
                        cy + m.swatch / 2.0,
                    )
                });
                let value = Rect::new((line.x1 - value_w).max(line.x0), line.y0, line.x1, line.y1);
                let label_end = if value_w > 0.0 {
                    (value.x0 - m.value_gap).max(line.x0 + lead)
                } else {
                    line.x1
                };
                let label = Rect::new(
                    line.x0 + lead,
                    line.y0,
                    label_end.min(line.x0 + lead + label_w),
                    line.y1,
                );
                PlacedRow {
                    swatch,
                    label,
                    value,
                }
            })
            .collect();
        Placed { card, title, rows }
    }

    /// The card of `size` beside the anchor: after it, or before it where it would leave the
    /// bounds or cover what it keeps clear of; centred on the anchor's height and kept inside.
    fn card_at(&self, (width, height): (f64, f64), offset: f64) -> Rect {
        let bounds = self.bounds;
        let y =
            (self.anchor.y - height / 2.0).clamp(bounds.y0, (bounds.y1 - height).max(bounds.y0));
        let at = |x: f64| {
            let x = x.clamp(bounds.x0, (bounds.x1 - width).max(bounds.x0));
            Rect::new(x, y, x + width, y + height)
        };
        let after = self.anchor.x + offset;
        let before = self.anchor.x - offset - width;
        let fits = |x: f64| x >= bounds.x0 && x + width <= bounds.x1;
        let clear = |rect: Rect| {
            self.avoid
                .iter()
                .all(|avoid| rect.intersect(*avoid).area() <= 0.0)
        };
        [after, before]
            .into_iter()
            .filter(|&x| fits(x))
            .map(at)
            .find(|rect| clear(*rect))
            .or_else(|| [after, before].into_iter().find(|&x| fits(x)).map(at))
            .unwrap_or_else(|| at(after))
    }

    /// The highlighted marks and the card over them, in `theme`.
    pub(crate) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        for mark in &self.marks {
            paint_mark(scene, transform, theme, *mark);
        }
        let placed = self.place(tr, theme);
        let m = Metrics::of(theme);
        tilo_ui_base::surface::shadow(
            scene,
            transform,
            placed.card,
            m.radius,
            &theme.tokens.elevation.floating,
        );
        tilo_ui_base::surface::panel(scene, transform, placed.card, m.radius, theme);
        let on_panel = theme.on_ground(tilo_ui_base::chrome::panel_ground(theme));
        let styles = Styles::of(theme);
        let ink = Inks::of(&on_panel);
        let mut words = |tr: &mut TextRenderer,
                         text: &str,
                         rect: Rect,
                         color,
                         style: &TextStyle| {
            if text.is_empty() || rect.width() <= 0.0 {
                return;
            }
            let shown =
                tr.elide_to_width_style(text, tilo_ui_base::num::f32_of_f64(rect.width()), style);
            let (_, h) = tr.measure_style(&shown, None, style);
            tr.draw_style(
                scene,
                &shown,
                color,
                transform * Affine::translate((rect.x0, rect.center().y - f64::from(h) / 2.0)),
                None,
                style,
            );
        };
        if let Some(rect) = placed.title {
            words(tr, &self.title, rect, ink.title, &styles.title);
        }
        for (row, at) in self.rows.iter().zip(&placed.rows) {
            if row.value.is_empty() {
                words(tr, &row.label, at.label, ink.note, &styles.note);
                continue;
            }
            words(
                tr,
                &row.label,
                at.label,
                if row.strong { ink.title } else { ink.note },
                &styles.label,
            );
            words(tr, &row.value, at.value, ink.title, &styles.value);
        }
        for (row, at) in self.rows.iter().zip(&placed.rows) {
            if let (Some(color), Some(rect)) = (row.swatch, at.swatch) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &Circle::new(rect.center(), rect.width() / 2.0),
                );
            }
        }
    }
}

/// Where [`Readout::place`] put the card and its words, in the window.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub(crate) card: Rect,
    pub(crate) title: Option<Rect>,
    pub(crate) rows: Vec<PlacedRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedRow {
    pub(crate) swatch: Option<Rect>,
    pub(crate) label: Rect,
    pub(crate) value: Rect,
}

/// The card's spacing, every value a token: a tooltip's material, sized for a few rows.
struct Metrics {
    pad_x: f64,
    pad_y: f64,
    line: f64,
    row_gap: f64,
    title_gap: f64,
    swatch: f64,
    swatch_gap: f64,
    value_gap: f64,
    offset: f64,
    radius: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            pad_x: f64::from(spacing.s3),
            pad_y: f64::from(spacing.s2),
            line: f64::from(theme.text(TextRole::Caption).line_height_px()).ceil(),
            row_gap: f64::from(spacing.rhythm.tight) / 2.0,
            title_gap: f64::from(spacing.rhythm.tight),
            swatch: f64::from(spacing.rhythm.related),
            swatch_gap: f64::from(spacing.s2),
            value_gap: f64::from(spacing.s4),
            offset: f64::from(spacing.s3),
            radius: f64::from(theme.tokens.radii.control),
        }
    }
}

struct Styles {
    title: TextStyle,
    label: TextStyle,
    note: TextStyle,
    value: TextStyle,
}

impl Styles {
    fn of(theme: &Theme) -> Self {
        let caption = theme.text(TextRole::Caption);
        Self {
            title: caption.clone().with_weight(TextWeight::Semibold),
            label: caption.clone(),
            note: caption.clone(),
            value: caption.numeric().with_weight(TextWeight::Semibold),
        }
    }
}

struct Inks {
    title: Color,
    note: Color,
}

impl Inks {
    fn of(on_panel: &Theme) -> Self {
        Self {
            title: on_panel.tokens.colors.text_primary,
            note: on_panel.tokens.colors.text_dim,
        }
    }
}

/// A marked dot's radius, and how far its ring stands out of it: the dot is a spacing step
/// across and its ring half a step, so both grow with the person's scale.
fn dot_radius(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1)
}

fn dot_ring(theme: &Theme) -> f64 {
    dot_radius(theme) / 2.0
}

fn guide_ink(theme: &Theme) -> Color {
    with_alpha(theme.tokens.colors.text_dim, 150)
}

fn band_ink(theme: &Theme) -> Color {
    with_alpha(theme.tokens.colors.text_primary, 14)
}

fn paint_mark(scene: &mut Scene, transform: Affine, theme: &Theme, mark: Highlight) {
    match mark {
        Highlight::Guide { x, y0, y1 } => scene.stroke(
            &Stroke::new(1.0),
            transform,
            guide_ink(theme),
            None,
            &Line::new((x, y0), (x, y1)),
        ),
        Highlight::Dot { at, color } => {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &Circle::new(at, dot_radius(theme) + dot_ring(theme)),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Circle::new(at, dot_radius(theme)),
            );
        }
        Highlight::Band(rect) => scene.fill(
            Fill::NonZero,
            transform,
            band_ink(theme),
            None,
            &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
        ),
        Highlight::Ring { rect, radius } => scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &RoundedRect::from_rect(rect.inflate(1.0, 1.0), radius + 1.0),
        ),
    }
}

/// A value as a readout writes it, in the reader's own digits: whole numbers whole, a fraction
/// to the two places it needs, a large one shortened (`12.5K`); then `unit`, when there is one:
/// a word after a space, a sign such as `/s` right after the number.
pub(crate) fn number(value: f64, unit: &str) -> String {
    let digits = if value.abs() >= 10_000.0 {
        None
    } else if (value - value.round()).abs() < 1e-9 {
        Some(0)
    } else if (value * 10.0 - (value * 10.0).round()).abs() < 1e-9 {
        Some(1)
    } else {
        Some(2)
    };
    let number = digits.map_or_else(
        || crate::widget::chart_labels::format_compact(value),
        |digits| tilo_ui_base::format::decimal(value, digits),
    );
    match unit.chars().next() {
        None => number,
        Some(first) if first.is_alphanumeric() => format!("{number} {unit}"),
        Some(_) => format!("{number}{unit}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readout(anchor: Point) -> Readout {
        let mut readout = Readout::new("9:54", anchor, Rect::new(0.0, 0.0, 400.0, 200.0));
        readout
            .value(Some(Color::WHITE), "Requests", "31", false)
            .value(Some(Color::BLACK), "Errors", "9", true);
        readout
    }

    #[test]
    fn a_readout_stands_beside_its_anchor_and_inside_its_chart() {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let right = readout(Point::new(40.0, 100.0)).place(&mut tr, &theme);
        assert!(right.card.x0 > 40.0, "after the anchor: {:?}", right.card);
        let left = readout(Point::new(390.0, 100.0)).place(&mut tr, &theme);
        assert!(
            left.card.x1 < 390.0,
            "flipped before it at the edge: {:?}",
            left.card
        );
        let low = readout(Point::new(40.0, 199.0)).place(&mut tr, &theme);
        assert!(low.card.y1 <= 200.0 + 1e-9, "kept inside: {:?}", low.card);
        // Every row's value ends on the card's inner edge, and its label stands after its swatch.
        for row in &right.rows {
            assert!((row.value.x1 - right.rows[0].value.x1).abs() < 1e-9);
            assert!(row.label.x0 >= row.swatch.expect("a swatch").x1);
            assert!(row.label.x1 <= row.value.x0);
        }
    }

    #[test]
    fn a_readout_keeps_clear_of_what_it_avoids() {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let mut covered = readout(Point::new(300.0, 100.0));
        covered.bounds = Rect::new(0.0, 0.0, 800.0, 200.0);
        covered.avoid.push(Rect::new(300.0, 0.0, 800.0, 200.0));
        let placed = covered.place(&mut tr, &theme);
        assert!(placed.card.x1 <= 300.0, "{:?}", placed.card);
    }

    #[test]
    fn a_readout_says_what_it_reads_in_one_line() {
        assert_eq!(readout(Point::ZERO).words(), "9:54: Requests 31, Errors 9");
        assert_eq!(number(6.0, ""), "6");
        assert_eq!(number(6.5, "h"), "6.5 h");
        assert_eq!(number(24.0, "/s"), "24/s");
        assert_eq!(number(0.126, ""), "0.13");
    }
}
