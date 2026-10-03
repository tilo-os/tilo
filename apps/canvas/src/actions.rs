//! The artwork's vocabulary: every edit a person, the palette, an agent or a
//! script makes is one of these values, applied the same way, one undo step
//! and one attributed history entry each.
//!
//! The vocabulary is deliberately small. A passage of painting is one
//! `paint` with many strokes, a composition is one `draw` with many shapes;
//! everything after that is addressed by the ids marks were given.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_actions::{self, ActionError};
use ui::tilo_resource::{self, Doc, JsonSchema, order};

use crate::doc;
use crate::media::{self, MediumSpec};
use crate::model::{
    Artwork, BLENDS, Gradient, Layer, MAX_MARKS_PER_ACTION, MAX_SIDE, Mark, Reference, Shadow,
    Shape, StrokePoint, Style, valid_id,
};

pub const ACTION_TYPE: &str = "canvas.artwork";

/// Set the artwork's title, size or ground.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArtwork {
    pub title: Option<String>,
    /// Width in units, 16 to 8192.
    pub width: Option<f64>,
    /// Height in units, 16 to 8192.
    pub height: Option<f64>,
    /// `smooth`, `paper`, `cold-press`, `rough`, `linen` or `panel`.
    pub texture: Option<String>,
    /// Ground colour, `#rrggbb`.
    pub ground_colour: Option<String>,
}

/// Start over on a blank artwork: its size, ground and first layer. The
/// artwork before it stays in history, one undo away.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewArtwork {
    pub title: Option<String>,
    /// A size: `screen`, `a4`, `letter`, `square`, `poster` or `icon`;
    /// `width` and `height` set it exactly.
    pub size: Option<String>,
    /// Turn the size on its side.
    pub landscape: Option<bool>,
    /// Width in units, 16 to 8192.
    pub width: Option<f64>,
    /// Height in units, 16 to 8192.
    pub height: Option<f64>,
    /// What it is painted on and with: `paper`, `white`, `watercolour`,
    /// `oil`, `panel` or `dark`; `texture`, `ground_colour` and `medium`
    /// set it exactly.
    pub start: Option<String>,
    pub texture: Option<String>,
    /// Ground colour, `#rrggbb`.
    pub ground_colour: Option<String>,
    /// The first layer's medium (see `media`); empty for plain vector art.
    pub medium: Option<String>,
}

/// Replace the artwork with one saved as a file: its title, size, ground,
/// layers, marks and media.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenArtwork {
    /// An absolute path to a `.artwork` file, a content address, or the
    /// file's JSON. The action keeps the file's JSON, so every replica
    /// opens the same artwork.
    pub file: String,
}

/// Add a layer on top, or above another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddLayer {
    /// An id to address the layer by (`sky`); one is made when absent.
    pub id: Option<String>,
    pub name: String,
    /// A medium id (`oil`, `watercolour`, … see `media`) to paint this
    /// layer's marks with; absent or empty for plain vector art.
    pub medium: Option<String>,
    /// `0..=1`.
    pub opacity: Option<f64>,
    /// Put the new layer directly above this layer instead of on top.
    pub above: Option<String>,
    /// `0..=1`: let the ground's tooth show through the layer's marks.
    pub texture: Option<f64>,
}

/// Change a layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditLayer {
    pub id: String,
    pub name: Option<String>,
    /// A medium id, or empty for plain vector art.
    pub medium: Option<String>,
    pub opacity: Option<f64>,
    pub visible: Option<bool>,
    /// Locked marks are not picked by the pointer.
    pub locked: Option<bool>,
    /// `front`, `back`, `forward` or `backward` among the layers.
    pub to: Option<String>,
    /// `0..=1`: let the ground's tooth show through the layer's marks.
    pub texture: Option<f64>,
    /// `reveal`, `hide` or `remove`: a mask layer `<id>-mask` to paint
    /// greys on, white shows, black hides.
    pub mask: Option<String>,
    /// Invert the mask.
    pub invert: Option<bool>,
    /// Clip to the layer below.
    pub clip: Option<bool>,
}

/// Remove a layer and everything on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveLayer {
    pub id: String,
}

/// One shape to draw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewMark {
    /// An id to address the mark by later (`sun`, `hill-2`); one is made when
    /// absent.
    pub id: Option<String>,
    pub name: Option<String>,
    /// The geometry: `{"kind": "rect", "x": 0, "y": 0, "width": 10, "height": 10}`,
    /// `ellipse` (cx, cy, rx, ry), `polygon`/`line` (points), `path` (d: SVG
    /// path data), `text` (x, y, text, size) or `stroke` (points, size).
    pub shape: Shape,
    /// `#rrggbb[aa]` or `pigment:<name>`.
    pub fill: Option<String>,
    /// Instead of `fill`; on an oil layer the brush is remixed along it.
    pub gradient: Option<Gradient>,
    /// Outline colour.
    pub stroke: Option<String>,
    /// Outline width in units.
    pub stroke_width: Option<f64>,
    /// `0..=1`.
    pub opacity: Option<f64>,
    /// Painted layers: the brush size a filled shape is laid in with.
    pub brush_size: Option<f64>,
    /// A stroke's brush preset, as `paint` takes it.
    pub brush: Option<String>,
    /// Painted layers: the brushwork's direction in degrees.
    pub angle: Option<f64>,
    /// Painted layers: paint into what is still wet.
    pub wet: Option<bool>,
    /// Painted layers: medium parameter overrides (`{"load": 0.3}`).
    pub params: Option<BTreeMap<String, f64>>,
    /// Outline dashes `[on, off, …]`.
    pub dash: Option<Vec<f64>>,
    /// `butt`, `round` or `square`.
    pub cap: Option<String>,
    /// `miter`, `round` or `bevel`.
    pub join: Option<String>,
    /// `normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`,
    /// `color-dodge`, `color-burn`, `hard-light`, `soft-light`, `difference`,
    /// `exclusion`, `hue`, `saturation`, `color` or `luminosity`.
    pub blend: Option<String>,
    pub shadow: Option<Shadow>,
    /// Inside the mark from its edge; light colour = highlight.
    pub inner_shadow: Option<Shadow>,
    /// `0..=1` film grain.
    pub grain: Option<f64>,
    /// Soften the edge outward over this many units.
    pub feather: Option<f64>,
    /// A group id: edits naming it change the members as one; `a/b`
    /// nests group b in a.
    pub group: Option<String>,
    /// Cut to this mark's shape (opacity 0 hides that mark), or
    /// `selection`.
    pub clip: Option<String>,
    /// An affine `[a, b, c, d, e, f]` on the geometry, as `inspect` shows.
    pub transform: Option<[f64; 6]>,
}

impl NewMark {
    /// `shape` with nothing else given.
    pub fn new(shape: Shape) -> Self {
        Self {
            id: None,
            name: None,
            shape,
            fill: None,
            gradient: None,
            stroke: None,
            stroke_width: None,
            opacity: None,
            brush_size: None,
            brush: None,
            angle: None,
            wet: None,
            params: None,
            dash: None,
            cap: None,
            join: None,
            blend: None,
            shadow: None,
            inner_shadow: None,
            grain: None,
            feather: None,
            group: None,
            clip: None,
            transform: None,
        }
    }
}

/// Draw shapes, in order, on top of a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Draw {
    /// The layer to draw on; the top layer when absent.
    pub layer: Option<String>,
    pub marks: Vec<NewMark>,
}

