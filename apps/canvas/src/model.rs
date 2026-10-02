//! The artwork: layers of marks on a ground.
//!
//! Every mark is a vector object with a stable id. What a mark looks like
//! depends on its layer: a plain layer draws it as crisp vector art; a layer
//! with a medium (oil, watercolour) paints it with that medium's simulation.
//! Coordinates are artwork units, origin top-left, y down.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};
use ui::vello::kurbo::{Affine, BezPath, Point, Rect, Shape as _};

/// The largest artwork side, in units.
pub const MAX_SIDE: f64 = 8192.0;
/// The most marks one action may add.
pub const MAX_MARKS_PER_ACTION: usize = 2000;
/// The most points one stroke or outline may have.
pub const MAX_POINTS: usize = 4096;

/// The support the artwork is made on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ground {
    /// `smooth`, `paper`, `cold-press`, `rough` or `linen`.
    pub texture: String,
    /// The ground's colour, `#rrggbb`.
    pub colour: String,
}

impl Default for Ground {
    fn default() -> Self {
        Self {
            texture: "paper".into(),
            colour: "#f4efe3".into(),
        }
    }
}

/// One layer of the artwork.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: String,
    pub name: String,
    /// Fractional position among layers; later is on top.
    pub order: String,
    /// The medium preset that paints this layer's marks, or empty for plain
    /// vector art.
    pub medium: String,
    pub opacity: f64,
    pub visible: bool,
}

/// A point of a stroke: `[x, y]` or `[x, y, pressure]` with pressure `0..=1`.
pub type StrokePoint = Vec<f64>;

/// The geometry of a mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Shape {
    /// An axis-aligned rectangle; `radius` rounds its corners.
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        #[serde(default)]
        radius: f64,
    },
    /// An ellipse around its centre.
    Ellipse { cx: f64, cy: f64, rx: f64, ry: f64 },
    /// A closed polygon through `points` (`[[x, y], …]`).
    Polygon { points: Vec<[f64; 2]> },
    /// An open polyline through `points`, drawn with the stroke colour.
    Line { points: Vec<[f64; 2]> },
    /// SVG path data (`M 10 10 C 20 0, 40 0, 50 10 Z`).
    Path { d: String },
    /// Text with its baseline starting at `x, y`.
    Text {
        x: f64,
        y: f64,
        text: String,
        size: f64,
    },
    /// A brush stroke: a smooth curve through `points`, `size` wide.
    /// Points are `[x, y]` or `[x, y, pressure]`; a handful of points is
    /// enough, the stroke is smoothed through them.
    Stroke { points: Vec<StrokePoint>, size: f64 },
}

/// A colour gradient across a mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Gradient {
    /// `linear` (from → to) or `radial` (centre `from`, radius to `to`).
    pub kind: String,
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// Colour stops: `[[0, "#ffd27a"], [1, "#e0603a"]]`.
    pub stops: Vec<(f64, String)>,
}

/// How a mark is coloured and, on a painted layer, how it is painted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Style {
    /// Fill colour: `#rrggbb`, `#rrggbbaa` or `pigment:<name>` from the
    /// layer medium's palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// A gradient fill, drawn instead of `fill` on plain layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<Gradient>,
    /// Outline or stroke colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    /// Outline width in units; zero draws no outline.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stroke_width: f64,
    /// `0..=1`; absent is opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// On a painted layer: the width of the brush that lays a filled shape
    /// in, in units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush: Option<f64>,
    /// On a painted layer: the direction of the brushwork, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    /// On a painted layer: paint into what is still wet (blending,
    /// bleeding) instead of over dry paint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wet: bool,
    /// On a painted layer: overrides of the medium's parameters for this
    /// mark (`{"load": 0.3}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f64>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

impl Style {
    pub fn opacity(&self) -> f64 {
        self.opacity.unwrap_or(1.0).clamp(0.0, 1.0)
    }
}

/// A mark as stored: its geometry, style and placement.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub id: String,
    pub layer: String,
    /// Fractional position within its layer; later is on top.
    pub order: String,
    pub name: String,
    pub shape: Shape,
    pub style: Style,
    /// An affine transform `[a, b, c, d, e, f]` applied to the geometry.
    pub transform: Option<[f64; 6]>,
}

/// The stored body of a mark: everything that is replaced together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkBody {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub shape: Shape,
    #[serde(default)]
    pub style: Style,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<[f64; 6]>,
}

/// The whole artwork as views and actions read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Artwork {
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub ground: Ground,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    /// Bottom to top within each layer, layers bottom to top.
    pub marks: Vec<Mark>,
    /// Media presets saved in this artwork, by id.
    pub media: BTreeMap<String, crate::media::MediumSpec>,
}

