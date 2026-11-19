//! The index and the hook every commit updates it through.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

use automerge::ChangeHash;

use crate::query::{Cursor, Filter, Page, Query, Sort, past};
use crate::value::{Cell, Value};

/// The field a resource's parent is indexed under: a filter `Is(PARENT, parent)` lists the
/// resources nested in one resource.
pub const PARENT: &str = "$parent";

/// What the index holds of one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indexed {
    /// The document the resource is stored in: where a read of what the index does not hold goes.
    pub doc: String,
    /// The resource it is nested in; `None` for a resource at the top of the workspace.
    pub parent: Option<String>,
    /// Its place among its parent's resources of its kind; `None` for a kind without one.
    pub order: Option<String>,
    /// Each indexed field it holds, by name.
    pub fields: BTreeMap<String, Cell>,
}

impl Indexed {
    /// The scalar field `field` holds; [`Value::Null`] when it holds none.
    #[must_use]
    pub fn scalar(&self, field: &str) -> Value {
        if field == PARENT {
            return self.parent.clone().map_or(Value::Null, Value::Text);
        }
        self.fields
            .get(field)
            .and_then(Cell::one)
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Where the resource stands in `sort`: what it sorts by, then its place.
    fn key(&self, sort: &Sort) -> Option<(Value, Value)> {
        match sort {
            Sort::Position => Some((position(self), Value::Null)),
            Sort::Field(field) if field == PARENT => Some((self.scalar(PARENT), position(self))),
            Sort::Field(field) => {
                let value = self.fields.get(field).and_then(Cell::one)?.clone();
                Some((value, position(self)))
            }
        }
    }

    /// Each value the resource is listed under in the index of `field`.
    fn indexed(&self) -> impl Iterator<Item = (&str, Value)> {
        let parent = self
            .parent
            .as_ref()
            .map(|parent| (PARENT, Value::Text(parent.clone())));
        parent
            .into_iter()
            .chain(self.fields.iter().flat_map(|(field, cell)| {
                let values: Vec<Value> = match cell {
                    Cell::One(value) => vec![value.clone()],
                    Cell::Set(members) => members.iter().cloned().collect(),
                };
                values.into_iter().map(move |value| (field.as_str(), value))
            }))
    }
}

fn position(stored: &Indexed) -> Value {
    stored.order.clone().map_or(Value::Null, Value::Text)
}

/// One resource a commit changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// The resource as the commit leaves it: where it is nested, its place, and every indexed
    /// field it holds.
    Put {
        kind: String,
        id: String,
        parent: Option<String>,
        order: Option<String>,
        fields: BTreeMap<String, Cell>,
    },
    /// The resource left the commit's document: removed, or moved to another one (whose commit
    /// puts it there).
    Remove { kind: String, id: String },
}

/// What one commit changed in one document. See the crate docs.
#[derive(Clone, Copy, Debug)]
pub struct Commit<'a> {
    /// The document committed to.
    pub doc: &'a str,
    /// Who may read the document: the reader set its resources were placed by, which picks
    /// its key. Every resource of a document has the same readers.
    pub readers: &'a str,
    /// The document's heads after the commit: what the index reflects of it.
    pub heads: &'a [ChangeHash],
    pub changes: &'a [Change],
}

/// What the store calls in the apply that commits a change, before anyone reads the document
/// again: the index reflects each commit the moment the document does.
pub trait CommitHook {
    fn committed(&mut self, commit: &Commit<'_>);
}

/// One entry of an order: what it sorts by, the resource's place, the resource's id.
type Entry = (Value, Value, String);

/// The resources of one kind, wherever they are stored, and their orders.
#[derive(Default)]
struct Table {
    rows: BTreeMap<String, Indexed>,
    /// Every resource by its place.
    by_position: BTreeSet<Entry>,
    /// Per scalar field, every resource holding it by its value, then by its place; per set
    /// field, every member. A filter on one value pages in the kind's order from here alone.
    by_field: HashMap<String, BTreeSet<Entry>>,
    /// Per field, how many resources hold each value: what a query reads to choose its path, and
    /// a count of one value answers from.
    counts: HashMap<String, HashMap<Value, usize>>,
}

impl Table {
    fn insert(&mut self, id: &str, stored: Indexed) {
        self.unlink(id);
        let place = position(&stored);
        self.by_position
            .insert((place.clone(), Value::Null, id.to_owned()));
        for (field, value) in stored.indexed() {
            let counted = self
                .counts
                .entry(field.to_owned())
                .or_default()
                .entry(value.clone())
                .or_default();
            *counted = counted.saturating_add(1);
            self.by_field.entry(field.to_owned()).or_default().insert((
                value,
                place.clone(),
                id.to_owned(),
            ));
        }
        self.rows.insert(id.to_owned(), stored);
    }

