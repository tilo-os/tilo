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
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{self, Doc, JsonSchema, order};
use ui::vello::kurbo::{BezPath, Rect, Shape as _};

use crate::actions::{add_marks, derived_id, placed, refuse, target_layer, units};
use crate::doc;
use crate::model::{Artwork, Layer, MAX_SIDE, Mark, Selection, Shape, Style, path_data, valid_id};
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
    /// `#rrggbb`, everywhere.
    pub colour: Option<String>,
    /// 0..1, default 0.15.
    pub tolerance: Option<f64>,
    /// `replace`, `add`, `subtract`, `intersect` or `none`.
    pub mode: Option<String>,
    /// Units.
    pub feather: Option<f64>,
    /// Units; negative shrinks.
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
fn mark_region(mark: &Mark) -> Result<Region, ActionError> {
    mark_outline(mark).map(|path| Region::of_path(&path))
}

/// The outline of the area a mark covers, in artwork units.
fn mark_outline(mark: &Mark) -> Result<BezPath, ActionError> {
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
fn named_marks<'a>(art: &'a Artwork, ids: &[String]) -> Result<Vec<&'a Mark>, ActionError> {
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

fn check_box(value: [f64; 4], what: &str) -> Result<Rect, ActionError> {
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
        self.at.is_some() || self.colour.is_some()
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
    fn shapes(&self, art: &Artwork) -> Result<Option<Region>, ActionError> {
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

    pub(crate) fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
        if self.looks() {
            return Err(refuse(
                "at and colour are found in the artwork as the window shows it: use the select action itself, not apply",
            ));
        }
        if self.mode.as_deref() == Some("none") {
            doc::write_selection(doc, None)?;
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
        if let Some(grow) = self.grow {
            region = region.grown(grow);
        }
        if self.invert == Some(true) {
            region = region.inverted(art.bounds());
        }
        let region = region
            .combine(&Region::of_rect(art.bounds()), Op::Intersect)
            .bounded(0.25);
        if region.is_empty() {
            doc::write_selection(doc, None)?;
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
        doc::write_selection(
            doc,
            Some(&Selection {
                d: path_data(&region.to_path()),
                feather,
            }),
        )?;
        Ok(())
    }
}

/// The selection's shape as a mark's geometry.
fn selected_shape(art: &Artwork) -> Result<(Shape, f64), ActionError> {
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
    pub(crate) fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
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
                let masking = ensure_mask(doc, art, owner, if reveal { 0.0 } else { 1.0 })?;
                let art = doc::read(doc);
                let style = Style {
                    fill: Some(if reveal { "#ffffff" } else { "#000000" }.into()),
                    feather: feathered,
                    ..Style::default()
                };
                let mark = placed(id(&self.what), String::new(), shape, style);
                add_marks(doc, &art, &masking, vec![mark])
            }
            other => Err(refuse(format!(
                "do {other:?} must be fill, stroke, mask or clear"
            ))),
        }
    }
}

/// The ordinary layer `id` names: masks have no mask of their own.
fn owner_of<'a>(art: &'a Artwork, id: &str) -> Result<&'a Layer, ActionError> {
    let layer = art
        .layer(id)
        .ok_or_else(|| refuse(format!("there is no layer {id:?}")))?;
    if layer.is_mask() {
        return Err(refuse(format!(
            "{id:?} is the mask of {:?}; name that layer",
            layer.mask_of
        )));
    }
    Ok(layer)
}

/// The mask of `owner`, made with `base` grey (1 shows all of the layer,
/// 0 hides it) when it has none: its id.
fn ensure_mask(
    doc: &mut Doc,
    art: &Artwork,
    owner: &Layer,
    base: f64,
) -> Result<String, ActionError> {
    if let Some(mask) = art.mask_of(&owner.id) {
        return Ok(mask.id.clone());
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
    doc::write_layer(
        doc,
        &Layer {
            id: id.clone(),
            name: format!("{} mask", owner.name),
            order: order::between(Some(&owner.order), above),
            opacity: 1.0,
            visible: true,
            mask_of: owner.id.clone(),
            base,
            ..Layer::default()
        },
    )?;
    Ok(id)
}

/// Give `layer` a mask (`reveal` all, or `hide` all), or `remove` it.
pub(crate) fn edit_mask(
    doc: &mut Doc,
    art: &Artwork,
    layer: &str,
    what: &str,
) -> Result<(), ActionError> {
    let owner = owner_of(art, layer)?;
    match what {
        "reveal" | "hide" => {
            let base = if what == "reveal" { 1.0 } else { 0.0 };
            match art.mask_of(&owner.id) {
                Some(mask) => doc::write_layer(
                    doc,
                    &Layer {
                        base,
                        ..mask.clone()
                    },
                )?,
                None => {
                    ensure_mask(doc, art, owner, base)?;
                }
            }
            Ok(())
        }
        "remove" => {
            if let Some(mask) = art.mask_of(&owner.id) {
                remove_layer_marks(doc, art, &mask.id)?;
                doc::remove(doc, doc::LAYERS, &mask.id)?;
            }
            Ok(())
        }
        other => Err(refuse(format!(
            "mask {other:?} must be reveal, hide or remove"
        ))),
    }
}

/// Remove every mark of `layer`.
pub(crate) fn remove_layer_marks(
    doc: &mut Doc,
    art: &Artwork,
    layer: &str,
) -> Result<(), ActionError> {
    for mark in art.marks_of(layer) {
        doc::remove(doc, doc::MARKS, &mark.id)?;
    }
    Ok(())
}

impl Combine {
    pub(crate) fn apply(&self, doc: &mut Doc, art: &Artwork) -> Result<(), ActionError> {
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
            .collect::<Result<Vec<BezPath>, ActionError>>()?;
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
            doc::remove(doc, doc::MARKS, &mark.id)?;
        }
        doc::write_mark(doc, &combined)?;
        Ok(())
    }
}
