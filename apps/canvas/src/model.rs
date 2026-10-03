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
pub const MAX_SIDE: f64 = 8192.0; // a document limit in artwork units, not on-screen spacing
/// The most marks one action may add.
pub const MAX_MARKS_PER_ACTION: usize = 2000;
/// The most points one stroke or outline may have.
pub const MAX_POINTS: usize = 4096;

/// The support the artwork is made on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ground {
    /// `smooth`, `paper`, `cold-press`, `rough`, `linen` or `panel`.
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
    /// Held still: the pointer passes through its marks to those beneath.
    pub locked: bool,
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
    /// Text, first baseline at `y`; `\n` breaks lines.
    Text {
        x: f64,
        y: f64,
        text: String,
        size: f64,
        /// `sans`, `serif`, `mono` or a family name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
        /// 400 regular to 700 bold.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<u16>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        italic: bool,
        /// `start`, `center` or `end` at `x`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<String>,
        /// Times size (1.2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_height: Option<f64>,
        /// Wrap width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
    },
    /// A brush stroke: a smooth curve through `points`, `size` wide.
    /// Points are `[x, y]` or `[x, y, pressure]`; a handful of points is
    /// enough, the stroke is smoothed through them.
    Stroke {
        points: Vec<StrokePoint>,
        size: f64,
        /// A limb that lifts to a point and forks this many times (1-6)
        /// into ever thinner twigs: a branch or bare tree in one stroke.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forks: Option<u32>,
        /// With `forks`: how widely the twigs splay, in degrees (default 35).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spread: Option<f64>,
        /// Instead of one line, this many short dabs (1-600) of `size`
        /// scattered along the points: foliage, flowers, broken light.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dabs: Option<u32>,
        /// With `dabs`: how far they stray either side of the path, in units
        /// (default twice `size`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scatter: Option<f64>,
        /// With `dabs`: how long each is, in units (about `size` when
        /// absent); much longer than `size`, each is a flick that lifts to
        /// a point, as grass is painted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dab_length: Option<f64>,
    },
}

/// A colour gradient across a mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Gradient {
    /// `linear` (from → to) or `radial` (centre `from`, radius to `to`).
    pub kind: String,
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// Colour stops: `[[0, "#ffd27a"], [1, "#e0603a"]]`; a stop's alpha
    /// (`#ffd27a00`) fades the paint out, for a glow or mist.
    pub stops: Vec<(f64, String)>,
}

/// How a mark is coloured and, on a painted layer, how it is painted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Style {
    /// Fill colour: `#rrggbb`, `#rrggbbaa` or `pigment:<name>` from the
    /// layer medium's palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// A gradient fill, drawn instead of `fill` on plain layers. On an oil
    /// layer the brush is remixed along it as the shape is laid in.
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
    /// Outline dash pattern, `[on, off, …]` in units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dash: Vec<f64>,
    /// Outline ends: `butt`, `round` or `square`; round when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<String>,
    /// Outline corners: `miter`, `round` or `bevel`; round when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
    /// How the mark mixes with what is beneath it; normal when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
}

/// The blend modes a mark may use.
pub const BLENDS: [&str; 16] = [
    "normal",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color-dodge",
    "color-burn",
    "hard-light",
    "soft-light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
];

/// A soft shadow cast by a mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    /// `[dx, dy]` in units.
    #[serde(default)]
    pub offset: [f64; 2],
    /// Softness in units; 0 is hard.
    #[serde(default)]
    pub blur: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
}

