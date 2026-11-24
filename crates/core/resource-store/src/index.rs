//! The index and the hook every commit updates it through.

use std::cell::{Cell as Counter, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Bound;
use std::sync::Arc;

use automerge::ChangeHash;

use crate::query::{Absent, Cursor, Filter, Order, Page, Query, Sort, past};
use crate::ranked::Ranked;
use crate::value::{Cell, Value};

/// The field a resource's parent is indexed under: a filter `Is(PARENT, parent)` lists the
/// resources nested in one resource.
pub const PARENT: &str = "$parent";

/// What the index holds of one resource.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Indexed {
    /// The document the resource is stored in: where a read of what the index does not hold goes.
    /// The store's own: it never leaves the device.
    #[serde(skip)]
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

    /// Where the resource stands in `sort`: what it sorts by, then its place. A field it does
    /// not hold sorts as [`Value::Null`], as a filter reads it; a set is no order.
    fn key(&self, sort: &Sort) -> Option<(Value, Value)> {
        match sort {
            Sort::Position => Some((position(self), Value::Null)),
            Sort::Field(field) if field == PARENT => Some((self.scalar(PARENT), position(self))),
            Sort::Field(field) => {
                let value = match self.fields.get(field) {
                    Some(Cell::Set(_)) => return None,
                    Some(cell) => cell.one()?.clone(),
                    None => Value::Null,
                };
                Some((value, position(self)))
            }
        }
    }

    /// Whether the resource holds nothing in the scalar field `field`: it is listed under
    /// [`Value::Null`] there.
    fn lacks(&self, field: &str) -> bool {
        if field == PARENT {
            return self.parent.is_none();
        }
        !self.fields.contains_key(field)
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

/// Where a resource stands in a composite order: what it holds of each of its fields, its place,
/// its id.
#[derive(Clone, Debug)]
struct Placed {
    levels: Vec<Value>,
    place: Value,
    id: String,
}

impl Placed {
    fn cmp_in(&self, order: &Order, other: &Self) -> std::cmp::Ordering {
        let directions =
            std::iter::once(order.descending).chain(order.then.iter().map(|(_, down)| *down));
        for ((a, b), descending) in self.levels.iter().zip(&other.levels).zip(directions) {
            let ordering = order.compare(a, b, descending);
            if ordering.is_ne() {
                return ordering;
            }
        }
        let rest = (&self.place, &self.id).cmp(&(&other.place, &other.id));
        if order.descending {
            rest.reverse()
        } else {
            rest
        }
    }
}

/// Where a resource stands in a listing the index keeps ([`Kept`]): ordered as the listing's
/// order sorts it, so the set of them is the listing.
#[derive(Clone, Debug)]
struct KeptKey {
    placed: Placed,
    order: Arc<Order>,
}

impl Ord for KeptKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.placed.cmp_in(&self.order, &other.placed)
    }
}

impl PartialOrd for KeptKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for KeptKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for KeptKey {}

/// A listing no span of the index's orders holds (several filters, or an order that sorts
/// further than one field), kept as writes land: each write moves the one resource it changed in
/// or out of it. Its length, a resource's position in it and the page from any place in it are
/// read off it, where each was a walk of the listing on every ask, and a write that moved one
/// task cost a walk of every task.
#[derive(Clone, Debug)]
struct Kept {
    filters: Vec<Filter>,
    order: Arc<Order>,
    keys: Ranked<KeptKey>,
    /// When it was last asked for, by the table's count of asks.
    asked: u64,
}

impl Kept {
    fn key(&self, id: &str, stored: &Indexed) -> Option<KeptKey> {
        if !self.filters.iter().all(|filter| filter.holds(stored)) {
            return None;
        }
        Some(KeptKey {
            placed: Table::placed(&self.order, id, stored)?,
            order: Arc::clone(&self.order),
        })
    }
}

/// The most listings a table keeps. Each costs a key per resource it lists and a little work on
/// every write of one, so those not asked for longest go first: a listing a window shows is asked
/// for whenever it is read.
const KEPT: usize = 32;

/// The fewest resources a listing reads before the index keeps it: a listing its first filter
/// narrows to fewer (a task's subtasks, one board's lists) is read in less than its upkeep on
/// every write would cost, and keeping one per row a window shows would push out the listings
/// worth keeping.
const KEEP_FROM: usize = 512;