/// One brush stroke.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewStroke {
    pub id: Option<String>,
    pub name: Option<String>,
    /// `[[x, y], …]` or `[[x, y, pressure], …]`, pressure `0..=1`; the
    /// stroke is smoothed through them, so a few points make a curve.
    pub points: Vec<StrokePoint>,
    /// Brush width in units.
    pub size: f64,
    /// `#rrggbb[aa]` or `pigment:<name>`; an eraser takes none.
    #[serde(default)]
    pub colour: String,
    /// Plain layers: `pen` (crisp ink, pressure sets width), `pencil`
    /// (grainy, pressure sets darkness), `marker` (flat, overlaps darken),
    /// `airbrush` (soft spray that builds up), `charcoal` (picks up the
    /// ground's tooth), `calligraphy` (broad nib at `angle`, default 45).
    /// Any layer: `eraser` clears paint beneath it on its layer. A round
    /// ink brush when absent.
    pub brush: Option<String>,
    pub opacity: Option<f64>,
    /// Paint into what is still wet: blends in oil, bleeds in watercolour.
    pub wet: Option<bool>,
    /// Medium overrides on a painted layer (`{"load": 0.2}` for dry brush);
    /// on a plain layer the brush's flow, hardness, spacing, smoothing and
    /// taper, each 0..=1.
    pub params: Option<BTreeMap<String, f64>>,
    /// A limb that lifts to a point and forks this many times (1-6) into
    /// ever thinner twigs: a branch or a bare tree in one stroke.
    pub forks: Option<u32>,
    /// With `forks`: how widely the twigs splay, in degrees (default 35).
    pub spread: Option<f64>,
    /// Instead of one line, this many short dabs (1-600) of `size`
    /// scattered along the points: foliage, flowers, broken light.
    pub dabs: Option<u32>,
    /// With `dabs`: how far they stray either side of the path, in units
    /// (default twice `size`).
    pub scatter: Option<f64>,
    /// With `dabs`: how long each is, in units (about `size` when absent);
    /// much longer than `size`, each is a flick lifting to a point (grass).
    pub dab_length: Option<f64>,
    /// With `dabs`: the way they point, in degrees (-90 is up); along the
    /// path when absent.
    pub angle: Option<f64>,
    /// Keep the paint inside this mark's shape, as a painter cuts in
    /// against an edge: texture a rock or a crown within its silhouette.
    /// Or `selection`.
    pub clip: Option<String>,
}

/// Paint brush strokes, in order, on top of a layer: a passage of painting
/// in one step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Paint {
    /// The layer to paint on; the top layer when absent.
    pub layer: Option<String>,
    pub strokes: Vec<NewStroke>,
}

/// Change marks: their colours and style, position, size, rotation or layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditMarks {
    /// Marks, or groups (all their members).
    pub ids: Vec<String>,
    pub name: Option<String>,
    /// Replace the geometry (e.g. edit a path's `d`).
    pub shape: Option<Shape>,
    /// `[]` removes dashes.
    pub dash: Option<Vec<f64>>,
    pub cap: Option<String>,
    pub join: Option<String>,
    pub blend: Option<String>,
    /// Zero offset and blur removes it.
    pub shadow: Option<Shadow>,
    /// Zero offset and blur removes it.
    pub inner_shadow: Option<Shadow>,
    /// 0 removes it.
    pub grain: Option<f64>,
    /// 0 removes it.
    pub feather: Option<f64>,
    /// Put into this group (`a/b` nests b in a); empty takes out of any.
    pub group: Option<String>,
    /// Cut to this mark's shape; empty uncuts.
    pub clip: Option<String>,
    /// A colour (replacing a gradient), or `none`.
    pub fill: Option<String>,
    pub gradient: Option<Gradient>,
    /// A colour, or `none`.
    pub stroke: Option<String>,
    pub stroke_width: Option<f64>,
    pub opacity: Option<f64>,
    /// A stroke's brush preset (see paint).
    pub brush: Option<String>,
    /// A stroke's width.
    pub size: Option<f64>,
    /// Painted layers: the brush size a filled shape is laid in with.
    pub brush_size: Option<f64>,
    pub angle: Option<f64>,
    pub wet: Option<bool>,
    pub params: Option<BTreeMap<String, f64>>,
    /// Move by `[dx, dy]` units.
    pub move_by: Option<[f64; 2]>,
    /// Scale about each mark's or group's centre.
    pub scale: Option<f64>,
    /// Degrees clockwise about each centre.
    pub rotate: Option<f64>,
    /// Move the marks onto this layer, on top.
    pub layer: Option<String>,
    /// An affine `[a, b, c, d, e, f]` in artwork units applied after the
    /// rest, to all the marks as one: a stretch or turn about any point.
    pub transform: Option<[f64; 6]>,
    /// Strokes: take the wobble out of their paths, 0..=1.
    pub smooth: Option<f64>,
    /// Strokes: drop points within this many units of the line.
    pub simplify: Option<f64>,
    /// A stroke: continue it through these points, from the end they start
    /// nearer.
    pub extend: Option<Vec<StrokePoint>>,
    /// A stroke: cut it in two at the point nearest [x, y]; the far part
    /// gets a new id.
    pub split: Option<[f64; 2]>,
    /// Strokes: join them, in the order named, into the first.
    pub merge: Option<bool>,
}

/// Remove marks by id, or every mark of a layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveMarks {
    #[serde(default)]
    pub ids: Vec<String>,
    /// Remove every mark on this layer (with along: only its strokes).
    pub layer: Option<String>,
    /// Remove the brush strokes this path `[[x, y], …]` crosses, as a
    /// stroke eraser does.
    pub along: Option<Vec<[f64; 2]>>,
}

/// Move marks within their layer's stacking order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Arrange {
    pub ids: Vec<String>,
    /// `front`, `back`, `forward`, `backward`, or `above`/`below` the mark or
    /// group `of`.
    pub to: String,
    /// The mark or group `above` and `below` are relative to.
    pub of: Option<String>,
}

/// Save a medium in the artwork (or replace one saved before), so layers can
/// use it and everyone who opens the artwork has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveMedium {
    pub medium: MediumSpec,
}

/// Remove a medium saved in the artwork; layers using it paint plain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveMedium {
    pub id: String,
}

/// Line marks up along an edge, or space them evenly. A group moves as one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Align {
    /// Marks or groups.
    pub ids: Vec<String>,
    /// `left`, `center`, `right`, `top`, `middle` or `bottom`.
    pub edge: Option<String>,
    /// `horizontal` or `vertical`: equal gaps.
    pub distribute: Option<String>,
    /// `artwork` or a mark/group id; else their extent.
    pub to: Option<String>,
}

/// Set the reference picture to work from: shown over the artwork like
/// tracing paper, compared by `compare`, never exported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetReference {
    /// A PNG or JPEG: an absolute file path, a content address, or the
    /// picture itself as `data:image/png;base64,…`.
    pub image: Option<String>,
    /// `[x, y, width, height]`; fitted to the artwork when absent.
    pub frame: Option<[f64; 4]>,
    /// `0..=1` (default 0.5).
    pub opacity: Option<f64>,
    pub visible: Option<bool>,
    /// Remove the reference.
    pub remove: Option<bool>,
}

/// One edit of the artwork.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum CanvasAction {
    SetArtwork(SetArtwork),
    NewArtwork(NewArtwork),
    OpenArtwork(OpenArtwork),
    AddLayer(AddLayer),
    EditLayer(EditLayer),
    RemoveLayer(RemoveLayer),
    Draw(Draw),
    Paint(Paint),
    EditMarks(Box<EditMarks>),
    RemoveMarks(RemoveMarks),
    Arrange(Arrange),
    SaveMedium(SaveMedium),
    RemoveMedium(RemoveMedium),
    Align(Align),
    SetReference(SetReference),
    Select(Box<crate::selection::Select>),
    UseSelection(crate::selection::UseSelection),
    Combine(crate::selection::Combine),
}

impl CanvasAction {
    /// An `edit-marks`.
    pub fn edit(edit: EditMarks) -> Self {
        Self::EditMarks(Box::new(edit))
    }
}

pub(crate) fn refuse(message: impl Into<String>) -> ActionError {
    ActionError::Refused(message.into())
}

/// The id of a mark or layer given none: a hash of what it is and where it
/// sits in its action, so every replica of the action names it alike and two
/// collaborators drawing different things never collide.
pub fn derived_id(prefix: &str, content: &impl Serialize, index: usize) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(
        serde_json::to_string(content)
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update(&index.to_le_bytes());
    let hex = hasher.finalize().to_hex();
    format!("{prefix}-{}", hex.get(..10).unwrap_or_default())
}

pub(crate) fn check_colour(
    art: &Artwork,
    layer: &str,
    colour: &str,
    what: &str,
) -> Result<(), ActionError> {
    if colour == "none"
        || tilo_wet_media::parse_paint(colour, &media::palette_for(art, layer)).is_some()
    {
        return Ok(());
    }
    let palette: Vec<String> = media::palette_for(art, layer)
        .into_iter()
        .map(|pigment| pigment.name)
        .collect();
    Err(refuse(format!(
        "{what} {colour:?} is not a colour: use #rrggbb, #rrggbbaa or pigment:<name>; this layer's pigments are {}",
        palette.join(", ")
    )))
}

