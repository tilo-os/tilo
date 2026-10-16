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
use ui::num::f32_of_f64;
use ui::num::f64_of_usize;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Keyed, Map, Model};
use ui::vello::kurbo::{Affine, BezPath, Point, Rect, Shape as _};

/// The largest artwork side, in units.
pub const MAX_SIDE: f64 = 8192.0; // a document limit in artwork units, not on-screen spacing
/// The most marks one action may add.
pub const MAX_MARKS_PER_ACTION: usize = 2000;
/// The most marks an artwork opened from a file may hold.
pub const MAX_MARKS: usize = 100_000;
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "shown, held still, an inverted or unlinked mask and clipped are independent settings"
)]
#[derive(Clone, Debug, Default, PartialEq, Model)]
pub struct Layer {
    #[model(atom, readonly)]
    pub id: String,
    /// Name
    #[model(label)]
    pub name: String,
    /// Organisational folder; empty for an ungrouped layer. It does not
    /// change stacking, clipping or how the layers composite.
    #[model(atom)]
    pub group: String,
    /// Fractional position among layers; later is on top.
    #[model(atom, readonly)]
    pub order: String,
    /// The medium preset that paints this layer's marks, or empty for plain
    /// vector art.
    #[model(atom)]
    pub medium: String,
    pub opacity: f64,
    pub visible: bool,
    /// `0..=1`: how much of the ground's tooth shows through the layer's
    /// marks, like ink printed on paper.
    pub texture: f64,
    /// Held still: the pointer passes through its marks to those beneath.
    pub locked: bool,
    /// Colour and tone adjustments of everything on the layer.
    pub adjust: Option<Adjust>,
    /// The layer this layer masks, or empty for an ordinary layer. A mask's
    /// marks are drawn in greys: where it is light that layer shows, where
    /// it is dark it is hidden. Hiding the mask turns it off.
    #[model(atom, readonly)]
    pub mask_of: String,
    /// A mask's grey before any mark, `0..=1`: 1 shows all of its layer,
    /// 0 hides all of it.
    pub base: f64,
    /// A mask that hides where it is light and shows where it is dark.
    pub invert: bool,
    /// Clipped to the layer below: it shows only where that layer has paint.
    pub clip: bool,
    /// A mask that stays put when its layer's marks move. Linked, it moves
    /// with them whenever they all move as one, as the layer's content.
    pub unlinked: bool,
}

impl Identified for Layer {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }

    /// Bottom to top.
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}

impl Layer {
    /// Whether this layer is another layer's mask.
    pub fn is_mask(&self) -> bool {
        !self.mask_of.is_empty()
    }
}

/// The area selected for the next edit: a region of the artwork, kept with
/// it like the reference picture and never drawn into it. Fills, masks and
/// copies take its shape; its feather softens their edge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    /// SVG path data in artwork units, filled non-zero.
    pub d: String,
    /// How far its edge fades, in units.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
    /// Its edge refined to the picture: how much of each pixel near it is
    /// in, where a path can only say in or out (strands of fur and hair).
    /// A mask made of the selection draws it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<Matte>,
}

/// A selection's soft edge: a picture, white where selected and as see-through
/// as each pixel is out, laid over the artwork at `rect`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Matte {
    pub src: String,
    /// `[x, y, w, h]`.
    pub rect: [f64; 4],
}

