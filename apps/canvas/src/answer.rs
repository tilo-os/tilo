//! What an edit answers its caller: the ids of the marks it made, the area
//! it selected, and a warning when what it drew cannot be seen. A compact
//! summary read from the document the edit left, so an agent needs no
//! second look to know what happened or to address what it made.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's float geometry and sample counts bounded by MOST_SAMPLES do not overflow"
)]

use serde_json::{Value, json};
use ui::automerge::{Patch, PatchAction, Prop};
use ui::num::f64_of_usize;
use ui::tilo_resource::Doc;
use ui::vello::kurbo::{BezPath, Point, Rect, Shape as _};

use crate::model::{Artwork, Layer, Mark, Selection};

/// How many points of the new marks are tested against their layer's mask,
/// spread over all of them.
const MOST_SAMPLES: usize = 400;
/// A mark this much hidden (`0..=1`, averaged over its area) is reported.
const MOSTLY: f64 = 0.75;

pub fn answer(doc: &Doc, action: &str, changes: &[Patch]) -> Option<Value> {
    match action {
        "select" => Some(json!({"selection": selection(&Artwork::from_doc(doc).ok()?)})),
        "paint" | "draw" | "chart" | "import-svg" | "use-selection" => {
            let added = added(changes);
            if added.is_empty() {
                return None;
            }
            let art = Artwork::from_doc(doc).ok()?;
            // Bottom first, the order they were given in.
            let marks: Vec<&Mark> = art
                .marks
                .iter()
                .filter(|mark| added.contains(&mark.id))
                .collect();
            let mut answer = json!({
                "added": marks.iter().map(|mark| mark.id.as_str()).collect::<Vec<_>>(),
            });
            if let (Some(warning), Some(fields)) = (hidden(&art, &marks), answer.as_object_mut()) {
                fields.insert("hidden".into(), warning.into());
            }
            Some(answer)
        }
        _ => None,
    }
}

