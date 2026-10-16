//! Working with areas: selecting part of the artwork, using the selection
//! (fill it, outline it, mask a layer to it, clear it), layer masks, and
//! shapes combined into one.
//!
//! The selection is kept with the artwork like the reference picture: it
//! syncs and undoes, and is never drawn into the artwork. What is made from
//! it is ordinary data: a fill or outline is a path mark, a mask is a layer
//! of grey marks, a stroke kept inside the selection keeps its area. Nothing
//! is a pixel buffer.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use serde::{Deserialize, Serialize};
use ui::tilo_resource::Refusal;
use ui::tilo_resource::{self, JsonSchema, order};
use ui::vello::kurbo::{BezPath, Rect, Shape as _};

use crate::actions::{add_marks, derived_id, placed, refuse, target_layer, units};
use crate::model::{
    Artwork, Layer, MAX_SIDE, Mark, Matte, Selection, Shape, Style, path_data, valid_id,
};
use crate::region::{Op, Region};

/// Ink, for a fill or outline given no colour.
const INK: &str = "#1d1b19"; // a paint colour of the artwork, not chrome

/// Select part of the artwork.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Select {
    /// `[x, y, w, h]`.
    pub rect: Option<[f64; 4]>,
    /// In `[x, y, w, h]`.
    pub ellipse: Option<[f64; 4]>,
    /// Lasso `[[x, y], …]`.
    pub points: Option<Vec<[f64; 2]>>,
    /// SVG path data.
    pub d: Option<String>,
    /// Marks or groups.
    pub marks: Option<Vec<String>>,
    /// Magic wand `[x, y]`: like colour touching it.
    pub at: Option<[f64; 2]>,
    /// The photo's main subject, found on-device (with at: the object
    /// there).
    pub subject: Option<bool>,
    /// Units: fit the edge to the picture, soft for fur (subject: 6px).
    pub refine: Option<f64>,
    /// Set by subject/refine.
    pub matte: Option<Matte>,
    /// `#rrggbb`, everywhere.
    pub colour: Option<String>,
    /// 0..1, default 0.15.
    pub tolerance: Option<f64>,
    /// at or colour: on this layer alone, not as all show.
    pub layer: Option<String>,
    /// `replace`, `add`, `subtract`, `intersect` or `none` (deselect).
    pub mode: Option<String>,
    /// Units.
    pub feather: Option<f64>,
    /// Units, after invert; negative shrinks.
    pub grow: Option<f64>,
    pub invert: Option<bool>,
}

/// Make something of the selection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseSelection {
    /// `fill`, `stroke`, `mask` or `clear`.
    #[serde(rename = "do")]
    pub what: String,
    pub layer: Option<String>,
    pub colour: Option<String>,
    /// Stroke width (2).
    pub width: Option<f64>,
    /// The new mark's.
    pub id: Option<String>,
    /// mask/clear by a refined edge: clean the old background out of the
    /// pictures' edge (default true).
    pub defringe: Option<bool>,
    /// Set by defringe.
    pub cleaned: Option<Vec<[String; 2]>>,
}

/// Combine shapes into one path mark.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Combine {
    /// Marks or groups.
    pub ids: Vec<String>,
    /// `union`, `subtract` (from the bottom one), `intersect` or `exclude`.
    pub op: String,
}

/// The region a mark covers in artwork units: a filled shape's inside, an
/// outline's or a brush stroke's ink.
fn mark_region(mark: &Mark) -> Result<Region, Refusal> {
    mark_outline(mark).map(|path| Region::of_path(&path))
}

/// The outline of the area a mark covers, in artwork units.
pub(crate) fn mark_outline(mark: &Mark) -> Result<BezPath, Refusal> {
    use ui::vello::kurbo::{Cap, Join, Stroke, StrokeOpts};
    let filled = mark.style.fill.is_some() || mark.style.gradient.is_some() || !mark.inked();
    let path: BezPath = match &mark.shape {
        Shape::Text { .. } => {
            return Err(refuse(format!(
                "{:?} is text; only shapes and strokes have an outline to use",
                mark.id
            )));
        }
        Shape::Stroke { .. } => crate::render::silhouette(mark),
        shape if shape.closed() && filled => shape.path(),
        shape => {
            let width = mark.style.stroke_width.max(1.0);
            let pen = Stroke::new(width)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            ui::vello::kurbo::stroke(shape.path(), &pen, &StrokeOpts::default(), 0.05)
        }
    };
    let mut path = path;
    path.apply_affine(mark.affine());
    Ok(path)
}

