//! What a read of a chain's listing asks of each item, and a page of the
//! listing as it fills: the fields a row carries of its item, and what an
//! item's fields equal for its row to be answered (the read's `fields` and
//! `where`). Compiled once, whatever resource the parts are.

use std::any::Any;
use std::collections::BTreeMap;

use super::{ChainRead, PartType, read_cursor};

/// What a read's `where` says: field names with the values they equal.
pub(super) type Filter = serde_json::Map<String, serde_json::Value>;

/// The `fields` and the `where` a read's arguments `args` carry.
pub(super) fn asked(args: &serde_json::Value) -> Result<(Option<Vec<String>>, Filter), String> {
    let fields = match args.get("fields") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Array(names)) => Some(
            names
                .iter()
                .map(|name| name.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "`fields` is a list of field names".to_owned())?,
        ),
        Some(_) => return Err("`fields` is a list of field names".into()),
    };
    let filter = match args.get("where") {
        None | Some(serde_json::Value::Null) => Filter::new(),
        Some(serde_json::Value::Object(filter)) => filter.clone(),
        Some(_) => {
            return Err("`where` is an object: field names with the values they equal".into());
        }
    };
    Ok((fields, filter))
}

/// What a read of a chain's listing asks of each item: the fields its rows
/// carry, and what an item's fields equal for its row to be answered.
pub(super) struct Listing {
    /// The fields read of every item: those its row shows, then those only
    /// compared.
    wanted: Vec<String>,
    /// How many of `wanted` a row shows.
    shown: usize,
    /// Each condition: the place in `wanted` of the field compared, the
    /// names reaching into it when it is a record, and what it equals.
    conditions: Vec<(usize, Vec<String>, serde_json::Value)>,
}

impl Listing {
    /// What `read` asks of the items of a chain of `part`s, its `where` put
    /// into the items' own fields first, where the read is answered
    /// (`prepare`, the kind's `Chained::prepare_read`).
    pub(super) fn of(
        read: &ChainRead,
        table: &'static tilo_resource::table::ModelTable,
        part: &PartType,
        parts: &[(u64, &dyn Any)],
        prepare: &dyn Fn(&mut Filter) -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut filter = read.filter.clone();
        prepare(&mut filter)?;
        let listed = || part.listed.iter().map(|name| (*name).to_owned()).collect();
        let asked: Vec<String> = read.fields.clone().unwrap_or_else(listed);
        // The fields the items have, read only when something is asked of
        // them. What the kind lists is what its items have of it; what the
        // caller names is refused when they have no such field.
        let known = if asked.is_empty() && filter.is_empty() {
            Vec::new()
        } else {
            item_field_names(table, part.kind, parts)
        };
        let asked: Vec<String> = if read.fields.is_some() {
            asked
        } else {
            asked
                .into_iter()
                .filter(|name| known.is_empty() || known.contains(name))
                .collect()
        };
        let mut wanted: Vec<String> = Vec::with_capacity(asked.len());
        for name in asked {
            // A row's `item` and `name` are its own.
            if name != "item" && name != "name" && !wanted.contains(&name) {
                wanted.push(name);
            }
        }
        let shown = wanted.len();
        let mut conditions = Vec::with_capacity(filter.len());
        for (name, expected) in filter {
            let mut path = name.split('.');
            let head = path.next().unwrap_or_default().to_owned();
            let found = wanted.iter().position(|held| *held == head);
            let at = found.unwrap_or_else(|| {
                wanted.push(head);
                wanted.len().saturating_sub(1)
            });
            conditions.push((at, path.map(str::to_owned).collect(), expected));
        }
        // A chain that holds no item yet answers nothing either way.
        if !known.is_empty()
            && let Some(unknown) = wanted.iter().find(|name| !known.contains(name))
        {
            return Err(format!(
                "{} have no field {unknown}; their fields: {}",
                part.label,
                known.join(", ")
            ));
        }
        Ok(Self {
            wanted,
            shown,
            conditions,
        })
    }

    /// Whether the read answers only some items.
    pub(super) fn filters(&self) -> bool {
        !self.conditions.is_empty()
    }

    /// The fields `wanted` names of an item, from what the item's own read
    /// answers.
    fn values_of(&self, item: Option<&serde_json::Value>) -> Vec<Option<serde_json::Value>> {
        self.wanted
            .iter()
            .map(|name| item.and_then(|item| item.get(name)).cloned())
            .collect()
    }

