//! The content a resource's own content names: every `tilo-blob://` link and `tilo.content:`
//! address in its rich text (its words, its marks and its blocks), its lists and its maps (their
//! keys too), read from its content document as each write leaves it, so whoever reads the
//! resource reads what it names by its address ([`crate::Index::naming_content`]).

use std::collections::BTreeSet;

use automerge::{Automerge, ObjId, ObjType, ROOT, ReadDoc as _, ScalarValue, Span, Value, hydrate};

use super::Store;

impl Store {
    /// Note what the content document `doc_id` names now, when it is the content of a resource
    /// ([`super::content_resource`]) and this store holds it: a write of it, a peer's change or
    /// its load. A document only a snapshot restored is noted from that snapshot.
    pub(super) fn note_named_content(&mut self, doc_id: &str) {
        let Some((kind, id)) = super::content_resource(doc_id) else {
            return;
        };
        let Some(doc) = self.docs.get(doc_id) else {
            return;
        };
        let mut named = BTreeSet::new();
        object(doc, &ROOT, &mut named);
        let (kind, id) = (kind.to_owned(), id.to_owned());
        self.index.name_in_content(doc_id, (&kind, &id), named);
    }
}

/// What `obj` of `doc` names, and every object in it.
fn object(doc: &Automerge, obj: &ObjId, named: &mut BTreeSet<String>) {
    match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => {
            for key in doc.keys(obj) {
                words(&key, named);
                if let Ok(Some((value, id))) = doc.get(obj, key.as_str()) {
                    value_of(doc, &value, &id, named);
                }
            }
        }
        Ok(ObjType::List) => {
            for at in 0..doc.length(obj) {
                if let Ok(Some((value, id))) = doc.get(obj, at) {
                    value_of(doc, &value, &id, named);
                }
            }
        }
        Ok(ObjType::Text) => {
            for span in doc.spans(obj).into_iter().flatten() {
                match span {
                    Span::Text { text, marks } => {
                        words(&text, named);
                        for (_, value) in marks.iter().flat_map(|marks| marks.iter()) {
                            scalar(value, named);
                        }
                    }
                    Span::Block(block) => hydrated_map(&block, named),
                }
            }
        }
        Err(_) => {}
    }
}

/// What `value`, held at `id`, names.
fn value_of(doc: &Automerge, value: &Value<'_>, id: &ObjId, named: &mut BTreeSet<String>) {
    match value {
        Value::Object(_) => object(doc, id, named),
        Value::Scalar(value) => scalar(value, named),
    }
}

/// What a text scalar names.
fn scalar(value: &ScalarValue, named: &mut BTreeSet<String>) {
    if let ScalarValue::Str(text) = value {
        words(text, named);
    }
}

/// What a block of rich text names: its attributes, whatever they hold.
fn hydrated_map(map: &hydrate::Map, named: &mut BTreeSet<String>) {
    for (key, entry) in map.iter() {
        words(key, named);
        hydrated(&entry.value, named);
    }
}

fn hydrated(value: &hydrate::Value, named: &mut BTreeSet<String>) {
    match value {
        hydrate::Value::Scalar(value) => scalar(value, named),
        hydrate::Value::Map(map) => hydrated_map(map, named),
        hydrate::Value::List(list) => {
            for item in list.iter() {
                hydrated(&item.value, named);
            }
        }
        hydrate::Value::Text(text) => words(&text.to_string(), named),
    }
}

/// What `text` names, anywhere in it.
fn words(text: &str, named: &mut BTreeSet<String>) {
    named.extend(tilo_actions::contents_named_in(text));
}