fn check_unit(value: Option<f64>, what: &str) -> Result<(), ActionError> {
    match value {
        Some(value) if !(0.0..=1.0).contains(&value) => {
            Err(refuse(format!("{what} must be within 0..=1, got {value}")))
        }
        _ => Ok(()),
    }
}

fn check_style(art: &Artwork, layer: &str, style: &Style) -> Result<(), ActionError> {
    for (colour, what) in [(&style.fill, "fill"), (&style.stroke, "stroke")] {
        if let Some(colour) = colour {
            check_colour(art, layer, colour, what)?;
        }
    }
    if let Some(gradient) = &style.gradient {
        if !matches!(gradient.kind.as_str(), "linear" | "radial") {
            return Err(refuse("a gradient's kind is linear or radial"));
        }
        if gradient.stops.len() < 2 {
            return Err(refuse("a gradient needs at least two stops"));
        }
        for (_, colour) in &gradient.stops {
            check_colour(art, layer, colour, "gradient stop")?;
        }
    }
    check_unit(style.opacity, "opacity")?;
    if !style.stroke_width.is_finite() || style.stroke_width < 0.0 {
        return Err(refuse("stroke_width must be zero or more"));
    }
    if style.dash.len() > 16
        || style
            .dash
            .iter()
            .any(|step| !step.is_finite() || *step < 0.0 || *step > MAX_SIDE)
        || (!style.dash.is_empty() && style.dash.iter().sum::<f64>() <= 0.0)
    {
        return Err(refuse(
            "dash is up to 16 lengths [on, off, …], zero or more and not all zero",
        ));
    }
    for (value, what, choices) in [
        (&style.cap, "cap", &["butt", "round", "square"][..]),
        (&style.join, "join", &["miter", "round", "bevel"][..]),
        (&style.blend, "blend", &BLENDS[..]),
    ] {
        if let Some(value) = value
            && !choices.contains(&value.as_str())
        {
            return Err(refuse(format!(
                "{what} {value:?} must be one of {}",
                choices.join(", ")
            )));
        }
    }
    for shadow in style.shadow.iter().chain(&style.inner_shadow) {
        if !shadow
            .offset
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_SIDE)
            || !(0.0..=500.0).contains(&shadow.blur)
        {
            return Err(refuse(
                "a shadow's offset must be finite and its blur 0 to 500",
            ));
        }
        if let Some(colour) = &shadow.colour {
            check_colour(art, layer, colour, "shadow colour")?;
        }
    }
    check_unit(style.grain, "grain")?;
    if style
        .feather
        .is_some_and(|feather| !(0.0..=500.0).contains(&feather))
    {
        return Err(refuse("feather is 0 to 500 units"));
    }
    if let Some(gradient) = &style.gradient
        && gradient
            .focal
            .is_some_and(|[x, y]| !x.is_finite() || !y.is_finite())
    {
        return Err(refuse("a gradient's focal point must be finite"));
    }
    Ok(())
}

/// A transform must be finite and keep some area, so the mark can be found
/// and turned back.
fn check_transform(transform: [f64; 6]) -> Result<(), ActionError> {
    let affine = ui::vello::kurbo::Affine::new(transform);
    if transform
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SIDE * 4.0)
        && affine.determinant().abs() > 1e-6
    {
        Ok(())
    } else {
        Err(refuse(
            "a transform is [a, b, c, d, e, f], finite, and must not flatten the mark",
        ))
    }
}