    /// Whether an item whose fields read `values`, as `wanted` names them,
    /// is answered: every condition holds. A list of values holds when the
    /// field equals one of them, a value holds for a field that is a list
    /// when the list has it, and null holds when the item has no value.
    fn takes(&self, values: &[Option<serde_json::Value>]) -> bool {
        self.conditions.iter().all(|(at, path, expected)| {
            let mut value = values.get(*at).and_then(Option::as_ref);
            for name in path {
                value = value.and_then(|inner| inner.get(name));
            }
            match (expected, value.filter(|value| !value.is_null())) {
                (serde_json::Value::Null, value) => value.is_none(),
                (_, None) => false,
                (serde_json::Value::Array(any), Some(value)) if !value.is_array() => {
                    any.contains(value)
                }
                (expected, Some(serde_json::Value::Array(held))) if !expected.is_array() => {
                    held.contains(expected)
                }
                (expected, Some(value)) => expected == value,
            }
        })
    }

    /// The row of the item at `item` offered as `label`: its address, its
    /// name, and of the fields shown those it has a value of.
    fn row(
        &self,
        item: String,
        label: &str,
        values: &[Option<serde_json::Value>],
    ) -> serde_json::Value {
        let mut row = serde_json::Map::new();
        row.insert("item".into(), item.into());
        row.insert("name".into(), label.into());
        for (name, value) in self.wanted.iter().zip(values).take(self.shown) {
            if let Some(value) = value.as_ref().filter(|value| !value.is_null()) {
                row.insert(name.clone(), value.clone());
            }
        }
        serde_json::Value::Object(row)
    }
}

/// One page of a chain's listing as it fills.
pub(super) struct Page {
    /// The rows it answers with at most.
    size: usize,
    /// Its rows, by the collection field holding their items.
    pub(super) lists: BTreeMap<String, Vec<serde_json::Value>>,
    /// How many rows it holds.
    pub(super) taken: usize,
    /// Where the page after it reads on, once it is full.
    pub(super) next: Option<String>,
}

impl Page {
    pub(super) fn new(size: usize) -> Self {
        Self {
            size,
            lists: BTreeMap::new(),
            taken: 0,
            next: None,
        }
    }

    /// Take the items of part `index`, read as `state`, past its first
    /// `skip`, newest first, until the page is full. How many of the part's
    /// items the page has passed, or `None` when it filled before the part
    /// ended.
    pub(super) fn take(
        &mut self,
        index: u64,
        state: &dyn Any,
        table: &'static tilo_resource::table::ModelTable,
        kind: &str,
        listing: &Listing,
        hidden: &std::collections::HashSet<String>,
        skip: usize,
    ) -> Option<usize> {
        // What the part holds now, newest first: a cursor counted against
        // the part as it was reads from its end where items went since.
        let newest_first: Vec<(&str, tilo_resource::Id, String)> =
            tilo_resource::table::resource_items(table, state)
                .into_iter()
                .flat_map(|(field, found)| {
                    found
                        .into_iter()
                        .rev()
                        .map(move |(id, label)| (field, id, label))
                })
                .collect();
        let mut passed = skip.min(newest_first.len());
        for (field, id, label) in newest_first.iter().skip(passed) {
            let item = tilo_resource::items::item_resource_id_in(kind, field, id);
            // An item the chain does not show is passed over, as one the
            // read does not ask for.
            if hidden.contains(&item) {
                passed = passed.saturating_add(1);
                continue;
            }
            // An item is read only when the listing asks something of it.
            let values = if listing.wanted.is_empty() {
                Vec::new()
            } else {
                let read = tilo_resource::table::item_json(table, state, kind, &item).ok();
                listing.values_of(read.as_ref())
            };
            // An item the read does not ask for is passed over: the cursor
            // counts it, the page does not.
            if !listing.takes(&values) {
                passed = passed.saturating_add(1);
                continue;
            }
            if self.taken >= self.size {
                self.next = Some(read_cursor(index, passed));
                return None;
            }
            self.lists
                .entry((*field).to_owned())
                .or_default()
                .push(listing.row(item, label, &values));
            self.taken = self.taken.saturating_add(1);
            passed = passed.saturating_add(1);
        }
        Some(passed)
    }
}

/// The names of the fields the chain's items have, as the newest item held
/// reads: what a listing can be asked for. Empty while the chain holds no
/// item.
fn item_field_names(
    table: &'static tilo_resource::table::ModelTable,
    kind: &str,
    parts: &[(u64, &dyn Any)],
) -> Vec<String> {
    for (_, state) in parts.iter().rev() {
        for (field, found) in tilo_resource::table::resource_items(table, *state) {
            let Some((id, _)) = found.last() else {
                continue;
            };
            let item = tilo_resource::items::item_resource_id_in(kind, field, id);
            if let Ok(serde_json::Value::Object(fields)) =
                tilo_resource::table::item_json(table, *state, kind, &item)
            {
                return fields.keys().cloned().collect();
            }
        }
    }
    Vec::new()
}
