//! JSON values stored as the document's own maps, lists and scalars.
//!
//! A [`serde_json::Value`] field holds JSON another party shapes: an account
//! type's connection settings, a binding's literal value, a parameter's JSON
//! Schema. It is stored as what it is: an object is a map, an array a list,
//! a string, a number, a boolean and `null` are scalars. So two people
//! changing different keys of one object both keep their change, and
//! history sees the key that changed, never a rewritten string.
//!
//! [`value_model!`](crate::value_model) stores a serde type the same way, as
//! its JSON form: for a type `#[derive(Model)]` cannot describe, because it
//! is defined below this crate or holds itself (a condition of conditions).

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{Doc, LocatedAt as _, Model, ModelError, Slot};
use crate::path::Settable;

/// What a JSON scalar is stored as: a number as an integer when it is one.
fn scalar_of(value: &serde_json::Value) -> Result<ScalarValue, ModelError> {
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
                        .ok_or_else(|| ModelError::shape("a number"))?,
                )
            }
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return Err(ModelError::shape("a scalar"));
        }
    })
}

/// The JSON value of a stored scalar.
fn json_of_scalar(scalar: &ScalarValue) -> Result<serde_json::Value, ModelError> {
    Ok(match scalar {
        ScalarValue::Null => serde_json::Value::Null,
        ScalarValue::Boolean(flag) => serde_json::Value::Bool(*flag),
        ScalarValue::Str(text) => serde_json::Value::String(text.to_string()),
        ScalarValue::Int(int) | ScalarValue::Timestamp(int) => (*int).into(),
        ScalarValue::Uint(uint) => (*uint).into(),
        ScalarValue::Counter(counter) => i64::from(counter).into(),
        ScalarValue::F64(float) => serde_json::Number::from_f64(*float)
            .map(serde_json::Value::Number)
            .ok_or_else(|| ModelError::shape("a finite number"))?,
        ScalarValue::Bytes(_) | ScalarValue::Unknown { .. } => {
            return Err(ModelError::shape("a JSON value"));
        }
    })
}

