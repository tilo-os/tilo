//! Queries answered from the filter columns ([`super::columns`]): a collection's items that a
//! predicate holds of, in an order, and every resource of a type that one holds of.
//!
//! One conjunct of the predicate that a column answers chooses the run of entries read; the
//! others are checked item by item, their fields read from the index. A query that one exact
//! run answers (one value, a range, presence, a named filter) in the collection's order, or in
//! the order of the column it ranges over, is read page by page as the run: its first page
//! costs the directory and the pages it shows, whatever the collection's size, and what it
//! leaves to check is checked as its pages fill ([`Filtered`]). Any other is read whole and
//! sorted.

use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use tilolang_runtime::query::Order;
use tilolang_runtime::store::{
    FieldRead, Listing, Lit, PagedItems, Pred, StoreError, lit_of, ordered_cmp,
};

use super::columns::{
    self, Access, after_value, reversed_list, scope_entry, scope_list, type_list, value_run,
};
use super::derive::{column_field, member_list};
use super::lists::Pages;
use super::merged::{Base, Filtered, Keep, Merged, PAGE, Partition, Shape};
use super::view::{View, child, index_error};
use super::{IndexError, Snapshot, TYPE};
use crate::schema::{ColumnKind, TypeSchema};

/// The entries the open writes put into a run, and those they take out of it.
type Moved = (Vec<super::lists::Entry>, Vec<super::lists::Entry>);

/// The parts of a listing ordered by two keys: one per run of the key before the last, each the
/// column and the value it is the run of ([`View::order_parts`]).
type Parts = Vec<Option<(crate::schema::ColumnSchema, String)>>;

/// What a query shows, and the listing it read, when a later change may keep what it checked
/// ([`Filtered::spliced`]).
pub struct Answer {
    pub listing: Listing,
    pub carry: Option<Rc<Filtered>>,
}

impl Answer {
    /// The answer is the listing, read whole: a change reads it anew.
    fn shown(listing: Listing) -> Self {
        Self {
            listing,
            carry: None,
        }
    }

    /// The answer is the listing `filtered`, whose checks a change of the items it names keeps.
    fn carried(filtered: Filtered) -> Self {
        let filtered = Rc::new(filtered);
        Self {
            listing: Listing::paged(Rc::clone(&filtered) as Rc<dyn PagedItems>),
            carry: Some(filtered),
        }
    }
}

/// The listing a query read before a change, and the items the change wrote: what a query read
/// the same way again keeps of it rather than checking again.
pub struct Carried<'a> {
    pub before: &'a Filtered,
    pub touched: &'a BTreeSet<String>,
}

/// The listing a run read page by page is of, as [`Filtered::spliced`] compares it: the read
/// that names the run and the checks it leaves.
fn shape_of(kind: &str, run: &str, (descending, absent_last): (bool, bool), part: &str) -> Shape {
    let flags = |flag: bool| if flag { "1" } else { "0" };
    let mut out = String::from(kind);
    for held in [run, part, flags(descending), flags(absent_last)] {
        out.push(columns::SEP);
        out.push_str(held);
    }
    Rc::from(out)
}

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

/// The conjunct a column answers best, and how: one exact run before anything else, and
/// whether that conjunct is a set's membership (whose run an item stands in once, so it is read
/// page by page like a value's run).
fn choose(schema: &TypeSchema, conj: &[&Pred], whole_type: bool) -> Option<(usize, Vec<Access>)> {
    let id = |f: &FieldRead| f.spec.id.clone();
    let mut found: Option<(usize, Vec<Access>)> = None;
    for (at, c) in conj.iter().enumerate() {
        let Some(access) = columns::access(schema, c, &id, whole_type) else {
            continue;
        };
        // A run of one value in a column an item stands in once, or a membership in a set
        // (one entry per member, the run of the ones holding it): both name the items a
        // single value puts in, so both are read page by page.
        let single = matches!(
            access.as_slice(),
            [a] if a.runs.len() == 1 && a.exact && (!a.repeats || membership(c))
        );
        if single {
            return Some((at, access));
        }
        if found.is_none() {
            found = Some((at, access));
        }
    }
    found
}

