//! An item of a record as JSON, read straight from the Automerge document
//! at given heads.
//!
//! The host process never runs app code beside its keys, so it cannot read
//! an item through the app's model. Records keep their items in maps keyed
//! by item id, so the item is the first map entry with that key, searched
//! breadth first, and its subtree is converted generically: maps to
//! objects, lists to arrays, text to strings, scalars to their JSON values.
//! What a bot reads is therefore the stored shape, not the app's typed one.

use automerge::{ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, Value as Am};
use serde_json::{Map, Number, Value};

/// How deep the search for an item goes.
const MAX_DEPTH: usize = 6;
/// How many values a converted item may hold in all, so one huge item
/// cannot balloon an event.
const MAX_ENTRIES: usize = 10_000;

/// The item `item` as it was at `heads`, or `None` when no map held it.
#[must_use]
pub fn item_json(doc: &impl ReadDoc, heads: &[ChangeHash], item: &str) -> Option<Value> {
    let object = find(doc, heads, item)?;
    let object = collection_value(doc, heads, &object).unwrap_or(object);
    let mut budget = MAX_ENTRIES;
    Some(convert(doc, heads, &object, &mut budget))
}

/// The value of a collection's entry: an ordered collection keeps each
/// item as `{k: order key, v: value}`, and a reader wants the value.
fn collection_value(doc: &impl ReadDoc, heads: &[ChangeHash], entry: &ObjId) -> Option<ObjId> {
    let keys: Vec<String> = doc.keys_at(entry, heads).collect();
    if keys != ["k", "v"] {
        return None;
    }
    match doc.get_at(entry, "v", heads) {
        Ok(Some((Am::Object(ObjType::Map), value))) => Some(value),
        _ => None,
    }
}

/// One entry of an outbox as stored: its id, the queued value, and the
/// settled outcome (`{"state": .., "result": ..}`) once there is one.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxRow {
    /// The entry's id.
    pub id: String,
    /// What was queued: for a connector, `{"op": .., "publish": [..]}`.
    pub value: Value,
    /// How it settled, when it did.
    pub settled: Option<Value>,
}