impl Selection {
    /// The selected area.
    pub fn region(&self) -> crate::region::Region {
        crate::region::Region::of_path(&BezPath::from_svg(&self.d).unwrap_or_default())
    }
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
        /// `sans`, `serif`, `mono` or a family `media` lists under fonts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
        /// 100 to 900; 400 regular, 700 bold, any value between.
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
        /// Letter spacing in thousandths of the size: 80 opens up capitals,
        /// negative tightens.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tracking: Option<f64>,
        /// SVG path data the baseline follows instead of starting at x, y;
        /// align sets the words at its start, middle or end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// A brush stroke as `paint` lays one: a smooth curve through `points`
    /// (`[x, y]` or `[x, y, pressure]`), `size` wide; forks, dabs and hatch
    /// as there.
    Stroke {
        points: Vec<StrokePoint>,
        size: f64,
        // Its forms are described once, on `paint`'s strokes: an agent reads
        // them there, and `draw` names this shape after it.
        //
        // A limb that lifts to a point and forks this many times (1-6) into
        // ever thinner twigs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forks: Option<u32>,
        // With `forks`: how widely the twigs splay, in degrees (default 35).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spread: Option<f64>,
        // Instead of one line, this many short dabs (1-600) of `size`
        // scattered along the points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dabs: Option<u32>,
        // With `dabs`: how far they stray either side of the path, in units
        // (default twice `size`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scatter: Option<f64>,
        // With `dabs`: how long each is, in units (about `size` when absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dab_length: Option<f64>,
        // Instead of one line, lines this many units apart across the area
        // the points enclose, at the mark's angle (default 45).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hatch: Option<f64>,
    },
    /// A PNG, JPEG or WebP in the frame `x, y, width, height`.
    Image {
        /// File path, content address or `data:` URI.
        src: String,
        x: f64,
        y: f64,
        /// Units; absent: a unit per pixel, or the other side's aspect.
        #[serde(default, skip_serializing_if = "is_zero")]
        width: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        height: f64,
        /// Fractions `[x, y, w, h]` of the picture shown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crop: Option<[f64; 4]>,
        /// Degrees turned inside the frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        straighten: Option<f64>,
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
    /// Radial only: where the light sits inside the circle, off its centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focal: Option<[f64; 2]>,
}

/// How a mark is coloured and, on a painted layer, how it is painted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Style {
    /// Enlarge image pixels with nearest-neighbor sampling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pixelated: bool,
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
    /// A brush stroke's preset on a plain layer (`pen`, `pencil`, `marker`,
    /// `airbrush`, `charcoal`, `calligraphy`), or `eraser` on any layer;
    /// a round ink brush when absent. See [`crate::brushes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush: Option<String>,
    /// On a painted layer: the size of the brush that lays a filled shape
    /// in, in units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush_size: Option<f64>,
    /// On a painted layer: the direction of the brushwork, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    /// On a painted layer: paint into what is still wet (blending,
    /// bleeding) instead of over dry paint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wet: bool,
    /// On a painted layer: overrides of the medium's parameters for this
    /// mark (`{"load": 0.3}`). On a plain layer's stroke: the brush's
    /// `flow`, `hardness`, `spacing`, `smoothing` and `taper`, each `0..=1`.
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
    /// A shadow cast inside the mark from its edge; a light colour and an
    /// offset toward the light make it a highlight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadow: Option<Shadow>,
    /// `0..=1`: film grain over the mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<f64>,
    /// Soften the mark's edge outward over this many units: mist, glow,
    /// shading that melts into what is under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather: Option<f64>,
    /// Colour and tone adjustments: a picture's pixels, a shape's colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<Adjust>,
}

/// Non-destructive colour and tone adjustments, applied in this order:
/// white balance, exposure, contrast, curve, highlights and
/// shadows, hue, saturation, vibrance, grayscale, invert; then blur,
/// sharpen and vignette. Absent or zero leaves a step out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Adjust {
    /// Stops, -5 to 5. Others -1 to 1 unless noted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlights: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f64>,
    /// Saturation sparing strong colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibrance: Option<f64>,
    /// Degrees, -180 to 180.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f64>,
    /// Warmer +.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Magenta +.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f64>,
    /// Tone curve `[[in, out], …]`, 0-1; `[[0.05, 0], [0.95, 1]]` sets
    /// black and white points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grayscale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invert: Option<bool>,
    /// Radius in units, 0-200 (pictures and paint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<f64>,
    /// 0-1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpen: Option<f64>,
    /// Edges darker +, lighter -.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vignette: Option<f64>,
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Model)]
pub struct Mark {
    #[model(atom, readonly)]
    pub id: String,
    #[model(atom)]
    pub layer: String,
    /// Fractional position within its layer; later is on top.
    #[model(atom, readonly)]
    pub order: String,
    /// Name
    #[model(label)]
    pub name: String,
    pub shape: Shape,
    pub style: Style,
    /// An affine transform `[a, b, c, d, e, f]` applied to the geometry.
    pub transform: Option<[f64; 6]>,
    /// The group the mark belongs to, or empty.
    #[model(atom)]
    pub group: String,
    /// The mark whose shape this mark is cut to, or empty.
    #[model(atom)]
    pub clip: String,
    /// SVG path data of the area the mark is kept inside (the selection's
    /// shape when it was laid), in the units of its geometry so it travels
    /// with it, or empty.
    #[model(atom, readonly)]
    pub area: String,
}