/// The marks `ids` address, as units that move, scale and rotate together:
/// a mark id is that mark, a group id all of the group's members. A mark
/// named twice (itself and through its group) counts once.
pub(crate) fn units<'a>(
    art: &'a Artwork,
    ids: &[String],
) -> Result<Vec<Vec<&'a Mark>>, ActionError> {
    if ids.is_empty() {
        return Err(refuse("name marks or groups in ids"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut units = Vec::new();
    for id in ids {
        let members: Vec<&Mark> = match art.mark(id) {
            Some(mark) => vec![mark],
            None => art.members(id).collect(),
        };
        if members.is_empty() {
            let groups = art.groups();
            return Err(refuse(format!(
                "there is no mark or group {id:?}; the groups are {}; inspect lists the marks",
                if groups.is_empty() {
                    "none".to_owned()
                } else {
                    groups.join(", ")
                }
            )));
        }
        let fresh: Vec<&Mark> = members
            .into_iter()
            .filter(|mark| seen.insert(mark.id.as_str()))
            .collect();
        if !fresh.is_empty() {
            units.push(fresh);
        }
    }
    Ok(units)
}

/// The extent of a unit of marks: what of them can show.
fn extent(art: &Artwork, unit: &[&Mark]) -> ui::vello::kurbo::Rect {
    unit.iter()
        .map(|mark| art.visible_bounds(mark))
        .reduce(|a, b| a.union(b))
        .unwrap_or_default()
}

/// A group id must be valid and not already a mark's id.
fn check_group(art: &Artwork, group: &str, new_ids: &[String]) -> Result<(), ActionError> {
    if !crate::model::valid_group(group) {
        return Err(refuse(format!(
            "group {group:?} must be 1-48 of a-z, 0-9, '-' and '_', or such ids joined by / to nest one group in another"
        )));
    }
    if art.mark(group).is_some() || new_ids.iter().any(|id| id == group) {
        return Err(refuse(format!(
            "{group:?} is a mark's id; name the group differently"
        )));
    }
    Ok(())
}

/// A clip must name an existing (or just drawn) mark other than itself.
fn check_clip(art: &Artwork, id: &str, clip: &str, new_ids: &[String]) -> Result<(), ActionError> {
    if clip == id {
        return Err(refuse(format!("mark {id:?} cannot clip itself")));
    }
    if art.mark(clip).is_none() && !new_ids.iter().any(|new| new == clip) {
        return Err(refuse(format!(
            "clip {clip:?} is not a mark; clip names the mark whose shape cuts this one"
        )));
    }
    Ok(())
}

/// The layer an action names, or the top one.
pub(crate) fn target_layer(art: &Artwork, named: Option<&str>) -> Result<String, ActionError> {
    match named {
        Some(id) => art.layer(id).map(|layer| layer.id.clone()).ok_or_else(|| {
            refuse(format!(
                "there is no layer {id:?}; the layers are {}",
                layer_list(art)
            ))
        }),
        None => art
            .top_layer()
            .map(|layer| layer.id.clone())
            .ok_or_else(|| refuse("the artwork has no layer yet; add-layer first")),
    }
}

pub(crate) fn layer_list(art: &Artwork) -> String {
    if art.layers.is_empty() {
        return "none".into();
    }
    art.layers
        .iter()
        .map(|layer| layer.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fresh order keys for `count` marks on top of `layer`.
pub(crate) fn on_top(art: &Artwork, layer: &str, count: usize) -> Vec<String> {
    let last = art.marks_of(layer).map(|mark| mark.order.clone()).max();
    order::sequence(last.as_deref(), None, count)
}

fn unique_ids(art: &Artwork, ids: &[String]) -> Result<(), ActionError> {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !valid_id(id) {
            return Err(refuse(format!(
                "id {id:?} must be 1-48 of a-z, 0-9, '-' and '_'"
            )));
        }
        if art.mark(id).is_some() {
            return Err(refuse(format!(
                "a mark {id:?} already exists; edit-marks changes it, remove-marks removes it"
            )));
        }
        if !seen.insert(id.as_str()) {
            return Err(refuse(format!("id {id:?} is given twice")));
        }
    }
    Ok(())
}

/// Write `marks` (their layer and order still to be given) on top of
/// `layer`.
pub(crate) fn add_marks(
    doc: &mut Doc,
    art: &Artwork,
    layer: &str,
    marks: Vec<Mark>,
) -> Result<(), ActionError> {
    if marks.is_empty() {
        return Err(refuse("give at least one mark"));
    }
    if marks.len() > MAX_MARKS_PER_ACTION {
        return Err(refuse(format!(
            "at most {MAX_MARKS_PER_ACTION} marks per action; split the passage"
        )));
    }
    let ids: Vec<String> = marks.iter().map(|mark| mark.id.clone()).collect();
    unique_ids(art, &ids)?;
    for (index, mark) in marks.iter().enumerate() {
        let problem = |error: ActionError| refuse(format!("mark {index}: {error}"));
        if let Some(problem) = mark.shape.problem() {
            return Err(refuse(format!("mark {index}: {problem}")));
        }
        if let Some(transform) = mark.transform {
            check_transform(transform).map_err(problem)?;
        }
        check_style(art, layer, &mark.style).map_err(problem)?;
        check_brush(art, layer, mark).map_err(problem)?;
        if !mark.group.is_empty() {
            check_group(art, &mark.group, &ids).map_err(problem)?;
        }
        if !mark.clip.is_empty() {
            check_clip(art, &mark.id, &mark.clip, &ids).map_err(problem)?;
        }
    }
    let orders = on_top(art, layer, marks.len());
    for (mark, order) in marks.into_iter().zip(orders) {
        doc::write_mark(
            doc,
            &Mark {
                layer: layer.to_owned(),
                order,
                ..mark
            },
        )?;
    }
    Ok(())
}

/// A mark to add, before its layer and order are known.
pub(crate) fn placed(id: String, name: String, shape: Shape, style: Style) -> Mark {
    Mark {
        id,
        layer: String::new(),
        order: String::new(),
        name,
        shape,
        style,
        transform: None,
        group: String::new(),
        clip: String::new(),
    }
}

/// The clip a new mark names: `selection` is a mark of the selection's
/// shape, made on `layer` when first used.
fn clip_named(
    doc: &mut Doc,
    art: &mut Artwork,
    layer: &str,
    clip: Option<&String>,
) -> Result<String, ActionError> {
    match clip.map(String::as_str) {
        Some(SELECTION_CLIP) => {
            let cut = crate::selection::selection_cut(doc, art, layer)?;
            if art.mark(&cut).is_none() {
                *art = doc::read(doc);
            }
            Ok(cut)
        }
        clip => Ok(clip.unwrap_or_default().to_owned()),
    }
}

/// The clip that keeps a new mark inside the selection.
pub const SELECTION_CLIP: &str = "selection";

impl Draw {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        let layer = target_layer(art, self.layer.as_deref())?;
        let mut art = art.clone();
        let clips = self
            .marks
            .iter()
            .map(|mark| clip_named(doc, &mut art, &layer, mark.clip.as_ref()))
            .collect::<Result<Vec<String>, ActionError>>()?;
        let art = &art;
        let marks = self
            .marks
            .iter()
            .enumerate()
            .map(|(index, mark)| {
                let id = mark
                    .id
                    .clone()
                    .unwrap_or_else(|| derived_id("m", mark, index));
                let style = Style {
                    fill: mark.fill.clone(),
                    gradient: mark.gradient.clone(),
                    stroke: mark.stroke.clone(),
                    stroke_width: mark
                        .stroke_width
                        .unwrap_or_else(|| if mark.stroke.is_some() { 2.0 } else { 0.0 }),
                    opacity: mark.opacity,
                    brush: mark.brush.as_deref().and_then(crate::brushes::stored),
                    brush_size: mark.brush_size,
                    angle: mark.angle,
                    wet: mark.wet.unwrap_or(false),
                    params: mark.params.clone().unwrap_or_default(),
                    dash: mark.dash.clone().unwrap_or_default(),
                    cap: mark.cap.clone(),
                    join: mark.join.clone(),
                    blend: mark.blend.clone().filter(|blend| blend != "normal"),
                    shadow: mark.shadow.clone(),
                    inner_shadow: mark.inner_shadow.clone(),
                    grain: mark.grain.filter(|grain| *grain > 0.0),
                    feather: mark.feather.filter(|feather| *feather > 0.0),
                };
                let style =
                    if style.fill.is_none() && style.gradient.is_none() && style.stroke.is_none() {
                        // An uncoloured shape would be invisible: draw it in ink.
                        Style {
                            fill: mark.shape.closed().then(|| "#1d1b19".to_owned()),
                            stroke: (!mark.shape.closed()).then(|| "#1d1b19".to_owned()),
                            stroke_width: if mark.shape.closed() { 0.0 } else { 2.0 },
                            ..style
                        }
                    } else {
                        style
                    };
                Mark {
                    group: mark.group.clone().unwrap_or_default(),
                    clip: clips.get(index).cloned().unwrap_or_default(),
                    transform: mark.transform,
                    ..placed(
                        id,
                        mark.name.clone().unwrap_or_default(),
                        mark.shape.clone(),
                        style,
                    )
                }
            })
            .collect();
        add_marks(doc, art, &layer, marks)
    }
}

impl Paint {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        let layer = target_layer(art, self.layer.as_deref())?;
        let mut art = art.clone();
        let clips = self
            .strokes
            .iter()
            .map(|stroke| clip_named(doc, &mut art, &layer, stroke.clip.as_ref()))
            .collect::<Result<Vec<String>, ActionError>>()?;
        let art = &art;
        let marks = self
            .strokes
            .iter()
            .enumerate()
            .map(|(index, stroke)| {
                let id = stroke
                    .id
                    .clone()
                    .unwrap_or_else(|| derived_id("s", stroke, index));
                let style = Style {
                    stroke: (!stroke.colour.is_empty()).then(|| stroke.colour.clone()),
                    brush: stroke.brush.as_deref().and_then(crate::brushes::stored),
                    opacity: stroke.opacity,
                    wet: stroke.wet.unwrap_or(false),
                    params: stroke.params.clone().unwrap_or_default(),
                    angle: stroke.angle,
                    ..Style::default()
                };
                let shape = Shape::Stroke {
                    points: stroke.points.clone(),
                    size: stroke.size,
                    forks: stroke.forks,
                    spread: stroke.spread,
                    dabs: stroke.dabs,
                    scatter: stroke.scatter,
                    dab_length: stroke.dab_length,
                };
                let mut mark = placed(id, stroke.name.clone().unwrap_or_default(), shape, style);
                mark.clip = clips.get(index).cloned().unwrap_or_default();
                mark
            })
            .collect::<Vec<Mark>>();
        for (index, (mark, stroke)) in marks.iter().zip(&self.strokes).enumerate() {
            if let Some(brush) = &stroke.brush
                && crate::brushes::Preset::parse(brush).is_none()
            {
                return Err(refuse(format!(
                    "stroke {index}: brush {brush:?} is not a preset; use one of {}",
                    crate::brushes::Preset::names()
                )));
            }
            if mark.style.stroke.is_none() && !mark.erases() {
                return Err(refuse(format!(
                    "stroke {index}: give a colour (#rrggbb or pigment:<name>); only an eraser paints none"
                )));
            }
        }
        add_marks(doc, art, &layer, marks)
    }
}

/// Why a stroke's preset or dynamics cannot draw it on its layer.
fn check_brush(art: &Artwork, layer: &str, mark: &Mark) -> Result<(), ActionError> {
    if !matches!(mark.shape, Shape::Stroke { .. }) {
        return Ok(());
    }
    let plain = art.layer(layer).is_none_or(|layer| layer.medium.is_empty());
    match crate::brushes::problem(mark.style.brush.as_deref(), &mark.style.params, plain) {
        Some(problem) => Err(refuse(problem)),
        None => Ok(()),
    }
}

/// A colour edit: absent changes nothing; `none` clears the colour.
enum ColourEdit {
    Keep,
    Set(Option<String>),
}

fn colour_patch(value: Option<&String>) -> ColourEdit {
    match value {
        None => ColourEdit::Keep,
        Some(colour) => ColourEdit::Set((colour != "none").then(|| colour.clone())),
    }
}

impl EditMarks {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        self.check_reshape()?;
        if self.merge == Some(true) {
            let kept = crate::strokes::merge(doc, art, &self.ids)?;
            return Self {
                ids: vec![kept],
                merge: None,
                ..self.clone()
            }
            .apply(doc, &doc::read(doc));
        }
        if let Some(at) = self.split {
            let made = crate::strokes::split(doc, art, &self.ids, at)?;
            return Self {
                ids: made,
                split: None,
                ..self.clone()
            }
            .apply(doc, &doc::read(doc));
        }
        let units = units(art, &self.ids)?;
        let count: usize = units.iter().map(Vec::len).sum();
        let destination = match &self.layer {
            Some(layer) => Some(target_layer(art, Some(layer))?),
            None => None,
        };
        let mut orders = destination
            .as_deref()
            .map(|layer| on_top(art, layer, count))
            .unwrap_or_default()
            .into_iter();
        let [dx, dy] = self.move_by.unwrap_or([0.0, 0.0]);
        if !dx.is_finite() || !dy.is_finite() {
            return Err(refuse("move_by must be finite"));
        }
        let scale = self.scale.unwrap_or(1.0);
        let rotate = self.rotate.unwrap_or(0.0);
        if !scale.is_finite() || scale <= 0.0 || !rotate.is_finite() {
            return Err(refuse("scale must be positive and rotate finite"));
        }
        if let Some(problem) = self.shape.as_ref().and_then(Shape::problem) {
            return Err(refuse(problem));
        }
        if let Some(group) = self.group.as_deref().filter(|group| !group.is_empty()) {
            check_group(art, group, &[])?;
        }
        if let Some(transform) = self.transform {
            check_transform(transform)?;
        }
        for unit in units {
            let centre = extent(art, &unit).center() + ui::vello::kurbo::Vec2::new(dx, dy);
            let about = ui::vello::kurbo::Affine::translate(centre.to_vec2())
                * ui::vello::kurbo::Affine::rotate(rotate.to_radians())
                * ui::vello::kurbo::Affine::scale(scale)
                * ui::vello::kurbo::Affine::translate(-centre.to_vec2());
            for original in unit {
                let mut mark = original.clone();
                if let Some(layer) = &destination
                    && *layer != mark.layer
                {
                    mark.layer.clone_from(layer);
                    if let Some(order) = orders.next() {
                        mark.order = order;
                    }
                }
                if let Some(shape) = &self.shape {
                    mark.shape = shape.clone();
                }
                if let Some(group) = &self.group {
                    mark.group.clone_from(group);
                }
                if let Some(clip) = &self.clip {
                    if !clip.is_empty() {
                        check_clip(art, &mark.id, clip, &[])?;
                    }
                    mark.clip.clone_from(clip);
                }
                self.restyle(&mut mark);
                self.reshape(&mut mark);
                if dx != 0.0 || dy != 0.0 {
                    mark = mark.moved(dx, dy);
                }
                if self.scale.is_some() || self.rotate.is_some() {
                    mark.transform = Some((about * mark.affine()).as_coeffs());
                }
                if let Some(transform) = self.transform {
                    mark = mark.transformed(ui::vello::kurbo::Affine::new(transform));
                }
                check_style(art, &mark.layer, &mark.style)
                    .and_then(|()| check_brush(art, &mark.layer, &mark))
                    .map_err(|error| refuse(format!("mark {:?}: {error}", mark.id)))?;
                if let Some(problem) = mark.shape.problem() {
                    return Err(refuse(format!("mark {:?}: {problem}", mark.id)));
                }
                doc::write_mark(doc, &mark)?;
            }
        }
        Ok(())
    }

    /// Refuse reshaping values no stroke can take.
    fn check_reshape(&self) -> Result<(), ActionError> {
        check_unit(self.smooth, "smooth")?;
        if self
            .simplify
            .is_some_and(|tolerance| !(0.0..=MAX_SIDE).contains(&tolerance))
        {
            return Err(refuse("simplify is a distance in units, 0 or more"));
        }
        if self
            .size
            .is_some_and(|size| !size.is_finite() || size <= 0.0 || size > MAX_SIDE)
        {
            return Err(refuse("size must be positive"));
        }
        if self
            .split
            .is_some_and(|[x, y]| !x.is_finite() || !y.is_finite())
        {
            return Err(refuse("split is a finite [x, y]"));
        }
        if let Some(points) = &self.extend
            && (points.is_empty()
                || !points.iter().all(|point| {
                    (2..=3).contains(&point.len()) && point.iter().all(|value| value.is_finite())
                }))
        {
            return Err(refuse("extend is points [[x, y] or [x, y, pressure], …]"));
        }
        Ok(())
    }

    /// The stroke edits, onto `mark`: its width and path.
    fn reshape(&self, mark: &mut Mark) {
        let Shape::Stroke { points, size, .. } = &mut mark.shape else {
            return;
        };
        if let Some(width) = self.size {
            *size = width;
        }
        if let Some(amount) = self.smooth {
            // Laid evenly to smooth it, then as few points as keep its curve.
            let smooth = crate::brushes::smoothed(points, amount);
            *points = crate::brushes::simplified(&smooth, 0.1);
        }
        if let Some(tolerance) = self.simplify {
            *points = crate::brushes::simplified(points, tolerance);
        }
        if let Some(more) = &self.extend {
            crate::strokes::extend(points, more);
        }
    }

    /// The style edits, onto `mark`.
    fn restyle(&self, mark: &mut Mark) {
        if let Some(name) = &self.name {
            mark.name.clone_from(name);
        }
        if let Some(dash) = &self.dash {
            mark.style.dash.clone_from(dash);
        }
        for (edit, field) in [
            (&self.cap, &mut mark.style.cap),
            (&self.join, &mut mark.style.join),
            (&self.blend, &mut mark.style.blend),
        ] {
            if let Some(value) = edit {
                *field = Some(value.clone()).filter(|value| value != "normal");
            }
        }
        for (edit, field) in [
            (&self.shadow, &mut mark.style.shadow),
            (&self.inner_shadow, &mut mark.style.inner_shadow),
        ] {
            if let Some(shadow) = edit {
                *field = (shadow.blur > 0.0 || shadow.offset != [0.0, 0.0]).then(|| shadow.clone());
            }
        }
        if let Some(grain) = self.grain {
            mark.style.grain = (grain > 0.0).then_some(grain);
        }
        if let Some(feather) = self.feather {
            mark.style.feather = (feather > 0.0).then_some(feather);
        }
        {
            let is_stroke = matches!(mark.shape, Shape::Stroke { .. });
            if let ColourEdit::Set(fill) = colour_patch(self.fill.as_ref()) {
                // A stroke's one colour is its stroke colour.
                if is_stroke {
                    mark.style.stroke = fill;
                } else {
                    mark.style.fill = fill;
                    // A gradient is drawn instead of the fill: a new fill
                    // replaces it.
                    mark.style.gradient = None;
                }
            }
            if let ColourEdit::Set(stroke) = colour_patch(self.stroke.as_ref()) {
                mark.style.stroke = stroke;
            }
            if self.gradient.is_some() {
                mark.style.gradient.clone_from(&self.gradient);
            }
            if let Some(width) = self.stroke_width {
                mark.style.stroke_width = width;
            }
            if let Some(opacity) = self.opacity {
                mark.style.opacity = Some(opacity);
            }
            if let Some(brush) = &self.brush {
                mark.style.brush = crate::brushes::stored(brush);
            }
            if self.brush_size.is_some() {
                mark.style.brush_size = self.brush_size;
            }
            if self.angle.is_some() {
                mark.style.angle = self.angle;
            }
            if let Some(wet) = self.wet {
                mark.style.wet = wet;
            }
            if let Some(params) = &self.params {
                mark.style.params.extend(params.clone());
            }
        }
    }
}