/// The JSON value the document holds at `value` (the object `id`).
fn read_value(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<serde_json::Value, ModelError> {
    match value {
        Value::Scalar(scalar) => json_of_scalar(scalar),
        Value::Object(ObjType::Text) => Ok(serde_json::Value::String(doc.text(id)?)),
        Value::Object(ObjType::Map | ObjType::Table) => {
            let mut fields = serde_json::Map::new();
            for key in doc.keys(id).collect::<Vec<_>>() {
                if let Some((value, inner)) = doc.get(id, key.as_str())? {
                    let read = read_value(doc, &value, &inner).located(&key)?;
                    fields.insert(key, read);
                }
            }
            Ok(serde_json::Value::Object(fields))
        }
        Value::Object(ObjType::List) => {
            let mut items = Vec::with_capacity(doc.length(id));
            for index in 0..doc.length(id) {
                let read = match doc.get(id, index)? {
                    Some((value, inner)) => {
                        read_value(doc, &value, &inner).located(&index.to_string())?
                    }
                    None => serde_json::Value::Null,
                };
                items.push(read);
            }
            Ok(serde_json::Value::Array(items))
        }
    }
}

/// Write `value` into `slot`, changing only what differs: a key of an
/// object or an element of an array that holds the same value is left as
/// it is.
fn write_value(doc: &mut Doc, slot: Slot<'_>, value: &serde_json::Value) -> Result<(), ModelError> {
    match value {
        serde_json::Value::Object(fields) => {
            let map = slot.object(doc, ObjType::Map)?;
            for (key, inner) in fields {
                write_value(doc, Slot::Key(&map, key), inner).located(key)?;
            }
            let stale: Vec<String> = doc
                .keys(&map)
                .filter(|key| !fields.contains_key(key))
                .collect();
            for key in stale {
                doc.delete(&map, key.as_str())?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let list = slot.object(doc, ObjType::List)?;
            let existing = doc.length(&list);
            for (index, item) in items.iter().enumerate() {
                let slot = if index < existing {
                    Slot::Index(&list, index)
                } else {
                    Slot::Insert(&list, index)
                };
                write_value(doc, slot, item).located(&index.to_string())?;
            }
            for index in (items.len()..existing).rev() {
                doc.delete(&list, index)?;
            }
            Ok(())
        }
        scalar => slot.put_scalar(doc, scalar_of(scalar)?),
    }
}

/// Any JSON value, stored as the document's maps, lists and scalars; see
/// the module docs. An absent slot reads as `null`.
impl Settable for serde_json::Value {}
impl Model for serde_json::Value {
    crate::__own_table!(serde_json::Value);
    crate::model::leaf_paths!();
    const NULLABLE: bool = true;
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        self.clone()
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Ok(value.clone())
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        read_value(doc, value, id)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::Null)
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        write_value(doc, slot, self)
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

/// A JSON object, stored as a map: [`serde_json::Value`]'s object.
impl Settable for serde_json::Map<String, serde_json::Value> {}
impl Model for serde_json::Map<String, serde_json::Value> {
    crate::__own_table!(serde_json::Map<String, serde_json::Value>);
    crate::model::leaf_paths!();
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Object(self.clone())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Object(fields) => Ok(fields.clone()),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(ModelError::shape("an object")),
        }
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        Self::from_json(&read_value(doc, value, id)?)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        for (key, inner) in self {
            write_value(doc, Slot::Key(&map, key), inner).located(key)?;
        }
        let stale: Vec<String> = doc
            .keys(&map)
            .filter(|key| !self.contains_key(key))
            .collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        Ok(())
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

/// Implements [`Model`] (and [`Settable`](crate::Settable)) for serde types
/// stored as their JSON form, the way a [`serde_json::Value`] is: an object
/// as a map, an array as a list, everything else as a scalar. For a type
/// `#[derive(Model)]` cannot describe: one defined in a crate below this
/// one (an [`ActionKey`](tilo_actions::ActionKey)), or one that holds
/// itself (a condition made of conditions). A type the app shapes derives
/// `Model` instead, so its fields have paths and edits of their own.
///
/// The types need `Serialize`, `Deserialize` and a `PartialEq` holding only
/// between values of equal JSON forms, as a derived one does. An absent slot
/// has no value: hold one in an `Option`, a `Vec` or a map, or name the
/// types after `default:` to read an absent slot as their `Default`.
#[macro_export]
macro_rules! value_model {
    (default: $($ty:ty),* $(,)?) => {$(
        impl $crate::Settable for $ty {}
        impl $crate::Model for $ty {
            $crate::leaf_paths!();
            const TABLE: &'static $crate::table::ModelTable =
                &$crate::table::ModelTable::of_json(&$crate::table::ValueOf::<$ty>::DEFAULTED);
            $crate::__table_functions!();
            fn missing() -> ::core::result::Result<Self, $crate::ModelError> {
                ::core::result::Result::Ok(<Self as ::core::default::Default>::default())
            }
        }
    )*};
    ($($ty:ty),* $(,)?) => {$(
        impl $crate::Settable for $ty {}
        impl $crate::Model for $ty {
            $crate::leaf_paths!();
            const TABLE: &'static $crate::table::ModelTable =
                &$crate::table::ModelTable::of_json(&$crate::table::ValueOf::<$ty>::DESC);
            $crate::__table_functions!();
            fn missing() -> ::core::result::Result<Self, $crate::ModelError> {
                ::core::result::Result::Err($crate::ModelError::Missing {
                    path: ::std::string::String::new(),
                })
            }
        }
    )*};
}

// The action addresses and parameter declarations other apps publish, as a
// Shortcut step stores them: types of the action protocol, below this crate.
value_model!(tilo_actions::ActionKey, tilo_actions::ActionParam);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{read_key, write_key};
    use automerge::ROOT;
    use serde_json::json;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn a_json_value_is_stored_as_maps_lists_and_scalars() {
        // phase: it_reads_back_as_written
        {
            let value = json!({
                "host": "imap.example",
                "port": 993,
                "big": u64::MAX,
                "ratio": 0.5,
                "tls": true,
                "folders": ["INBOX", {"name": "Sent", "sync": false}],
                "proxy": null,
            });
            let mut doc = Doc::new();
            write_key(&mut doc, &ROOT, "config", &value).unwrap();
            assert_eq!(
                read_key::<serde_json::Value>(&doc, &ROOT, "config").unwrap(),
                value
            );
            let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "config").unwrap() else {
                panic!("an object is a map");
            };
            assert!(matches!(
                doc.get(&map, "folders").unwrap(),
                Some((Value::Object(ObjType::List), _))
            ));
            assert_eq!(
                read_key::<serde_json::Value>(&doc, &ROOT, "absent").unwrap(),
                serde_json::Value::Null
            );
        }
        // phase: an_unchanged_write_makes_no_operation
        {
            let value = json!({"a": [1, 2, {"b": "c"}], "d": "e"});
            let mut doc = Doc::new();
            write_key(&mut doc, &ROOT, "v", &value).unwrap();
            doc.commit();
            write_key(&mut doc, &ROOT, "v", &value).unwrap();
            assert_eq!(doc.pending_ops(), 0);
        }
        // phase: removed_keys_and_elements_are_removed
        {
            let mut doc = Doc::new();
            write_key(&mut doc, &ROOT, "v", &json!({"a": 1, "b": [1, 2, 3]})).unwrap();
            write_key(&mut doc, &ROOT, "v", &json!({"b": [1]})).unwrap();
            assert_eq!(
                read_key::<serde_json::Value>(&doc, &ROOT, "v").unwrap(),
                json!({"b": [1]})
            );
            write_key(&mut doc, &ROOT, "v", &json!("whole")).unwrap();
            assert_eq!(
                read_key::<serde_json::Value>(&doc, &ROOT, "v").unwrap(),
                json!("whole")
            );
        }
    }

    #[test]
    fn concurrent_changes_to_different_keys_both_survive() {
        let mut base = Doc::new();
        write_key(&mut base, &ROOT, "config", &json!({"host": "a", "port": 1})).unwrap();
        base.commit();
        let mut left = base.fork();
        let mut right = base.fork();
        write_key(&mut left, &ROOT, "config", &json!({"host": "b", "port": 1})).unwrap();
        write_key(
            &mut right,
            &ROOT,
            "config",
            &json!({"host": "a", "port": 2}),
        )
        .unwrap();
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(
            read_key::<serde_json::Value>(&left, &ROOT, "config").unwrap(),
            json!({"host": "b", "port": 2})
        );
    }

    #[test]
    fn a_value_model_type_is_stored_as_its_json_form() {
        let key = tilo_actions::ActionKey::new("so.tilo.notes", "notes.page", "rename");
        let mut doc = Doc::new();
        write_key(&mut doc, &ROOT, "action", &Some(key.clone())).unwrap();
        assert!(matches!(
            doc.get(&ROOT, "action").unwrap(),
            Some((Value::Object(ObjType::Map), _))
        ));
        assert_eq!(
            read_key::<Option<tilo_actions::ActionKey>>(&doc, &ROOT, "action").unwrap(),
            Some(key)
        );
        assert_eq!(
            read_key::<Option<tilo_actions::ActionKey>>(&doc, &ROOT, "absent").unwrap(),
            None
        );
        assert!(read_key::<tilo_actions::ActionKey>(&doc, &ROOT, "absent").is_err());
        let param = tilo_actions::ActionParam::required(
            "title",
            tilo_actions::ParamType::Text,
            "The page's title",
        );
        write_key(&mut doc, &ROOT, "params", &vec![param.clone()]).unwrap();
        assert_eq!(
            read_key::<Vec<tilo_actions::ActionParam>>(&doc, &ROOT, "params").unwrap(),
            vec![param]
        );
    }
}
