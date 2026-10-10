//! Interactive weighted chord diagram.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded radial and Bézier geometry"
)]

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use vello::Scene;
use vello::kurbo::{Affine, Arc, BezPath, Point, Rect, Shape, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::paint::with_alpha;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, chart_leaf, handle_chart_event,
    moved_index,
};

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
    /// The room the arc labels need beside the ring, measured at paint.
    label_room: f64,
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
            label_room: 0.0,
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
            self.rect.x0 + 12.0,
            self.rect.y0 + top,
            self.rect.x1 - 12.0,
            self.rect.y1 - 12.0,
        );
        // The ring leaves the labels their measured room at the sides and a
        // line above and below.
        let side = self.label_room.max(self.style.label_offset + 16.0);
        let outer = (area.width() / 2.0 - side)
            .min(area.height() / 2.0 - self.style.label_offset - 16.0)
            .max(2.0);
        (
            area.center(),
            (outer - self.style.ring_width.max(4.0)).max(1.0),
            outer,
        )
    }
    fn palette(index: usize, theme: &Theme) -> Color {
        theme.tokens.colors.presence.series(index)
    }

    /// The room the labels need beside the ring: the widest label, never
    /// more than a quarter of the chart, so the ring keeps its size.
    fn measure_label_room(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        if !self.style.show_labels {
            self.label_room = 0.0;
            return;
        }
        let style = theme.text(TextRole::Caption);
        let widest = self
            .nodes
            .iter()
            .map(|node| f64::from(tr.measure_style(&node.label, None, &style).0))
            .fold(0.0, f64::max);
        let pad = f64::from(theme.tokens.spacing.s2);
        self.label_room = (widest + 2.0 * pad).min(self.rect.width() / 4.0);
    }

    /// Each arc's label outside its arc, largest arc first: shortened to the
    /// room left before the chart's edge, and dropped (the tooltip names
    /// the arc) where it would collide with a label already placed.
    fn label_layout(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        center: Point,
        outer: f64,
    ) -> Vec<(String, Rect)> {
        let style = theme.text(TextRole::Caption);
        let mut placer = crate::widget::chart_labels::LabelPlacer::new(self.rect, theme);
        let radius = outer
            + self
                .style
                .label_offset
                .min(f64::from(theme.tokens.spacing.s2));
        let mut order: Vec<&NodeGeom> = self
            .node_geometry
            .iter()
            .filter(|geom| geom.end > geom.start)
            .collect();
        order.sort_by(|a, b| (b.end - b.start).total_cmp(&(a.end - a.start)));
        let mut placed = Vec::new();
        for geom in order {
            let label = self.node(&geom.id).map_or("", |n| n.label.as_str());
            let label = if self.style.show_values {
                format!("{label} {}", format_compact(geom.value))
            } else {
                label.to_owned()
            };
            let mid = geom.start.midpoint(geom.end);
            let room = crate::widget::chart_labels::radial_room(center, radius, mid, self.rect);
            let Some((shown, w, h)) =
                crate::widget::chart_labels::fit_label(tr, &label, room, &style)
            else {
                continue;
            };
            let rect = crate::widget::chart_labels::radial_label_rect(center, radius, mid, (w, h));
            if placer.place(rect) {
                placed.push((shown, rect));
            }
        }
        placed
    }

    fn draw_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        center: Point,
        outer: f64,
    ) {
        let style = theme.text(TextRole::Caption);
        for (label, rect) in self.label_layout(tr, theme, center, outer) {
            tr.draw_style(
                scene,
                &label,
                crate::widget::chart_labels::ground_ink(theme),
                transform * Affine::translate((rect.x0, rect.y0)),
                None,
                &style,
            );
        }
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

impl InteractiveChart for ChordChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self.hit(self.pointer);
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(selection) = self.hovered.clone() else {
            return false;
        };
        self.activated = Some(selection);
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
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
        if let Some(selection) = self.hovered.as_ref() {
            self.pointer = self.inspection_anchor(selection);
        }
        self.hovered.is_some()
    }
}

impl Widget for ChordChart {
    chart_leaf!();
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
        self.measure_label_room(tr, theme);
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 7.0)),
                None,
                &theme.text(TextRole::Heading),
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
                with_alpha(
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
        }
        if self.style.show_labels {
            self.draw_labels(scene, transform, tr, theme, center, outer);
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
                                .map_or_else(String::new, |g| format_compact(g.value))
                        } else {
                            n.detail.clone()
                        },
                    )
                }),
                ChordSelection::Link(id) => self.link(id).map(|l| {
                    (
                        format!("{} → {}", l.source, l.target),
                        if l.detail.is_empty() {
                            format_compact(l.value)
                        } else {
                            format!("{} · {}", format_compact(l.value), l.detail)
                        },
                    )
                }),
            }
            .unwrap_or_default();
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.pointer,
                self.rect,
                150.0,
                (&label, &detail),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
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
                                format_compact(l.value)
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
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(key) = crate::widget::chart_navigation::activated_key(action, keys) else {
            return false;
        };
        for node in &self.nodes {
            if crate::a11y::hash_key(&format!("node:{}", node.id)) == key {
                self.activated = Some(ChordSelection::Node(node.id.clone()));
                return true;
            }
        }
        for link in &self.links {
            if crate::a11y::hash_key(&format!("link:{}", link.id)) == key {
                self.activated = Some(ChordSelection::Link(link.id.clone()));
                return true;
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "ChordChart"
    }
    fn is_focusable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At every size the arc labels stay outside the ring, inside the
    /// chart, and clear of each other.
    #[test]
    fn arc_labels_stay_off_the_ring_and_off_each_other() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        for (w, h) in [(220.0, 200.0), (320.0, 260.0), (520.0, 360.0)] {
            let mut chart = ChordChart::new(
                ["Design", "Engineering", "Research", "Support", "Sales"]
                    .into_iter()
                    .map(|label| ChordNode::new(label, label))
                    .collect(),
                vec![
                    ChordLink::new("d-e", "Design", "Engineering", 32.0),
                    ChordLink::new("d-r", "Design", "Research", 14.0),
                    ChordLink::new("e-r", "Engineering", "Research", 25.0),
                    ChordLink::new("e-s", "Engineering", "Support", 18.0),
                    ChordLink::new("s-s", "Support", "Sales", 27.0),
                    ChordLink::new("s-d", "Sales", "Design", 21.0),
                ],
            );
            chart.rect = Rect::new(0.0, 0.0, w, h);
            let mut scene = Scene::new();
            chart.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let (center, _, outer) = chart.geometry();
            let labels = chart.label_layout(&mut tr, &theme, center, outer);
            assert!(!labels.is_empty(), "{w}: some arcs are named");
            for (i, (_, a)) in labels.iter().enumerate() {
                let nearest = Point::new(center.x.clamp(a.x0, a.x1), center.y.clamp(a.y0, a.y1));
                assert!(
                    nearest.distance(center) >= outer,
                    "{w}: {a:?} covers the ring"
                );
                assert!(
                    a.x0 >= chart.rect.x0 - 0.5 && a.x1 <= chart.rect.x1 + 0.5,
                    "{w}: {a:?} leaves the chart"
                );
                for (_, b) in labels.iter().skip(i + 1) {
                    assert!(a.intersect(*b).is_zero_area(), "{w}: {a:?} hits {b:?}");
                }
            }
        }
    }
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
