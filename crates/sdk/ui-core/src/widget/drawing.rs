//! [`Drawing`] — shapes drawn with the design system (docs/designs/app-
//! language/components.md §4.1, the custom-widget ladder's second step).

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{
    Affine, Arc, BezPath, Ellipse, Line, Point, Rect, RoundedRect, Shape as _, Stroke,
};
use vello::peniko::{Color, Fill, Gradient};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::motion::{Direction, Feel, MotionValue, Reading};
use crate::paint::with_alpha_scaled;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{EffectiveMotion, MotionSource, TextRole};
use crate::widget::{TaffyTreeExt, Tone, Widget};

/// A colour of the design system a shape is painted in: a role of the person's theme, never a
/// colour of the app's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ink {
    #[default]
    Accent,
    Ink,
    Muted,
    Track,
    Success,
    Warning,
    Danger,
    Info,
}

impl Ink {
    /// The colour the theme gives the role.
    #[must_use]
    pub fn colour(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Accent => colors.intent.accent,
            Self::Ink => colors.text_primary,
            Self::Muted => colors.text_dim,
            Self::Track => with_alpha_scaled(colors.text_primary, 0.12),
            Self::Success => Tone::Success.colour(theme),
            Self::Warning => Tone::Warning.colour(theme),
            Self::Danger => Tone::Danger.colour(theme),
            Self::Info => Tone::Info.colour(theme),
        }
    }
}

crate::component::choice!(Ink, "Ink", {
    Accent => "accent", Ink => "ink", Muted => "muted", Track => "track",
    Success => "success", Warning => "warning", Danger => "danger", Info => "info",
});

/// How heavy a stroke is: a token of the design system, never pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Weight {
    Hairline,
    #[default]
    Regular,
    Strong,
}

impl Weight {
    /// The stroke's width in logical pixels, from the theme's border tokens.
    #[must_use]
    pub fn width(self, theme: &Theme) -> f64 {
        let base = f64::from(theme.tokens.border.width).max(1.0);
        match self {
            Self::Hairline => base,
            Self::Regular => base * 3.0,
            Self::Strong => base * 6.0,
        }
    }
}

crate::component::choice!(Weight, "Weight", {
    Hairline => "hairline", Regular => "regular", Strong => "strong",
});

/// What a drawing means, for assistive technology and agents: required, so a drawing is
/// never only pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Meaning {
    /// Shows how far something has got; its `value` is the fraction.
    Progress,
    Chart,
    Picture,
    /// Only looks: nothing to announce.
    #[default]
    Decoration,
}

crate::component::choice!(Meaning, "Meaning", {
    Progress => "progress", Chart => "chart", Picture => "picture", Decoration => "decoration",
});

/// Which side of a text stands at its point: its start, its middle or its end. The text is centred
/// on the point's height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    Start,
    #[default]
    Middle,
    End,
}

crate::component::choice!(Anchor, "Anchor", {
    Start => "start", Middle => "middle", End => "end",
});

/// One shape of a [`Drawing`].
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A rectangle, rounded by `round` (a fraction of its shorter side, `0.5` a pill).
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        round: f64,
        fill: Option<Ink>,
        stroke: Option<Ink>,
        weight: Weight,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        fill: Option<Ink>,
        stroke: Option<Ink>,
        weight: Weight,
    },
    Arc {
        cx: f64,
        cy: f64,
        radius: f64,
        from: f64,
        to: f64,
        stroke: Ink,
        weight: Weight,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        stroke: Ink,
        weight: Weight,
    },
    Path {
        points: Vec<f64>,
        smooth: bool,
        closed: bool,
        fill: Option<Ink>,
        stroke: Option<Ink>,
        weight: Weight,
        wash: bool,
    },
    /// A text at (x, y), set in one of the theme's type roles.
    Text {
        x: f64,
        y: f64,
        text: String,
        role: TextRole,
        ink: Ink,
        anchor: Anchor,
        strong: bool,
    },
    /// A rectangle filled in a colour the content brings (`#rrggbb`, `#rrggbbaa`): a highlight,
    /// a selection or a find hit over a page. Its colour is the content's, never the theme's, so
    /// what a reader marked there shows as the person marked it in every theme.
    Area {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        round: f64,
        colour: String,
    },
    /// A line through `points` (x and y in turn), `width` of the box's shorter side thick, in a
    /// colour the content brings: an ink stroke, an underline, a signature.
    Stroke {
        points: Vec<f64>,
        colour: String,
        width: f64,
    },
    /// Words at (x, y) in a colour the content brings, `size` of the box's height: a page's own
    /// text, a signature's name.
    Label {
        x: f64,
        y: f64,
        text: String,
        colour: String,
        size: f64,
        anchor: Anchor,
    },
}