/// A picture to work from, laid over the artwork like tracing paper. It is
/// not part of the artwork: exports never show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    /// The picture's content address.
    pub image: String,
    /// `[x, y, width, height]`; fitted to the artwork, aspect kept, when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<[f64; 4]>,
    pub opacity: f64,
    pub visible: bool,
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
    /// The group the mark belongs to, or empty.
    pub group: String,
    /// The mark whose shape this mark is cut to, or empty.
    pub clip: String,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub clip: String,
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
    pub reference: Option<Reference>,
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
            reference: None,
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

    /// The topmost visible mark of an unlocked layer under `point`, for
    /// hit-testing.
    pub fn mark_at(&self, point: Point) -> Option<&Mark> {
        self.marks.iter().rev().find(|mark| {
            self.layer(&mark.layer)
                .is_some_and(|layer| layer.visible && !layer.locked)
                && mark.hit(point)
                && self.clip_of(mark).is_none_or(|clip| clip.hit(point))
        })
    }

    /// The mark `mark` is cut to, if any.
    pub fn clip_of(&self, mark: &Mark) -> Option<&Mark> {
        (!mark.clip.is_empty())
            .then(|| self.mark(&mark.clip))
            .flatten()
    }

    /// What of `mark` can show: its bounds, cut to its clip's.
    pub fn visible_bounds(&self, mark: &Mark) -> Rect {
        let bounds = mark.bounds();
        self.clip_of(mark)
            .map_or(bounds, |clip| bounds.intersect(clip.bounds()))
    }

    /// The members of group `id`, bottom to top.
    pub fn members(&self, id: &str) -> impl Iterator<Item = &Mark> + '_ {
        let id = id.to_owned();
        self.marks.iter().filter(move |mark| mark.group == id)
    }

    /// The groups' ids, sorted.
    pub fn groups(&self) -> Vec<&str> {
        let mut groups: Vec<&str> = self
            .marks
            .iter()
            .map(|mark| mark.group.as_str())
            .filter(|group| !group.is_empty())
            .collect();
        groups.sort_unstable();
        groups.dedup();
        groups
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
    /// A brush stroke through `points`, `size` wide, that grows nothing.
    pub fn plain_stroke(points: Vec<StrokePoint>, size: f64) -> Self {
        Self::Stroke {
            points,
            size,
            forks: None,
            spread: None,
            dabs: None,
            scatter: None,
            dab_length: None,
        }
    }

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
            Self::Text {
                x,
                y,
                text,
                size,
                font,
                weight,
                align,
                line_height,
                width,
                ..
            } => {
                if !finite(&[*x, *y, *size])
                    || *size <= 0.0
                    || text.is_empty()
                    || text.chars().count() > 2000
                {
                    Some(
                        "text needs a finite position, a positive size and 1-2000 characters"
                            .into(),
                    )
                } else if align
                    .as_deref()
                    .is_some_and(|align| !matches!(align, "start" | "center" | "end"))
                {
                    Some("a text's align is start, center or end".into())
                } else if weight.is_some_and(|weight| !(100..=900).contains(&weight)) {
                    Some("a text's weight is 100 to 900 (400 regular, 700 bold)".into())
                } else if line_height.is_some_and(|lh| !(0.5..=4.0).contains(&lh)) {
                    Some("a text's line_height is 0.5 to 4".into())
                } else if width.is_some_and(|width| !finite(&[width]) || width <= 0.0) {
                    Some("a text's width must be positive".into())
                } else if font
                    .as_deref()
                    .is_some_and(|font| font.trim().is_empty() || font.len() > 64)
                {
                    Some("a text's font is sans, serif, mono or a family name".into())
                } else {
                    None
                }
            }
            Self::Stroke { points, size, .. } => {
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
                    self.growth_problem()
                }
            }
        }
    }

    /// Why a stroke's twigs or dabs cannot be grown.
    fn growth_problem(&self) -> Option<String> {
        let Self::Stroke {
            forks,
            spread,
            dabs,
            scatter,
            dab_length,
            ..
        } = self
        else {
            return None;
        };
        let units =
            |value: &Option<f64>| value.is_some_and(|value| !(0.0..=MAX_SIDE).contains(&value));
        if forks.is_some() && dabs.is_some() {
            Some("a stroke either forks or breaks into dabs, not both".into())
        } else if forks.is_some_and(|forks| !(1..=6).contains(&forks)) {
            Some("forks is 1 to 6".into())
        } else if spread.is_some_and(|spread| !(5.0..=90.0).contains(&spread)) {
            Some("spread is 5 to 90 degrees".into())
        } else if dabs.is_some_and(|dabs| !(1..=600).contains(&dabs)) {
            Some("dabs is 1 to 600".into())
        } else if units(scatter) || units(dab_length) {
            Some("scatter and dab_length are 0 or more units".into())
        } else {
            None
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
            Self::Text { .. } => self.text_box().unwrap_or_default().to_path(0.1),
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
                Self::Path {
                    d: path_data(&path),
                }
            }
            Self::Text { .. } => {
                let mut moved = self.clone();
                if let Self::Text { x, y, .. } = &mut moved {
                    *x += dx;
                    *y += dy;
                }
                moved
            }
            Self::Stroke {
                points,
                size,
                forks,
                spread,
                dabs,
                scatter,
                dab_length,
            } => Self::Stroke {
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
                forks: *forks,
                spread: *spread,
                dabs: *dabs,
                scatter: *scatter,
                dab_length: *dab_length,
            },
        }
    }

    /// What a stroke grows into, in units: twigs, or dabs pointing along
    /// the path or `angle` degrees.
    pub fn form(&self, angle: Option<f64>) -> Option<tilo_wet_media::Form> {
        let Self::Stroke {
            size,
            forks,
            spread,
            dabs,
            scatter,
            dab_length,
            ..
        } = self
        else {
            return None;
        };
        if let Some(forks) = forks {
            return Some(tilo_wet_media::Form::Branch {
                forks: *forks,
                spread: narrow(spread.unwrap_or(35.0)),
            });
        }
        dabs.map(|count| tilo_wet_media::Form::Dabs {
            count,
            scatter: narrow(scatter.unwrap_or(size * 2.0)),
            length: narrow(dab_length.unwrap_or(0.0)),
            angle: angle.map(narrow),
        })
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "stroke geometry is bounded by the document's size limits"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

impl Shape {
    /// The same shape under `affine`, when the geometry itself can carry it:
    /// a move and a stretch along the axes, the same both ways for text and
    /// brush strokes. `None` asks for a transform instead (a turn, a skew, a
    /// flip).
    #[must_use]
    pub fn transformed(&self, affine: Affine) -> Option<Self> {
        let [sx, b, c, sy, dx, dy] = affine.as_coeffs();
        if b != 0.0 || c != 0.0 || sx <= 0.0 || sy <= 0.0 {
            return None;
        }
        let uniform = (sx - sy).abs() <= 1e-9 * sx.max(sy);
        let map = |[x, y]: [f64; 2]| [x * sx + dx, y * sy + dy];
        let points = |points: &[[f64; 2]]| points.iter().copied().map(map).collect();
        Some(match self {
            Self::Rect {
                x,
                y,
                width,
                height,
                radius,
            } => {
                let [x, y] = map([*x, *y]);
                Self::Rect {
                    x,
                    y,
                    width: width * sx,
                    height: height * sy,
                    radius: radius * sx.min(sy),
                }
            }
            Self::Ellipse { cx, cy, rx, ry } => {
                let [cx, cy] = map([*cx, *cy]);
                Self::Ellipse {
                    cx,
                    cy,
                    rx: rx * sx,
                    ry: ry * sy,
                }
            }
            Self::Polygon { points: at } => Self::Polygon { points: points(at) },
            Self::Line { points: at } => Self::Line { points: points(at) },
            Self::Path { d } => {
                let mut path = BezPath::from_svg(d).ok()?;
                path.apply_affine(affine);
                Self::Path {
                    d: path_data(&path),
                }
            }
            Self::Text { .. } if uniform => {
                let mut text = self.clone();
                if let Self::Text {
                    x, y, size, width, ..
                } = &mut text
                {
                    [*x, *y] = map([*x, *y]);
                    *size *= sx;
                    *width = width.map(|width| width * sx);
                }
                text
            }
            Self::Stroke {
                points,
                size,
                forks,
                spread,
                dabs,
                scatter,
                dab_length,
            } if uniform => Self::Stroke {
                points: points
                    .iter()
                    .map(|point| {
                        let mut moved = point.clone();
                        if let [x, y, ..] = moved.as_mut_slice() {
                            [*x, *y] = map([*x, *y]);
                        }
                        moved
                    })
                    .collect(),
                size: size * sx,
                forks: *forks,
                spread: *spread,
                dabs: *dabs,
                scatter: scatter.map(|scatter| scatter * sx),
                dab_length: dab_length.map(|length| length * sx),
            },
            Self::Text { .. } | Self::Stroke { .. } => return None,
        })
    }
}

/// A number as path data writes it: at most three decimals, no trailing
/// zeros.
#[must_use]
pub fn short_number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" || text.is_empty() {
        "0".into()
    } else {
        text.into()
    }
}