/// The marks `ids` name, bottom to top.
fn named_marks<'a>(art: &'a Artwork, ids: &[String]) -> Result<Vec<&'a Mark>, Refusal> {
    let named: Vec<&str> = units(art, ids)?
        .into_iter()
        .flatten()
        .map(|mark| mark.id.as_str())
        .collect();
    Ok(art
        .marks
        .iter()
        .filter(|mark| named.contains(&mark.id.as_str()))
        .collect())
}

fn check_box(value: [f64; 4], what: &str) -> Result<Rect, Refusal> {
    let [x, y, width, height] = value;
    if [x, y, width, height]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SIDE * 4.0)
        && width > 0.0
        && height > 0.0
    {
        Ok(Rect::new(x, y, x + width, y + height))
    } else {
        Err(refuse(format!(
            "{what} is [x, y, width, height] with a positive width and height"
        )))
    }
}

impl Select {
    /// Whether it asks for colour seen in the artwork, which only the
    /// window that shows it can find.
    pub fn looks(&self) -> bool {
        self.at.is_some()
            || self.colour.is_some()
            || self.subject == Some(true)
            || self.refine.is_some()
    }

    /// Whether it only changes the selection there is (grow, feather,
    /// invert) rather than naming a new area.
    fn looks_unchanged(&self) -> bool {
        self.rect.is_none()
            && self.ellipse.is_none()
            && self.points.is_none()
            && self.d.is_none()
            && self.marks.is_none()
    }

    /// The area the shapes it names cover, if it names any.
    fn shapes(&self, art: &Artwork) -> Result<Option<Region>, Refusal> {
        let mut parts: Vec<Region> = Vec::new();
        if let Some(rect) = self.rect {
            parts.push(Region::of_rect(check_box(rect, "rect")?));
        }
        if let Some(bounds) = self.ellipse {
            let bounds = check_box(bounds, "ellipse")?;
            let ellipse = ui::vello::kurbo::Ellipse::from_rect(bounds);
            parts.push(Region::of_path(&ellipse.to_path(0.05)));
        }
        if let Some(points) = &self.points {
            let shape = Shape::Polygon {
                points: points.clone(),
            };
            if let Some(problem) = shape.problem() {
                return Err(refuse(format!("points: {problem}")));
            }
            parts.push(Region::of_path(&shape.path()));
        }
        if let Some(d) = &self.d {
            let path = BezPath::from_svg(d)
                .map_err(|error| refuse(format!("d is not SVG path data: {error}")))?;
            parts.push(Region::of_path(&path));
        }
        if let Some(ids) = &self.marks {
            for mark in named_marks(art, ids)? {
                parts.push(mark_region(mark)?);
            }
        }
        Ok(parts
            .into_iter()
            .reduce(|all, part| all.combine(&part, Op::Union)))
    }

