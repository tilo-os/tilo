//! Typed reads of one map entry or list element.
//!
//! Each reader answers `None` when the entry is missing, is a different type,
//! or cannot be read, so callers state the type they expect instead of
//! matching `Ok(Some((Value::Scalar(..), _)))` themselves. Integer readers
//! take either integer encoding when the value fits, so a count one writer
//! stored as `Uint` reads the same through [`ReadTyped::int_at`]; floats are
//! never truncated into integers. The free [`int_at`] function retains the
//! saturating semantics used by existing document history readers.

use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// Typed readers for every [`ReadDoc`].
pub trait ReadTyped: ReadDoc {
    fn str_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<String> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The integer at `prop` of `obj`, when it fits an `i64`.
    fn int_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<i64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Int(number) => Some(*number),
                ScalarValue::Uint(number) => i64::try_from(*number).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The integer at `prop` of `obj`, when it fits a `u64`.
    fn uint_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<u64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Uint(number) => Some(*number),
                ScalarValue::Int(number) => u64::try_from(*number).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The number at `prop` of `obj` as a float.
    #[allow(
        clippy::cast_precision_loss,
        reason = "document numbers read as floats are measures well below 2^53"
    )]
    fn f64_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<f64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::F64(number) => Some(*number),
                ScalarValue::Int(number) => Some(*number as f64),
                ScalarValue::Uint(number) => Some(*number as f64),
                _ => None,
            },
            _ => None,
        }
    }

    fn bool_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<bool> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Boolean(flag) => Some(*flag),
                _ => None,
            },
            _ => None,
        }
    }

    fn bytes_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<Vec<u8>> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => Some(bytes.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn obj_at(&self, obj: &ObjId, prop: impl Into<Prop>, kind: ObjType) -> Option<ObjId> {
        match self.get(obj, prop) {
            Ok(Some((Value::Object(found), id))) if found == kind => Some(id),
            _ => None,
        }
    }
}

impl<T: ReadDoc + ?Sized> ReadTyped for T {}

pub fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    doc.str_at(obj, key)
}

/// The integer at `obj[key]`; an unsigned value past `i64::MAX` saturates.
pub fn int_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
            ScalarValue::Int(n) => Some(*n),
            ScalarValue::Uint(n) => Some(i64::try_from(*n).unwrap_or(i64::MAX)),
            _ => None,
        },
        _ => None,
    }
}

pub fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    doc.obj_at(obj, key, ObjType::Map)
}