/// The marks `changes` created: entries put into the marks map.
fn added(changes: &[Patch]) -> Vec<String> {
    changes
        .iter()
        .filter(|patch| matches!(patch.path.as_slice(), [(_, Prop::Map(map))] if map == "marks"))
        .filter_map(|patch| match &patch.action {
            PatchAction::PutMap { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect()
}

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The selection in brief: the box around it, how much it covers and in
/// how many separate parts, its feather; null when nothing is selected.
pub fn selection(art: &Artwork) -> Value {
    let Some(selection) = &art.selection else {
        return Value::Null;
    };
    let region = Selection::region(selection);
    let Some(bounds) = region.bounds() else {
        return Value::Null;
    };
    let mut summary = json!({
        "bounds": [rounded(bounds.x0), rounded(bounds.y0), rounded(bounds.width()), rounded(bounds.height())],
        "area": region.area().round(),
        "of_artwork": rounded(100.0 * region.area() / (art.width * art.height).max(1.0)) / 100.0,
        "parts": region.parts(),
    });
    if selection.feather > 0.0
        && let Some(fields) = summary.as_object_mut()
    {
        fields.insert("feather".into(), rounded(selection.feather).into());
    }
    if selection.matte.is_some()
        && let Some(fields) = summary.as_object_mut()
    {
        // A mask made of it is soft where the picture's edge is.
        fields.insert("edge".into(), "refined".into());
    }
    summary
}

/// Why the new `marks` cannot be seen, if most of one cannot: their layer
/// is hidden, or its mask hides where they lie.
fn hidden(art: &Artwork, marks: &[&Mark]) -> Option<String> {
    let first = marks.first()?;
    let layer = art.layer(&first.layer)?;
    if layer.is_mask() {
        return None;
    }
    if !layer.visible || layer.opacity <= 0.0 {
        return Some(format!(
            "layer {:?} is hidden; edit-layer visible: true (and opacity) shows it",
            layer.id
        ));
    }
    let mask = art.mask_of(&layer.id).filter(|mask| mask.visible)?;
    let greys = Greys::of(art, mask);
    let per_mark = (MOST_SAMPLES / marks.len()).clamp(4, 64);
    let mostly = marks
        .iter()
        .filter(|mark| {
            let points = inside(mark, per_mark);
            !points.is_empty()
                && points
                    .iter()
                    .map(|point| 1.0 - greys.at(*point))
                    .sum::<f64>()
                    / f64_of_usize(points.len())
                    >= MOSTLY
        })
        .count();
    (mostly > 0).then(|| {
        format!(
            "{mostly} of {} lie where {:?}'s mask hides them; draw on another layer, or paint the mask white there",
            marks.len(),
            layer.id
        )
    })
}

/// About `wanted` points spread over the area `mark` covers.
fn inside(mark: &Mark, wanted: usize) -> Vec<Point> {
    let outline = crate::selection::mark_outline(mark).ok();
    let bounds = outline
        .as_ref()
        .map_or_else(|| mark.bounds(), BezPath::bounding_box);
    let steps = (2..=wanted.max(2))
        .find(|steps| steps * steps >= wanted)
        .unwrap_or(2);
    let side = f64_of_usize(steps);
    let grid = |at: f64, from: f64, length: f64| from + length * (at + 0.5) / side;
    let mut points = Vec::new();
    for row in 0..steps {
        for column in 0..steps {
            let point = Point::new(
                grid(f64_of_usize(column), bounds.x0, bounds.width()),
                grid(f64_of_usize(row), bounds.y0, bounds.height()),
            );
            if outline
                .as_ref()
                .is_none_or(|outline| outline.contains(point))
            {
                points.push(point);
            }
        }
    }
    if points.is_empty() {
        points.push(bounds.center());
    }
    points
}

/// A mask's greys, as a point sees them: its base, then each of its marks
/// over it in turn.
struct Greys {
    base: f64,
    invert: bool,
    marks: Vec<(BezPath, Rect, Option<f64>, f64)>,
}

impl Greys {
    fn of(art: &Artwork, mask: &Layer) -> Self {
        let marks = art
            .marks_of(&mask.id)
            .filter_map(|mark| {
                let outline = crate::selection::mark_outline(mark).ok()?;
                let bounds = outline.bounding_box();
                // An eraser on a mask takes its greys back to the base.
                let grey = if mark.erases() {
                    None
                } else {
                    Some(grey(mark)?)
                };
                Some((outline, bounds, grey, mark.style.opacity.unwrap_or(1.0)))
            })
            .collect();
        Self {
            base: mask.base,
            invert: mask.invert,
            marks,
        }
    }

    /// How much of the layer shows at `point`, `0..=1`.
    fn at(&self, point: Point) -> f64 {
        let mut shown = self.base;
        for (outline, bounds, grey, opacity) in &self.marks {
            if bounds.contains(point) && outline.contains(point) {
                let grey = grey.unwrap_or(self.base);
                shown = shown * (1.0 - opacity) + grey * opacity;
            }
        }
        if self.invert { 1.0 - shown } else { shown }
    }
}

/// How light a mask mark is, `0..=1`: its fill, outline or gradient's
/// stops on average.
fn grey(mark: &Mark) -> Option<f64> {
    let light = |hex: &str| {
        tilo_wet_media::colour::parse_hex(hex).map(|(rgb, _)| {
            let [r, g, b] = rgb.to_srgb8();
            (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0
        })
    };
    let style = &mark.style;
    if let Some(gradient) = &style.gradient {
        let lights: Vec<f64> = gradient
            .stops
            .iter()
            .filter_map(|(_, colour)| light(colour))
            .collect();
        return (!lights.is_empty())
            .then(|| lights.iter().sum::<f64>() / f64_of_usize(lights.len()));
    }
    style
        .fill
        .as_deref()
        .or(style.stroke.as_deref())
        .and_then(light)
}