/// The colour a shape's own text names (`#rrggbb` or `#rrggbbaa`); black for one nothing reads.
#[must_use]
pub fn colour_of(text: &str) -> Color {
    let hex = text.trim().trim_start_matches('#');
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2).unwrap_or("00"), 16).unwrap_or(0);
    if hex.len() < 6 {
        return Color::from_rgba8(0, 0, 0, 255);
    }
    let alpha = if hex.len() >= 8 { byte(6) } else { 255 };
    Color::from_rgba8(byte(0), byte(2), byte(4), alpha)
}

enum Lowered {
    Area(BezPath),
    Outline(BezPath),
}

impl Shape {
    /// The shape's path in `bounds`, and whether its inside counts for a press (an area) or
    /// only its line does (an outline). A text has no path: it is set, not drawn.
    fn lowered(&self, bounds: Rect) -> Option<Lowered> {
        let at = |x: f64, y: f64| {
            Point::new(
                bounds.x0 + x * bounds.width(),
                bounds.y0 + y * bounds.height(),
            )
        };
        let side = bounds.width().min(bounds.height());
        match self {
            Self::Rect {
                x, y, w, h, round, ..
            } => {
                let rect = Rect::from_points(at(*x, *y), at(x + w, y + h));
                let radius = round.clamp(0.0, 0.5) * rect.width().min(rect.height());
                Some(Lowered::Area(
                    RoundedRect::from_rect(rect, radius).to_path(0.1),
                ))
            }
            Self::Ellipse { cx, cy, rx, ry, .. } => Some(Lowered::Area(
                Ellipse::new(
                    at(*cx, *cy),
                    (rx * bounds.width(), ry * bounds.height()),
                    0.0,
                )
                .to_path(0.1),
            )),
            Self::Arc {
                cx,
                cy,
                radius,
                from,
                to,
                ..
            } => {
                let turn = std::f64::consts::TAU;
                let start = from * turn - std::f64::consts::FRAC_PI_2;
                let sweep = (to - from) * turn;
                let r = radius * side;
                Some(Lowered::Outline(
                    Arc::new(at(*cx, *cy), (r, r), start, sweep, 0.0).to_path(0.1),
                ))
            }
            Self::Line { x1, y1, x2, y2, .. } => Some(Lowered::Outline(
                Line::new(at(*x1, *y1), at(*x2, *y2)).to_path(0.1),
            )),
            Self::Area {
                x, y, w, h, round, ..
            } => {
                let rect = Rect::from_points(at(*x, *y), at(x + w, y + h));
                let radius = round.clamp(0.0, 0.5) * rect.width().min(rect.height());
                Some(Lowered::Area(
                    RoundedRect::from_rect(rect, radius).to_path(0.1),
                ))
            }
            Self::Stroke { points, .. } => {
                let pts: Vec<Point> = points.chunks_exact(2).map(|p| at(p[0], p[1])).collect();
                if pts.len() < 2 {
                    return None;
                }
                let mut path = BezPath::new();
                path.move_to(pts[0]);
                for pair in pts.windows(2) {
                    path.line_to(pair[1]);
                }
                Some(Lowered::Outline(path))
            }
            // Only the path's own lowered form, never the arms before it: this one comes after
            // the text's, so a shape is lowered once.
            Self::Path {
                points,
                smooth,
                closed,
                fill,
                ..
            } => {
                let mut path = BezPath::new();
                let pts: Vec<Point> = points.chunks_exact(2).map(|p| at(p[0], p[1])).collect();
                if let Some(first) = pts.first() {
                    path.move_to(*first);
                    for pair in pts.windows(2) {
                        if *smooth {
                            let mid = pair[0].midpoint(pair[1]);
                            path.quad_to(pair[0], mid);
                        } else {
                            path.line_to(pair[1]);
                        }
                    }
                    if *smooth && let Some(last) = pts.last() {
                        path.line_to(*last);
                    }
                    if *closed {
                        path.close_path();
                    }
                }
                Some(if fill.is_some() || *closed {
                    Lowered::Area(path)
                } else {
                    Lowered::Outline(path)
                })
            }
            Self::Text { .. } | Self::Label { .. } => None,
        }
    }

