//! Queries answered from the filter columns ([`super::columns`]): a collection's items that a
//! predicate holds of, in an order, and every resource of a type that one holds of.
//!
//! One conjunct of the predicate that a column answers chooses the run of entries read; the
//! others are checked item by item, their fields read from the index. A query that one exact
//! run answers (one value, a range, presence, a named filter) in the collection's order, or in
//! the order of the column it ranges over, is read page by page as the run: its first page
//! costs the directory and the pages it shows, whatever the collection's size. Any other is
//! read whole and sorted.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use tilolang_runtime::query::Order;
use tilolang_runtime::store::{FieldRead, Listing, Lit, Pred, StoreError, lit_of, order_cmp};

use super::columns::{
    self, Access, after_value, reversed_list, scope_entry, scope_list, type_list, value_run,
};
use super::derive::{column_field, member_list};
use super::lists::Pages;
use super::merged::{Base, Merged};
use super::view::{View, child, index_error};
use super::{IndexError, Snapshot, TYPE};
use crate::schema::{ColumnKind, TypeSchema};

/// The entries the open writes put into a run, and those they take out of it.
type Moved = (Vec<super::lists::Entry>, Vec<super::lists::Entry>);

/// The predicate's conjuncts that read a field; `None` when one that reads none does not hold.
fn conjuncts(pred: Option<&Pred>) -> Result<Option<Vec<&Pred>>, StoreError> {
    let mut out = Vec::new();
    for c in pred.map(Pred::conjuncts).unwrap_or_default() {
        if c.fields().is_empty() {
            let holds = c.eval(&mut |_: &FieldRead| -> Result<Lit, StoreError> {
                Err(StoreError::Invalid("a field".into()))
            })?;
            if !holds {
                return Ok(None);
            }
            continue;
        }
        out.push(c);
    }
    Ok(Some(out))
}

/// The conjunct a column answers best, and how: one exact run before anything else.
fn choose(schema: &TypeSchema, conj: &[&Pred], whole_type: bool) -> Option<(usize, Vec<Access>)> {
    let id = |f: &FieldRead| f.spec.id.clone();
    let mut found: Option<(usize, Vec<Access>)> = None;
    for (at, c) in conj.iter().enumerate() {
        let Some(access) = columns::access(schema, c, &id, whole_type) else {
            continue;
        };
        let single = matches!(access.as_slice(), [a] if a.runs.len() == 1 && a.exact && !a.repeats);
        if single {
            return Some((at, access));
        }
        if found.is_none() {
            found = Some((at, access));
        }
    }
    found
}

/// The predicate of the conjuncts left to check item by item.
fn rest(conj: &[&Pred], skip: Option<usize>) -> Option<Pred> {
    let left: Vec<Pred> = conj
        .iter()
        .enumerate()
        .filter(|(at, _)| Some(*at) != skip)
        .map(|(_, c)| (*c).clone())
        .collect();
    match left.len() {
        0 => None,
        1 => left.into_iter().next(),
        _ => Some(Pred::And(left)),
    }
}

/// Where a run starts and ends in `pages`.
fn bounds(pages: &Pages<Snapshot>, (from, to): &(String, String)) -> (usize, usize) {
    let (start, _) = pages.rank(from, "");
    let (end, _) = pages.rank(to, "");
    (start, end.max(start))
}

/// Where the run `run` starts and ends in the list `list` (read as `pages`) of the column
/// `column`: from the counts of its values when it is one value of a counted column, which
/// reads none of the list's pages; else from the pages.
fn run_bounds(
    snapshot: &Snapshot,
    (list, pages): (&str, &Pages<Snapshot>),
    column: Option<&crate::schema::ColumnSchema>,
    run: &(String, String),
) -> Result<(usize, usize), IndexError> {
    if let (Some(column), Some(value)) = (column, columns::run_value(run))
        && columns::counted(column)
        && columns::is_value_run(run)
    {
        let mut start = 0u64;
        let mut count = 0u64;
        for (held, n) in snapshot.counts(list)? {
            match held.as_str().cmp(value) {
                std::cmp::Ordering::Less => start = start.saturating_add(n),
                std::cmp::Ordering::Equal => count = n,
                std::cmp::Ordering::Greater => break,
            }
        }
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        return Ok((
            start,
            start.saturating_add(usize::try_from(count).unwrap_or(usize::MAX)),
        ));
    }
    Ok(bounds(pages, run))
}

