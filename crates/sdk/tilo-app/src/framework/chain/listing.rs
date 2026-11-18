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
/// keep of it, and what an item's fields equal for its row to be answered.
pub(super) struct Listing {
    /// The fields a row keeps of its item's value, when the read names
    /// them; else the whole value.
    shown: Option<Vec<String>>,
    /// Each condition: the names reaching from the item's value to the
    /// field compared, and what it equals.
    conditions: Vec<(Vec<String>, serde_json::Value)>,
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
        // A row's `id` is its own; `*`, every field, is the whole value, as
        // naming no fields is.
        let every = |names: &Vec<String>| names.iter().any(|name| name == "*");
        let shown: Option<Vec<String>> =
            read.fields
                .as_ref()
                .filter(|names| !every(names))
                .map(|names| {
                    let mut shown: Vec<String> = Vec::with_capacity(names.len());
                    for name in names {
                        if name != "id" && !shown.contains(name) {
                            shown.push(name.clone());
                        }
                    }
                    shown
                });
        let conditions: Vec<(Vec<String>, serde_json::Value)> = filter
            .into_iter()
            .map(|(name, expected)| (name.split('.').map(str::to_owned).collect(), expected))
            .collect();
        // The fields the items have, read only when something is asked of
        // them; a field named that they lack is refused. A chain that holds
        // no item yet answers nothing either way.
        let named = shown
            .iter()
            .flatten()
            .chain(conditions.iter().filter_map(|(path, _)| path.first()));
        let mut named = named.peekable();
        if named.peek().is_some() {
            let known = item_field_names(table, parts);
            if !known.is_empty()
                && let Some(unknown) = named.find(|name| !known.contains(*name))
            {
                return Err(format!(
                    "{} have no field {unknown}; their fields: {}",
                    part.label,
                    known.join(", ")
                ));
            }
        }
        Ok(Self { shown, conditions })
    }

    /// Whether the read answers only some items.
    pub(super) fn filters(&self) -> bool {
        !self.conditions.is_empty()
    }

    /// Whether an item whose value reads `value` is answered: every
    /// condition holds. A list of values holds when the field equals one of
    /// them, a value holds for a field that is a list when the list has it,
    /// and null holds when the item has no value.
    fn takes(&self, value: &serde_json::Value) -> bool {
        self.conditions.iter().all(|(path, expected)| {
            let mut found = Some(value);
            for name in path {
                found = found.and_then(|inner| inner.get(name));
            }
            match (expected, found.filter(|found| !found.is_null())) {
                (serde_json::Value::Null, found) => found.is_none(),
                (_, None) => false,
                (serde_json::Value::Array(any), Some(found)) if !found.is_array() => {
                    any.contains(found)
                }
                (expected, Some(serde_json::Value::Array(held))) if !expected.is_array() => {
                    held.contains(expected)
                }
                (expected, Some(found)) => expected == found,
            }
        })
    }

    /// The row of the item `id` whose value reads `value`: the entry an
    /// inline collection holds it by (`{id, value}`), its value kept to the
    /// fields shown when the read names them.
    fn row(&self, id: &tilo_resource::Id, value: serde_json::Value) -> serde_json::Value {
        let value = match (&self.shown, value) {
            (Some(shown), serde_json::Value::Object(mut fields)) => {
                fields.retain(|name, _| shown.contains(name));
                serde_json::Value::Object(fields)
            }
            (_, value) => value,
        };
        serde_json::json!({ "id": id.as_str(), "value": value })
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
        let mut held: Vec<(&'static str, Vec<Entry<'_>>)> = Vec::new();
        tilo_resource::stored_items::each_entry(
            table,
            state,
            &mut |field, id, item, of| match held.last_mut() {
                Some((at, entries)) if *at == field => entries.push((id, item, of)),
                _ => held.push((field, vec![(id, item, of)])),
            },
        );
        let newest_first = held.iter().flat_map(|(field, entries)| {
            entries
                .iter()
                .rev()
                .map(move |(id, item, of)| (*field, id, *item, *of))
        });
        let total: usize = held.iter().map(|(_, entries)| entries.len()).sum();
        let mut passed = skip.min(total);
        for (field, id, item, of) in newest_first.skip(passed) {
            // An item the chain does not show is passed over, as one the
            // read does not ask for.
            if !hidden.is_empty()
                && hidden.contains(&tilo_resource::items::item_resource_id_in(kind, field, id))
            {
                passed = passed.saturating_add(1);
                continue;
            }
            let value = tilo_resource::stored_items::json(of, item);
            // An item the read does not ask for is passed over: the cursor
            // counts it, the page does not.
            if !listing.takes(&value) {
                passed = passed.saturating_add(1);
                continue;
            }
            if self.taken >= self.size {
                self.next = Some(read_cursor(index, passed));
                return None;
            }
            self.lists
                .entry(field.to_owned())
                .or_default()
                .push(listing.row(id, value));
            self.taken = self.taken.saturating_add(1);
            passed = passed.saturating_add(1);
        }
        Some(passed)
    }
}

/// An item a part holds: its id, its value and its type's table.
type Entry<'a> = (
    tilo_resource::Id,
    &'a dyn Any,
    &'static tilo_resource::table::ModelTable,
);

/// The names of the fields the chain's items have, as the newest item held
/// reads: what a listing can be asked for. Empty while the chain holds no
/// item.
fn item_field_names(
    table: &'static tilo_resource::table::ModelTable,
    parts: &[(u64, &dyn Any)],
) -> Vec<String> {
    for (_, state) in parts.iter().rev() {
        let mut newest = None;
        tilo_resource::stored_items::each_entry(table, *state, &mut |_, _, item, of| {
            newest = Some(tilo_resource::stored_items::json(of, item));
        });
        if let Some(serde_json::Value::Object(fields)) = newest {
            return fields.keys().cloned().collect();
        }
    }
    Vec::new()
}
