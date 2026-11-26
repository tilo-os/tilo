#![allow(
    clippy::arithmetic_side_effects,
    reason = "funnel projection is bounded floating-point geometry"
)]

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::ChartItem;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

/// The height the funnel draws at where its rows have room: a stage's name breaks over two lines
/// beside it, so a chart of many stages grows to keep every row that tall.
const NATURAL_HEIGHT: f64 = 380.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunnelDirection {
    Funnel,
    Pyramid,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent label, conversion, and interaction switches form the public chart style"
)]
#[derive(Clone, Debug)]
pub struct FunnelStyle {
    pub direction: FunnelDirection,
    pub proportional_height: bool,
    pub gap: f64,
    pub minimum_width_fraction: f64,
    pub show_values: bool,
    pub show_conversion: bool,
    pub tooltip: bool,
}

impl Default for FunnelStyle {
    fn default() -> Self {
        Self {
            direction: FunnelDirection::Funnel,
            proportional_height: false,
            gap: 5.0,
            minimum_width_fraction: 0.12,
            show_values: true,
            show_conversion: true,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug)]
struct StageGeometry {
    index: usize,
    path: BezPath,
    bounds: Rect,
    narrow_width: f64,
    wide_width: f64,
    conversion: f64,
}

/// A stage's name as it is painted: the lines it reads as with the box each covers, its value
/// under them, and the ink it reads in.
#[derive(Clone, Debug)]
struct StageLabel {
    lines: Vec<(String, Rect)>,
    value: Option<(String, Rect)>,
    ink: Color,
    /// The value's ink: the name's on a band, where only an ink chosen against the band's
    /// colour reads, and the theme's secondary text beside the funnel, on the chart's ground.
    value_ink: Color,
}

pub struct FunnelMarks {
    stages: Vec<ChartItem>,
    style: FunnelStyle,
    frame: PlotFrame<usize>,
}

/// Stages narrowing (funnel) or widening (pyramid) by value.
pub type FunnelChart = Plot<FunnelMarks>;

impl FunnelChart {
    pub fn new(stages: Vec<ChartItem>) -> Self {
        Plot::from_marks(FunnelMarks {
            stages,
            style: FunnelStyle::default(),
            frame: PlotFrame::fill(150.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: FunnelStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn direction(mut self, direction: FunnelDirection) -> Self {
        self.marks.style.direction = direction;
        self
    }
    /// The stages, from the first to the last: each stage's place in the funnel follows its
    /// order.
    pub fn set_stages(&mut self, stages: Vec<ChartItem>) {
        let marks = &mut self.marks;
        marks.stages = stages;
        marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let stage = self.marks.stages.iter().position(|stage| stage.id == id);
        self.inspect_item(stage)
    }
}

impl FunnelMarks {
    fn content_rect(&self) -> Rect {
        self.frame.inside()
    }
    /// The same margin on both sides of the centred funnel: a stage's name where it stands beside
    /// a stage too narrow for it, and the conversion beside the other edge, a step from the
    /// stages; never more than a quarter of the chart a side.
    fn side_margin(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let name_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let names = self
            .stages
            .iter()
            .map(|stage| f64::from(tr.measure_style(&stage.label, None, &name_style).0))
            .fold(0.0, f64::max);
        let conversion = if self.style.show_conversion {
            crate::widget::plot::widest(tr, theme, ["100%"])
        } else {
            0.0
        };
        (names.max(conversion) + crate::widget::plot::step(theme)).min(width / 4.0)
    }
    fn measure_margins(&self, tr: &mut TextRenderer, theme: &Theme) -> Margins {
        let side = self.side_margin(tr, theme, self.frame.rect.width());
        let mut margins = Margins::header(theme, &self.frame.title, 0);
        margins.left = side;
        margins.right = side;
        margins
    }
    fn valid(&self) -> Vec<usize> {
        self.stages
            .iter()
            .enumerate()
            .filter(|(_, stage)| stage.value.is_finite() && stage.value > 0.0)
            .map(|(index, _)| index)
            .collect()
    }
    fn width_at(&self, value: f64, max: f64) -> f64 {
        let fraction = (value / max).clamp(self.style.minimum_width_fraction.clamp(0.01, 1.0), 1.0);
        let fraction = if self.style.direction == FunnelDirection::Pyramid {
            1.0 - fraction + self.style.minimum_width_fraction.clamp(0.01, 1.0)
        } else {
            fraction
        };
        self.content_rect().width() * fraction.clamp(self.style.minimum_width_fraction, 1.0)
    }
    fn geometry(&self) -> Vec<StageGeometry> {
        let valid = self.valid();
        if valid.is_empty() {
            return Vec::new();
        }
        let content = self.content_rect();
        let max = valid
            .iter()
            .filter_map(|index| self.stages.get(*index))
            .map(|stage| stage.value)
            .fold(0.0_f64, f64::max)
            .max(f64::EPSILON);
        let total = valid
            .iter()
            .filter_map(|index| self.stages.get(*index))
            .map(|stage| stage.value)
            .sum::<f64>();
        let gaps = self.style.gap
            * f64::from(u32::try_from(valid.len().saturating_sub(1)).unwrap_or(u32::MAX));
        let usable = (content.height() - gaps).max(1.0);
        let mut y = content.y0;
        let mut output = Vec::new();
        for (position, index) in valid.iter().enumerate() {
            let Some(stage) = self.stages.get(*index) else {
                continue;
            };
            let next_value = valid
                .get(position + 1)
                .and_then(|next| self.stages.get(*next))
                .map_or(
                    if self.style.direction == FunnelDirection::Funnel {
                        stage.value * self.style.minimum_width_fraction
                    } else {
                        stage.value
                    },
                    |next| next.value,
                );
            let height = if self.style.proportional_height {
                usable * stage.value / total.max(f64::EPSILON)
            } else {
                usable / f64::from(u32::try_from(valid.len()).unwrap_or(u32::MAX))
            };
            let top_width = self.width_at(stage.value, max);
            let bottom_width = self.width_at(next_value, max);
            let center = content.center().x;
            let mut path = BezPath::new();
            path.move_to((center - top_width / 2.0, y));
            path.line_to((center + top_width / 2.0, y));
            path.line_to((center + bottom_width / 2.0, y + height));
            path.line_to((center - bottom_width / 2.0, y + height));
            path.close_path();
            let bounds = Rect::new(
                center - top_width.max(bottom_width) / 2.0,
                y,
                center + top_width.max(bottom_width) / 2.0,
                y + height,
            );
            let previous = position
                .checked_sub(1)
                .and_then(|previous| valid.get(previous))
                .and_then(|previous| self.stages.get(*previous))
                .map_or(stage.value, |previous| previous.value);
            output.push(StageGeometry {
                index: *index,
                path,
                bounds,
                narrow_width: top_width.min(bottom_width),
                wide_width: top_width.max(bottom_width),
                conversion: stage.value / previous.max(f64::EPSILON),
            });
            y += height + self.style.gap;
        }
        output
    }
    /// A stage's name as it is painted, whole inside the stage where it fits, otherwise beside
    /// it.
    fn stage_label(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        geometry: &StageGeometry,
        color: Color,
    ) -> Option<StageLabel> {
        let stage = self.stages.get(geometry.index)?;
        let name_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let value_style = theme.text(TextRole::Caption);
        let pad = f64::from(theme.tokens.spacing.s2);
        let value = self.style.show_values.then(|| format_compact(stage.value));
        let (nw, nh) = tr.measure_style(&stage.label, None, &name_style);
        let (nw, nh) = (f64::from(nw), f64::from(nh));
        let (vw, vh) = value.as_deref().map_or((0.0, 0.0), |v| {
            let (w, h) = tr.measure_style(v, None, &value_style);
            (f64::from(w), f64::from(h))
        });
        let bounds = geometry.bounds;
        let narrow = geometry.narrow_width;
        let inside_ink = crate::widget::chart_labels::ink_on_fill(theme, color);
        let two_lines = vh > 0.0 && nh + vh + pad <= bounds.height();
        let block_h = if two_lines { nh + vh } else { nh };
        let top = bounds.center().y - block_h / 2.0;
        if nw.max(if two_lines { vw } else { 0.0 }) + 2.0 * pad <= narrow
            && nh + pad <= bounds.height()
        {
            let cx = bounds.center().x;
            let name = Rect::new(cx - nw / 2.0, top, cx + nw / 2.0, top + nh);
            let value = if two_lines {
                value.as_ref().map(|value| {
                    let at = Rect::new(cx - vw / 2.0, top + nh, cx + vw / 2.0, top + nh + vh);
                    (value.clone(), at)
                })
            } else {
                None
            };
            return Some(StageLabel {
                lines: vec![(stage.label.clone(), name)],
                value,
                ink: inside_ink,
                value_ink: inside_ink,
            });
        }
        // Beside the funnel: broken over the lines the row holds so the name reads whole, then
        // shortened to whole words around one ellipsis, and left out where no whole-word reading
        // fits — its tooltip and accessible name still read it.
        let edge = bounds.center().x - narrow.max(geometry.wide_width) / 2.0;
        let spare = edge - pad - self.frame.rect.x0 - pad;
        let line = f64::from(name_style.line_height_px());
        let value_shown = vh > 0.0 && vw <= spare && line + vh + pad <= bounds.height();
        let room = bounds.height() - if value_shown { vh } else { 0.0 };
        if room + 0.5 < line {
            return None;
        }
        let mut lines = crate::widget::chart_labels::wrapped_labels(tr, &stage.label, &name_style)
            .into_iter()
            .find(|wrapped| wrapped.width <= spare && wrapped.height <= room + 0.5)
            .map(|wrapped| wrapped.lines)
            .unwrap_or_default();
        if lines.is_empty() {
            lines = crate::widget::chart_labels::shortened_labels(tr, &stage.label, &name_style)
                .into_iter()
                .find(|(_, width, height)| *width <= spare && *height <= room + 0.5)
                .map(|(text, width, height)| vec![(text, width, height)])
                .unwrap_or_default();
        }
        if lines.is_empty() {
            return None;
        }
        let height: f64 = lines.iter().map(|(_, _, height)| *height).sum();
        let total = height + if value_shown { vh } else { 0.0 };
        if total > bounds.height() + 0.5 {
            return None;
        }
        let x1 = edge - pad;
        let mut y = bounds.center().y - total / 2.0;
        let mut shown = Vec::with_capacity(lines.len());
        for (text, width, height) in lines {
            shown.push((text, Rect::new(x1 - width, y, x1, y + height)));
            y += height;
        }
        let value = if value_shown {
            value.as_ref().map(|value| {
                let at = Rect::new(x1 - vw, y, x1, y + vh);
                (value.clone(), at)
            })
        } else {
            None
        };
        Some(StageLabel {
            lines: shown,
            value,
            ink: theme.tokens.colors.text_primary,
            value_ink: theme.tokens.colors.text_dim,
        })
    }

    fn draw_stage_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        geometry: &StageGeometry,
        color: Color,
    ) {
        let Some(label) = self.stage_label(tr, theme, geometry, color) else {
            return;
        };
        let name_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let value_style = theme.text(TextRole::Caption);
        for (text, rect) in label.lines {
            tr.draw_style(
                scene,
                &text,
                label.ink,
                transform * Affine::translate((rect.x0, rect.y0)),
                None,
                &name_style,
            );
        }
        if let Some((text, rect)) = label.value {
            tr.draw_style(
                scene,
                &text,
                label.value_ink,
                transform * Affine::translate((rect.x0, rect.y0)),
                None,
                &value_style,
            );
        }
    }

    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }
}

impl PlotMarks for FunnelMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "FunnelChart";
    const LABEL: &'static str = "Funnel chart";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn label(&self) -> &'static str {
        if self.style.direction == FunnelDirection::Funnel {
            Self::LABEL
        } else {
            "Pyramid chart"
        }
    }
    fn hit(&self, point: Point) -> Option<usize> {
        self.geometry()
            .into_iter()
            .find(|stage| stage.bounds.contains(point))
            .map(|stage| stage.index)
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        Some(self.stages.get(index)?.id.clone())
    }
    fn keyboard_items(&self) -> Vec<usize> {
        self.valid()
    }
    fn anchor(&self, &index: &usize) -> Option<Point> {
        self.geometry()
            .into_iter()
            .find(|stage| stage.index == index)
            .map(|stage| stage.bounds.center())
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.valid()
            .into_iter()
            .filter_map(|index| self.stages.get(index))
            .map(|stage| {
                crate::widget::plot::value_item(&stage.id, &stage.label, &stage.detail, stage.value)
            })
            .collect()
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = self.measure_margins(tr, theme);
    }

    /// A row tall enough for a stage's name broken over its lines beside the funnel, with its
    /// value under it, for every stage: a chart of few stages still draws at
    /// [`NATURAL_HEIGHT`], and one of many grows so their labels read whole.
    fn natural_height(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let stages = self.valid().len();
        if stages == 0 {
            return NATURAL_HEIGHT;
        }
        let name_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let line = f64::from(name_style.line_height_px());
        let value_line = crate::widget::plot::caption_line(theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        let side = self.side_margin(tr, theme, width);
        let spare = (side - 2.0 * pad).max(0.0);
        let value = if self.style.show_values {
            value_line
        } else {
            0.0
        };
        let mut row = line + value;
        for stage in self
            .valid()
            .iter()
            .filter_map(|index| self.stages.get(*index))
        {
            let lines = crate::widget::chart_labels::wrapped_labels(tr, &stage.label, &name_style)
                .into_iter()
                .find(|wrapped| wrapped.width <= spare)
                .map_or(1, |wrapped| wrapped.lines.len());
            row = row.max(crate::num::f64_of_usize(lines) * line + value);
        }
        let gaps = self.style.gap * crate::num::f64_of_usize(stages.saturating_sub(1));
        let title = crate::widget::chart_navigation::title_band(theme, &self.frame.title);
        (title + crate::num::f64_of_usize(stages) * row + gaps).max(NATURAL_HEIGHT)
    }

    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.follow_keyboard_anchor();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        for geometry in self.geometry() {
            let Some(stage) = self.stages.get(geometry.index) else {
                continue;
            };
            let color = stage
                .color
                .unwrap_or_else(|| Self::palette(theme, geometry.index));
            scene.fill(Fill::NonZero, transform, color, None, &geometry.path);
            if self.frame.inspection.hovered == Some(geometry.index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &geometry.path,
                );
            }
            self.draw_stage_label(scene, transform, tr, theme, &geometry, color);
            if self.style.show_conversion && geometry.conversion < 0.999 {
                let label = format!("{:.0}%", geometry.conversion * 100.0);
                let (w, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
                let x = geometry.bounds.x1 + crate::widget::plot::step(theme);
                if x + f64::from(w) <= self.frame.rect.x1 + 0.5 {
                    tr.draw_style(
                        scene,
                        &label,
                        theme.tokens.colors.text_dim,
                        transform
                            * Affine::translate((
                                x,
                                geometry.bounds.y0 + f64::from(theme.tokens.spacing.s1),
                            )),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                }
            }
        }
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let Some(stage) = self.stages.get(index)
        {
            let detail = if stage.detail.is_empty() {
                format_compact(stage.value)
            } else {
                stage.detail.clone()
            };
            return Some(Readout::of_mark(
                &stage.label,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

mod funnel_chart_component;
pub use funnel_chart_component::{FunnelChartProps, FunnelStage, stages_description};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::chart_labels::reads_as_whole_words;

    #[test]
    fn invalid_stages_are_omitted() {
        let chart = FunnelChart::new(vec![
            ChartItem::new("bad", "Bad", f64::NAN),
            ChartItem::new("zero", "Zero", 0.0),
        ]);
        assert!(chart.marks.valid().is_empty());
    }

    /// The gallery's long funnel: names far wider than the row they stand beside.
    fn self_serve_path() -> Vec<ChartItem> {
        vec![
            ChartItem::new(
                "opened",
                "Opened the product page from a shared link",
                860.0,
            ),
            ChartItem::new("read", "Read past the first screen of the tour", 412.0),
            ChartItem::new(
                "started",
                "Started an import of an existing workspace",
                148.0,
            ),
            ChartItem::new("finished", "Finished the import and invited the team", 73.0),
        ]
    }

    /// Every stage's name stays readable at a laptop's width and a phone's: it is the whole name
    /// — broken over the lines its row holds — or whole words at its start and its end around
    /// one ellipsis, never a cut inside a word. A stage left unnamed is still read whole by its
    /// tooltip and accessible name, and no label paints outside the chart.
    #[test]
    fn stage_labels_stay_readable_at_every_width() {
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let mut chart = FunnelChart::new(self_serve_path());
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 380.0);
                chart.marks.place(&mut tr, &theme);
                let rect = chart.marks.frame.rect;
                let items = chart.marks.items();
                let geometry = chart.marks.geometry();
                assert_eq!(geometry.len(), 4, "{width}: every stage is drawn");
                let mut shown = 0;
                for stage in &geometry {
                    let name = chart
                        .marks
                        .stages
                        .get(stage.index)
                        .map(|stage| stage.label.clone())
                        .unwrap_or_default();
                    assert!(
                        items.iter().any(|item| item.name == name),
                        "{width}: {name:?} is named nowhere"
                    );
                    let Some(label) = chart
                        .marks
                        .stage_label(&mut tr, &theme, stage, Color::BLACK)
                    else {
                        continue;
                    };
                    shown += 1;
                    let text = label
                        .lines
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert!(
                        reads_as_whole_words(&name, &text),
                        "{width}: {name:?} reads as {text:?}"
                    );
                    let rects = label
                        .lines
                        .iter()
                        .map(|(_, rect)| *rect)
                        .chain(label.value.iter().map(|(_, rect)| *rect));
                    for at in rects {
                        assert!(
                            at.x0 >= rect.x0 - 0.5
                                && at.x1 <= rect.x1 + 0.5
                                && at.y0 >= rect.y0 - 0.5
                                && at.y1 <= rect.y1 + 0.5,
                            "{width}: {at:?} lies outside {rect:?}"
                        );
                    }
                }
                // A laptop's margin holds every long name over its lines; a phone's holds the
                // ones a whole word either side of an ellipsis still names, and drops the rest.
                let least = if width > 500.0 { 4 } else { 1 };
                assert!(shown >= least, "{width}: only {shown} stages are named");
            }
        }
    }

    /// A value read on its band is as legible as the name above it, in either theme: on every
    /// band colour the theme gives a stage, it clears the text floor against that colour.
    #[test]
    fn a_value_on_its_band_reads_against_the_band() {
        let stages = ["Visits", "Sign-ups", "Trials", "Paid", "Kept", "Upgraded"]
            .iter()
            .enumerate()
            .map(|(i, name)| ChartItem::new(*name, *name, 1000.0 / (i as f64 + 1.0)))
            .collect::<Vec<_>>();
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            let mut chart = FunnelChart::new(stages.clone());
            chart.marks.frame.rect = Rect::new(0.0, 0.0, 1000.0, 380.0);
            chart.marks.place(&mut tr, &theme);
            let mut on_band = 0;
            for stage in chart.marks.geometry() {
                let fill = FunnelMarks::palette(&theme, stage.index);
                let Some(label) = chart.marks.stage_label(&mut tr, &theme, &stage, fill) else {
                    continue;
                };
                let (Some((_, name)), Some((value, _))) = (label.lines.first(), &label.value)
                else {
                    continue;
                };
                if (name.center().x - stage.bounds.center().x).abs() > 1.0 {
                    continue;
                }
                on_band += 1;
                let ratio = crate::a11y::contrast_ratio(label.value_ink, fill);
                assert!(
                    ratio >= crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                    "{value:?} on stage {} reads at {ratio:.2}:1",
                    stage.index
                );
            }
            assert!(on_band >= 3, "short names stand on their bands: {on_band}");
        }
    }

    /// A row too short for three lines still holds the name broken over the lines it has, and
    /// the natural height grows a chart of many stages so their rows hold the name whole.
    /// A chart of many stages grows past the height it draws at so each row holds its name over
    /// the lines it needs: at the height the chart asks for, every stage reads whole.
    #[test]
    fn the_natural_height_holds_the_names_wrapped() {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let mut chart = FunnelChart::new(self_serve_path().into_iter().cycle().take(8).collect());
        let height = chart.marks.natural_height(&mut tr, &theme, 1000.0);
        assert!(
            height > NATURAL_HEIGHT,
            "eight stages of long names need more than {NATURAL_HEIGHT}: {height}"
        );
        chart.marks.frame.rect = Rect::new(0.0, 0.0, 1000.0, height);
        chart.marks.place(&mut tr, &theme);
        let mut named = 0;
        for geometry in chart.marks.geometry() {
            let name = chart
                .marks
                .stages
                .get(geometry.index)
                .map(|stage| stage.label.clone())
                .unwrap_or_default();
            let Some(label) = chart
                .marks
                .stage_label(&mut tr, &theme, &geometry, Color::BLACK)
            else {
                continue;
            };
            named += 1;
            let text = label
                .lines
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                text, name,
                "the row the chart asked for reads the name whole"
            );
        }
        assert_eq!(named, 8, "every stage is named at the height it asked for");
        let four = FunnelChart::new(self_serve_path());
        assert_eq!(
            four.marks.natural_height(&mut tr, &theme, 1000.0),
            NATURAL_HEIGHT,
            "the gallery's four stages keep the height they draw at"
        );
    }
}