/// The resources of one kind, wherever they are stored, and their orders.
#[derive(Clone, Default)]
struct Table {
    rows: BTreeMap<String, Indexed>,
    /// Every resource by its place.
    by_position: Ranked<Entry>,
    /// Per scalar field, every resource holding it by its value, then by its place; per set
    /// field, every member. A filter on one value pages in the kind's order from here alone.
    by_field: HashMap<String, Ranked<Entry>>,
    /// The fields some resource holds a set in: their entries list members, not one value per
    /// resource, so they are no order and no span of one.
    sets: HashSet<String>,
    /// Per field, how many resources hold each value: what a query reads to choose its path, and
    /// a count of one value answers from.
    counts: HashMap<String, HashMap<Value, usize>>,
    /// The listings it keeps, by their filters and order ([`Kept`]).
    kept: RefCell<HashMap<String, Kept>>,
    /// How many times a kept listing was asked for: the clock [`Kept::asked`] reads.
    asks: Counter<u64>,
}

impl Table {
    /// Whether a listing of `query` can be kept: every field it sorts by holds one value per
    /// resource, so a resource stands in one place in it.
    fn keepable(&self, query: &Query) -> bool {
        let scalar = |field: &str| !self.sets.contains(field);
        let by = match &query.order.by {
            Sort::Position => true,
            Sort::Field(field) => scalar(field),
        };
        by && query.order.then.iter().all(|(field, _)| scalar(field))
    }

    /// Read `read` off the kept listing of `query`, keeping it from now on when it is not yet;
    /// `None` when it cannot be kept ([`Self::keepable`]).
    fn with_kept<R>(&self, query: &Query, read: impl FnOnce(&Kept) -> R) -> Option<R> {
        if !self.keepable(query) {
            return None;
        }
        let name = format!("{:?}|{:?}", query.order, query.filters);
        let now = self.asks.get().saturating_add(1);
        self.asks.set(now);
        let mut kept = self.kept.borrow_mut();
        if !kept.contains_key(&name) {
            let ids: Vec<&str> = match self.narrowed(&query.filters) {
                Some(ids) => ids,
                None => self.rows.keys().map(String::as_str).collect(),
            };
            if ids.len() < KEEP_FROM {
                return None;
            }
            if kept.len() >= KEPT
                && let Some(oldest) = kept
                    .iter()
                    .min_by_key(|(_, listing)| listing.asked)
                    .map(|(name, _)| name.clone())
            {
                kept.remove(&oldest);
            }
            let mut listing = Kept {
                filters: query.filters.clone(),
                order: Arc::new(query.order.clone()),
                keys: Ranked::default(),
                asked: now,
            };
            for id in ids {
                if let Some(key) = self.rows.get(id).and_then(|stored| listing.key(id, stored)) {
                    listing.keys.insert(key);
                }
            }
            kept.insert(name.clone(), listing);
        }
        let listing = kept.get_mut(&name)?;
        listing.asked = now;
        Some(read(listing))
    }

    fn insert(&mut self, id: &str, stored: Indexed) {
        self.unlink(id);
        let place = position(&stored);
        self.by_position
            .insert((place.clone(), Value::Null, id.to_owned()));
        for (field, cell) in &stored.fields {
            if matches!(cell, Cell::Set(_)) && !self.sets.contains(field) {
                self.sets.insert(field.clone());
            }
        }
        // A field no resource held until now: those that do not hold it are listed under
        // nothing there, so an order by it or a filter for nothing in it reads them all.
        let first_held: Vec<String> = stored
            .indexed()
            .map(|(field, _)| field)
            .filter(|field| !self.by_field.contains_key(*field) && !self.sets.contains(*field))
            .map(str::to_owned)
            .collect();
        for field in first_held {
            let mut entries = Ranked::default();
            for (other, row) in &self.rows {
                if other != id && row.lacks(&field) {
                    entries.insert((Value::Null, position(row), other.clone()));
                }
            }
            self.by_field.insert(field, entries);
        }
        for field in self.lacked(&stored) {
            if let Some(entries) = self.by_field.get_mut(&field) {
                entries.insert((Value::Null, place.clone(), id.to_owned()));
            }
        }
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
        for listing in self.kept.get_mut().values_mut() {
            if let Some(key) = listing.key(id, &stored) {
                listing.keys.insert(key);
            }
        }
        self.rows.insert(id.to_owned(), stored);
    }

    /// Takes `id` out of every order, keeping its row; whether it had one.
    fn unlink(&mut self, id: &str) -> bool {
        let Some(stored) = self.rows.get(id) else {
            return false;
        };
        for listing in self.kept.get_mut().values_mut() {
            if let Some(key) = listing.key(id, stored) {
                listing.keys.remove(&key);
            }
        }
        let place = position(stored);
        let lacked = self.lacked(stored);
        let mut key = (place.clone(), Value::Null, id.to_owned());
        self.by_position.remove(&key);
        key.1 = place;
        for field in lacked {
            if let Some(entries) = self.by_field.get_mut(&field) {
                key.0 = Value::Null;
                entries.remove(&key);
            }
        }
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

    /// The scalar fields other resources of the table hold and `stored` does not.
    fn lacked(&self, stored: &Indexed) -> Vec<String> {
        self.by_field
            .keys()
            .filter(|field| !self.sets.contains(*field) && stored.lacks(field))
            .cloned()
            .collect()
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
                .range(
                    Bound::Included(&(value.clone(), Value::Null, String::new())),
                    Bound::Unbounded,
                )
                .take_while(move |(held, _, _)| held == value)
                .map(|(_, _, id)| id.as_str()),
        )
    }

