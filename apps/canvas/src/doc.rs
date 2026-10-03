//! The artwork's document layout.
//!
//! ```text
//! title, width, height      scalars
//! ground                    JSON of Ground
//! layers.<id>               { name, order, medium, opacity, visible, texture, locked }
//! marks.<id>                { layer, order, body: JSON of MarkBody }
//! media.<id>                JSON of MediumSpec
//! reference                 JSON of Reference, when there is one
//! ```
//!
//! Keyed maps, so concurrent edits of different layers and marks merge, and
//! each edit writes only the keys it changes: moving a mark to another layer
//! rewrites its `layer`, recolouring it rewrites its `body`.

use std::borrow::Cow;

use ui::automerge::transaction::Transactable;
use ui::automerge::{AutomergeError, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::tilo_resource::Doc;

use crate::media::MediumSpec;
use crate::model::{Artwork, Ground, Layer, Mark, MarkBody, Reference};

pub const LAYERS: &str = "layers";
pub const MARKS: &str = "marks";
pub const MEDIA: &str = "media";
pub const REFERENCE: &str = "reference";

pub fn child(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key).ok().flatten()? {
        (Value::Object(ObjType::Map), id) => Some(id),
        _ => None,
    }
}

/// The map at `key` under `parent`, created when absent.
///
/// # Errors
/// Returns the document's write error.
pub fn ensure(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, AutomergeError> {
    match child(doc, parent, key) {
        Some(id) => Ok(id),
        None => doc.put_object(parent, key, ObjType::Map),
    }
}

fn scalar<'a>(doc: &'a Doc, obj: &ObjId, key: &str) -> Option<Cow<'a, ScalarValue>> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(value), _) => Some(value),
        _ => None,
    }
}

pub fn text(doc: &Doc, obj: &ObjId, key: &str) -> Option<String> {
    match scalar(doc, obj, key)?.as_ref() {
        ScalarValue::Str(text) => Some(text.to_string()),
        _ => None,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "stored integers are small dimensions and opacities"
)]
pub fn number(doc: &Doc, obj: &ObjId, key: &str) -> Option<f64> {
    match scalar(doc, obj, key)?.as_ref() {
        ScalarValue::F64(value) => Some(*value),
        ScalarValue::Int(value) => Some(*value as f64),
        ScalarValue::Uint(value) => Some(*value as f64),
        _ => None,
    }
}

fn flag(doc: &Doc, obj: &ObjId, key: &str) -> Option<bool> {
    match scalar(doc, obj, key)?.as_ref() {
        ScalarValue::Boolean(value) => Some(*value),
        _ => None,
    }
}

fn json<T: serde::de::DeserializeOwned>(doc: &Doc, obj: &ObjId, key: &str) -> Option<T> {
    serde_json::from_str(&text(doc, obj, key)?).ok()
}

/// The ids of a keyed map's entries.
pub fn ids(doc: &Doc, map: Option<&ObjId>) -> Vec<String> {
    map.map(|map| doc.keys(map).collect()).unwrap_or_default()
}

/// The whole artwork.
pub fn read(doc: &Doc) -> Artwork {
    let defaults = Artwork::default();
    let layers_map = child(doc, &ROOT, LAYERS);
    let mut layers: Vec<Layer> = ids(doc, layers_map.as_ref())
        .into_iter()
        .filter_map(|id| {
            let obj = child(doc, layers_map.as_ref()?, &id)?;
            Some(Layer {
                name: text(doc, &obj, "name").unwrap_or_else(|| id.clone()),
                order: text(doc, &obj, "order").unwrap_or_default(),
                medium: text(doc, &obj, "medium").unwrap_or_default(),
                opacity: number(doc, &obj, "opacity").unwrap_or(1.0),
                visible: flag(doc, &obj, "visible").unwrap_or(true),
                texture: number(doc, &obj, "texture").unwrap_or(0.0),
                locked: flag(doc, &obj, "locked").unwrap_or(false),
                id,
            })
        })
        .collect();
    layers.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    let marks_map = child(doc, &ROOT, MARKS);
    let mut marks: Vec<Mark> = ids(doc, marks_map.as_ref())
        .into_iter()
        .filter_map(|id| {
            let obj = child(doc, marks_map.as_ref()?, &id)?;
            let body: MarkBody = json(doc, &obj, "body")?;
            Some(Mark {
                layer: text(doc, &obj, "layer").unwrap_or_default(),
                order: text(doc, &obj, "order").unwrap_or_default(),
                name: body.name,
                shape: body.shape,
                style: body.style,
                transform: body.transform,
                group: body.group,
                clip: body.clip,
                id,
            })
        })
        .filter(|mark| layers.iter().any(|layer| layer.id == mark.layer))
        .collect();
    let rank = |layer: &str| layers.iter().position(|candidate| candidate.id == layer);
    marks.sort_by(|a, b| {
        rank(&a.layer)
            .cmp(&rank(&b.layer))
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.id.cmp(&b.id))
    });

    let media_map = child(doc, &ROOT, MEDIA);
    let media = ids(doc, media_map.as_ref())
        .into_iter()
        .filter_map(|id| {
            let spec: MediumSpec = json(doc, media_map.as_ref()?, &id)?;
            Some((id, spec))
        })
        .collect();

    Artwork {
        title: text(doc, &ROOT, "title").unwrap_or_default(),
        width: number(doc, &ROOT, "width").unwrap_or(defaults.width),
        height: number(doc, &ROOT, "height").unwrap_or(defaults.height),
        ground: json::<Ground>(doc, &ROOT, "ground").unwrap_or_default(),
        layers,
        marks,
        media,
        reference: json(doc, &ROOT, REFERENCE),
    }
}

