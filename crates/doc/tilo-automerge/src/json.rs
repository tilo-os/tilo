//! A [`serde_json::Value`] stored as the document's own maps, lists and
//! scalars.
//!
//! A JSON value holds what another party shapes: an account type's connection
//! settings, a binding's literal value, a parameter's JSON Schema, a settings
//! section. It is stored as what it is: an object is a map, an array a list,
//! a string, a number, a boolean and `null` are scalars. So two people
//! changing different keys of one object both keep their change, and history
//! sees the key that changed, never a rewritten string.
//!
//! [`write`] changes only what differs, so writing a value the document
//! already holds makes no operation. [`read_at`] reads a subtree as it was at
//! given heads, bounded by a limit, for a reader outside the app that cannot
//! run the app's own model beside its keys.

use automerge::transaction::Transactable;
use automerge::{AutomergeError, ChangeHash, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};
use serde_json::{Map, Number};
use std::fmt;

/// Why a value could not be read or written as JSON.
#[derive(Debug)]
pub enum JsonError {
    Shape {
        /// Where the value is, as `config.folders.0`; empty at the value
        /// itself.
        path: String,
        expected: &'static str,
    },
    Automerge(AutomergeError),
}

impl JsonError {
    fn shape(expected: &'static str) -> Self {
        Self::Shape {
            path: String::new(),
            expected,
        }
    }

    fn at(mut self, segment: &str) -> Self {
        if let Self::Shape { path, .. } = &mut self {
            *path = if path.is_empty() {
                segment.to_owned()
            } else {
                format!("{segment}.{path}")
            };
        }
        self
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape { path, expected } if path.is_empty() => {
                write!(formatter, "expected {expected}")
            }
            Self::Shape { path, expected } => write!(formatter, "{path}: expected {expected}"),
            Self::Automerge(error) => write!(formatter, "document error: {error}"),
        }
    }
}

impl std::error::Error for JsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Automerge(error) => Some(error),
            Self::Shape { .. } => None,
        }
    }
}

impl From<AutomergeError> for JsonError {
    fn from(error: AutomergeError) -> Self {
        Self::Automerge(error)
    }
}

fn scalar_of(value: &serde_json::Value) -> Result<ScalarValue, JsonError> {
    Ok(match value {
        serde_json::Value::Null => ScalarValue::Null,
        serde_json::Value::Bool(flag) => ScalarValue::Boolean(*flag),
        serde_json::Value::String(text) => ScalarValue::Str(text.as_str().into()),
        serde_json::Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                ScalarValue::Int(int)
            } else if let Some(uint) = number.as_u64() {
                ScalarValue::Uint(uint)
            } else {
                ScalarValue::F64(
                    number
                        .as_f64()
                        .ok_or_else(|| JsonError::shape("a number"))?,
                )
            }
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return Err(JsonError::shape("a scalar"));
        }
    })
}

pub fn scalar_json(scalar: &ScalarValue) -> Result<serde_json::Value, JsonError> {
    Ok(match scalar {
        ScalarValue::Null => serde_json::Value::Null,
        ScalarValue::Boolean(flag) => serde_json::Value::Bool(*flag),
        ScalarValue::Str(text) => serde_json::Value::String(text.to_string()),
        ScalarValue::Int(int) | ScalarValue::Timestamp(int) => (*int).into(),
        ScalarValue::Uint(uint) => (*uint).into(),
        ScalarValue::Counter(counter) => i64::from(counter).into(),
        ScalarValue::F64(float) => Number::from_f64(*float)
            .map(serde_json::Value::Number)
            .ok_or_else(|| JsonError::shape("a finite number"))?,
        ScalarValue::Bytes(_) | ScalarValue::Unknown { .. } => {
            return Err(JsonError::shape("a JSON value"));
        }
    })
}

type Heads<'a> = Option<&'a [ChangeHash]>;

/// How a read goes: how many values it may still produce, and whether bytes read as their count
/// rather than as an error.
struct Budget {
    left: Option<usize>,
    bytes_counted: bool,
}