    /// How many resources [`Self::holding`] names, from the ranks the index keeps: none walked.
    fn holding_len(&self, field: &str, value: &Value) -> usize {
        let Some(entries) = self.by_field.get(field) else {
            return 0;
        };
        let from = entries.partition_point(|(held, _, _)| held < value);
        let to = entries.partition_point(|(held, _, _)| held <= value);
        to.saturating_sub(from)
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
                .range(start.as_ref(), Bound::Unbounded)
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
        // Of the values asked for, the one fewest resources hold: a task's subtasks are found
        // by their parent, not by walking every task of the list they are in.
        let by_value = filters
            .iter()
            .filter_map(|filter| match filter {
                Filter::Is(field, value) | Filter::Has(field, value) => {
                    Some((self.holding_len(field, value), field, value))
                }
                _ => None,
            })
            .min_by_key(|(len, _, _)| *len)
            .map(|(_, field, value)| self.holding(field, value).map(Iterator::collect));
        let by_range = || {
            filters.iter().find_map(|filter| match filter {
                Filter::Within { field, from, to } => {
                    Some(self.ranging(field, from, to).map(Iterator::collect))
                }
                _ => None,
            })
        };
        // Any of several values, each of a field the index keeps by value: the resources holding
        // one of them, as what a resource is mentioned by under each of its names.
        let by_any = || {
            filters.iter().find_map(|filter| match filter {
                Filter::Any(any) if !any.is_empty() => any
                    .iter()
                    .map(|alternative| match alternative {
                        // A resource that holds no such field may have no entry of it.
                        Filter::Is(field, value) | Filter::Has(field, value)
                            if *value != Value::Null =>
                        {
                            Some(self.holding(field, value).into_iter().flatten())
                        }
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|holders| Some(holders.into_iter().flatten().collect())),
                _ => None,
            })
        };
        // A filter on a field no resource holds names none.
        by_value
            .or_else(by_any)
            .or_else(by_range)
            .map(|ids: Option<Vec<&str>>| {
                let mut ids = ids.unwrap_or_default();
                ids.sort_unstable();
                ids.dedup();
                ids
            })
    }

    /// The entries of `entries` from `from` on (after it), in `descending` order or not, that
    /// `keep` keeps, `wanted` at most.
    fn scan(
        entries: &Ranked<Entry>,
        lower: Bound<&Entry>,
        upper: Bound<&Entry>,
        descending: bool,
        wanted: usize,
        keep: impl FnMut(&&Entry) -> bool,
    ) -> Vec<Entry> {
        let range = entries.range(lower, upper);
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
                Self::scan(
                    entries,
                    lower.as_ref(),
                    upper.as_ref(),
                    order.descending,
                    wanted,
                    keep,
                )
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
                    Self::scan(
                        entries,
                        lower.as_ref(),
                        upper.as_ref(),
                        order.descending,
                        wanted,
                        keep,
                    )
                }
            };
        keys.truncate(wanted);
        keys
    }

    /// Where `stored` stands in the composite `order`.
    fn placed(order: &Order, id: &str, stored: &Indexed) -> Option<Placed> {
        let (key, place) = stored.key(&order.by)?;
        let mut levels = Vec::with_capacity(order.then.len().saturating_add(1));
        levels.push(key);
        levels.extend(order.then.iter().map(|(field, _)| stored.scalar(field)));
        Some(Placed {
            levels,
            place,
            id: id.to_owned(),
        })
    }

    /// The first `query.limit + 1` resources this table shows of `query`, whose order sorts
    /// further than one of the index's orders does: the index's order of its first field is
    /// walked, from the cursor's value on, and each stretch of resources alike in it is sorted by
    /// the rest. A page costs what it shows, what its filters skip and the stretch it ends in.
    fn composite_keys(&self, query: &Query) -> Vec<Placed> {
        let order = &query.order;
        let wanted = query.limit.saturating_add(1);
        let from: Option<Placed> = query.after.as_ref().map(|cursor| Placed {
            levels: std::iter::once(cursor.key.clone())
                .chain(cursor.rest.iter().cloned())
                .collect(),
            place: cursor.place.clone(),
            id: cursor.id.clone(),
        });
        let entries = match &order.by {
            Sort::Position => &self.by_position,
            Sort::Field(field) => match self.by_field.get(field) {
                Some(entries) if !self.sets.contains(field) => entries,
                _ => return Vec::new(),
            },
        };
        let descending = order.descending;
        let nulls_first = match order.absent {
            Some(Absent::First) => true,
            Some(Absent::Last) => false,
            None => !descending,
        };
        let nulls = entries.partition_point(|(held, _, _)| *held == Value::Null);
        let none = (0, nulls);
        let some = (nulls, entries.len());
        let mut spans = if nulls_first {
            vec![(none, true), (some, false)]
        } else {
            vec![(some, false), (none, true)]
        };
        // From the cursor's value on: the spans before it are passed, and its own starts there.
        if let Some(from) = &from
            && let Some(first) = from.levels.first()
        {
            let null = *first == Value::Null;
            while spans.first().is_some_and(|(_, held)| *held != null) {
                spans.remove(0);
            }
            if let Some(((lower, upper), _)) = spans.first_mut() {
                if descending {
                    *upper = (*upper).min(entries.partition_point(|(held, _, _)| held <= first));
                } else {
                    *lower = (*lower).max(entries.partition_point(|(held, _, _)| held < first));
                }
            }
        }
        let holds = |stored: &Indexed| query.filters.iter().all(|filter| filter.holds(stored));
        let mut found: Vec<Placed> = Vec::new();
        'spans: for ((lower, upper), _) in spans {
            let span = entries.between(lower, upper.max(lower));
            let span: Box<dyn Iterator<Item = &Entry>> = if descending {
                Box::new(span.rev())
            } else {
                Box::new(span)
            };
            for (key, place, id) in span {
                let Some(stored) = self.rows.get(id) else {
                    continue;
                };
                if !holds(stored) {
                    continue;
                }
                let Some(placed) = Self::placed(order, id, stored) else {
                    continue;
                };
                // An entry listed for something other than the order's field.
                if !matches!(order.by, Sort::Position)
                    && (placed.levels.first() != Some(key) || placed.place != *place)
                {
                    continue;
                }
                if from
                    .as_ref()
                    .is_some_and(|from| placed.cmp_in(order, from) != std::cmp::Ordering::Greater)
                {
                    continue;
                }
                if found.len() >= wanted
                    && found.last().map(|last| last.levels.first()) != Some(placed.levels.first())
                {
                    break 'spans;
                }
                found.push(placed);
            }
        }
        found.sort_by(|a, b| a.cmp_in(order, b));
        found.truncate(wanted);
        found
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

    /// The values the resources holding to `filters` hold in `field`, each once: each member of
    /// a set. Where every resource of the table holds to the filters (the items of a collection,
    /// all under its one holder), the counts the index keeps per value answer without reading a
    /// resource; otherwise the resources the filters narrow to are read.
    fn values(&self, filters: &[Filter], field: &str) -> BTreeSet<Value> {
        let every = filters.iter().all(|filter| match filter {
            Filter::Is(held, value) | Filter::Has(held, value) => {
                *value != Value::Null && self.holders(held, value) == self.rows.len()
            }
            _ => false,
        });
        if every {
            return self
                .counts
                .get(field)
                .into_iter()
                .flat_map(HashMap::keys)
                .filter(|value| **value != Value::Null)
                .cloned()
                .collect();
        }
        let holds = |stored: &&Indexed| filters.iter().all(|filter| filter.holds(stored));
        let held = |stored: &Indexed| -> Vec<Value> {
            match stored.fields.get(field) {
                Some(Cell::One(value)) if *value != Value::Null => vec![value.clone()],
                Some(Cell::Set(members)) => members.iter().cloned().collect(),
                _ => Vec::new(),
            }
        };
        match self.narrowed(filters) {
            Some(ids) => ids
                .into_iter()
                .filter_map(|id| self.rows.get(id))
                .filter(holds)
                .flat_map(held)
                .collect(),
            None => self.rows.values().filter(holds).flat_map(held).collect(),
        }
    }

    /// Where the whole listing of `query` lies in one of this table's orders, when it is one
    /// span of it: the kind in an order, one value's resources in the kind's order, or a range
    /// of a field in that field's order. `None` for any other query.
    fn window(&self, query: &Query) -> Option<Window<'_>> {
        if query.order.is_composite() {
            return None;
        }
        let scalar = |field: &str| !self.sets.contains(field);
        let span = |entries: &Ranked<Entry>,
                    lower: &dyn Fn(&Value) -> bool,
                    upper: &dyn Fn(&Value) -> bool| {
            (
                entries.partition_point(|(held, _, _)| lower(held)),
                entries.partition_point(|(held, _, _)| upper(held)),
            )
        };
        let empty = || Window {
            entries: &EMPTY,
            from: 0,
            to: 0,
            value: None,
        };
        match (&query.order.by, query.filters.as_slice()) {
            (Sort::Position, []) => Some(Window {
                entries: &self.by_position,
                from: 0,
                to: self.by_position.len(),
                value: None,
            }),
            (Sort::Field(field), []) if scalar(field) => Some(
                self.by_field
                    .get(field)
                    .map_or_else(empty, |entries| Window {
                        entries,
                        from: 0,
                        to: entries.len(),
                        value: None,
                    }),
            ),
            (Sort::Position, [Filter::Is(field, value) | Filter::Has(field, value)])
                if *value != Value::Null
                    && (scalar(field)
                        || matches!(query.filters.first(), Some(Filter::Has(..)))) =>
            {
                Some(self.by_field.get(field).map_or_else(empty, |entries| {
                    let (from, to) = span(entries, &|held| held < value, &|held| held <= value);
                    Window {
                        entries,
                        from,
                        to,
                        value: Some(value.clone()),
                    }
                }))
            }
            (Sort::Field(sorted), [Filter::Within { field, from, to }])
                if sorted == field && scalar(field) =>
            {
                Some(self.by_field.get(field).map_or_else(empty, |entries| {
                    let (from, to) = span(
                        entries,
                        &|held| match from {
                            Bound::Included(from) => held < from,
                            Bound::Excluded(from) => held <= from,
                            Bound::Unbounded => false,
                        },
                        &|held| !past(held, to),
                    );
                    Window {
                        entries,
                        from,
                        to: to.max(from),
                        value: None,
                    }
                }))
            }
            _ => None,
        }
    }
}