    /// The shape's numbers, in a fixed order: what moves when a reading changes.
    fn numbers(&self, out: &mut Vec<f64>) {
        match self {
            Self::Rect {
                x, y, w, h, round, ..
            } => out.extend([*x, *y, *w, *h, *round]),
            Self::Ellipse { cx, cy, rx, ry, .. } => out.extend([*cx, *cy, *rx, *ry]),
            Self::Arc {
                cx,
                cy,
                radius,
                from,
                to,
                ..
            } => out.extend([*cx, *cy, *radius, *from, *to]),
            Self::Line { x1, y1, x2, y2, .. } => out.extend([*x1, *y1, *x2, *y2]),
            Self::Path { points, .. } => out.extend(points.iter().copied()),
            Self::Text { x, y, .. } | Self::Label { x, y, .. } => out.extend([*x, *y]),
            Self::Area {
                x, y, w, h, round, ..
            } => out.extend([*x, *y, *w, *h, *round]),
            Self::Stroke { points, .. } => out.extend(points.iter().copied()),
        }
    }

    /// The shape with its numbers taken, in [`Self::numbers`]' order, from `from`.
    fn take_numbers(&mut self, from: &mut impl Iterator<Item = f64>) {
        let mut next = |slot: &mut f64| {
            if let Some(value) = from.next() {
                *slot = value;
            }
        };
        match self {
            Self::Rect {
                x, y, w, h, round, ..
            } => {
                for slot in [x, y, w, h, round] {
                    next(slot);
                }
            }
            Self::Ellipse { cx, cy, rx, ry, .. }
            | Self::Line {
                x1: cx,
                y1: cy,
                x2: rx,
                y2: ry,
                ..
            } => {
                for slot in [cx, cy, rx, ry] {
                    next(slot);
                }
            }
            Self::Arc {
                cx,
                cy,
                radius,
                from: start,
                to,
                ..
            } => {
                for slot in [cx, cy, radius, start, to] {
                    next(slot);
                }
            }
            Self::Path { points, .. } => points.iter_mut().for_each(next),
            Self::Text { x, y, .. } | Self::Label { x, y, .. } => {
                for slot in [x, y] {
                    next(slot);
                }
            }
            Self::Area {
                x, y, w, h, round, ..
            } => {
                for slot in [x, y, w, h, round] {
                    next(slot);
                }
            }
            Self::Stroke { points, .. } => points.iter_mut().for_each(next),
        }
    }

    /// Whether `other` is this shape with other numbers: the same case, and a path through as many
    /// points. Only such a change moves; any other lands at once.
    fn same_form(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Path { points: a, .. }, Self::Path { points: b, .. }) => a.len() == b.len(),
            (Self::Stroke { points: a, .. }, Self::Stroke { points: b, .. }) => a.len() == b.len(),
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }

    fn fill(&self) -> Option<Ink> {
        match self {
            Self::Rect { fill, .. } | Self::Ellipse { fill, .. } | Self::Path { fill, .. } => *fill,
            Self::Arc { .. }
            | Self::Line { .. }
            | Self::Text { .. }
            | Self::Area { .. }
            | Self::Stroke { .. }
            | Self::Label { .. } => None,
        }
    }

    fn stroke(&self) -> Option<(Ink, Weight)> {
        match self {
            Self::Rect { stroke, weight, .. }
            | Self::Ellipse { stroke, weight, .. }
            | Self::Path { stroke, weight, .. } => stroke.map(|ink| (ink, *weight)),
            Self::Arc { stroke, weight, .. } | Self::Line { stroke, weight, .. } => {
                Some((*stroke, *weight))
            }
            Self::Text { .. } | Self::Area { .. } | Self::Stroke { .. } | Self::Label { .. } => None,
        }
    }

    fn washed(&self) -> bool {
        matches!(self, Self::Path { wash: true, .. })
    }
}