impl Default for Artwork {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 1200.0,
            height: 900.0,
            ground: Ground::default(),
            layers: Vec::new(),
            marks: Vec::new(),
            media: BTreeMap::new(),
        }
    }
}

impl Artwork {
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn mark(&self, id: &str) -> Option<&Mark> {
        self.marks.iter().find(|mark| mark.id == id)
    }

    /// The marks of `layer`, bottom to top.
    pub fn marks_of<'a>(&'a self, layer: &'a str) -> impl Iterator<Item = &'a Mark> + 'a {
        self.marks.iter().filter(move |mark| mark.layer == layer)
    }

    /// The artboard, in artwork units.
    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width, self.height)
    }

    /// The topmost visible mark under `point`, for hit-testing.
    pub fn mark_at(&self, point: Point) -> Option<&Mark> {
        self.marks.iter().rev().find(|mark| {
            self.layer(&mark.layer).is_some_and(|layer| layer.visible) && mark.hit(point)
        })
    }
}

/// A valid id: 1–48 of `a-z`, `0-9`, `-` and `_`, so an agent can name a mark
/// (`sun`, `tree-3`) and address it later.
pub fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn finite(values: &[f64]) -> bool {
    values
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SIDE * 4.0)
}

impl Shape {
    /// Why this geometry cannot be drawn, in words an agent can act on.
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::Rect {
                x,
                y,
                width,
                height,
                radius,
            } => (!finite(&[*x, *y, *width, *height, *radius]) || *width <= 0.0 || *height <= 0.0)
                .then(|| "a rect needs finite x, y and a positive width and height".into()),
            Self::Ellipse { cx, cy, rx, ry } => {
                (!finite(&[*cx, *cy, *rx, *ry]) || *rx <= 0.0 || *ry <= 0.0)
                    .then(|| "an ellipse needs a finite centre and positive rx, ry".into())
            }
            Self::Polygon { points } | Self::Line { points } => {
                if points.len() < 2 || points.len() > MAX_POINTS {
                    Some(format!("give 2 to {MAX_POINTS} points"))
                } else if !points.iter().all(|point| finite(point)) {
                    Some("every point must be finite".into())
                } else {
                    None
                }
            }
            Self::Path { d } => match BezPath::from_svg(d) {
                Ok(path) if path.elements().is_empty() => Some("the path is empty".into()),
                Ok(_) => None,
                Err(error) => Some(format!("invalid SVG path data: {error}")),
            },
            Self::Text { x, y, text, size } => (!finite(&[*x, *y, *size])
                || *size <= 0.0
                || text.is_empty()
                || text.chars().count() > 2000)
                .then(|| {
                    "text needs a finite position, a positive size and 1-2000 characters".into()
                }),
            Self::Stroke { points, size } => {
                if points.is_empty() || points.len() > MAX_POINTS {
                    Some(format!("a stroke needs 1 to {MAX_POINTS} points"))
                } else if !points
                    .iter()
                    .all(|point| (2..=3).contains(&point.len()) && finite(point))
                {
                    Some("stroke points are [x, y] or [x, y, pressure]".into())
                } else if !size.is_finite() || *size <= 0.0 || *size > MAX_SIDE {
                    Some("a stroke needs a positive size".into())
                } else {
                    None
                }
            }
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Rect { .. } => "rect",
            Self::Ellipse { .. } => "ellipse",
            Self::Polygon { .. } => "polygon",
            Self::Line { .. } => "line",
            Self::Path { .. } => "path",
            Self::Text { .. } => "text",
            Self::Stroke { .. } => "stroke",
        }
    }

    /// The outline as a path, in untransformed units. Text and strokes give
    /// the path their geometry follows (a stroke's centre line).
    pub fn path(&self) -> BezPath {
        match self {
            Self::Rect {
                x,
                y,
                width,
                height,
                radius,
            } => {
                let rect = Rect::new(*x, *y, x + width, y + height);
                if *radius > 0.0 {
                    rect.to_rounded_rect(*radius).to_path(0.1)
                } else {
                    rect.to_path(0.1)
                }
            }
            Self::Ellipse { cx, cy, rx, ry } => {
                ui::vello::kurbo::Ellipse::new((*cx, *cy), (*rx, *ry), 0.0).to_path(0.1)
            }
            Self::Polygon { points } => polyline(points, true),
            Self::Line { points } => polyline(points, false),
            Self::Path { d } => BezPath::from_svg(d).unwrap_or_default(),
            Self::Text { x, y, text, size } => {
                let width = size
                    * 0.55
                    * f64::from(u32::try_from(text.chars().count()).unwrap_or(u32::MAX));
                Rect::new(*x, y - size * 0.8, x + width, y + size * 0.25).to_path(0.1)
            }
            Self::Stroke { points, .. } => {
                let mut path = BezPath::new();
                for (index, point) in points.iter().enumerate() {
                    let at = Point::new(
                        point.first().copied().unwrap_or(0.0),
                        point.get(1).copied().unwrap_or(0.0),
                    );
                    if index == 0 {
                        path.move_to(at);
                    } else {
                        path.line_to(at);
                    }
                }
                path
            }
        }
    }

    /// Whether the shape is a closed region that can be filled.
    pub fn closed(&self) -> bool {
        match self {
            Self::Rect { .. } | Self::Ellipse { .. } | Self::Polygon { .. } => true,
            Self::Path { d } => d.trim_end().ends_with(['z', 'Z']),
            Self::Line { .. } | Self::Text { .. } | Self::Stroke { .. } => false,
        }
    }

    /// The same shape moved by `(dx, dy)`.
    #[must_use]
    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        let shift = |points: &[[f64; 2]]| points.iter().map(|[x, y]| [x + dx, y + dy]).collect();
        match self {
            Self::Rect {
                x,
                y,
                width,
                height,
                radius,
            } => Self::Rect {
                x: x + dx,
                y: y + dy,
                width: *width,
                height: *height,
                radius: *radius,
            },
            Self::Ellipse { cx, cy, rx, ry } => Self::Ellipse {
                cx: cx + dx,
                cy: cy + dy,
                rx: *rx,
                ry: *ry,
            },
            Self::Polygon { points } => Self::Polygon {
                points: shift(points),
            },
            Self::Line { points } => Self::Line {
                points: shift(points),
            },
            Self::Path { d } => {
                let mut path = BezPath::from_svg(d).unwrap_or_default();
                path.apply_affine(Affine::translate((dx, dy)));
                Self::Path { d: path.to_svg() }
            }
            Self::Text { x, y, text, size } => Self::Text {
                x: x + dx,
                y: y + dy,
                text: text.clone(),
                size: *size,
            },
            Self::Stroke { points, size } => Self::Stroke {
                points: points
                    .iter()
                    .map(|point| {
                        let mut moved = point.clone();
                        if let Some(x) = moved.get_mut(0) {
                            *x += dx;
                        }
                        if let Some(y) = moved.get_mut(1) {
                            *y += dy;
                        }
                        moved
                    })
                    .collect(),
                size: *size,
            },
        }
    }
}

