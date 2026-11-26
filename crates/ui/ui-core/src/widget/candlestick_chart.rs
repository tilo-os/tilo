//! Interactive OHLC/candlestick charts with an optional volume panel.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "financial chart projection is bounded floating-point geometry"
)]

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::{Highlight, Readout};
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks, ValueAxis};

#[derive(Clone, Debug)]
pub struct Candle {
    pub id: String,
    pub label: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub detail: String,
}

impl Candle {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            open,
            high,
            low,
            close,
            volume: 0.0,
            detail: String::new(),
        }
    }

    #[must_use]
    pub fn with_volume(mut self, volume: f64) -> Self {
        self.volume = volume.max(0.0);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn valid(&self) -> bool {
        [self.open, self.high, self.low, self.close]
            .into_iter()
            .all(f64::is_finite)
            && self.high >= self.low
            && self.high >= self.open.max(self.close)
            && self.low <= self.open.min(self.close)
    }
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent financial-chart presentation toggles are intentionally composable"
)]
pub struct CandlestickStyle {
    pub rising_color: Option<Color>,
    pub falling_color: Option<Color>,
    pub neutral_color: Option<Color>,
    pub show_volume: bool,
    pub show_grid: bool,
    pub show_crosshair: bool,
    pub tooltip: bool,
    pub candle_width_fraction: f64,
    pub price_decimals: usize,
}

impl Default for CandlestickStyle {
    fn default() -> Self {
        Self {
            rising_color: None,
            falling_color: None,
            neutral_color: None,
            show_volume: true,
            show_grid: true,
            show_crosshair: true,
            tooltip: true,
            candle_width_fraction: 0.64,
            price_decimals: 2,
        }
    }
}

pub struct CandlestickMarks {
    candles: Vec<Candle>,
    style: CandlestickStyle,
    frame: PlotFrame<usize>,
    /// The gap between the price panel and the volume panel under it.
    panel_gap: f64,
}

pub type CandlestickChart = Plot<CandlestickMarks>;

mod candlestick_chart_component;
pub use candlestick_chart_component::{
    CandleObservation, CandlestickChartProps, candle_description,
};

impl CandlestickChart {
    pub fn new(candles: Vec<Candle>) -> Self {
        Plot::from_marks(CandlestickMarks {
            candles,
            style: CandlestickStyle::default(),
            frame: PlotFrame::new(380.0, 160.0),
            panel_gap: 0.0,
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: CandlestickStyle) -> Self {
        self.marks.style = style;
        self
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let candle = self
            .marks
            .candles
            .iter()
            .position(|candle| candle.id == id && candle.valid());
        self.inspect_item(candle)
    }
}

impl CandlestickMarks {
    /// The candles' panel: what the margins leave, less the volume panel under it.
    fn price_rect(&self) -> Rect {
        let inside = self.frame.inside();
        match self.volume_rect() {
            Some(volume) => Rect::new(
                inside.x0,
                inside.y0,
                inside.x1,
                (volume.y0 - self.panel_gap).max(inside.y0),
            ),
            None => inside,
        }
    }

    /// The volume panel: the last fifth of what the margins leave.
    fn volume_rect(&self) -> Option<Rect> {
        self.style.show_volume.then(|| {
            let inside = self.frame.inside();
            Rect::new(
                inside.x0,
                inside.y1 - inside.height() / 5.0,
                inside.x1,
                inside.y1,
            )
        })
    }

    fn price_label(&self, value: f64) -> String {
        format!("{:.*}", self.style.price_decimals, value)
    }

    fn price_range(&self) -> (f64, f64) {
        let (low, high) = self
            .candles
            .iter()
            .filter(|candle| candle.valid())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), candle| {
                (low.min(candle.low), high.max(candle.high))
            });
        if low.is_finite() && high > low {
            let pad = (high - low) * 0.08;
            (low - pad, high + pad)
        } else {
            (low.min(0.0) - 0.5, low.min(0.0) + 0.5)
        }
    }