/// Shapes drawn with the design system over a box of their own.
#[derive(Default)]
pub struct Drawing {
    /// The shapes of the latest reading: what the drawing comes to rest at.
    pub(crate) shapes: Vec<Shape>,
    pub(crate) meaning: Meaning,
    pub(crate) label: Option<String>,
    pub(crate) value: Option<f64>,
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// How emphatic its motion is, over the theme's.
    pub(crate) feel: Feel,
    rect: Rect,
    last_cursor: Point,
    /// Each number of [`Self::shapes`] on its way from the reading before, while one moves: empty
    /// at rest, when the shapes are drawn as they are.
    moving: Vec<MotionValue<f64>>,
    /// The motion the person allows, as the last frame drew it: `None` until then, so the first
    /// reading is drawn where it is, never grown into.
    motion: Option<EffectiveMotion>,
    /// Where each text shape was set on the last frame, by its index, for what is under the pointer.
    text_boxes: Vec<Option<Rect>>,
    /// The shape the pointer is over, as last reported.
    hovered: Option<usize>,
    dirty: bool,
    pub(crate) layout_dirty: bool,
    pub(crate) on_press: Option<Box<dyn FnMut(usize)>>,
    pub(crate) on_hover: Option<Box<dyn FnMut(Option<usize>)>>,
    /// The pointer pressed on the drawing, moved while pressed and let go: each is handed the
    /// point as fractions of the box, x first (`drawing_point`).
    pub(crate) on_press_at: Option<Box<dyn FnMut(f64, f64)>>,
    pub(crate) on_drag_at: Option<Box<dyn FnMut(f64, f64)>>,
    pub(crate) on_release_at: Option<Box<dyn FnMut(f64, f64)>>,
    /// Whether the pointer is down on the drawing, so a move is a drag.
    pressed: bool,
}

impl Drawing {
    /// `p` as the drawing's own point: fractions of its box, x then y, from its top-left
    /// corner. A point outside the box is told as it is, so a drag that leaves the box keeps
    /// following the pointer.
    fn point_of(&self, p: Point) -> (f64, f64) {
        let w = self.rect.width();
        let h = self.rect.height();
        if w <= 0.0 || h <= 0.0 {
            return (0.0, 0.0);
        }
        ((p.x - self.rect.x0) / w, (p.y - self.rect.y0) / h)
    }

