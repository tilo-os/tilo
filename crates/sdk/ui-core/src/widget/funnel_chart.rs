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
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

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
    /// The stage's narrower and wider edge.
    narrow_width: f64,
    wide_width: f64,
    conversion: f64,
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
            frame: PlotFrame::new(380.0, 150.0),
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
    pub fn inspect(&mut self, id: &str) -> bool {
        let stage = self.marks.stages.iter().position(|stage| stage.id == id);
        self.inspect_item(stage)
    }
}

impl FunnelMarks {
    fn content_rect(&self) -> Rect {
        Rect::new(
            self.frame.rect.x0 + 90.0,
            self.frame.rect.y0
                + if self.frame.title.is_empty() {
                    14.0
                } else {
                    48.0
                },
            self.frame.rect.x1 - 90.0,
            self.frame.rect.y1 - 14.0,
        )
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
    /// A stage's name, and its value under it where there is room: inside
    /// the stage where both fit its narrower edge, otherwise beside the
    /// funnel on the leading side, shortened to the room there and dropped
    /// (the tooltip names the stage) only where not even a short name fits.
    fn draw_stage_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        geometry: &StageGeometry,
        color: Color,
    ) {
        let Some(stage) = self.stages.get(geometry.index) else {
            return;
        };
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
        // The narrower of the stage's two edges is the room inside it.
        let narrow = geometry.narrow_width;
        let inside_ink = theme
            .tokens
            .colors
            .ink_on(color, theme.tokens.colors.text_primary, 4.5);
        let two_lines = vh > 0.0 && nh + vh + pad <= bounds.height();
        let block_h = if two_lines { nh + vh } else { nh };
        let top = bounds.center().y - block_h / 2.0;
        if nw.max(if two_lines { vw } else { 0.0 }) + 2.0 * pad <= narrow
            && nh + pad <= bounds.height()
        {
            let cx = bounds.center().x;
            tr.draw_style(
                scene,
                &stage.label,
                inside_ink,
                transform * Affine::translate((cx - nw / 2.0, top)),
                None,
                &name_style,
            );
            if two_lines && let Some(value) = value.as_deref() {
                tr.draw_style(
                    scene,
                    value,
                    inside_ink,
                    transform * Affine::translate((cx - vw / 2.0, top + nh)),
                    None,
                    &value_style,
                );
            }
            return;
        }
        // Beside the stage, trailing-aligned to its leading edge at its middle.
        let edge = bounds.center().x - narrow.max(geometry.wide_width) / 2.0;
        let room = edge - pad - self.frame.rect.x0 - pad;
        let Some((name, w, _)) =
            crate::widget::chart_labels::fit_label(tr, &stage.label, room, &name_style)
        else {
            return;
        };
        let x1 = edge - pad;
        tr.draw_style(
            scene,
            &name,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x1 - w, top)),
            None,
            &name_style,
        );
        if two_lines
            && let Some(value) = value.as_deref()
            && vw <= room
        {
            tr.draw_style(
                scene,
                value,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x1 - vw, top + nh)),
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
                tr.draw_style(
                    scene,
                    &label,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((geometry.bounds.x1 + 10.0, geometry.bounds.y0 + 4.0)),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let Some(stage) = self.stages.get(index)
        {
            let detail = if stage.detail.is_empty() {
                format_compact(stage.value)
            } else {
                stage.detail.clone()
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.frame.inspection.pointer,
                self.frame.rect,
                140.0,
                (&stage.label, &detail),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_stages_are_omitted() {
        let chart = FunnelChart::new(vec![
            ChartItem::new("bad", "Bad", f64::NAN),
            ChartItem::new("zero", "Zero", 0.0),
        ]);
        assert!(chart.marks.valid().is_empty());
    }
}