fn spend(budget: &mut Budget) -> bool {
    match &mut budget.left {
        Some(left) => match left.checked_sub(1) {
            Some(rest) => {
                *left = rest;
                true
            }
            None => false,
        },
        None => true,
    }
}

fn get<'a, D: ReadDoc + ?Sized>(
    doc: &'a D,
    obj: &ObjId,
    prop: impl Into<Prop>,
    heads: Heads<'_>,
) -> Result<Option<(Value<'a>, ObjId)>, AutomergeError> {
    match heads {
        Some(heads) => doc.get_at(obj, prop, heads),
        None => doc.get(obj, prop),
    }
}

fn keys<D: ReadDoc + ?Sized>(doc: &D, obj: &ObjId, heads: Heads<'_>) -> Vec<String> {
    match heads {
        Some(heads) => doc.keys_at(obj, heads).collect(),
        None => doc.keys(obj).collect(),
    }
}

fn length<D: ReadDoc + ?Sized>(doc: &D, obj: &ObjId, heads: Heads<'_>) -> usize {
    match heads {
        Some(heads) => doc.length_at(obj, heads),
        None => doc.length(obj),
    }
}

fn text<D: ReadDoc + ?Sized>(
    doc: &D,
    obj: &ObjId,
    heads: Heads<'_>,
) -> Result<String, AutomergeError> {
    match heads {
        Some(heads) => doc.text_at(obj, heads),
        None => doc.text(obj),
    }
}

fn value_of<D: ReadDoc + ?Sized>(
    doc: &D,
    value: &Value<'_>,
    id: &ObjId,
    heads: Heads<'_>,
    budget: &mut Budget,
) -> Result<serde_json::Value, JsonError> {
    match value {
        // Bytes have no JSON value: a whole document's reader is told how many there are.
        Value::Scalar(scalar) => match (scalar.as_ref(), budget.bytes_counted) {
            (ScalarValue::Bytes(bytes) | ScalarValue::Unknown { bytes, .. }, true) => {
                Ok(serde_json::json!({ "bytes": bytes.len() }))
            }
            (scalar, _) => scalar_json(scalar),
        },
        Value::Object(kind) => object_of(doc, id, *kind, heads, budget),
    }
}

fn object_of<D: ReadDoc + ?Sized>(
    doc: &D,
    id: &ObjId,
    kind: ObjType,
    heads: Heads<'_>,
    budget: &mut Budget,
) -> Result<serde_json::Value, JsonError> {
    Ok(match kind {
        ObjType::Text => serde_json::Value::String(text(doc, id, heads)?),
        ObjType::Map | ObjType::Table => {
            let mut fields = Map::new();
            for key in keys(doc, id, heads) {
                if !spend(budget) {
                    break;
                }
                let Some((value, inner)) = get(doc, id, key.as_str(), heads)? else {
                    continue;
                };
                let read =
                    value_of(doc, &value, &inner, heads, budget).map_err(|error| error.at(&key))?;
                fields.insert(key, read);
            }
            serde_json::Value::Object(fields)
        }
        ObjType::List => {
            let mut items = Vec::new();
            for index in 0..length(doc, id, heads) {
                if !spend(budget) {
                    break;
                }
                let read = match get(doc, id, index, heads)? {
                    Some((value, inner)) => value_of(doc, &value, &inner, heads, budget)
                        .map_err(|error| error.at(&index.to_string()))?,
                    None => serde_json::Value::Null,
                };
                items.push(read);
            }
            serde_json::Value::Array(items)
        }
    })
}

pub fn read_value<D: ReadDoc + ?Sized>(
    doc: &D,
    value: &Value<'_>,
    id: &ObjId,
) -> Result<serde_json::Value, JsonError> {
    value_of(
        doc,
        value,
        id,
        None,
        &mut Budget {
            left: None,
            bytes_counted: false,
        },
    )
}