    pub(crate) fn apply(&self, doc: &mut Artwork, art: &Artwork) -> Result<(), Refusal> {
        if self.looks() {
            return Err(refuse(
                "at, colour, subject and refine are found in the artwork as the window shows it, where select is invoked; subject and refine through find-area",
            ));
        }
        if self.mode.as_deref() == Some("none") {
            doc.selection = None;
            return Ok(());
        }
        let shapes = self.shapes(art)?;
        if shapes.is_none()
            && self.invert.is_none()
            && self.grow.is_none()
            && self.feather.is_none()
        {
            return Err(refuse(
                "give rect, ellipse, points, d, marks, at or colour to select; grow, feather or invert to change the selection; or mode none",
            ));
        }
        for (value, what) in [(self.feather, "feather"), (self.grow, "grow")] {
            if value.is_some_and(|value| !value.is_finite() || value.abs() > 500.0) {
                return Err(refuse(format!("{what} is -500 to 500 units")));
            }
        }
        if self.feather.is_some_and(|feather| feather < 0.0) {
            return Err(refuse("feather is 0 to 500 units"));
        }
        let current = art.selection.as_ref();
        let had = current.map(Selection::region).unwrap_or_default();
        let mode = self.mode.as_deref().unwrap_or("replace");
        let mut region = match (shapes, mode) {
            (None, _) => had,
            (Some(shapes), "replace") => shapes,
            (Some(shapes), "add") => had.combine(&shapes, Op::Union),
            (Some(shapes), "subtract") => had.combine(&shapes, Op::Subtract),
            (Some(shapes), "intersect") => had.combine(&shapes, Op::Intersect),
            (Some(_), other) => {
                return Err(refuse(format!(
                    "mode {other:?} must be replace, add, subtract, intersect or none"
                )));
            }
        };
        // Invert first: grow and feather refine the area that ends up
        // selected, so `invert, grow -3` keeps a subject clear of its old
        // background's edge.
        if self.invert == Some(true) {
            region = region.inverted(art.bounds());
        }
        if let Some(grow) = self.grow {
            region = region.grown(grow);
        }
        let region = region
            .combine(&Region::of_rect(art.bounds()), Op::Intersect)
            .bounded(0.25);
        if region.is_empty() {
            doc.selection = None;
            return Ok(());
        }
        // A new selection starts sharp; a changed one keeps its feather.
        let feather = self.feather.unwrap_or_else(|| {
            if mode == "replace" && !self.looks_unchanged() {
                0.0
            } else {
                current.map_or(0.0, |selection| selection.feather)
            }
        });
        // A soft edge found for this area goes with it while it is the
        // area it was found for: a new selection, not inverted or grown.
        let matte = self
            .matte
            .clone()
            .filter(|_| mode == "replace" && self.invert != Some(true) && self.grow.is_none());
        if let Some(Matte { src, rect }) = &matte
            && (ui::tilo_actions::content_hash(src).is_none()
                || !rect.iter().all(|value| value.is_finite())
                || rect[2] <= 0.0
                || rect[3] <= 0.0)
        {
            return Err(refuse("a matte is a content address and a box"));
        }
        doc.selection = Some(Selection {
            d: path_data(&region.to_path()),
            feather,
            matte,
        });
        Ok(())
    }
}

/// The selection's shape as a mark's geometry.
fn selected_shape(art: &Artwork) -> Result<(Shape, f64), Refusal> {
    let selection = art
        .selection
        .as_ref()
        .ok_or_else(|| refuse("nothing is selected; select part of the artwork first"))?;
    Ok((
        Shape::Path {
            d: selection.d.clone(),
        },
        selection.feather,
    ))
}