    /// Takes `id` out of every order, keeping its row; whether it had one.
    fn unlink(&mut self, id: &str) -> bool {
        let Some(stored) = self.rows.get(id) else {
            return false;
        };
        let place = position(stored);
        let mut key = (place.clone(), Value::Null, id.to_owned());
        self.by_position.remove(&key);
        key.1 = place;
        for (field, value) in stored.indexed() {
            if let Some(counts) = self.counts.get_mut(field)
                && let Some(counted) = counts.get_mut(&value)
            {
                *counted = counted.saturating_sub(1);
                if *counted == 0 {
                    counts.remove(&value);
                }
            }
            if let Some(entries) = self.by_field.get_mut(field) {
                key.0 = value;
                entries.remove(&key);
            }
        }
        true
    }

    fn remove(&mut self, id: &str) {
        if self.unlink(id) {
            self.rows.remove(id);
        }
    }

    /// The ids holding `value` in `field`, as its index lists them.
    fn holding<'a>(
        &'a self,
        field: &str,
        value: &'a Value,
    ) -> Option<impl Iterator<Item = &'a str> + 'a> {
        let entries = self.by_field.get(field)?;
        Some(
            entries
                .range((
                    Bound::Included((value.clone(), Value::Null, String::new())),
                    Bound::Unbounded,
                ))
                .take_while(move |(held, _, _)| held == value)
                .map(|(_, _, id)| id.as_str()),
        )
    }

