//! Interactive hierarchical sunburst charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "radial hierarchy projection is bounded floating-point geometry"
)]

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::ops::Range;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, handle_chart_event, moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

const ARC_STEPS: u32 = 64;

#[derive(Clone, Debug)]
pub struct SunburstNode {
    pub id: String,
    pub label: String,
    /// A non-positive value is derived from the children.
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
    pub children: Vec<SunburstNode>,
}

impl SunburstNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
            children: Vec::new(),
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
    #[must_use]
    pub fn with_children(mut self, children: Vec<SunburstNode>) -> Self {
        self.children = children;
        self
    }
    fn effective_value(&self) -> f64 {
        let children = self.children.iter().map(Self::effective_value).sum::<f64>();
        if self.value.is_finite() && self.value > 0.0 {
            self.value.max(children)
        } else {
            children
        }
    }
    fn depth(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(Self::depth)
            .max()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct SunburstStyle {
    pub inner_radius: f64,
    pub level_gap: f64,
    pub angular_gap: f64,
    pub start_angle: f64,
    pub max_depth: Option<usize>,
    pub hover_offset: f64,
    pub tooltip: bool,
}

impl Default for SunburstStyle {
    fn default() -> Self {
        Self {
            inner_radius: 0.22,
            level_gap: 3.0,
            angular_gap: 0.012,
            start_angle: -FRAC_PI_2,
            max_depth: None,
            hover_offset: 4.0,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug)]
struct Segment {
    id: String,
    detail: String,
    path: String,
    value: f64,
    depth: usize,
    start: f64,
    end: f64,
    color: Option<Color>,
    family: usize,
}

/// Where a node sits in the hierarchy being flattened: its ring, the
/// top-level family it descends from, its ancestors' label path and the
/// colour it inherits when it names none.
struct Lineage<'a> {
    depth: usize,
    family: usize,
    path: &'a str,
    color: Option<Color>,
}

pub struct SunburstChart {
    root: SunburstNode,
    style: SunburstStyle,
    center_title: String,
    center_subtitle: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<String>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl SunburstChart {
    pub fn new(root: SunburstNode) -> Self {
        Self {
            root,
            style: SunburstStyle::default(),
            center_title: String::new(),
            center_subtitle: String::new(),
            height: 440.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: SunburstStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: SunburstStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &SunburstStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut SunburstStyle {
        &mut self.style
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.center_title = title.into();
        self.center_subtitle = subtitle.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(180.0);
        self
    }
    pub fn set_root(&mut self, root: SunburstNode) {
        self.root = root;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self
            .segments()
            .iter()
            .any(|segment| segment.id == id)
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
        let Some(segment) = self.segments().into_iter().find(|segment| segment.id == id) else {
            return;
        };
        let (center, inner, _, band) = self.geometry();
        let angle = segment.start.midpoint(segment.end);
        let depth = f64::from(u32::try_from(segment.depth).unwrap_or(u32::MAX));
        let radius = inner + (depth - 0.5) * band;
        self.pointer = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
    }

    fn depth(&self) -> usize {
        self.style
            .max_depth
            .unwrap_or_else(|| self.root.depth().saturating_sub(1))
            .min(self.root.depth().saturating_sub(1))
            .max(1)
    }
    fn geometry(&self) -> (Point, f64, f64, f64) {
        let center = self.rect.center();
        let outer = (self.rect.width().min(self.rect.height()) / 2.0 - 16.0).max(1.0);
        let inner = outer * self.style.inner_radius.clamp(0.0, 0.75);
        let band = (outer - inner) / f64::from(u32::try_from(self.depth()).unwrap_or(u32::MAX));
        (center, inner, outer, band)
    }

    fn segments(&self) -> Vec<Segment> {
        let mut output = Vec::new();
        let total = self
            .root
            .children
            .iter()
            .map(SunburstNode::effective_value)
            .sum::<f64>();
        if total <= f64::EPSILON {
            return output;
        }
        let mut angle = self.style.start_angle;
        for (family, child) in self.root.children.iter().enumerate() {
            let sweep = child.effective_value() / total * TAU;
            self.flatten(
                child,
                angle..angle + sweep,
                Lineage {
                    depth: 1,
                    family,
                    path: &self.root.label,
                    color: None,
                },
                &mut output,
            );
            angle += sweep;
        }
        output
    }

    /// Push `node`'s segment over the angular `span`, then its children's
    /// across the same span in proportion to their values.
    fn flatten(
        &self,
        node: &SunburstNode,
        span: Range<f64>,
        lineage: Lineage<'_>,
        output: &mut Vec<Segment>,
    ) {
        let Lineage {
            depth,
            family,
            path: parent_path,
            color: inherited_color,
        } = lineage;
        if depth > self.depth() || node.effective_value() <= f64::EPSILON {
            return;
        }
        let path = if parent_path.is_empty() {
            node.label.clone()
        } else {
            format!("{parent_path} › {}", node.label)
        };
        let color = node.color.or(inherited_color);
        let Range { start, end } = span;
        output.push(Segment {
            id: node.id.clone(),
            detail: node.detail.clone(),
            path: path.clone(),
            value: node.effective_value(),
            depth,
            start,
            end,
            color,
            family,
        });
        let total = node
            .children
            .iter()
            .map(SunburstNode::effective_value)
            .sum::<f64>();
        if total <= f64::EPSILON {
            return;
        }
        let mut angle = start;
        let scale = (end - start) / total;
        for child in &node.children {
            let sweep = child.effective_value() * scale;
            self.flatten(
                child,
                angle..angle + sweep,
                Lineage {
                    depth: depth.saturating_add(1),
                    family,
                    path: &path,
                    color,
                },
                output,
            );
            angle += sweep;
        }
    }

    fn wedge(center: Point, inner: f64, outer: f64, start: f64, end: f64) -> BezPath {
        let mut path = BezPath::new();
        let polar = |radius: f64, angle: f64| {
            Point::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        };
        path.move_to(polar(outer, start));
        for step in 1..=ARC_STEPS {
            let fraction = f64::from(step) / f64::from(ARC_STEPS);
            path.line_to(polar(outer, start + (end - start) * fraction));
        }
        path.line_to(polar(inner, end));
        for step in 1..=ARC_STEPS {
            let fraction = f64::from(step) / f64::from(ARC_STEPS);
            path.line_to(polar(inner, end - (end - start) * fraction));
        }
        path.close_path();
        path
    }

    fn hit_test(&self, point: Point) -> Option<String> {
        let (center, inner, outer, band) = self.geometry();
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        let radius = dx.hypot(dy);
        if radius < inner || radius > outer + self.style.hover_offset {
            return None;
        }
        let depth = ((radius - inner) / band).floor();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "nonnegative radius is bounded by the finite chart depth"
        )]
        let depth = depth as usize + 1;
        let angle =
            (dy.atan2(dx) - self.style.start_angle).rem_euclid(TAU) + self.style.start_angle;
        self.segments()
            .into_iter()
            .find(|segment| segment.depth == depth && angle >= segment.start && angle < segment.end)
            .map(|segment| segment.id)
    }

    fn palette(theme: &Theme, family: usize, depth: usize) -> Color {
        let colors = &theme.tokens.colors;
        let base = colors.presence.series(family);
        let target = colors.surfaces.base;
        let amount = (f64::from(u32::try_from(depth.saturating_sub(1)).unwrap_or(u32::MAX)) * 0.12)
            .min(0.48);
        mix(base, target, amount)
    }
}

fn mix(start: Color, end: Color, amount: f64) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        let value = f64::from(a) + (f64::from(b) - f64::from(a)) * amount;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "interpolation remains inside u8 endpoints"
        )]
        {
            value.round() as u8
        }
    };
    Color::from_rgb8(
        channel(start.r(), end.r()),
        channel(start.g(), end.g()),
        channel(start.b(), end.b()),
    )
}