/// Every string entry of the map `obj`, sorted by key; other values are skipped.
pub fn str_entries(doc: &AutoCommit, obj: &ObjId) -> BTreeMap<String, String> {
    doc.keys(obj)
        .filter_map(|key| str_at(doc, obj, &key).map(|value| (key, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    #[test]
    fn typed_readers_cover_scalars_objects_and_integer_bounds() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "signed", 7_i64).unwrap();
        doc.put(&ROOT, "unsigned", 7_u64).unwrap();
        doc.put(&ROOT, "negative", -1_i64).unwrap();
        doc.put(&ROOT, "huge", u64::MAX).unwrap();
        doc.put(&ROOT, "float", 1.5_f64).unwrap();
        doc.put(&ROOT, "flag", true).unwrap();
        doc.put(&ROOT, "bytes", vec![1_u8, 2]).unwrap();
        doc.put(&ROOT, "name", "tilo").unwrap();
        let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc.insert(&list, 0, "entry").unwrap();

        let doc = doc.document();
        assert_eq!(doc.int_at(&ROOT, "signed"), Some(7));
        assert_eq!(doc.int_at(&ROOT, "unsigned"), Some(7));
        assert_eq!(doc.int_at(&ROOT, "huge"), None);
        assert_eq!(doc.uint_at(&ROOT, "signed"), Some(7));
        assert_eq!(doc.uint_at(&ROOT, "unsigned"), Some(7));
        assert_eq!(doc.uint_at(&ROOT, "huge"), Some(u64::MAX));
        assert_eq!(doc.uint_at(&ROOT, "negative"), None);
        assert_eq!(doc.int_at(&ROOT, "float"), None);
        assert_eq!(doc.uint_at(&ROOT, "float"), None);
        assert_eq!(doc.f64_at(&ROOT, "float"), Some(1.5));
        assert_eq!(doc.f64_at(&ROOT, "signed"), Some(7.0));
        assert_eq!(doc.f64_at(&ROOT, "unsigned"), Some(7.0));
        assert_eq!(doc.f64_at(&ROOT, "name"), None);
        assert_eq!(doc.bool_at(&ROOT, "flag"), Some(true));
        assert_eq!(doc.bool_at(&ROOT, "signed"), None);
        assert_eq!(doc.bytes_at(&ROOT, "bytes"), Some(vec![1, 2]));
        assert_eq!(doc.bytes_at(&ROOT, "name"), None);
        assert_eq!(doc.obj_at(&ROOT, "list", ObjType::List), Some(list.clone()));
        assert_eq!(doc.obj_at(&ROOT, "list", ObjType::Map), None);
        assert_eq!(doc.obj_at(&ROOT, "name", ObjType::Map), None);
        assert_eq!(doc.str_at(&list, 0), Some("entry".to_string()));
        assert_eq!(doc.str_at(&list, 1), None);
        assert_eq!(doc.str_at(&ROOT, "missing"), None);
        assert_eq!(doc.str_at(&ROOT, "signed"), None);
    }

    #[test]
    fn typed_reads_check_ranges_types_and_list_slots() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "signed", 7_i64).unwrap();
        doc.put(&ROOT, "unsigned", 8_u64).unwrap();
        doc.put(&ROOT, "huge", u64::MAX).unwrap();
        doc.put(&ROOT, "negative", -1_i64).unwrap();
        doc.put(&ROOT, "float", 1.5_f64).unwrap();
        doc.put(&ROOT, "flag", true).unwrap();
        doc.put(&ROOT, "bytes", vec![1_u8, 2]).unwrap();
        let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc.insert(&list, 0, "first").unwrap();

        assert_eq!(doc.int_at(&ROOT, "signed"), Some(7));
        assert_eq!(doc.int_at(&ROOT, "unsigned"), Some(8));
        assert_eq!(doc.int_at(&ROOT, "huge"), None);
        assert_eq!(int_at(&doc, &ROOT, "huge"), Some(i64::MAX));
        assert_eq!(doc.uint_at(&ROOT, "signed"), Some(7));
        assert_eq!(doc.uint_at(&ROOT, "huge"), Some(u64::MAX));
        assert_eq!(doc.uint_at(&ROOT, "negative"), None);
        assert_eq!(doc.int_at(&ROOT, "float"), None);
        assert_eq!(doc.uint_at(&ROOT, "float"), None);
        assert_eq!(doc.f64_at(&ROOT, "float"), Some(1.5));
        assert_eq!(doc.f64_at(&ROOT, "signed"), Some(7.0));
        assert_eq!(doc.f64_at(&ROOT, "unsigned"), Some(8.0));
        assert_eq!(doc.bool_at(&ROOT, "flag"), Some(true));
        assert_eq!(doc.bytes_at(&ROOT, "bytes"), Some(vec![1, 2]));
        assert_eq!(doc.obj_at(&ROOT, "list", ObjType::List), Some(list.clone()));
        assert_eq!(map_at(&doc, &ROOT, "list"), None);
        assert_eq!(doc.str_at(&list, 0).as_deref(), Some("first"));
        assert_eq!(doc.str_at(&list, 1), None);
        assert_eq!(doc.bool_at(&ROOT, "bytes"), None);
        assert_eq!(doc.bytes_at(&ROOT, "flag"), None);
        assert_eq!(doc.f64_at(&ROOT, "missing"), None);

        let immutable = automerge::Automerge::load(&doc.save()).unwrap();
        assert_eq!(immutable.str_at(&list, 0).as_deref(), Some("first"));
        assert_eq!(immutable.int_at(&ROOT, "huge"), None);
    }

    #[test]
    fn slots_read_only_as_their_own_type() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "name", "tilo").unwrap();
        doc.put(&ROOT, "count", 7_i64).unwrap();
        doc.put(&ROOT, "huge", u64::MAX).unwrap();
        let map = doc.put_object(&ROOT, "meta", ObjType::Map).unwrap();
        doc.put(&map, "a", "1").unwrap();
        doc.put(&map, "b", 2_i64).unwrap();

        assert_eq!(str_at(&doc, &ROOT, "name").as_deref(), Some("tilo"));
        assert_eq!(str_at(&doc, &ROOT, "count"), None);
        assert_eq!(str_at(&doc, &ROOT, "missing"), None);
        assert_eq!(int_at(&doc, &ROOT, "count"), Some(7));
        assert_eq!(int_at(&doc, &ROOT, "huge"), Some(i64::MAX));
        assert_eq!(int_at(&doc, &ROOT, "name"), None);
        assert_eq!(map_at(&doc, &ROOT, "meta"), Some(map.clone()));
        assert_eq!(map_at(&doc, &ROOT, "name"), None);
        assert_eq!(
            str_entries(&doc, &map),
            BTreeMap::from([("a".to_string(), "1".to_string())])
        );
    }
}
