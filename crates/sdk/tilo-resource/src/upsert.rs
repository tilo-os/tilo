//! Collection items addressed by what they hold, and places named by a
//! neighbour, so a caller never counts positions.
//!
//! A position read a moment ago is stale once anyone else inserts, removes
//! or sorts: an agent that writes "item 3" overwrites or shifts whatever is
//! third by then. [`EditOp::UpsertItem`](crate::EditOp::UpsertItem) names
//! the item by its fields instead and is resolved against the document as
//! it is when the edit applies, merged edits of others included; [`At`]
//! names a place by the item beside it.

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use crate::collection::Id;
use crate::model::{Doc, ModelError};

/// Where an inserted or moved item goes: a position (clamped to the end),
/// the end, or beside an item named by its id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum At {
    Index(usize),
    #[default]
    End,
    After(Id),
    Before(Id),
}

impl From<usize> for At {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

impl At {
    /// The position among `others`, the collection's items in order without
    /// the one being placed.
    ///
    /// # Errors
    /// The item it names to place beside is not among `others`.
    pub fn index(&self, others: &[&Id]) -> Result<usize, ModelError> {
        let beside = |id: &Id| {
            others
                .iter()
                .position(|other| *other == id)
                .ok_or_else(|| ModelError::invalid(format!("no item `{id}` to place it beside")))
        };
        Ok(match self {
            Self::Index(index) => (*index).min(others.len()),
            Self::End => others.len(),
            Self::After(id) => beside(id)?.saturating_add(1),
            Self::Before(id) => beside(id)?,
        })
    }
}

impl Serialize for At {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Index(index) => serializer.serialize_u64(*index as u64),
            Self::End => serializer.serialize_str("end"),
            Self::After(id) => serde_json::json!({ "after": id.as_str() }).serialize(serializer),
            Self::Before(id) => serde_json::json!({ "before": id.as_str() }).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for At {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        at_from_json(&serde_json::Value::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[inline(never)]
fn at_from_json(value: &serde_json::Value) -> Result<At, String> {
    const SHAPE: &str = "`at` is a position, \"end\", {\"after\": id} or {\"before\": id}";
    if let Some(index) = value.as_u64() {
        return usize::try_from(index)
            .map(At::Index)
            .map_err(|_| SHAPE.to_owned());
    }
    match value {
        serde_json::Value::String(text) if text.trim().eq_ignore_ascii_case("end") => Ok(At::End),
        serde_json::Value::Null => Ok(At::End),
        serde_json::Value::Object(fields) if fields.len() == 1 => {
            let id = |value: &serde_json::Value| {
                value
                    .as_str()
                    .map(|id| Id::from_raw(id.trim()))
                    .ok_or_else(|| SHAPE.to_owned())
            };
            match (fields.get("after"), fields.get("before")) {
                (Some(after), None) => id(after).map(At::After),
                (None, Some(before)) => id(before).map(At::Before),
                _ => Err(SHAPE.to_owned()),
            }
        }
        _ => Err(SHAPE.to_owned()),
    }
}

/// One item of a collection as an upsert reads it: its id, and the map its
/// fields are stored in.
pub(crate) struct Candidate {
    pub(crate) id: Id,
    pub(crate) fields: ObjId,
}

/// The one of `items` whose fields hold `matching`, `None` when none does.
///
/// # Errors
/// `matching` names no field, or more than one item matches: which one is
/// meant is unclear, and updating either could overwrite another's row.
pub(crate) fn matching_one<'a>(
    doc: &Doc,
    items: &'a [Candidate],
    matching: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<&'a Candidate>, ModelError> {
    if matching.is_empty() {
        return Err(ModelError::invalid(
            "upsert-item needs `match`: the fields that identify the item",
        ));
    }
    let mut found = items.iter().filter(|item| {
        matching
            .iter()
            .all(|(key, wanted)| same_value(stored(doc, &item.fields, key).as_ref(), wanted))
    });
    match (found.next(), found.next()) {
        (None, _) => Ok(None),
        (Some(one), None) => Ok(Some(one)),
        (Some(first), Some(second)) => Err(ModelError::invalid(format!(
            "`match` {} fits items `{}` and `{}`: name more fields so it fits one",
            serde_json::Value::Object(matching.clone()),
            first.id,
            second.id
        ))),
    }
}

/// The fields of `value` that differ from what the item stored at `fields`
/// holds: an upsert run again changes nothing.
///
/// # Errors
/// `value` is not an object of fields.
pub(crate) fn changed_fields(
    doc: &Doc,
    fields: &ObjId,
    value: &serde_json::Value,
) -> Result<Vec<(String, serde_json::Value)>, ModelError> {
    Ok(fields_of(value)?
        .iter()
        .filter(|(key, wanted)| stored(doc, fields, key).as_ref() != Some(*wanted))
        .map(|(key, wanted)| (key.clone(), wanted.clone()))
        .collect())
}

/// What a new item holds: `value` with the fields of `matching` it leaves
/// out, so the item is found by them the next time.
///
/// # Errors
/// `value` is not an object of fields.
pub(crate) fn inserted(
    matching: &serde_json::Map<String, serde_json::Value>,
    value: &serde_json::Value,
) -> Result<serde_json::Value, ModelError> {
    let mut fields = fields_of(value)?.clone();
    for (key, wanted) in matching {
        fields.entry(key.clone()).or_insert_with(|| wanted.clone());
    }
    Ok(serde_json::Value::Object(fields))
}

/// The id a new item found by `matching` is minted under: the same for the
/// same key whoever inserts it, so two inserts of one key made at once
/// merge into one item rather than two.
pub(crate) fn key_id(matching: &serde_json::Map<String, serde_json::Value>) -> Id {
    let mut hasher = blake3::Hasher::new();
    let mut keys: Vec<&String> = matching.keys().collect();
    keys.sort();
    for key in keys {
        hasher.update(fold(key).as_bytes());
        hasher.update(&[0]);
        let wanted = matching.get(key).map_or_else(String::new, key_text);
        hasher.update(wanted.as_bytes());
        hasher.update(&[0]);
    }
    let hash = hasher.finalize().to_hex();
    Id::from_raw(format!("key-{}", hash.get(..24).unwrap_or_default()))
}

fn fields_of(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, ModelError> {
    value.as_object().ok_or_else(|| {
        ModelError::invalid("upsert-item's `value` is an object of the item's fields")
    })
}

/// The scalar or text stored under `key` in `fields`, as JSON; `None` when
/// absent or a container.
fn stored(doc: &Doc, fields: &ObjId, key: &str) -> Option<serde_json::Value> {
    match doc.get(fields, key).ok()?? {
        (Value::Scalar(scalar), _) => scalar_json(&scalar),
        (Value::Object(ObjType::Text), text) => doc.text(&text).ok().map(serde_json::Value::String),
        (Value::Object(_), _) => None,
    }
}

fn scalar_json(scalar: &ScalarValue) -> Option<serde_json::Value> {
    Some(match scalar {
        ScalarValue::Str(text) => serde_json::Value::String(text.to_string()),
        ScalarValue::Int(number) => (*number).into(),
        ScalarValue::Uint(number) => (*number).into(),
        ScalarValue::F64(number) => serde_json::Number::from_f64(*number)?.into(),
        ScalarValue::Boolean(flag) => (*flag).into(),
        ScalarValue::Null => serde_json::Value::Null,
        _ => return None,
    })
}

/// Whether `stored` holds the key value `wanted`: numbers by value (`7`,
/// `7.0` and `"7"` alike), text alike but for case and spacing, absent as
/// null.
pub(crate) fn same_value(stored: Option<&serde_json::Value>, wanted: &serde_json::Value) -> bool {
    let stored = stored.unwrap_or(&serde_json::Value::Null);
    match (number(stored), number(wanted)) {
        (Some(a), Some(b)) => (a - b).abs() <= f64::EPSILON * a.abs().max(b.abs()).max(1.0),
        _ => match (stored, wanted) {
            (serde_json::Value::String(_), _) | (_, serde_json::Value::String(_)) => {
                fold(&key_text(stored)) == fold(&key_text(wanted))
            }
            _ => stored == wanted,
        },
    }
}

fn number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            text.trim().parse::<f64>().ok().filter(|n| n.is_finite())
        }
        _ => None,
    }
}

fn key_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => fold(text),
        serde_json::Value::Null => String::new(),
        other => number(other).map_or_else(|| other.to_string(), |number| number.to_string()),
    }
}

