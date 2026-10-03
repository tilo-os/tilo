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
    Adjust, Artwork, BLENDS, Gradient, Layer, MAX_MARKS_PER_ACTION, MAX_SIDE, Mark, Reference,
    Shadow, Shape, StrokePoint, Style, valid_id,
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
    /// Crop to `[x, y, width, height]`: that corner becomes 0, 0 and every
    /// mark moves with it.
    pub crop: Option<[f64; 4]>,
    /// Crop to what this mark or group shows (a photo after its crop).
    pub fit: Option<String>,
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
    /// Colour and tone of everything on it; given steps replace.
    pub adjust: Option<Adjust>,
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
    /// Painted layers: the brush width a filled shape is laid in with.
    pub brush: Option<f64>,
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
    /// Cut to this mark's shape (opacity 0 hides that mark).
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
    /// `#rrggbb[aa]` or `pigment:<name>`.
    pub colour: String,
    pub opacity: Option<f64>,
    /// Paint into what is still wet: blends in oil, bleeds in watercolour.
    pub wet: Option<bool>,
    /// Medium parameter overrides for this stroke (`{"load": 0.2}` for dry
    /// brush).
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
    pub brush: Option<f64>,
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
    /// `horizontal` or `vertical`: mirror about each centre.
    pub flip: Option<String>,
    /// Images: `[x, y, w, h]` fractions of the picture to show; the picture
    /// stays put, its frame follows. `[]` shows it whole.
    pub crop: Option<Vec<f64>>,
    /// Images: degrees to turn the picture in its frame (-45 to 45).
    pub straighten: Option<f64>,
    /// Colour and tone; given steps replace.
    pub adjust: Option<Adjust>,
}

/// Remove marks by id, or every mark of a layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveMarks {
    #[serde(default)]
    pub ids: Vec<String>,
    /// Remove every mark on this layer.
    pub layer: Option<String>,
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
    AddLayer(AddLayer),
    EditLayer(Box<EditLayer>),
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
    Open(Open),
}

/// Start a new artwork from a picture: sized to it, the picture its first
/// mark (`picture`) on its one layer. Everything the artwork held before is
/// replaced, as one step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Open {
    /// The picture's content address.
    pub src: String,
    /// Its size in pixels.
    pub pixels: [u32; 2],
    pub title: String,
}

impl CanvasAction {
    /// An `edit-marks`.
    pub fn edit(edit: EditMarks) -> Self {
        Self::EditMarks(Box::new(edit))
    }
}