impl Identified for Mark {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }

    /// Bottom to top within a layer; [`Artwork::settle`] stacks the
    /// layers.
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub area: String,
}

impl Mark {
    /// What is stored of the mark besides its id, layer and place.
    pub fn body(&self) -> MarkBody {
        MarkBody {
            name: self.name.clone(),
            shape: self.shape.clone(),
            style: self.style.clone(),
            transform: self.transform,
            group: self.group.clone(),
            clip: self.clip.clone(),
            area: self.area.clone(),
        }
    }

    /// The mark `id` on `layer` at `order`, made of `body`.
    pub fn of_body(id: String, layer: String, order: String, body: MarkBody) -> Self {
        Self {
            id,
            layer,
            order,
            name: body.name,
            shape: body.shape,
            style: body.style,
            transform: body.transform,
            group: body.group,
            clip: body.clip,
            area: body.area,
        }
    }
}

/// The whole artwork as views and actions read it.
#[derive(Clone, Debug, PartialEq, Model)]
pub struct Artwork {
    /// Title
    #[model(label)]
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub ground: Ground,
    /// Bottom to top.
    pub layers: Keyed<Layer>,
    /// Bottom to top within each layer, layers bottom to top (see
    /// [`Artwork::settle`]).
    pub marks: Keyed<Mark>,
    /// Media presets saved in this artwork, by id.
    pub media: Map<crate::media::MediumSpec>,
    pub reference: Option<Reference>,
    pub selection: Option<Selection>,
}

impl Default for Artwork {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 1200.0,
            height: 900.0,
            ground: Ground::default(),
            layers: Keyed::new(),
            marks: Keyed::new(),
            media: Map::new(),
            reference: None,
            selection: None,
        }
    }
}

tilo_resource::json_model!(
    Ground,
    Shape,
    Style,
    Adjust,
    Reference,
    Selection,
    crate::media::MediumSpec
);

/// A blank rectangle: what a mark is before it is given a shape.
impl Default for Shape {
    fn default() -> Self {
        Self::Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            radius: 0.0,
        }
    }
}

impl Artwork {
    /// The artwork as `doc` holds it, stacked (see [`Self::settle`]).
    ///
    /// # Errors
    /// Returns why the document does not hold an artwork.
    pub fn from_doc(doc: &tilo_resource::Doc) -> Result<Self, tilo_resource::ModelError> {
        let mut art = <Self as tilo_resource::Fields>::read_fields(doc, &ui::automerge::ROOT)?;
        art.settle();
        Ok(art)
    }

    /// Stack the marks as they are drawn: layers bottom to top, each
    /// layer's marks bottom to top. A mark whose layer is gone (removed
    /// while someone drew on it) is not part of the artwork.
    pub fn settle(&mut self) {
        // A document no one has started yet (a viewer joining before the
        // artwork's first change reached it) is the blank default size.
        let blank = Self::default();
        if self.width <= 0.0 || self.height <= 0.0 {
            (self.width, self.height) = (blank.width, blank.height);
        }
        let rank: std::collections::HashMap<&str, usize> = self
            .layers
            .iter()
            .enumerate()
            .map(|(at, layer)| (layer.id.as_str(), at))
            .collect();
        let mut marks: Vec<(usize, Mark)> = std::mem::take(&mut *self.marks)
            .into_iter()
            .filter_map(|mark| Some((*rank.get(mark.layer.as_str())?, mark)))
            .collect();
        marks.sort_by(|(a_rank, a), (b_rank, b)| {
            a_rank
                .cmp(b_rank)
                .then_with(|| a.order.cmp(&b.order))
                .then_with(|| a.id.cmp(&b.id))
        });
        *self.marks = marks.into_iter().map(|(_, mark)| mark).collect();
    }

