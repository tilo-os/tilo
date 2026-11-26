//! Interactive hierarchical sunburst charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "radial hierarchy projection is bounded floating-point geometry"
)]

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::ops::Range;

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::widget::chart_geometry;
use crate::widget::chart_labels::{ShownLabel, format_compact};
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextWeight};

#[derive(Clone, Debug)]
pub struct SunburstNode {
    pub id: String,
    pub label: String,
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
    label: String,
    detail: String,
    /// The labels above this one, from the root down: the segments this one is a sibling of
    /// share it.
    parent: String,
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

pub type SunburstChart = Plot<SunburstMarks>;

#[path = "sunburst_component.rs"]
mod component;
pub use component::SunburstChartProps;

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
        // A hovered segment reaches past the others by its offset, so the rings keep that margin.
        let rect = self.frame.inside();
        let center = rect.center();
        let outer = (rect.width().min(rect.height()) / 2.0 - self.style.hover_offset).max(1.0);
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
            label: node.label.clone(),
            detail: node.detail.clone(),
            parent: parent_path.to_owned(),
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

    /// Each segment as it is drawn: its band, lifted off the chart where the pointer is on it.
    fn drawn(&self) -> Vec<(Segment, Ring)> {
        let (base_center, inner, _, band) = self.geometry();
        self.segments()
            .into_iter()
            .map(|segment| {
                let half_gap =
                    (self.style.angular_gap / 2.0).min((segment.end - segment.start) * 0.2);
                let mid = segment.start.midpoint(segment.end);
                let lift = if self.frame.inspection.hovered.as_deref() == Some(segment.id.as_str())
                {
                    self.style.hover_offset
                } else {
                    0.0
                };
                let center = Point::new(
                    base_center.x + lift * mid.cos(),
                    base_center.y + lift * mid.sin(),
                );
                let ring = Ring {
                    center,
                    inner: inner
                        + f64::from(
                            u32::try_from(segment.depth.saturating_sub(1)).unwrap_or(u32::MAX),
                        ) * band
                        + self.style.level_gap / 2.0,
                    outer: inner
                        + f64::from(u32::try_from(segment.depth).unwrap_or(u32::MAX)) * band
                        - self.style.level_gap / 2.0,
                    start: segment.start + half_gap,
                    end: segment.end - half_gap,
                };
                (segment, ring)
            })
            .collect()
    }

