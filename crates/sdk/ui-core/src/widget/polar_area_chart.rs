//! Interactive polar-area (rose / radial bar) chart.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded polar geometry uses floating point"
)]

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, ParamCurve, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

fn alpha(color: Color, value: u8) -> Color {
    Color::from_rgba8(color.r(), color.g(), color.b(), value)
}

#[derive(Clone, Debug)]
pub struct PolarAreaDatum {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl PolarAreaDatum {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent polar presentation toggles are composable"
)]
#[derive(Clone, Debug)]
pub struct PolarAreaStyle {
    pub max: Option<f64>,
    pub start_angle: f64,
    pub inner_radius: f64,
    pub angular_gap: f64,
    pub rings: u8,
    pub radius_exponent: f64,
    pub hover_offset: f64,
    pub show_labels: bool,
    pub show_values: bool,
    pub tooltip: bool,
}

impl Default for PolarAreaStyle {
    fn default() -> Self {
        Self {
            max: None,
            start_angle: -FRAC_PI_2,
            inner_radius: 0.16,
            angular_gap: 0.025,
            rings: 4,
            radius_exponent: 0.5,
            hover_offset: 6.0,
            show_labels: true,
            show_values: false,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug)]
struct Sector {
    id: String,
    start: f64,
    sweep: f64,
    inner: f64,
    outer: f64,
}

pub struct PolarAreaChart {
    data: Vec<PolarAreaDatum>,
    style: PolarAreaStyle,
    title: String,
    center_title: String,
    center_subtitle: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    sectors: Vec<Sector>,
    hovered: Option<String>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl PolarAreaChart {
    pub fn new(data: Vec<PolarAreaDatum>) -> Self {
        Self {
            data,
            style: PolarAreaStyle::default(),
            title: String::new(),
            center_title: String::new(),
            center_subtitle: String::new(),
            height: 320.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            sectors: Vec::new(),
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: PolarAreaStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: PolarAreaStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &PolarAreaStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut PolarAreaStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(140.0);
        self
    }
    pub fn set_data(&mut self, data: Vec<PolarAreaDatum>) {
        self.data = data;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self
            .data
            .iter()
            .any(|datum| datum.id == id && datum.value.is_finite() && datum.value >= 0.0)
            .then(|| id.to_owned());
        self.keyboard_inspection = self.hovered.is_some();
        self.update_inspection_anchor();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn update_inspection_anchor(&mut self) {
        let Some(id) = self.hovered.as_deref() else {
            return;
        };
        let Some(sector) = self.sectors.iter().find(|sector| sector.id == id) else {
            return;
        };
        let (center, _, _) = self.geometry();
        let angle = sector.start + sector.sweep / 2.0;
        let radius = sector.inner.midpoint(sector.outer);
        self.pointer = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
    }

    fn geometry(&self) -> (Point, f64, f64) {
        let top = if self.title.is_empty() { 12.0 } else { 40.0 };
        let area = Rect::new(
            self.rect.x0 + 14.0,
            self.rect.y0 + top,
            self.rect.x1 - 14.0,
            self.rect.y1 - 26.0,
        );
        let radius = (area.width().min(area.height()) / 2.0 - 18.0).max(1.0);
        (
            area.center(),
            radius * self.style.inner_radius.clamp(0.0, 0.92),
            radius,
        )
    }
    fn max(&self) -> f64 {
        self.style
            .max
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or_else(|| {
                self.data
                    .iter()
                    .map(|d| d.value)
                    .filter(|v| v.is_finite())
                    .fold(0.0_f64, f64::max)
                    .max(1.0)
            })
    }
    fn datum(&self, id: &str) -> Option<&PolarAreaDatum> {
        self.data.iter().find(|d| d.id == id)
    }
    fn color(datum: &PolarAreaDatum, index: usize, theme: &Theme) -> Color {
        datum
            .color
            .unwrap_or_else(|| theme.tokens.colors.presence.series(index))
    }
    fn hit(&self, point: Point) -> Option<&str> {
        let (center, _, _) = self.geometry();
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = dx.hypot(dy);
        let angle =
            (dy.atan2(dx) - self.style.start_angle).rem_euclid(TAU) + self.style.start_angle;
        self.sectors
            .iter()
            .find(|s| {
                angle >= s.start
                    && angle <= s.start + s.sweep
                    && radius >= s.inner
                    && radius <= s.outer + self.style.hover_offset
            })
            .map(|s| s.id.as_str())
    }
}

fn sector_path(
    center: Point,
    inner: f64,
    outer: f64,
    start: f64,
    sweep: f64,
) -> vello::kurbo::BezPath {
    let mut path = vello::kurbo::BezPath::new();
    let outer_arc = Arc::new(center, (outer, outer), start, sweep, 0.0);
    let inner_arc = Arc::new(center, (inner, inner), start + sweep, -sweep, 0.0);
    path.move_to(outer_arc.eval(0.0));
    outer_arc.to_cubic_beziers(0.1, |p1, p2, p3| path.curve_to(p1, p2, p3));
    path.line_to(inner_arc.eval(0.0));
    inner_arc.to_cubic_beziers(0.1, |p1, p2, p3| path.curve_to(p1, p2, p3));
    path.close_path();
    path
}

impl Widget for PolarAreaChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "rings, sectors, radial labels, center copy, and inspector share polar geometry"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
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
        let (center, inner, outer) = self.geometry();
        let max = self.max();
        let rings = self.style.rings.max(1);
        for ring in 1..=rings {
            let fraction = f64::from(ring) / f64::from(rings);
            let radius = fraction.mul_add(outer - inner, inner);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                alpha(theme.tokens.colors.text_dim, 45),
                None,
                &Circle::new(center, radius),
            );
        }
        let valid = self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .count();
        self.sectors.clear();
        if valid == 0 {
            return;
        }
        let count = f64::from(u32::try_from(valid).unwrap_or(u32::MAX));
        let slot = TAU / count;
        let gap = self.style.angular_gap.clamp(0.0, slot * 0.7);
        let exponent = self.style.radius_exponent.max(0.01);
        for (index, datum) in self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .enumerate()
        {
            let start = self.style.start_angle
                + f64::from(u32::try_from(index).unwrap_or(u32::MAX)) * slot
                + gap / 2.0;
            let sweep = slot - gap;
            let fraction = (datum.value / max).clamp(0.0, 1.0).powf(exponent);
            let mut radius = fraction.mul_add(outer - inner, inner);
            let hovered = self.hovered.as_deref() == Some(&datum.id);
            if hovered {
                radius += self.style.hover_offset;
            }
            let color = Self::color(datum, index, theme);
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &sector_path(center, inner, radius, start, sweep),
            );
            self.sectors.push(Sector {
                id: datum.id.clone(),
                start,
                sweep,
                inner,
                outer: radius,
            });
            if self.style.show_labels && radius - inner > 32.0 {
                let angle = start + sweep / 2.0;
                let label_r = inner + (radius - inner) * 0.68;
                let label = if self.style.show_values {
                    format!("{}  {}", datum.label, format_value(datum.value))
                } else {
                    datum.label.clone()
                };
                tr.draw_style(
                    scene,
                    &label,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((
                            angle.cos().mul_add(label_r, center.x) - 22.0,
                            angle.sin().mul_add(label_r, center.y) - 6.0,
                        )),
                    Some(58.0),
                    &theme.text(TextRole::Caption),
                );
            }
        }
        if !self.center_title.is_empty() {
            let (title_width, _) =
                tr.measure_style(&self.center_title, None, &theme.text(TextRole::Heading));
            tr.draw_style(
                scene,
                &self.center_title,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((center.x - f64::from(title_width) / 2.0, center.y - 15.0)),
                None,
                &theme.text(TextRole::Heading).with_weight(TextWeight::Bold),
            );
            tr.draw_style(
                scene,
                &self.center_subtitle,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((center.x - 30.0, center.y + 12.0)),
                Some(60.0),
                &theme.text(TextRole::Caption),
            );
        }
        if self.keyboard_inspection {
            self.update_inspection_anchor();
        }
        if let Some(datum) = self
            .hovered
            .as_deref()
            .and_then(|id| self.datum(id))
            .filter(|_| self.style.tooltip)
        {
            let detail = if datum.detail.is_empty() {
                format_value(datum.value)
            } else {
                format!("{} · {}", format_value(datum.value), datum.detail)
            };
            let (label_width, _) =
                tr.measure_style(&datum.label, None, &theme.text(TextRole::Caption));
            let (detail_width, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
            let width = f64::from(label_width.max(detail_width)).max(140.0) + 28.0;
            let x = (self.pointer.x + 16.0)
                .min(self.rect.x1 - width - 8.0)
                .max(self.rect.x0 + 8.0);
            let y = (self.pointer.y + 16.0)
                .min(self.rect.y1 - 72.0)
                .max(self.rect.y0 + 8.0);
            let tip = Rect::new(x, y, x + width, y + 64.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &vello::kurbo::RoundedRect::from_rect(tip, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &datum.label,
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
                transform * Affine::translate((x + 14.0, y + 37.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(action) = self.touch.handle(event, ctx.scale_factor) {
            match action {
                ChartTouchAction::Inspect(point) => {
                    self.pointer = point;
                    self.hovered = self.hit(point).map(str::to_owned);
                    self.keyboard_inspection = false;
                }
                ChartTouchAction::Activate(point) => {
                    self.pointer = point;
                    self.hovered = self.hit(point).map(str::to_owned);
                    if let Some(id) = &self.hovered {
                        self.activated = Some(id.clone());
                        *ctx.handled = true;
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
                ChartTouchAction::Clear => self.hovered = None,
            }
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.hovered = self.hit(self.pointer).map(str::to_owned);
            self.keyboard_inspection = false;
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) && !self.keyboard_inspection {
            self.hovered = None;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) && let Some(id) = self.hit(self.pointer).map(str::to_owned)
        {
            self.activated = Some(id);
            *ctx.handled = true;
        }
        if let Some(action) = chart_nav_action(event) {
            if action == ChartNavAction::Activate {
                self.activated.clone_from(&self.hovered);
            } else {
                let candidates = self
                    .data
                    .iter()
                    .filter(|datum| datum.value.is_finite() && datum.value >= 0.0)
                    .map(|datum| datum.id.as_str())
                    .collect::<Vec<_>>();
                let current = self
                    .hovered
                    .as_deref()
                    .and_then(|id| candidates.iter().position(|candidate| *candidate == id));
                self.hovered = moved_index(current, candidates.len(), action)
                    .and_then(|index| candidates.get(index))
                    .map(|id| (*id).to_owned());
                self.keyboard_inspection = self.hovered.is_some();
                self.update_inspection_anchor();
            }
            *ctx.handled = true;
            *ctx.focus_target = Some(ctx.node_id);
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Group).with_name(if self.title.is_empty() {
            "Polar area chart"
        } else {
            &self.title
        });
        root.children = self
            .data
            .iter()
            .filter(|d| d.value.is_finite() && d.value >= 0.0)
            .map(|d| {
                AccessNode::new(Role::Button)
                    .with_action_target(&d.id)
                    .with_name(&d.label)
                    .with_description(if d.detail.is_empty() {
                        format_value(d.value)
                    } else {
                        d.detail.clone()
                    })
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(root)
    }
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        keys: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = keys.last() else {
            return false;
        };
        if let Some(datum) = self
            .data
            .iter()
            .find(|d| crate::a11y::hash_key(&d.id) == *key)
        {
            self.activated = Some(datum.id.clone());
            true
        } else {
            false
        }
    }
    fn name(&self) -> &'static str {
        "PolarAreaChart"
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

fn format_value(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.fract().abs() < 0.001 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_max_requires_positive_finite_value() {
        let mut chart = PolarAreaChart::new(vec![PolarAreaDatum::new("a", "A", 7.0)]);
        chart.style.max = Some(-2.0);
        assert_eq!(chart.max(), 7.0);
        chart.style.max = Some(10.0);
        assert_eq!(chart.max(), 10.0);
    }
}