    /// This artwork, stacked.
    #[must_use]
    pub fn settled(&self) -> Self {
        let mut art = self.clone();
        art.settle();
        art
    }

    /// Add `mark`, or replace the mark with its id. Found and added one at a
    /// time, so a copy of the artwork remembers which mark changed or came,
    /// and saving it writes that mark, not every one.
    pub fn put_mark(&mut self, mark: Mark) {
        match self.marks.find_mut(&mark.id()) {
            Some(held) => *held = mark,
            None => self.marks.push(mark),
        }
    }

    /// Add `layer`, or replace the layer with its id, as [`Self::put_mark`].
    pub fn put_layer(&mut self, layer: Layer) {
        match self.layers.find_mut(&layer.id()) {
            Some(held) => *held = layer,
            None => self.layers.push(layer),
        }
    }

    /// Remove the mark `id`, if there is one.
    pub fn remove_mark(&mut self, id: &str) {
        self.marks.retain(|mark| mark.id != id);
    }

    /// Remove the layer `id`, if there is one; its marks stay until
    /// removed too.
    pub fn drop_layer(&mut self, id: &str) {
        self.layers.retain(|layer| layer.id != id);
    }
}

impl Artwork {
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn mark(&self, id: &str) -> Option<&Mark> {
        self.marks.iter().find(|mark| mark.id == id)
    }

    /// The layer new marks go on when none is named: the top layer that is
    /// not a mask.
    pub fn top_layer(&self) -> Option<&Layer> {
        self.layers.iter().rev().find(|layer| !layer.is_mask())
    }

