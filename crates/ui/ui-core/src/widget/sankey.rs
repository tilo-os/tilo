//! Automatic-layout interactive Sankey flow diagrams.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "flow layout and Bézier ribbon geometry are bounded floating-point operations"
)]

use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape as _, Stroke};
use vello::peniko::{Color, Fill};

use crate::color_compat::ColorCompat;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::{ChartLink, ChartNode, graph_items};
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

/// The height the flow draws at: its nodes share the room between its labels, so a flow whose
/// labels break over two lines at a narrow width grows to keep them whole.
const NATURAL_HEIGHT: f64 = 420.0;

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

pub type SankeySelection = crate::widget::chart_data::GraphSelection;

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

/// A node's label as it is painted: the lines it reads as with the box each covers, and its
/// value under them.
#[derive(Clone, Debug)]
struct NodeLabel {
    /// Which node it names.
    #[cfg_attr(not(test), allow(dead_code))]
    index: usize,
    lines: Vec<(String, Rect)>,
    value: Option<(String, Rect)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hit {
    Node(usize),
    Link(usize),
}

pub struct SankeyMarks {
    nodes: Vec<ChartNode>,
    links: Vec<ChartLink>,
    style: SankeyStyle,
    frame: PlotFrame<Hit>,
}

pub type SankeyChart = Plot<SankeyMarks>;

#[path = "sankey_component.rs"]
mod component;
pub use component::SankeyChartProps;

impl SankeyChart {
    pub fn new(nodes: Vec<ChartNode>, links: Vec<ChartLink>) -> Self {
        Plot::from_marks(SankeyMarks {
            nodes,
            links,
            style: SankeyStyle::default(),
            frame: PlotFrame::fill(180.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: SankeyStyle) -> Self {
        self.marks.style = style;
        self
    }
    pub fn set_data(&mut self, nodes: Vec<ChartNode>, links: Vec<ChartLink>) {
        self.marks.nodes = nodes;
        self.marks.links = links;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, selection: &SankeySelection) -> bool {
        let hit = match selection {
            SankeySelection::Node(id) => self
                .marks
                .nodes
                .iter()
                .position(|node| node.id == *id)
                .map(Hit::Node),
            SankeySelection::Link(id) => self
                .marks
                .links
                .iter()
                .position(|link| link.id == *id && link.flows())
                .map(Hit::Link),
        };
        self.inspect_item(hit)
    }
}

impl SankeyMarks {
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

    fn content_rect(&self) -> Rect {
        self.frame.inside()
    }

    /// The title over the flow, and the trailing width the last column's labels need: its
    /// widest label, never more than a third of the chart, so the flow keeps its width.
    fn measure_margins(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let links = self.resolved_links();
        let depths = self.depths(&links);
        let last = depths.iter().copied().max().unwrap_or_default();
        let style = Self::label_style(theme);
        let widest = self
            .nodes
            .iter()
            .zip(&depths)
            .filter(|(_, depth)| **depth == last && last > 0)
            .map(|(node, _)| f64::from(tr.measure_style(&node.label, None, &style).0))
            .fold(0.0, f64::max);
        let pad = f64::from(theme.tokens.spacing.s2);
        let reserve = if widest > 0.0 { widest + pad } else { 0.0 };
        let mut margins = Margins::header(theme, &self.frame.title, 0);
        margins.right = reserve.min(self.frame.rect.width() / 3.0).max(0.0);
        self.frame.margins = margins;
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

    /// Where each node's label goes, laid out so no two collide at any size: after its node, in
    /// the gap before the next column (the last column's in the trailing width the chart
    /// reserves for them), broken over the lines the room between it and the node below holds so
    /// a long name reads whole; shortened at a word boundary where even those lines do not hold
    /// it; and dropped (the tooltip and accessible name still read the node) where not even a
    /// whole word either side of its ellipsis fits, or it would run into the label above it.
    #[allow(
        clippy::too_many_lines,
        reason = "each node's column, room and collision are weighed in one ordered pass"
    )]
    fn label_layout(
        &self,
        nodes: &[NodeBox],
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<NodeLabel> {
        let style = Self::label_style(theme);
        let value_style = theme.text(TextRole::Caption);
        let line = f64::from(style.line_height_px());
        let value_line = f64::from(value_style.line_height_px());
        let pad = f64::from(theme.tokens.spacing.s2);
        let mut columns: Vec<f64> = nodes.iter().map(|node| node.rect.x0).collect();
        columns.compact_sort_by(f64::total_cmp);
        columns.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        let last = columns.len().saturating_sub(1);
        let column_of = |x: f64| {
            columns
                .iter()
                .position(|c| (c - x).abs() < 0.5)
                .unwrap_or_default()
        };
        let gap = if columns.len() > 1 {
            columns[1] - columns[0] - self.style.node_width
        } else {
            0.0
        };
        let mut order: Vec<&NodeBox> = nodes.iter().collect();
        order.compact_sort_by(|a, b| {
            a.rect
                .x0
                .total_cmp(&b.rect.x0)
                .then(a.rect.y0.total_cmp(&b.rect.y0))
        });
        // Where each column's nodes start, in order: a label's room ends at the node below it.
        let mut tops: HashMap<usize, Vec<f64>> = HashMap::new();
        for node in &order {
            tops.entry(column_of(node.rect.x0))
                .or_default()
                .push(node.rect.y0);
        }
        let mut bottoms: HashMap<usize, f64> = HashMap::new();
        let mut placed = Vec::new();
        for node in order {
            let Some(spec) = self.nodes.get(node.index) else {
                continue;
            };
            let column = column_of(node.rect.x0);
            // A label stands a step past its node; the last column's ends on the chart's edge.
            let spare = if column == last {
                self.frame.rect.x1 - node.rect.x1 - pad
            } else {
                gap - 2.0 * pad
            };
            let x = node.rect.x1 + pad;
            let y = node.rect.y0 + 2.0;
            let below = tops
                .get(&column)
                .and_then(|tops| tops.iter().copied().find(|top| *top > node.rect.y0 + 0.5))
                .unwrap_or(self.frame.rect.y1);
            let available = (below - y - pad).max(0.0);
            let value_text = format_compact(node.value);
            let (value_width, _) = tr.measure_style(&value_text, None, &value_style);
            let value_width = f64::from(value_width);
            let show_value = self.style.show_values
                && value_width <= spare
                && node.rect.height() >= line + value_line + 4.0;
            let room = available - if show_value { value_line } else { 0.0 };
            if spare <= 0.0 || room + 0.5 < line {
                continue;
            }
            // Whole on one line where it fits, whole over two lines where a break fits, otherwise
            // whole words around one ellipsis.
            let (whole_width, whole_height) = tr.measure_style(&spec.label, None, &style);
            let (whole_width, whole_height) = (f64::from(whole_width), f64::from(whole_height));
            let mut lines = if whole_width <= spare && whole_height <= room + 0.5 {
                vec![(spec.label.clone(), whole_width, whole_height)]
            } else {
                crate::widget::chart_labels::wrapped_labels(tr, &spec.label, &style)
                    .into_iter()
                    .find(|wrapped| wrapped.width <= spare && wrapped.height <= room + 0.5)
                    .map(|wrapped| wrapped.lines)
                    .unwrap_or_default()
            };
            if lines.is_empty() {
                lines = crate::widget::chart_labels::shortened_labels(tr, &spec.label, &style)
                    .into_iter()
                    .find(|(_, width, height)| *width <= spare && *height <= room + 0.5)
                    .map(|(text, width, height)| vec![(text, width, height)])
                    .unwrap_or_default();
            }
            if lines.is_empty() {
                continue;
            }
            let bottom = bottoms.entry(column).or_insert(f64::NEG_INFINITY);
            if y < *bottom {
                continue;
            }
            let mut cursor = y;
            let mut shown = Vec::with_capacity(lines.len());
            for (text, width, height) in lines {
                shown.push((text, Rect::new(x, cursor, x + width, cursor + height)));
                cursor += height;
            }
            let value = show_value.then(|| {
                let at = Rect::new(x, cursor, x + value_width, cursor + value_line);
                (value_text, at)
            });
            if value.is_some() {
                cursor += value_line;
            }
            *bottom = cursor;
            placed.push(NodeLabel {
                index: node.index,
                lines: shown,
                value,
            });
        }
        placed
    }

    fn label_style(theme: &Theme) -> crate::text_render::TextStyle {
        theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Medium)
    }

    /// The colour a link is painted in over the chart's ground: its palette colour, faded by
    /// the link's alpha and brought forward while it is inspected.
    fn link_ground(&self, link: &LinkBand, theme: &Theme) -> Option<Color> {
        let spec = self.links.get(link.index)?;
        let source = self
            .nodes
            .iter()
            .position(|node| node.id == spec.source)
            .unwrap_or_default();
        let base = spec.color.unwrap_or_else(|| Self::palette(theme, source));
        let alpha = if self.frame.inspection.hovered == Some(Hit::Link(link.index)) {
            self.style.link_alpha.saturating_add(70)
        } else {
            self.style.link_alpha
        };
        let wash = Color::from_rgb8(base.r(), base.g(), base.b());
        Some(crate::tokens::colors::over(
            theme.tokens.colors.surfaces.base,
            wash,
            f32::from(alpha) / 255.0,
        ))
    }

    /// The colours under `rects`: the chart's ground and every ribbon whose band crosses them.
    /// A band's outline is taken whole rather than sampled, so a word on the edge of one is
    /// read as the ribbon it touches.
    fn label_grounds(&self, links: &[LinkBand], rects: &[Rect], theme: &Theme) -> Vec<Color> {
        let base = theme.tokens.colors.surfaces.base;
        let mut grounds = vec![base];
        for link in links {
            let under = rects
                .iter()
                .any(|rect| !rect.intersect(link.path.bounding_box()).is_zero_area());
            if under && let Some(colour) = self.link_ground(link, theme) {
                grounds.push(colour);
            }
        }
        grounds
    }

    /// The ink `tone` takes to read on everything under `rects`: the small-text floor, so a
    /// pixel of ribbon blending cannot leave it under the 4.5:1 a label needs.
    fn label_ink(&self, links: &[LinkBand], rects: &[Rect], tone: Color, theme: &Theme) -> Color {
        let grounds = self.label_grounds(links, rects, theme);
        theme.tokens.colors.ink_on_any(
            &grounds,
            tone,
            crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
        )
    }

    fn draw_node_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nodes: &[NodeBox],
        links: &[LinkBand],
    ) {
        let style = Self::label_style(theme);
        let value_style = theme.text(TextRole::Caption);
        for label in self.label_layout(nodes, tr, theme) {
            for (text, rect) in &label.lines {
                tr.draw_style(
                    scene,
                    text,
                    self.label_ink(links, &[*rect], theme.tokens.colors.text_primary, theme),
                    transform * Affine::translate((rect.x0, rect.y0)),
                    None,
                    &style,
                );
            }
            if let Some((text, rect)) = &label.value {
                tr.draw_style(
                    scene,
                    text,
                    self.label_ink(links, &[*rect], theme.tokens.colors.text_dim, theme),
                    transform * Affine::translate((rect.x0, rect.y0)),
                    None,
                    &value_style,
                );
            }
        }
    }

    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }
}

impl PlotMarks for SankeyMarks {
    type Hover = Hit;
    type Activated = SankeySelection;
    const NAME: &'static str = "SankeyChart";
    const LABEL: &'static str = "Sankey diagram";