    /// The ids whose `field` lies between the bounds, as its index lists them.
    fn ranging<'a>(
        &'a self,
        field: &str,
        from: &'a Bound<Value>,
        to: &'a Bound<Value>,
    ) -> Option<impl Iterator<Item = &'a str> + 'a> {
        let entries = self.by_field.get(field)?;
        let start = match from {
            Bound::Included(value) | Bound::Excluded(value) => {
                Bound::Included((value.clone(), Value::Null, String::new()))
            }
            Bound::Unbounded => Bound::Unbounded,
        };
        Some(
            entries
                .range((start, Bound::Unbounded))
                .skip_while(
                    move |(held, _, _)| matches!(from, Bound::Excluded(from) if held == from),
                )
                .take_while(move |(held, _, _)| !past(held, to))
                .map(|(_, _, id)| id.as_str()),
        )
    }

    /// The ids a query reads before its other filters: those its first value filter names,
    /// else those its first range names; `None` when no filter narrows the kind.
    /// How many resources hold `value` in `field`.
    fn holders(&self, field: &str, value: &Value) -> usize {
        self.counts
            .get(field)
            .and_then(|counts| counts.get(value))
            .copied()
            .unwrap_or(0)
    }

    fn narrowed<'a>(&'a self, filters: &'a [Filter]) -> Option<Vec<&'a str>> {
        let by_value = filters.iter().find_map(|filter| match filter {
            Filter::Is(field, value) | Filter::Has(field, value) => {
                Some(self.holding(field, value).map(Iterator::collect))
            }
            _ => None,
        });
        let by_range = || {
            filters.iter().find_map(|filter| match filter {
                Filter::Within { field, from, to } => {
                    Some(self.ranging(field, from, to).map(Iterator::collect))
                }
                _ => None,
            })
        };
        // A filter on a field no resource holds names none.
        by_value.or_else(by_range).map(|ids: Option<Vec<&str>>| {
            let mut ids = ids.unwrap_or_default();
            ids.sort_unstable();
            ids.dedup();
            ids
        })
    }

    /// The entries of `entries` from `from` on (after it), in `descending` order or not, that
    /// `keep` keeps, `wanted` at most.
    fn scan(
        entries: &BTreeSet<Entry>,
        lower: Bound<Entry>,
        upper: Bound<Entry>,
        descending: bool,
        wanted: usize,
        keep: impl FnMut(&&Entry) -> bool,
    ) -> Vec<Entry> {
        let range = entries.range((lower, upper));
        if descending {
            range.rev().filter(keep).take(wanted).cloned().collect()
        } else {
            range.filter(keep).take(wanted).cloned().collect()
        }
    }

    /// The resources `ids` names that hold to `query`'s filters, in its order from `from` on,
    /// `wanted` at most.
    fn sorted(
        &self,
        ids: Vec<&str>,
        query: &Query,
        from: Option<&Entry>,
        wanted: usize,
    ) -> Vec<Entry> {
        let order = &query.order;
        let starts = |key: &Entry| match from {
            None => true,
            Some(from) if order.descending => key < from,
            Some(from) => key > from,
        };
        let mut keys: Vec<Entry> = ids
            .into_iter()
            .filter_map(|id| {
                let stored = self.rows.get(id)?;
                let (key, place) = stored.key(&order.by)?;
                query
                    .filters
                    .iter()
                    .all(|filter| filter.holds(stored))
                    .then(|| (key, place, id.to_owned()))
            })
            .filter(|key| starts(key))
            .collect();
        if order.descending {
            keys.sort_unstable_by(|a, b| b.cmp(a));
        } else {
            keys.sort_unstable();
        }
        keys.truncate(wanted);
        keys
    }

    /// Where the first `query.limit + 1` resources this table shows of `query` stand in its
    /// order.
    fn keys(&self, query: &Query) -> Vec<Entry> {
        let order = &query.order;
        let holds = |stored: &Indexed| query.filters.iter().all(|filter| filter.holds(stored));
        let from: Option<Entry> = query
            .after
            .as_ref()
            .map(|cursor| (cursor.key.clone(), cursor.place.clone(), cursor.id.clone()));
        let wanted = query.limit.saturating_add(1);
        // Whether a resource listed at `entry` is listed there for the order, not for a member of
        // a set or another field the entries also hold.
        let sorts_at = |stored: &Indexed, key: &Value, place: &Value| {
            stored
                .key(&order.by)
                .is_some_and(|(held, at)| held == *key && at == *place)
        };
        let by_value = query.filters.iter().find_map(|filter| match filter {
            Filter::Is(field, value) | Filter::Has(field, value) => Some((field, value)),
            _ => None,
        });
        // One value's resources in the kind's order: a range of that field's entries.
        let mut keys: Vec<Entry> =
            if let (Sort::Position, Some((field, value))) = (&order.by, by_value) {
                let Some(entries) = self.by_field.get(field.as_str()) else {
                    return Vec::new();
                };
                let first = (value.clone(), Value::Null, String::new());
                let resume = from.map(|(place, _, id)| (value.clone(), place, id));
                let (lower, upper) = if order.descending {
                    (
                        Bound::Included(first),
                        resume.map_or(Bound::Unbounded, Bound::Excluded),
                    )
                } else {
                    (
                        resume.map_or(Bound::Included(first), Bound::Excluded),
                        Bound::Unbounded,
                    )
                };
                let keep =
                    |(held, _, id): &&Entry| held == value && self.rows.get(id).is_some_and(holds);
                Self::scan(entries, lower, upper, order.descending, wanted, keep)
                    .into_iter()
                    .filter(|(held, _, _)| held == value)
                    .map(|(_, place, id)| (place, Value::Null, id))
                    .collect()
            } else {
                // Sorting what one value names costs its holders; walking the order and skipping
                // the rest costs the page times how much rarer than the kind the value is.
                let walk = by_value.is_some_and(|(field, value)| {
                    let holders = self.holders(field, value).max(1);
                    wanted
                        .saturating_mul(self.rows.len())
                        .checked_div(holders)
                        .unwrap_or(0)
                        < holders
                });
                if let Some(ids) = (!walk).then(|| self.narrowed(&query.filters)).flatten() {
                    self.sorted(ids, query, from.as_ref(), wanted)
                } else {
                    let entries = match &order.by {
                        Sort::Position => Some(&self.by_position),
                        Sort::Field(field) => self.by_field.get(field),
                    };
                    let Some(entries) = entries else {
                        return Vec::new();
                    };
                    let keep = |(key, place, id): &&Entry| {
                        self.rows
                            .get(id)
                            .is_some_and(|stored| holds(stored) && sorts_at(stored, key, place))
                    };
                    let bound = from.map_or(Bound::Unbounded, Bound::Excluded);
                    let (lower, upper) = if order.descending {
                        (Bound::Unbounded, bound)
                    } else {
                        (bound, Bound::Unbounded)
                    };
                    Self::scan(entries, lower, upper, order.descending, wanted, keep)
                }
            };
        keys.truncate(wanted);
        keys
    }

    fn count(&self, filters: &[Filter]) -> usize {
        if let [Filter::Is(field, value)] = filters
            && *value != Value::Null
        {
            return self.holders(field, value);
        }
        let holds = |stored: &Indexed| filters.iter().all(|filter| filter.holds(stored));
        match self.narrowed(filters) {
            Some(ids) => ids
                .into_iter()
                .filter(|id| self.rows.get(*id).is_some_and(holds))
                .count(),
            None if filters.is_empty() => self.rows.len(),
            None => self.rows.values().filter(|stored| holds(stored)).count(),
        }
    }
}