    /// Each drawn segment's label, as its band shows it. Segments sharing a parent are named
    /// in turn, and a shortened reading one of them already shows is passed over for the next,
    /// so a ring never says the same words twice.
    fn shown_labels(
        &self,
        drawn: &[(Segment, Ring)],
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Option<ShownLabel>> {
        let mut shown = Vec::with_capacity(drawn.len());
        let mut taken: std::collections::HashMap<(usize, &str), Vec<String>> =
            std::collections::HashMap::new();
        for (segment, ring) in drawn {
            let key = (segment.depth, segment.parent.as_str());
            let label = {
                let used = taken.get(&key).map_or(&[][..], Vec::as_slice);
                ring_label(tr, theme, &segment.label, ring, used)
            };
            if let Some(label) = &label {
                taken.entry(key).or_default().push(label.text());
            }
            shown.push(label);
        }
        shown
    }

    fn palette(theme: &Theme, family: usize, depth: usize) -> Color {
        let colors = &theme.tokens.colors;
        let base = colors.presence.series(family);
        let target = colors.surfaces.base;
        let amount = (f64::from(u32::try_from(depth.saturating_sub(1)).unwrap_or(u32::MAX)) * 0.12)
            .min(0.48);
        tilo_ui_base::tokens::colors::mix8(base, target, tilo_ui_base::num::f32_of_f64(amount))
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

    fn place(&mut self, _tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = Margins::header(theme, &self.frame.title, 0);
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
        let (base_center, inner, _, _) = self.geometry();
        let drawn = self.drawn();
        let labels = self.shown_labels(&drawn, tr, theme);
        for (index, (segment, ring)) in drawn.iter().enumerate() {
            let color = segment
                .color
                .unwrap_or_else(|| Self::palette(theme, segment.family, segment.depth));
            let path = chart_geometry::ring_sector(
                ring.center,
                ring.inner,
                ring.outer,
                ring.start,
                ring.end,
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
            let Some(label) = labels.get(index).and_then(Option::as_ref) else {
                continue;
            };
            let ink = crate::widget::chart_labels::ink_on_fill(theme, color);
            for (line, at) in &label.lines {
                let turned = Affine::rotate_about(label.turn, at.center())
                    * Affine::translate((at.x0, at.y0));
                tr.draw_style(
                    scene,
                    line,
                    ink,
                    transform * turned,
                    None,
                    &label_style(theme),
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
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
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
            return Some(Readout::of_mark(
                &segment.path,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

/// One segment's band: the ring between two radii round a centre, over an angular span.
struct Ring {
    center: Point,
    inner: f64,
    outer: f64,
    start: f64,
    end: f64,
}

impl Ring {
    /// Whether `rect`, turned `turn` about its own centre, lies wholly inside the band, `air`
    /// clear of its edges: its outline is walked, since a straight edge can bulge past a curved
    /// one between its corners.
    fn holds(&self, rect: Rect, turn: f64, air: f64) -> bool {
        const STEPS: u32 = 8;
        let inner = self.inner + air;
        let outer = self.outer - air;
        if outer <= inner {
            return false;
        }
        let turned = Affine::rotate_about(turn, rect.center());
        let corners = [
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x1, rect.y0),
            Point::new(rect.x1, rect.y1),
            Point::new(rect.x0, rect.y1),
        ]
        .map(|corner| turned * corner);
        (0..4).all(|side| {
            let from = corners[side];
            let to = corners[(side + 1) % 4];
            (0..STEPS).all(|step| {
                let point = from.lerp(to, f64::from(step) / f64::from(STEPS));
                let dx = point.x - self.center.x;
                let dy = point.y - self.center.y;
                let radius = dx.hypot(dy);
                if radius < inner || radius > outer {
                    return false;
                }
                // The span may start anywhere; the angle is read from its start.
                let angle = (dy.atan2(dx) - self.start).rem_euclid(TAU);
                let clear = air / radius;
                angle >= clear && angle <= self.end - self.start - clear
            })
        })
    }
}

fn label_style(theme: &Theme) -> tilo_ui_base::text_render::TextStyle {
    theme
        .text(TextRole::Label)
        .with_weight(TextWeight::Semibold)
}

/// The turn a band's words take at `angle` round the ring: along the ring's tangent, never more
/// than a quarter turn from upright, so a label on the lower half is not read upside down.
fn tangent_turn(angle: f64) -> f64 {
    let mut turn = (angle + FRAC_PI_2).rem_euclid(PI);
    if turn > FRAC_PI_2 {
        turn -= PI;
    }
    turn
}

/// `label` as the band shows it, centred in the middle of it and reading along the ring: whole
/// where it fits, otherwise over two lines where the band is thick enough to hold them, otherwise
/// shortened to whole words of itself at either side of an ellipsis — a cut never splits a word —
/// and left out where none of those fits, since a stub names nothing. A ring's room runs along
/// the arc, so its words follow the arc: read flat, a long name cannot fit a band at all.
/// Pointing at the segment reads its whole path in the tooltip, and the accessible tree carries
/// it whole too.
fn ring_label(
    tr: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    ring: &Ring,
    taken: &[String],
) -> Option<ShownLabel> {
    if label.is_empty() || ring.end <= ring.start {
        return None;
    }
    let style = label_style(theme);
    let air = f64::from(theme.tokens.spacing.s1);
    let mid = ring.start.midpoint(ring.end);
    let radius = ring.inner.midpoint(ring.outer);
    let anchor = Point::new(
        ring.center.x + radius * mid.cos(),
        ring.center.y + radius * mid.sin(),
    );
    let turn = tangent_turn(mid);
    let at = |w: f64, h: f64| {
        Rect::new(
            anchor.x - w / 2.0,
            anchor.y - h / 2.0,
            anchor.x + w / 2.0,
            anchor.y + h / 2.0,
        )
    };
    let measure = |tr: &mut TextRenderer, text: &str| {
        let (w, h) = tr.measure_style(text, None, &style);
        (f64::from(w), f64::from(h))
    };
    let (w, h) = measure(tr, label);
    let whole = at(w, h);
    if ring.holds(whole, turn, air) {
        return Some(ShownLabel {
            lines: vec![(label.to_owned(), whole)],
            turn,
        });
    }
    if let Some(shown) = wrapped_label(tr, label, ring, &style, air) {
        return Some(shown);
    }
    for (text, w, h) in crate::widget::chart_labels::shortened_labels(tr, label, &style) {
        if taken.contains(&text) {
            continue;
        }
        let rect = at(w, h);
        if ring.holds(rect, turn, air) {
            return Some(ShownLabel {
                lines: vec![(text, rect)],
                turn,
            });
        }
    }
    None
}

/// `label` broken over two lines at the words' own breaks, the evenest pair the band holds
/// inside it, the second line across the band from the first. `None` where no pair fits.
fn wrapped_label(
    tr: &mut TextRenderer,
    label: &str,
    ring: &Ring,
    style: &tilo_ui_base::text_render::TextStyle,
    air: f64,
) -> Option<ShownLabel> {
    let mid = ring.start.midpoint(ring.end);
    let turn = tangent_turn(mid);
    let (sin, cos) = mid.sin_cos();
    let radius = ring.inner.midpoint(ring.outer);
    // The first line stands above the second as the words are read: away from the centre where
    // the turned words' up points outward (the upper half), towards it where it points inward.
    let outward_is_up = (turn - mid).sin() > 0.0;
    let down = if outward_is_up { -1.0 } else { 1.0 };
    for block in crate::widget::chart_labels::wrapped_labels(tr, label, style) {
        let mut top = -block.height / 2.0;
        let mut lines = Vec::with_capacity(block.lines.len());
        let mut fits = true;
        for (text, w, h) in block.lines {
            let across = down * (top + h / 2.0);
            let centre = Point::new(
                ring.center.x + (radius + across) * cos,
                ring.center.y + (radius + across) * sin,
            );
            let rect = Rect::new(
                centre.x - w / 2.0,
                centre.y - h / 2.0,
                centre.x + w / 2.0,
                centre.y + h / 2.0,
            );
            if !ring.holds(rect, turn, air) {
                fits = false;
                break;
            }
            lines.push((text, rect));
            top += h;
        }
        if fits {
            return Some(ShownLabel { lines, turn });
        }
    }
    None
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

    /// A week at work, as the gallery's first sunburst shows it.
    fn week_root() -> SunburstNode {
        SunburstNode::new("root", "Where the week went", 0.0).with_children(vec![
            SunburstNode::new("pipeline", "Release pipeline", 0.0).with_children(vec![
                SunburstNode::new("build", "Build", 18.0),
                SunburstNode::new("test", "Test", 26.0),
            ]),
            SunburstNode::new("review", "Review", 0.0).with_children(vec![
                SunburstNode::new("reviews", "Code review", 14.0),
                SunburstNode::new("design", "Design review", 6.0),
            ]),
            SunburstNode::new("docs", "Docs", 0.0).with_children(vec![SunburstNode::new(
                "notes",
                "Release notes",
                9.0,
            )]),
        ])
    }

    /// The gallery's second sunburst: names far longer than the bands that show them.
    fn long_names_root() -> SunburstNode {
        SunburstNode::new("root", "Where the long names went", 0.0).with_children(vec![
            SunburstNode::new("platform", "Platform work that ran long", 0.0).with_children(vec![
                SunburstNode::new(
                    "integration",
                    "Integration tests for the desktop clients",
                    22.0,
                ),
                SunburstNode::new("migration", "Migration of the old calendar records", 14.0),
            ]),
            SunburstNode::new("people", "Talking to people about the release", 0.0).with_children(
                vec![SunburstNode::new(
                    "interviews",
                    "Interviews with the support team",
                    7.0,
                )],
            ),
        ])
    }

    /// A band tall enough to hold two lines takes a long name over two lines whole, rather than
    /// cutting it: both lines sit inside the band, one over the other.
    #[test]
    fn a_long_label_wraps_where_the_band_holds_two_lines() {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let ring = Ring {
            center: Point::new(0.0, 0.0),
            inner: 200.0,
            outer: 400.0,
            start: 0.0,
            end: std::f64::consts::PI,
        };
        let air = f64::from(theme.tokens.spacing.s1);
        let shown = wrapped_label(
            &mut tr,
            "Integration tests for the desktop clients",
            &ring,
            &label_style(&theme),
            air,
        )
        .expect("a band this tall holds two lines");
        assert_eq!(shown.lines.len(), 2, "{:?}", shown.text());
        assert_eq!(shown.text(), "Integration tests for the desktop clients");
        for (_, at) in &shown.lines {
            assert!(
                ring.holds(*at, shown.turn, air),
                "{at:?} lies outside the band"
            );
        }
    }

    /// A label over two lines reads in order wherever its segment stands round the ring: the
    /// first line stands above the second as the turned words are read, on the upper half of the
    /// ring as on the lower.
    #[test]
    fn a_wrapped_label_reads_its_lines_in_order_all_round_the_ring() {
        use std::f64::consts::{FRAC_PI_4, PI};
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let air = f64::from(theme.tokens.spacing.s1);
        // Half a ring at a time: the top, each side, the bottom.
        for start in [
            -PI,
            -PI / 2.0 - FRAC_PI_4,
            -FRAC_PI_4,
            0.0,
            PI / 2.0 - FRAC_PI_4,
        ] {
            let ring = Ring {
                center: Point::new(0.0, 0.0),
                inner: 200.0,
                outer: 400.0,
                start,
                end: start + PI,
            };
            let shown = wrapped_label(
                &mut tr,
                "Interviews with the support team",
                &ring,
                &label_style(&theme),
                air,
            )
            .expect("a band this tall holds two lines");
            assert_eq!(shown.text(), "Interviews with the support team");
            let [(_, first), (_, second)] = shown.lines.as_slice() else {
                panic!("two lines: {:?}", shown.text());
            };
            // The turned words' up, and how far each line's centre stands along it.
            let up = (shown.turn.sin(), -shown.turn.cos());
            let along_up = |at: &Rect| at.center().x * up.0 + at.center().y * up.1;
            assert!(
                along_up(first) > along_up(second),
                "at {start:.2} the first line {first:?} stands below the second {second:?}"
            );
        }
    }

    /// Every label a segment shows names something: it is the segment's whole name, or whole
    /// words of it on either side of one ellipsis — a cut never splits a word — and two segments
    /// of one ring never show the same reading, so the words on screen tell the segments apart
    /// at a laptop's width and at a phone's. Where a label cannot be read, the segment is left
    /// unnamed, and its tooltip and accessible name still read its whole path.
    #[test]
    fn segment_labels_stay_readable_at_every_width() {
        for root in [week_root(), long_names_root()] {
            for theme in [Theme::light(), Theme::dark()] {
                let mut tr = TextRenderer::new();
                for width in [1000.0, 358.0] {
                    let mut chart = SunburstChart::new(root.clone());
                    chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 440.0);
                    chart.marks.place(&mut tr, &theme);
                    let drawn = chart.marks.drawn();
                    let labels = chart.marks.shown_labels(&drawn, &mut tr, &theme);
                    let items = chart.marks.items();
                    let mut shown = 0;
                    let mut by_siblings: std::collections::HashMap<(usize, &str), Vec<String>> =
                        std::collections::HashMap::new();
                    for ((segment, _), label) in drawn.iter().zip(&labels) {
                        // However short the band draws it, the whole name is still read by
                        // the tooltip and the accessible tree.
                        assert!(
                            items
                                .iter()
                                .any(|item| item.name.contains(segment.label.as_str())),
                            "{width}: {:?} is named nowhere",
                            segment.label
                        );
                        let Some(label) = label else {
                            continue;
                        };
                        shown += 1;
                        let text = label.text();
                        assert!(
                            crate::widget::chart_labels::reads_as_whole_words(
                                &segment.label,
                                &text
                            ),
                            "{width}: {:?} shows {text:?}, which is not whole words of it",
                            segment.label
                        );
                        by_siblings
                            .entry((segment.depth, segment.parent.as_str()))
                            .or_default()
                            .push(text);
                    }
                    assert!(shown >= 4, "{width}: the chart still names its segments");
                    for texts in by_siblings.values() {
                        let mut cuts: Vec<&String> =
                            texts.iter().filter(|text| text.contains('…')).collect();
                        let count = cuts.len();
                        cuts.sort();
                        cuts.dedup();
                        assert_eq!(
                            cuts.len(),
                            count,
                            "{width}: two segments of one ring show the same cut: {texts:?}"
                        );
                    }
                }
            }
        }
    }

    /// The centre total fits a phone's chart as it does a laptop's: the hole shrinks with the
    /// width, and the words take a smaller role rather than vanishing.
    #[test]
    fn the_centre_total_fits_a_phone_width_chart() {
        let root = SunburstNode::new("root", "Week", 0.0).with_children(vec![
            SunburstNode::new("a", "A", 2.0),
            SunburstNode::new("b", "B", 3.0),
        ]);
        for theme in [Theme::light(), Theme::dark()] {
            let mut tr = TextRenderer::new();
            for width in [1000.0, 358.0] {
                let mut chart = SunburstChart::new(root.clone()).with_center("73 hours", "");
                chart.marks.frame.rect = Rect::new(0.0, 0.0, width, 440.0);
                chart.marks.place(&mut tr, &theme);
                let (center, inner, _, _) = chart.marks.geometry();
                let label = crate::widget::chart_labels::centre_label_layout(
                    &mut tr,
                    &theme,
                    center,
                    (inner - 2.0).max(0.0),
                    "73 hours",
                    "",
                );
                assert!(
                    label.is_some(),
                    "{width}: the hole is wide enough to say the total"
                );
            }
        }
    }
}