    fn frame(&self) -> &PlotFrame<Hit> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<Hit> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<Hit> {
        let (nodes, links) = self.geometry();
        if let Some(node) = nodes
            .iter()
            .find(|node| node.rect.inflate(3.0, 3.0).contains(point))
        {
            return Some(Hit::Node(node.index));
        }
        links
            .iter()
            .find(|link| {
                (0_u32..=20).any(|step| {
                    let t = f64::from(step) / 20.0;
                    let x = link.source.x + (link.target.x - link.source.x) * t;
                    let smooth = t * t * (3.0 - 2.0 * t);
                    let y = link.source.y + (link.target.y - link.source.y) * smooth;
                    Point::new(x, y).distance(point) <= link.thickness / 2.0 + 4.0
                })
            })
            .map(|link| Hit::Link(link.index))
    }
    fn activation(&self, &hit: &Hit) -> Option<SankeySelection> {
        self.selection_for(hit)
    }
    fn keyboard_items(&self) -> Vec<Hit> {
        let (_, links) = self.geometry();
        let nodes = (0..self.nodes.len()).map(Hit::Node);
        nodes
            .chain(links.iter().map(|link| Hit::Link(link.index)))
            .collect()
    }
    fn anchor(&self, &hit: &Hit) -> Option<Point> {
        let (nodes, links) = self.geometry();
        match hit {
            Hit::Node(index) => nodes
                .iter()
                .find(|node| node.index == index)
                .map(|node| node.rect.center()),
            Hit::Link(index) => links.iter().find(|link| link.index == index).map(|link| {
                Point::new(
                    link.source.x.midpoint(link.target.x),
                    link.source.y.midpoint(link.target.y),
                )
            }),
        }
    }
    fn items(&self) -> Vec<PlotItem<SankeySelection>> {
        graph_items(&self.nodes, &self.links, |value| format!("Flow {value}"))
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.measure_margins(tr, theme);
    }

