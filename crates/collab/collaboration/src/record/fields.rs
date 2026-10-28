//! Reading a record document's own fields, failing closed: a field present
//! in another shape, or conflicted, reads as invalid rather than absent.
//! The toolkit's record store (`ui::record`) and this crate's record readers
//! share these.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

/// Key, in a record's own document, of the map holding its identity.
pub const META: &str = "tilo:record";

/// An object field, as [`object_at`] reads it.
#[doc(hidden)]
pub enum ObjectAt {
    Missing,
    Object(ObjId),
    Invalid,
}

/// A text field, as [`string_at`] reads it.
#[doc(hidden)]
pub enum StringAt {
    Missing,
    String(String),
    Invalid,
}

/// The object of type `kind` at `key` of `object`.
#[doc(hidden)]
#[must_use]
pub fn object_at(doc: &AutoCommit, object: &ObjId, key: &str, kind: ObjType) -> ObjectAt {
    let Ok(mut values) = doc.get_all(object, key) else {
        return ObjectAt::Invalid;
    };
    if values.is_empty() {
        return ObjectAt::Missing;
    }
    if values.len() != 1 {
        return ObjectAt::Invalid;
    }
    match values.pop() {
        Some((Value::Object(found), id)) if found == kind => ObjectAt::Object(id),
        _ => ObjectAt::Invalid,
    }
}

/// The text at `key` of `object`.
#[doc(hidden)]
#[must_use]
pub fn string_at(doc: &AutoCommit, object: &ObjId, key: &str) -> StringAt {
    let Ok(mut values) = doc.get_all(object, key) else {
        return StringAt::Invalid;
    };
    if values.is_empty() {
        return StringAt::Missing;
    }
    if values.len() != 1 {
        return StringAt::Invalid;
    }
    match values.pop() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(value) => StringAt::String(value.to_string()),
            _ => StringAt::Invalid,
        },
        _ => StringAt::Invalid,
    }
}

/// The document's metadata map, when it has a valid one.
#[doc(hidden)]
#[must_use]
pub fn meta_ro(doc: &AutoCommit) -> Option<ObjId> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Missing | ObjectAt::Invalid => None,
    }
}
