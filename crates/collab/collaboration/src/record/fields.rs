//! Reading a record document's own fields, failing closed: a field present
//! in another shape, or written by concurrent changes that disagree, reads as
//! invalid rather than absent. Two devices writing the same value without
//! having seen each other's change agree, and read as that value.
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
    let Some((Value::Object(found), id)) = values.pop() else {
        return ObjectAt::Invalid;
    };
    if found != kind {
        return ObjectAt::Invalid;
    }
    // Concurrent writes of the same object are the same value, not a
    // conflict: a second device writing what the first one wrote changes
    // nothing about what the field means.
    if values.iter().all(|(value, other)| {
        matches!(value, Value::Object(shape) if *shape == kind) && *other == id
    }) {
        ObjectAt::Object(id)
    } else {
        ObjectAt::Invalid
    }
}

/// The text at `key` of `object`.
#[doc(hidden)]
#[must_use]
pub fn string_at(doc: &AutoCommit, object: &ObjId, key: &str) -> StringAt {
    let Ok(values) = doc.get_all(object, key) else {
        return StringAt::Invalid;
    };
    let text_of = |value: &Value<'_>| match value {
        Value::Scalar(scalar) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Value::Object(_) => None,
    };
    let Some((first, _)) = values.first() else {
        return StringAt::Missing;
    };
    let Some(text) = text_of(first) else {
        return StringAt::Invalid;
    };
    // As with objects: devices that wrote the same text independently have
    // agreed, so the field reads as that text rather than as a conflict.
    if values
        .iter()
        .all(|(value, _)| text_of(value).as_deref() == Some(&text))
    {
        StringAt::String(text)
    } else {
        StringAt::Invalid
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ActorId, AutoCommit, ObjType, ROOT};

    /// Two devices' documents over one record, each writing `values` at
    /// `key` of the shared metadata map without seeing the other's edit.
    fn written_apart(values: (&str, &str)) -> (AutoCommit, ObjId) {
        let mut base = AutoCommit::new();
        let meta = base.put_object(&ROOT, META, ObjType::Map).unwrap();
        let mut one = base.fork();
        one.set_actor(ActorId::random());
        one.put(&meta, "home", values.0).unwrap();
        let mut two = base.fork();
        two.set_actor(ActorId::random());
        two.put(&meta, "home", values.1).unwrap();
        one.merge(&mut two).unwrap();
        (one, meta)
    }

    #[test]
    fn a_field_two_devices_wrote_alike_reads_as_written() {
        let (doc, meta) = written_apart(("personal", "personal"));
        let StringAt::String(home) = string_at(&doc, &meta, "home") else {
            panic!("two devices writing the same home agree");
        };
        assert_eq!(home, "personal");
    }

    #[test]
    fn a_field_two_devices_wrote_apart_does_not_read() {
        let (doc, meta) = written_apart(("personal", "tilo:abc"));
        assert!(matches!(string_at(&doc, &meta, "home"), StringAt::Invalid));
    }
}