    /// The height the flow wants at `width`: each column's labels stand beside its nodes, so the
    /// tallest column's names broken over two lines, with their values and the gaps between
    /// nodes, is the room its nodes share. [`NATURAL_HEIGHT`] is the least it draws at.
    fn natural_height(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let links = self.resolved_links();
        if self.nodes.is_empty() || links.is_empty() {
            return NATURAL_HEIGHT;
        }
        let depths = self.depths(&links);
        let last = depths.iter().copied().max().unwrap_or_default();
        let style = Self::label_style(theme);
        let line = f64::from(style.line_height_px());
        let value_line = crate::widget::plot::caption_line(theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        let widest = self
            .nodes
            .iter()
            .zip(&depths)
            .filter(|(_, depth)| **depth == last && last > 0)
            .map(|(node, _)| f64::from(tr.measure_style(&node.label, None, &style).0))
            .fold(0.0, f64::max);
        let reserve = if widest > 0.0 {
            (widest + pad).min(width / 3.0)
        } else {
            0.0
        };
        let content = (width - reserve).max(0.0);
        let columns = last.saturating_add(1);
        let step = if columns > 1 {
            (content - self.style.node_width) / crate::num::f64_of_usize(columns.saturating_sub(1))
        } else {
            0.0
        };
        let gap = if columns > 1 {
            step - self.style.node_width
        } else {
            0.0
        };
        let value = if self.style.show_values {
            value_line
        } else {
            0.0
        };
        let mut needed = vec![0.0_f64; columns];
        for (index, depth) in depths.iter().enumerate() {
            let Some(node) = self.nodes.get(index) else {
                continue;
            };
            let spare = if *depth == last {
                if columns > 1 {
                    reserve - pad
                } else {
                    width - self.style.node_width - pad
                }
            } else {
                gap - 2.0 * pad
            };
            let lines = crate::widget::chart_labels::wrapped_labels(tr, &node.label, &style)
                .into_iter()
                .find(|wrapped| wrapped.width <= spare.max(0.0))
                .map_or(1, |wrapped| wrapped.lines.len());
            if let Some(total) = needed.get_mut(*depth) {
                // Each label stands a step past its node's top and keeps air above the node
                // below it, so it needs those along with its lines and value.
                *total += crate::num::f64_of_usize(lines) * line
                    + value
                    + self.style.node_gap
                    + pad
                    + 2.0;
            }
        }
        let body = needed.iter().copied().fold(0.0, f64::max);
        let title = crate::widget::chart_navigation::title_band(theme, &self.frame.title);
        (title + body).max(NATURAL_HEIGHT)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ribbons, nodes, labels and inspector share one ordered flow paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.follow_keyboard_anchor();
        let (nodes, links) = self.geometry();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
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
            let alpha = if self.frame.inspection.hovered == Some(Hit::Link(link.index)) {
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
            if self.frame.inspection.hovered == Some(Hit::Node(node.index)) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }
        }
        self.draw_node_labels(scene, transform, tr, theme, &nodes, &links);
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        let (nodes, _) = self.geometry();
        if let Some(hit) = self.frame.inspection.hovered.filter(|_| self.style.tooltip) {
            let (title, detail) = match hit {
                Hit::Node(index) => {
                    let node = self.nodes.get(index)?;
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
                    let link = self.links.get(index)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::chart_labels::reads_as_whole_words;

    /// The gallery's long flow: node names far wider than the gap beside their node.
    fn docs_to_team() -> (Vec<ChartNode>, Vec<ChartLink>) {
        (
            vec![
                ChartNode::new("docs", "Visited the documentation from the release notes"),
                ChartNode::new("pricing", "Read the pricing page for the hosted workspace"),
                ChartNode::new("trial", "Started a trial on their own computer"),
                ChartNode::new("team", "Invited the rest of the team to the workspace"),
            ],
            vec![
                ChartLink::new("one", "docs", "pricing", 48.0),
                ChartLink::new("two", "pricing", "trial", 26.0),
                ChartLink::new("three", "trial", "team", 11.0),
            ],
        )
    }

    /// Every node's label stays readable at a laptop's width and a phone's: it is the whole name
    /// — broken over the lines the gap beside its node holds — or whole words at its start and
    /// its end around one ellipsis, never a cut inside a word. A node left unnamed is still read
    /// whole by its tooltip and accessible name, and no label paints outside the chart.
    #[test]
    fn node_labels_stay_readable_at_every_width() {
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let (nodes, links) = docs_to_team();
                let mut chart = SankeyChart::new(nodes, links);
                let height = chart.marks.natural_height(&mut tr, &theme, width);
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, height);
                chart.marks.place(&mut tr, &theme);
                let rect = chart.marks.frame.rect;
                let items = chart.marks.items();
                for node in &chart.marks.nodes {
                    assert!(
                        items.iter().any(|item| item.name == node.label),
                        "{width}: {:?} is named nowhere",
                        node.label
                    );
                }
                let (boxes, _) = chart.marks.geometry();
                let labels = chart.marks.label_layout(&boxes, &mut tr, &theme);
                let mut shown = 0;
                for label in &labels {
                    let Some(node) = chart.marks.nodes.get(label.index) else {
                        continue;
                    };
                    shown += 1;
                    let text = label
                        .lines
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert!(
                        reads_as_whole_words(&node.label, &text),
                        "{width}: {:?} reads as {text:?}",
                        node.label
                    );
                    let rects = label
                        .lines
                        .iter()
                        .map(|(_, rect)| *rect)
                        .chain(label.value.iter().map(|(_, rect)| *rect));
                    for at in rects {
                        assert!(
                            at.x0 >= rect.x0 - 0.5
                                && at.x1 <= rect.x1 + 0.5
                                && at.y0 >= rect.y0 - 0.5
                                && at.y1 <= rect.y1 + 0.5,
                            "{width}: {at:?} lies outside {rect:?}"
                        );
                    }
                }
                // A laptop's gap holds every long name over its lines; a phone's holds the ones
                // a whole word either side of an ellipsis still names, and drops the rest.
                if width > 500.0 {
                    assert_eq!(shown, 4, "{width}: every node of the flow is named");
                }
            }
        }
    }

    /// A name that fits beside its node reads whole on one line, a one-word name included: the
    /// breaks and the shortening are for names that do not fit, never for the ones that do.
    #[test]
    fn short_names_read_whole_on_one_line() {
        let nodes = vec![
            ChartNode::new("visits", "Visits"),
            ChartNode::new("signups", "Sign-ups"),
            ChartNode::new("trials", "Trials"),
            ChartNode::new("paid", "Paid"),
            ChartNode::new("churned", "Churned"),
        ];
        let links = vec![
            ChartLink::new("a", "visits", "signups", 72.0),
            ChartLink::new("b", "signups", "trials", 54.0),
            ChartLink::new("c", "trials", "paid", 31.0),
            ChartLink::new("d", "trials", "churned", 23.0),
        ];
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let mut chart = SankeyChart::new(nodes.clone(), links.clone());
                let height = chart.marks.natural_height(&mut tr, &theme, width);
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, height);
                chart.marks.place(&mut tr, &theme);
                let (boxes, _) = chart.marks.geometry();
                let labels = chart.marks.label_layout(&boxes, &mut tr, &theme);
                let mut named: Vec<String> = labels
                    .iter()
                    .map(|label| {
                        assert_eq!(label.lines.len(), 1, "{width}: a short name takes one line");
                        label.lines.first().map(|(text, _)| text.clone()).unwrap_or_default()
                    })
                    .collect();
                named.sort();
                assert_eq!(
                    named,
                    ["Churned", "Paid", "Sign-ups", "Trials", "Visits"],
                    "{width}: every node reads its whole name"
                );
            }
        }
    }

    /// The natural height grows with the lines the labels break over, so a column of many named
    /// nodes is drawn tall enough to read them whole rather than shortening or dropping them.
    #[test]
    fn the_natural_height_holds_a_column_of_named_nodes() {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let mut nodes = vec![ChartNode::new(
            "trial",
            "Started a trial on their own computer",
        )];
        let mut links = Vec::new();
        for index in 0..8 {
            let label = format!("Started a trial on their own computer {index}");
            nodes.push(ChartNode::new(format!("team-{index}"), label));
            links.push(ChartLink::new(
                format!("link-{index}"),
                "trial",
                format!("team-{index}"),
                2.0,
            ));
        }
        let mut chart = SankeyChart::new(nodes, links);
        let width = 1000.0;
        let height = chart.marks.natural_height(&mut tr, &theme, width);
        assert!(
            height > NATURAL_HEIGHT,
            "eight named nodes need more than the least height: {height}"
        );
        chart.marks.frame.rect = Rect::new(0.0, 0.0, width, height);
        chart.marks.place(&mut tr, &theme);
        let (boxes, _) = chart.marks.geometry();
        let labels = chart.marks.label_layout(&boxes, &mut tr, &theme);
        let named = labels
            .iter()
            .filter(|label| label.index != 0)
            .collect::<Vec<_>>();
        assert_eq!(
            named.len(),
            8,
            "the room the chart asked for names them all"
        );
        for label in named {
            let node = chart.marks.nodes.get(label.index);
            let text = label
                .lines
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(Some(text.as_str()), node.map(|node| node.label.as_str()));
        }
    }
}
