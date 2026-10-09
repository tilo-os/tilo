//! What an action may write, and which storage units a change wrote.
//!
//! A record is divided into storage units: the record itself, each
//! `#[model(unit)]` field, and each item of a `#[model(unit)]` collection
//! ([`Shape::unit_of`]). Who may write a unit is decided per unit, so every
//! action declares the units it writes — its footprint:
//!
//! * a field edit writes the unit of its path;
//! * an item verb (`#[action(item = …)]`, or one whose first parameter
//!   names an item) writes its item's unit, and the record unit;
//! * any other verb writes the record unit;
//! * a verb that writes another unit declares it: `#[action(writes = field)]`.
//!
//! The origin checks what a verb produced against its footprint and refuses
//! it otherwise, so a mistake surfaces where it is made; the surface lists
//! each verb's footprint ([`VerbSurface::writes`](crate::surface::VerbSurface)).
//! A record with units keeps an index of them in its document
//! ([`UNIT_INDEX`]), so a relay maps a change's operations to the units they
//! touched ([`units_of`]) without the app's code.

use std::collections::BTreeSet;

use automerge::transaction::Transactable;
use automerge::{ObjType, Prop, ROOT, ReadDoc, Value};

use crate::action::Refusal;
use crate::collection::{Id, VALUE};
use crate::model::{Doc, Model};
use crate::path::{Path, Segment};
use crate::shape::Shape;

/// The record unit, as footprints and [`units_of`] name it.
pub const RECORD_UNIT: &str = "record";

/// The document key of a record's unit index: the storage patterns of its
/// units (`comments/*`, `body`), one string each.
pub const UNIT_INDEX: &str = "_units";

/// The units one verb invocation may write beside the record unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Writes {
    /// The item an item verb acts on: its unit.
    pub item: Option<Id>,
    /// Fields whose units the verb declares (`#[action(writes = field)]`).
    pub fields: &'static [&'static str],
}

impl Writes {
    /// The record unit only.
    #[must_use]
    pub const fn record() -> Self {
        Self {
            item: None,
            fields: &[],
        }
    }

    fn allows(&self, unit: &[Segment]) -> bool {
        match unit {
            [] => true,
            [Segment::Field(key), rest @ ..] if self.fields.contains(&key.as_str()) => {
                let _ = rest;
                true
            }
            [Segment::Field(_), Segment::Item(id)] => self.item.as_ref() == Some(id),
            _ => false,
        }
    }
}

/// Whether `shape` has units beside the record.
#[must_use]
pub fn has_units(shape: Shape) -> bool {
    match shape {
        Shape::Record(fields) => fields
            .iter()
            .any(|field| field.unit || has_units(field.shape)),
        Shape::Collection(item) | Shape::Keyed(item) => has_units(*item),
        Shape::Value => false,
    }
}