fn refuse(message: impl Into<String>) -> ActionError {
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

fn check_colour(art: &Artwork, layer: &str, colour: &str, what: &str) -> Result<(), ActionError> {
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
fn units<'a>(art: &'a Artwork, ids: &[String]) -> Result<Vec<Vec<&'a Mark>>, ActionError> {
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
fn target_layer(art: &Artwork, named: Option<&str>) -> Result<String, ActionError> {
    match named {
        Some(id) => art.layer(id).map(|layer| layer.id.clone()).ok_or_else(|| {
            refuse(format!(
                "there is no layer {id:?}; the layers are {}",
                layer_list(art)
            ))
        }),
        None => art
            .layers
            .last()
            .map(|layer| layer.id.clone())
            .ok_or_else(|| refuse("the artwork has no layer yet; add-layer first")),
    }
}

fn layer_list(art: &Artwork) -> String {
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
fn on_top(art: &Artwork, layer: &str, count: usize) -> Vec<String> {
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
fn add_marks(
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
fn placed(id: String, name: String, shape: Shape, style: Style) -> Mark {
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

impl Draw {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        let layer = target_layer(art, self.layer.as_deref())?;
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
                    brush: mark.brush,
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
                    adjust: None,
                };
                let picture = matches!(mark.shape, Shape::Image { .. });
                let style = if !picture
                    && style.fill.is_none()
                    && style.gradient.is_none()
                    && style.stroke.is_none()
                {
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
                    clip: mark.clip.clone().unwrap_or_default(),
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
                    stroke: Some(stroke.colour.clone()),
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
                mark.clip = stroke.clip.clone().unwrap_or_default();
                mark
            })
            .collect();
        add_marks(doc, art, &layer, marks)
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
        let mirror = match self.flip.as_deref() {
            None => ui::vello::kurbo::Affine::IDENTITY,
            Some("horizontal") => ui::vello::kurbo::Affine::FLIP_X,
            Some("vertical") => ui::vello::kurbo::Affine::FLIP_Y,
            Some(other) => {
                return Err(refuse(format!(
                    "flip {other:?} must be horizontal or vertical"
                )));
            }
        };
        if let Some(problem) = self.adjust.as_ref().and_then(Adjust::problem) {
            return Err(refuse(problem));
        }
        let crop = match self.crop.as_deref() {
            None => None,
            Some([]) => Some([0.0, 0.0, 1.0, 1.0]),
            Some(&[x, y, w, h]) => Some([x, y, w, h]),
            Some(_) => {
                return Err(refuse(
                    "crop is [x, y, w, h] fractions of the picture, or [] for all of it",
                ));
            }
        };
        for unit in units {
            let centre = extent(art, &unit).center() + ui::vello::kurbo::Vec2::new(dx, dy);
            let about = ui::vello::kurbo::Affine::translate(centre.to_vec2())
                * ui::vello::kurbo::Affine::rotate(rotate.to_radians())
                * ui::vello::kurbo::Affine::scale(scale)
                * mirror
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
                self.reframe(&mut mark, crop)?;
                if dx != 0.0 || dy != 0.0 {
                    mark = mark.moved(dx, dy);
                }
                if self.scale.is_some() || self.rotate.is_some() || self.flip.is_some() {
                    mark.transform = Some((about * mark.affine()).as_coeffs());
                }
                if let Some(transform) = self.transform {
                    mark = mark.transformed(ui::vello::kurbo::Affine::new(transform));
                }
                check_style(art, &mark.layer, &mark.style)
                    .map_err(|error| refuse(format!("mark {:?}: {error}", mark.id)))?;
                doc::write_mark(doc, &mark)?;
            }
        }
        Ok(())
    }

    /// An image's crop and straightening, onto `mark`.
    fn reframe(&self, mark: &mut Mark, crop: Option<[f64; 4]>) -> Result<(), ActionError> {
        if crop.is_none() && self.straighten.is_none() {
            return Ok(());
        }
        let Some(mut placement) = crate::picture::Placement::of(&mark.shape) else {
            return Err(refuse(format!(
                "mark {:?} is a {}; crop and straighten are for images",
                mark.id,
                mark.shape.kind()
            )));
        };
        if let Some(crop) = crop {
            placement = placement.with_crop(crop);
        }
        if let Some(angle) = self.straighten {
            placement.straighten = angle;
        }
        placement.apply(&mut mark.shape);
        if let Some(problem) = mark.shape.problem() {
            return Err(refuse(format!("mark {:?}: {problem}", mark.id)));
        }
        Ok(())
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
            if self.brush.is_some() {
                mark.style.brush = self.brush;
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
            if let Some(adjust) = &self.adjust {
                mark.style.adjust = mark.style.adjust.clone().unwrap_or_default().merged(adjust);
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
            Self::SetArtwork(set) => set_artwork(doc, &art, set),
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
                for mark in art.marks_of(&remove.id) {
                    doc::remove(doc, doc::MARKS, &mark.id)?;
                }
                doc::remove(doc, doc::LAYERS, &remove.id)?;
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
                if let Some(layer) = &remove.layer {
                    let layer = target_layer(&art, Some(layer))?;
                    ids.extend(art.marks_of(&layer).map(|mark| mark.id.clone()));
                    if ids.is_empty() {
                        return Ok(());
                    }
                }
                if ids.is_empty() {
                    return Err(refuse("name marks or groups in ids, or a layer to clear"));
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
            Self::Open(open) => open.apply(doc, &art),
        }
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

fn set_artwork(doc: &mut Doc, art: &Artwork, set: &SetArtwork) -> Result<(), ActionError> {
    use ui::automerge::ROOT;
    use ui::automerge::transaction::Transactable as _;
    let crop = match (&set.crop, &set.fit) {
        (Some(_), Some(_)) => return Err(refuse("give crop or fit, not both")),
        (Some([x, y, width, height]), None) => {
            Some(ui::vello::kurbo::Rect::new(*x, *y, x + width, y + height))
        }
        (None, Some(id)) => Some(extent(art, &units_of(art, id)?)),
        (None, None) => None,
    };
    if let Some(crop) = crop {
        crop_artwork(doc, art, crop)?;
    }
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

/// The artwork cut down to `area`: its corner becomes the origin, every
/// mark and the reference moving with it.
fn crop_artwork(
    doc: &mut Doc,
    art: &Artwork,
    area: ui::vello::kurbo::Rect,
) -> Result<(), ActionError> {
    use ui::automerge::ROOT;
    use ui::automerge::transaction::Transactable as _;
    // Whole units, so the picture an export makes is exactly the area.
    let (x0, y0) = (area.x0.round(), area.y0.round());
    let (width, height) = ((area.x1.round() - x0), (area.y1.round() - y0));
    if ![x0, y0, width, height]
        .iter()
        .all(|value| value.is_finite())
        || !(16.0..=MAX_SIDE).contains(&width)
        || !(16.0..=MAX_SIDE).contains(&height)
    {
        return Err(refuse(format!(
            "a crop is [x, y, width, height] with width and height 16 to {MAX_SIDE}"
        )));
    }
    doc.put(&ROOT, "width", width)?;
    doc.put(&ROOT, "height", height)?;
    if x0 != 0.0 || y0 != 0.0 {
        for mark in &art.marks {
            doc::write_mark(doc, &mark.moved(-x0, -y0))?;
        }
        if let Some(reference) = &art.reference
            && let Some([x, y, w, h]) = reference.frame
        {
            let moved = Reference {
                frame: Some([x - x0, y - y0, w, h]),
                ..reference.clone()
            };
            doc::write_reference(doc, Some(&moved))?;
        }
    }
    Ok(())
}

impl Open {
    fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        use ui::automerge::ROOT;
        use ui::automerge::transaction::Transactable as _;
        let [width, height] = self.pixels.map(f64::from);
        if width < 1.0 || height < 1.0 {
            return Err(refuse("the picture has no pixels"));
        }
        // A unit to a pixel, unless the picture is larger than an artwork
        // may be; never smaller than an artwork may be.
        let fit = (MAX_SIDE / width.max(height)).min(1.0);
        let fit = fit.max(16.0 / width.min(height));
        let (width, height) = ((width * fit).round(), (height * fit).round());
        for mark in &art.marks {
            doc::remove(doc, doc::MARKS, &mark.id)?;
        }
        for layer in &art.layers {
            doc::remove(doc, doc::LAYERS, &layer.id)?;
        }
        for medium in art.media.keys() {
            doc::remove(doc, doc::MEDIA, medium)?;
        }
        doc::write_reference(doc, None)?;
        doc.put(&ROOT, "width", width.clamp(16.0, MAX_SIDE))?;
        doc.put(&ROOT, "height", height.clamp(16.0, MAX_SIDE))?;
        doc::put_text(doc, &ROOT, "title", &self.title)?;
        let ground = crate::model::Ground {
            texture: "smooth".into(),
            colour: "#ffffff".into(),
        };
        let encoded = serde_json::to_string(&ground).unwrap_or_default();
        doc::put_text(doc, &ROOT, "ground", &encoded)?;
        let fresh = doc::read(doc);
        add_layer(
            doc,
            &fresh,
            &AddLayer {
                id: Some("layer-1".into()),
                name: "Layer 1".into(),
                ..AddLayer::default()
            },
        )?;
        let fresh = doc::read(doc);
        let shape = Shape::Image {
            src: self.src.clone(),
            x: 0.0,
            y: 0.0,
            width,
            height,
            crop: None,
            straighten: None,
        };
        add_marks(
            doc,
            &fresh,
            "layer-1",
            vec![placed(
                "picture".into(),
                self.title.clone(),
                shape,
                Style::default(),
            )],
        )
    }
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
            adjust: None,
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
        layer.medium.clone_from(medium);
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
    if let Some(adjust) = &edit.adjust {
        if let Some(problem) = adjust.problem() {
            return Err(refuse(problem));
        }
        layer.adjust = layer.adjust.clone().unwrap_or_default().merged(adjust);
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