    /// The mask of `layer`, if it has one.
    pub fn mask_of(&self, layer: &str) -> Option<&Layer> {
        self.layers.iter().find(|mask| mask.mask_of == layer)
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
                .is_some_and(|layer| layer.visible && !layer.locked && !layer.is_mask())
                && mark.inked()
                && mark.hit(point)
                && self.clipped_in(mark, point)
        })
    }

    /// The marks `mark` is cut to: the mark its clip names, or the members
    /// of the group it names; none when it is not cut.
    pub fn clips_of(&self, mark: &Mark) -> Vec<&Mark> {
        if mark.clip.is_empty() {
            return Vec::new();
        }
        match self.mark(&mark.clip) {
            Some(clip) => vec![clip],
            None => self
                .members(&mark.clip)
                .filter(|member| member.id != mark.id)
                .collect(),
        }
    }

    /// Whether `point` is inside what `mark` is cut to, its clip and its
    /// area (anywhere when it is not cut).
    pub fn clipped_in(&self, mark: &Mark, point: Point) -> bool {
        let clips = self.clips_of(mark);
        (clips.is_empty() || clips.iter().any(|clip| clip.hit(point)))
            && mark.area().is_none_or(|area| area.contains(point))
    }

    /// What a click at `point` picks: the topmost mark under it, an eraser
    /// included. An eraser shows no ink of its own, so only the pointer's
    /// tools that outline erasers pick them; [`Self::mark_at`] passes
    /// through them.
    pub fn pick_at(&self, point: Point) -> Option<&Mark> {
        self.pick_near(point, 0.0)
    }

    /// [`Self::pick_at`] forgiving a pointer `slop` artwork units off a
    /// line or a stroke: a hand's few pixels at the zoom it is shown at.
    pub fn pick_near(&self, point: Point, slop: f64) -> Option<&Mark> {
        self.marks.iter().rev().find(|mark| {
            self.layer(&mark.layer)
                .is_some_and(|layer| layer.visible && !layer.locked && !layer.is_mask())
                && (mark.inked() || mark.erases())
                && mark.hit_within(point, slop)
                && self.clipped_in(mark, point)
        })
    }

    /// The erasers on shown layers, bottom to top: what the eraser and
    /// selection tools outline.
    pub fn erasers(&self) -> impl Iterator<Item = &Mark> + '_ {
        self.marks.iter().filter(|mark| {
            mark.erases()
                && self
                    .layer(&mark.layer)
                    .is_some_and(|layer| layer.visible && !layer.is_mask())
        })
    }

    /// The marks beneath eraser `eraser` on its layer that it takes paint
    /// off: those its reach overlaps.
    pub fn erased_by<'a>(&'a self, eraser: &'a Mark) -> impl Iterator<Item = &'a Mark> + 'a {
        let reach = eraser.bounds();
        self.marks
            .iter()
            .take_while(move |mark| mark.id != eraser.id)
            .filter(move |mark| {
                mark.layer == eraser.layer
                    && !mark.erases()
                    && mark.inked()
                    && self.visible_bounds(mark).intersect(reach).area() > 0.0
            })
    }

    /// What of `mark` can show: its bounds, cut to its clip's.
    pub fn visible_bounds(&self, mark: &Mark) -> Rect {
        let bounds = mark.bounds();
        let bounds = self
            .clips_of(mark)
            .iter()
            .map(|clip| clip.bounds())
            .reduce(|a, b| a.union(b))
            .map_or(bounds, |clip| bounds.intersect(clip));
        mark.area()
            .map_or(bounds, |area| bounds.intersect(area.bounding_box()))
    }

    /// The members of group `id`, bottom to top: its own marks and those of
    /// the groups inside it (`icons` holds `icons/mail`'s).
    pub fn members(&self, id: &str) -> impl Iterator<Item = &Mark> + '_ {
        let id = id.to_owned();
        self.marks
            .iter()
            .filter(move |mark| in_group(&mark.group, &id))
    }

    /// The groups' ids, sorted, a nested group after the one it is in.
    pub fn groups(&self) -> Vec<&str> {
        let mut groups: Vec<&str> = Vec::new();
        for mark in &self.marks {
            let mut end = 0;
            for part in mark.group.split('/') {
                end += part.len();
                if let Some(path) = mark.group.get(..end).filter(|path| !path.is_empty()) {
                    groups.push(path);
                }
                end += 1;
            }
        }
        groups.sort_unstable();
        groups.dedup();
        groups
    }
}