impl View<'_> {
    /// The items of the collection `field` of `resource` (of the type `schema`) that `pred`
    /// holds of (all of them for none), in `order` (the collection's for none).
    ///
    /// # Errors
    /// The index cannot be read, or `pred` cannot compare what a field holds.
    pub fn query(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (pred, order): (Option<&Pred>, Option<&Order>),
        evaluated: &Cell<u64>,
    ) -> Result<Listing, StoreError> {
        let Some(conj) = conjuncts(pred)? else {
            return Ok(Listing::default());
        };
        let member = member_list(resource, field);
        let read: Vec<String> = conj
            .iter()
            .flat_map(|c| c.fields())
            .map(|f| f.spec.id.clone())
            .chain(order.map(|o| o.field.clone()))
            .collect();
        let named = self.overlay.touched_reading(
            &member,
            &|path| self.is_item_of(path, (resource, field)),
            &|written| read.iter().any(|r| r == written),
        );
        let touched = self.changed_items((resource, field), named, &read)?;
        let chosen = choose(schema, &conj, false);
        let order_column = order.and_then(|o| schema.column_of(&o.field));
        // A run read page by page: one exact run and nothing left to check, in the
        // collection's order or the order of the column it runs over.
        if let Some((at, access)) = &chosen
            && let [a] = access.as_slice()
            && let [run] = a.runs.as_slice()
            && a.exact
            && !a.repeats
            && conj.len() == 1
        {
            let column = schema.column(&a.column);
            let descending = order.is_some_and(|o| o.descending);
            let in_order = match (order, order_column) {
                (None, _) => columns::is_value_run(run),
                (Some(_), Some(oc)) => {
                    column.is_some_and(|c| c.name == oc.name) && ordered(oc.kind)
                }
                (Some(_), None) => false,
            };
            if in_order {
                let list = scope_list(resource, field, &a.column);
                let pages = self.snapshot.pages(&list).map_err(index_error)?;
                let (start, end) =
                    run_bounds(self.snapshot, (&list, &pages), column, run).map_err(index_error)?;
                let base = Base::Slice {
                    pages,
                    start,
                    end,
                    reversed: descending,
                };
                let (added, removed) = self.moved_in_run(
                    (resource, field),
                    schema,
                    (&a.column, conj.get(*at).copied()),
                    &touched,
                )?;
                let locate = self.locate((resource, field), Some((&schema.id, &a.column)));
                return Ok(Listing::paged(Rc::new(
                    Merged::new(base, added, &removed).locating(locate),
                )));
            }
        }
        // Every item in an order a column keeps.
        if conj.is_empty() {
            let Some(order) = order else {
                return self.listing(resource, field).map_err(index_error);
            };
            // The column's list, with the items the open writes moved in it moved.
            if let Some(oc) = order_column.filter(|oc| !oc.many && ordered_or_text(oc.kind)) {
                let list = scope_list(resource, field, &oc.name);
                let pages = self.snapshot.pages(&list).map_err(index_error)?;
                let end = pages.len();
                let (added, removed) =
                    self.moved_in_run((resource, field), schema, (&oc.name, None), &touched)?;
                let locate = self.locate((resource, field), Some((&schema.id, &oc.name)));
                return Ok(Listing::paged(Rc::new(
                    Merged::new(
                        Base::Slice {
                            pages,
                            start: 0,
                            end,
                            reversed: order.descending,
                        },
                        added,
                        &removed,
                    )
                    .locating(locate),
                )));
            }
        }
        self.read_whole(
            (resource, field),
            schema,
            (&conj, chosen.as_ref()),
            order,
            (&touched, evaluated),
        )
    }

    /// [`Self::query`] read whole: the candidates the chosen access reads (every item when
    /// none), checked against what is left, then ordered. One the open writes touched is
    /// checked against every conjunct, as it is now.
    fn read_whole(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (conj, chosen): (&[&Pred], Option<&(usize, Vec<Access>)>),
        order: Option<&Order>,
        (touched, evaluated): (&std::collections::BTreeSet<String>, &Cell<u64>),
    ) -> Result<Listing, StoreError> {
        let mut candidates: Vec<(String, Rc<str>, bool)> = Vec::new();
        let mut residual_skip = None;
        if let Some((at, access)) = chosen {
            if access.iter().all(|a| a.exact) {
                residual_skip = Some(*at);
            }
            let mut seen: HashSet<Rc<str>> = HashSet::new();
            for a in access {
                let list = scope_list(resource, field, &a.column);
                let pages = self.snapshot.pages(&list).map_err(index_error)?;
                let column = schema.column(&a.column);
                for run in &a.runs {
                    let (start, end) = run_bounds(self.snapshot, (&list, &pages), column, run)
                        .map_err(index_error)?;
                    for index in start..end {
                        let Some((key, id)) = pages.entry(index) else {
                            break;
                        };
                        if seen.insert(Rc::clone(&id)) {
                            candidates.push((after_value(&key).to_owned(), id, false));
                        }
                    }
                }
            }
            candidates.retain(|(_, id, _)| !touched.contains(&**id));
            for id in touched {
                if let Some(key) = self
                    .key_in(&child(resource, id), (resource, field))
                    .map_err(index_error)?
                {
                    candidates.push((key, Rc::from(id.as_str()), true));
                }
            }
        } else {
            let all = self.list(resource, field).map_err(index_error)?;
            for index in 0..all.len() {
                let Some(id) = all.get(index) else {
                    break;
                };
                candidates.push((String::new(), id, false));
            }
        }
        let residual = rest(conj, residual_skip);
        let every = rest(conj, None);
        let mut kept = Vec::with_capacity(candidates.len());
        for (key, id, touched) in candidates {
            let path = child(resource, &id);
            let check = if touched { &every } else { &residual };
            if let Some(p) = check {
                evaluated.set(evaluated.get().saturating_add(1));
                if !self.holds(&path, schema, p)? {
                    continue;
                }
            }
            kept.push((key, id, path));
        }
        if chosen.is_some() {
            kept.sort_by(|(a, ai, _), (b, bi, _)| (a.as_str(), &**ai).cmp(&(b.as_str(), &**bi)));
        }
        let items = match order {
            None => kept.into_iter().map(|(_, id, _)| id).collect(),
            Some(order) => self.ordered(
                kept.into_iter().map(|(_, id, path)| (id, path)),
                schema,
                order,
            )?,
        };
        Ok(Listing::from_items(items))
    }

    /// Of the items `candidates` of the collection `field` of `resource` the open writes
    /// named, those they changed for a query reading the fields `read`: added, removed or
    /// moved, or one of those fields holding another value than the index holds. A write that
    /// left them as they were (an item derived anew) moves nothing.
    fn changed_items(
        &self,
        (resource, field): (&str, &str),
        candidates: std::collections::BTreeSet<String>,
        read: &[String],
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let mut out = std::collections::BTreeSet::new();
        for id in candidates {
            let path = child(resource, &id);
            let mut changed = self
                .held_key_in(&path, (resource, field))
                .map_err(index_error)?
                != self.key_in(&path, (resource, field)).map_err(index_error)?;
            for f in read {
                if changed {
                    break;
                }
                changed = self.snapshot.field(&path, f).map_err(index_error)?
                    != self.field(&path, f).map_err(index_error)?;
            }
            if changed {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// `items` (id and path) in `order`, by the field's value as the index holds it.
    fn ordered<T: Clone>(
        &self,
        items: impl Iterator<Item = (T, String)>,
        schema: &TypeSchema,
        order: &Order,
    ) -> Result<Vec<T>, StoreError> {
        let field = schema
            .field(&order.field)
            .or_else(|| super::reflect::field(&order.field));
        let mut keyed = Vec::new();
        for (item, path) in items {
            let value = match field {
                Some(f) if super::reflect::is_reflected(&f.id) => self
                    .field(&path, &f.id)
                    .map_err(index_error)?
                    .map_or(Lit::Absent, |held| super::reflect::at_lit(&held)),
                Some(f) => {
                    let held = self.field(&path, &f.id).map_err(index_error)?;
                    lit_of(
                        &f.id,
                        held.unwrap_or_else(|| crate::reads::default_value(f)),
                        &Lit::Absent,
                    )?
                }
                None => Lit::Absent,
            };
            keyed.push((value, item));
        }
        keyed.sort_by(|(a, _), (b, _)| order_cmp(a, b));
        if order.descending {
            keyed.reverse();
        }
        Ok(keyed.into_iter().map(|(_, item)| item).collect())
    }

    /// The entries the open writes take out of a run of the column `column` (the items they
    /// touched, as the index has them) and put in (those the conjunct `pred` now holds of).
    fn moved_in_run(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (column, pred): (&str, Option<&Pred>),
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Moved, StoreError> {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let Some(spec) = schema.column(column) else {
            return Ok((added, removed));
        };
        for id in touched {
            let path = child(resource, id);
            let was_key = self
                .held_key_in(&path, (resource, field))
                .map_err(index_error)?;
            let was: Vec<String> = self
                .snapshot
                .text(&path, &column_field(column))
                .map_err(index_error)?
                .map(|held| held.split(columns::SEP).map(str::to_owned).collect())
                .unwrap_or_default();
            let now_key = self.key_in(&path, (resource, field)).map_err(index_error)?;
            let now = match &now_key {
                Some(_) => self
                    .column_values(&path, schema, spec)
                    .map_err(index_error)?,
                None => Vec::new(),
            };
            // Where it was is where it is: the writes moved nothing in the column.
            if was_key == now_key && was == now {
                continue;
            }
            if let Some(key) = &was_key {
                for value in &was {
                    removed.push(scope_entry(value, key, id));
                }
            }
            let Some(key) = now_key else {
                continue;
            };
            if let Some(p) = pred
                && !self.holds(&path, schema, p)?
            {
                continue;
            }
            for value in now {
                added.push(scope_entry(&value, &key, id));
            }
        }
        Ok((added, removed))
    }

    /// The resources of the type `schema` anywhere in the index that `pred` holds of, by
    /// path, in `order` (by path for none). The open writes are read where a resource's fields
    /// are; a resource they made is not found until they are written.
    ///
    /// # Errors
    /// The index cannot be read, or `pred` cannot compare what a field holds.
    pub fn select(
        &self,
        schema: &TypeSchema,
        pred: &Pred,
        order: Option<&Order>,
        evaluated: &Cell<u64>,
    ) -> Result<Vec<String>, StoreError> {
        let Some(conj) = conjuncts(Some(pred))? else {
            return Ok(Vec::new());
        };
        let ty = &schema.id;
        let chosen = choose(schema, &conj, true);
        let mut candidates: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut residual_skip = None;
        match &chosen {
            Some((at, access)) => {
                if access.iter().all(|a| a.exact) {
                    residual_skip = Some(*at);
                }
                for a in access {
                    let list = if a.reversed {
                        reversed_list(ty, &a.column)
                    } else {
                        type_list(ty, &a.column)
                    };
                    let pages = self.snapshot.pages(&list).map_err(index_error)?;
                    for run in &a.runs {
                        let (start, end) = bounds(&pages, run);
                        for index in start..end {
                            let Some((_, path)) = pages.entry(index) else {
                                break;
                            };
                            if seen.insert(path.to_string()) {
                                candidates.push(path.to_string());
                            }
                        }
                    }
                }
            }
            None => {
                // Every resource of the type: a column each of them stands in once.
                match schema.columns.iter().find(|c| c.filter && !c.many) {
                    Some(column) => {
                        let pages = self
                            .snapshot
                            .pages(&type_list(ty, &column.name))
                            .map_err(index_error)?;
                        for index in 0..pages.len() {
                            let Some((_, path)) = pages.entry(index) else {
                                break;
                            };
                            candidates.push(path.to_string());
                        }
                    }
                    None => {
                        for (path, held) in
                            super::view::resources(self.snapshot).map_err(index_error)?
                        {
                            if held == *ty {
                                candidates.push(path);
                            }
                        }
                    }
                }
            }
        }
        let residual = rest(&conj, residual_skip);
        let mut kept = Vec::new();
        for path in candidates {
            if self.text(&path, TYPE).map_err(index_error)?.is_none() {
                continue;
            }
            if let Some(p) = &residual {
                evaluated.set(evaluated.get().saturating_add(1));
                if !self.holds(&path, schema, p)? {
                    continue;
                }
            }
            kept.push(path);
        }
        kept.sort();
        match order {
            None => Ok(kept),
            Some(order) => self.ordered(kept.into_iter().map(|p| (p.clone(), p)), schema, order),
        }
    }

    /// How many items of the collection `field` of `resource` hold each value of the column
    /// `column` whose values are known (a toggle's, an enum's cases), as the index holds them:
    /// the difference of two positions in the column's list.
    ///
    /// # Errors
    /// The index cannot be read.
    pub fn facets(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        column: &str,
    ) -> Result<Vec<(String, usize)>, IndexError> {
        let Some(spec) = schema.column(column) else {
            return Ok(Vec::new());
        };
        let list = scope_list(resource, field, column);
        let pages = self.snapshot.pages(&list)?;
        let mut out = Vec::new();
        for value in columns::facet_values(spec) {
            let (start, end) = run_bounds(
                self.snapshot,
                (&list, &pages),
                Some(spec),
                &value_run(&value),
            )?;
            out.push((value, end.saturating_sub(start)));
        }
        Ok(out)
    }
}

fn ordered(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Int | ColumnKind::Float | ColumnKind::Date | ColumnKind::Time
    )
}

fn ordered_or_text(kind: ColumnKind) -> bool {
    ordered(kind) || matches!(kind, ColumnKind::Text | ColumnKind::Ident)
}
