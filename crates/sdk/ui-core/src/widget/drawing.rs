//! [`Drawing`] — shapes drawn with the design system (docs/designs/app-language/components.md
//! §4.1, the custom-widget ladder's second step).
//!
//! A drawing is a list of [`Shape`] values over its own box: coordinates are fractions of the
//! box, colours are theme roles ([`Ink`]), stroke widths are tokens ([`Weight`]). It lowers to
//! the same `kurbo` paths, fills and strokes [`super::Sparkline`] paints with, resolved against
//! the theme each frame, so every theme reaches it. It carries a semantic role so assistive
//! technology and agents read what it means rather than its shapes, and a press reports the
//! topmost shape under the pointer as data.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{
    Affine, Arc, BezPath, Ellipse, Line, Point, Rect, RoundedRect, Shape as _, Stroke,
};
use vello::peniko::{Color, Fill, Gradient};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::paint::with_alpha_scaled;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Tone, Widget};

/// A colour of the design system a shape is painted in: a role of the person's theme, never a
/// colour of the app's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ink {
    /// The theme's accent: what the drawing is about.
    #[default]
    Accent,
    /// Primary text ink.
    Ink,
    /// Secondary text ink.
    Muted,
    /// The quiet ground a value stands on: a progress ring's track.
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
    /// Shows data; its `label` summarises it.
    Chart,
    /// A picture of something; its `label` says what.
    Picture,
    /// Only looks: nothing to announce.
    #[default]
    Decoration,
}

crate::component::choice!(Meaning, "Meaning", {
    Progress => "progress", Chart => "chart", Picture => "picture", Decoration => "decoration",
});

/// One shape of a [`Drawing`]. Coordinates and sizes are fractions of the drawing's box (`0.5`
/// is its middle); angles of an arc are fractions of a turn, from the top, clockwise.
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
    /// An ellipse around `(cx, cy)`.
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        fill: Option<Ink>,
        stroke: Option<Ink>,
        weight: Weight,
    },
    /// An arc of the circle around `(cx, cy)`, from `from` to `to`.
    Arc {
        cx: f64,
        cy: f64,
        radius: f64,
        from: f64,
        to: f64,
        stroke: Ink,
        weight: Weight,
    },
    /// A line from `(x1, y1)` to `(x2, y2)`.
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        stroke: Ink,
        weight: Weight,
    },
    /// A path through `points` (x, y pairs), straight or `smooth`, open or `closed`; `wash`
    /// fills under it with its ink fading out downward.
    Path {
        points: Vec<f64>,
        smooth: bool,
        closed: bool,
        fill: Option<Ink>,
        stroke: Option<Ink>,
        weight: Weight,
        wash: bool,
    },
}

/// A [`Shape`] lowered into the drawing's box, in logical pixels.
enum Lowered {
    Area(BezPath),
    Outline(BezPath),
}

impl Shape {
    /// The shape's path in `bounds`, and whether its inside counts for a press (an area) or
    /// only its line does (an outline).
    fn lowered(&self, bounds: Rect) -> Lowered {
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
                Lowered::Area(RoundedRect::from_rect(rect, radius).to_path(0.1))
            }
            Self::Ellipse { cx, cy, rx, ry, .. } => Lowered::Area(
                Ellipse::new(
                    at(*cx, *cy),
                    (rx * bounds.width(), ry * bounds.height()),
                    0.0,
                )
                .to_path(0.1),
            ),
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
                Lowered::Outline(Arc::new(at(*cx, *cy), (r, r), start, sweep, 0.0).to_path(0.1))
            }
            Self::Line { x1, y1, x2, y2, .. } => {
                Lowered::Outline(Line::new(at(*x1, *y1), at(*x2, *y2)).to_path(0.1))
            }
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
                if fill.is_some() || *closed {
                    Lowered::Area(path)
                } else {
                    Lowered::Outline(path)
                }
            }
        }
    }

    fn fill(&self) -> Option<Ink> {
        match self {
            Self::Rect { fill, .. } | Self::Ellipse { fill, .. } | Self::Path { fill, .. } => *fill,
            Self::Arc { .. } | Self::Line { .. } => None,
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
        }
    }

    fn washed(&self) -> bool {
        matches!(self, Self::Path { wash: true, .. })
    }
}

/// Shapes drawn with the design system over a box of their own.
#[derive(Default)]
pub struct Drawing {
    pub(crate) shapes: Vec<Shape>,
    pub(crate) meaning: Meaning,
    /// What it shows, as assistive technology announces it.
    pub(crate) label: Option<String>,
    /// How far it has got, for a progress drawing.
    pub(crate) value: Option<f64>,
    pub(crate) width: f32,
    pub(crate) height: f32,
    rect: Rect,
    last_cursor: Point,
    /// What a press on a shape fires: its index, topmost first.
    pub(crate) on_press: Option<Box<dyn FnMut(usize)>>,
}

impl Drawing {
    /// The index of the topmost shape at `point`: inside an area, or within reach of an
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
                    Lowered::Area(path) => path.contains(point),
                    Lowered::Outline(path) => {
                        path.bounding_box().inflate(reach, reach).contains(point)
                    }
                };
                hit.then_some(index)
            })
    }

    /// Paint `shapes` into `bounds` with `theme`.
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        bounds: Rect,
        shapes: &[Shape],
        theme: &Theme,
    ) {
        for shape in shapes {
            let path = match shape.lowered(bounds) {
                Lowered::Area(path) | Lowered::Outline(path) => path,
            };
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
                    scene.fill(Fill::NonZero, transform, &wash, None, &path);
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
        }
    }
}

impl Widget for Drawing {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        Self::paint(scene, transform, self.rect, &self.shapes, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                if let Some(index) = self.shape_at(self.last_cursor, 1.0)
                    && let Some(press) = self.on_press.as_mut()
                {
                    press(index);
                    *ctx.handled = true;
                }
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
            // Decoration is not in the tree at all.
            Meaning::Decoration => return None,
        })
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// A progress fraction as it is announced: `0.6` is "60%".
fn percent(value: f64) -> String {
    format!("{:.0}%", (value.clamp(0.0, 1.0) * 100.0).round())
}

mod drawing_component;
pub use drawing_component::{DrawingProps, shape_description};

#[cfg(test)]
mod tests;
