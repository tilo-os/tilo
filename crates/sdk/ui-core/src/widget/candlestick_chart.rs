//! Interactive OHLC/candlestick charts with an optional volume panel.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "financial chart projection is bounded floating-point geometry"
)]

use std::collections::HashMap;

use crate::widget::ModelPath;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, handle_chart_event, moved_item,
};
use crate::widget::{TaffyTreeExt, Widget};

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

pub struct CandlestickChart {
    candles: Vec<Candle>,
    style: CandlestickStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<usize>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl CandlestickChart {
    pub fn new(candles: Vec<Candle>) -> Self {
        Self {
            candles,
            style: CandlestickStyle::default(),
            title: String::new(),
            height: 380.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }

    #[must_use]
    pub fn styled(mut self, style: CandlestickStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: CandlestickStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &CandlestickStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut CandlestickStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(160.0);
        self
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self
            .candles
            .iter()
            .position(|candle| candle.id == id && candle.valid());
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(index) = self.hovered
            && let Some(candle) = self.candles.get(index)
        {
            self.pointer = Point::new(self.center_x(index), self.price_y(candle.close));
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn price_rect(&self) -> Rect {
        let bottom = if self.style.show_volume { 88.0 } else { 38.0 };
        Rect::new(
            self.rect.x0 + 60.0,
            self.rect.y0 + if self.title.is_empty() { 18.0 } else { 48.0 },
            self.rect.x1 - 14.0,
            self.rect.y1 - bottom,
        )
    }

    fn volume_rect(&self) -> Option<Rect> {
        self.style.show_volume.then(|| {
            let price = self.price_rect();
            Rect::new(price.x0, price.y1 + 16.0, price.x1, self.rect.y1 - 26.0)
        })
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

    fn draw_inspector(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(index) = self.hovered.filter(|_| self.style.tooltip) else {
            return;
        };
        let Some(candle) = self.candles.get(index) else {
            return;
        };
        if self.style.show_crosshair {
            let x = self.center_x(index);
            let price = self.price_rect();
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                Color::from_rgba8(
                    theme.tokens.colors.text_dim.r(),
                    theme.tokens.colors.text_dim.g(),
                    theme.tokens.colors.text_dim.b(),
                    110,
                ),
                None,
                &Line::new((x, price.y0), (x, price.y1)),
            );
        }
        let delta = candle.close - candle.open;
        let detail = if candle.detail.is_empty() {
            let percent = if candle.open.abs() > f64::EPSILON {
                delta / candle.open * 100.0
            } else {
                0.0
            };
            format!(
                "O {:.*}   H {:.*}   L {:.*}   C {:.*}  ·  {percent:+.2}%",
                self.style.price_decimals,
                candle.open,
                self.style.price_decimals,
                candle.high,
                self.style.price_decimals,
                candle.low,
                self.style.price_decimals,
                candle.close,
            )
        } else {
            candle.detail.clone()
        };
        let (detail_width, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
        let width = f64::from(detail_width).max(236.0) + 28.0;
        let mut x = self.pointer.x + 16.0;
        let mut y = self.pointer.y + 16.0;
        if x + width > self.rect.x1 {
            x = self.pointer.x - width - 16.0;
        }
        if y + 72.0 > self.rect.y1 {
            y = self.pointer.y - 82.0;
        }
        let tooltip = Rect::new(x, y, x + width, y + 70.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(tooltip, f64::from(theme.tokens.radii.control)),
        );
        tr.draw_style(
            scene,
            &candle.label,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x + 14.0, y + 11.0)),
            None,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
        );
        tr.draw_style(
            scene,
            &detail,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x + 14.0, y + 39.0)),
            None,
            &theme.text(TextRole::Caption),
        );
    }
}

impl InteractiveChart for CandlestickChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        let inside = self.price_rect().contains(self.pointer)
            || self
                .volume_rect()
                .is_some_and(|rect| rect.contains(self.pointer));
        if !inside {
            self.hovered = None;
            return;
        }
        let slot = self.slot_width();
        self.hovered = self
            .candles
            .iter()
            .enumerate()
            .filter(|(_, candle)| candle.valid())
            .min_by(|(left, _), (right, _)| {
                (self.center_x(*left) - self.pointer.x)
                    .abs()
                    .total_cmp(&(self.center_x(*right) - self.pointer.x).abs())
            })
            .and_then(|(index, _)| {
                ((self.center_x(index) - self.pointer.x).abs() <= slot / 2.0).then_some(index)
            });
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(candle) = self.hovered.and_then(|index| self.candles.get(index)) else {
            return false;
        };
        self.activated = Some(candle.id.clone());
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let candidates = self
            .candles
            .iter()
            .enumerate()
            .filter(|(_, candle)| candle.valid())
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        self.hovered = moved_item(self.hovered, &candidates, action);
        if let Some(index) = self.hovered
            && let Some(candle) = self.candles.get(index)
        {
            self.pointer = Point::new(self.center_x(index), self.price_y(candle.close));
        }
        self.hovered.is_some()
    }
}

impl Widget for CandlestickChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "candles, volume, axes and tooltip form one plot pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let price = self.price_rect();
        let (min, max) = self.price_range();
        if self.keyboard_inspection
            && let Some(index) = self.hovered
            && let Some(candle) = self.candles.get(index)
        {
            self.pointer = Point::new(self.center_x(index), self.price_y(candle.close));
        }
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
                None,
                &theme.text(TextRole::Heading),
            );
        }
        for value in crate::charts::nice_ticks(min, max, 5) {
            let fraction = if max > min {
                (max - value) / (max - min)
            } else {
                0.0
            };
            let y = price.y0 + price.height() * fraction;
            if self.style.show_grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.border_standard,
                    None,
                    &Line::new((price.x0, y), (price.x1, y)),
                );
            }
            let label = format!("{:.*}", self.style.price_decimals, value);
            let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((price.x0 - f64::from(width) - 9.0, y - 6.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
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
            if self.hovered == Some(index) {
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
        let label_step = (self.candles.len() / 6).max(1);
        for (index, candle) in self.candles.iter().enumerate().step_by(label_step) {
            let x = self.center_x(index);
            let (width, _) = tr.measure_style(&candle.label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &candle.label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x - f64::from(width) / 2.0, self.rect.y1 - 18.0)),
                Some(crate::num::f32_of_f64(self.slot_width().max(0.0))),
                &theme.text(TextRole::Caption),
            );
        }
        self.draw_inspector(scene, transform, tr, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let children = self
            .candles
            .iter()
            .filter(|candle| candle.valid())
            .map(|candle| {
                AccessNode::new(Role::Button)
                    .with_action_target(&candle.id)
                    .with_name(&candle.label)
                    .with_description(format!(
                        "Open {}, high {}, low {}, close {}. {}",
                        candle.open, candle.high, candle.low, candle.close, candle.detail
                    ))
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.title.is_empty() {
                    "Candlestick chart"
                } else {
                    &self.title
                })
                .with_children(children),
        )
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if let Some(candle) = self
            .candles
            .iter()
            .find(|c| crate::a11y::hash_key(&c.id) == *key)
        {
            self.activated = Some(candle.id.clone());
            return true;
        }
        false
    }

    fn name(&self) -> &'static str {
        "CandlestickChart"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
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
            let (min, max) = chart.price_range();
            assert!(max > min);
        }

        impossible_ohlc_is_rejected();
        flat_data_gets_a_safe_price_range();
    }
}