/// What the index knows of one document.
#[derive(Clone, Debug, Default)]
struct Held {
    readers: String,
    heads: Vec<ChangeHash>,
    /// The resources it holds, by kind and id.
    resources: BTreeSet<(String, String)>,
}

/// Every kind's resources, as the commits so far left them, kept apart by who may read them.
/// See the crate docs.
#[derive(Default)]
pub struct Index {
    /// Per kind, a table per reader set.
    kinds: HashMap<String, HashMap<String, Table>>,
    docs: HashMap<String, Held>,
}

impl Index {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The heads of `doc` the index reflects; `None` for a document no commit reached it from.
    #[must_use]
    pub fn heads(&self, doc: &str) -> Option<&[ChangeHash]> {
        self.docs.get(doc).map(|held| held.heads.as_slice())
    }

    /// Who may read `doc`.
    #[must_use]
    pub fn readers(&self, doc: &str) -> Option<&str> {
        self.docs.get(doc).map(|held| held.readers.as_str())
    }

    /// The resource `id` of `kind`, whoever may read it: what the store itself reads.
    #[must_use]
    pub fn get(&self, kind: &str, id: &str) -> Option<&Indexed> {
        self.kinds
            .get(kind)?
            .values()
            .find_map(|table| table.rows.get(id))
    }

    /// The kind of the resource `id` held in `doc`.
    #[must_use]
    pub fn kind_of(&self, id: &str, doc: &str) -> Option<String> {
        self.docs
            .get(doc)?
            .resources
            .iter()
            .find(|(_, held)| held == id)
            .map(|(kind, _)| kind.clone())
    }

    /// The resources `doc` holds, by kind and id.
    pub fn held(&self, doc: &str) -> impl Iterator<Item = (&str, &str)> {
        self.docs.get(doc).into_iter().flat_map(|held| {
            held.resources
                .iter()
                .map(|(kind, id)| (kind.as_str(), id.as_str()))
        })
    }

