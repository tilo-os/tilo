//! JSON values stored as the document's own maps, lists and scalars.
//!
//! A [`serde_json::Value`] field holds JSON another party shapes: an account
//! type's connection settings, a binding's literal value, a parameter's JSON
//! Schema. It is stored as what it is: an object is a map, an array a list,
//! a string, a number, a boolean and `null` are scalars. So two people
//! changing different keys of one object both keep their change, and
//! history sees the key that changed, never a rewritten string.
//!
//! The mapping is [`tilo_automerge::json`]'s, which stores a settings section,
//! a page's JSON and a bot's view of an item the same way.
//!
//! [`value_model!`](crate::value_model) stores a serde type the same way, as
//! its JSON form: for a type `#[derive(Model)]` cannot describe, because it
//! is defined below this crate or holds itself (a condition of conditions).

use automerge::{ObjId, Prop, Value};

use crate::model::{Doc, Model, ModelError, Slot};
use crate::path::Settable;

/// The document's own error, or the shape a JSON value could not hold,
/// as [`ModelError`] reports it.
impl From<tilo_automerge::json::JsonError> for ModelError {
    fn from(error: tilo_automerge::json::JsonError) -> Self {
        match error {
            tilo_automerge::json::JsonError::Shape { path, expected } => {
                Self::Shape { path, expected }
            }
            tilo_automerge::json::JsonError::Automerge(error) => Self::Automerge(error),
        }
    }
}

/// The slot as the document names it: a key, or an index a list inserts at
/// when it is the list's end.
fn prop(slot: Slot<'_>) -> (&ObjId, Prop) {
    match slot {
        Slot::Key(obj, key) => (obj, Prop::Map(key.to_owned())),
        Slot::Index(obj, index) | Slot::Insert(obj, index) => (obj, Prop::Seq(index)),
    }
}

/// Write a JSON value into `slot`, changing only what differs.
fn store_json(doc: &mut Doc, slot: Slot<'_>, value: &serde_json::Value) -> Result<(), ModelError> {
    let (obj, prop) = prop(slot);
    Ok(tilo_automerge::json::write(doc, obj, prop, value)?)
}

/// The JSON value the document holds at `value` (the object `id`).
fn load_json(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<serde_json::Value, ModelError> {
    Ok(tilo_automerge::json::read_value(doc, value, id)?)
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
        load_json(doc, value, id)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::Null)
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        store_json(doc, slot, self)
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
        Self::from_json(&load_json(doc, value, id)?)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let (obj, prop) = prop(slot);
        Ok(tilo_automerge::json::write_object(doc, obj, prop, self)?)
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
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};
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