/// Set or clear the reference picture.
///
/// # Errors
/// Returns the document's write error.
pub fn write_reference(doc: &mut Doc, reference: Option<&Reference>) -> Result<(), AutomergeError> {
    if let Some(reference) = reference {
        let encoded = serde_json::to_string(reference).unwrap_or_default();
        return put_text(doc, &ROOT, REFERENCE, &encoded);
    }
    if doc.get(&ROOT, REFERENCE)?.is_some() {
        doc.delete(&ROOT, REFERENCE)?;
    }
    Ok(())
}

/// Write `layer`'s fields, only those that differ.
///
/// # Errors
/// Returns the document's write error.
pub fn write_layer(doc: &mut Doc, layer: &Layer) -> Result<(), AutomergeError> {
    let layers = ensure(doc, &ROOT, LAYERS)?;
    let obj = ensure(doc, &layers, &layer.id)?;
    put_text(doc, &obj, "name", &layer.name)?;
    put_text(doc, &obj, "order", &layer.order)?;
    put_text(doc, &obj, "medium", &layer.medium)?;
    if number(doc, &obj, "opacity") != Some(layer.opacity) {
        doc.put(&obj, "opacity", layer.opacity)?;
    }
    if flag(doc, &obj, "visible") != Some(layer.visible) {
        doc.put(&obj, "visible", layer.visible)?;
    }
    let texture = number(doc, &obj, "texture");
    if texture != Some(layer.texture) && (texture.is_some() || layer.texture > 0.0) {
        doc.put(&obj, "texture", layer.texture)?;
    }
    if flag(doc, &obj, "locked").unwrap_or(false) != layer.locked {
        doc.put(&obj, "locked", layer.locked)?;
    }
    Ok(())
}

/// Write `mark`, only the keys that differ.
///
/// # Errors
/// Returns the document's write error.
pub fn write_mark(doc: &mut Doc, mark: &Mark) -> Result<(), AutomergeError> {
    let marks = ensure(doc, &ROOT, MARKS)?;
    let obj = ensure(doc, &marks, &mark.id)?;
    put_text(doc, &obj, "layer", &mark.layer)?;
    put_text(doc, &obj, "order", &mark.order)?;
    let body = MarkBody {
        name: mark.name.clone(),
        shape: mark.shape.clone(),
        style: mark.style.clone(),
        transform: mark.transform,
        group: mark.group.clone(),
        clip: mark.clip.clone(),
    };
    let encoded = serde_json::to_string(&body).unwrap_or_default();
    put_text(doc, &obj, "body", &encoded)
}

/// Write `value` at `key` unless it is already there.
///
/// # Errors
/// Returns the document's write error.
pub fn put_text(doc: &mut Doc, obj: &ObjId, key: &str, value: &str) -> Result<(), AutomergeError> {
    if text(doc, obj, key).as_deref() != Some(value) {
        doc.put(obj, key, value)?;
    }
    Ok(())
}

/// Remove the entry `id` of the keyed map `map`.
///
/// # Errors
/// Returns the document's write error.
pub fn remove(doc: &mut Doc, map: &str, id: &str) -> Result<(), AutomergeError> {
    if let Some(map) = child(doc, &ROOT, map)
        && doc.get(&map, id)?.is_some()
    {
        doc.delete(&map, id)?;
    }
    Ok(())
}

/// Save `medium` in the artwork.
///
/// # Errors
/// Returns the document's write error.
pub fn write_medium(doc: &mut Doc, medium: &MediumSpec) -> Result<(), AutomergeError> {
    let media = ensure(doc, &ROOT, MEDIA)?;
    let encoded = serde_json::to_string(medium).unwrap_or_default();
    put_text(doc, &media, &medium.id, &encoded)
}