impl UseSelection {
    pub(crate) fn apply(&self, doc: &mut Artwork, art: &Artwork) -> Result<(), Refusal> {
        if let Some(cleaned) = self
            .cleaned
            .as_deref()
            .filter(|cleaned| !cleaned.is_empty())
        {
            give_pictures(doc, art, cleaned)?;
            let art = doc.settled();
            return Self {
                cleaned: None,
                ..self.clone()
            }
            .apply(doc, &art);
        }
        let (shape, feather) = selected_shape(art)?;
        let layer_id = target_layer(art, self.layer.as_deref())?;
        let feathered = (feather > 0.0).then_some(feather);
        let id = |what: &str| {
            self.id
                .clone()
                .unwrap_or_else(|| derived_id(what, &(&shape, &self.colour, &layer_id), 0))
        };
        match self.what.as_str() {
            "fill" => {
                let style = Style {
                    fill: Some(self.colour.clone().unwrap_or_else(|| INK.into())),
                    feather: feathered,
                    ..Style::default()
                };
                let mark = placed(id("fill"), String::new(), shape, style);
                add_marks(doc, art, &layer_id, vec![mark])
            }
            "stroke" => {
                let width = self.width.unwrap_or(2.0);
                if !width.is_finite() || width <= 0.0 || width > MAX_SIDE {
                    return Err(refuse("width must be positive"));
                }
                let style = Style {
                    stroke: Some(self.colour.clone().unwrap_or_else(|| INK.into())),
                    stroke_width: width,
                    ..Style::default()
                };
                let mark = placed(id("outline"), String::new(), shape, style);
                add_marks(doc, art, &layer_id, vec![mark])
            }
            "mask" | "clear" => {
                let reveal = self.what == "mask";
                let owner = owner_of(art, &layer_id)?;
                let masking = ensure_mask(doc, art, owner, if reveal { 0.0 } else { 1.0 });
                let art = doc.settled();
                let style = Style {
                    fill: Some(if reveal { "#ffffff" } else { "#000000" }.into()),
                    feather: feathered,
                    ..Style::default()
                };
                // A refined edge is drawn as it is, soft where the subject
                // is partly there: white for what shows, its inverse black
                // for what is cleared.
                let matte = art
                    .selection
                    .as_ref()
                    .and_then(|selection| selection.matte.clone());
                let id = id(&self.what);
                let (shape, style) = match matte {
                    Some(Matte {
                        src,
                        rect: [x, y, width, height],
                    }) => (
                        Shape::Image {
                            src,
                            x,
                            y,
                            width,
                            height,
                            crop: None,
                            straighten: None,
                        },
                        Style {
                            adjust: (!reveal).then(|| crate::model::Adjust {
                                invert: Some(true),
                                ..crate::model::Adjust::default()
                            }),
                            ..Style::default()
                        },
                    ),
                    None => (shape, style),
                };
                let mark = placed(id, String::new(), shape, style);
                add_marks(doc, &art, &masking, vec![mark])
            }
            other => Err(refuse(format!(
                "do {other:?} must be fill, stroke, mask or clear"
            ))),
        }
    }
}

/// Give each named picture mark its cleaned picture.
fn give_pictures(
    doc: &mut Artwork,
    art: &Artwork,
    pictures: &[[String; 2]],
) -> Result<(), Refusal> {
    for [id, src] in pictures {
        if ui::tilo_actions::content_hash(src).is_none() {
            return Err(refuse(format!("{src:?} is not a content address")));
        }
        let mark = art
            .marks
            .iter()
            .find(|mark| &mark.id == id)
            .ok_or_else(|| refuse(format!("there is no mark {id:?}")))?;
        let mut cleaned = mark.clone();
        match &mut cleaned.shape {
            Shape::Image { src: picture, .. } => picture.clone_from(src),
            _ => return Err(refuse(format!("{id:?} is not a picture"))),
        }
        doc.put_mark(cleaned.clone());
    }
    Ok(())
}

/// The ordinary layer `id` names, or the one it masks when it names a
/// mask: a mask is part of its layer, as the layers panel shows it.
fn owner_of<'a>(art: &'a Artwork, id: &str) -> Result<&'a Layer, Refusal> {
    let layer = art
        .layer(id)
        .ok_or_else(|| refuse(format!("there is no layer {id:?}")))?;
    if layer.is_mask() {
        return art
            .layer(&layer.mask_of)
            .ok_or_else(|| refuse(format!("{id:?} masks {:?}, which is gone", layer.mask_of)));
    }
    Ok(layer)
}

/// The mask of `owner`, made with `base` grey (1 shows all of the layer,
/// 0 hides it) when it has none: its id.
fn ensure_mask(doc: &mut Artwork, art: &Artwork, owner: &Layer, base: f64) -> String {
    if let Some(mask) = art.mask_of(&owner.id) {
        return mask.id.clone();
    }
    let named = format!("{}-mask", owner.id);
    let id = if valid_id(&named) && art.layer(&named).is_none() {
        named
    } else {
        derived_id("mask", &owner.id, art.layers.len())
    };
    let above = art
        .layers
        .iter()
        .position(|layer| layer.id == owner.id)
        .and_then(|at| art.layers.get(at.saturating_add(1)))
        .map(|layer| layer.order.as_str());
    doc.put_layer(Layer {
        id: id.clone(),
        name: format!("{} mask", owner.name),
        order: order::between(Some(&owner.order), above),
        opacity: 1.0,
        visible: true,
        mask_of: owner.id.clone(),
        base,
        ..Layer::default()
    });
    id
}