static EMPTY: Ranked<Entry> = Ranked::new();

/// The page of the first `limit` of `keys`, which are in the page's order, and where the next
/// one starts when there are more.
fn page_of(mut keys: Vec<(Entry, &Table)>, limit: usize) -> Page {
    let more = keys.len() > limit;
    keys.truncate(limit);
    let next = if more {
        keys.last().map(|((key, place, id), _)| Cursor {
            key: key.clone(),
            place: place.clone(),
            id: id.clone(),
            rest: Vec::new(),
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

/// The span of one table's order a query lists. See [`Table::window`].
struct Window<'a> {
    entries: &'a Ranked<Entry>,
    from: usize,
    to: usize,
    /// For one value's resources in the kind's order, the value: the span's entries lead with
    /// it, and sort by what follows.
    value: Option<Value>,
}

impl Window<'_> {
    const fn len(&self) -> usize {
        self.to.saturating_sub(self.from)
    }

    /// How many of the span's entries sort before `entry`.
    fn before(&self, entry: &Entry) -> usize {
        self.entries
            .rank(entry)
            .clamp(self.from, self.to)
            .saturating_sub(self.from)
    }

    /// Where `stored` stands in this span's entries, given the order's key of it.
    fn entry(&self, id: &str, (key, place): (Value, Value)) -> Entry {
        match &self.value {
            Some(value) => (value.clone(), key, id.to_owned()),
            None => (key, place, id.to_owned()),
        }
    }

    /// The order's key of an entry of this span.
    fn key(&self, (held, place, id): &Entry) -> Entry {
        match self.value {
            Some(_) => (place.clone(), Value::Null, id.clone()),
            None => (held.clone(), place.clone(), id.clone()),
        }
    }
}

/// Where to cut each span so that `k` of their entries, together, lie before the cuts and the
/// rest after: each cut is how many of its span's entries come first. Each step halves the
/// largest span left, so this reads a few positions per span and step, never the entries.
fn cuts(windows: &[Window<'_>], mut k: usize) -> Vec<usize> {
    let mut lower = vec![0_usize; windows.len()];
    let mut upper: Vec<usize> = windows.iter().map(Window::len).collect();
    loop {
        let left = lower
            .iter()
            .zip(&upper)
            .map(|(lower, upper)| upper.saturating_sub(*lower))
            .fold(0, usize::saturating_add);
        if k == 0 || left == 0 {
            return lower;
        }
        if k >= left {
            return upper;
        }
        // The middle entry of the largest span left is the pivot.
        let Some((widest, (low, high))) = lower
            .iter()
            .zip(&upper)
            .enumerate()
            .max_by_key(|(_, (lower, upper))| upper.saturating_sub(**lower))
        else {
            return lower;
        };
        let middle = low.saturating_add(high.saturating_sub(*low) / 2);
        let Some(pivot) = windows
            .get(widest)
            .and_then(|window| window.entries.get(window.from.saturating_add(middle)))
        else {
            return lower;
        };
        let below: Vec<usize> = windows
            .iter()
            .zip(lower.iter().zip(&upper))
            .map(|(window, (lower, upper))| window.before(pivot).clamp(*lower, *upper))
            .collect();
        let less = below
            .iter()
            .zip(&lower)
            .map(|(below, lower)| below.saturating_sub(*lower))
            .fold(0, usize::saturating_add);
        match k.cmp(&less) {
            std::cmp::Ordering::Less => upper = below,
            std::cmp::Ordering::Equal => return below,
            std::cmp::Ordering::Greater => {
                k = k.saturating_sub(less.saturating_add(1));
                lower = below;
                if let Some(at) = lower.get_mut(widest) {
                    *at = middle.saturating_add(1);
                }
            }
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
#[derive(Clone, Default)]
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

    /// The rows a draft command can look up by kind and id.
    pub(crate) fn selected(&self, resources: &BTreeSet<(String, String)>) -> Self {
        let mut index = Self::new();
        for (kind, id) in resources {
            let Some(row) = self.get(kind, id) else {
                continue;
            };
            let Some(held) = self.docs.get(&row.doc) else {
                continue;
            };
            index.committed(&Commit {
                doc: &row.doc,
                readers: &held.readers,
                heads: &held.heads,
                changes: &[Change::Put {
                    kind: kind.clone(),
                    id: id.clone(),
                    parent: row.parent.clone(),
                    order: row.order.clone(),
                    fields: row.fields.clone(),
                }],
            });
        }
        index
    }

    /// The heads of `doc` the index reflects; `None` for a document no commit reached it from.
    #[must_use]
    pub fn heads(&self, doc: &str) -> Option<&[ChangeHash]> {
        self.docs.get(doc).map(|held| held.heads.as_slice())
    }

    /// Every document a commit reached the index from.
    pub fn documents(&self) -> impl Iterator<Item = &str> {
        self.docs.keys().map(String::as_str)
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

    /// The id of every resource of `kind` the index holds, whoever may read it.
    pub(crate) fn ids(&self, kind: &str) -> Vec<String> {
        self.kinds
            .get(kind)
            .into_iter()
            .flat_map(HashMap::values)
            .flat_map(|table| table.rows.keys().cloned())
            .collect()
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

    /// The kind of the resource `id`, wherever it is held.
    #[must_use]
    pub fn kind_anywhere(&self, id: &str) -> Option<String> {
        self.kinds
            .iter()
            .find(|(_, tables)| tables.values().any(|table| table.rows.contains_key(id)))
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
        if self.windows(query, may_read).is_none()
            && let Some(page) = self.kept_page(query, None, may_read)
        {
            return page;
        }
        if query.order.is_composite() {
            return self.composite(query, may_read);
        }
        let mut keys: Vec<(Entry, &Table)> = self
            .readable(&query.kind, may_read)
            .flat_map(|table| table.keys(query).into_iter().map(move |key| (key, table)))
            .collect();
        if query.order.descending {
            keys.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        } else {
            keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }
        page_of(keys, query.limit)
    }

    /// A page of `query` whose order sorts further than one of the index's orders.
    fn composite(&self, query: &Query, may_read: &dyn Fn(&str) -> bool) -> Page {
        let order = &query.order;
        let mut keys: Vec<(Placed, &Table)> = self
            .readable(&query.kind, may_read)
            .flat_map(|table| {
                table
                    .composite_keys(query)
                    .into_iter()
                    .map(move |key| (key, table))
            })
            .collect();
        keys.sort_by(|a, b| a.0.cmp_in(order, &b.0));
        let more = keys.len() > query.limit;
        keys.truncate(query.limit);
        let next = more.then(|| keys.last()).flatten().map(|(placed, _)| {
            let mut levels = placed.levels.iter().cloned();
            Cursor {
                key: levels.next().unwrap_or(Value::Null),
                rest: levels.collect(),
                place: placed.place.clone(),
                id: placed.id.clone(),
            }
        });
        let resources = keys
            .into_iter()
            .filter_map(|(placed, table)| {
                let stored = table.rows.get(&placed.id)?.clone();
                Some((placed.id, stored))
            })
            .collect();
        Page { resources, next }
    }

    /// How many resources of `kind` whose readers `may_read` admits hold to every filter.
    #[must_use]
    pub fn count(&self, kind: &str, filters: &[Filter], may_read: &dyn Fn(&str) -> bool) -> usize {
        // Several filters, or one the index keeps no count of, are a listing kept as writes land
        // (in the kind's order): counted off it, where each count walked every resource of the
        // kind the first filter narrowed to.
        let counted_as_held = match filters {
            [] => true,
            [Filter::Is(_, value)] => *value != Value::Null,
            _ => false,
        };
        if !counted_as_held {
            let mut listing = Query::of(kind, 0);
            listing.filters = filters.to_vec();
            if let Some(lens) = self.kept_tables(&listing, may_read, |kept| kept.keys.len()) {
                return lens.into_iter().fold(0, usize::saturating_add);
            }
        }
        self.readable(kind, may_read)
            .map(|table| table.count(filters))
            .fold(0, usize::saturating_add)
    }

    /// The values the resources of `kind` whose readers `may_read` admits and that hold to
    /// `filters` hold in `field`, each once, in order: the labels a workspace's tasks are sorted
    /// by, read from the counts the index keeps rather than from every task.
    #[must_use]
    pub fn values(
        &self,
        kind: &str,
        filters: &[Filter],
        field: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Vec<Value> {
        let mut values = BTreeSet::new();
        for table in self.readable(kind, may_read) {
            values.extend(table.values(filters, field));
        }
        values.into_iter().collect()
    }

    /// The span of each readable table `query` lists, when it is one span in every one of them.
    fn windows<'a>(
        &'a self,
        query: &Query,
        may_read: &'a dyn Fn(&str) -> bool,
    ) -> Option<Vec<Window<'a>>> {
        self.readable(&query.kind, may_read)
            .map(|table| table.window(query))
            .collect()
    }

    /// How many resources `query` lists, of those whose readers `may_read` admits.
    #[must_use]
    pub fn len(&self, query: &Query, may_read: &dyn Fn(&str) -> bool) -> usize {
        match self.windows(query, may_read) {
            Some(windows) => windows
                .iter()
                .map(Window::len)
                .fold(0, usize::saturating_add),
            None => self
                .kept_tables(query, may_read, |listing| listing.keys.len())
                .map_or_else(
                    || self.count(&query.kind, &query.filters, may_read),
                    |lens| lens.into_iter().fold(0, usize::saturating_add),
                ),
        }
    }

    /// The page of `query` that starts at `position` in its listing, ignoring its cursor: what
    /// a window scrolled to that row shows. A query listing the kind in an order, one value's
    /// resources in the kind's order, or a range of a field in that order finds the position
    /// from the counts it keeps; any other walks the listing up to it.
    #[must_use]
    pub fn at(&self, query: &Query, position: usize, may_read: &dyn Fn(&str) -> bool) -> Page {
        let Some(windows) = self.windows(query, may_read) else {
            if let Some(page) = self.kept_page(query, Some(position), may_read) {
                return page;
            }
            let mut walked = query.clone();
            walked.after = None;
            walked.limit = position.saturating_add(query.limit);
            let mut page = self.query(&walked, may_read);
            page.resources.drain(..position.min(page.resources.len()));
            return page;
        };
        let total = windows
            .iter()
            .map(Window::len)
            .fold(0, usize::saturating_add);
        let descending = query.order.descending;
        // In the ascending order the spans keep, the first shown is at `start`, and a page
        // turned around walks down from the one before the cut.
        let start = if descending {
            total.saturating_sub(position)
        } else {
            position
        };
        let cut = cuts(&windows, start);
        let mut heads: Vec<_> = windows
            .iter()
            .zip(&cut)
            .map(|(window, at)| {
                let at = window.from.saturating_add(*at);
                let entries: Box<dyn DoubleEndedIterator<Item = &Entry>> = if descending {
                    Box::new(window.entries.between(window.from, at).rev())
                } else {
                    Box::new(window.entries.between(at, window.to))
                };
                entries.map(|entry| window.key(entry)).peekable()
            })
            .collect();
        let tables: Vec<&Table> = self.readable(&query.kind, may_read).collect();
        let mut keys: Vec<(Entry, &Table)> = Vec::new();
        while keys.len() <= query.limit {
            let next = heads
                .iter_mut()
                .enumerate()
                .filter_map(|(at, head)| head.peek().map(|key| (at, key.clone())))
                .reduce(|best, other| {
                    let better = if descending {
                        other.1 > best.1
                    } else {
                        other.1 < best.1
                    };
                    if better { other } else { best }
                });
            let Some((at, key)) = next else {
                break;
            };
            if let Some(head) = heads.get_mut(at) {
                head.next();
            }
            if let Some(table) = tables.get(at) {
                keys.push((key, table));
            }
        }
        page_of(keys, query.limit)
    }

    /// Where the resource `id` stands in the listing of `query`; `None` when it lists no such
    /// resource or one `may_read` does not admit.
    #[must_use]
    pub fn position(
        &self,
        query: &Query,
        id: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        let stored = self
            .readable(&query.kind, may_read)
            .find_map(|table| table.rows.get(id))?;
        if !query.filters.iter().all(|filter| filter.holds(stored)) {
            return None;
        }
        let key = stored.key(&query.order.by)?;
        let Some(windows) = self.windows(query, may_read) else {
            return self
                .kept_tables(query, may_read, |listing| {
                    listing
                        .key(id, stored)
                        .map_or(0, |key| listing.keys.rank(&key))
                })
                .map_or_else(
                    || self.walked_position(query, id, may_read),
                    |ranks| Some(ranks.into_iter().fold(0, usize::saturating_add)),
                );
        };
        let total = windows
            .iter()
            .map(Window::len)
            .fold(0, usize::saturating_add);
        let before = windows
            .iter()
            .map(|window| window.before(&window.entry(id, key.clone())))
            .fold(0, usize::saturating_add);
        Some(if query.order.descending {
            total.saturating_sub(before).saturating_sub(1)
        } else {
            before
        })
    }

    /// What `read` reads off the kept listing of `query` ([`Kept`]) in each table `may_read`
    /// admits; `None` when one of them cannot keep it.
    fn kept_tables<R>(
        &self,
        query: &Query,
        may_read: &dyn Fn(&str) -> bool,
        read: impl Fn(&Kept) -> R,
    ) -> Option<Vec<R>> {
        self.readable(&query.kind, may_read)
            .map(|table| table.with_kept(query, &read))
            .collect()
    }

    /// The page of `query` off its kept listings ([`Kept`]): after its cursor, or from `at` when
    /// a position is given. A position spanning several tables is no one place in each, so it is
    /// read off kept listings only where one table lists the kind; `None` where it cannot be read
    /// so, or a table cannot keep the listing.
    fn kept_page(
        &self,
        query: &Query,
        at: Option<usize>,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Option<Page> {
        let tables: Vec<&Table> = self.readable(&query.kind, may_read).collect();
        if at.is_some() && tables.len() > 1 {
            return None;
        }
        let order = Arc::new(query.order.clone());
        let after = match (at, &query.after) {
            (None, Some(cursor)) => Some(KeptKey {
                placed: Placed {
                    levels: std::iter::once(cursor.key.clone())
                        .chain(cursor.rest.iter().cloned())
                        .collect(),
                    place: cursor.place.clone(),
                    id: cursor.id.clone(),
                },
                order: Arc::clone(&order),
            }),
            _ => None,
        };
        let wanted = query.limit.saturating_add(1);
        let mut keys: Vec<(Placed, &Table)> = Vec::new();
        for table in tables {
            let taken = table.with_kept(query, |listing| {
                let from = match (&after, at) {
                    (Some(after), _) => listing.keys.partition_point(|key| key <= after),
                    (None, Some(at)) => at,
                    (None, None) => 0,
                };
                listing
                    .keys
                    .between(from, from.saturating_add(wanted))
                    .map(|key| key.placed.clone())
                    .collect::<Vec<_>>()
            })?;
            keys.extend(taken.into_iter().map(|placed| (placed, table)));
        }
        keys.sort_by(|a, b| a.0.cmp_in(&order, &b.0));
        let more = keys.len() > query.limit;
        keys.truncate(query.limit);
        let next = more.then(|| keys.last()).flatten().map(|(placed, _)| {
            let mut levels = placed.levels.iter().cloned();
            Cursor {
                key: levels.next().unwrap_or(Value::Null),
                rest: levels.collect(),
                place: placed.place.clone(),
                id: placed.id.clone(),
            }
        });
        let resources = keys
            .into_iter()
            .filter_map(|(placed, table)| {
                let stored = table.rows.get(&placed.id)?.clone();
                Some((placed.id, stored))
            })
            .collect();
        Some(Page { resources, next })
    }

    /// Where `id` stands in the listing of `query`, which no one span of the index holds: the
    /// listing is walked in its order as a page of it is, keeping the keys it sorts by and no
    /// copy of a resource, and the item found among them. A position asked for after each write
    /// copied every resource of the listing before.
    fn walked_position(
        &self,
        query: &Query,
        id: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        let mut walked = query.clone();
        walked.after = None;
        walked.limit = usize::MAX;
        let tables = self.readable(&query.kind, may_read);
        if query.order.is_composite() {
            let order = &query.order;
            let mut keys: Vec<Placed> = tables
                .flat_map(|table| table.composite_keys(&walked))
                .collect();
            keys.sort_by(|a, b| a.cmp_in(order, b));
            return keys.iter().position(|placed| placed.id == id);
        }
        let mut keys: Vec<Entry> = tables.flat_map(|table| table.keys(&walked)).collect();
        if query.order.descending {
            keys.sort_unstable_by(|a, b| b.cmp(a));
        } else {
            keys.sort_unstable();
        }
        keys.iter().position(|(_, _, listed)| listed == id)
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
                && table.rows.get(&id).is_some_and(|stored| stored.doc == doc)
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