/// The JSON value of the whole document as it is now: its maps, lists, text
/// as strings and scalars, bytes as their count.
pub fn read_document<D: ReadDoc + ?Sized>(doc: &D) -> Result<serde_json::Value, JsonError> {
    let mut budget = Budget {
        left: None,
        bytes_counted: true,
    };
    object_of(doc, &automerge::ROOT, ObjType::Map, None, &mut budget)
}

pub fn read_at<D: ReadDoc + ?Sized>(
    doc: &D,
    id: &ObjId,
    heads: &[ChangeHash],
    limit: Option<usize>,
) -> Result<serde_json::Value, JsonError> {
    let kind = doc.object_type(id)?;
    let mut budget = Budget {
        left: limit,
        bytes_counted: false,
    };
    object_of(doc, id, kind, Some(heads), &mut budget)
}

fn object_at<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: Prop,
    kind: ObjType,
) -> Result<ObjId, AutomergeError> {
    match doc.get(obj, prop.clone())? {
        Some((Value::Object(found), id)) if found == kind => Ok(id),
        _ => doc.put_object(obj, prop, kind),
    }
}

pub fn map_at<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: impl Into<Prop>,
) -> Result<ObjId, AutomergeError> {
    object_at(doc, obj, prop.into(), ObjType::Map)
}

/// Whether `current` is the value `next` writes, so writing it changes
/// nothing: a float by its bits, as `-0.0` and `0.0` are two values.
fn same_scalar(current: &ScalarValue, next: &ScalarValue) -> bool {
    match (current, next) {
        (ScalarValue::F64(current), ScalarValue::F64(next)) => current.to_bits() == next.to_bits(),
        (current, next) => current == next,
    }
}

/// Write the scalar `scalar` at `prop` of `obj` unless it is already there.
fn put_scalar<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: Prop,
    scalar: ScalarValue,
) -> Result<(), JsonError> {
    let same = matches!(
        doc.get(obj, prop.clone())?,
        Some((Value::Scalar(current), _)) if same_scalar(&current, &scalar)
    );
    if !same {
        doc.put(obj, prop, scalar)?;
    }
    Ok(())
}

/// Add the element `value` to the list `obj` at `index`.
fn insert_at<D: Transactable + ?Sized>(
    doc: &mut D,
    list: &ObjId,
    index: usize,
    value: &serde_json::Value,
) -> Result<(), JsonError> {
    match value {
        serde_json::Value::Object(fields) => {
            let map = doc.insert_object(list, index, ObjType::Map)?;
            for (key, inner) in fields {
                write_at(doc, &map, Prop::Map(key.clone()), inner)
                    .map_err(|error| error.at(key))?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let inner = doc.insert_object(list, index, ObjType::List)?;
            for (at, item) in items.iter().enumerate() {
                insert_at(doc, &inner, at, item).map_err(|error| error.at(&at.to_string()))?;
            }
            Ok(())
        }
        scalar => Ok(doc.insert(list, index, scalar_of(scalar)?)?),
    }
}

pub fn write_object<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: impl Into<Prop>,
    fields: &Map<String, serde_json::Value>,
) -> Result<(), JsonError> {
    let map = object_at(doc, obj, prop.into(), ObjType::Map)?;
    for (key, inner) in fields {
        write_at(doc, &map, Prop::Map(key.clone()), inner).map_err(|error| error.at(key))?;
    }
    let stale: Vec<String> = keys(doc, &map, None)
        .into_iter()
        .filter(|key| !fields.contains_key(key))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    Ok(())
}

fn write_at<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: Prop,
    value: &serde_json::Value,
) -> Result<(), JsonError> {
    if let Prop::Seq(index) = &prop
        && *index == doc.length(obj)
    {
        return insert_at(doc, obj, *index, value);
    }
    match value {
        serde_json::Value::Object(fields) => write_object(doc, obj, prop, fields),
        serde_json::Value::Array(items) => {
            let list = object_at(doc, obj, prop, ObjType::List)?;
            let held = doc.length(&list);
            for (index, item) in items.iter().enumerate() {
                write_at(doc, &list, Prop::Seq(index), item)
                    .map_err(|error| error.at(&index.to_string()))?;
            }
            for index in (items.len()..held).rev() {
                doc.delete(&list, index)?;
            }
            Ok(())
        }
        scalar => put_scalar(doc, obj, prop, scalar_of(scalar)?),
    }
}

