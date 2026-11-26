//! Interactive matrix heatmaps with labelled axes and theme-aware ramps.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "heatmap layout and colour interpolation are bounded numeric operations"
)]

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

#[derive(Clone, Debug)]
pub struct HeatmapCell {
    pub id: String,
    pub row: usize,
    pub column: usize,
    pub value: f64,
    pub label: String,
    pub detail: String,
}

impl HeatmapCell {
    pub fn new(id: impl Into<String>, row: usize, column: usize, value: f64) -> Self {
        Self {
            id: id.into(),
            row,
            column,
            value,
            label: String::new(),
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct HeatmapStyle {
    pub low_color: Option<Color>,
    pub high_color: Option<Color>,
    pub missing_color: Option<Color>,
    pub cell_gap: f64,
    pub corner_radius: f64,
    pub show_values: bool,
    pub show_axis_labels: bool,
    pub tooltip: bool,
}

impl Default for HeatmapStyle {
    fn default() -> Self {
        Self {
            low_color: None,
            high_color: None,
            missing_color: None,
            cell_gap: 4.0,
            corner_radius: 5.0,
            show_values: false,
            show_axis_labels: true,
            tooltip: true,
        }
    }
}

pub struct HeatmapMarks {
    rows: Vec<String>,
    columns: Vec<String>,
    cells: Vec<HeatmapCell>,
    style: HeatmapStyle,
    frame: PlotFrame<usize>,
}

/// A matrix of cells coloured along a theme ramp.
pub type Heatmap = Plot<HeatmapMarks>;

impl Heatmap {
    pub fn new(rows: Vec<String>, columns: Vec<String>, cells: Vec<HeatmapCell>) -> Self {
        Plot::from_marks(HeatmapMarks {
            rows,
            columns,
            cells,
            style: HeatmapStyle::default(),
            frame: PlotFrame::new(340.0, 100.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: HeatmapStyle) -> Self {
        self.marks.style = style;
        self
    }
    /// The matrix a person reads: its rows and columns in order, and the cells at their
    /// crossings.
    pub fn set_cells(&mut self, rows: Vec<String>, columns: Vec<String>, cells: Vec<HeatmapCell>) {
        let marks = &mut self.marks;
        marks.rows = rows;
        marks.columns = columns;
        marks.cells = cells;
        marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let cell = self.marks.cells.iter().position(|cell| cell.id == id);
        self.inspect_item(cell)
    }
}

impl HeatmapMarks {
    fn grid_rect(&self) -> Rect {
        self.frame.inside()
    }
    /// The column labels' line over the grid and the row labels' column before it, a step from
    /// the cells.
    fn measure_margins(&self, tr: &mut TextRenderer, theme: &Theme) -> Margins {
        let mut margins = Margins::header(theme, &self.frame.title, 0);
        if self.style.show_axis_labels {
            let step = crate::widget::plot::step(theme);
            margins.top += crate::widget::plot::caption_line(theme) + step;
            let rows = crate::widget::plot::widest(tr, theme, &self.rows);
            margins.left = (rows + step).min(self.frame.rect.width() / 3.0);
        }
        margins
    }
    fn cell_rect(&self, cell: &HeatmapCell) -> Option<Rect> {
        if self.rows.is_empty()
            || self.columns.is_empty()
            || cell.row >= self.rows.len()
            || cell.column >= self.columns.len()
        {
            return None;
        }
        let grid = self.grid_rect();
        let columns = f64::from(u32::try_from(self.columns.len()).unwrap_or(u32::MAX));
        let rows = f64::from(u32::try_from(self.rows.len()).unwrap_or(u32::MAX));
        let width = grid.width() / columns;
        let height = grid.height() / rows;
        let column = f64::from(u32::try_from(cell.column).unwrap_or(u32::MAX));
        let row = f64::from(u32::try_from(cell.row).unwrap_or(u32::MAX));
        Some(Rect::new(
            grid.x0 + column * width + self.style.cell_gap / 2.0,
            grid.y0 + row * height + self.style.cell_gap / 2.0,
            grid.x0 + (column + 1.0) * width - self.style.cell_gap / 2.0,
            grid.y0 + (row + 1.0) * height - self.style.cell_gap / 2.0,
        ))
    }
    fn range(&self) -> (f64, f64) {
        let (min, max) = self
            .cells
            .iter()
            .map(|cell| cell.value)
            .filter(|value| value.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        if min.is_finite() && max > min {
            (min, max)
        } else {
            (min.min(0.0), min.min(0.0) + 1.0)
        }
    }
}

impl PlotMarks for HeatmapMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "Heatmap";
    const LABEL: &'static str = "Heatmap";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<usize> {
        self.cells.iter().position(|cell| {
            self.cell_rect(cell)
                .is_some_and(|rect| rect.contains(point))
        })
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        Some(self.cells.get(index)?.id.clone())
    }
    fn keyboard_items(&self) -> Vec<usize> {
        let placed = self.cells.iter().map(|cell| self.cell_rect(cell).is_some());
        placed
            .enumerate()
            .filter_map(|(index, placed)| placed.then_some(index))
            .collect()
    }
    fn anchor(&self, &index: &usize) -> Option<Point> {
        Some(self.cell_rect(self.cells.get(index)?)?.center())
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.cells
            .iter()
            .filter(|cell| cell.row < self.rows.len() && cell.column < self.columns.len())
            .map(|cell| {
                let row = self.rows.get(cell.row).map_or("", String::as_str);
                let column = self.columns.get(cell.column).map_or("", String::as_str);
                PlotItem::new(
                    &cell.id,
                    if cell.label.is_empty() {
                        format!("{row}, {column}")
                    } else {
                        cell.label.clone()
                    },
                    if cell.detail.is_empty() {
                        format!("Value {}", cell.value)
                    } else {
                        cell.detail.clone()
                    },
                    cell.id.clone(),
                )
            })
            .collect()
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = self.measure_margins(tr, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "matrix cells, axes and inspector share one retained paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.follow_keyboard_anchor();
        let low = self
            .style
            .low_color
            .unwrap_or(theme.tokens.colors.surfaces.raised);
        let high = self
            .style
            .high_color
            .unwrap_or(theme.tokens.colors.intent.value_fill);
        let missing = self
            .style
            .missing_color
            .unwrap_or(theme.tokens.colors.border_standard);
        let (min, max) = self.range();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        if self.style.show_axis_labels {
            let grid = self.grid_rect();
            let step = crate::widget::plot::step(theme);
            let line = crate::widget::plot::caption_line(theme);
            let column_width = grid.width()
                / f64::from(u32::try_from(self.columns.len().max(1)).unwrap_or(u32::MAX));
            let row_height = grid.height()
                / f64::from(u32::try_from(self.rows.len().max(1)).unwrap_or(u32::MAX));
            for (index, label) in self.columns.iter().enumerate() {
                let x = grid.x0
                    + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * column_width;
                let (width, _) = tr.measure_style(label, None, &theme.text(TextRole::Caption));
                tr.draw_style(
                    scene,
                    label,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((x - f64::from(width) / 2.0, grid.y0 - step - line)),
                    Some(crate::num::f32_of_f64(column_width.max(0.0))),
                    &theme.text(TextRole::Caption),
                );
            }
            for (index, label) in self.rows.iter().enumerate() {
                let y = grid.y0
                    + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * row_height;
                tr.draw_style(
                    scene,
                    label,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((self.frame.rect.x0, y - line / 2.0)),
                    Some(crate::num::f32_of_f64(
                        (grid.x0 - self.frame.rect.x0 - step).max(1.0),
                    )),
                    &theme.text(TextRole::Caption),
                );
            }
        }
        for (index, cell) in self.cells.iter().enumerate() {
            let Some(rect) = self.cell_rect(cell) else {
                continue;
            };
            let color = if cell.value.is_finite() {
                crate::tokens::colors::mix8(
                    low,
                    high,
                    crate::num::f32_of_f64((cell.value - min) / (max - min)),
                )
            } else {
                missing
            };
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(rect, self.style.corner_radius),
            );
            if self.frame.inspection.hovered == Some(index) {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &RoundedRect::from_rect(rect, self.style.corner_radius),
                );
            }
            if self.style.show_values && rect.width() >= 38.0 && rect.height() >= 22.0 {
                let value = format!("{:.1}", cell.value);
                let (width, _) = tr.measure_style(&value, None, &theme.text(TextRole::Caption));
                let ink = crate::widget::chart_labels::ink_on_fill(theme, color);
                tr.draw_style(
                    scene,
                    &value,
                    ink,
                    transform
                        * Affine::translate((
                            rect.center().x - f64::from(width) / 2.0,
                            rect.center().y - 6.0,
                        )),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let Some(cell) = self.cells.get(index)
        {
            let row = self.rows.get(cell.row).map_or("", String::as_str);
            let column = self.columns.get(cell.column).map_or("", String::as_str);
            let title = if cell.label.is_empty() {
                format!("{row} · {column}")
            } else {
                cell.label.clone()
            };
            let detail = if cell.detail.is_empty() {
                format!("Value {:.2}", cell.value)
            } else {
                cell.detail.clone()
            };
            return Some(Readout::of_mark(
                &title,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

mod heatmap_component;
pub use heatmap_component::{HeatmapPoint, HeatmapProps, cells_description};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heatmap_guards_constant_values_and_invalid_coordinates() {
        fn constant_values_have_a_safe_range() {
            let heatmap = Heatmap::new(
                vec!["A".into()],
                vec!["B".into()],
                vec![HeatmapCell::new("x", 0, 0, 3.0)],
            );
            let range = heatmap.marks.range();
            assert!(range.1 > range.0);
        }

        fn invalid_coordinates_have_no_rect() {
            let mut heatmap = Heatmap::new(vec!["A".into()], vec!["B".into()], vec![]);
            heatmap.marks.frame.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
            assert!(
                heatmap
                    .marks
                    .cell_rect(&HeatmapCell::new("x", 2, 0, 1.0))
                    .is_none()
            );
        }

        constant_values_have_a_safe_range();
        invalid_coordinates_have_no_rect();
    }
}
