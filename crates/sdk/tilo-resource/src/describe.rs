//! What a record's actions did, as history records it: read from the
//! document before the action applies, through the record's stored shape,
//! without reading the record whole.

use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use tilo_actions::{ActionKey, Described, Did, FieldEdit, PathStep, Scalar};

use crate::model::Doc;
use crate::path::{Edit, EditOp, Segment};
use crate::shape::Shape;

/// Where a path leads in the document.
struct Located {
    /// What the path's slot holds, if anything.
    value: Option<(Value<'static>, ObjId)>,
    /// The record type owning the last field on the path, and its key.
    owner: Option<(&'static str, String)>,
    /// The last item on the path: its id and label.
    item: Option<(String, Option<String>)>,
    /// The shape of what the path leads to.
    shape: Shape,
}

fn get(doc: &Doc, obj: &ObjId, key: &str) -> Option<(Value<'static>, ObjId)> {
    doc.get(obj, key)
        .ok()
        .flatten()
        .map(|(value, id)| (value.to_owned(), id))
}

fn map_at(doc: &Doc, obj: &ObjId, key: &str) -> Option<ObjId> {
    match get(doc, obj, key)? {
        (Value::Object(ObjType::Map), id) => Some(id),
        _ => None,
    }
}

/// The label of the record of shape `shape` stored in the map `obj`: the
/// texts of its label fields joined by spaces, else its `#[model(label)]`
/// field's.
fn label_of(doc: &Doc, shape: Shape, obj: &ObjId) -> Option<String> {
    let mut parts: Vec<_> = shape
        .fields()
        .iter()
        .filter(|field| field.label_part > 0)
        .collect();
    parts.sort_by_key(|field| field.label_part);
    let joined: Vec<String> = parts
        .iter()
        .filter_map(|field| field_text(doc, obj, field))
        .collect();
    if !joined.is_empty() {
        return Some(joined.join(" "));
    }
    let field = shape.fields().iter().find(|field| field.label)?;
    field_text(doc, obj, field)
}

/// The first line of the text `field` holds in the map `obj`, read as its
/// type stores it (a rich text's first paragraph too).
fn field_text(doc: &Doc, obj: &ObjId, field: &crate::FieldShape) -> Option<String> {
    let (value, id) = get(doc, obj, field.key)?;
    (field.text)(doc, &value, &id)
}

fn locate(doc: &Doc, root: Shape, path: &[Segment]) -> Located {
    let mut located = Located {
        value: Some((Value::Object(ObjType::Map), ROOT)),
        owner: None,
        item: None,
        shape: root,
    };
    let mut shape = root;
    for segment in path {
        let Some((Value::Object(_), obj)) = located.value.take() else {
            break;
        };
        match (shape, segment) {
            (Shape::Record(fields), Segment::Field(key)) => {
                if let Some(field) = fields.iter().find(|field| field.key == key) {
                    located.owner = Some((field.owner, key.clone()));
                    shape = field.shape;
                } else {
                    shape = Shape::Value;
                }
                located.value = get(doc, &obj, key);
            }
            (Shape::Collection(item) | Shape::Keyed(item), Segment::Item(id)) => {
                let value = if matches!(shape, Shape::Keyed(_)) {
                    get(doc, &obj, id.as_str())
                } else {
                    map_at(doc, &obj, id.as_str()).and_then(|entry| get(doc, &entry, "v"))
                };
                let label = match &value {
                    Some((Value::Object(ObjType::Map), map)) => label_of(doc, *item, map),
                    _ => None,
                };
                located.item = Some((id.as_str().to_owned(), label));
                located.value = value;
                shape = *item;
            }
            (Shape::Map(value), Segment::Key(key)) => {
                let entry = get(doc, &obj, key);
                let label = match &entry {
                    Some((Value::Object(ObjType::Map), map)) => label_of(doc, *value, map),
                    _ => None,
                };
                located.item = Some((key.clone(), label));
                located.value = entry;
                shape = *value;
            }
            (_, Segment::Key(key)) => {
                located.value = get(doc, &obj, key);
                shape = Shape::Value;
            }
            (_, Segment::Cell { row, column }) => {
                located.value = map_at(doc, &obj, "cells")
                    .and_then(|cells| get(doc, &cells, &format!("{row}/{column}")));
                shape = Shape::Value;
            }
            _ => {
                located.value = None;
                shape = Shape::Value;
            }
        }
    }
    located.shape = shape;
    located
}

/// The label an item written as `value` (its JSON form) carries, for a
/// collection item of shape `item`.
fn label_in(item: Shape, value: &serde_json::Value) -> Option<String> {
    let text = |key: &str| {
        value
            .get(key)?
            .as_str()?
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
    };
    let mut parts: Vec<_> = item
        .fields()
        .iter()
        .filter(|field| field.label_part > 0)
        .collect();
    parts.sort_by_key(|field| field.label_part);
    let joined: Vec<String> = parts.iter().filter_map(|field| text(field.key)).collect();
    if !joined.is_empty() {
        return Some(joined.join(" "));
    }
    text(item.fields().iter().find(|field| field.label)?.key)
}

fn scalar(value: &Value<'_>) -> Option<Scalar> {
    let Value::Scalar(scalar) = value else {
        return None;
    };
    let json = match scalar.as_ref() {
        ScalarValue::Str(text) => serde_json::Value::String(text.to_string()),
        ScalarValue::Int(value) => (*value).into(),
        ScalarValue::Uint(value) => (*value).into(),
        ScalarValue::F64(value) => serde_json::Number::from_f64(*value)?.into(),
        ScalarValue::Counter(counter) => i64::from(counter).into(),
        ScalarValue::Boolean(value) => (*value).into(),
        _ => return None,
    };
    Scalar::from_json(&json)
}

fn step(segment: &Segment) -> PathStep {
    match segment {
        Segment::Field(key) => PathStep::Field(key.clone()),
        Segment::Item(id) => PathStep::Item(id.as_str().to_owned()),
        Segment::Key(key) => PathStep::Key(key.clone()),
        Segment::Cell { row, column } => {
            PathStep::Cell(row.as_str().to_owned(), column.as_str().to_owned())
        }
    }
}

const fn operation(op: &EditOp) -> tilo_actions::EditOp {
    use tilo_actions::EditOp as Recorded;
    match op {
        EditOp::Set { .. } => Recorded::Set,
        EditOp::Settle { .. } => Recorded::Settle,
        EditOp::Forget { .. } => Recorded::Forget,
        EditOp::Splice { .. } => Recorded::Splice,
        EditOp::Replace { .. } => Recorded::Replace,
        EditOp::Add { .. } => Recorded::Add,
        EditOp::Insert { .. } | EditOp::InsertItem { .. } => Recorded::Insert,
        EditOp::Remove { .. } | EditOp::RemoveItem { .. } | EditOp::RemoveKey { .. } => {
            Recorded::Remove
        }
        EditOp::MoveTo { .. } => Recorded::MoveTo,
        EditOp::SetCell { .. } => Recorded::SetCell,
        EditOp::InsertRow { .. } => Recorded::InsertRow,
        EditOp::InsertColumn { .. } => Recorded::InsertColumn,
        EditOp::RemoveRow { .. } => Recorded::RemoveRow,
        EditOp::RemoveColumn { .. } => Recorded::RemoveColumn,
        EditOp::Rich { .. } => Recorded::Mark,
    }
}

/// A field edit of a record of shape `shape`, as it reads before it applies.
pub(crate) fn edit<R: crate::Model>(doc: &Doc, shape: Shape, edit: &Edit<R>) -> Option<Described> {
    let located = locate(doc, shape, edit.path());
    let (owner, field) = located.owner?;
    let old = located.value.as_ref().and_then(|(value, _)| scalar(value));
    let new = match edit.op() {
        // A set's value, or the element a set gained or lost.
        EditOp::Set { value } | EditOp::Insert { value } | EditOp::Remove { value } => {
            Scalar::from_json(value)
        }
        EditOp::Add { by } => old
            .as_ref()
            .and_then(|old| serde_json::to_value(old).ok())
            .and_then(|old| old.as_i64())
            .map(|old| Scalar::from_json(&old.saturating_add(*by).into()))
            .unwrap_or_default(),
        _ => None,
    };
    // An item added or removed is the item the entry is about, unless the
    // collection sits inside an item (a playlist's songs).
    let item = match (edit.op(), located.shape) {
        _ if located.item.is_some() => located.item.clone(),
        (EditOp::InsertItem { id, value, .. }, Shape::Collection(item) | Shape::Keyed(item)) => {
            let label = label_in(*item, value);
            let id = id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .or_else(|| label.clone());
            id.map(|id| (id, label))
        }
        (EditOp::RemoveItem { id }, Shape::Collection(item) | Shape::Keyed(item)) => {
            let stored = match &located.value {
                Some((Value::Object(ObjType::Map), collection)) => {
                    if matches!(located.shape, Shape::Keyed(_)) {
                        map_at(doc, collection, id.as_str())
                    } else {
                        map_at(doc, collection, id.as_str())
                            .and_then(|entry| map_at(doc, &entry, "v"))
                    }
                }
                _ => None,
            };
            let label = stored.and_then(|map| label_of(doc, *item, &map));
            Some((id.as_str().to_owned(), label))
        }
        // An entry removed from a map is the entry the entry is about.
        (EditOp::RemoveKey { key }, Shape::Map(value)) => {
            let label = match &located.value {
                Some((Value::Object(ObjType::Map), map)) => {
                    map_at(doc, map, key).and_then(|entry| label_of(doc, *value, &entry))
                }
                _ => None,
            };
            Some((key.clone(), label))
        }
        _ => located.item.clone(),
    };
    let rich = match edit.op() {
        EditOp::Rich { rich } => R::op_describe(edit.path(), rich),
        _ => None,
    };
    if let EditOp::Settle { id, .. } | EditOp::Forget { id } = edit.op() {
        return Some(outbox_edit(owner, field, edit, id));
    }
    let described = Described::new(Did::Edit(FieldEdit {
        ty: owner.to_owned(),
        field,
        path: edit.path().iter().map(step).collect(),
        op: match edit.op() {
            EditOp::Rich { .. } => rich
                .as_ref()
                .map_or(tilo_actions::EditOp::Mark, |described| described.op),
            op => operation(op),
        },
        args: rich.map(|described| described.args).unwrap_or_default(),
        old: matches!(edit.op(), EditOp::Set { .. } | EditOp::Add { .. })
            .then_some(old)
            .flatten(),
        new,
    }));
    Some(match item {
        Some((id, label)) => {
            let label = label.unwrap_or_else(|| id.clone());
            described.on(id, label)
        }
        None => described,
    })
}

/// An outbox entry settled or forgotten: the entry's id ends the path, and a
/// settlement names its state (`done`, `failed`) and reason.
fn outbox_edit<R>(owner: &str, field: String, edit: &Edit<R>, id: &crate::Id) -> Described {
    let mut path: Vec<PathStep> = edit.path().iter().map(step).collect();
    path.push(PathStep::Item(id.as_str().to_owned()));
    let mut args = std::collections::BTreeMap::new();
    if let EditOp::Settle { outcome, .. } = edit.op() {
        if let Some(state) = outcome.get("state").and_then(Scalar::from_json) {
            args.insert("state".to_owned(), state);
        }
        if let Some(reason) = outcome
            .get("result")
            .and_then(|result| result.get("reason"))
            .and_then(Scalar::from_json)
        {
            args.insert("reason".to_owned(), reason);
        }
    }
    Described::new(Did::Edit(FieldEdit {
        ty: owner.to_owned(),
        field,
        path,
        op: operation(edit.op()),
        args,
        old: None,
        new: None,
    }))
}

/// A verb of a record of shape `shape` and kind `kind`, from its JSON form
/// (`{"action": id, "args": {..}}`): its plain arguments by name and the
/// item its first id argument names in one of the record's collections.
pub(crate) fn verb(
    doc: &Doc,
    shape: Shape,
    kind: &str,
    value: &serde_json::Value,
) -> Option<Described> {
    let id = value.get("action")?.as_str()?;
    let args = value
        .get("args")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    // Plain arguments by name; a record argument by its fields, one level
    // deep (`receipt.completed`).
    let mut plain: Vec<(String, Scalar)> = Vec::new();
    for (name, value) in &args {
        match value {
            serde_json::Value::Object(fields) => {
                for (field, value) in fields {
                    if let Some(scalar) = Scalar::from_json(value) {
                        plain.push((format!("{name}.{field}"), scalar));
                    }
                }
            }
            value => {
                if let Some(scalar) = Scalar::from_json(value) {
                    plain.push((name.clone(), scalar));
                }
            }
        }
    }
    let described = Described::new(Did::verb(ActionKey::new("", kind, id), plain));
    // The item: the innermost item the id arguments name, through the
    // record's collections and those inside their items (a slide's
    // element).
    let ids: Vec<&str> = args
        .values()
        .filter_map(serde_json::Value::as_str)
        .collect();
    Some(match item_named(doc, shape, &ROOT, &ids) {
        Some((id, label)) => {
            let label = label.unwrap_or_else(|| id.clone());
            described.on(id, label)
        }
        None => described,
    })
}

/// The innermost item `ids` name among the collections of the record of
/// shape `shape` stored in `obj`, and inside that item's own collections:
/// its id and label.
fn item_named(
    doc: &Doc,
    shape: Shape,
    obj: &ObjId,
    ids: &[&str],
) -> Option<(String, Option<String>)> {
    for field in shape.fields() {
        let (Shape::Collection(item) | Shape::Keyed(item)) = field.shape else {
            continue;
        };
        let keyed = matches!(field.shape, Shape::Keyed(_));
        let Some(collection) = map_at(doc, obj, field.key) else {
            continue;
        };
        for candidate in ids {
            let Some(entry) = map_at(doc, &collection, candidate) else {
                continue;
            };
            let value = if keyed {
                Some(entry)
            } else {
                map_at(doc, &entry, "v")
            };
            let inner = value
                .as_ref()
                .and_then(|map| item_named(doc, *item, map, ids));
            return inner.or_else(|| {
                let label = value.and_then(|map| label_of(doc, *item, &map));
                Some(((*candidate).to_owned(), label))
            });
        }
    }
    None
}