fn unit_name(unit: &[Segment]) -> String {
    if unit.is_empty() {
        return RECORD_UNIT.to_owned();
    }
    unit.iter()
        .map(|segment| match segment {
            Segment::Field(key) | Segment::Key(key) => key.clone(),
            Segment::Item(id) => id.as_str().to_owned(),
            Segment::Cell { row, column } => format!("{row}:{column}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Refuse a verb that changed `before` into `after` outside `writes`.
///
/// # Errors
/// Names the first unit written outside the footprint.
pub fn check<R: Model>(before: &R, after: &R, writes: &Writes) -> Result<(), Refusal> {
    use crate::path::EditOp;
    for edit in Path::<R, R>::root().edits(before, after) {
        // Adding, removing or moving an item writes the item itself.
        let mut path = edit.path().to_vec();
        match edit.op() {
            EditOp::InsertItem { id: Some(id), .. }
            | EditOp::RemoveItem { id }
            | EditOp::MoveTo { id, .. } => path.push(Segment::Item(id.clone())),
            _ => {}
        }
        let unit = R::SHAPE.unit_of(&path);
        if !writes.allows(&unit) {
            return Err(Refusal::new(format!(
                "the action wrote `{}`, outside the units it declares; add \
                 `#[action(writes = field)]`",
                unit_name(&unit)
            )));
        }
    }
    Ok(())
}

/// The footprint a verb declares, as the surface lists it: `record`, its
/// item's unit (`comments.*`) and the units of the fields it declares.
#[must_use]
pub fn declared(shape: Shape, item: Option<&str>, fields: &[&str]) -> Vec<String> {
    let mut units = vec![RECORD_UNIT.to_owned()];
    let field_of = |key: &str| shape.fields().iter().find(|field| field.key == key);
    if let Some(collection) = item.and_then(field_of)
        && collection.unit
        && matches!(collection.shape, Shape::Collection(_) | Shape::Keyed(_))
    {
        units.push(format!("{}.*", collection.key));
    }
    for key in fields {
        let Some(field) = field_of(key) else { continue };
        let unit = if matches!(field.shape, Shape::Collection(_) | Shape::Keyed(_)) {
            format!("{}.*", field.key)
        } else {
            field.key.to_owned()
        };
        if !units.contains(&unit) {
            units.push(unit);
        }
    }
    units
}

/// The storage patterns of `shape`'s units: keys from the record's root,
/// `*` for any item.
#[must_use]
pub fn patterns(shape: Shape) -> Vec<String> {
    fn walk(shape: Shape, prefix: &str, out: &mut Vec<String>) {
        match shape {
            Shape::Record(fields) => {
                for field in fields {
                    let path = format!("{prefix}{}", field.key);
                    match field.shape {
                        Shape::Collection(item) | Shape::Keyed(item) if field.unit => {
                            out.push(format!("{path}/*"));
                            let value = if matches!(field.shape, Shape::Collection(_)) {
                                format!("{path}/*/{VALUE}/")
                            } else {
                                format!("{path}/*/")
                            };
                            walk(*item, &value, out);
                        }
                        _ if field.unit => {
                            out.push(path.clone());
                            walk(field.shape, &format!("{path}/"), out);
                        }
                        _ => walk(field.shape, &format!("{path}/"), out),
                    }
                }
            }
            Shape::Collection(item) => walk(*item, &format!("{prefix}*/{VALUE}/"), out),
            Shape::Keyed(item) => walk(*item, &format!("{prefix}*/"), out),
            Shape::Value => {}
        }
    }
    let mut out = Vec::new();
    walk(shape, "", &mut out);
    out
}

/// Keep the unit index of a record of shape `shape` in `doc`, written once.
///
/// # Errors
/// Returns the document's write error.
pub fn index(doc: &mut Doc, shape: Shape) -> Result<(), automerge::AutomergeError> {
    if !has_units(shape) || doc.get(&ROOT, UNIT_INDEX)?.is_some() {
        return Ok(());
    }
    let list = doc.put_object(&ROOT, UNIT_INDEX, ObjType::List)?;
    for (at, pattern) in patterns(shape).into_iter().enumerate() {
        doc.insert(&list, at, pattern)?;
    }
    Ok(())
}

fn indexed(doc: &Doc) -> Vec<Vec<String>> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, UNIT_INDEX) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|at| match doc.get(&list, at).ok()?? {
            (Value::Scalar(scalar), _) => scalar.as_str().map(str::to_owned),
            _ => None,
        })
        .map(|pattern| pattern.split('/').map(str::to_owned).collect())
        .collect()
}

/// The units the changes `patches` (as `doc` reports them) touched, by the
/// record's unit index: `record` for anything outside every unit, else the
/// unit's storage path (`comments/<id>`). Needs no app code.
#[must_use]
pub fn units_of(doc: &Doc, patches: &[automerge::Patch]) -> BTreeSet<String> {
    use automerge::PatchAction;
    let patterns = indexed(doc);
    let mut units = BTreeSet::new();
    for patch in patches {
        let mut keys: Vec<String> = patch
            .path
            .iter()
            .map(|(_, prop)| match prop {
                Prop::Map(key) => key.clone(),
                Prop::Seq(index) => index.to_string(),
            })
            .collect();
        match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                keys.push(key.clone());
            }
            PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                keys.push(match prop {
                    Prop::Map(key) => key.clone(),
                    Prop::Seq(index) => index.to_string(),
                });
            }
            _ => {}
        }
        let unit = patterns
            .iter()
            .filter(|pattern| {
                pattern.len() <= keys.len()
                    && pattern
                        .iter()
                        .zip(&keys)
                        .all(|(want, key)| want == "*" || want == key)
            })
            .max_by_key(|pattern| pattern.len())
            .map_or_else(
                || RECORD_UNIT.to_owned(),
                |pattern| keys.get(..pattern.len()).unwrap_or_default().join("/"),
            );
        units.insert(unit);
    }
    units
}