/// Where `ids` go when moved `to` the front, back, forward or backward of
/// the ordered `all`, or directly `above` or `below` the entries `of`: new
/// order keys for the moved entries.
fn rearranged(
    all: &[(String, String)],
    ids: &[String],
    to: &str,
    of: &[String],
) -> Result<Vec<(String, String)>, ActionError> {
    let moving: Vec<&(String, String)> = all.iter().filter(|(id, _)| ids.contains(id)).collect();
    let staying: Vec<&(String, String)> = all.iter().filter(|(id, _)| !ids.contains(id)).collect();
    let anchor = || {
        let first = staying.iter().position(|(id, _)| of.contains(id));
        let last = staying.iter().rposition(|(id, _)| of.contains(id));
        first.zip(last).ok_or_else(|| {
            refuse("name the mark or group to go above or below in `of`, on the same layer and not among ids")
        })
    };
    let (low, high) = match to {
        "above" => {
            let (_, top) = anchor()?;
            (
                staying.get(top).map(|(_, order)| order.clone()),
                staying
                    .get(top.saturating_add(1))
                    .map(|(_, order)| order.clone()),
            )
        }
        "below" => {
            let (bottom, _) = anchor()?;
            (
                bottom
                    .checked_sub(1)
                    .and_then(|before| staying.get(before))
                    .map(|(_, order)| order.clone()),
                staying.get(bottom).map(|(_, order)| order.clone()),
            )
        }
        "front" => (staying.last().map(|(_, order)| order.clone()), None),
        "back" => (None, staying.first().map(|(_, order)| order.clone())),
        "forward" | "backward" => {
            // Step past the nearest entry that is not moving.
            let first = all.iter().position(|(id, _)| ids.contains(id)).unwrap_or(0);
            let last = all
                .iter()
                .rposition(|(id, _)| ids.contains(id))
                .unwrap_or(0);
            let over = if to == "forward" {
                all.iter().skip(last).find(|(id, _)| !ids.contains(id))
            } else {
                all.iter()
                    .take(first)
                    .rev()
                    .find(|(id, _)| !ids.contains(id))
            };
            let Some((over, _)) = over else {
                return Ok(Vec::new());
            };
            let at = staying.iter().position(|(id, _)| id == over).unwrap_or(0);
            if to == "forward" {
                (
                    staying.get(at).map(|(_, order)| order.clone()),
                    staying
                        .get(at.saturating_add(1))
                        .map(|(_, order)| order.clone()),
                )
            } else {
                (
                    at.checked_sub(1)
                        .and_then(|before| staying.get(before))
                        .map(|(_, order)| order.clone()),
                    staying.get(at).map(|(_, order)| order.clone()),
                )
            }
        }
        other => {
            return Err(refuse(format!(
                "to {other:?} must be front, back, forward, backward, above or below"
            )));
        }
    };
    let orders = order::sequence(low.as_deref(), high.as_deref(), moving.len());
    Ok(moving
        .into_iter()
        .zip(orders)
        .map(|((id, _), order)| (id.clone(), order))
        .collect())
}

