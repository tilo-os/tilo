//! Reading the index as the store's open writes leave it: an index snapshot with the
//! [`Overlay`] over it. Every read the store answers at open comes from here: a field, a
//! collection's items in order, those a filter holds of, what names a resource, where an item
//! is, and how a list changed between two such views.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use tilolang_runtime::store::{
    FieldKey, FieldRead, FieldValue, Listing, Lit, Pred, Splice, StoreError,
};

use super::derive::{item_of, member_list};
use super::lists::Entry;
use super::merged::{Base, Merged};
use super::overlay::{ListState, Overlay};
use super::{IndexError, KEY, Snapshot, TYPE};
use crate::reads::{default_value, may_hold_text};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

pub(super) fn index_error(error: IndexError) -> StoreError {
    StoreError::Backend(match error {
        IndexError::Corrupt => error.to_string(),
        IndexError::Io(message) => format!("the index cannot be read or written: {message}"),
    })
}

/// One package's index as the open writes leave it.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub snapshot: &'a Arc<Snapshot>,
    pub overlay: &'a Overlay,
}

/// A resource holding text, as [`View::every_text`] lists it.
#[derive(Clone, Debug)]
pub struct Texts {
    pub path: String,
    pub schema: &'static TypeSchema,
    /// Its texts that are not blank, in field order.
    pub texts: Vec<String>,
}

/// The path of the item `id` of the collection `field` of `resource`.
#[must_use]
pub fn child(resource: &str, field: &str, id: &str) -> String {
    format!("{resource}/{field}:{id}")
}

