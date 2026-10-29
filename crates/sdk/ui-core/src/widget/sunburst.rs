//! Interactive hierarchical sunburst charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "radial hierarchy projection is bounded floating-point geometry"
)]

use std::f64::consts::{FRAC_PI_2, TAU};
use std::ops::Range;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::chart_labels::format_compact;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

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

pub struct SunburstMarks {
    root: SunburstNode,
    style: SunburstStyle,
    center_title: String,
    center_subtitle: String,
    frame: PlotFrame<String>,
}

/// A hierarchy as rings of sectors, the root at the centre.
pub type SunburstChart = Plot<SunburstMarks>;

impl SunburstChart {
    pub fn new(root: SunburstNode) -> Self {
        Plot::from_marks(SunburstMarks {
            root,
            style: SunburstStyle::default(),
            center_title: String::new(),
            center_subtitle: String::new(),
            frame: PlotFrame::new(440.0, 180.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: SunburstStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn with_center(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.marks.center_title = title.into();
        self.marks.center_subtitle = subtitle.into();
        self
    }
    pub fn set_root(&mut self, root: SunburstNode) {
        self.marks.root = root;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let known = self.marks.segments().iter().any(|segment| segment.id == id);
        self.inspect_item(known.then(|| id.to_owned()))
    }
}

impl SunburstMarks {
    fn depth(&self) -> usize {
        self.style
            .max_depth
            .unwrap_or_else(|| self.root.depth().saturating_sub(1))
            .min(self.root.depth().saturating_sub(1))
            .max(1)
    }
    fn geometry(&self) -> (Point, f64, f64, f64) {
        let rect = self.frame.rect;
        let top = if self.frame.title.is_empty() {
            0.0
        } else {
            34.0
        };
        let rect = Rect::new(rect.x0, rect.y0 + top, rect.x1, rect.y1);
        let center = rect.center();
        let outer = (rect.width().min(rect.height()) / 2.0 - 16.0).max(1.0);
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
        crate::tokens::colors::mix8(base, target, crate::num::f32_of_f64(amount))
    }
}

impl PlotMarks for SunburstMarks {
    type Hover = String;
    type Activated = String;
    const NAME: &'static str = "SunburstChart";
    const LABEL: &'static str = "Sunburst chart";

    fn frame(&self) -> &PlotFrame<String> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<String> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<String> {
        self.hit_test(point)
    }
    fn activation(&self, id: &String) -> Option<String> {
        Some(id.clone())
    }
    fn keyboard_items(&self) -> Vec<String> {
        self.segments()
            .into_iter()
            .map(|segment| segment.id)
            .collect()
    }
    fn anchor(&self, id: &String) -> Option<Point> {
        let segment = self
            .segments()
            .into_iter()
            .find(|segment| segment.id == *id)?;
        let (center, inner, _, band) = self.geometry();
        let angle = segment.start.midpoint(segment.end);
        let depth = f64::from(u32::try_from(segment.depth).unwrap_or(u32::MAX));
        let radius = inner + (depth - 0.5) * band;
        Some(Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ))
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.segments()
            .into_iter()
            .map(|segment| {
                crate::widget::plot::value_item(
                    &segment.id,
                    &segment.path,
                    &segment.detail,
                    segment.value,
                )
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
        crate::widget::chart_navigation::draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        let (base_center, inner, _, band) = self.geometry();
        for segment in self.segments() {
            let half_gap = (self.style.angular_gap / 2.0).min((segment.end - segment.start) * 0.2);
            let mid = segment.start.midpoint(segment.end);
            let lift = if self.frame.inspection.hovered.as_deref() == Some(segment.id.as_str()) {
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
            if self.frame.inspection.hovered.as_deref() == Some(segment.id.as_str()) {
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
        if let Some(id) = self
            .frame
            .inspection
            .hovered
            .as_deref()
            .filter(|_| self.style.tooltip)
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
                self.frame.inspection.pointer,
                self.frame.rect,
                170.0,
                (&segment.path, &detail),
            );
        }
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