/// `text` lower-cased with its spacing collapsed.
fn fold(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn at_reads_a_position_the_end_or_a_neighbour_and_writes_back_the_same() {
        for (text, at) in [
            (json!(2), At::Index(2)),
            (json!("end"), At::End),
            (json!({"after": "b"}), At::After(Id::from("b"))),
            (json!({"before": "a"}), At::Before(Id::from("a"))),
        ] {
            let read: At = serde_json::from_value(text.clone()).unwrap();
            assert_eq!(read, at);
            assert_eq!(serde_json::to_value(&read).unwrap(), text);
        }
        assert!(serde_json::from_value::<At>(json!({"after": "a", "before": "b"})).is_err());
        assert!(serde_json::from_value::<At>(json!("middle")).is_err());
    }

    #[test]
    fn at_places_beside_the_item_it_names() {
        let (a, b, c) = (Id::from("a"), Id::from("b"), Id::from("c"));
        let others = [&a, &b, &c];
        assert_eq!(At::Index(9).index(&others).unwrap(), 3);
        assert_eq!(At::End.index(&others).unwrap(), 3);
        assert_eq!(At::After(b.clone()).index(&others).unwrap(), 2);
        assert_eq!(At::Before(a.clone()).index(&others).unwrap(), 0);
        assert!(At::After(Id::from("gone")).index(&others).is_err());
    }

    #[test]
    fn keys_match_folded_text_and_numbers_by_value() {
        assert!(same_value(Some(&json!("  Ana  Lima ")), &json!("ana lima")));
        assert!(same_value(Some(&json!(7)), &json!("7.0")));
        assert!(same_value(Some(&json!(7.0)), &json!(7)));
        assert!(!same_value(Some(&json!("Ana")), &json!("Anna")));
        assert!(!same_value(None, &json!("Ana")));
        assert!(same_value(None, &json!(null)));
    }

    #[test]
    fn one_key_mints_one_id_however_it_is_written() {
        let one = key_id(json!({"Name": "Ana Lima"}).as_object().unwrap());
        let same = key_id(json!({"name": " ana  lima"}).as_object().unwrap());
        let other = key_id(json!({"Name": "Ben"}).as_object().unwrap());
        assert_eq!(one, same);
        assert_ne!(one, other);
    }
}