/// Give `layer` a mask (`reveal` all, or `hide` all), `remove` it, or
/// `unlink` it from (or `link` it to) the layer's marks as they move.
pub(crate) fn edit_mask(
    doc: &mut Artwork,
    art: &Artwork,
    layer: &str,
    what: &str,
) -> Result<(), Refusal> {
    let owner = owner_of(art, layer)?;
    match what {
        "reveal" | "hide" => {
            let base = if what == "reveal" { 1.0 } else { 0.0 };
            match art.mask_of(&owner.id) {
                Some(mask) => doc.put_layer(Layer {
                    base,
                    ..mask.clone()
                }),
                None => {
                    ensure_mask(doc, art, owner, base);
                }
            }
            Ok(())
        }
        "remove" => {
            if let Some(mask) = art.mask_of(&owner.id) {
                remove_layer_marks(doc, art, &mask.id);
                doc.drop_layer(&mask.id);
            }
            Ok(())
        }
        "link" | "unlink" => {
            let mask = art.mask_of(&owner.id).ok_or_else(|| {
                refuse(format!(
                    "{:?} has no mask to {what}; give it one with mask reveal or hide",
                    owner.id
                ))
            })?;
            doc.put_layer(Layer {
                unlinked: what == "unlink",
                ..mask.clone()
            });
            Ok(())
        }
        other => Err(refuse(format!(
            "mask {other:?} must be reveal, hide, remove, link or unlink"
        ))),
    }
}

/// Remove every mark of `layer`.
pub(crate) fn remove_layer_marks(doc: &mut Artwork, art: &Artwork, layer: &str) {
    for mark in art.marks_of(layer) {
        doc.remove_mark(&mark.id);
    }
}

impl Combine {
    pub(crate) fn apply(&self, doc: &mut Artwork, art: &Artwork) -> Result<(), Refusal> {
        let op = Op::parse(&self.op).ok_or_else(|| {
            refuse(format!(
                "op {:?} must be union, subtract, intersect or exclude",
                self.op
            ))
        })?;
        let marks = named_marks(art, &self.ids)?;
        let [bottom, rest @ ..] = marks.as_slice() else {
            return Err(refuse("name the marks to combine in ids"));
        };
        if rest.is_empty() {
            return Err(refuse("name two or more marks to combine"));
        }
        let outlines = marks
            .iter()
            .map(|mark| mark_outline(mark))
            .collect::<Result<Vec<BezPath>, Refusal>>()?;
        // Subtract takes everything above from the bottom shape.
        let region = outlines
            .iter()
            .map(Region::of_path)
            .reduce(|all, next| all.combine(&next, op))
            .unwrap_or_default();
        if region.is_empty() {
            return Err(refuse(format!(
                "{} leaves nothing of those shapes",
                self.op
            )));
        }
        let path = region.to_curves(&outlines);
        let mut style = bottom.style.clone();
        if style.fill.is_none() && style.gradient.is_none() {
            // An outline combined is the area its ink covered.
            style.fill = style.stroke.take();
            style.stroke_width = 0.0;
        }
        if let (Some(gradient), Some(transform)) = (&mut style.gradient, bottom.transform) {
            let affine = ui::vello::kurbo::Affine::new(transform);
            for point in [&mut gradient.from, &mut gradient.to] {
                let at = affine * ui::vello::kurbo::Point::new(point[0], point[1]);
                *point = [at.x, at.y];
            }
        }
        let combined = Mark {
            shape: Shape::Path {
                d: path_data(&path),
            },
            style,
            transform: None,
            ..(*bottom).clone()
        };
        for mark in rest {
            doc.remove_mark(&mark.id);
        }
        doc.put_mark(combined);
        Ok(())
    }
}
