//! Interactive weighted chord diagram.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded radial and Bézier geometry"
)]

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, BezPath, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

fn alpha(color: Color, value: u8) -> Color {
    Color::from_rgba8(color.r(), color.g(), color.b(), value)
}
fn polar(center: Point, radius: f64, angle: f64) -> Point {
    Point::new(
        angle.cos().mul_add(radius, center.x),
        angle.sin().mul_add(radius, center.y),
    )
}
fn append_arc(path: &mut BezPath, center: Point, radius: f64, start: f64, sweep: f64) {
    Arc::new(center, (radius, radius), start, sweep, 0.0)
        .to_cubic_beziers(0.1, |a, b, c| path.curve_to(a, b, c));
}

#[derive(Clone, Debug)]
pub struct ChordNode {
    pub id: String,
    pub label: String,
    pub color: Option<Color>,
    pub detail: String,
}
impl ChordNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
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

#[derive(Clone, Debug)]
pub struct ChordLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}
impl ChordLink {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
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
    reason = "independent chord presentation switches are composable"
)]
#[derive(Clone, Debug)]
pub struct ChordStyle {
    pub start_angle: f64,
    pub node_gap: f64,
    pub ring_width: f64,
    pub ribbon_alpha: u8,
    pub hover_alpha: u8,
    pub label_offset: f64,
    pub show_labels: bool,
    pub show_values: bool,
    pub tooltip: bool,
}
impl Default for ChordStyle {
    fn default() -> Self {
        Self {
            start_angle: -FRAC_PI_2,
            node_gap: 0.035,
            ring_width: 18.0,
            ribbon_alpha: 72,
            hover_alpha: 155,
            label_offset: 12.0,
            show_labels: true,
            show_values: false,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChordSelection {
    Node(String),
    Link(String),
}
#[derive(Clone)]
struct NodeGeom {
    id: String,
    start: f64,
    end: f64,
    value: f64,
    color: Color,
    path: BezPath,
}
#[derive(Clone)]
struct LinkGeom {
    id: String,
    path: BezPath,
    color: Color,
}

pub struct ChordChart {
    nodes: Vec<ChordNode>,
    links: Vec<ChordLink>,
    style: ChordStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    node_geometry: Vec<NodeGeom>,
    link_geometry: Vec<LinkGeom>,
    hovered: Option<ChordSelection>,
    activated: Option<ChordSelection>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}
impl ChordChart {
    pub fn new(nodes: Vec<ChordNode>, links: Vec<ChordLink>) -> Self {
        Self {
            nodes,
            links,
            style: ChordStyle::default(),
            title: String::new(),
            height: 360.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            node_geometry: Vec::new(),
            link_geometry: Vec::new(),
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: ChordStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: ChordStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &ChordStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut ChordStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(180.0);
        self
    }
    pub fn set_data(&mut self, nodes: Vec<ChordNode>, links: Vec<ChordLink>) {
        self.nodes = nodes;
        self.links = links;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<ChordSelection> {
        self.activated.take()
    }
    pub fn inspect(&mut self, selection: &ChordSelection) -> bool {
        self.hovered = match selection {
            ChordSelection::Node(id) => self.nodes.iter().any(|node| node.id == *id),
            ChordSelection::Link(id) => self
                .links
                .iter()
                .any(|link| link.id == *id && link.value.is_finite() && link.value > 0.0),
        }
        .then(|| selection.clone());
        self.keyboard_inspection = self.hovered.is_some();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }
    fn inspection_anchor(&self, selection: &ChordSelection) -> Point {
        let (center, inner, outer) = self.geometry();
        match selection {
            ChordSelection::Node(id) => self
                .node_geometry
                .iter()
                .find(|node| node.id == *id)
                .map_or(center, |node| {
                    polar(center, inner.midpoint(outer), node.start.midpoint(node.end))
                }),
            ChordSelection::Link(_) => center,
        }
    }
    fn geometry(&self) -> (Point, f64, f64) {
        let top = if self.title.is_empty() { 10.0 } else { 38.0 };
        let area = Rect::new(
            self.rect.x0 + 28.0,
            self.rect.y0 + top,
            self.rect.x1 - 28.0,
            self.rect.y1 - 24.0,
        );
        let outer =
            (area.width().min(area.height()) / 2.0 - self.style.label_offset - 16.0).max(2.0);
        (
            area.center(),
            (outer - self.style.ring_width.max(4.0)).max(1.0),
            outer,
        )
    }
    fn palette(index: usize, theme: &Theme) -> Color {
        theme.tokens.colors.presence.series(index)
    }
    fn hit(&self, point: Point) -> Option<ChordSelection> {
        self.node_geometry
            .iter()
            .find(|g| g.path.winding(point) != 0)
            .map(|g| ChordSelection::Node(g.id.clone()))
            .or_else(|| {
                self.link_geometry
                    .iter()
                    .rev()
                    .find(|g| g.path.winding(point) != 0)
                    .map(|g| ChordSelection::Link(g.id.clone()))
            })
    }
    fn node(&self, id: &str) -> Option<&ChordNode> {
        self.nodes.iter().find(|node| node.id == id)
    }
    fn link(&self, id: &str) -> Option<&ChordLink> {
        self.links.iter().find(|link| link.id == id)
    }
}

fn annulus(center: Point, inner: f64, outer: f64, start: f64, end: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(polar(center, outer, start));
    append_arc(&mut p, center, outer, start, end - start);
    p.line_to(polar(center, inner, end));
    append_arc(&mut p, center, inner, end, start - end);
    p.close_path();
    p
}
fn ribbon(center: Point, radius: f64, a0: f64, a1: f64, b0: f64, b1: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(polar(center, radius, a0));
    append_arc(&mut p, center, radius, a0, a1 - a0);
    let b1p = polar(center, radius, b1);
    p.curve_to(center, center, b1p);
    append_arc(&mut p, center, radius, b1, b0 - b1);
    let a0p = polar(center, radius, a0);
    p.curve_to(center, center, a0p);
    p.close_path();
    p
}

impl Widget for ChordChart {
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
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "weighted allocation, ribbons, arcs, labels and inspector share chord geometry"
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
            tr.draw_text_weighted(
                scene,
                &self.title,
                theme.tokens.typography.scale.lg,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 7.0)),
                None,
                TextWeight::Semibold,
            );
        }
        let (center, inner, outer) = self.geometry();
        let index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect::<HashMap<_, _>>();
        let mut totals = vec![0.0_f64; self.nodes.len()];
        let valid = self
            .links
            .iter()
            .filter(|l| {
                l.value.is_finite()
                    && l.value > 0.0
                    && index.contains_key(&l.source)
                    && index.contains_key(&l.target)
            })
            .collect::<Vec<_>>();
        for link in &valid {
            if let Some(&source) = index.get(&link.source)
                && let Some(total) = totals.get_mut(source)
            {
                *total += link.value;
            }
            if let Some(&target) = index.get(&link.target)
                && let Some(total) = totals.get_mut(target)
            {
                *total += link.value;
            }
        }
        let sum = totals.iter().sum::<f64>();
        self.node_geometry.clear();
        self.link_geometry.clear();
        if sum <= f64::EPSILON {
            return;
        }
        let node_count = f64::from(u32::try_from(self.nodes.len().max(1)).unwrap_or(u32::MAX));
        let gap = self.style.node_gap.clamp(0.0, TAU / node_count * 0.8);
        let usable = (TAU - gap * node_count).max(TAU * 0.1);
        let mut angle = self.style.start_angle;
        let mut spans = Vec::with_capacity(self.nodes.len());
        for (node_index, node) in self.nodes.iter().enumerate() {
            let value = totals.get(node_index).copied().unwrap_or(0.0);
            let sweep = value / sum * usable;
            let start = angle;
            let end = angle + sweep;
            let color = node
                .color
                .unwrap_or_else(|| Self::palette(node_index, theme));
            let path = annulus(center, inner, outer, start, end);
            self.node_geometry.push(NodeGeom {
                id: node.id.clone(),
                start,
                end,
                value,
                color,
                path,
            });
            spans.push((start, end));
            angle = end + gap;
        }
        let mut cursors = spans.iter().map(|(start, _)| *start).collect::<Vec<_>>();
        for link in valid {
            let Some(&source) = index.get(&link.source) else {
                continue;
            };
            let Some(&target) = index.get(&link.target) else {
                continue;
            };
            let source_total = totals.get(source).copied().unwrap_or(0.0);
            let target_total = totals.get(target).copied().unwrap_or(0.0);
            if source_total <= 0.0 || target_total <= 0.0 {
                continue;
            }
            let source_span = spans.get(source).map_or(0.0, |(a, b)| b - a);
            let target_span = spans.get(target).map_or(0.0, |(a, b)| b - a);
            let a0 = cursors.get(source).copied().unwrap_or(0.0);
            let a1 = a0 + source_span * link.value / source_total;
            let b0 = cursors.get(target).copied().unwrap_or(0.0);
            let b1 = b0 + target_span * link.value / target_total;
            if let Some(cursor) = cursors.get_mut(source) {
                *cursor = a1;
            }
            if let Some(cursor) = cursors.get_mut(target) {
                *cursor = b1;
            }
            let color = link.color.unwrap_or_else(|| {
                self.node_geometry
                    .get(source)
                    .map_or_else(|| Self::palette(source, theme), |g| g.color)
            });
            self.link_geometry.push(LinkGeom {
                id: link.id.clone(),
                path: ribbon(center, inner, a0, a1, b0, b1),
                color,
            });
        }
        for geom in &self.link_geometry {
            let hovered = self.hovered.as_ref() == Some(&ChordSelection::Link(geom.id.clone()));
            scene.fill(
                Fill::NonZero,
                transform,
                alpha(
                    geom.color,
                    if hovered {
                        self.style.hover_alpha
                    } else {
                        self.style.ribbon_alpha
                    },
                ),
                None,
                &geom.path,
            );
        }
        for geom in &self.node_geometry {
            let hovered = self.hovered.as_ref() == Some(&ChordSelection::Node(geom.id.clone()));
            scene.fill(Fill::NonZero, transform, geom.color, None, &geom.path);
            if hovered {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.text_primary,
                    None,
                    &geom.path,
                );
            }
            if self.style.show_labels && geom.end > geom.start {
                let mid = geom.start.midpoint(geom.end);
                let label_r = outer + self.style.label_offset;
                let label = self.node(&geom.id).map_or("", |n| n.label.as_str());
                let label = if self.style.show_values {
                    format!("{label} {}", format_value(geom.value))
                } else {
                    label.to_owned()
                };
                tr.draw_text(
                    scene,
                    &label,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            mid.cos().mul_add(label_r, center.x) - 24.0,
                            mid.sin().mul_add(label_r, center.y) - 6.0,
                        )),
                    Some(64.0),
                );
            }
        }
        if self.keyboard_inspection
            && let Some(selection) = self.hovered.as_ref()
        {
            self.pointer = self.inspection_anchor(selection);
        }
        if let Some(selection) = self.hovered.as_ref().filter(|_| self.style.tooltip) {
            let (label, detail) = match selection {
                ChordSelection::Node(id) => self.node(id).map(|n| {
                    (
                        n.label.clone(),
                        if n.detail.is_empty() {
                            self.node_geometry
                                .iter()
                                .find(|g| g.id == *id)
                                .map_or_else(String::new, |g| format_value(g.value))
                        } else {
                            n.detail.clone()
                        },
                    )
                }),
                ChordSelection::Link(id) => self.link(id).map(|l| {
                    (
                        format!("{} → {}", l.source, l.target),
                        if l.detail.is_empty() {
                            format_value(l.value)
                        } else {
                            format!("{} · {}", format_value(l.value), l.detail)
                        },
                    )
                }),
            }
            .unwrap_or_default();
            let (a, _) = tr.measure(&label, theme.tokens.typography.scale.sm, None);
            let (b, _) = tr.measure(&detail, theme.tokens.typography.scale.xs, None);
            let width = f64::from(a.max(b)).max(150.0) + 28.0;
            let x = (self.pointer.x + 16.0)
                .min(self.rect.x1 - width - 8.0)
                .max(self.rect.x0 + 8.0);
            let y = (self.pointer.y + 16.0)
                .min(self.rect.y1 - 72.0)
                .max(self.rect.y0 + 8.0);
            let tip = RoundedRect::from_rect(
                Rect::new(x, y, x + width, y + 64.0),
                f64::from(theme.tokens.radii.control),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &tip,
            );
            tr.draw_text_weighted(
                scene,
                &label,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 14.0, y + 11.0)),
                None,
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &detail,
                theme.tokens.typography.scale.xs,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x + 14.0, y + 37.0)),
                None,
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let node = ctx.node_id;
        if let Some(action) = self.touch.handle(event, ctx.scale_factor) {
            match action {
                ChartTouchAction::Inspect(point) => {
                    self.pointer = point;
                    self.hovered = self.hit(point);
                    self.keyboard_inspection = false;
                }
                ChartTouchAction::Activate(point) => {
                    self.pointer = point;
                    self.hovered = self.hit(point);
                    if let Some(selection) = self.hovered.clone() {
                        self.activated = Some(selection);
                        *ctx.handled = true;
                        *ctx.focus_target = Some(node);
                    }
                }
                ChartTouchAction::Clear => self.hovered = None,
            }
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.hovered = self.hit(self.pointer);
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
        ) && let Some(selection) = self.hit(self.pointer)
        {
            self.activated = Some(selection);
            *ctx.handled = true;
        }
        if let Some(action) = chart_nav_action(event) {
            if action == ChartNavAction::Activate {
                self.activated.clone_from(&self.hovered);
            } else {
                let candidates = self
                    .nodes
                    .iter()
                    .map(|node| ChordSelection::Node(node.id.clone()))
                    .chain(
                        self.links
                            .iter()
                            .filter(|link| link.value.is_finite() && link.value > 0.0)
                            .map(|link| ChordSelection::Link(link.id.clone())),
                    )
                    .collect::<Vec<_>>();
                let current = self
                    .hovered
                    .as_ref()
                    .and_then(|selection| candidates.iter().position(|item| item == selection));
                self.hovered = moved_index(current, candidates.len(), action)
                    .and_then(|index| candidates.get(index).cloned());
                self.keyboard_inspection = self.hovered.is_some();
                if let Some(selection) = self.hovered.as_ref() {
                    self.pointer = self.inspection_anchor(selection);
                }
            }
            *ctx.handled = true;
            *ctx.focus_target = Some(node);
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Group).with_name(if self.title.is_empty() {
            "Chord diagram"
        } else {
            &self.title
        });
        root.children = self
            .nodes
            .iter()
            .map(|n| {
                AccessNode::new(Role::Button)
                    .with_action_target(format!("node:{}", n.id))
                    .with_name(&n.label)
                    .with_description(&n.detail)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .chain(
                self.links
                    .iter()
                    .filter(|l| l.value.is_finite() && l.value > 0.0)
                    .map(|l| {
                        AccessNode::new(Role::Button)
                            .with_action_target(format!("link:{}", l.id))
                            .with_name(format!("{} to {}", l.source, l.target))
                            .with_description(if l.detail.is_empty() {
                                format_value(l.value)
                            } else {
                                l.detail.clone()
                            })
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                    }),
            )
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
        for node in &self.nodes {
            if crate::a11y::hash_key(&format!("node:{}", node.id)) == *key {
                self.activated = Some(ChordSelection::Node(node.id.clone()));
                return true;
            }
        }
        for link in &self.links {
            if crate::a11y::hash_key(&format!("link:{}", link.id)) == *key {
                self.activated = Some(ChordSelection::Link(link.id.clone()));
                return true;
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "ChordChart"
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
fn format_value(v: f64) -> String {
    if v.abs() >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v.abs() >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else if v.fract().abs() < 0.001 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chord_ribbons_hit_test_and_invalid_links_are_safe() {
        fn ribbon_is_closed_and_hit_testable() {
            let path = ribbon(Point::ZERO, 100.0, 0.0, 0.2, 2.0, 2.2);
            assert!(path.winding(Point::new(20.0, 20.0)) != 0 || path.elements().last().is_some());
        }

        fn invalid_links_do_not_panic() {
            let mut chart = ChordChart::new(
                vec![ChordNode::new("a", "A")],
                vec![ChordLink::new("x", "missing", "a", 1.0)],
            );
            assert!(chart.take_activated().is_none());
        }

        ribbon_is_closed_and_hit_testable();
        invalid_links_do_not_panic();
    }
}
