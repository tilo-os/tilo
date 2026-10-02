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
    Artwork, Gradient, Layer, MAX_MARKS_PER_ACTION, MAX_SIDE, Mark, Shape, StrokePoint, Style,
    valid_id,
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
    /// `smooth`, `paper`, `cold-press`, `rough` or `linen`.
    pub texture: Option<String>,
    /// Ground colour, `#rrggbb`.
    pub ground_colour: Option<String>,
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
    /// `front`, `back`, `forward` or `backward` among the layers.
    pub to: Option<String>,
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    pub ids: Vec<String>,
    pub name: Option<String>,
    /// A colour, or `none`.
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
    /// Scale about each mark's centre (`1.5` is half again as large).
    pub scale: Option<f64>,
    /// Rotate about each mark's centre, in degrees clockwise.
    pub rotate: Option<f64>,
    /// Move the marks onto this layer, on top.
    pub layer: Option<String>,
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
    /// `front`, `back`, `forward` or `backward`.
    pub to: String,
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

/// One edit of the artwork.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum CanvasAction {
    SetArtwork(SetArtwork),
    AddLayer(AddLayer),
    EditLayer(EditLayer),
    RemoveLayer(RemoveLayer),
    Draw(Draw),
    Paint(Paint),
    EditMarks(EditMarks),
    RemoveMarks(RemoveMarks),
    Arrange(Arrange),
    SaveMedium(SaveMedium),
    RemoveMedium(RemoveMedium),
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

fn add_marks(
    doc: &mut Doc,
    art: &Artwork,
    layer: &str,
    marks: Vec<(String, String, Shape, Style)>,
) -> Result<(), ActionError> {
    if marks.is_empty() {
        return Err(refuse("give at least one mark"));
    }
    if marks.len() > MAX_MARKS_PER_ACTION {
        return Err(refuse(format!(
            "at most {MAX_MARKS_PER_ACTION} marks per action; split the passage"
        )));
    }
    let ids: Vec<String> = marks.iter().map(|(id, ..)| id.clone()).collect();
    unique_ids(art, &ids)?;
    for (index, (_, _, shape, style)) in marks.iter().enumerate() {
        if let Some(problem) = shape.problem() {
            return Err(refuse(format!("mark {index}: {problem}")));
        }
        check_style(art, layer, style).map_err(|error| refuse(format!("mark {index}: {error}")))?;
    }
    let orders = on_top(art, layer, marks.len());
    for ((id, name, shape, style), order) in marks.into_iter().zip(orders) {
        doc::write_mark(
            doc,
            &Mark {
                id,
                layer: layer.to_owned(),
                order,
                name,
                shape,
                style,
                transform: None,
            },
        )?;
    }
    Ok(())
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
                (
                    id,
                    mark.name.clone().unwrap_or_default(),
                    mark.shape.clone(),
                    style,
                )
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
                    ..Style::default()
                };
                let shape = Shape::Stroke {
                    points: stroke.points.clone(),
                    size: stroke.size,
                };
                (id, stroke.name.clone().unwrap_or_default(), shape, style)
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
        if self.ids.is_empty() {
            return Err(refuse("name the marks to change in ids"));
        }
        let destination = match &self.layer {
            Some(layer) => Some(target_layer(art, Some(layer))?),
            None => None,
        };
        let mut orders = destination
            .as_deref()
            .map(|layer| on_top(art, layer, self.ids.len()))
            .unwrap_or_default()
            .into_iter();
        for id in &self.ids {
            let mut mark = art
                .mark(id)
                .cloned()
                .ok_or_else(|| refuse(format!("there is no mark {id:?}")))?;
            if let Some(layer) = &destination
                && *layer != mark.layer
            {
                mark.layer.clone_from(layer);
                if let Some(order) = orders.next() {
                    mark.order = order;
                }
            }
            if let Some(name) = &self.name {
                mark.name.clone_from(name);
            }
            let is_stroke = matches!(mark.shape, Shape::Stroke { .. });
            if let ColourEdit::Set(fill) = colour_patch(self.fill.as_ref()) {
                // A stroke's one colour is its stroke colour.
                if is_stroke {
                    mark.style.stroke = fill;
                } else {
                    mark.style.fill = fill;
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
            if let Some([dx, dy]) = self.move_by {
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(refuse("move_by must be finite"));
                }
                mark.shape = mark.shape.translated(dx, dy);
            }
            if self.scale.is_some() || self.rotate.is_some() {
                let scale = self.scale.unwrap_or(1.0);
                let rotate = self.rotate.unwrap_or(0.0);
                if !scale.is_finite() || scale <= 0.0 || !rotate.is_finite() {
                    return Err(refuse("scale must be positive and rotate finite"));
                }
                let centre = mark.bounds().center();
                let about = ui::vello::kurbo::Affine::translate(centre.to_vec2())
                    * ui::vello::kurbo::Affine::rotate(rotate.to_radians())
                    * ui::vello::kurbo::Affine::scale(scale)
                    * ui::vello::kurbo::Affine::translate(-centre.to_vec2());
                mark.transform = Some((about * mark.affine()).as_coeffs());
            }
            check_style(art, &mark.layer, &mark.style)
                .map_err(|error| refuse(format!("mark {id:?}: {error}")))?;
            doc::write_mark(doc, &mark)?;
        }
        Ok(())
    }
}

/// Where `ids` go when moved `to` the front, back, forward or backward of
/// the ordered `all`: new order keys for the moved entries.
fn rearranged(
    all: &[(String, String)],
    ids: &[String],
    to: &str,
) -> Result<Vec<(String, String)>, ActionError> {
    let moving: Vec<&(String, String)> = all.iter().filter(|(id, _)| ids.contains(id)).collect();
    let staying: Vec<&(String, String)> = all.iter().filter(|(id, _)| !ids.contains(id)).collect();
    let (low, high) = match to {
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
                "to {other:?} must be front, back, forward or backward"
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

impl tilo_actions::Action for CanvasAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let art = doc::read(doc);
        match self {
            Self::SetArtwork(set) => set_artwork(doc, set),
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
                let mut ids = remove.ids.clone();
                if let Some(layer) = &remove.layer {
                    let layer = target_layer(&art, Some(layer))?;
                    ids.extend(art.marks_of(&layer).map(|mark| mark.id.clone()));
                }
                if ids.is_empty() {
                    return Err(refuse("name marks in ids, or a layer to clear"));
                }
                if let Some(missing) = ids.iter().find(|id| art.mark(id).is_none()) {
                    return Err(refuse(format!("there is no mark {missing:?}")));
                }
                for id in ids {
                    doc::remove(doc, doc::MARKS, &id)?;
                }
                Ok(())
            }
            Self::Arrange(arrange) => {
                let Some(first) = arrange.ids.first().and_then(|id| art.mark(id)) else {
                    return Err(refuse("name existing marks in ids"));
                };
                if let Some(other) = arrange
                    .ids
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
                for (id, order) in rearranged(&all, &arrange.ids, &arrange.to)? {
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
                    "texture {texture:?} must be smooth, paper, cold-press, rough or linen"
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
    if let Some(to) = &edit.to {
        let all: Vec<(String, String)> = art
            .layers
            .iter()
            .map(|layer| (layer.id.clone(), layer.order.clone()))
            .collect();
        if let Some((_, order)) = rearranged(&all, std::slice::from_ref(&layer.id), to)?.pop() {
            layer.order = order;
        }
    }
    doc::write_layer(doc, &layer)?;
    Ok(())
}