/// Move marks within their layer's stacking order.
fn arrange_marks(doc: &mut Doc, art: &Artwork, arrange: &Arrange) -> Result<(), ActionError> {
    let ids: Vec<String> = units(art, &arrange.ids)?
        .into_iter()
        .flatten()
        .map(|mark| mark.id.clone())
        .collect();
    let Some(first) = ids.first().and_then(|id| art.mark(id)) else {
        return Err(refuse("name existing marks in ids"));
    };
    if let Some(other) = ids
        .iter()
        .find(|id| art.mark(id).is_none_or(|mark| mark.layer != first.layer))
    {
        return Err(refuse(format!(
            "{other:?} is not a mark on layer {:?}; arrange marks of one layer at a time",
            first.layer
        )));
    }
    let all: Vec<(String, String)> = art
        .marks_of(&first.layer)
        .map(|mark| (mark.id.clone(), mark.order.clone()))
        .collect();
    let of: Vec<String> = match &arrange.of {
        Some(of) => units_of(art, of)?
            .into_iter()
            .map(|mark| mark.id.clone())
            .collect(),
        None => Vec::new(),
    };
    for (id, order) in rearranged(&all, &ids, &arrange.to, &of)? {
        if let Some(mark) = art.mark(&id) {
            doc::write_mark(
                doc,
                &Mark {
                    order,
                    ..mark.clone()
                },
            )?;
        }
    }
    Ok(())
}

impl tilo_actions::Action for CanvasAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let art = doc::read(doc);
        match self {
            Self::SetArtwork(set) => set_artwork(doc, set),
            Self::NewArtwork(new) => replace(doc, &new.file()?),
            Self::OpenArtwork(open) => {
                let file =
                    crate::document::ArtworkFile::parse(open.file.as_bytes()).map_err(refuse)?;
                replace(doc, &file)
            }
            Self::AddLayer(add) => add_layer(doc, &art, add),
            Self::EditLayer(edit) => edit_layer(doc, &art, edit),
            Self::RemoveLayer(remove) => {
                if art.layer(&remove.id).is_none() {
                    return Err(refuse(format!(
                        "there is no layer {:?}; the layers are {}",
                        remove.id,
                        layer_list(&art)
                    )));
                }
                // A layer's mask goes with it.
                let mask = art.mask_of(&remove.id).map(|mask| mask.id.clone());
                for layer in std::iter::once(remove.id.clone()).chain(mask) {
                    crate::selection::remove_layer_marks(doc, &art, &layer)?;
                    doc::remove(doc, doc::LAYERS, &layer)?;
                }
                Ok(())
            }
            Self::Draw(draw) => draw.apply(doc, &art),
            Self::Paint(paint) => paint.apply(doc, &art),
            Self::EditMarks(edit) => edit.apply(doc, &art),
            Self::RemoveMarks(remove) => {
                let mut ids: Vec<String> = if remove.ids.is_empty() {
                    Vec::new()
                } else {
                    units(&art, &remove.ids)?
                        .into_iter()
                        .flatten()
                        .map(|mark| mark.id.clone())
                        .collect()
                };
                if let Some(path) = &remove.along {
                    let layer = match &remove.layer {
                        Some(layer) => Some(target_layer(&art, Some(layer))?),
                        None => None,
                    };
                    let crossed = crate::strokes::crossed(&art, path, layer.as_deref());
                    if crossed.is_empty() && ids.is_empty() {
                        return Err(refuse(
                            "along crosses no brush stroke on a shown, unlocked layer; look with grid: true to find them",
                        ));
                    }
                    ids.extend(crossed);
                } else if let Some(layer) = &remove.layer {
                    let layer = target_layer(&art, Some(layer))?;
                    ids.extend(art.marks_of(&layer).map(|mark| mark.id.clone()));
                    if ids.is_empty() {
                        return Ok(());
                    }
                }
                if ids.is_empty() {
                    return Err(refuse(
                        "name marks or groups in ids, a layer to clear, or a path along",
                    ));
                }
                for id in ids {
                    doc::remove(doc, doc::MARKS, &id)?;
                }
                Ok(())
            }
            Self::Arrange(arrange) => arrange_marks(doc, &art, arrange),
            Self::SaveMedium(save) => {
                save.medium.validate().map_err(refuse)?;
                doc::write_medium(doc, &save.medium)?;
                Ok(())
            }
            Self::RemoveMedium(remove) => {
                if !art.media.contains_key(&remove.id) {
                    return Err(refuse(format!(
                        "no medium {:?} is saved in this artwork",
                        remove.id
                    )));
                }
                doc::remove(doc, doc::MEDIA, &remove.id)?;
                Ok(())
            }
            Self::Align(align) => align.apply(doc, &art),
            Self::SetReference(set) => set_reference(doc, &art, set),
            Self::Select(select) => select.apply(doc, &art),
            Self::UseSelection(using) => using.apply(doc, &art),
            Self::Combine(combine) => combine.apply(doc, &art),
        }
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

fn set_artwork(doc: &mut Doc, set: &SetArtwork) -> Result<(), ActionError> {
    use ui::automerge::ROOT;
    use ui::automerge::transaction::Transactable as _;
    for (value, what) in [(set.width, "width"), (set.height, "height")] {
        if let Some(value) = value {
            if !(16.0..=MAX_SIDE).contains(&value) {
                return Err(refuse(format!("{what} must be within 16..={MAX_SIDE}")));
            }
            doc.put(&ROOT, what, value)?;
        }
    }
    if let Some(title) = &set.title {
        doc::put_text(doc, &ROOT, "title", title)?;
    }
    if set.texture.is_some() || set.ground_colour.is_some() {
        let mut ground = doc::read(doc).ground;
        if let Some(texture) = &set.texture {
            if tilo_wet_media::Texture::parse(texture).is_none() {
                return Err(refuse(format!(
                    "texture {texture:?} must be one of {}",
                    tilo_wet_media::Texture::names()
                )));
            }
            ground.texture.clone_from(texture);
        }
        if let Some(colour) = &set.ground_colour {
            if tilo_wet_media::colour::parse_hex(colour).is_none() {
                return Err(refuse(format!("ground_colour {colour:?} must be #rrggbb")));
            }
            ground.colour.clone_from(colour);
        }
        let encoded = serde_json::to_string(&ground).unwrap_or_default();
        doc::put_text(doc, &ROOT, "ground", &encoded)?;
    }
    Ok(())
}

