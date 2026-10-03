//! Automatic-layout interactive Sankey flow diagrams.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "flow layout and Bézier ribbon geometry are bounded floating-point operations"
)]

use std::collections::HashMap;

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
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

#[derive(Clone, Debug)]
pub struct SankeyNode {
    pub id: String,
    pub label: String,
    pub color: Option<Color>,
    pub detail: String,
}

impl SankeyNode {
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
pub struct SankeyLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl SankeyLink {
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

#[derive(Clone, Debug)]
pub struct SankeyStyle {
    pub node_width: f64,
    pub node_gap: f64,
    pub node_radius: f64,
    pub link_alpha: u8,
    pub link_curvature: f64,
    pub minimum_node_height: f64,
    pub show_values: bool,
    pub tooltip: bool,
}

impl Default for SankeyStyle {
    fn default() -> Self {
        Self {
            node_width: 18.0,
            node_gap: 14.0,
            node_radius: 4.0,
            link_alpha: 105,
            link_curvature: 0.52,
            minimum_node_height: 5.0,
            show_values: true,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SankeySelection {
    Node(String),
    Link(String),
}

#[derive(Clone, Copy, Debug)]
struct NodeBox {
    index: usize,
    rect: Rect,
    value: f64,
}

#[derive(Clone, Debug)]
struct LinkBand {
    index: usize,
    path: BezPath,
    source: Point,
    target: Point,
    thickness: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Node(usize),
    Link(usize),
}

pub struct SankeyChart {
    nodes: Vec<SankeyNode>,
    links: Vec<SankeyLink>,
    style: SankeyStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<Hit>,
    activated: Option<SankeySelection>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl SankeyChart {
    pub fn new(nodes: Vec<SankeyNode>, links: Vec<SankeyLink>) -> Self {
        Self {
            nodes,
            links,
            style: SankeyStyle::default(),
            title: String::new(),
            height: 420.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: SankeyStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: SankeyStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &SankeyStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut SankeyStyle {
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
    pub fn set_data(&mut self, nodes: Vec<SankeyNode>, links: Vec<SankeyLink>) {
        self.nodes = nodes;
        self.links = links;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<SankeySelection> {
        self.activated.take()
    }
    fn selection_for(&self, hit: Hit) -> Option<SankeySelection> {
        match hit {
            Hit::Node(index) => self
                .nodes
                .get(index)
                .map(|node| SankeySelection::Node(node.id.clone())),
            Hit::Link(index) => self
                .links
                .get(index)
                .map(|link| SankeySelection::Link(link.id.clone())),
        }
    }
    pub fn inspect(&mut self, selection: &SankeySelection) -> bool {
        self.hovered = match selection {
            SankeySelection::Node(id) => self
                .nodes
                .iter()
                .position(|node| node.id == *id)
                .map(Hit::Node),
            SankeySelection::Link(id) => self
                .links
                .iter()
                .position(|link| link.id == *id && link.value.is_finite() && link.value > 0.0)
                .map(Hit::Link),
        };
        self.keyboard_inspection = self.hovered.is_some();
        self.update_inspection_anchor();
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }
    fn update_inspection_anchor(&mut self) {
        let Some(hit) = self.hovered else { return };
        let (nodes, links) = self.geometry();
        self.pointer = match hit {
            Hit::Node(index) => nodes
                .iter()
                .find(|node| node.index == index)
                .map_or(self.pointer, |node| node.rect.center()),
            Hit::Link(index) => {
                links
                    .iter()
                    .find(|link| link.index == index)
                    .map_or(self.pointer, |link| {
                        Point::new(
                            link.source.x.midpoint(link.target.x),
                            link.source.y.midpoint(link.target.y),
                        )
                    })
            }
        };
    }

    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 18.0,
            self.rect.y0 + if self.title.is_empty() { 18.0 } else { 52.0 },
            self.rect.x1 - 18.0,
            self.rect.y1 - 18.0,
        )
    }

    fn resolved_links(&self) -> Vec<(usize, usize, usize, f64)> {
        let lookup = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect::<HashMap<_, _>>();
        self.links
            .iter()
            .enumerate()
            .filter_map(|(index, link)| {
                let source = *lookup.get(link.source.as_str())?;
                let target = *lookup.get(link.target.as_str())?;
                (source != target && link.value.is_finite() && link.value > 0.0)
                    .then_some((index, source, target, link.value))
            })
            .collect()
    }

    fn depths(&self, links: &[(usize, usize, usize, f64)]) -> Vec<usize> {
        let mut depth = vec![0_usize; self.nodes.len()];
        // Bounded relaxation gives a useful left-to-right layout for DAGs and
        // degrades cycles into the final column without looping forever.
        for _ in 0..self.nodes.len() {
            let previous = depth.clone();
            for (_, source, target, _) in links {
                let candidate = previous
                    .get(*source)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(1)
                    .min(self.nodes.len().saturating_sub(1));
                if let Some(target_depth) = depth.get_mut(*target) {
                    *target_depth = (*target_depth).max(candidate);
                }
            }
            if depth == previous {
                break;
            }
        }
        depth
    }

    fn node_values(&self, links: &[(usize, usize, usize, f64)]) -> Vec<f64> {
        let mut incoming = vec![0.0_f64; self.nodes.len()];
        let mut outgoing = incoming.clone();
        for (_, source, target, value) in links {
            if let Some(total) = outgoing.get_mut(*source) {
                *total += value;
            }
            if let Some(total) = incoming.get_mut(*target) {
                *total += value;
            }
        }
        incoming
            .into_iter()
            .zip(outgoing)
            .map(|(input, output)| input.max(output).max(f64::EPSILON))
            .collect()
    }

    fn node_boxes(&self, links: &[(usize, usize, usize, f64)]) -> Vec<NodeBox> {
        let content = self.content_rect();
        let depths = self.depths(links);
        let values = self.node_values(links);
        let max_depth = depths.iter().copied().max().unwrap_or_default();
        let columns = max_depth.saturating_add(1);
        let x_step = if columns > 1 {
            (content.width() - self.style.node_width)
                / f64::from(u32::try_from(columns - 1).unwrap_or(u32::MAX))
        } else {
            0.0
        };
        let mut boxes = Vec::new();
        for column in 0..columns {
            let indices = depths
                .iter()
                .enumerate()
                .filter_map(|(index, depth)| (*depth == column).then_some(index))
                .collect::<Vec<_>>();
            let total = indices
                .iter()
                .filter_map(|index| values.get(*index))
                .sum::<f64>();
            let gap_total = self.style.node_gap
                * f64::from(u32::try_from(indices.len().saturating_sub(1)).unwrap_or(u32::MAX));
            let usable = (content.height() - gap_total).max(1.0);
            let mut y = content.y0;
            for index in indices {
                let value = values.get(index).copied().unwrap_or(f64::EPSILON);
                let height =
                    (usable * value / total.max(f64::EPSILON)).max(self.style.minimum_node_height);
                let x = content.x0 + f64::from(u32::try_from(column).unwrap_or(u32::MAX)) * x_step;
                boxes.push(NodeBox {
                    index,
                    rect: Rect::new(
                        x,
                        y,
                        x + self.style.node_width,
                        (y + height).min(content.y1),
                    ),
                    value,
                });
                y += height + self.style.node_gap;
            }
        }
        boxes
    }

    fn geometry(&self) -> (Vec<NodeBox>, Vec<LinkBand>) {
        let links = self.resolved_links();
        let boxes = self.node_boxes(&links);
        let by_node = boxes
            .iter()
            .map(|node| (node.index, *node))
            .collect::<HashMap<_, _>>();
        let mut source_offsets = vec![0.0; self.nodes.len()];
        let mut target_offsets = source_offsets.clone();
        let mut bands = Vec::new();
        for (index, source, target, value) in links {
            let (Some(source_box), Some(target_box)) = (by_node.get(&source), by_node.get(&target))
            else {
                continue;
            };
            let source_height = source_box.rect.height() * value / source_box.value;
            let target_height = target_box.rect.height() * value / target_box.value;
            let sy = source_box.rect.y0
                + source_offsets.get(source).copied().unwrap_or_default()
                + source_height / 2.0;
            let ty = target_box.rect.y0
                + target_offsets.get(target).copied().unwrap_or_default()
                + target_height / 2.0;
            if let Some(offset) = source_offsets.get_mut(source) {
                *offset += source_height;
            }
            if let Some(offset) = target_offsets.get_mut(target) {
                *offset += target_height;
            }
            let source_point = Point::new(source_box.rect.x1, sy);
            let target_point = Point::new(target_box.rect.x0, ty);
            let curve = self.style.link_curvature.clamp(0.0, 1.0);
            let c1x = source_point.x + (target_point.x - source_point.x) * curve;
            let c2x = target_point.x - (target_point.x - source_point.x) * curve;
            let mut path = BezPath::new();
            path.move_to((source_point.x, sy - source_height / 2.0));
            path.curve_to(
                (c1x, sy - source_height / 2.0),
                (c2x, ty - target_height / 2.0),
                (target_point.x, ty - target_height / 2.0),
            );
            path.line_to((target_point.x, ty + target_height / 2.0));
            path.curve_to(
                (c2x, ty + target_height / 2.0),
                (c1x, sy + source_height / 2.0),
                (source_point.x, sy + source_height / 2.0),
            );
            path.close_path();
            bands.push(LinkBand {
                index,
                path,
                source: source_point,
                target: target_point,
                thickness: source_height.max(target_height),
            });
        }
        (boxes, bands)
    }

    fn update_hover(&mut self) {
        let (nodes, links) = self.geometry();
        if let Some(node) = nodes
            .iter()
            .find(|node| node.rect.inflate(3.0, 3.0).contains(self.pointer))
        {
            self.hovered = Some(Hit::Node(node.index));
            return;
        }
        self.hovered = links
            .iter()
            .find(|link| {
                (0_u32..=20).any(|step| {
                    let t = f64::from(step) / 20.0;
                    let x = link.source.x + (link.target.x - link.source.x) * t;
                    let smooth = t * t * (3.0 - 2.0 * t);
                    let y = link.source.y + (link.target.y - link.source.y) * smooth;
                    Point::new(x, y).distance(self.pointer) <= link.thickness / 2.0 + 4.0
                })
            })
            .map(|link| Hit::Link(link.index));
    }

    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }
}

impl Widget for SankeyChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        reason = "ribbons, nodes, labels and inspector share one ordered flow paint pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.keyboard_inspection {
            self.update_inspection_anchor();
        }
        let (nodes, links) = self.geometry();
        if !self.title.is_empty() {
            tr.draw_text_weighted(
                scene,
                &self.title,
                theme.tokens.typography.scale.lg,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
        }
        for link in &links {
            let Some(spec) = self.links.get(link.index) else {
                continue;
            };
            let source_index = self
                .nodes
                .iter()
                .position(|node| node.id == spec.source)
                .unwrap_or_default();
            let base = spec
                .color
                .unwrap_or_else(|| Self::palette(theme, source_index));
            let alpha = if self.hovered == Some(Hit::Link(link.index)) {
                self.style.link_alpha.saturating_add(70)
            } else {
                self.style.link_alpha
            };
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(base.r(), base.g(), base.b(), alpha),
                None,
                &link.path,
            );
        }
        for node in &nodes {
            let Some(spec) = self.nodes.get(node.index) else {
                continue;
            };
            let color = spec
                .color
                .unwrap_or_else(|| Self::palette(theme, node.index));
            let shape = RoundedRect::from_rect(
                node.rect,
                self.style.node_radius.min(node.rect.width() / 2.0),
            );
            scene.fill(Fill::NonZero, transform, color, None, &shape);
            if self.hovered == Some(Hit::Node(node.index)) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }
            let label_x = if node.rect.center().x < self.rect.center().x {
                node.rect.x1 + 8.0
            } else {
                node.rect.x0 - 8.0
            };
            let (width, _) = tr.measure(&spec.label, theme.tokens.typography.scale.xs, None);
            let x = if node.rect.center().x < self.rect.center().x {
                label_x
            } else {
                label_x - f64::from(width)
            };
            tr.draw_text_weighted(
                scene,
                &spec.label,
                theme.tokens.typography.scale.xs,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x, node.rect.y0 + 2.0)),
                None,
                TextWeight::Medium,
            );
            if self.style.show_values && node.rect.height() >= 34.0 {
                tr.draw_text(
                    scene,
                    &format_compact(node.value),
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((x, node.rect.y0 + 19.0)),
                    None,
                );
            }
        }
        if let Some(hit) = self.hovered.filter(|_| self.style.tooltip) {
            let (title, detail) = match hit {
                Hit::Node(index) => {
                    let Some(node) = self.nodes.get(index) else {
                        return;
                    };
                    let value = nodes
                        .iter()
                        .find(|geometry| geometry.index == index)
                        .map_or(0.0, |geometry| geometry.value);
                    (
                        node.label.clone(),
                        if node.detail.is_empty() {
                            format!("{} total flow", format_compact(value))
                        } else {
                            node.detail.clone()
                        },
                    )
                }
                Hit::Link(index) => {
                    let Some(link) = self.links.get(index) else {
                        return;
                    };
                    let source = self
                        .nodes
                        .iter()
                        .find(|node| node.id == link.source)
                        .map_or(link.source.as_str(), |node| node.label.as_str());
                    let target = self
                        .nodes
                        .iter()
                        .find(|node| node.id == link.target)
                        .map_or(link.target.as_str(), |node| node.label.as_str());
                    (
                        format!("{source} → {target}"),
                        if link.detail.is_empty() {
                            format_compact(link.value)
                        } else {
                            link.detail.clone()
                        },
                    )
                }
            };
            let (tw, _) = tr.measure(&title, theme.tokens.typography.scale.sm, None);
            let (dw, _) = tr.measure(&detail, theme.tokens.typography.scale.xs, None);
            let width = f64::from(tw.max(dw)).max(150.0) + 28.0;
            let mut x = self.pointer.x + 16.0;
            let mut y = self.pointer.y + 16.0;
            if x + width > self.rect.x1 {
                x = self.pointer.x - width - 16.0;
            }
            if y + 66.0 > self.rect.y1 {
                y = self.pointer.y - 76.0;
            }
            let tooltip = Rect::new(x, y, x + width, y + 64.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &RoundedRect::from_rect(tooltip, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_text_weighted(
                scene,
                &title,
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
        if let Some(action) = self.touch.handle(event, ctx.scale_factor) {
            match action {
                ChartTouchAction::Inspect(point) => {
                    self.pointer = point;
                    self.update_hover();
                    self.keyboard_inspection = false;
                }
                ChartTouchAction::Activate(point) => {
                    self.pointer = point;
                    self.update_hover();
                    if let Some(hit) = self.hovered {
                        self.activated = self.selection_for(hit);
                        if self.activated.is_some() {
                            *ctx.handled = true;
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                    }
                }
                ChartTouchAction::Clear => self.hovered = None,
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.update_hover();
                self.keyboard_inspection = false;
            }
            WindowEvent::CursorLeft { .. } if !self.keyboard_inspection => self.hovered = None,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self.hovered {
                    self.activated = self.selection_for(hit);
                    if self.activated.is_some() {
                        *ctx.handled = true;
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    self.activated = self.hovered.and_then(|hit| self.selection_for(hit));
                } else {
                    let (_, links) = self.geometry();
                    let candidates = self
                        .nodes
                        .iter()
                        .enumerate()
                        .map(|(index, _)| Hit::Node(index))
                        .chain(links.iter().map(|link| Hit::Link(link.index)))
                        .collect::<Vec<_>>();
                    let current = self
                        .hovered
                        .and_then(|hit| candidates.iter().position(|candidate| *candidate == hit));
                    self.hovered = moved_index(current, candidates.len(), action)
                        .and_then(|index| candidates.get(index).copied());
                    self.keyboard_inspection = self.hovered.is_some();
                    self.update_inspection_anchor();
                }
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            _ => {}
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut children = self
            .nodes
            .iter()
            .map(|node| {
                AccessNode::new(Role::Button)
                    .with_action_target(format!("node:{}", node.id))
                    .with_name(&node.label)
                    .with_description(&node.detail)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect::<Vec<_>>();
        children.extend(
            self.links
                .iter()
                .filter(|link| link.value.is_finite() && link.value > 0.0)
                .map(|link| {
                    AccessNode::new(Role::Button)
                        .with_action_target(format!("link:{}", link.id))
                        .with_name(format!("{} to {}", link.source, link.target))
                        .with_description(if link.detail.is_empty() {
                            format!("Flow {}", link.value)
                        } else {
                            link.detail.clone()
                        })
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                }),
        );
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.title.is_empty() {
                    "Sankey diagram"
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        for node in &self.nodes {
            if crate::a11y::hash_key(&format!("node:{}", node.id)) == *key {
                self.activated = Some(SankeySelection::Node(node.id.clone()));
                return true;
            }
        }
        for link in &self.links {
            if crate::a11y::hash_key(&format!("link:{}", link.id)) == *key {
                self.activated = Some(SankeySelection::Link(link.id.clone()));
                return true;
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "SankeyChart"
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

fn format_compact(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.abs() >= 10.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_and_dangling_links_are_ignored() {
        let chart = SankeyChart::new(
            vec![SankeyNode::new("a", "A")],
            vec![
                SankeyLink::new("bad", "a", "missing", 2.0),
                SankeyLink::new("self", "a", "a", 1.0),
            ],
        );
        assert!(chart.resolved_links().is_empty());
    }
}
