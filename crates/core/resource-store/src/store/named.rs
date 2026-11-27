//! The content a resource's own content names: every `tilo-blob://` link and `tilo.content:`
//! address in its rich text (its words, its marks and its blocks), its lists and its maps (their
//! keys too), read from its content document as each write leaves it, so whoever reads the
//! resource reads what it names by its address ([`crate::Index::naming_content`]).

use std::collections::BTreeSet;

use automerge::{
    Automerge, ObjId, ObjType, Patch, PatchAction, ROOT, ReadDoc as _, ScalarValue, Span, Value,
    hydrate,
};

use super::Store;

impl Store {
    /// Note what the content document `doc_id` names now, when it is the content of a resource
    /// ([`super::content_resource`]) and this store holds it: a write of it, a peer's change or
    /// its load. A document only a snapshot restored is noted from that snapshot.
    ///
    /// `changed` is what the write or the peer's change did to the document, when that is all
    /// that happened to it since it was last noted: what it names is then read from what the
    /// change put, not the whole document, so a write costs what it changed, however large the
    /// document is. `None` reads the whole document.
    pub(super) fn note_named_content(&mut self, doc_id: &str, changed: Option<&[Patch]>) {
        let Some((kind, id)) = super::content_resource(doc_id) else {
            return;
        };
        let Some(doc) = self.docs.get(doc_id) else {
            return;
        };
        let before: BTreeSet<String> = self.index.named_in(doc_id).map(str::to_owned).collect();
        let named = changed
            .and_then(|patches| named_after(doc, before.clone(), patches))
            .unwrap_or_else(|| {
                let mut named = BTreeSet::new();
                object(doc, &ROOT, &mut named);
                named
            });
        if named == before {
            return;
        }
        let (kind, id) = (kind.to_owned(), id.to_owned());
        self.index.name_in_content(doc_id, (&kind, &id), named);
    }
}

/// What a document that named `before` names once `patches` changed it, read from what they put;
/// `None` when they may have taken back something it named, which only the whole document says.
/// A document that named nothing loses nothing, and an insertion into a list never ends a name.
fn named_after(
    doc: &Automerge,
    mut named: BTreeSet<String>,
    patches: &[Patch],
) -> Option<BTreeSet<String>> {
    for patch in patches {
        // Nothing named yet, nothing can be taken back, the change's own earlier patches included.
        let named_nothing = named.is_empty();
        match &patch.action {
            PatchAction::Insert { values, .. } => {
                for (value, id, _) in values.iter() {
                    value_of(doc, value, id, &mut named);
                }
            }
            PatchAction::PutMap {
                key,
                value: (value, id),
                ..
            } if named_nothing => {
                words(key, &mut named);
                value_of(doc, value, id, &mut named);
            }
            PatchAction::PutSeq {
                value: (value, id), ..
            } if named_nothing => value_of(doc, value, id, &mut named),
            // Spliced or marked text can join or split a name across what it held: the text is
            // read again, all of it.
            PatchAction::SpliceText { .. } | PatchAction::Mark { .. } if named_nothing => {
                object(doc, &patch.obj, &mut named);
            }
            PatchAction::DeleteMap { .. } | PatchAction::DeleteSeq { .. } if named_nothing => {}
            PatchAction::Increment { .. } | PatchAction::Conflict { .. } => {}
            _ => return None,
        }
    }
    Some(named)
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