/// SVG path data for `path`, to a thousandth of a unit.
#[must_use]
pub fn path_data(path: &BezPath) -> String {
    use ui::vello::kurbo::PathEl;
    let point = |p: Point| format!("{},{}", short_number(p.x), short_number(p.y));
    let parts: Vec<String> = path
        .elements()
        .iter()
        .map(|element| match *element {
            PathEl::MoveTo(p) => format!("M{}", point(p)),
            PathEl::LineTo(p) => format!("L{}", point(p)),
            PathEl::QuadTo(a, p) => format!("Q{} {}", point(a), point(p)),
            PathEl::CurveTo(a, b, p) => format!("C{} {} {}", point(a), point(b), point(p)),
            PathEl::ClosePath => "Z".into(),
        })
        .collect();
    parts.join(" ")
}

fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Shape {
    /// The box a text's lines take, estimated from its characters: what
    /// selection, hit-testing and `inspect` use without shaping it.
    pub fn text_box(&self) -> Option<Rect> {
        let Self::Text {
            x,
            y,
            text,
            size,
            weight,
            align,
            line_height,
            width,
            ..
        } = self
        else {
            return None;
        };
        let advance = size
            * if weight.unwrap_or(400) >= 600 {
                0.6
            } else {
                0.55
            };
        let lines: Vec<f64> = text
            .split('\n')
            .map(|line| advance * count(line.chars().count()))
            .collect();
        let natural = lines.iter().copied().fold(0.0, f64::max);
        let (rows, wide) = match width {
            Some(width) => (
                lines
                    .iter()
                    .map(|line| (line / width).ceil().max(1.0))
                    .sum::<f64>(),
                *width,
            ),
            None => (count(lines.len()), natural),
        };
        let left = match (width, align.as_deref()) {
            (None, Some("center")) => x - wide / 2.0,
            (None, Some("end")) => x - wide,
            _ => *x,
        };
        let leading = size * line_height.unwrap_or(1.2);
        let top = y - size * 0.8;
        Some(Rect::new(
            left,
            top,
            left + wide,
            top + leading * (rows - 1.0) + size * 1.05,
        ))
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

    /// The mark moved by `(dx, dy)` artwork units: its geometry when it has
    /// no transform, its transform when it has one.
    #[must_use]
    pub fn moved(&self, dx: f64, dy: f64) -> Self {
        let mut mark = self.clone();
        if self.transform.is_some() {
            mark.transform = Some((Affine::translate((dx, dy)) * self.affine()).as_coeffs());
        } else {
            mark.shape = self.shape.translated(dx, dy);
            // A gradient is placed in the same units as the geometry.
            if let Some(gradient) = &mut mark.style.gradient {
                for point in [&mut gradient.from, &mut gradient.to] {
                    point[0] += dx;
                    point[1] += dy;
                }
            }
        }
        mark
    }

    /// The mark under `affine` (artwork units): baked into its geometry
    /// where the geometry can carry it, so a stretched rectangle stays a
    /// rectangle; otherwise composed onto its transform.
    #[must_use]
    pub fn transformed(&self, affine: Affine) -> Self {
        let mut mark = self.clone();
        match self
            .transform
            .is_none()
            .then(|| self.shape.transformed(affine))
            .flatten()
        {
            Some(shape) => mark.shape = shape,
            None => mark.transform = Some((affine * self.affine()).as_coeffs()),
        }
        mark
    }

    /// The mark's extent in artwork units, including stroke and outline
    /// width.
    pub fn bounds(&self) -> Rect {
        let mut path = self.shape.path();
        path.apply_affine(self.affine());
        let reach = match &self.shape {
            // Twigs reach beyond the limb about as far again as it is long;
            // dabs stray their scatter from the path.
            Shape::Stroke {
                size,
                forks: Some(_),
                ..
            } => size * 0.5 + path.perimeter(0.5) * 0.9,
            Shape::Stroke {
                size,
                dabs: Some(_),
                scatter,
                dab_length,
                ..
            } => size * 2.0 + scatter.unwrap_or(size * 2.0) + dab_length.unwrap_or(0.0) * 1.4,
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