/// Whether `pred` is `f.contains(v)` of a set: the one shape whose run of a value names the
/// items holding it and nothing else, though a set's items may stand in several runs.
fn membership(pred: &Pred) -> bool {
    matches!(pred, Pred::Has(..))
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

/// The conjunct of a query that names the fewest items: its place in the predicate, the runs
/// that read it, and how many items they hold ([`View::narrowest`]).
type Fewest = (usize, Vec<Access>, usize);
/// The conjunct a query reads page by page: its place in the predicate and the runs that read it
/// ([`choose`]).
type Chosen = (usize, Vec<Access>);

/// The fields a query reads: those its conjuncts compare, and those its order sorts by.
fn read_fields(conj: &[&Pred], order: Option<&Order>) -> Vec<String> {
    conj.iter()
        .flat_map(|c| c.fields())
        .map(|f| f.spec.id.clone())
        .chain(
            order
                .into_iter()
                .flat_map(|o| o.levels().into_iter().map(|level| level.field.clone())),
        )
        .collect()
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
    pub fn query(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (pred, order): (Option<&Pred>, Option<&Order>),
        (carried, evaluated): (Option<&Carried<'_>>, &Cell<u64>),
    ) -> Result<Answer, StoreError> {
        let Some(conj) = conjuncts(pred)? else {
            return Ok(Answer::shown(Listing::default()));
        };
        // A named filter keeps a column holding whether its predicate does: the column answers
        // the predicate whole, so its run is read rather than a conjunct of it, which names
        // every item that conjunct holds of and leaves the rest to be checked one by one.
        let id = |f: &FieldRead| f.spec.id.clone();
        let conj: Vec<&Pred> =
            match pred.filter(|whole| columns::named(schema, whole, &id).is_some()) {
                Some(whole) => vec![whole],
                None => conj,
            };
        let member = member_list(resource, field);
        let read = read_fields(&conj, order);
        let named = self.overlay.touched_reading(
            &member,
            &|path| self.is_item_of(path, (resource, field)),
            &|written| read.iter().any(|r| r == written),
        );
        let touched = self.changed_items((resource, field), named, &read)?;
        let chosen = choose(schema, &conj, false);
        // A predicate of one conjunct is its run, whole: nothing is left to check against it.
        // Where one is left, the conjunct naming the fewest items is read instead, and the
        // count of the fewest the others can name sizes the list before its pages are read.
        let (narrow, bound) = if chosen
            .as_ref()
            .and_then(|(at, _)| rest(&conj, Some(*at)))
            .is_some()
        {
            (
                self.narrowest((resource, field), schema, &conj, &touched)?,
                self.run_count((resource, field), schema, &conj, &touched)?,
            )
        } else {
            (None, None)
        };
        // A run no longer than a page is read whole: it names both the items the query holds
        // and how many they are, exactly, where a listing read page by page only says the most
        // its run can hold. A longer run is read as the pages a view asks for.
        if let Some((at, access, count)) = narrow.clone()
            && count <= PAGE
        {
            return self
                .read_whole(
                    (resource, field),
                    schema,
                    (&conj, Some(&(at, access))),
                    order,
                    (&touched, evaluated),
                )
                .map(Answer::shown);
        }
        if let Some(answer) = self.narrowed(
            (resource, field),
            schema,
            (&conj, order),
            chosen.as_ref(),
            (&touched, evaluated),
        )? {
            return Ok(answer);
        }
        // The runs a query may be read page by page from: the one the narrowest conjunct names
        // first, then the one [`choose`] found. A run read page by page is one exact run, in the
        // collection's order or the order of the column it runs over; the conjuncts it leaves
        // are checked as its pages fill ([`Filtered`]), so a filter of several conjuncts costs
        // the entries its first page reads, not the run's length. A narrowest run that cannot be
        // read that way (a range, whose entries run by value when the collection's own order is
        // asked) leaves the listing to the run that can: reading it whole reads every item it
        // names, which is the workspace's size for a place that holds half of it. The narrowest
        // still sizes the listing, before its pages are read.
        let narrow = narrow.map(|(at, access, _)| (at, access));
        let order_column = order.and_then(|o| schema.column(&o.field));
        let mut run = None;
        for (at, access) in narrow.iter().chain(chosen.iter()) {
            let [a] = access.as_slice() else {
                continue;
            };
            let [one] = a.runs.as_slice() else {
                continue;
            };
            if !a.exact
                || (a.repeats && !membership(conj.get(*at).copied().unwrap_or(&Pred::Const(false))))
            {
                continue;
            }
            run = self.paged_run(
                (resource, field),
                schema,
                (&conj, *at, a, one),
                (order, order_column),
                (Some(bound).flatten(), &touched, carried, evaluated),
            )?;
            if run.is_some() {
                break;
            }
        }
        if let Some(listing) = run {
            return Ok(listing);
        }
        // Every item in an order the index's columns keep, read page by page.
        if let Some(order) = order {
            let answer = self.ordered_listing(
                (resource, field),
                schema,
                (&conj, order),
                (&touched, carried, evaluated),
            )?;
            if let Some(listing) = answer {
                return Ok(listing);
            }
        }
        if conj.is_empty() && order.is_none() {
            return self
                .listing(resource, field)
                .map(Answer::shown)
                .map_err(index_error);
        }
        // Read whole: the narrowest conjunct's run, which names the fewest items to check.
        self.read_whole(
            (resource, field),
            schema,
            (&conj, narrow.as_ref().or(chosen.as_ref())),
            order,
            (&touched, evaluated),
        )
        .map(Answer::shown)
    }

    /// The answer of a query ordered by a column, when the run of the conjunct it reads names no
    /// more items than the page a listing reads at once: read whole and put in the order, so a
    /// place of the day's work costs those tasks and not the collection. A longer run is left to
    /// the pages of the order column ([`Self::ordered_listing`]): reading it whole reads an entry
    /// of the index per item it names, which is the workspace's size for a place that holds most
    /// of it, where a page costs what the window shows.
    fn narrowed(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (conj, order): (&[&Pred], Option<&Order>),
        chosen: Option<&Chosen>,
        (touched, evaluated): (&std::collections::BTreeSet<String>, &Cell<u64>),
    ) -> Result<Option<Answer>, StoreError> {
        let Some(order) = order.filter(|order| order.then.is_none()) else {
            return Ok(None);
        };
        let Some((at, access)) = chosen else {
            return Ok(None);
        };
        let (at, access) = (*at, access.clone());
        let Some(count) = self.bound_of(
            (resource, field),
            schema,
            conj.get(at).copied().unwrap_or(&Pred::Const(false)),
            touched,
        )?
        else {
            return Ok(None);
        };
        if count > PAGE {
            return Ok(None);
        }
        self.read_whole(
            (resource, field),
            schema,
            (conj, Some(&(at, access))),
            Some(order),
            (touched, evaluated),
        )
        .map(|listing| Some(Answer::shown(listing)))
    }

    /// The items of the collection `field` of `resource` under the item `node` (above it, for
    /// `down` false) by the reference field `parent`: the items under it in the order the
    /// collection keeps them, the items above it from the nearest, each with the type it is
    /// (the collection's `schema`).
    ///
    /// The field's column holds its values as runs ([`super::columns`]): the run of the node
    /// names its children, and each child's run the items under it, so what a walk reads is
    /// the subtree it answers, never the collection. `None` when the field is no column of the
    /// type, and the caller's own walk runs instead.
    pub fn relatives(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        parent: &str,
        node: &str,
        down: bool,
    ) -> Result<Option<Vec<(String, String)>>, StoreError> {
        let Some(column) = schema.column(parent).filter(|column| {
            column.kind == ColumnKind::Ref && !column.many && column.pred.is_none()
        }) else {
            return Ok(None);
        };
        let member = member_list(resource, field);
        let touched = self.overlay.touched_reading(
            &member,
            &|path| self.is_item_of(path, (resource, field)),
            &|written| written == parent,
        );
        let mut found: Vec<(String, Rc<str>)> = Vec::new();
        let mut walked: HashSet<String> = HashSet::from([node.to_owned()]);
        if down {
            let mut above = vec![node.to_owned()];
            while let Some(at) = above.pop() {
                for (key, id) in self.children((resource, field), schema, column, &at, &touched)? {
                    if walked.insert(id.to_string()) {
                        above.push(id.to_string());
                        found.push((key, id));
                    }
                }
            }
            found.sort_by(|(a, ai), (b, bi)| (a.as_str(), &**ai).cmp(&(b.as_str(), &**bi)));
        } else {
            let mut at = node.to_owned();
            while let Lit::Ref(held) = self.lit_of_field(&child(resource, &at), schema, parent)? {
                let Some(id) = super::derive::id_of(&held).map(str::to_owned) else {
                    break;
                };
                if !walked.insert(id.clone()) {
                    break;
                }
                found.push((String::new(), Rc::from(id.as_str())));
                at = id;
            }
        }
        Ok(Some(
            found
                .into_iter()
                .map(|(_, id)| (id.to_string(), schema.id.clone()))
                .collect(),
        ))
    }

    /// The items of the collection `field` of `resource` whose `parent` column holds `above`,
    /// as the open writes leave them: their collection order key and id.
    fn children(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        column: &crate::schema::ColumnSchema,
        above: &str,
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<(String, Rc<str>)>, StoreError> {
        let here = child(resource, above);
        let list = scope_list(resource, field, &column.name);
        let pages = self.snapshot.pages(&list).map_err(index_error)?;
        let run = columns::value_run(&here);
        let (start, end) =
            run_bounds(self.snapshot, (&list, &pages), Some(column), &run).map_err(index_error)?;
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        for index in start..end {
            let Some((key, id)) = pages.entry(index) else {
                break;
            };
            // What an open write changed it to is read below, as it is now.
            if touched.contains(&*id) {
                continue;
            }
            out.push((after_value(&key).to_owned(), id));
        }
        for id in touched {
            let path = child(resource, id);
            let Some(key) = self.key_in(&path, (resource, field)).map_err(index_error)? else {
                continue;
            };
            let now = self
                .column_values(&path, schema, column)
                .map_err(index_error)?;
            if now.contains(&here) {
                out.push((key, Rc::from(id.as_str())));
            }
        }
        Ok(out)
    }

    /// [`Self::query`]'s run read page by page, when the chosen run is in the collection's order
    /// or the order of the column it runs over: the entries of that run whose items the other
    /// conjuncts hold of, `None` when the run is not read that way (another order, or the
    /// conjunct it answered is not one exact run).
    fn paged_run(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (conj, at, a, run): (&[&Pred], usize, &Access, &(String, String)),
        (order, order_column): (Option<&Order>, Option<&crate::schema::ColumnSchema>),
        (bound, touched, carried, evaluated): (
            Option<usize>,
            &BTreeSet<String>,
            Option<&Carried<'_>>,
            &Cell<u64>,
        ),
    ) -> Result<Option<Answer>, StoreError> {
        let column = schema.column(&a.column);
        let descending = order.is_some_and(|o| o.descending);
        // A run is read in one order; an order over another one, or one with the items without
        // a value last, is not that order: [`Self::ordered_listing`] reads it instead.
        if order.is_some_and(|o| o.then.is_some() || o.absent_last) {
            return Ok(None);
        }
        let in_order = match (order, order_column) {
            (None, _) => columns::is_value_run(run),
            (Some(_), Some(oc)) => column.is_some_and(|c| c.name == oc.name) && ordered(oc.kind),
            (Some(_), None) => false,
        };
        if !in_order {
            return Ok(None);
        }
        let run = self.run_as_written(
            (resource, field),
            schema,
            (&a.column, conj.get(at).copied()),
            run,
            descending,
            touched,
        )?;
        let locate = self.locate((resource, field), Some((&schema.id, &a.column)));
        let Some(residual) = rest(conj, Some(at)) else {
            return Ok(Some(Answer::shown(Listing::paged(Rc::new(
                run.locating(locate),
            )))));
        };
        // The listing outlives this read, so it holds the snapshot and the schema itself: a
        // check of the run reads the index's fields, which hold the open writes too.
        let Some(keep_schema) = crate::schema::lookup(&schema.id) else {
            return Ok(None);
        };
        let evaluated = evaluated.clone();
        let resource = resource.to_owned();
        let snapshot = std::sync::Arc::clone(self.snapshot);
        let overlay = std::sync::Arc::clone(self.overlay);
        let clock = self.clock;
        let keep: Keep = Box::new(move |id: &str| {
            evaluated.set(evaluated.get().saturating_add(1));
            let view = View {
                snapshot: &snapshot,
                overlay: &overlay,
                clock,
            };
            view.holds(&child(&resource, id), keep_schema, &residual)
                .unwrap_or(false)
        });
        // The most the listing can hold: the run's own count, or the count of a narrower
        // conjunct of the predicate, which the run is read against item by item.
        let shape = shape_of(
            "run",
            &format!("{at}{}{}", columns::SEP, a.column),
            (descending, false),
            "",
        );
        let filtered = Self::spliced(carried, run, keep, shape, touched);
        Ok(Some(Answer::carried(
            filtered.locating(locate).bounded(bound),
        )))
    }

    /// A part per run of the key before the last one: a column an item stands in once, whose
    /// runs follow one another in its values' order. `None` when the key is not such a column, or
    /// a write the change names may have moved an item between its runs.
    fn order_parts(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        before: Option<&Order>,
        touched: &BTreeSet<String>,
    ) -> Result<Option<Parts>, StoreError> {
        let Some(before) = before else {
            return Ok(Some(vec![None]));
        };
        if !touched.is_empty() {
            return Ok(None);
        }
        let Some(column) = schema
            .column(&before.field)
            .filter(|column| columns::counted(column))
        else {
            return Ok(None);
        };
        let list = scope_list(resource, field, &column.name);
        let mut values: Vec<String> = self
            .snapshot
            .counts(&list)
            .map_err(index_error)?
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(value, _)| value)
            .collect();
        if before.descending {
            values.reverse();
        }
        Ok(Some(
            values
                .into_iter()
                .map(|value| Some((column.clone(), value)))
                .collect(),
        ))
    }

    /// [`Self::query`]'s order read from the index's columns: the key the order falls back to
    /// read as its column's runs, in one part per run of the key before it, and the conjuncts
    /// checked as the parts' pages fill ([`Filtered`], [`Partition`]).
    ///
    /// `None` when the last key has no column read in its order, when a key before it is not a
    /// column an item stands in once (a counted one), when the items without a value in the
    /// last key are to stand first, or when a write of the open ones may have moved an item
    /// between the runs of a key before it: the caller reads the items whole.
    fn ordered_listing(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (conj, order): (&[&Pred], &Order),
        (touched, carried, evaluated): (&BTreeSet<String>, Option<&Carried<'_>>, &Cell<u64>),
    ) -> Result<Option<Answer>, StoreError> {
        let levels = order.levels();
        let Some((scan, outer)) = levels.split_last() else {
            return Ok(None);
        };
        // The items with a value run from the first one that has one, those without one after
        // them: one read that way does not also run from the last.
        if scan.absent_last && scan.descending {
            return Ok(None);
        }
        let Some(scan_column) = schema
            .column(&scan.field)
            .filter(|column| !column.many && ordered_or_text(column.kind))
        else {
            return Ok(None);
        };
        let before = match outer {
            [] => None,
            [one] => Some(*one),
            _ => return Ok(None),
        };
        let Some(parts) = self.order_parts((resource, field), schema, before, touched)? else {
            return Ok(None);
        };
        // The listing outlives this read: it holds the snapshot and the schema itself, as the
        // check of a run does ([`Self::paged_run`]).
        let Some(keep_schema) = crate::schema::lookup(&schema.id) else {
            return Ok(None);
        };
        // The most items the conjuncts can name, from the columns' runs: what a list sized
        // before its pages are read costs, where reading its pages whole costs the workspace.
        let bound = self.run_count((resource, field), schema, conj, touched)?;
        let counted =
            self.counted_whole((resource, field), schema, (conj, before.is_none()), touched)?;
        let residual = rest(conj, None);
        let mut reads: Vec<Filtered> = Vec::new();
        let mut most = None;
        for part in &parts {
            let base = self.listed(
                (resource, field),
                scan_column,
                (scan.absent_last, scan.descending),
            )?;
            let (added, removed) = self.moved_in_run(
                (resource, field),
                schema,
                (&scan_column.name, None),
                touched,
            )?;
            let run = Merged::new(base, added, &removed);
            most.get_or_insert_with(|| run.len());
            let keep = self.keeping(
                resource,
                keep_schema,
                residual.as_ref(),
                part.as_ref(),
                evaluated,
            );
            let locate = self.locate((resource, field), Some((&schema.id, &scan_column.name)));
            let shape = shape_of(
                "order",
                &scan_column.name,
                (scan.descending, scan.absent_last),
                &part
                    .as_ref()
                    .map(|(column, value)| format!("{}{}{value}", column.name, columns::SEP))
                    .unwrap_or_default(),
            );
            let filtered = Self::spliced(carried, run, keep, shape, touched);
            let filtered = match counted {
                Some(count) => filtered.known(count),
                None => filtered,
            };
            reads.push(filtered.locating(locate).bounded(bound));
        }
        if parts.len() == 1 {
            let Some(one) = reads.pop() else {
                return Ok(None);
            };
            return Ok(Some(Answer::carried(one)));
        }
        let most = bound.unwrap_or_else(|| most.unwrap_or(0));
        Ok(Some(Answer::shown(Listing::paged(Rc::new(
            Partition::new(reads, most),
        )))))
    }

    /// How many items one exact run of one column holds, when one conjunct answers the whole
    /// predicate: the count the index keeps of it, so a list sized before its pages are read
    /// costs a count rather than a read of every item.
    fn counted_whole(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (conj, alone): (&[&Pred], bool),
        touched: &BTreeSet<String>,
    ) -> Result<Option<usize>, StoreError> {
        let [one] = conj else {
            return Ok(None);
        };
        if !alone {
            return Ok(None);
        }
        let id = |f: &FieldRead| f.spec.id.clone();
        let accesses = columns::access(schema, one, &id, false);
        let Some([a]) = accesses.as_deref() else {
            return Ok(None);
        };
        let [run] = a.runs.as_slice() else {
            return Ok(None);
        };
        if !a.exact || a.repeats {
            return Ok(None);
        }
        // The items the open writes moved into the run, checked against the conjunct it
        // answers: a run read without one takes every item they wrote, which is the most it
        // can hold rather than how many it does.
        let run = self.run_as_written(
            (resource, field),
            schema,
            (&a.column, Some(*one)),
            run,
            false,
            touched,
        )?;
        Ok(Some(run.len()))
    }

    /// The listing of the run `base` kept by `keep`, of the read `shape`: what `carried` kept of
    /// the run it read before, when the change left it the same read, or the run checked here.
    fn spliced(
        carried: Option<&Carried<'_>>,
        base: Merged,
        keep: Keep,
        shape: Shape,
        touched: &BTreeSet<String>,
    ) -> Filtered {
        let Some(carried) = carried else {
            return Filtered::new(base, keep).shaped(shape);
        };
        // The items the change wrote, and those the open writes have written since: either may
        // have moved an entry of the run.
        let mut touched = touched.clone();
        touched.extend(carried.touched.iter().cloned());
        match Filtered::spliced(carried.before, base, keep, shape.clone(), &touched) {
            Ok(spliced) => spliced,
            Err(boxed) => {
                let (base, keep) = *boxed;
                Filtered::new(base, keep).shaped(shape)
            }
        }
    }

    /// The entries of the column `column` of the collection `field` of `resource` as a listing
    /// reads them: in the column's order, the items without a value last when `absent_last`, and
    /// from the last one when `descending`.
    fn listed(
        &self,
        (resource, field): (&str, &str),
        column: &crate::schema::ColumnSchema,
        (absent_last, descending): (bool, bool),
    ) -> Result<Base, StoreError> {
        let list = scope_list(resource, field, &column.name);
        let pages = self.snapshot.pages(&list).map_err(index_error)?;
        if absent_last {
            let absent = pages.absent_run();
            return Ok(Base::Presence { pages, absent });
        }
        let end = pages.len();
        Ok(Base::Slice {
            pages,
            start: 0,
            end,
            reversed: descending,
        })
    }

    /// The check a listing makes as its pages fill: the item stands in the run `part` names, and
    /// the conjuncts `residual` left hold of it. It reads the item's fields then, the open writes
    /// among them, so it holds the schema and the snapshot itself.
    fn keeping(
        &self,
        resource: &str,
        schema: &'static TypeSchema,
        residual: Option<&Pred>,
        part: Option<&(crate::schema::ColumnSchema, String)>,
        evaluated: &Cell<u64>,
    ) -> Keep {
        let evaluated = evaluated.clone();
        let resource = resource.to_owned();
        let residual = residual.cloned();
        let part = part.cloned();
        let snapshot = std::sync::Arc::clone(self.snapshot);
        let overlay = std::sync::Arc::clone(self.overlay);
        let clock = self.clock;
        Box::new(move |id: &str| {
            evaluated.set(evaluated.get().saturating_add(1));
            let view = View {
                snapshot: &snapshot,
                overlay: &overlay,
                clock,
            };
            let path = child(&resource, id);
            if let Some((column, value)) = &part
                && !view
                    .column_values(&path, schema, column)
                    .is_ok_and(|values| values.iter().any(|held| held == value))
            {
                return false;
            }
            match &residual {
                Some(pred) => view.holds(&path, schema, pred).unwrap_or(false),
                None => true,
            }
        })
    }

    /// The conjunct of `conj` whose runs name the fewest items, as the open writes leave them,
    /// its access and how many items those runs hold: the smallest of the runs the query can be
    /// narrowed to, which it is cheapest to read whole. `None` when no conjunct names a run.
    fn narrowest(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        conj: &[&Pred],
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Option<Fewest>, StoreError> {
        let id = |f: &FieldRead| f.spec.id.clone();
        let mut best: Option<Fewest> = None;
        for (at, c) in conj.iter().enumerate() {
            let Some(access) = columns::access(schema, c, &id, false) else {
                continue;
            };
            let mut count = 0usize;
            for a in &access {
                for run in &a.runs {
                    let held = self.run_as_written(
                        (resource, field),
                        schema,
                        (&a.column, None),
                        run,
                        false,
                        touched,
                    )?;
                    count = count.saturating_add(held.len());
                }
            }
            if best.as_ref().is_none_or(|(_, _, held)| count < *held) {
                best = Some((at, access, count));
            }
        }
        Ok(best)
    }

    /// The most items `conj` can name, from the columns' runs and counts: the smallest upper
    /// bound any conjunct gives. A conjunct of an `and` bounds the whole of it, and an `or` is
    /// bounded by the sum of its branches, so `not done and (picked or due <= today)` names no
    /// more than what is due, which a list sized by it costs a count of rather than a read of
    /// every item it holds.
    ///
    /// The runs are counted as the open writes leave them: one counted from the index alone is
    /// short of what was just written, and a list sized by it shows none of that.
    fn run_count(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        conj: &[&Pred],
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Option<usize>, StoreError> {
        let mut most: Option<usize> = None;
        for c in conj {
            let Some(bound) = self.bound_of((resource, field), schema, c, touched)? else {
                continue;
            };
            most = Some(most.map_or(bound, |held| held.min(bound)));
        }
        Ok(most)
    }

    /// The most items `pred` can name, from the columns' runs: an `and` no more than any one of
    /// its conjuncts, an `or` no more than the sum of its branches, a run of a column no more
    /// than its entries. `None` when a conjunct names no exact run, and so bounds nothing.
    fn bound_of(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        pred: &Pred,
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Option<usize>, StoreError> {
        match pred {
            Pred::And(conj) => {
                let conj: Vec<&Pred> = conj.iter().collect();
                self.run_count((resource, field), schema, &conj, touched)
            }
            Pred::Or(each) => {
                let mut sum = 0usize;
                for branch in each {
                    let Some(bound) = self.bound_of((resource, field), schema, branch, touched)?
                    else {
                        return Ok(None);
                    };
                    sum = sum.saturating_add(bound);
                }
                Ok(Some(sum))
            }
            leaf => {
                let id = |f: &FieldRead| f.spec.id.clone();
                let Some(access) = columns::access(schema, leaf, &id, false) else {
                    return Ok(None);
                };
                let mut sum = 0usize;
                for a in access {
                    if !a.exact {
                        return Ok(None);
                    }
                    for run in &a.runs {
                        let held = self.run_as_written(
                            (resource, field),
                            schema,
                            (&a.column, None),
                            run,
                            false,
                            touched,
                        )?;
                        sum = sum.saturating_add(held.len());
                    }
                }
                Ok(Some(sum))
            }
        }
    }

    /// The run `run` of the column `column` that `pred` chose, as the open writes leave it: the
    /// entries of its run of the column's list, in its order (`descending` from the last), with
    /// those the writes put in and took out.
    fn run_as_written(
        &self,
        (resource, field): (&str, &str),
        schema: &TypeSchema,
        (column, pred): (&str, Option<&Pred>),
        run: &(String, String),
        descending: bool,
        touched: &std::collections::BTreeSet<String>,
    ) -> Result<Merged, StoreError> {
        let list = scope_list(resource, field, column);
        let pages = self.snapshot.pages(&list).map_err(index_error)?;
        let column_spec = schema.column(column);
        let (start, end) =
            run_bounds(self.snapshot, (&list, &pages), column_spec, run).map_err(index_error)?;
        let base = Base::Slice {
            pages,
            start,
            end,
            reversed: descending,
        };
        let (added, removed) =
            self.moved_in_run((resource, field), schema, (column, pred), touched)?;
        Ok(Merged::new(base, added, &removed))
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
            let membership = conj.get(*at).copied().is_some_and(membership);
            if access.iter().all(|a| a.exact)
                && (access.first().is_none_or(|a| !a.repeats) || membership)
            {
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
        // A read whole carries the key each item stands at in the column its run was read
        // from; the read of the collection's list does not.
        let keys = chosen.is_some();
        let items = match order {
            None => kept.into_iter().map(|(_, id, _)| id).collect(),
            Some(order) => self.ordered(
                kept.into_iter()
                    .map(|(key, id, path)| (id, path, keys.then_some(key))),
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

    /// `items` (id, path and the key the item stands at in the column it was read from) in
    /// `order`, by the fields' values as the index holds them: each key after the one before it,
    /// and those it cannot tell apart in the order of the keys, as the column keeps them, or the
    /// order the items came in where they have none.
    fn ordered<T: Clone>(
        &self,
        items: impl Iterator<Item = (T, String, Option<String>)>,
        schema: &TypeSchema,
        order: &Order,
    ) -> Result<Vec<T>, StoreError> {
        let levels = order.levels();
        // The column's keys are read the way its last level is: the items one value holds stand
        // in it by them, last first where the level descends.
        let descending = levels.last().is_some_and(|level| level.descending);
        let mut keyed = Vec::new();
        for (item, path, key) in items {
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                keys.push(self.lit_of_field(&path, schema, &level.field)?);
            }
            keyed.push((keys, key, item));
        }
        keyed.sort_by(|(a, ak, _), (b, bk, _)| {
            ordered_cmp(order, a, b).then_with(|| match (ak, bk) {
                (Some(a), Some(b)) if descending => b.cmp(a),
                (Some(a), Some(b)) => a.cmp(b),
                _ => std::cmp::Ordering::Equal,
            })
        });
        Ok(keyed.into_iter().map(|(_, _, item)| item).collect())
    }

    /// The value of the field `field` at `path`, of the type `schema`, as the index holds it, as
    /// an [`Order`] compares items by it; absent when the type has no such field.
    fn lit_of_field(
        &self,
        path: &str,
        schema: &TypeSchema,
        field: &str,
    ) -> Result<Lit, StoreError> {
        let Some(f) = schema.field(field) else {
            let Some(f) = super::reflect::field(field) else {
                // When a field last changed, which the index keeps as a pseudo-field of its
                // own: by the time of the stamp it holds.
                if super::reflect::field_of_changed(field).is_none() {
                    return Ok(Lit::Absent);
                }
                let held = self.field(path, field).map_err(index_error)?;
                return Ok(held.map_or(Lit::Absent, |held| super::reflect::at_lit(&held)));
            };
            let held = self.field(path, &f.id).map_err(index_error)?;
            return Ok(held.map_or(Lit::Absent, |held| super::reflect::at_lit(&held)));
        };
        let held = self.field(path, &f.id).map_err(index_error)?;
        let whole = held.unwrap_or_else(|| crate::reads::default_value(f));
        lit_of(&f.id, whole, &Lit::Absent)
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
            Some(order) => self.ordered(
                kept.into_iter().map(|p| (p.clone(), p, None)),
                schema,
                order,
            ),
        }
    }

    /// The resource of the type `schema` anywhere in the index that `pred` holds of and comes
    /// first in `order`, or `None` when `pred` holds of none: [`Self::select`]'s query asked for
    /// one answer. When one exact run of a column answers the predicate and that column is what
    /// `order` (or no order) keeps the resources by, the first entry of the run is read, and the
    /// rest of the run is not: a query that ends at its first answer costs the pages up to it.
    pub fn first(
        &self,
        schema: &TypeSchema,
        pred: &Pred,
        order: Option<&Order>,
        evaluated: &Cell<u64>,
    ) -> Result<Option<String>, StoreError> {
        let Some(conj) = conjuncts(Some(pred))? else {
            return Ok(None);
        };
        let Some((at, access)) = choose(schema, &conj, true) else {
            return Ok(self
                .select(schema, pred, order, evaluated)?
                .into_iter()
                .next());
        };
        let [run] = access.as_slice() else {
            return Ok(self
                .select(schema, pred, order, evaluated)?
                .into_iter()
                .next());
        };
        let [one] = run.runs.as_slice() else {
            return Ok(self
                .select(schema, pred, order, evaluated)?
                .into_iter()
                .next());
        };
        let ascending = order.is_none_or(|o| {
            !o.descending
                && !o.absent_last
                && o.then.is_none()
                && schema
                    .column(&o.field)
                    .is_some_and(|column| column.name == run.column)
        });
        if !run.exact
            || !ascending
            || (run.repeats && !membership(conj.get(at).copied().unwrap_or(&Pred::Const(false))))
        {
            return Ok(self
                .select(schema, pred, order, evaluated)?
                .into_iter()
                .next());
        }
        let list = type_list(&schema.id, &run.column);
        let pages = self.snapshot.pages(&list).map_err(index_error)?;
        let (start, end) = bounds(&pages, one);
        let residual = rest(&conj, Some(at));
        for index in start..end {
            let Some((_, path)) = pages.entry(index) else {
                break;
            };
            if self.text(&path, TYPE).map_err(index_error)?.is_none() {
                continue;
            }
            if let Some(p) = &residual {
                evaluated.set(evaluated.get().saturating_add(1));
                if !self.holds(&path, schema, p)? {
                    continue;
                }
            }
            return Ok(Some(path.to_string()));
        }
        Ok(None)
    }

    /// How many items of the collection `field` of `resource` hold each value of the column
    /// `column` whose values are known (a toggle's, an enum's cases), as the index holds them:
    /// the difference of two positions in the column's list.
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