    fn slot_width(&self) -> f64 {
        self.price_rect().width()
            / f64::from(u32::try_from(self.candles.len().max(1)).unwrap_or(u32::MAX))
    }

    fn center_x(&self, index: usize) -> f64 {
        self.price_rect().x0
            + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * self.slot_width()
    }

    fn price_y(&self, value: f64) -> f64 {
        let (min, max) = self.price_range();
        let rect = self.price_rect();
        rect.y1 - (value - min) / (max - min) * rect.height()
    }

    fn color(&self, candle: &Candle, theme: &Theme) -> Color {
        if (candle.close - candle.open).abs() <= f64::EPSILON {
            self.style
                .neutral_color
                .unwrap_or(theme.tokens.colors.text_dim)
        } else if candle.close > candle.open {
            self.style
                .rising_color
                .unwrap_or(theme.tokens.colors.semantic.success)
        } else {
            self.style
                .falling_color
                .unwrap_or(theme.tokens.colors.semantic.danger)
        }
    }
}

impl PlotMarks for CandlestickMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "CandlestickChart";
    const LABEL: &'static str = "Candlestick chart";

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        let index = self
            .frame
            .inspection
            .hovered
            .filter(|_| self.style.tooltip)?;
        let candle = self.candles.get(index)?;
        let detail = if candle.detail.is_empty() {
            let percent = if candle.open.abs() > f64::EPSILON {
                (candle.close - candle.open) / candle.open * 100.0
            } else {
                0.0
            };
            let price = |value: f64| format!("{:.*}", self.style.price_decimals, value);
            format!(
                "O {}   H {}   L {}   C {}   {percent:+.2}%",
                price(candle.open),
                price(candle.high),
                price(candle.low),
                price(candle.close),
            )
        } else {
            candle.detail.clone()
        };
        let mut readout = Readout::of_mark(
            &candle.label,
            &detail,
            self.frame.inspection.pointer,
            self.frame.rect,
        );
        if self.style.show_crosshair {
            let price = self.price_rect();
            readout.mark(Highlight::Guide {
                x: self.center_x(index),
                y0: price.y0,
                y1: price.y1,
            });
        }
        Some(readout)
    }

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<usize> {
        let inside = self.price_rect().contains(point)
            || self.volume_rect().is_some_and(|rect| rect.contains(point));
        if !inside {
            return None;
        }
        let slot = self.slot_width();
        self.candles
            .iter()
            .enumerate()
            .filter(|(_, candle)| candle.valid())
            .min_by(|(left, _), (right, _)| {
                (self.center_x(*left) - point.x)
                    .abs()
                    .total_cmp(&(self.center_x(*right) - point.x).abs())
            })
            .and_then(|(index, _)| {
                ((self.center_x(index) - point.x).abs() <= slot / 2.0).then_some(index)
            })
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        Some(self.candles.get(index)?.id.clone())
    }
    fn keyboard_items(&self) -> Vec<usize> {
        let valid = self.candles.iter().map(Candle::valid);
        valid
            .enumerate()
            .filter_map(|(index, valid)| valid.then_some(index))
            .collect()
    }
    fn anchor(&self, &index: &usize) -> Option<Point> {
        let candle = self.candles.get(index)?;
        Some(Point::new(self.center_x(index), self.price_y(candle.close)))
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.candles
            .iter()
            .filter(|candle| candle.valid())
            .map(|candle| {
                PlotItem::new(
                    &candle.id,
                    &candle.label,
                    format!(
                        "Open {}, high {}, low {}, close {}. {}",
                        candle.open, candle.high, candle.low, candle.close, candle.detail
                    ),
                    candle.id.clone(),
                )
            })
            .collect()
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let widest = ValueAxis::widest(tr, theme, self.price_range(), |value, _| {
            self.price_label(value)
        });
        self.frame.margins = Margins::header(theme, &self.frame.title, 0)
            .value_axis(theme, widest)
            .labels_under(theme, 1);
        self.panel_gap = f64::from(theme.tokens.spacing.rhythm.related);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "candles, volume, axes and tooltip form one plot pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let price = self.price_rect();
        let (min, max) = self.price_range();
        self.follow_keyboard_anchor();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        ValueAxis {
            plot: price,
            range: (min, max),
            grid: self.style.show_grid,
            label: |value, _| self.price_label(value),
        }
        .paint(scene, transform, tr, theme);
        let candle_width =
            (self.slot_width() * self.style.candle_width_fraction.clamp(0.1, 0.95)).max(2.0);
        let max_volume = self
            .candles
            .iter()
            .map(|candle| candle.volume)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        for (index, candle) in self.candles.iter().enumerate() {
            if !candle.valid() {
                continue;
            }
            let x = self.center_x(index);
            let color = self.color(candle, theme);
            let open = self.price_y(candle.open);
            let close = self.price_y(candle.close);
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                color,
                None,
                &Line::new(
                    (x, self.price_y(candle.high)),
                    (x, self.price_y(candle.low)),
                ),
            );
            let body = Rect::new(
                x - candle_width / 2.0,
                open.min(close),
                x + candle_width / 2.0,
                open.max(close).max(open.min(close) + 1.5),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(body, 2.0),
            );
            if self.frame.inspection.hovered == Some(index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &RoundedRect::from_rect(body.inflate(3.0, 3.0), 4.0),
                );
            }
            if let Some(volume) = self.volume_rect() {
                let top = volume.y1 - candle.volume / max_volume * volume.height();
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(color.r(), color.g(), color.b(), 115),
                    None,
                    &Rect::new(
                        x - candle_width / 2.0,
                        top,
                        x + candle_width / 2.0,
                        volume.y1,
                    ),
                );
            }
        }
        for label in self.axis_labels(tr, theme) {
            let (width, _) = tr.measure_style(&label.text, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &label.text,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        label.rect.center().x - f64::from(width) / 2.0,
                        label.rect.y0,
                    )),
                None,
                &theme.text(TextRole::Caption),
            );
        }
    }
}