impl NewArtwork {
    /// The blank artwork this starts: a size and a start resolved, each
    /// overridden by what is given exactly.
    ///
    /// # Errors
    /// Names the size, start, side, texture, colour or medium that is not
    /// one of the choices.
    pub fn file(&self) -> Result<crate::document::ArtworkFile, ActionError> {
        use crate::document::{self, ArtworkFile, FileLayer};
        let size = match &self.size {
            Some(id) => Some(document::size(id).ok_or_else(|| {
                refuse(format!(
                    "size {id:?} must be one of {}",
                    document::SIZES.map(|size| size.id).join(", ")
                ))
            })?),
            None => None,
        };
        let start = match &self.start {
            Some(id) => document::start(id).ok_or_else(|| {
                refuse(format!(
                    "start {id:?} must be one of {}",
                    document::STARTS.map(|start| start.id).join(", ")
                ))
            })?,
            None => document::STARTS[0],
        };
        let defaults = Artwork::default();
        let (mut width, mut height) = size.map_or((defaults.width, defaults.height), |size| {
            (size.width, size.height)
        });
        if self
            .landscape
            .is_some_and(|landscape| landscape != (width > height))
        {
            std::mem::swap(&mut width, &mut height);
        }
        let width = self.width.unwrap_or(width);
        let height = self.height.unwrap_or(height);
        for (value, what) in [(width, "width"), (height, "height")] {
            if !(16.0..=MAX_SIDE).contains(&value) {
                return Err(refuse(format!("{what} must be within 16..={MAX_SIDE}")));
            }
        }
        let texture = self.texture.clone().unwrap_or_else(|| start.texture.into());
        if tilo_wet_media::Texture::parse(&texture).is_none() {
            return Err(refuse(format!(
                "texture {texture:?} must be one of {}",
                tilo_wet_media::Texture::names()
            )));
        }
        let colour = self
            .ground_colour
            .clone()
            .unwrap_or_else(|| start.colour.into());
        if tilo_wet_media::colour::parse_hex(&colour).is_none() {
            return Err(refuse(format!("ground_colour {colour:?} must be #rrggbb")));
        }
        let medium = self.medium.clone().unwrap_or_else(|| start.medium.into());
        check_medium(&Artwork::default(), &medium)?;
        let name = if medium.is_empty() {
            "Layer 1".to_owned()
        } else {
            // "Watercolour, washes" names its layer "Watercolour".
            media::resolve(&Artwork::default(), &medium).map_or_else(
                || medium.clone(),
                |spec| {
                    spec.label
                        .split(',')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                },
            )
        };
        Ok(ArtworkFile {
            title: self.title.clone().unwrap_or_default(),
            width,
            height,
            ground: crate::model::Ground { texture, colour },
            layers: vec![FileLayer {
                id: "layer-1".into(),
                name,
                medium,
                opacity: 1.0,
                visible: true,
                texture: 0.0,
                locked: false,
                mask_of: String::new(),
                base: 1.0,
                invert: false,
                clip: false,
            }],
            ..ArtworkFile::of(&Artwork::default())
        })
    }
}

/// Make the artwork `file`: everything the artwork had is removed and the
/// file's title, size, ground, layers, marks and media written in their
/// order.
fn replace(doc: &mut Doc, file: &crate::document::ArtworkFile) -> Result<(), ActionError> {
    use ui::automerge::ROOT;
    use ui::automerge::transaction::Transactable as _;
    for (value, what) in [(file.width, "width"), (file.height, "height")] {
        if !(16.0..=MAX_SIDE).contains(&value) {
            return Err(refuse(format!("{what} must be within 16..={MAX_SIDE}")));
        }
    }
    if file.marks.len() > crate::model::MAX_MARKS {
        return Err(refuse(format!(
            "the artwork has {} marks, more than the {} one artwork holds",
            file.marks.len(),
            crate::model::MAX_MARKS
        )));
    }
    for layer in &file.layers {
        if !valid_id(&layer.id) {
            return Err(refuse(format!(
                "layer id {:?} must be 1-48 of a-z, 0-9, '-' and '_'",
                layer.id
            )));
        }
    }
    for medium in file.media.values() {
        medium.validate().map_err(refuse)?;
    }
    let with_media = Artwork {
        media: file.media.clone(),
        ..Artwork::default()
    };
    for layer in &file.layers {
        check_medium(&with_media, &layer.medium)?;
    }
    for mark in &file.marks {
        if !file.layers.iter().any(|layer| layer.id == mark.layer) {
            return Err(refuse(format!(
                "mark {:?} is on layer {:?}, which the artwork does not have",
                mark.id, mark.layer
            )));
        }
    }
    let old = doc::read(doc);
    for mark in &old.marks {
        doc::remove(doc, doc::MARKS, &mark.id)?;
    }
    for layer in &old.layers {
        doc::remove(doc, doc::LAYERS, &layer.id)?;
    }
    for id in old.media.keys() {
        doc::remove(doc, doc::MEDIA, id)?;
    }
    doc::write_reference(doc, None)?;
    doc::write_selection(doc, None)?;
    doc::put_text(doc, &ROOT, "title", &file.title)?;
    doc.put(&ROOT, "width", file.width)?;
    doc.put(&ROOT, "height", file.height)?;
    let ground = serde_json::to_string(&file.ground).unwrap_or_default();
    doc::put_text(doc, &ROOT, "ground", &ground)?;
    for medium in file.media.values() {
        doc::write_medium(doc, medium)?;
    }
    let mut below: Option<String> = None;
    for layer in &file.layers {
        let order = order::between(below.as_deref(), None);
        doc::write_layer(
            doc,
            &Layer {
                id: layer.id.clone(),
                name: layer.name.clone(),
                order: order.clone(),
                medium: layer.medium.clone(),
                opacity: layer.opacity.clamp(0.0, 1.0),
                visible: layer.visible,
                texture: layer.texture.clamp(0.0, 1.0),
                locked: layer.locked,
                mask_of: layer.mask_of.clone(),
                base: layer.base.clamp(0.0, 1.0),
                invert: layer.invert,
                clip: layer.clip,
            },
        )?;
        below = Some(order);
    }
    let mut last: BTreeMap<&str, String> = BTreeMap::new();
    for mark in &file.marks {
        let order = order::between(last.get(mark.layer.as_str()).map(String::as_str), None);
        doc::write_mark(
            doc,
            &Mark {
                id: mark.id.clone(),
                layer: mark.layer.clone(),
                order: order.clone(),
                name: mark.body.name.clone(),
                shape: mark.body.shape.clone(),
                style: mark.body.style.clone(),
                transform: mark.body.transform,
                group: mark.body.group.clone(),
                clip: mark.body.clip.clone(),
            },
        )?;
        last.insert(&mark.layer, order);
    }
    Ok(())
}

fn check_medium(art: &Artwork, medium: &str) -> Result<(), ActionError> {
    if medium.is_empty() || media::resolve(art, medium).is_some() {
        return Ok(());
    }
    let known: Vec<String> = media::available(art)
        .into_iter()
        .map(|medium| medium.id)
        .collect();
    Err(refuse(format!(
        "there is no medium {medium:?}; the media are {} (or empty for plain)",
        known.join(", ")
    )))
}

fn add_layer(doc: &mut Doc, art: &Artwork, add: &AddLayer) -> Result<(), ActionError> {
    let id = add
        .id
        .clone()
        .unwrap_or_else(|| derived_id("layer", add, art.layers.len()));
    if !valid_id(&id) {
        return Err(refuse(format!(
            "id {id:?} must be 1-48 of a-z, 0-9, '-' and '_'"
        )));
    }
    if art.layer(&id).is_some() {
        return Err(refuse(format!("a layer {id:?} already exists")));
    }
    let medium = add.medium.clone().unwrap_or_default();
    check_medium(art, &medium)?;
    check_unit(add.opacity, "opacity")?;
    check_unit(add.texture, "texture")?;
    let order = match &add.above {
        Some(above) => {
            let at = art
                .layers
                .iter()
                .position(|layer| layer.id == *above)
                .ok_or_else(|| refuse(format!("there is no layer {above:?} to go above")))?;
            order::between(
                art.layers.get(at).map(|layer| layer.order.as_str()),
                art.layers
                    .get(at.saturating_add(1))
                    .map(|layer| layer.order.as_str()),
            )
        }
        None => order::between(art.layers.last().map(|layer| layer.order.as_str()), None),
    };
    doc::write_layer(
        doc,
        &Layer {
            id,
            name: if add.name.trim().is_empty() {
                "Layer".into()
            } else {
                add.name.clone()
            },
            order,
            medium,
            opacity: add.opacity.unwrap_or(1.0),
            visible: true,
            texture: add.texture.unwrap_or(0.0),
            locked: false,
            ..Layer::default()
        },
    )?;
    Ok(())
}

