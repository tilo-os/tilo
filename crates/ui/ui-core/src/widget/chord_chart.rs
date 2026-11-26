//! Interactive weighted chord diagram.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded radial and Bézier geometry"
)]

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Shape, Stroke};
use vello::peniko::{Color, Fill};

use crate::paint::with_alpha;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::{ChartLink, ChartNode, graph_items};
use crate::widget::chart_geometry::{annulus, append_arc, polar};
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

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

pub type ChordSelection = crate::widget::chart_data::GraphSelection;
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

pub struct ChordMarks {
    nodes: Vec<ChartNode>,
    links: Vec<ChartLink>,
    style: ChordStyle,
    frame: PlotFrame<ChordSelection>,
    node_geometry: Vec<NodeGeom>,
    link_geometry: Vec<LinkGeom>,
    label_width: f64,
    /// The line a label takes over or under the ring.
    label_line: f64,
}

pub type ChordChart = Plot<ChordMarks>;

#[path = "chord_component.rs"]
mod component;
pub use component::ChordChartProps;

impl ChordChart {
    pub fn new(nodes: Vec<ChartNode>, links: Vec<ChartLink>) -> Self {
        Plot::from_marks(ChordMarks {
            nodes,
            links,
            style: ChordStyle::default(),
            frame: PlotFrame::new(360.0, 180.0),
            node_geometry: Vec::new(),
            link_geometry: Vec::new(),
            label_width: 0.0,
            label_line: 0.0,
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: ChordStyle) -> Self {
        self.marks.style = style;
        self
    }
    pub fn set_data(&mut self, nodes: Vec<ChartNode>, links: Vec<ChartLink>) {
        self.marks.nodes = nodes;
        self.marks.links = links;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, selection: &ChordSelection) -> bool {
        let known = match selection {
            ChordSelection::Node(id) => self.marks.nodes.iter().any(|node| node.id == *id),
            ChordSelection::Link(id) => self
                .marks
                .links
                .iter()
                .any(|link| link.id == *id && link.flows()),
        };
        self.inspect_item(known.then(|| selection.clone()))
    }
}

impl ChordMarks {
    fn geometry(&self) -> (Point, f64, f64) {
        let area = self.frame.inside();
        let side = self
            .label_width
            .max(self.style.label_offset + self.label_line);
        let outer = (area.width() / 2.0 - side)
            .min(area.height() / 2.0 - self.style.label_offset - self.label_line)
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

    /// The width the labels need beside the ring: the widest label, never
    /// more than a quarter of the chart, so the ring keeps its size.
    fn measure_label_width(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.label_line = crate::widget::plot::caption_line(theme);
        if !self.style.show_labels {
            self.label_width = 0.0;
            return;
        }
        let style = theme.text(TextRole::Caption);
        let widest = self
            .nodes
            .iter()
            .map(|node| f64::from(tr.measure_style(&node.label, None, &style).0))
            .fold(0.0, f64::max);
        let pad = f64::from(theme.tokens.spacing.s2);
        self.label_width = (widest + 2.0 * pad).min(self.frame.rect.width() / 4.0);
    }

    /// Each arc's label outside its arc, largest arc first: whole where the room before the
    /// chart's edge holds it, otherwise shortened at a word boundary keeping the words at its
    /// start and its end, and dropped (the tooltip and accessible name still read the arc) where
    /// no reading of it fits the room or clears a label already placed.
    fn label_layout(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        center: Point,
        outer: f64,
    ) -> Vec<(String, Rect)> {
        let style = theme.text(TextRole::Caption);
        let mut placer = crate::widget::chart_labels::LabelPlacer::new(self.frame.rect, theme);
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
        order.compact_sort_by(|a, b| (b.end - b.start).total_cmp(&(a.end - a.start)));
        let mut placed = Vec::new();
        for geom in order {
            let label = self.node(&geom.id).map_or("", |n| n.label.as_str());
            let label = if self.style.show_values {
                format!("{label} {}", format_compact(geom.value))
            } else {
                label.to_owned()
            };
            let mid = geom.start.midpoint(geom.end);
            let spare =
                crate::widget::chart_labels::radial_extent(center, radius, mid, self.frame.rect);
            let (whole_w, whole_h) = tr.measure_style(&label, None, &style);
            let readings = if f64::from(whole_w) <= spare {
                vec![(label, f64::from(whole_w), f64::from(whole_h))]
            } else {
                crate::widget::chart_labels::shortened_labels(tr, &label, &style)
                    .into_iter()
                    .filter(|(_, width, _)| *width <= spare)
                    .collect()
            };
            for (shown, w, h) in readings {
                let rect =
                    crate::widget::chart_labels::radial_label_rect(center, radius, mid, (w, h));
                if placer.place(rect) {
                    placed.push((shown, rect));
                    break;
                }
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
    fn hit_mark(&self, point: Point) -> Option<ChordSelection> {
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
    fn node(&self, id: &str) -> Option<&ChartNode> {
        self.nodes.iter().find(|node| node.id == id)
    }
    fn link(&self, id: &str) -> Option<&ChartLink> {
        self.links.iter().find(|link| link.id == id)
    }
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

impl PlotMarks for ChordMarks {
    type Hover = ChordSelection;
    type Activated = ChordSelection;
    const NAME: &'static str = "ChordChart";
    const LABEL: &'static str = "Chord diagram";

    fn frame(&self) -> &PlotFrame<ChordSelection> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<ChordSelection> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<ChordSelection> {
        self.hit_mark(point)
    }
    fn activation(&self, selection: &ChordSelection) -> Option<ChordSelection> {
        Some(selection.clone())
    }
    fn keyboard_items(&self) -> Vec<ChordSelection> {
        self.nodes
            .iter()
            .map(|node| ChordSelection::Node(node.id.clone()))
            .chain(
                self.links
                    .iter()
                    .filter(|link| link.flows())
                    .map(|link| ChordSelection::Link(link.id.clone())),
            )
            .collect()
    }
    fn anchor(&self, selection: &ChordSelection) -> Option<Point> {
        let (center, inner, outer) = self.geometry();
        Some(match selection {
            ChordSelection::Node(id) => self
                .node_geometry
                .iter()
                .find(|node| node.id == *id)
                .map_or(center, |node| {
                    polar(center, inner.midpoint(outer), node.start.midpoint(node.end))
                }),
            ChordSelection::Link(_) => center,
        })
    }
    fn items(&self) -> Vec<PlotItem<ChordSelection>> {
        graph_items(&self.nodes, &self.links, format_compact)
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = Margins::header(theme, &self.frame.title, 0);
        self.measure_label_width(tr, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "weighted allocation, ribbons, arcs, labels and inspector share chord geometry"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
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
            let hovered = self.frame.inspection.hovered.as_ref()
                == Some(&ChordSelection::Link(geom.id.clone()));
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
            let hovered = self.frame.inspection.hovered.as_ref()
                == Some(&ChordSelection::Node(geom.id.clone()));
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
        self.follow_keyboard_anchor();
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        if let Some(selection) = self
            .frame
            .inspection
            .hovered
            .as_ref()
            .filter(|_| self.style.tooltip)
        {
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
            return Some(Readout::of_mark(
                &label,
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

    /// The gallery's long chord: arc names far wider than the room outside the ring.
    fn weekly_report() -> (Vec<ChartNode>, Vec<ChartLink>) {
        (
            vec![
                ChartNode::new("ingest", "Ingest the raw readings from the field"),
                ChartNode::new("clean", "Clean and join them with the customer list"),
                ChartNode::new("model", "Model the weekly totals for the report"),
                ChartNode::new("review", "Review the numbers before they go out"),
            ],
            vec![
                ChartLink::new("one", "ingest", "clean", 44.0),
                ChartLink::new("two", "clean", "model", 37.0),
                ChartLink::new("three", "model", "review", 29.0),
                ChartLink::new("four", "review", "ingest", 6.0),
            ],
        )
    }

    /// Every arc's label stays readable at a laptop's width and a phone's: it is the arc's whole
    /// name, or whole words at its start and its end around one ellipsis, never a cut inside a
    /// word. An arc left unnamed is still read whole by its tooltip and accessible name, and no
    /// label paints outside the chart.
    #[test]
    fn arc_labels_stay_readable_at_every_width() {
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let (nodes, links) = weekly_report();
                let mut chart = ChordChart::new(nodes, links);
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 360.0);
                chart.marks.place(&mut tr, &theme);
                // The arcs are laid out as they paint, so paint them before reading the labels.
                let mut scene = Scene::new();
                chart
                    .marks
                    .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);
                let (center, _, outer) = chart.marks.geometry();
                let labels = chart.marks.label_layout(&mut tr, &theme, center, outer);
                let rect = chart.marks.frame.rect;
                let items = chart.marks.items();
                for node in &chart.marks.nodes {
                    assert!(
                        items.iter().any(|item| item.name == node.label),
                        "{width}: {:?} is named nowhere",
                        node.label
                    );
                }
                let mut shown = 0;
                for (text, at) in &labels {
                    shown += 1;
                    assert!(
                        chart
                            .marks
                            .nodes
                            .iter()
                            .any(|node| reads_as_whole_words(&node.label, text)),
                        "{width}: {text:?} names no arc whole"
                    );
                    assert!(
                        at.x0 >= rect.x0 - 0.5
                            && at.x1 <= rect.x1 + 0.5
                            && at.y0 >= rect.y0 - 0.5
                            && at.y1 <= rect.y1 + 0.5,
                        "{width}: {at:?} lies outside {rect:?}"
                    );
                }
                if width > 500.0 {
                    assert!(shown >= 2, "{width}: only {shown} arcs are named");
                }
            }
        }
    }
}