    /// The tables of `kind` whose readers `may_read` admits.
    fn readable<'a>(
        &'a self,
        kind: &str,
        may_read: &'a dyn Fn(&str) -> bool,
    ) -> impl Iterator<Item = &'a Table> + 'a {
        self.kinds
            .get(kind)
            .into_iter()
            .flat_map(|tables| tables.iter())
            .filter(move |(readers, _)| may_read(readers))
            .map(|(_, table)| table)
    }

    /// One page of `query`, of the resources whose readers `may_read` admits: nothing else is
    /// listed, counted or skipped over.
    #[must_use]
    pub fn query(&self, query: &Query, may_read: &dyn Fn(&str) -> bool) -> Page {
        let mut keys: Vec<(Entry, &Table)> = self
            .readable(&query.kind, may_read)
            .flat_map(|table| table.keys(query).into_iter().map(move |key| (key, table)))
            .collect();
        if query.order.descending {
            keys.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        } else {
            keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }
        let more = keys.len() > query.limit;
        keys.truncate(query.limit);
        let next = if more {
            keys.last().map(|((key, place, id), _)| Cursor {
                key: key.clone(),
                place: place.clone(),
                id: id.clone(),
            })
        } else {
            None
        };
        let resources = keys
            .into_iter()
            .filter_map(|((_, _, id), table)| {
                let stored = table.rows.get(&id)?.clone();
                Some((id, stored))
            })
            .collect();
        Page { resources, next }
    }

    /// How many resources of `kind` whose readers `may_read` admits hold to every filter.
    #[must_use]
    pub fn count(&self, kind: &str, filters: &[Filter], may_read: &dyn Fn(&str) -> bool) -> usize {
        self.readable(kind, may_read)
            .map(|table| table.count(filters))
            .fold(0, usize::saturating_add)
    }

    /// Takes out everything stored in `doc`: a document this store no longer keeps.
    pub fn forget(&mut self, doc: &str) {
        let Some(held) = self.docs.remove(doc) else {
            return;
        };
        for (kind, id) in held.resources {
            if let Some(table) = self
                .kinds
                .get_mut(&kind)
                .and_then(|tables| tables.get_mut(&held.readers))
            {
                table.remove(&id);
            }
        }
    }

    fn take_out(&mut self, kind: &str, id: &str, doc: &str) {
        let Some(tables) = self.kinds.get_mut(kind) else {
            return;
        };
        for table in tables.values_mut() {
            // A resource moved to another document stays where that one's commit put it.
            if table.rows.get(id).is_some_and(|stored| stored.doc == doc) {
                table.remove(id);
            }
        }
        if let Some(held) = self.docs.get_mut(doc) {
            held.resources.remove(&(kind.to_owned(), id.to_owned()));
        }
    }
}

impl CommitHook for Index {
    fn committed(&mut self, commit: &Commit<'_>) {
        for change in commit.changes {
            match change {
                Change::Put {
                    kind,
                    id,
                    parent,
                    order,
                    fields,
                } => {
                    // Moved here from another document: out of where it was.
                    let earlier = self.kinds.get(kind).and_then(|tables| {
                        tables.iter().find_map(|(readers, table)| {
                            table
                                .rows
                                .get(id)
                                .map(|held| (readers.clone(), held.doc.clone()))
                        })
                    });
                    if let Some((readers, doc)) = earlier {
                        if readers != commit.readers
                            && let Some(table) = self
                                .kinds
                                .get_mut(kind)
                                .and_then(|tables| tables.get_mut(&readers))
                        {
                            table.remove(id);
                        }
                        if doc != commit.doc
                            && let Some(held) = self.docs.get_mut(&doc)
                        {
                            held.resources.remove(&(kind.clone(), id.clone()));
                        }
                    }
                    let stored = Indexed {
                        doc: commit.doc.to_owned(),
                        parent: parent.clone(),
                        order: order.clone(),
                        fields: fields.clone(),
                    };
                    self.kinds
                        .entry(kind.clone())
                        .or_default()
                        .entry(commit.readers.to_owned())
                        .or_default()
                        .insert(id, stored);
                    self.docs
                        .entry(commit.doc.to_owned())
                        .or_default()
                        .resources
                        .insert((kind.clone(), id.clone()));
                }
                Change::Remove { kind, id } => self.take_out(kind, id, commit.doc),
            }
        }
        let held = self.docs.entry(commit.doc.to_owned()).or_default();
        commit.readers.clone_into(&mut held.readers);
        held.heads = commit.heads.to_vec();
    }
}