fn edit_layer(doc: &mut Doc, art: &Artwork, edit: &EditLayer) -> Result<(), ActionError> {
    let mut layer = art.layer(&edit.id).cloned().ok_or_else(|| {
        refuse(format!(
            "there is no layer {:?}; the layers are {}",
            edit.id,
            layer_list(art)
        ))
    })?;
    if let Some(name) = &edit.name {
        layer.name.clone_from(name);
    }
    if let Some(medium) = &edit.medium {
        check_medium(art, medium)?;
        if layer.is_mask() && !medium.is_empty() {
            return Err(refuse("a mask is drawn in greys, not painted in a medium"));
        }
        layer.medium.clone_from(medium);
    }
    if let Some(clip) = edit.clip {
        if layer.is_mask() {
            return Err(refuse("a mask cannot be clipped; clip the layer it masks"));
        }
        layer.clip = clip;
    }
    if let Some(invert) = edit.invert {
        // A layer's mask is inverted by naming the layer or the mask.
        match art.mask_of(&layer.id) {
            Some(mask) if !layer.is_mask() => doc::write_layer(
                doc,
                &Layer {
                    invert,
                    ..mask.clone()
                },
            )?,
            _ if layer.is_mask() => layer.invert = invert,
            _ => {
                return Err(refuse(format!(
                    "{:?} has no mask to invert; give it one with mask",
                    layer.id
                )));
            }
        }
    }
    check_unit(edit.opacity, "opacity")?;
    if let Some(opacity) = edit.opacity {
        layer.opacity = opacity;
    }
    if let Some(visible) = edit.visible {
        layer.visible = visible;
    }
    check_unit(edit.texture, "texture")?;
    if let Some(texture) = edit.texture {
        layer.texture = texture;
    }
    if let Some(locked) = edit.locked {
        layer.locked = locked;
    }
    if let Some(to) = &edit.to {
        let all: Vec<(String, String)> = art
            .layers
            .iter()
            .map(|layer| (layer.id.clone(), layer.order.clone()))
            .collect();
        if let Some((_, order)) = rearranged(&all, std::slice::from_ref(&layer.id), to, &[])?.pop()
        {
            layer.order = order;
        }
    }
    doc::write_layer(doc, &layer)?;
    if let Some(mask) = &edit.mask {
        crate::selection::edit_mask(doc, &doc::read(doc), &layer.id, mask)?;
    }
    Ok(())
}

impl Align {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        use ui::vello::kurbo::Rect;
        let units = units(art, &self.ids)?;
        if self.edge.is_none() && self.distribute.is_none() {
            return Err(refuse(
                "give an edge (left, center, right, top, middle, bottom), distribute (horizontal, vertical) or both",
            ));
        }
        let boxes: Vec<Rect> = units.iter().map(|unit| extent(art, unit)).collect();
        let target = match self.to.as_deref() {
            None => boxes
                .iter()
                .copied()
                .reduce(|a, b| a.union(b))
                .unwrap_or_default(),
            Some("artwork") => art.bounds(),
            Some(id) => {
                let unit: Vec<&Mark> = units_of(art, id)?;
                extent(art, &unit)
            }
        };
        let mut shifts = vec![(0.0, 0.0); boxes.len()];
        if let Some(edge) = &self.edge {
            for (shift, b) in shifts.iter_mut().zip(&boxes) {
                match edge.as_str() {
                    "left" => shift.0 = target.x0 - b.x0,
                    "center" => shift.0 = target.center().x - b.center().x,
                    "right" => shift.0 = target.x1 - b.x1,
                    "top" => shift.1 = target.y0 - b.y0,
                    "middle" => shift.1 = target.center().y - b.center().y,
                    "bottom" => shift.1 = target.y1 - b.y1,
                    other => {
                        return Err(refuse(format!(
                            "edge {other:?} must be left, center, right, top, middle or bottom"
                        )));
                    }
                }
            }
        }
        if let Some(axis) = &self.distribute {
            let horizontal = match axis.as_str() {
                "horizontal" => true,
                "vertical" => false,
                other => {
                    return Err(refuse(format!(
                        "distribute {other:?} must be horizontal or vertical"
                    )));
                }
            };
            if boxes.len() < 2 && self.to.is_none() {
                return Err(refuse(
                    "distribute needs two or more marks or groups, or a `to` to spread across",
                ));
            }
            let start = |b: &Rect| if horizontal { b.x0 } else { b.y0 };
            let length = |b: &Rect| if horizontal { b.width() } else { b.height() };
            let mut order: Vec<usize> = (0..boxes.len()).collect();
            order.sort_by(|a, b| {
                let centre =
                    |index: usize| boxes.get(index).map_or(0.0, |b| start(b) + length(b) / 2.0);
                centre(*a).total_cmp(&centre(*b))
            });
            let (from, to) = if self.to.is_some() {
                (start(&target), start(&target) + length(&target))
            } else {
                let all = boxes
                    .iter()
                    .copied()
                    .reduce(|a, b| a.union(b))
                    .unwrap_or_default();
                (start(&all), start(&all) + length(&all))
            };
            let total: f64 = boxes.iter().map(length).sum();
            let gaps = count_f64(boxes.len().saturating_sub(1)).max(1.0);
            let gap = (to - from - total) / gaps;
            let mut cursor = if boxes.len() == 1 {
                from + (to - from - total) / 2.0
            } else {
                from
            };
            for index in order {
                if let (Some(b), Some(shift)) = (boxes.get(index), shifts.get_mut(index)) {
                    let moved = cursor - start(b);
                    if horizontal {
                        shift.0 = moved;
                    } else {
                        shift.1 = moved;
                    }
                    cursor += length(b) + gap;
                }
            }
        }
        for (unit, (dx, dy)) in units.into_iter().zip(shifts) {
            if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
                continue;
            }
            for mark in unit {
                doc::write_mark(doc, &mark.moved(dx, dy))?;
            }
        }
        Ok(())
    }
}

fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The members `id` addresses: a mark, or a group's members.
fn units_of<'a>(art: &'a Artwork, id: &str) -> Result<Vec<&'a Mark>, ActionError> {
    Ok(units(art, &[id.to_owned()])?
        .into_iter()
        .flatten()
        .collect())
}

fn set_reference(doc: &mut Doc, art: &Artwork, set: &SetReference) -> Result<(), ActionError> {
    if set.remove == Some(true) {
        if art.reference.is_some() {
            doc::write_reference(doc, None)?;
        }
        return Ok(());
    }
    let mut reference = match (&art.reference, &set.image) {
        (_, Some(image)) => {
            if ui::tilo_actions::content_hash(image).is_none() {
                return Err(refuse(format!(
                    "image {image:?} must be an absolute path to a PNG or JPEG, a content address, or data:image/png;base64,…"
                )));
            }
            Reference {
                image: image.clone(),
                frame: None,
                opacity: art.reference.as_ref().map_or(0.5, |old| old.opacity),
                visible: true,
            }
        }
        (Some(existing), None) => existing.clone(),
        (None, None) => {
            return Err(refuse(
                "there is no reference yet: give image, a PNG or JPEG path, content address or data: URI",
            ));
        }
    };
    if let Some(frame) = set.frame {
        if !frame
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_SIDE * 4.0)
            || frame[2] <= 0.0
            || frame[3] <= 0.0
        {
            return Err(refuse(
                "frame is [x, y, width, height] with a positive width and height",
            ));
        }
        reference.frame = Some(frame);
    }
    check_unit(set.opacity, "opacity")?;
    if let Some(opacity) = set.opacity {
        reference.opacity = opacity;
    }
    if let Some(visible) = set.visible {
        reference.visible = visible;
    }
    doc::write_reference(doc, Some(&reference))?;
    Ok(())
}