impl View<'_> {
    /// The value of `field` of `resource`, if the index holds one.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn field(&self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        if let Some(written) = self.overlay.field(resource, field) {
            return Ok(written);
        }
        self.snapshot.field(resource, field)
    }

    /// The type (`$type`) or order key (`$key`) of `resource`.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn text(&self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        if let Some(written) = self.overlay.text(resource, key) {
            return Ok(written);
        }
        self.snapshot.text(resource, key)
    }

    /// The type of `resource`, if the index holds it.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn type_of(&self, resource: &str) -> Result<Option<String>, IndexError> {
        self.text(resource, TYPE)
    }

    /// The schema of `resource`: its stored type's, else `fallback`'s.
    fn schema_of(&self, resource: &str, fallback: &str) -> Option<&'static TypeSchema> {
        self.type_of(resource)
            .ok()
            .flatten()
            .and_then(|ty| lookup(&ty))
            .or_else(|| lookup(fallback))
    }

    /// The items of the collection `field` of `resource`, in order.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list(&self, resource: &str, field: &str) -> Result<Merged, IndexError> {
        let name = member_list(resource, field);
        Ok(match self.overlay.list(resource, &name) {
            None => Merged::new(Base::Pages(self.snapshot.pages(&name)?), Vec::new(), &[]),
            Some(ListState::Delta { added, removed }) => {
                let removed: Vec<Entry> = removed.into_iter().collect();
                Merged::new(
                    Base::Pages(self.snapshot.pages(&name)?),
                    added.into_iter().collect(),
                    &removed,
                )
            }
            Some(ListState::Whole(entries)) => Merged::new(held(entries), Vec::new(), &[]),
        })
    }

    /// The listing of the collection `field` of `resource`, read page by page.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn listing(&self, resource: &str, field: &str) -> Result<Listing, IndexError> {
        Ok(Listing::paged(Rc::new(self.list(resource, field)?)))
    }

    /// The value `item_field` of the item at `path` holds, as a filter reads it.
    fn item_value(&self, path: &str, item_field: &FieldSchema) -> Result<FieldValue, IndexError> {
        Ok(self
            .field(path, &item_field.id)?
            .unwrap_or_else(|| default_value(item_field)))
    }

    /// Whether `pred` holds of the resource at `path` (of `schema`): its fields read from the
    /// index, a collection as how many items it holds.
    pub(super) fn holds(
        &self,
        path: &str,
        schema: &TypeSchema,
        pred: &Pred,
    ) -> Result<bool, StoreError> {
        pred.eval(&mut |read: &FieldRead| -> Result<Lit, StoreError> {
            let value = match schema.field(&read.spec.id) {
                Some(spec) if spec.kind == MergeKind::Collection => {
                    let len = self.list(path, &spec.id).map_err(index_error)?.len();
                    FieldValue::Counter(i64::try_from(len).unwrap_or(i64::MAX))
                }
                Some(spec) => self.item_value(path, spec).map_err(index_error)?,
                None => FieldValue::Absent,
            };
            read.lit(value)
        })
    }

    /// Where the item `id` is in the collection `field` of `resource`.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn position(
        &self,
        resource: &str,
        field: &str,
        id: &str,
    ) -> Result<Option<usize>, IndexError> {
        let Some(key) = self.text(&child(resource, field, id), KEY)? else {
            return Ok(None);
        };
        let (at, found) = self.list(resource, field)?.rank(&key, id);
        Ok(found.then_some(at))
    }

    /// The first non-empty text of the resource at `path` (of `schema`, unless the index holds
    /// another type for it), and the path of the resource holding it: fields by id, a
    /// collection's items in order ([`crate::reads::first_text`]).
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn first_text(
        &self,
        path: &str,
        schema: &TypeSchema,
    ) -> Result<Option<(String, String)>, IndexError> {
        let schema = self.schema_of(path, &schema.id).unwrap_or(schema);
        let mut fields: Vec<&FieldSchema> = schema.fields.iter().collect();
        fields.sort_by(|a, b| a.id.cmp(&b.id));
        for field in fields {
            match field.kind {
                MergeKind::Text | MergeKind::RichText => {
                    if let Some(text) = self.field(path, &field.id)?.and_then(crate::reads::text_of)
                        && !text.is_empty()
                    {
                        return Ok(Some((text, path.to_owned())));
                    }
                }
                MergeKind::Collection => {
                    let Some(item_schema) = field
                        .record
                        .as_deref()
                        .and_then(lookup)
                        .filter(|item| may_hold_text(item))
                    else {
                        continue;
                    };
                    let items = self.list(path, &field.id)?;
                    for index in 0..items.len() {
                        let Some(id) = items.get(index) else {
                            break;
                        };
                        let found = self.first_text(&child(path, &field.id, &id), item_schema)?;
                        if found.is_some() {
                            return Ok(found);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Every resource holding a text that is not blank, by path, with its schema and those
    /// texts in field order ([`Texts`]): what search finds of the package. One pass over the index's
    /// fields; a resource the open writes touched is read through them.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn every_text(&self) -> Result<Vec<Texts>, IndexError> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for held in self.snapshot.texted()? {
            if self.overlay.touched(&held.path) {
                seen.insert(held.path.clone());
                if let Some(found) = self.texts_at(&held.path)? {
                    out.push(found);
                }
                continue;
            }
            let Some(schema) = held.ty.as_deref().and_then(lookup) else {
                continue;
            };
            let texts: Vec<String> = schema
                .fields
                .iter()
                .filter(|f| f.kind.holds_text())
                .filter_map(|f| held.texts.get(&f.id))
                .filter(|text| !text.trim().is_empty())
                .cloned()
                .collect();
            if !texts.is_empty() {
                out.push(Texts {
                    path: held.path,
                    schema,
                    texts,
                });
            }
        }
        let made: Vec<String> = self
            .overlay
            .typed()
            .filter(|path| !seen.contains(*path))
            .map(str::to_owned)
            .collect();
        if !made.is_empty() {
            for path in made {
                if let Some(found) = self.texts_at(&path)? {
                    out.push(found);
                }
            }
            out.sort_by(|a, b| a.path.cmp(&b.path));
        }
        Ok(out)
    }

    /// The resource at `path` as [`Self::every_text`] lists it, read through the open writes:
    /// none when it is gone or holds no text.
    fn texts_at(&self, path: &str) -> Result<Option<Texts>, IndexError> {
        let Some(schema) = self.type_of(path)?.as_deref().and_then(lookup) else {
            return Ok(None);
        };
        let texts = self.texts(path, schema)?;
        Ok((!texts.is_empty()).then(|| Texts {
            path: path.to_owned(),
            schema,
            texts,
        }))
    }

    /// The texts of the resource at `path`, in field order, that are not blank: what search
    /// finds it by.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn texts(&self, path: &str, schema: &TypeSchema) -> Result<Vec<String>, IndexError> {
        let mut out = Vec::new();
        for field in schema.fields.iter().filter(|f| f.kind.holds_text()) {
            if let Some(text) = self.field(path, &field.id)?.and_then(crate::reads::text_of)
                && !text.trim().is_empty()
            {
                out.push(text);
            }
        }
        Ok(out)
    }
}

fn held(entries: Vec<Entry>) -> Base {
    Base::Held(
        entries
            .into_iter()
            .map(|(key, id)| (key, Rc::<str>::from(id)))
            .collect(),
    )
}

/// How the listing of the collection `coll` went from `before` to `after`, for the items
/// `ids` (those added, removed or moved): removals from the end first, then insertions in
/// order, each index the one the steps before it left.
///
/// # Errors
/// The index cannot be read or is corrupt.
pub fn splices(
    before: &View<'_>,
    after: &View<'_>,
    coll: &FieldKey,
    ids: &BTreeSet<String>,
) -> Result<Vec<Splice>, IndexError> {
    let (resource, field) = (coll.resource.as_str(), coll.field.as_str());
    let mut leaving: Vec<(usize, String)> = Vec::new();
    let mut arriving: Vec<Entry> = Vec::new();
    let mut old: Option<Merged> = None;
    for id in ids {
        let path = child(resource, field, id);
        let was = before.text(&path, KEY)?;
        let now = after.text(&path, KEY)?;
        if was == now {
            continue;
        }
        if let Some(key) = was {
            if old.is_none() {
                old = Some(before.list(resource, field)?);
            }
            if let Some(list) = old.as_ref()
                && let (at, true) = list.rank(&key, id)
            {
                leaving.push((at, id.clone()));
            }
        }
        if let Some(key) = now {
            arriving.push((key, id.clone()));
        }
    }
    let mut out = Vec::with_capacity(leaving.len().saturating_add(arriving.len()));
    if leaving.is_empty() && arriving.is_empty() {
        return Ok(out);
    }
    leaving.sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, item) in leaving {
        out.push(Splice::Remove { at, item });
    }
    arriving.sort_unstable();
    let new = after.list(resource, field)?;
    for (key, item) in arriving {
        let (at, _) = new.rank(&key, &item);
        out.push(Splice::Insert { at, item });
    }
    // An item that left and came back to the same place did not move.
    if let [
        Splice::Remove {
            at: from,
            item: gone,
        },
        Splice::Insert { at: to, item: back },
    ] = out.as_slice()
        && from == to
        && gone == back
    {
        out.clear();
    }
    Ok(out)
}

/// The resource holding the item at `path`, if it is one, with the collection and id.
#[must_use]
pub fn parent_of(path: &str) -> Option<(&str, &str, &str)> {
    item_of(path)
}

/// Every resource of the index (records and items) with text, by path: what search lists.
///
/// # Errors
/// The index cannot be read or is corrupt.
pub fn resources(snapshot: &Snapshot) -> Result<BTreeMap<String, String>, IndexError> {
    let mut out = BTreeMap::new();
    for (doc, ty) in snapshot.docs()? {
        for path in snapshot.inside(&doc)? {
            if let Some(ty) = snapshot.text(&path, TYPE)? {
                out.insert(path, ty);
            }
        }
        out.insert(doc, ty);
    }
    Ok(out)
}