impl InteractiveChart for SunburstChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self.hit_test(self.pointer);
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(id) = &self.hovered else {
            return false;
        };
        self.activated = Some(id.clone());
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let candidates = self.segments();
        let current = self
            .hovered
            .as_deref()
            .and_then(|id| candidates.iter().position(|segment| segment.id == id));
        self.hovered = moved_index(current, candidates.len(), action)
            .and_then(|index| candidates.get(index))
            .map(|segment| segment.id.clone());
        self.update_inspection_anchor();
        self.hovered.is_some()
    }
}

impl Widget for SunburstChart {
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
        reason = "the retained draw pass keeps segment, center-label, and tooltip painting together"
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
        let (base_center, inner, _, band) = self.geometry();
        for segment in self.segments() {
            let half_gap = (self.style.angular_gap / 2.0).min((segment.end - segment.start) * 0.2);
            let mid = segment.start.midpoint(segment.end);
            let lift = if self.hovered.as_deref() == Some(segment.id.as_str()) {
                self.style.hover_offset
            } else {
                0.0
            };
            let center = Point::new(
                base_center.x + lift * mid.cos(),
                base_center.y + lift * mid.sin(),
            );
            let inner_radius = inner
                + f64::from(u32::try_from(segment.depth.saturating_sub(1)).unwrap_or(u32::MAX))
                    * band
                + self.style.level_gap / 2.0;
            let outer_radius = inner
                + f64::from(u32::try_from(segment.depth).unwrap_or(u32::MAX)) * band
                - self.style.level_gap / 2.0;
            let color = segment
                .color
                .unwrap_or_else(|| Self::palette(theme, segment.family, segment.depth));
            let path = Self::wedge(
                center,
                inner_radius,
                outer_radius,
                segment.start + half_gap,
                segment.end - half_gap,
            );
            scene.fill(Fill::NonZero, transform, color, None, &path);
            if self.hovered.as_deref() == Some(segment.id.as_str()) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &path,
                );
            }
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &Circle::new(base_center, (inner - 2.0).max(0.0)),
        );
        // The centre copy at the largest size the hole holds; left out
        // where even a heading does not fit (the tooltip carries it).
        let _ = crate::widget::chart_labels::paint_centre_label(
            scene,
            transform,
            tr,
            theme,
            base_center,
            (inner - 2.0).max(0.0),
            &self.center_title,
            &self.center_subtitle,
        );
        if let Some(id) = self.hovered.as_deref().filter(|_| self.style.tooltip)
            && let Some(segment) = self.segments().into_iter().find(|segment| segment.id == id)
        {
            let detail = if segment.detail.is_empty() {
                format_compact(segment.value)
            } else {
                format!("{}  ·  {}", format_compact(segment.value), segment.detail)
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.pointer,
                self.rect,
                170.0,
                (&segment.path, &detail),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let children = self
            .segments()
            .into_iter()
            .map(|segment| {
                AccessNode::new(Role::Button)
                    .with_action_target(&segment.id)
                    .with_name(&segment.path)
                    .with_description(if segment.detail.is_empty() {
                        format!("Value {}", segment.value)
                    } else {
                        segment.detail
                    })
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name("Sunburst chart")
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
        if let Some(segment) = self
            .segments()
            .into_iter()
            .find(|segment| crate::a11y::hash_key(&segment.id) == *key)
        {
            self.activated = Some(segment.id);
            return true;
        }
        false
    }
    fn name(&self) -> &'static str {
        "SunburstChart"
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
    fn parent_values_are_derived_from_children() {
        let root = SunburstNode::new("root", "Root", 0.0).with_children(vec![
            SunburstNode::new("a", "A", 2.0),
            SunburstNode::new("b", "B", 3.0),
        ]);
        assert_eq!(root.effective_value(), 5.0);
    }
}
