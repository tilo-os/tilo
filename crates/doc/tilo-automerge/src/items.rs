//! An item of a record as JSON, read straight from the Automerge document
//! at given heads.
//!
//! A host process (a bot's door, the session serving History) never runs app
//! code beside its keys, so it cannot read an item through the app's model. Records keep their items in maps keyed
//! by item id, so the item is the first map entry with that key, searched
//! breadth first, and its subtree is read by [`tilo_automerge::json`], the one
//! JSON mapping every Tilo document stores by. What a host reads is therefore
//! the stored shape, not the app's typed one.

use crate::json;
use automerge::{ChangeHash, ObjId, ObjType, ReadDoc, Value as Am};
use serde_json::Value;

const MAX_DEPTH: usize = 6;
/// How many values a converted item may hold in all, so one huge item
/// cannot balloon an event.
const MAX_ENTRIES: usize = 10_000;

#[must_use]
pub fn item_json(doc: &impl ReadDoc, heads: &[ChangeHash], item: &str) -> Option<Value> {
    let object = find(doc, heads, item)?;
    let object = collection_value(doc, heads, &object).unwrap_or(object);
    Some(json::read_at(doc, &object, heads, Some(MAX_ENTRIES)).unwrap_or(Value::Null))
}

#[must_use]
pub fn collection_item_json(
    doc: &impl ReadDoc,
    heads: &[ChangeHash],
    field: &str,
    item: &str,
) -> Option<Value> {
    let collection = find(doc, heads, field)?;
    let Ok(Some((Am::Object(_), object))) = doc.get_at(&collection, item, heads) else {
        return None;
    };
    let object = collection_value(doc, heads, &object).unwrap_or(object);
    Some(json::read_at(doc, &object, heads, Some(MAX_ENTRIES)).unwrap_or(Value::Null))
}

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

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxRow {
    pub id: String,
    pub value: Value,
    pub settled: Option<Value>,
}

#[must_use]
pub fn outbox_entries(doc: &impl ReadDoc, heads: &[ChangeHash], field: &str) -> Vec<OutboxRow> {
    let Some(Value::Object(entries)) = item_json(doc, heads, field) else {
        return Vec::new();
    };
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

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
    fn a_collection_item_is_read_in_its_own_collection() {
        let mut doc = AutoCommit::new();
        for (field, text) in [("requests", "asked"), ("runs", "ran")] {
            let collection = doc.put_object(ROOT, field, ObjType::Map).unwrap();
            let entry = doc.put_object(&collection, "r1", ObjType::Map).unwrap();
            doc.put(&entry, "k", "a0").unwrap();
            let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
            doc.put(&value, "text", text).unwrap();
        }
        doc.commit();
        let heads = doc.get_heads();
        assert_eq!(
            collection_item_json(&doc, &heads, "runs", "r1").unwrap()["text"],
            "ran"
        );
        assert!(collection_item_json(&doc, &heads, "runs", "r2").is_none());
    }

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
    fn an_item_reads_as_json_as_of_the_heads_asked_for() {
        let cases: [(&str, fn()); 4] = [
            (
                "an item reads as json before and after a change",
                an_item_reads_as_json_before_and_after_a_change,
            ),
            (
                "a collection item reads as its value",
                a_collection_item_reads_as_its_value,
            ),
            (
                "text objects and lists read as strings and arrays",
                text_objects_and_lists_read_as_strings_and_arrays,
            ),
            (
                "an item created by a change is absent before it",
                an_item_created_by_a_change_is_absent_before_it,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
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
