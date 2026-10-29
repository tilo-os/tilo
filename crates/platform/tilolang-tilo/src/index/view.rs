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

use super::derive::{column_field, in_of, item_path, member_list};
use super::lists::Entry;
use super::merged::{Base, Merged};
use super::overlay::{ListState, Overlay};
use super::{IN, IndexError, KEY, Snapshot, TYPE};
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
    pub overlay: &'a Arc<Overlay>,
}

/// A resource holding text, as [`View::every_text`] lists it.
#[derive(Clone, Debug)]
pub struct Texts {
    pub path: String,
    pub schema: &'static TypeSchema,
    /// Its texts that are not blank, in field order, as stored: texts and rich texts, read for
    /// a reader by whoever shows them ([`crate::reading::said`]).
    pub texts: Vec<FieldValue>,
    /// The resources its rich text refers to, in the one form (`tilo_reference::embed`).
    pub references: Vec<String>,
}

/// The path of the item `id` of a collection of `resource`: its id in the record, wherever it
/// is.
#[must_use]
pub fn child(resource: &str, id: &str) -> String {
    item_path(resource, id)
}

impl View<'_> {
    /// The value of `field` of `resource`, if the index holds one.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn field(&self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        // A resource's versions are the log's, with the provisional ones of the actions this
        // device committed and the record does not hold yet.
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

    /// The order key of the item at `path` in the collection `field` of `resource`: `None`
    /// when it is not in that collection.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn key_in(
        &self,
        path: &str,
        (resource, field): (&str, &str),
    ) -> Result<Option<String>, IndexError> {
        if self.text(path, IN)?.as_deref() != Some(in_of(resource, field).as_str()) {
            return Ok(None);
        }
        self.text(path, KEY)
    }

    /// [`Self::key_in`] as the index holds it, without the open writes.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn held_key_in(
        &self,
        path: &str,
        (resource, field): (&str, &str),
    ) -> Result<Option<String>, IndexError> {
        if self.snapshot.text(path, IN)?.as_deref() != Some(in_of(resource, field).as_str()) {
            return Ok(None);
        }
        self.snapshot.text(path, KEY)
    }

    /// Whether the resource at `path` is an item of the collection `field` of `resource`.
    #[must_use]
    pub fn is_item_of(&self, path: &str, (resource, field): (&str, &str)) -> bool {
        self.text(path, IN)
            .ok()
            .flatten()
            .is_some_and(|held| held == in_of(resource, field))
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
        let merged = match self.overlay.list(resource, &name) {
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
        };
        Ok(merged.locating(self.locate((resource, field), None)))
    }

    /// What finds the entries an item of the collection `field` of `resource` has, as this
    /// view holds them: in the list of its items, or, given an item type and a column of it,
    /// in that column's list ([`Merged::position`]).
    pub(super) fn locate(
        &self,
        (resource, field): (&str, &str),
        column: Option<(&str, &str)>,
    ) -> super::merged::Locate {
        let snapshot = Arc::clone(self.snapshot);
        let overlay = Arc::clone(self.overlay);
        let resource = resource.to_owned();
        let field = field.to_owned();
        let column = column.map(|(ty, name)| (ty.to_owned(), name.to_owned()));
        Box::new(move |id| {
            let view = View {
                snapshot: &snapshot,
                overlay: &overlay,
            };
            let column = column
                .as_ref()
                .map(|(ty, name)| (ty.as_str(), name.as_str()));
            view.entries_of((&resource, &field), id, column).ok()
        })
    }

    /// The entries the item `id` of the collection `field` of `resource` has in the list of its
    /// items, or in the list of the column `column` of its type: none when it is not there.
    /// A column's are those the index holds of the item and those its fields give now (they
    /// differ for an item the open writes changed); the list holds one of them.
    fn entries_of(
        &self,
        (resource, field): (&str, &str),
        id: &str,
        column: Option<(&str, &str)>,
    ) -> Result<Vec<Entry>, IndexError> {
        let path = child(resource, id);
        let Some(key) = self.key_in(&path, (resource, field))? else {
            return Ok(Vec::new());
        };
        let Some((ty, column)) = column else {
            return Ok(vec![(key, id.to_owned())]);
        };
        let schema = lookup(ty).ok_or(IndexError::Corrupt)?;
        let spec = schema.column(column).ok_or(IndexError::Corrupt)?;
        let mut values = self.column_values(&path, schema, spec)?;
        if let Some(held) = self.snapshot.text(&path, &column_field(column))? {
            values.extend(held.split(super::columns::SEP).map(str::to_owned));
        }
        values.sort();
        values.dedup();
        Ok(values
            .iter()
            .map(|value| super::columns::scope_entry(value, &key, id))
            .collect())
    }

    /// The values the column `column` of the resource at `path` (of `schema`) holds as this
    /// view reads its fields: a collection as how many items it holds.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub(super) fn column_values(
        &self,
        path: &str,
        schema: &TypeSchema,
        column: &crate::schema::ColumnSchema,
    ) -> Result<Vec<String>, IndexError> {
        super::columns::values(column, schema, &mut |f| {
            if f.kind == MergeKind::Collection {
                let len = self.list(path, &f.id)?.len();
                return Ok(FieldValue::Counter(i64::try_from(len).unwrap_or(i64::MAX)));
            }
            Ok(self.field(path, &f.id)?.unwrap_or_else(|| default_value(f)))
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
        let Some(key) = self.key_in(&child(resource, id), (resource, field))? else {
            return Ok(None);
        };
        let (at, found) = self.list(resource, field)?.rank(&key, id);
        Ok(found.then_some(at))
    }

    /// The first non-empty text of the resource at `path` (of `schema`, unless the index holds
    /// another type for it), as stored (a text or rich text, [`crate::reads::text_value`]), and
    /// the path of the resource holding it: fields by id, a collection's items in order
    /// ([`crate::reads::first_text`]).
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn first_text(
        &self,
        path: &str,
        schema: &TypeSchema,
    ) -> Result<Option<(FieldValue, String)>, IndexError> {
        let schema = self.schema_of(path, &schema.id).unwrap_or(schema);
        let mut fields: Vec<&FieldSchema> = schema.fields.iter().collect();
        fields.sort_by(|a, b| a.id.cmp(&b.id));
        for field in fields {
            match field.kind {
                MergeKind::Text | MergeKind::RichText => {
                    if let Some(text) = self
                        .field(path, &field.id)?
                        .and_then(crate::reads::text_value)
                        && !crate::reads::is_empty(&text)
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
                        let found = self.first_text(&child(path, &id), item_schema)?;
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
            let texts: Vec<FieldValue> = schema
                .fields
                .iter()
                .filter(|f| f.kind.holds_text())
                .filter_map(|f| held.texts.get(&f.id))
                .filter(|text| !crate::reads::is_blank(text))
                .cloned()
                .collect();
            if !texts.is_empty() {
                let references = self.references(&held.path, schema)?;
                out.push(Texts {
                    path: held.path,
                    schema,
                    texts,
                    references,
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
        if texts.is_empty() {
            return Ok(None);
        }
        let references = self.references(path, schema)?;
        Ok(Some(Texts {
            path: path.to_owned(),
            schema,
            texts,
            references,
        }))
    }

    /// The resources the rich text of the resource at `path` refers to, each once, in the one
    /// form: what search finds it by as they are called now, and what its backlinks are.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn references(&self, path: &str, schema: &TypeSchema) -> Result<Vec<String>, IndexError> {
        let mut out: Vec<String> = Vec::new();
        for field in schema
            .fields
            .iter()
            .filter(|f| f.kind == crate::schema::MergeKind::RichText)
        {
            if let Some(FieldValue::Rich(rich)) = self.field(path, &field.id)? {
                for reference in rich.references() {
                    if reference.as_resource().is_some() {
                        let value = reference.mark_value();
                        if !out.contains(&value) {
                            out.push(value);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The texts of the resource at `path`, in field order, that are not blank: what search
    /// finds it by, as stored (read for a reader by whoever shows them).
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn texts(&self, path: &str, schema: &TypeSchema) -> Result<Vec<FieldValue>, IndexError> {
        let mut out = Vec::new();
        for field in schema.fields.iter().filter(|f| f.kind.holds_text()) {
            if let Some(text) = self
                .field(path, &field.id)?
                .and_then(crate::reads::text_value)
                && !crate::reads::is_blank(&text)
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
        let path = child(resource, id);
        let was = before.key_in(&path, (resource, field))?;
        let now = after.key_in(&path, (resource, field))?;
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
