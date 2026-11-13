//! Write another document's content into this one, as it is.

use std::collections::BTreeMap;

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, AutomergeError, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use super::{Containers, DocMut};

/// Where a copied value goes: under a key of a map, or at an index of a list.
#[derive(Clone, Copy)]
enum At<'k> {
    Key(&'k str),
    Index(usize),
}

impl DocMut<'_> {
    /// Write every entry of `from`'s containers (blocks, regions, notes, comments, styles, package
    /// parts, content descriptors and meta) into this document's, each under its own key, in place
    /// of any entry there. `rename` maps a string `from` holds, a content address it names its
    /// images by, to the one this document names it by instead: in a key, a value or a mark alike.
    ///
    /// # Errors
    ///
    /// The write automerge refused; what was written before it stays.
    pub fn graft(
        &mut self,
        from: &AutoCommit,
        c: &Containers,
        rename: &BTreeMap<String, String>,
    ) -> Result<(), AutomergeError> {
        self.flush_bulk();
        let pairs = [
            (&c.blocks, &self.c.blocks),
            (&c.styles, &self.c.styles),
            (&c.preserved, &self.c.preserved),
            (&c.meta, &self.c.meta),
            (&c.blobs, &self.c.blobs),
            (&c.header, &self.c.header),
            (&c.footer, &self.c.footer),
            (&c.notes, &self.c.notes),
            (&c.comments, &self.c.comments),
            (&c.changes, &self.c.changes),
        ];
        for (source, target) in pairs {
            copy_entries(self.doc, target, from, source, ObjType::Map, rename)?;
            self.ids.order.by_container.remove(target);
        }
        self.ids.run = None;
        Ok(())
    }
}

fn renamed<'s>(text: &'s str, rename: &'s BTreeMap<String, String>) -> &'s str {
    rename.get(text).map_or(text, String::as_str)
}

fn renamed_scalar(scalar: ScalarValue, rename: &BTreeMap<String, String>) -> ScalarValue {
    match scalar {
        ScalarValue::Str(text) => match rename.get(text.as_str()) {
            Some(name) => ScalarValue::Str(name.as_str().into()),
            None => ScalarValue::Str(text),
        },
        other => other,
    }
}

fn copy_entries(
    to: &mut AutoCommit,
    target: &ObjId,
    from: &AutoCommit,
    source: &ObjId,
    kind: ObjType,
    rename: &BTreeMap<String, String>,
) -> Result<(), AutomergeError> {
    match kind {
        ObjType::Map | ObjType::Table => {
            for key in from.keys(source) {
                if let Some((value, id)) = from.get(source, key.as_str())? {
                    let key = renamed(&key, rename);
                    copy_value(to, target, At::Key(key), from, value, &id, rename)?;
                }
            }
        }
        ObjType::List => {
            for index in 0..from.length(source) {
                if let Some((value, id)) = from.get(source, index)? {
                    copy_value(to, target, At::Index(index), from, value, &id, rename)?;
                }
            }
        }
        ObjType::Text => {
            to.splice_text(target, 0, 0, &from.text(source)?)?;
            for mark in from.marks(source)? {
                let value = renamed_scalar(mark.value().clone(), rename);
                let mark = Mark::new(mark.name().to_owned(), value, mark.start, mark.end);
                to.mark(target, mark, ExpandMark::After)?;
            }
        }
    }
    Ok(())
}

fn copy_value(
    to: &mut AutoCommit,
    target: &ObjId,
    at: At<'_>,
    from: &AutoCommit,
    value: Value<'_>,
    source: &ObjId,
    rename: &BTreeMap<String, String>,
) -> Result<(), AutomergeError> {
    match value {
        Value::Scalar(scalar) => {
            let scalar = renamed_scalar(scalar.into_owned(), rename);
            match at {
                At::Key(key) => to.put(target, key, scalar),
                At::Index(index) => to.insert(target, index, scalar),
            }
        }
        Value::Object(kind) => {
            let object = match at {
                At::Key(key) => to.put_object(target, key, kind)?,
                At::Index(index) => to.insert_object(target, index, kind)?,
            };
            copy_entries(to, &object, from, source, kind, rename)
        }
    }
}