/// The entries of the outbox stored under the field `field`, in queue
/// order, as they are at `heads`.
#[must_use]
pub fn outbox_entries(doc: &impl ReadDoc, heads: &[ChangeHash], field: &str) -> Vec<OutboxRow> {
    let Some(Value::Object(entries)) = item_json(doc, heads, field) else {
        return Vec::new();
    };
    // A collection keeps each entry as `{k: order key, v: entry}`.
    let mut rows: Vec<(String, OutboxRow)> = entries
        .into_iter()
        .filter_map(|(id, stored)| {
            let order = stored.get("k")?.as_str()?.to_owned();
            let entry = stored.get("v")?;
            let settled = match entry.get("settled") {
                Some(Value::String(text)) => serde_json::from_str(text).ok(),
                Some(Value::Object(settled)) => Some(Value::Object(settled.clone())),
                _ => None,
            };
            Some((
                order,
                OutboxRow {
                    id,
                    value: queued(entry.get("value")),
                    settled,
                },
            ))
        })
        .collect();
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// A queued value as stored. A JSON model keeps its value as JSON text
/// (`{"op": ..}` as a string), so text that holds an object is that object;
/// any other value, a plain string included, is as stored.
fn queued(stored: Option<&Value>) -> Value {
    match stored {
        Some(Value::String(text)) => match serde_json::from_str(text) {
            Ok(object @ Value::Object(_)) => object,
            _ => Value::String(text.clone()),
        },
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

fn find(doc: &impl ReadDoc, heads: &[ChangeHash], item: &str) -> Option<ObjId> {
    let mut level = vec![automerge::ROOT];
    for _ in 0..=MAX_DEPTH {
        let mut next = Vec::new();
        for map in &level {
            if let Ok(Some((Am::Object(_), id))) = doc.get_at(map, item, heads) {
                return Some(id);
            }
            for key in doc.keys_at(map, heads) {
                if let Ok(Some((Am::Object(ObjType::Map | ObjType::Table), id))) =
                    doc.get_at(map, key, heads)
                {
                    next.push(id);
                }
            }
        }
        if next.is_empty() {
            return None;
        }
        level = next;
    }
    None
}

fn convert(doc: &impl ReadDoc, heads: &[ChangeHash], object: &ObjId, budget: &mut usize) -> Value {
    match doc.object_type(object) {
        Ok(ObjType::Map | ObjType::Table) => {
            let mut out = Map::new();
            for key in doc.keys_at(object, heads) {
                let Some(left) = budget.checked_sub(1) else {
                    break;
                };
                *budget = left;
                let value = entry(doc, heads, doc.get_at(object, key.as_str(), heads), budget);
                out.insert(key, value);
            }
            Value::Object(out)
        }
        Ok(ObjType::List) => {
            let mut out = Vec::new();
            for index in 0..doc.length_at(object, heads) {
                let Some(left) = budget.checked_sub(1) else {
                    break;
                };
                *budget = left;
                out.push(entry(doc, heads, doc.get_at(object, index, heads), budget));
            }
            Value::Array(out)
        }
        Ok(ObjType::Text) => Value::String(doc.text_at(object, heads).unwrap_or_default()),
        Err(_) => Value::Null,
    }
}

fn entry(
    doc: &impl ReadDoc,
    heads: &[ChangeHash],
    found: Result<Option<(Am<'_>, ObjId)>, automerge::AutomergeError>,
    budget: &mut usize,
) -> Value {
    match found {
        Ok(Some((Am::Object(_), id))) => convert(doc, heads, &id, budget),
        Ok(Some((Am::Scalar(scalar), _))) => scalar_json(&scalar),
        _ => Value::Null,
    }
}

fn scalar_json(scalar: &ScalarValue) -> Value {
    match scalar {
        ScalarValue::Str(text) => Value::String(text.to_string()),
        ScalarValue::Int(n) | ScalarValue::Timestamp(n) => Value::from(*n),
        ScalarValue::Uint(n) => Value::from(*n),
        ScalarValue::F64(n) => Number::from_f64(*n).map_or(Value::Null, Value::Number),
        ScalarValue::Boolean(b) => Value::Bool(*b),
        ScalarValue::Counter(counter) => Value::from(i64::from(counter)),
        ScalarValue::Null | ScalarValue::Bytes(_) | ScalarValue::Unknown { .. } => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

    #[test]
    fn an_item_reads_as_json_before_and_after_a_change() {
        let mut doc = AutoCommit::new();
        let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
        let m1 = doc.put_object(&messages, "m1", ObjType::Map).unwrap();
        doc.put(&m1, "text", "hi").unwrap();
        doc.put(&m1, "pinned", false).unwrap();
        doc.commit();
        let before = doc.get_heads();
        doc.put(&m1, "text", "hi there").unwrap();
        doc.commit();
        let after = doc.get_heads();
        assert_eq!(
            item_json(&doc, &before, "m1").unwrap(),
            serde_json::json!({"text": "hi", "pinned": false})
        );
        assert_eq!(item_json(&doc, &after, "m1").unwrap()["text"], "hi there");
        assert!(item_json(&doc, &after, "missing").is_none());
    }

    #[test]
    fn a_collection_item_reads_as_its_value() {
        let mut doc = AutoCommit::new();
        let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
        let m1 = doc.put_object(&messages, "m1", ObjType::Map).unwrap();
        doc.put(&m1, "k", "a0").unwrap();
        let value = doc.put_object(&m1, "v", ObjType::Map).unwrap();
        doc.put(&value, "channel", "c1").unwrap();
        doc.commit();
        let heads = doc.get_heads();
        assert_eq!(
            item_json(&doc, &heads, "m1").unwrap(),
            serde_json::json!({"channel": "c1"})
        );
    }

    #[test]
    fn text_objects_and_lists_read_as_strings_and_arrays() {
        let mut doc = AutoCommit::new();
        let items = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
        let item = doc.put_object(&items, "i1", ObjType::Map).unwrap();
        let body = doc.put_object(&item, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "rich").unwrap();
        let tags = doc.put_object(&item, "tags", ObjType::List).unwrap();
        doc.insert(&tags, 0, "a").unwrap();
        doc.commit();
        let heads = doc.get_heads();
        assert_eq!(
            item_json(&doc, &heads, "i1").unwrap(),
            serde_json::json!({"body": "rich", "tags": ["a"]})
        );
    }

    #[test]
    fn an_item_created_by_a_change_is_absent_before_it() {
        let mut doc = AutoCommit::new();
        let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
        doc.commit();
        let before = doc.get_heads();
        let m2 = doc.put_object(&messages, "m2", ObjType::Map).unwrap();
        doc.put(&m2, "text", "new").unwrap();
        doc.commit();
        assert!(item_json(&doc, &before, "m2").is_none());
        let after = doc.get_heads();
        assert_eq!(item_json(&doc, &after, "m2").unwrap()["text"], "new");
    }

    #[test]
    fn a_value_kept_as_json_text_reads_as_its_object() {
        let mut doc = AutoCommit::new();
        let outbox = doc.put_object(ROOT, "outbox", ObjType::Map).unwrap();
        for (id, order, value) in [
            ("q1", "a", r#"{"op":{"op":"send"}}"#),
            ("q2", "b", "plain text"),
        ] {
            let stored = doc.put_object(&outbox, id, ObjType::Map).unwrap();
            doc.put(&stored, "k", order).unwrap();
            let entry = doc.put_object(&stored, "v", ObjType::Map).unwrap();
            doc.put(&entry, "value", value).unwrap();
        }
        doc.commit();
        let heads = doc.get_heads();
        let rows = outbox_entries(&doc, &heads, "outbox");
        assert_eq!(rows[0].value, serde_json::json!({"op": {"op": "send"}}));
        assert_eq!(rows[1].value, "plain text");
    }
}