    /// The index of the topmost shape at `point`: inside an area or a text, or within reach of an
    /// outline's line.
    #[must_use]
    pub fn shape_at(&self, point: Point, theme_width: f64) -> Option<usize> {
        let reach = theme_width.max(6.0);
        self.shapes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, shape)| {
                let hit = match shape.lowered(self.rect) {
                    Some(Lowered::Area(path)) => path.contains(point),
                    Some(Lowered::Outline(path)) => {
                        path.bounding_box().inflate(reach, reach).contains(point)
                    }
                    None => self
                        .text_boxes
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|text| text.contains(point)),
                };
                hit.then_some(index)
            })
    }

    /// Take a new reading: a shape whose numbers changed moves from where it is drawn to them on the
    /// platform's motion, and any other change lands at once.
    pub fn set_shapes(&mut self, shapes: Vec<Shape>) {
        if shapes == self.shapes {
            return;
        }
        self.dirty = true;
        let same_form = shapes.len() == self.shapes.len()
            && shapes.iter().zip(&self.shapes).all(|(a, b)| a.same_form(b));
        let Some(motion) = self.motion.filter(|_| same_form) else {
            self.moving.clear();
            self.shapes = shapes;
            return;
        };
        let now = crate::motion::frame_now();
        let source = crate::motion::recent_input(now).unwrap_or(MotionSource::Pointer);
        let timing = Reading::CHANGE.timing(Direction::Enter, self.feel.over(motion), source);
        if self.moving.is_empty() {
            let mut at = Vec::new();
            for shape in &self.shapes {
                shape.numbers(&mut at);
            }
            self.moving = at.into_iter().map(MotionValue::new).collect();
        }
        let mut to = Vec::new();
        for shape in &shapes {
            shape.numbers(&mut to);
        }
        for (number, target) in self.moving.iter_mut().zip(to) {
            number.set(target, timing, now);
        }
        self.shapes = shapes;
    }

    /// Whether a number is still on its way to the latest reading.
    fn is_moving(&self, now: crate::time::Instant) -> bool {
        self.moving.iter().any(|number| number.is_moving(now))
    }

    /// The shapes as drawn at `now`: on their way while a reading moves, else as they are.
    fn shapes_at(&self, now: crate::time::Instant) -> Option<Vec<Shape>> {
        if !self.is_moving(now) {
            return None;
        }
        let mut numbers = self.moving.iter().map(|number| number.get(now));
        let mut shown = self.shapes.clone();
        for shape in &mut shown {
            shape.take_numbers(&mut numbers);
        }
        Some(shown)
    }

    /// Tell the view which shape the pointer is over now, when that changed.
    fn hover(&mut self, index: Option<usize>) {
        if index == self.hovered {
            return;
        }
        self.hovered = index;
        if let Some(hover) = self.on_hover.as_mut() {
            hover(index);
        }
    }

    /// Paint `shapes` into `bounds` with `theme`, and say where each text was set.
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        bounds: Rect,
        shapes: &[Shape],
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Vec<Option<Rect>> {
        shapes
            .iter()
            .map(|shape| {
                if let Shape::Text {
                    x,
                    y,
                    text,
                    role,
                    ink,
                    anchor,
                    strong,
                } = shape
                {
                    let mut style = theme.text(*role);
                    if *strong {
                        style = style.strong();
                    }
                    let (width, height) = text_renderer.measure_style(text, None, &style);
                    let (width, height) = (f64::from(width), f64::from(height));
                    let px = bounds.x0 + x * bounds.width();
                    let left = match anchor {
                        Anchor::Start => px,
                        Anchor::Middle => px - width / 2.0,
                        Anchor::End => px - width,
                    };
                    let top = bounds.y0 + y * bounds.height() - height / 2.0;
                    text_renderer.draw_style(
                        scene,
                        text,
                        ink.colour(theme),
                        transform.pre_translate((left, top).into()),
                        None,
                        &style,
                    );
                    return Some(Rect::new(left, top, left + width, top + height));
                }
                // Words in a colour the content brings are set where the shape says, at its
                // own size, and are no path: a page's text, a signature's name.
                if let Shape::Label {
                    x,
                    y,
                    text,
                    colour,
                    size,
                    anchor,
                } = shape
                {
                    let mut style = theme.text(TextRole::Body);
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "a size inside a drawing's box, far below f32's range"
                    )]
                    let set = (size * bounds.height()) as f32;
                    style.size = set;
                    style.line_height = set * 1.2;
                    let (width, height) = text_renderer.measure_style(text, None, &style);
                    let (width, height) = (f64::from(width), f64::from(height));
                    let px = bounds.x0 + x * bounds.width();
                    let left = match anchor {
                        Anchor::Start => px,
                        Anchor::Middle => px - width / 2.0,
                        Anchor::End => px - width,
                    };
                    let top = bounds.y0 + y * bounds.height() - height / 2.0;
                    text_renderer.draw_style(
                        scene,
                        text,
                        colour_of(colour),
                        transform.pre_translate((left, top).into()),
                        None,
                        &style,
                    );
                    return Some(Rect::new(left, top, left + width, top + height));
                }
                let Some(Lowered::Area(path) | Lowered::Outline(path)) = shape.lowered(bounds)
                else {
                    return None;
                };
                // A shape whose colour the content brings is filled and stroked in it, and one
                // of the theme's inks otherwise.
                if let Shape::Area { colour, .. } = shape {
                    scene.fill(Fill::NonZero, transform, colour_of(colour), None, &path);
                    return None;
                }
                if let Shape::Stroke { colour, width, .. } = shape {
                    let stroke = Stroke::new(width.clamp(0.0, 1.0) * bounds.width().min(bounds.height()))
                        .with_caps(vello::kurbo::Cap::Round)
                        .with_join(vello::kurbo::Join::Round);
                    scene.stroke(&stroke, transform, colour_of(colour), None, &path);
                    return None;
                }
                if let Some(ink) = shape.fill() {
                    let colour = ink.colour(theme);
                    if shape.washed() {
                        let top = path.bounding_box();
                        let wash = Gradient::new_linear(
                            Point::new(top.x0, top.y0),
                            Point::new(top.x0, bounds.y1),
                        )
                        .with_stops([
                            (0.0_f32, colour.multiply_alpha(0.35)),
                            (1.0_f32, colour.multiply_alpha(0.0)),
                        ]);
                        let area = washed_area(&path, shape, bounds);
                        scene.fill(Fill::NonZero, transform, &wash, None, &area);
                    } else {
                        scene.fill(Fill::NonZero, transform, colour, None, &path);
                    }
                }
                if let Some((ink, weight)) = shape.stroke() {
                    let stroke = Stroke::new(weight.width(theme))
                        .with_caps(vello::kurbo::Cap::Round)
                        .with_join(vello::kurbo::Join::Round);
                    scene.stroke(&stroke, transform, ink.colour(theme), None, &path);
                }
                None
            })
            .collect()
    }
}

