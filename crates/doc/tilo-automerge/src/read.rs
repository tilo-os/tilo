//! Typed reads of one map slot, for documents whose schema is checked by the
//! reader: a missing key and a value of another type both read as `None`.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// The string at `obj[key]`.
pub fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((value, _))) => value.as_str().map(str::to_string),
        _ => None,
    }
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

/// The map object at `obj[key]`.
pub fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
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