/// Write `value` at `prop` of `obj`, changing only what differs: a key of an
/// object or an element of an array that holds the same value is left as it
/// is. An index one past the end of a list adds an element.
pub fn write<D: Transactable + ?Sized>(
    doc: &mut D,
    obj: &ObjId,
    prop: impl Into<Prop>,
    value: &serde_json::Value,
) -> Result<(), JsonError> {
    write_at(doc, obj, prop.into(), value)
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;
    use crate::ApplyIncoming;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT};

    fn a_json_value_is_stored_as_maps_lists_and_scalars() {
        let value = serde_json::json!({
            "host": "imap.example",
            "port": 993,
            "big": u64::MAX,
            "ratio": 0.5,
            "tls": true,
            "folders": ["INBOX", {"name": "Sent", "sync": false}],
            "proxy": null,
        });
        let mut doc = AutoCommit::new();
        write(&mut doc, &ROOT, "config", &value).unwrap();
        assert!(matches!(
            doc.get(&ROOT, "config").unwrap(),
            Some((Value::Object(ObjType::Map), _))
        ));
        let (_, config) = doc
            .get(&ROOT, "config")
            .expect("get config")
            .expect("config is there");
        assert_eq!(
            read_value(&doc, &Value::Object(ObjType::Map), &config).unwrap(),
            value
        );
        assert!(matches!(
            doc.get(&config, "folders").unwrap(),
            Some((Value::Object(ObjType::List), _))
        ));
    }

    fn an_unchanged_write_makes_no_operation() {
        let value = serde_json::json!({"a": [1, 2, {"b": "c"}], "d": "e"});
        let mut doc = AutoCommit::new();
        write(&mut doc, &ROOT, "v", &value).unwrap();
        doc.commit();
        write(&mut doc, &ROOT, "v", &value).unwrap();
        assert_eq!(doc.pending_ops(), 0);
    }

    fn removed_keys_and_elements_are_removed() {
        let mut doc = AutoCommit::new();
        write(
            &mut doc,
            &ROOT,
            "v",
            &serde_json::json!({"a": 1, "b": [1, 2, 3]}),
        )
        .unwrap();
        write(&mut doc, &ROOT, "v", &serde_json::json!({"b": [1]})).unwrap();
        let (_, v) = doc.get(&ROOT, "v").expect("get v").expect("v is there");
        assert_eq!(
            read_value(&doc, &Value::Object(ObjType::Map), &v).unwrap(),
            serde_json::json!({"b": [1]})
        );
        write(&mut doc, &ROOT, "v", &serde_json::json!("whole")).unwrap();
        assert_eq!(
            doc.get(&ROOT, "v").unwrap().unwrap().0,
            Value::Scalar(std::borrow::Cow::Owned(ScalarValue::Str("whole".into())))
        );
    }

    fn a_write_changes_only_the_key_that_differs() {
        let mut base = AutoCommit::new();
        write(
            &mut base,
            &ROOT,
            "config",
            &serde_json::json!({"host": "a", "port": 1}),
        )
        .unwrap();
        base.commit();
        let mut left = base.fork();
        let mut right = base.fork();
        write(
            &mut left,
            &ROOT,
            "config",
            &serde_json::json!({"host": "b", "port": 1}),
        )
        .unwrap();
        write(
            &mut right,
            &ROOT,
            "config",
            &serde_json::json!({"host": "a", "port": 2}),
        )
        .unwrap();
        left.merge_incoming(&mut right).unwrap();
        let (_, config) = left
            .get(&ROOT, "config")
            .expect("get config")
            .expect("config is there");
        assert_eq!(
            read_value(&left, &Value::Object(ObjType::Map), &config).unwrap(),
            serde_json::json!({"host": "b", "port": 2})
        );
    }

    fn a_subtree_reads_as_it_was_at_given_heads() {
        let mut doc = AutoCommit::new();
        let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
        let m1 = doc.put_object(&messages, "m1", ObjType::Map).unwrap();
        doc.put(&m1, "text", "hi").unwrap();
        let body = doc.put_object(&m1, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "rich").unwrap();
        let tags = doc.put_object(&m1, "tags", ObjType::List).unwrap();
        doc.insert(&tags, 0, "a").unwrap();
        doc.commit();
        let before = doc.get_heads();
        doc.put(&m1, "text", "hi there").unwrap();
        doc.commit();
        let after = doc.get_heads();
        assert_eq!(
            read_at(&doc, &m1, &before, None).unwrap(),
            serde_json::json!({"text": "hi", "body": "rich", "tags": ["a"]})
        );
        assert_eq!(
            read_at(&doc, &m1, &after, None).unwrap()["text"],
            "hi there"
        );
    }

    fn a_read_stops_at_its_limit() {
        let mut doc = AutoCommit::new();
        let fields: serde_json::Map<String, serde_json::Value> = (0..50)
            .map(|index| (format!("k{index}"), serde_json::json!(index)))
            .collect();
        write(&mut doc, &ROOT, "wide", &serde_json::Value::Object(fields)).unwrap();
        doc.commit();
        let (_, wide) = doc
            .get(&ROOT, "wide")
            .expect("get wide")
            .expect("wide is there");
        let heads = doc.get_heads();
        let read = read_at(&doc, &wide, &heads, Some(5)).expect("read wide");
        assert_eq!(read.as_object().expect("an object").len(), 5);
    }

    fn a_scalar_json_has_no_value_for_is_an_error_at_its_key() {
        let mut doc = AutoCommit::new();
        let map = doc.put_object(ROOT, "v", ObjType::Map).unwrap();
        doc.put(&map, "bytes", ScalarValue::Bytes(vec![1])).unwrap();
        let (_, v) = doc.get(&ROOT, "v").expect("get v").expect("v is there");
        let error = read_value(&doc, &Value::Object(ObjType::Map), &v).unwrap_err();
        assert_eq!(error.to_string(), "bytes: expected a JSON value");
    }

    #[test]
    fn a_read_is_as_of_its_heads_stops_at_its_limit_and_names_what_json_cannot_hold() {
        let cases: [(&str, fn()); 3] = [
            (
                "a subtree reads as it was at given heads",
                a_subtree_reads_as_it_was_at_given_heads,
            ),
            ("a read stops at its limit", a_read_stops_at_its_limit),
            (
                "a scalar json has no value for is an error at its key",
                a_scalar_json_has_no_value_for_is_an_error_at_its_key,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }

    fn an_index_one_past_the_end_adds_an_element() {
        let mut doc = AutoCommit::new();
        let list = doc.put_object(ROOT, "tags", ObjType::List).unwrap();
        write(&mut doc, &list, 0_usize, &serde_json::json!("a")).unwrap();
        write(&mut doc, &list, 1_usize, &serde_json::json!({"b": 1})).unwrap();
        assert_eq!(
            read_value(&doc, &Value::Object(ObjType::List), &list).unwrap(),
            serde_json::json!(["a", {"b": 1}])
        );
    }

    #[test]
    fn a_write_stores_json_as_maps_lists_and_scalars_and_changes_only_what_differs() {
        let cases: [(&str, fn()); 5] = [
            (
                "a json value is stored as maps lists and scalars",
                a_json_value_is_stored_as_maps_lists_and_scalars,
            ),
            (
                "an unchanged write makes no operation",
                an_unchanged_write_makes_no_operation,
            ),
            (
                "removed keys and elements are removed",
                removed_keys_and_elements_are_removed,
            ),
            (
                "a write changes only the key that differs",
                a_write_changes_only_the_key_that_differs,
            ),
            (
                "an index one past the end adds an element",
                an_index_one_past_the_end_adds_an_element,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }
}