impl Widget for Drawing {
    /// What it draws: a picture, named by its label.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("span");
        cx.picture();
    }

    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_dirty = false;
        let style = Style {
            size: Size::from_lengths(self.width, self.height),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        self.motion = Some(theme.motion());
        let moved = self.shapes_at(now);
        if moved.is_none() {
            self.moving.clear();
        }
        let shapes = moved.as_deref().unwrap_or(&self.shapes);
        self.text_boxes = Self::paint(scene, transform, self.rect, shapes, theme, text_renderer);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.is_moving(crate::motion::frame_now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Frames only while a reading moves: a drawing at rest asks for none.
    fn needs_animation_frame(&self) -> bool {
        self.is_moving(crate::motion::frame_now())
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.pressed
                    && let Some(drag) = self.on_drag_at.as_mut()
                {
                    let (x, y) = self.point_of(self.last_cursor);
                    drag(x, y);
                    *ctx.handled = true;
                }
                if self.on_hover.is_some() {
                    let under = if self.rect.contains(self.last_cursor) {
                        self.shape_at(self.last_cursor, 1.0)
                    } else {
                        None
                    };
                    self.hover(under);
                }
            }
            WindowEvent::CursorLeft { .. } => self.hover(None),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                self.pressed = true;
                if let Some(press) = self.on_press_at.as_mut() {
                    let (x, y) = self.point_of(self.last_cursor);
                    press(x, y);
                    *ctx.handled = true;
                }
                if let Some(index) = self.shape_at(self.last_cursor, 1.0)
                    && let Some(press) = self.on_press.as_mut()
                {
                    press(index);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.pressed
                    && let Some(release) = self.on_release_at.as_mut()
                {
                    let (x, y) = self.point_of(self.last_cursor);
                    release(x, y);
                    *ctx.handled = true;
                }
                self.pressed = false;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Drawing"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(match self.meaning {
            Meaning::Progress => AccessNode::new(Role::ProgressBar)
                .with_optional_name(self.label.clone())
                .with_optional_value(self.value.map(percent)),
            Meaning::Chart | Meaning::Picture => {
                AccessNode::new(Role::Image).with_optional_name(self.label.clone())
            }
            Meaning::Decoration => return None,
        })
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }
}

/// What a washed path fills: a closed path itself, and under an open one the area down to the
/// drawing's foot, so a trend's line and its wash are one shape and the line keeps its curve.
fn washed_area(path: &BezPath, shape: &Shape, bounds: Rect) -> BezPath {
    if matches!(shape, Shape::Path { closed: true, .. }) {
        return path.clone();
    }
    let (Some(first), Some(last)) = (path.elements().first(), path.elements().last()) else {
        return path.clone();
    };
    let (Some(start), Some(end)) = (first.end_point(), last.end_point()) else {
        return path.clone();
    };
    let mut area = path.clone();
    area.line_to(Point::new(end.x, bounds.y1));
    area.line_to(Point::new(start.x, bounds.y1));
    area.close_path();
    area
}

/// A progress fraction as it is announced: `0.6` is "60%".
fn percent(value: f64) -> String {
    format!("{:.0}%", (value.clamp(0.0, 1.0) * 100.0).round())
}

mod drawing_component;
pub use drawing_component::{DrawingProps, shape_description};

#[cfg(test)]
mod tests;