/// One period's name under the candles: the words as drawn and the box they fill.
#[derive(Clone, Debug, PartialEq)]
struct AxisLabel {
    text: String,
    rect: Rect,
}

impl CandlestickMarks {
    /// Where the x axis names its periods: thinned to every period whose own share of the
    /// width holds a name, and each name shortened at a word where its share is short, so
    /// two names never touch and none runs past the chart. A name with no readable short
    /// form left is left out, its period still named by the tooltip and the accessible name.
    fn axis_labels(&self, tr: &mut TextRenderer, theme: &Theme) -> Vec<AxisLabel> {
        use crate::widget::chart_labels::{label_stride, min_label_width, shortened_labels};
        let style = theme.text(TextRole::Caption);
        let slot = self.slot_width();
        let gap = f64::from(theme.tokens.spacing.s1);
        // The stride is the one at which the shortest readable name fits its share, so a chart
        // with many periods names some of them rather than losing the axis to collisions.
        let every = label_stride(min_label_width(tr, &style) + gap, slot);
        let room = slot * crate::num::f64_of_usize(every) - gap;
        let last = self.candles.len().saturating_sub(1);
        let mut labels = Vec::new();
        for (index, candle) in self.candles.iter().enumerate() {
            if !(last - index).is_multiple_of(every) || candle.label.trim().is_empty() {
                continue;
            }
            let (width, height) = tr.measure_style(&candle.label, None, &style);
            let shown = if f64::from(width) <= room {
                Some((candle.label.clone(), f64::from(width), f64::from(height)))
            } else {
                shortened_labels(tr, &candle.label, &style)
                    .into_iter()
                    .find(|(_, width, _)| *width <= room)
            };
            let Some((text, width, height)) = shown else {
                continue;
            };
            let x = self.center_x(index);
            let y = self.frame.inside().y1 + crate::widget::plot::step(theme);
            labels.push(AxisLabel {
                text,
                rect: Rect::new(x - width / 2.0, y, x + width / 2.0, y + height),
            });
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candlestick_rejects_impossible_ohlc_and_ranges_flat_data() {
        fn impossible_ohlc_is_rejected() {
            assert!(!Candle::new("x", "X", 2.0, 1.0, 3.0, 2.0).valid());
        }

        fn flat_data_gets_a_safe_price_range() {
            let chart = CandlestickChart::new(vec![Candle::new("x", "X", 2.0, 2.0, 2.0, 2.0)]);
            let (min, max) = chart.marks.price_range();
            assert!(max > min);
        }

        impossible_ohlc_is_rejected();
        flat_data_gets_a_safe_price_range();
    }

    /// Two weeks of prices, the last period named by the week it covers.
    fn two_weeks() -> Vec<Candle> {
        [
            ("Mon", 82.4, 88.1, 80.2, 86.9, 1240.0),
            ("Tue", 86.9, 91.4, 85.0, 90.2, 1580.0),
            ("Wed", 90.2, 90.8, 84.6, 85.3, 1360.0),
            ("Thu", 85.3, 89.7, 83.9, 89.1, 1120.0),
            ("Fri", 89.1, 93.5, 88.4, 92.8, 1710.0),
            ("Week of 12 October 2026", 92.8, 96.2, 90.1, 91.4, 6240.0),
        ]
        .into_iter()
        .map(|(label, open, high, low, close, volume)| {
            Candle::new(label, label, open, high, low, close).with_volume(volume)
        })
        .collect()
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
    }

    /// The last period's name is a whole sentence, and on a phone a chart cannot show it
    /// whole beside the day before it: neither name may print over the other, and both stay
    /// in the chart. The full name still reaches the tooltip and the accessible tree.
    #[test]
    fn a_period_name_never_touches_its_neighbour_and_stays_in_the_chart() {
        let week = "Week of 12 October 2026";
        for theme in [Theme::light(), Theme::dark()] {
            for (width, whole_name) in [(358.0, false), (1000.0, true)] {
                let mut chart =
                    CandlestickChart::new(two_weeks()).titled("Closing price, last two weeks");
                crate::testing::lay_out_in(&mut chart, width, 160.0, &theme)
                    .expect("the chart lays out");
                let labels = crate::text_render::with_layout_renderer(&theme, |tr| {
                    chart.marks.axis_labels(tr, &theme)
                });
                assert!(!labels.is_empty(), "the axis names its periods");
                let chart_rect = chart.marks.frame.rect;
                for (index, label) in labels.iter().enumerate() {
                    assert!(
                        label.rect.x0 >= chart_rect.x0 - 0.5
                            && label.rect.x1 <= chart_rect.x1 + 0.5
                            && label.rect.y0 >= chart_rect.y0 - 0.5
                            && label.rect.y1 <= chart_rect.y1 + 0.5,
                        "{label:?} stands outside {chart_rect:?}"
                    );
                    for other in labels.iter().skip(index + 1) {
                        assert!(
                            !overlaps(label.rect, other.rect),
                            "{label:?} prints over {other:?} at {width} px"
                        );
                    }
                }
                let shown: Vec<&str> = labels.iter().map(|label| label.text.as_str()).collect();
                assert_eq!(
                    labels.iter().any(|label| label.text == week),
                    whole_name,
                    "at {width} px the names read {shown:?}"
                );
                if !whole_name {
                    assert!(
                        shown
                            .iter()
                            .any(|text| text.starts_with("Week") && text.contains('\u{2026}')),
                        "at {width} px the week reads {shown:?}"
                    );
                }
            }
        }
    }
}