fn polyline(points: &[[f64; 2]], closed: bool) -> BezPath {
    let mut path = BezPath::new();
    for (index, [x, y]) in points.iter().enumerate() {
        if index == 0 {
            path.move_to((*x, *y));
        } else {
            path.line_to((*x, *y));
        }
    }
    if closed {
        path.close_path();
    }
    path
}

impl Mark {
    pub fn affine(&self) -> Affine {
        self.transform.map_or(Affine::IDENTITY, Affine::new)
    }

    /// The mark's extent in artwork units, including stroke and outline
    /// width.
    pub fn bounds(&self) -> Rect {
        let mut path = self.shape.path();
        path.apply_affine(self.affine());
        let reach = match &self.shape {
            Shape::Stroke { size, .. } => size * 0.5,
            _ => self.style.stroke_width * 0.5,
        };
        path.bounding_box().inflate(reach, reach)
    }

    /// Whether `point` lands on the mark: inside a filled shape, or near a
    /// line or stroke.
    pub fn hit(&self, point: Point) -> bool {
        if !self.bounds().contains(point) {
            return false;
        }
        let local = self.affine().inverse() * point;
        match &self.shape {
            Shape::Text { .. } | Shape::Rect { .. } | Shape::Ellipse { .. } => {
                self.shape.path().contains(local)
            }
            Shape::Polygon { .. } | Shape::Path { .. } if self.shape.closed() => {
                self.shape.path().contains(local) || self.near_outline(local, 4.0)
            }
            Shape::Stroke { size, .. } => self.near_outline(local, size * 0.5 + 2.0),
            _ => self.near_outline(local, self.style.stroke_width * 0.5 + 4.0),
        }
    }

    fn near_outline(&self, point: Point, within: f64) -> bool {
        use ui::vello::kurbo::ParamCurveNearest as _;
        let mut near = false;
        let mut last: Option<Point> = None;
        ui::vello::kurbo::flatten(self.shape.path(), 0.5, |element| match element {
            ui::vello::kurbo::PathEl::MoveTo(at) => last = Some(at),
            ui::vello::kurbo::PathEl::LineTo(at) => {
                if let Some(from) = last {
                    let line = ui::vello::kurbo::Line::new(from, at);
                    if line.nearest(point, 0.1).distance_sq <= within * within {
                        near = true;
                    }
                }
                last = Some(at);
            }
            _ => {}
        });
        near
    }
}