/// Whether a mark in `group` belongs to group `id`: it is `id` or nested in it.
pub fn in_group(group: &str, id: &str) -> bool {
    !id.is_empty()
        && group
            .strip_prefix(id)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// A valid group id: one or more ids joined by `/`, a group in a group.
pub fn valid_group(group: &str) -> bool {
    group.len() <= 160 && group.split('/').all(valid_id)
}

impl Artwork {}

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

/// Why a picture cannot be shown, in words an agent can act on.
fn image_problem(shape: &Shape) -> Option<String> {
    let Shape::Image {
        src,
        x,
        y,
        width,
        height,
        crop,
        straighten,
    } = shape
    else {
        return None;
    };
    let (frame, width, height) = ([*x, *y, *width, *height], *width, *height);
    if ui::tilo_actions::content_hash(src).is_none() {
        Some("an image's src is a file path, content address or data: URI".into())
    } else if !finite(&frame) || width <= 0.0 || height <= 0.0 {
        Some("an image needs finite x, y and a positive width and height".into())
    } else if crop.is_some_and(|[x, y, w, h]| {
        ![x, y, w, h].iter().all(|v| v.is_finite())
            || x < 0.0
            || y < 0.0
            || w <= 0.0
            || h <= 0.0
            || x + w > 1.0 + 1e-9
            || y + h > 1.0 + 1e-9
    }) {
        Some("crop is [x, y, w, h] fractions of the picture, inside 0..1, w and h positive".into())
    } else if straighten.is_some_and(|angle| !(-45.0..=45.0).contains(&angle)) {
        Some("straighten is -45 to 45 degrees".into())
    } else {
        None
    }
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
            hatch: None,
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
                tracking,
                path,
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
                } else if let Some(font) = font.as_deref().filter(|font| !crate::fonts::known(font))
                {
                    Some(format!(
                        "font {font:?} is not one Tilo ships; use sans, serif, mono or one of {}",
                        crate::fonts::families().join(", ")
                    ))
                } else if tracking.is_some_and(|tracking| !(-300.0..=1000.0).contains(&tracking)) {
                    Some("a text's tracking is -300 to 1000 thousandths of its size".into())
                } else if let Some(path) = path {
                    match BezPath::from_svg(path) {
                        Ok(path) if path.elements().len() >= 2 => None,
                        Ok(_) => Some("a text's path needs at least two points".into()),
                        Err(error) => Some(format!("a text's path is not SVG path data: {error}")),
                    }
                } else {
                    None
                }
            }
            Self::Image { .. } => image_problem(self),
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
            hatch,
            points,
            ..
        } = self
        else {
            return None;
        };
        let units =
            |value: &Option<f64>| value.is_some_and(|value| !(0.0..=MAX_SIDE).contains(&value));
        if [forks.is_some(), dabs.is_some(), hatch.is_some()]
            .iter()
            .filter(|given| **given)
            .count()
            > 1
        {
            Some("a stroke forks, breaks into dabs or hatches an area: one of them".into())
        } else if hatch.is_some_and(|hatch| !(0.5..=MAX_SIDE).contains(&hatch)) {
            Some("hatch is 0.5 or more units between lines".into())
        } else if hatch.is_some() && points.len() < 3 {
            Some("a hatched stroke's points enclose an area: give 3 or more".into())
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
            Self::Image { .. } => "image",
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
            Self::Image {
                x,
                y,
                width,
                height,
                ..
            } => Rect::new(*x, *y, x + width, y + height).to_path(0.1),
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
            Self::Rect { .. }
            | Self::Ellipse { .. }
            | Self::Polygon { .. }
            | Self::Image { .. } => true,
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
            Self::Image { .. } => {
                let mut moved = self.clone();
                if let Self::Image { x, y, .. } = &mut moved {
                    *x += dx;
                    *y += dy;
                }
                moved
            }
            Self::Text { .. } => {
                let mut moved = self.clone();
                if let Self::Text { x, y, path, .. } = &mut moved {
                    *x += dx;
                    *y += dy;
                    if let Some(d) = path {
                        let mut along = BezPath::from_svg(d).unwrap_or_default();
                        along.apply_affine(Affine::translate((dx, dy)));
                        *d = path_data(&along);
                    }
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
                hatch,
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
                hatch: *hatch,
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
            hatch,
            ..
        } = self
        else {
            return None;
        };
        if let Some(spacing) = hatch {
            return Some(tilo_wet_media::Form::Hatch {
                spacing: f32_of_f64(*spacing),
                angle: f32_of_f64(angle.unwrap_or(45.0)),
            });
        }
        if let Some(forks) = forks {
            return Some(tilo_wet_media::Form::Branch {
                forks: *forks,
                spread: f32_of_f64(spread.unwrap_or(35.0)),
            });
        }
        dabs.map(|count| tilo_wet_media::Form::Dabs {
            count,
            scatter: f32_of_f64(scatter.unwrap_or(size * 2.0)),
            length: f32_of_f64(dab_length.unwrap_or(0.0)),
            angle: angle.map(f32_of_f64),
        })
    }
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
            Self::Image { .. } => {
                let mut image = self.clone();
                if let Self::Image {
                    x,
                    y,
                    width,
                    height,
                    ..
                } = &mut image
                {
                    [*x, *y] = map([*x, *y]);
                    *width *= sx;
                    *height *= sy;
                }
                image
            }
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
                hatch,
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
                hatch: hatch.map(|spacing| spacing * sx),
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
            tracking,
            path,
            ..
        } = self
        else {
            return None;
        };
        if let Some(path) = path {
            // Words along a path stay within about their size of it.
            let along = BezPath::from_svg(path).unwrap_or_default().bounding_box();
            return Some(along.inflate(*size * 0.5, *size));
        }
        let advance = size
            * (if weight.unwrap_or(400) >= 600 {
                0.6
            } else {
                0.55
            } + tracking.unwrap_or(0.0) / 1000.0);
        let lines: Vec<f64> = text
            .split('\n')
            .map(|line| advance * f64_of_usize(line.chars().count()))
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
            None => (f64_of_usize(lines.len()), natural),
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
    /// Whether the mark puts down any colour: a mark with none only cuts
    /// others to its shape.
    pub fn inked(&self) -> bool {
        let style = &self.style;
        style.fill.is_some()
            || style.stroke.is_some()
            || style.gradient.is_some()
            || matches!(self.shape, Shape::Image { .. })
    }

    /// Whether the mark is an eraser: it takes paint off its layer and has
    /// none of its own to pick.
    pub fn erases(&self) -> bool {
        matches!(self.shape, Shape::Stroke { .. })
            && crate::brushes::Preset::of(&self.style).erases()
    }

    /// The area the mark is kept inside, in artwork units, if it has one.
    pub fn area(&self) -> Option<BezPath> {
        let mut area = BezPath::from_svg(&self.area)
            .ok()
            .filter(|area| !area.elements().is_empty())?;
        area.apply_affine(self.affine());
        Some(area)
    }

    /// Keep the mark inside `area` (SVG path data in artwork units), or
    /// inside nothing when it is empty.
    pub fn keep_inside(&mut self, area: &str) {
        self.area = BezPath::from_svg(area)
            .ok()
            .filter(|path| !path.elements().is_empty())
            .map(|mut path| {
                path.apply_affine(self.affine().inverse());
                path_data(&path)
            })
            .unwrap_or_default();
    }

    /// The mark's area moved with its geometry by `affine`.
    fn carry_area(&mut self, affine: Affine) {
        if let Ok(mut area) = BezPath::from_svg(&self.area)
            && !area.elements().is_empty()
        {
            area.apply_affine(affine);
            self.area = path_data(&area);
        }
    }

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
            mark.carry_area(Affine::translate((dx, dy)));
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
            Some(shape) => {
                mark.shape = shape;
                mark.carry_area(affine);
            }
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
            Shape::Stroke {
                size,
                hatch: Some(spacing),
                ..
            } => size * 0.5 + spacing * 0.5,
            Shape::Stroke { size, .. } => size * 0.5,
            _ => self.style.stroke_width * 0.5,
        };
        path.bounding_box().inflate(reach, reach)
    }

    /// Whether `point` lands on the mark: inside a filled shape, or near a
    /// line or stroke.
    pub fn hit(&self, point: Point) -> bool {
        self.hit_within(point, 0.0)
    }

    /// [`Self::hit`] with lines and strokes reaching `slop` artwork units
    /// further, so a thin line is not missed by a hair at a far zoom.
    pub fn hit_within(&self, point: Point, slop: f64) -> bool {
        let slop = slop.max(0.0);
        if !self.bounds().inflate(slop, slop).contains(point) {
            return false;
        }
        let local = self.affine().inverse() * point;
        match &self.shape {
            Shape::Text { .. }
            | Shape::Rect { .. }
            | Shape::Ellipse { .. }
            | Shape::Image { .. } => self.shape.path().contains(local),
            Shape::Polygon { .. } | Shape::Path { .. } if self.shape.closed() => {
                self.shape.path().contains(local) || self.near_outline(local, 4.0_f64.max(slop))
            }
            Shape::Stroke {
                hatch: Some(_),
                points,
                ..
            } => Shape::Polygon {
                points: points
                    .iter()
                    .map(|point| {
                        [
                            point.first().copied().unwrap_or(0.0),
                            point.get(1).copied().unwrap_or(0.0),
                        ]
                    })
                    .collect(),
            }
            .path()
            .contains(local),
            Shape::Stroke { size, .. } => self.near_outline(local, size * 0.5 + 2.0_f64.max(slop)),
            _ => self.near_outline(local, self.style.stroke_width * 0.5 + 4.0_f64.max(slop)),
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
