//! Items keyed by an id they carry, with no stored order: [`Keyed`].
//!
//! A [`Collection`](crate::Collection) keeps an order key per item, which is
//! right for a list someone arranges. Many items are ordered by their own
//! content instead — clips by where they sit on a timeline, markers by
//! their time, tracks by their index — and an order key would be a second
//! truth that can disagree with it. `Keyed<T>` stores each item under the id
//! it carries ([`Identified::id`]), merges concurrent edits per field of each
//! item, and holds them in memory as a plain `Vec<T>` in id order (then
//! [`Identified::order`]), so code that works with a vector keeps working.
//!
//! Clones share the vector until one of them changes it: a copy of a state
//! holding items (a view keeping the state it was shown while the reader
//! follows) costs a pointer, and a change copies only the items of the field
//! it touched.
//!
//! A refresh keeps each item's id beside it, so bringing the items up to
//! date after a change asks only the items the change touched for their
//! ids: an id may be costly to make (formatted from a UUID), and finding and
//! sorting by asking every item again cost more than the change itself.
//!
//! A copy changed one item at a time ([`Items::find_mut`]) remembers which
//! items it handed out since it was copied, so the edits between it and the
//! items it was copied from ([`Model::diff`], what a surface that edited a
//! local copy submits) compare only those: an edit of one clip of 600 costs
//! one clip, not 600. Items added at the end ([`Items::push`]) and items
//! removed ([`Items::retain`], [`Items::take`], [`Items::remove`]) are
//! remembered the same way, so cutting a clip in two costs the two halves
//! and deleting one costs that one. Saving a copy over the items it was
//! copied from writes only the items changed, added and removed, and for a
//! [`Keyed`] those someone else touched meanwhile.

use std::any::Any;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as Atomic};
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, Value};

use crate::action::{ActionCx, Refusal};
use crate::collection::Id;
use crate::cow::Shared;
use crate::delta::{Delta, Touch};
use crate::model::LocatedAt as _;
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key};
use crate::path::{Edit, EditOp, Path, Segment};

/// An item of a [`Keyed`] or an [`Ordered`](crate::Ordered): what it is
/// stored under, and how items sort in memory beyond that.
pub trait Identified {
    /// The item's identity: the key it is stored under.
    fn id(&self) -> Id;

    /// How two items sort in memory after their ids; the default keeps id
    /// order. A timeline sorts its markers by time here.
    fn order(&self, other: &Self) -> Ordering {
        let _ = other;
        Ordering::Equal
    }
}

/// Items that carry their own ids, held as a plain `Vec<T>` in memory and
/// stored the way `S` says: [`Keyed`] under their ids alone, in their own
/// order; [`Ordered`](crate::Ordered) as someone arranged them.
pub struct Items<T, S> {
    /// Shared between clones until one of them changes the items.
    stock: Shared<Stock<T>>,
    storage: PhantomData<fn() -> S>,
}

/// The items, the id of each once a refresh has needed them, and what they
/// were copied from.
struct Stock<T> {
    items: Vec<T>,
    /// `ids[i]` is `items[i].id()`, except at the positions in `stale`. Kept
    /// by the refreshes that read it, dropped whenever all the items are
    /// handed out to change: whoever changes an item may change its id.
    ids: Option<Vec<Arc<str>>>,
    /// Positions, ascending, whose kept id may be out of date: items handed
    /// out one at a time to change ([`Items::find_mut`]).
    stale: Vec<usize>,
    /// Which state of the items this is, a mark no other has had: made anew
    /// whenever they are handed out to change, so items copied from these
    /// can tell whether these are still as they were copied.
    mark: u64,
    /// The items these were copied from, and which of them changed since.
    since: Option<Since>,
}

/// Items still as the ones they were copied from, except as noted: some
/// changed in place, some removed, some added at the end. Nothing else
/// changed: handing out all the items, or any change that moves one,
/// forgets the copy.
struct Since {
    /// The [`Stock::mark`] of the items copied.
    copied: u64,
    /// How many of the items copied these still hold: `items[..held]` are
    /// they, in the order copied. Every item past them was added at the end
    /// ([`Items::push`]) under an id no item held then.
    held: usize,
    /// Positions among the items copied, ascending, of items still held
    /// that were handed out one at a time to change ([`Items::find_mut`]).
    changed: Vec<usize>,
    /// Positions among the items copied, ascending, of items removed.
    removed: Vec<usize>,
    /// The id each item past `held` was added under, in order.
    added: Vec<Arc<str>>,
}

impl Since {
    fn new(copied: u64, held: usize) -> Self {
        Self {
            copied,
            held,
            changed: Vec::new(),
            removed: Vec::new(),
            added: Vec::new(),
        }
    }

    /// The item at `at` was handed out to change. An item added since is
    /// written whole whatever it holds, so only one copied is noted.
    fn handed_out(&mut self, at: usize) {
        if at < self.held {
            note(&mut self.changed, copied_at(&self.removed, at));
        }
    }

    /// The items at the positions `doomed` (ascending) are being removed.
    fn removing(&mut self, doomed: &[usize]) {
        let held = doomed.partition_point(|&at| at < self.held);
        let (copied, added) = doomed.split_at(held);
        // Where each was among the items copied, against the removals so far.
        let mut gone = 0usize;
        let mut origins = Vec::with_capacity(copied.len());
        for &at in copied {
            while self
                .removed
                .get(gone)
                .is_some_and(|&removed| removed <= at.saturating_add(gone))
            {
                gone = gone.saturating_add(1);
            }
            origins.push(at.saturating_add(gone));
        }
        self.changed
            .retain(|origin| origins.binary_search(origin).is_err());
        // The added ones were never copied: they are simply no longer added.
        let first_added = self.held;
        let added: Vec<usize> = added
            .iter()
            .map(|at| at.saturating_sub(first_added))
            .collect();
        remove_positions(&mut self.added, &added);
        self.held = self.held.saturating_sub(origins.len());
        self.removed.extend_from_slice(&origins);
        self.removed.sort_unstable();
    }
}

/// The position among the items copied of the item now at `at`, one of
/// those still held, given the positions `removed` (ascending) of the
/// copied items removed since.
fn copied_at(removed: &[usize], at: usize) -> usize {
    let mut origin = at;
    for &gone in removed {
        if gone > origin {
            break;
        }
        origin = origin.saturating_add(1);
    }
    origin
}

/// Remove the elements at the positions `doomed` (ascending) of `items`.
fn remove_positions<X>(items: &mut Vec<X>, doomed: &[usize]) {
    if doomed.is_empty() {
        return;
    }
    let mut at = 0usize;
    let mut next = doomed.iter().peekable();
    items.retain(|_| {
        let gone = next.next_if_eq(&&at).is_some();
        at = at.saturating_add(1);
        !gone
    });
}

/// What changed in items since they were copied ([`Stock::changed_since`]).
#[derive(Clone, Copy)]
struct Changes<'a> {
    /// Positions among the items copied, ascending, of items held that may
    /// have changed in place.
    changed: &'a [usize],
    /// Positions among the items copied, ascending, of items removed.
    removed: &'a [usize],
    /// How many of the items copied are held: the ones past them were added
    /// at the end, under the ids `added`.
    held: usize,
    added: &'a [Arc<str>],
}

impl Changes<'_> {
    /// No change to `count` items.
    fn none(count: usize) -> Self {
        Changes {
            changed: &[],
            removed: &[],
            held: count,
            added: &[],
        }
    }
}

/// A [`Stock::mark`] no other stock has had.
fn fresh_mark() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Atomic::Relaxed)
}

/// Add `at` to the ascending positions `positions`, once.
fn note(positions: &mut Vec<usize>, at: usize) {
    if let Err(slot) = positions.binary_search(&at) {
        positions.insert(slot, at);
    }
}

impl<T> Stock<T> {
    fn new(items: Vec<T>) -> Self {
        Self {
            items,
            ids: None,
            stale: Vec::new(),
            mark: fresh_mark(),
            since: None,
        }
    }

    /// All the items, to change: their ids, and what they were copied
    /// from, are no longer known.
    fn all_mut(&mut self) -> &mut Vec<T> {
        self.mark = fresh_mark();
        self.ids = None;
        self.stale.clear();
        self.since = None;
        &mut self.items
    }

    /// The item at `at`, to change; the copy remembers it changed.
    fn one_mut(&mut self, at: usize) -> Option<&mut T> {
        if at >= self.items.len() {
            return None;
        }
        self.mark = fresh_mark();
        if self.ids.is_some() {
            note(&mut self.stale, at);
        }
        if let Some(since) = &mut self.since {
            since.handed_out(at);
        }
        self.items.get_mut(at)
    }

    /// Forget the items at the positions `doomed` (ascending, each held)
    /// everywhere but in the items themselves; the copy remembers they
    /// left.
    fn forget(&mut self, doomed: &[usize]) {
        self.mark = fresh_mark();
        if let Some(since) = &mut self.since {
            since.removing(doomed);
        }
        if let Some(ids) = &mut self.ids {
            remove_positions(ids, doomed);
        }
        self.stale = self
            .stale
            .iter()
            .filter_map(|&at| match doomed.binary_search(&at) {
                Ok(_) => None,
                Err(before) => Some(at.saturating_sub(before)),
            })
            .collect();
    }

    /// Remove the items at the positions `doomed` (ascending, each held).
    fn remove_all(&mut self, doomed: &[usize]) {
        if doomed.is_empty() {
            return;
        }
        self.forget(doomed);
        remove_positions(&mut self.items, doomed);
    }

    /// Remove the item at `at`, which is held.
    fn remove_one(&mut self, at: usize) -> T {
        self.forget(&[at]);
        self.items.remove(at)
    }

    /// What changed since these items were copied from `origin`, when they
    /// were and `origin` is still as it was then.
    fn changed_since(&self, origin: &Self) -> Option<Changes<'_>> {
        let since = self.since.as_ref()?;
        let still_as_copied = since.copied == origin.mark
            && origin.items.len() == since.held.saturating_add(since.removed.len())
            && self.items.len() == since.held.saturating_add(since.added.len());
        still_as_copied.then_some(Changes {
            changed: since.changed.as_slice(),
            removed: since.removed.as_slice(),
            held: since.held,
            added: since.added.as_slice(),
        })
    }
}

impl<T: Identified> Stock<T> {
    /// The kept ids, brought up to date where an item handed out may have
    /// changed its id; taken, to be put back.
    fn take_ids(&mut self) -> Option<Vec<Arc<str>>> {
        let mut ids = self.ids.take()?;
        for at in self.stale.drain(..) {
            if let (Some(held), Some(item)) = (ids.get_mut(at), self.items.get(at)) {
                let id = item.id();
                if **held != *id.as_str() {
                    *held = Arc::from(id.as_str());
                }
            }
        }
        Some(ids)
    }
}

impl<T> Default for Stock<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<T: Clone> Clone for Stock<T> {
    /// The copy made to change items another holder still shares: the ids
    /// come along (a pointer each), and the copy remembers these items as
    /// what it was copied from.
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            ids: self.ids.clone(),
            stale: self.stale.clone(),
            mark: fresh_mark(),
            since: Some(Since::new(self.mark, self.items.len())),
        }
    }
}

/// The id of each of `items`, asked once.
fn ids_of<T: Identified>(items: &[T]) -> Vec<Arc<str>> {
    items
        .iter()
        .map(|item| Arc::from(item.id().as_str()))
        .collect()
}

/// How two items sort in memory, given their ids: by
/// [`Identified::order`], then by id.
fn in_order<T: Identified>(a: (&T, &str), b: (&T, &str)) -> Ordering {
    a.0.order(b.0).then_with(|| a.1.cmp(b.1))
}

/// Sort `items` and their `ids` alike, as a read from the document does: by
/// [`Identified::order`], then id. Stable, so this is a sort by id followed
/// by a stable sort by order; items already in place are not moved.
/// Whether any moved.
fn settle_with_ids<T: Identified>(items: &mut [T], ids: &mut [Arc<str>]) -> bool {
    let held = || items.iter().zip(ids.iter().map(|id| &**id));
    if held()
        .zip(held().skip(1))
        .all(|(a, b)| in_order(a, b).is_le())
    {
        return false;
    }
    let swaps = {
        let (held, held_ids): (&[T], &[Arc<str>]) = (items, ids);
        crate::sort::swaps_to_sort(held.len(), &mut |a, b| match (
            held.get(a).zip(held_ids.get(a)),
            held.get(b).zip(held_ids.get(b)),
        ) {
            (Some((a, a_id)), Some((b, b_id))) => in_order((a, &**a_id), (b, &**b_id)),
            _ => Ordering::Equal,
        })
    };
    for (a, b) in swaps {
        items.swap(a, b);
        ids.swap(a, b);
    }
    true
}

/// Items stored under the ids they carry. See the module docs.
pub type Keyed<T> = Items<T, ById>;

/// A `Vec<T>` stored under each item's id, as [`Keyed`] stores its items,
/// while the in-memory value stays a plain vector (`#[model(keyed)]`).
///
/// The serde form is still a list, so a settings file or an action argument
/// written by an older build reads unchanged. Only the document's shape
/// changes: a list's items move by position, where a keyed map keeps each
/// item under its own id, so two people changing different providers do not
/// overwrite one another.
pub struct KeyedVecOf<T>(PhantomData<T>);

/// A keyed vector, reached without its item type: reading its items, and
/// appending an item read through the item table.
pub(crate) trait VecItems {
    fn clear_items(&mut self);
    fn push_from_json(
        &mut self,
        item: &'static crate::table::ModelTable,
        value: &serde_json::Value,
    ) -> Result<(), ModelError>;
    fn push_loaded(
        &mut self,
        item: &'static crate::table::ModelTable,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
    ) -> Result<(), ModelError>;
}

impl<T: Model + 'static> VecItems for Vec<T> {
    fn clear_items(&mut self) {
        self.clear();
    }

    fn push_from_json(
        &mut self,
        item: &'static crate::table::ModelTable,
        value: &serde_json::Value,
    ) -> Result<(), ModelError> {
        let mut staged: Option<T> = None;
        (item.ops.from_json)(item, value, &mut staged)?;
        self.push(staged.ok_or_else(crate::table::mismatch)?);
        Ok(())
    }

    fn push_loaded(
        &mut self,
        item: &'static crate::table::ModelTable,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
    ) -> Result<(), ModelError> {
        let mut staged: Option<T> = None;
        (item.ops.load)(item, doc, value, id, &mut staged)?;
        self.push(staged.ok_or_else(crate::table::mismatch)?);
        Ok(())
    }
}

/// What [`KeyedVecOf<T>`]'s table reaches into a value with.
pub struct KeyedVecDesc {
    pub(crate) item: &'static crate::table::ModelTable,
    pub(crate) items: for<'a> fn(&'a dyn Any) -> Option<&'a dyn crate::collection::ItemList>,
    pub(crate) values: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn VecItems>,
    pub(crate) staged: (fn() -> Box<dyn Any>, crate::table::Unstage),
    pub(crate) replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
}

impl<T: Model + Identified + 'static> KeyedVecOf<T> {
    pub(crate) const DESC: KeyedVecDesc = KeyedVecDesc {
        item: T::TABLE,
        items: |value| Some(value.downcast_ref::<Vec<T>>()? as &dyn crate::collection::ItemList),
        values: |value| {
            let items = value.downcast_mut::<Option<Vec<T>>>()?;
            Some(items.get_or_insert_with(Vec::new) as &mut dyn VecItems)
        },
        staged: (
            crate::table::empty_of::<Vec<T>>,
            crate::table::unwrap_of::<Vec<T>>,
        ),
        replace: crate::table::replace_of::<Vec<T>>,
    };

    pub const TABLE: crate::table::ModelTable =
        crate::table::ModelTable::of_keyed_vec(&Self::DESC, T::CHECKS, T::OPS);
}

impl<T: Identified + 'static> crate::collection::ItemList for Vec<T> {
    fn count(&self) -> usize {
        self.len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn Any)> {
        let item = self.get(at)?;
        Some((item.id(), item as &dyn Any))
    }
}

fn keyed_vecs(table: &crate::table::ModelTable) -> Result<&'static KeyedVecDesc, ModelError> {
    table.keyed_vec().ok_or_else(crate::table::mismatch)
}

fn keyed_vec_values<'a>(
    table: &'a crate::table::ModelTable,
    value: &'a mut dyn Any,
) -> Result<&'a mut dyn VecItems, ModelError> {
    (keyed_vecs(table)?.values)(value).ok_or_else(crate::table::mismatch)
}

fn keyed_vec_json(table: &crate::table::ModelTable, value: &dyn Any) -> serde_json::Value {
    let Ok(desc) = keyed_vecs(table) else {
        return serde_json::Value::Null;
    };
    let Some(items) = (desc.items)(value) else {
        return serde_json::Value::Null;
    };
    serde_json::Value::Array(
        (0..items.count())
            .filter_map(|at| items.item(at))
            .map(|(_, item)| (desc.item.ops.to_json)(desc.item, item))
            .collect(),
    )
}

fn keyed_vec_from_json(
    table: &crate::table::ModelTable,
    value: &serde_json::Value,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    let desc = keyed_vecs(table)?;
    let values = keyed_vec_values(table, out)?;
    values.clear_items();
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                values.push_from_json(desc.item, item)?;
            }
            Ok(())
        }
        serde_json::Value::Null => Ok(()),
        _ => Err(ModelError::shape("a list")),
    }
}

fn keyed_vec_load(
    table: &crate::table::ModelTable,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    let desc = keyed_vecs(table)?;
    if !matches!(value, Value::Object(ObjType::Map)) {
        return Err(ModelError::shape("keyed items"));
    }
    let values = keyed_vec_values(table, out)?;
    values.clear_items();
    let mut keys: Vec<String> = doc.keys(id).collect();
    keys.sort_unstable();
    for key in keys {
        let Some((value, item)) = doc.get(id, key.as_str())? else {
            continue;
        };
        values.push_loaded(desc.item, doc, &value, &item)?;
    }
    Ok(())
}

fn keyed_vec_store(
    table: &crate::table::ModelTable,
    value: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
) -> Result<(), ModelError> {
    let desc = keyed_vecs(table)?;
    let items = (desc.items)(value).ok_or_else(crate::table::mismatch)?;
    store_by_id_in(desc.item, items, doc, slot)
}

fn keyed_vec_store_changed(
    table: &crate::table::ModelTable,
    value: &dyn Any,
    before: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
    dirty: &Delta,
) -> Result<(), ModelError> {
    let desc = keyed_vecs(table)?;
    let now = (desc.items)(value).ok_or_else(crate::table::mismatch)?;
    let was = (desc.items)(before).ok_or_else(crate::table::mismatch)?;
    let Some((Value::Object(ObjType::Map), map)) = slot.current(doc)? else {
        return store_by_id_in(desc.item, now, doc, slot);
    };
    let mut held = HashMap::with_capacity(was.count());
    for at in 0..was.count() {
        if let Some((id, _)) = was.item(at) {
            held.insert(id.as_str().to_owned(), at);
        }
    }
    let mut kept = HashSet::with_capacity(now.count());
    for at in 0..now.count() {
        if let Some((id, _)) = now.item(at) {
            kept.insert(id.as_str().to_owned());
        }
    }
    // An id the copy held and no longer does is this change's removal. An
    // id neither copy holds was added elsewhere meanwhile: keep it.
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|key| held.contains_key(key.as_str()) && !kept.contains(key.as_str()))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    for at in 0..now.count() {
        let Some((id, item)) = now.item(at) else {
            continue;
        };
        let key = id.as_str();
        match held.get(key).and_then(|at| was.item(*at)) {
            Some((_, was)) => write_item_changed(desc.item, (doc, &map, key), item, was, dirty)?,
            None => {
                (desc.item.ops.store)(desc.item, item, doc, Slot::Key(&map, key)).located(key)?;
            }
        }
    }
    Ok(())
}

fn keyed_vec_reload(
    table: &crate::table::ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
) -> Result<(), ModelError> {
    let desc = keyed_vecs(table)?;
    let mut staged = (desc.staged.0)();
    if let Some((value, id)) = slot {
        keyed_vec_load(table, doc, value, id, staged.as_mut())?;
    } else {
        let values = keyed_vec_values(table, staged.as_mut())?;
        values.clear_items();
    }
    (desc.replace)(target, staged.as_mut())
}

fn keyed_vec_check(
    table: &crate::table::ModelTable,
    value: &dyn Any,
) -> Result<(), crate::action::Refusal> {
    let Ok(desc) = keyed_vecs(table) else {
        return Ok(());
    };
    let Some(items) = (desc.items)(value) else {
        return Ok(());
    };
    for at in 0..items.count() {
        if let Some((_, item)) = items.item(at) {
            (desc.item.ops.check)(desc.item, item)?;
        }
    }
    Ok(())
}

fn keyed_vec_collect_text(
    table: &crate::table::ModelTable,
    value: &dyn Any,
    out: &mut Vec<String>,
) {
    let Ok(desc) = keyed_vecs(table) else {
        return;
    };
    let Some(items) = (desc.items)(value) else {
        return;
    };
    for at in 0..items.count() {
        if let Some((_, item)) = items.item(at) {
            (desc.item.ops.collect_text)(desc.item, item, out);
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "a model table's check has one shape, whether this type has an invariant or not"
)]
fn no_check(_: &crate::table::ModelTable, _: &dyn Any) -> Result<(), crate::action::Refusal> {
    Ok(())
}

fn no_coalesces(
    _: &crate::table::ModelTable,
    _: &[Segment],
    _: &serde_json::Value,
    _: &serde_json::Value,
) -> Option<bool> {
    None
}

fn no_continuable(
    _: &crate::table::ModelTable,
    _: &[Segment],
    _: &serde_json::Value,
) -> Option<bool> {
    None
}

fn no_describe(
    _: &crate::table::ModelTable,
    _: &[Segment],
    _: &serde_json::Value,
) -> Option<crate::path::OpDescription> {
    None
}

fn no_lands(
    _: &crate::table::ModelTable,
    _: &dyn Any,
    _: &[Segment],
    _: &serde_json::Value,
) -> Option<crate::resource::Place> {
    None
}

fn no_schemas(_: &crate::table::ModelTable, _: &str, _: &mut Vec<(String, serde_json::Value)>) {}

fn no_nested(_: &crate::table::ModelTable, _: &dyn Any) -> Vec<crate::items::ItemCollection> {
    Vec::new()
}

fn no_get_at<'a>(
    _: &crate::table::ModelTable,
    _: &'a dyn Any,
    _: &[Segment],
) -> Option<&'a dyn Any> {
    None
}

/// The functions of a `Vec<T>` stored under each item's id.
pub(crate) static KEYED_VEC_OPS: crate::table::ModelOps = crate::table::ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| keyed_vec_json(table, value),
    from_json: |table, value, out| keyed_vec_from_json(table, value, out),
    load: |table, doc, value, id, out| keyed_vec_load(table, doc, value, id, out),
    missing: |table, out| {
        let values = keyed_vec_values(table, out)?;
        values.clear_items();
        Ok(())
    },
    reload: |table, target, doc, slot| keyed_vec_reload(table, target, doc, slot),
    store: |table, value, doc, slot| keyed_vec_store(table, value, doc, slot),
    stores_as: |table, value, before| keyed_vec_json(table, value) == keyed_vec_json(table, before),
    store_changed: |table, value, before, doc, slot, dirty| {
        keyed_vec_store_changed(table, value, before, doc, slot, dirty)
    },
    refresh: |table, target, doc, value, id, _| {
        keyed_vec_reload(table, target, doc, Some((value, id)))
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, _| {
        let desc = keyed_vecs(table)?;
        crate::table::edit_whole(
            table,
            ((desc.staged.0)(), desc.staged.1),
            false,
            doc,
            slot,
            path,
            op,
        )
    },
    diff: |table, before, after, path, out| {
        let value = keyed_vec_json(table, after);
        if keyed_vec_json(table, before) != value {
            out.push(Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    },
    collect_text: |table, value, out| keyed_vec_collect_text(table, value, out),
    check: |table, value| {
        if table.checks {
            keyed_vec_check(table, value)
        } else {
            no_check(table, value)
        }
    },
    get_at: no_get_at,
    label: |_, _| None,
    nested_collections: no_nested,
    op_coalesces: no_coalesces,
    op_continuable: no_continuable,
    op_describe: no_describe,
    op_lands: no_lands,
    op_schemas: no_schemas,
};

/// How [`Items`] are stored: the parts of their [`Model`] that differ.
pub trait ItemStorage: 'static {
    /// Whether the items keep an order someone arranged.
    const ARRANGED: bool;
    /// What a stored value that is not of this storage is called in errors.
    const NAME: &'static str;

    fn load<T: Model + Identified>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError>;

    /// Write `items` into `slot`.
    fn store<T: Model + Identified>(
        items: &[T],
        doc: &mut Doc,
        slot: Slot<'_>,
    ) -> Result<(), ModelError>;

    /// Bring `items` up to date after `delta`; the default reads them again.
    #[inline(never)]
    fn refresh<T: Model + Identified>(
        items: &mut Vec<T>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if delta.touches(map) {
            *items = Self::load(doc, map)?;
        }
        Ok(())
    }

    /// [`refresh`](Self::refresh), given the id of each item when known
    /// (`ids[i]` is `items[i].id()`) and leaving them known when it can.
    /// The default forgets them and refreshes.
    ///
    /// # Errors
    /// Returns an item that does not read; `ids` is then unknown.
    fn refresh_with_ids<T: Model + Identified>(
        items: &mut Vec<T>,
        ids: &mut Option<Vec<Arc<str>>>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        *ids = None;
        Self::refresh(items, doc, map, delta)
    }

    /// [`Self::diff`] over items whose type a table describes, compiled
    /// once for every item type.
    #[doc(hidden)]
    const DIFF: ErasedDiff;

    /// The edits that turn `before` into `after`, below `path`.
    fn diff<T: Model + Identified, R>(
        before: &[T],
        after: &[T],
        path: &[Segment],
        out: &mut Vec<Edit<R>>,
    ) {
        let mut found = Vec::new();
        Self::DIFF(T::TABLE, (&Rows(before), &Rows(after)), path, &mut found);
        out.extend(found.into_iter().map(Edit::retyped));
    }
}

/// [`ItemStorage::diff`] with the item type erased: the edits turning the
/// items `before` into `after`, whose type the table describes, below a
/// path.
#[doc(hidden)]
pub type ErasedDiff = fn(
    &'static crate::table::ModelTable,
    (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    &[Segment],
    &mut Vec<Edit<()>>,
);

/// [`Keyed`] storage: each item under its id, in memory by id and then
/// [`Identified::order`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ById;

impl<T: Clone, S> Clone for Items<T, S> {
    /// Shares the items, however many: the first of the clones to change
    /// them copies them.
    fn clone(&self) -> Self {
        Self {
            stock: self.stock.share(),
            storage: PhantomData,
        }
    }
}

impl<T: std::fmt::Debug, S> std::fmt::Debug for Items<T, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.items().fmt(f)
    }
}

impl<T: PartialEq, S> PartialEq for Items<T, S> {
    /// Items two clones still share are the same and not compared one by
    /// one.
    fn eq(&self, other: &Self) -> bool {
        self.stock.same(&other.stock) || self.items() == other.items()
    }
}

impl<T, S> Default for Items<T, S> {
    fn default() -> Self {
        Self::from(Vec::new())
    }
}

impl<T, S> Items<T, S> {
    fn items(&self) -> &Vec<T> {
        &self.stock.get().items
    }

    /// The items, to change: copied first when a clone still shares them.
    /// Their ids are forgotten, as the change may change one.
    fn items_mut(&mut self) -> &mut Vec<T> {
        self.stock.make_mut().all_mut()
    }
}

impl<T, S> Deref for Items<T, S> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        self.items()
    }
}

impl<T, S> DerefMut for Items<T, S> {
    /// The items, to change: copied first when a clone still shares them.
    fn deref_mut(&mut self) -> &mut Vec<T> {
        self.items_mut()
    }
}

impl<T, S> From<Vec<T>> for Items<T, S> {
    fn from(items: Vec<T>) -> Self {
        Self {
            stock: Shared::new(Stock::new(items)),
            storage: PhantomData,
        }
    }
}

impl<T, S> FromIterator<T> for Items<T, S> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl<T, S> IntoIterator for Items<T, S> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.stock.into_inner().items.into_iter()
    }
}

impl<'a, T, S> IntoIterator for &'a Items<T, S> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items().iter()
    }
}

impl<'a, T, S> IntoIterator for &'a mut Items<T, S> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items_mut().iter_mut()
    }
}

impl<T: serde::Serialize, S> serde::Serialize for Items<T, S> {
    fn serialize<Ser: serde::Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        self.items().serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>, S> serde::Deserialize<'de> for Items<T, S> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

impl<T: crate::JsonSchema, S> crate::JsonSchema for Items<T, S> {
    fn json_schema() -> serde_json::Value {
        crate::schema::array(T::json_schema())
    }
}

impl<T: Identified, S> Items<T, S> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn position_of(&self, id: &Id) -> Option<usize> {
        let stock = self.stock.get();
        match &stock.ids {
            Some(ids) if stock.stale.is_empty() => {
                ids.iter().position(|held| **held == *id.as_str())
            }
            // An item handed out to change is asked: its id may have changed.
            Some(ids) => ids.iter().enumerate().position(|(at, held)| {
                if stock.stale.binary_search(&at).is_ok() {
                    stock.items.get(at).is_some_and(|item| &item.id() == id)
                } else {
                    **held == *id.as_str()
                }
            }),
            None => stock.items.iter().position(|item| &item.id() == id),
        }
    }

    pub fn find(&self, id: &Id) -> Option<&T> {
        self.items().get(self.position_of(id)?)
    }

    /// The item `id`, to change. The items are copied first when a clone
    /// still shares them, and only when the item is there; the copy
    /// remembers which items changed, so comparing it with what it was
    /// copied from compares only those. The ids asked to find it are kept,
    /// so finding the next one asks none.
    pub fn find_mut(&mut self, id: &Id) -> Option<&mut T> {
        let learned = self.stock.get().ids.is_none().then(|| ids_of(self.items()));
        let at = match &learned {
            Some(ids) => ids.iter().position(|held| **held == *id.as_str())?,
            None => self.position_of(id)?,
        };
        let stock = self.stock.make_mut();
        if learned.is_some() {
            stock.ids = learned;
        }
        stock.one_mut(at)
    }

    /// The item at position `at`, to change, remembered as
    /// [`find_mut`](Self::find_mut) remembers it: changing a few items
    /// picked by position costs those items, where changing them through
    /// `iter_mut` hands out every item.
    pub fn at_mut(&mut self, at: usize) -> Option<&mut T> {
        if at >= self.items().len() {
            return None;
        }
        self.stock.make_mut().one_mut(at)
    }

    /// Add `item` at the end, as `Vec::push` does. The items are copied
    /// first when a clone still shares them; the copy remembers the item
    /// came, so comparing it with what it was copied from, or saving it over
    /// that, visits the items changed and added rather than every item. An
    /// id the items already hold is no addition but items leaving and
    /// coming, which only comparing them all can tell: the copy forgets
    /// what it was copied from. The ids kept beside the items are kept.
    pub fn push(&mut self, item: T) {
        let stock = self.stock.make_mut();
        if stock.since.is_some() || stock.ids.is_some() {
            let id = item.id();
            if stock.since.is_some() {
                // The ids, learned when unknown, to tell an addition.
                let ids = stock.take_ids().unwrap_or_else(|| ids_of(&stock.items));
                if ids.iter().any(|held| **held == *id.as_str()) {
                    stock.since = None;
                }
                stock.ids = Some(ids);
            }
            let id: Arc<str> = Arc::from(id.as_str());
            if let Some(since) = &mut stock.since {
                since.added.push(Arc::clone(&id));
            }
            if let Some(ids) = &mut stock.ids {
                ids.push(id);
            }
        }
        stock.mark = fresh_mark();
        stock.items.push(item);
    }

    /// Remove the item `id`. The items are copied first when a clone still
    /// shares them, and only when the item is there; the copy remembers the
    /// item left, so comparing it with what it was copied from, or saving
    /// it over that, visits the items removed rather than every item.
    pub fn take(&mut self, id: &Id) -> Option<T> {
        let at = self.position_of(id)?;
        Some(self.stock.make_mut().remove_one(at))
    }

    /// Remove and return the item at `at`, as `Vec::remove` does, and
    /// remember it left as [`take`](Self::take) does.
    ///
    /// # Panics
    /// When `at` is past the items, as `Vec::remove` does.
    pub fn remove(&mut self, at: usize) -> T {
        let stock = self.stock.make_mut();
        if at >= stock.items.len() {
            // Out of range: `Vec::remove` says so.
            return stock.all_mut().remove(at);
        }
        stock.remove_one(at)
    }

    /// Keep only the items `keep` accepts, asking it once for each item in
    /// order, as `Vec::retain` does. Items it keeps all are not copied or
    /// changed; otherwise the copy remembers which items left, as
    /// [`take`](Self::take) does.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let doomed: Vec<usize> = self
            .items()
            .iter()
            .enumerate()
            .filter_map(|(at, item)| (!keep(item)).then_some(at))
            .collect();
        if !doomed.is_empty() {
            self.stock.make_mut().remove_all(&doomed);
        }
    }
}

impl<T: Identified> Keyed<T> {
    /// Sort as a read from the document does: by id, then [`Identified::order`].
    pub fn settle(&mut self) {
        let stock = self.stock.make_mut();
        let mut ids = stock.take_ids().unwrap_or_else(|| ids_of(&stock.items));
        if settle_with_ids(&mut stock.items, &mut ids) {
            // Moved: no longer as they were copied, position by position.
            stock.mark = fresh_mark();
            stock.since = None;
        }
        stock.ids = Some(ids);
    }
}

impl<R, T: Model + Identified, S> Path<R, Items<T, S>> {
    /// The item `id`, its own paths rooted here.
    #[must_use]
    pub fn item(&self, id: &Id) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Item(id.clone())))
    }

    #[must_use]
    pub fn item_path(&self, id: &Id) -> Path<R, T> {
        self.child(Segment::Item(id.clone()))
    }

    #[must_use]
    pub fn remove(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveItem { id: id.clone() })
    }
}

impl<R, T: Model + Identified> Path<R, Keyed<T>> {
    /// The edits that make the item `after` what it is: added when `before`
    /// is `None`, else changed field by field from `before`.
    #[must_use]
    pub fn upsert(&self, before: Option<&T>, after: &T) -> Vec<Edit<R>> {
        match before {
            Some(before) => self.item_path(&after.id()).edits(before, after),
            None => vec![self.insert(after)],
        }
    }

    /// Add `item` under its own id.
    #[must_use]
    pub fn insert(&self, item: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: Some(item.id()),
            at: crate::At::Index(0),
            value: item.to_json(),
        })
    }
}

impl ItemStorage for ById {
    const ARRANGED: bool = false;
    const NAME: &'static str = "keyed items";

    #[inline(never)]
    fn load<T: Model + Identified>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError> {
        let mut items = doc
            .keys(map)
            .map(|key| read_key::<T>(doc, map, &key))
            .collect::<Result<Vec<_>, _>>()?;
        crate::sort::sort_by(&mut items, T::order);
        Ok(items)
    }

    fn store<T: Model + Identified>(
        items: &[T],
        doc: &mut Doc,
        slot: Slot<'_>,
    ) -> Result<(), ModelError> {
        store_by_id_in(T::TABLE, &Rows(items), doc, slot)
    }

    #[inline(never)]
    fn refresh<T: Model + Identified>(
        items: &mut Vec<T>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        Self::refresh_with_ids(items, &mut None, doc, map, delta)
    }

    /// Finds each touched item by the id kept beside it and asks only the
    /// items it read for their ids; sorts only when they are out of place.
    #[inline(never)]
    fn refresh_with_ids<T: Model + Identified>(
        items: &mut Vec<T>,
        known: &mut Option<Vec<Arc<str>>>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let touched = delta.under(map);
        if touched.is_all() {
            *known = None;
            *items = Self::load(doc, map)?;
            *known = Some(ids_of(items));
            return Ok(());
        }
        let mut ids = known.take().unwrap_or_else(|| ids_of(items));
        let mut changed = false;
        for key in touched.keys() {
            let at = ids.iter().position(|id| **id == *key);
            match (doc.get(map, key)?, at) {
                (None, Some(at)) => {
                    items.remove(at);
                    ids.remove(at);
                    changed = true;
                }
                (None, None) => {}
                (Some(_), Some(at)) => {
                    if let (Some(item), Some(held)) = (items.get_mut(at), ids.get_mut(at)) {
                        refresh_key(item, doc, map, key, delta)?;
                        let id = item.id();
                        if **held != *id.as_str() {
                            *held = Arc::from(id.as_str());
                        }
                    }
                    changed = true;
                }
                (Some(_), None) => {
                    let item: T = read_key(doc, map, key)?;
                    ids.push(Arc::from(item.id().as_str()));
                    items.push(item);
                    changed = true;
                }
            }
        }
        if changed {
            settle_with_ids(items, &mut ids);
        }
        *known = Some(ids);
        Ok(())
    }

    const DIFF: ErasedDiff = diff_by_id_in;
}

/// [`ById::diff`] over items whose type `table` describes, compiled once for
/// every item type.
#[inline(never)]
fn diff_by_id_in(
    table: &'static crate::table::ModelTable,
    (before, after): (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    let after_ids: Vec<Id> = (0..after.count())
        .filter_map(|at| Some(after.item(at)?.0))
        .collect();
    let before_ids = removed_and_changed_in(table, (before, after), &after_ids, path, out);
    let held: HashSet<&Id> = before_ids.iter().collect();
    for (at, id) in after_ids.iter().enumerate() {
        if held.contains(id) {
            continue;
        }
        let Some((_, item)) = after.item(at) else {
            continue;
        };
        out.push(Edit::new(
            path.to_vec(),
            EditOp::InsertItem {
                id: Some(id.clone()),
                at: crate::At::Index(0),
                value: (table.ops.to_json)(table, item),
            },
        ));
    }
}

/// [`Keyed`] editing: an item lives under its id, in no entry of its own.
fn edit_by_id(
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
    edits: &crate::collection::EntryEdits<'_>,
) -> Result<(), ModelError> {
    match (path.split_first(), op) {
        (Some((Segment::Item(id), rest)), _) => {
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_none() {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            }
            (edits.item)(doc, Slot::Key(&map, id.as_str()), rest, op, cx).located(id.as_str())
        }
        (None, EditOp::InsertItem { value, .. }) => {
            let (id, item) = (edits.parse)(value)?;
            let id = id.unwrap_or_else(|| cx.id());
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_some() {
                return Err(ModelError::invalid(format!("an item `{id}` exists")));
            }
            item.store_to(doc, Slot::Key(&map, id.as_str()))
                .located(id.as_str())
        }
        (None, EditOp::RemoveItem { id }) => {
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_none() {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            }
            doc.delete(&map, id.as_str())?;
            Ok(())
        }
        (
            None,
            EditOp::UpsertItem {
                matching,
                value,
                id,
            },
        ) => {
            let map = slot.object(doc, ObjType::Map)?;
            let mut candidates = Vec::new();
            for key in doc.keys(&map).collect::<Vec<_>>() {
                if let Some((automerge::Value::Object(ObjType::Map), fields)) =
                    doc.get(&map, key.as_str())?
                {
                    candidates.push(crate::upsert::Candidate {
                        id: Id::from_raw(key),
                        fields,
                    });
                }
            }
            if let Some(item) = crate::upsert::matching_one(doc, &candidates, matching)? {
                for (key, field) in crate::upsert::changed_fields(doc, &item.fields, value)? {
                    (edits.item)(
                        doc,
                        Slot::Key(&map, item.id.as_str()),
                        &[Segment::Field(key)],
                        &EditOp::Set { value: field },
                        cx,
                    )
                    .located(item.id.as_str())?;
                }
                return Ok(());
            }
            let insert = crate::collection::upserted(doc, &map, matching, value, id.as_ref())?;
            edit_by_id(doc, slot, path, &insert, cx, edits)
        }
        _ => (edits.whole)(doc, slot, path, op),
    }
}

/// The edits removing what `after` lacks and changing what both hold, given
/// the id of each item of `after`, for items whose type `table` describes;
/// returns the id of each item of `before`. Compiled once for every item
/// type.
#[inline(never)]
pub(crate) fn removed_and_changed_in(
    table: &'static crate::table::ModelTable,
    (before, after): (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    after_ids: &[Id],
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) -> Vec<Id> {
    let mut found: HashMap<&str, usize> = HashMap::with_capacity(after_ids.len());
    for (at, id) in after_ids.iter().enumerate() {
        found.entry(id.as_str()).or_insert(at);
    }
    let mut before_ids = Vec::with_capacity(before.count());
    for at in 0..before.count() {
        let Some((id, item)) = before.item(at) else {
            continue;
        };
        match found.get(id.as_str()).and_then(|at| after.item(*at)) {
            Some((_, next)) => {
                let mut at = path.to_vec();
                at.push(Segment::Item(id.clone()));
                (table.ops.diff)(table, item, next, &at, out);
            }
            None => out.push(Edit::new(
                path.to_vec(),
                EditOp::RemoveItem { id: id.clone() },
            )),
        }
        before_ids.push(id);
    }
    before_ids
}

/// Items as the type-erased list the functions compiled once read.
struct Rows<'a, T>(&'a [T]);

impl<T: Identified + 'static> crate::collection::ItemList for Rows<'_, T> {
    fn count(&self) -> usize {
        self.0.len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn std::any::Any)> {
        let item = self.0.get(at)?;
        Some((item.id(), item as &dyn std::any::Any))
    }
}

/// What a copy's [`Changes`] come to, item by item: what comparing every
/// item would find, found without visiting the others.
struct Plan {
    /// The items copied that changed in place or left, in the order copied.
    steps: Vec<Step>,
    /// The id of each item added at the end, in order.
    added: Vec<Id>,
}

/// An item copied that changed in place, or left.
struct Step {
    /// Where it was among the items copied.
    was: usize,
    /// Where it is now; `None` when it left.
    now: Option<usize>,
    id: Id,
}

/// The [`Plan`] of the changes `changes` between `before` and `after`;
/// `None` when an item changed its id (or one added since did), or one was
/// added under the id of one removed: then items left and came, which only
/// comparing them all can tell.
#[inline(never)]
fn plan(
    before: &dyn crate::collection::ItemList,
    after: &dyn crate::collection::ItemList,
    changes: Changes<'_>,
) -> Option<Plan> {
    let mut steps = Vec::with_capacity(changes.changed.len().saturating_add(changes.removed.len()));
    let (mut in_place, mut left) = (changes.changed.iter(), changes.removed.iter());
    let (mut next_changed, mut next_removed) = (in_place.next(), left.next());
    let mut gone = 0usize;
    loop {
        match (next_changed, next_removed) {
            (None, None) => break,
            (Some(&was), next) if next.is_none_or(|&removed| was < removed) => {
                let now = was.checked_sub(gone)?;
                let (id, _) = before.item(was)?;
                if after.item(now)?.0 != id {
                    return None;
                }
                steps.push(Step {
                    was,
                    now: Some(now),
                    id,
                });
                next_changed = in_place.next();
            }
            (_, Some(&was)) => {
                steps.push(Step {
                    was,
                    now: None,
                    id: before.item(was)?.0,
                });
                gone = gone.saturating_add(1);
                next_removed = left.next();
            }
            (Some(_), None) => return None,
        }
    }
    if after.count() < changes.held {
        return None;
    }
    let mut added = Vec::with_capacity(changes.added.len());
    for (at, came) in (changes.held..after.count()).zip(changes.added) {
        let (id, _) = after.item(at)?;
        if *id.as_str() != **came {
            return None;
        }
        added.push(id);
    }
    if !added.is_empty() && !changes.removed.is_empty() {
        let left: HashSet<&Id> = steps
            .iter()
            .filter(|step| step.now.is_none())
            .map(|step| &step.id)
            .collect();
        if added.iter().any(|id| left.contains(id)) {
            return None;
        }
    }
    Some(Plan { steps, added })
}

/// The edits between `before` and `after`, which holds the same items in
/// the same order except as `changes` says: each item copied that changed
/// in place, field by field, or left, then each added one, in the order
/// [`ItemStorage::diff`] would list them (an [`Ordered`](crate::Ordered)
/// item inserted at its position, `arranged`). `false`, with nothing
/// written, when only comparing every item can tell ([`plan`]). For items
/// whose type `table` describes, compiled once for every item type.
#[inline(never)]
fn diff_changed_in(
    table: &'static crate::table::ModelTable,
    (before, after): (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    changes: Changes<'_>,
    arranged: bool,
    (path, out): (&[Segment], &mut Vec<Edit<()>>),
) -> bool {
    let Some(plan) = plan(before, after, changes) else {
        return false;
    };
    let mut at = Vec::with_capacity(path.len().saturating_add(1));
    at.extend_from_slice(path);
    for step in plan.steps {
        match step.now {
            Some(now) => {
                if let (Some((_, was)), Some((_, now))) = (before.item(step.was), after.item(now)) {
                    at.push(Segment::Item(step.id));
                    (table.ops.diff)(table, was, now, &at, out);
                    at.pop();
                }
            }
            None => out.push(Edit::new(path.to_vec(), EditOp::RemoveItem { id: step.id })),
        }
    }
    for (offset, id) in plan.added.into_iter().enumerate() {
        let index = changes.held.saturating_add(offset);
        let Some((_, item)) = after.item(index) else {
            continue;
        };
        out.push(Edit::new(
            path.to_vec(),
            EditOp::InsertItem {
                id: Some(id),
                at: crate::At::Index(if arranged { index } else { 0 }),
                value: (table.ops.to_json)(table, item),
            },
        ));
    }
    true
}

/// [`Model::diff`] of [`Items`] whose type `table` describes, stored as
/// `diff` and `arranged` say: from `changes` when the copy's changes tell,
/// comparing every item otherwise. Compiled once for every item type.
#[inline(never)]
fn diff_items_in(
    (table, diff, arranged): (&'static crate::table::ModelTable, ErasedDiff, bool),
    lists: (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    changes: Option<Changes<'_>>,
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    if let Some(changes) = changes
        && diff_changed_in(table, lists, changes, arranged, (path, &mut *out))
    {
        return;
    }
    diff(table, lists, path, out);
}

/// Finds an item by id.
type PositionOf<'a> = &'a dyn Fn(&Id) -> Option<usize>;

/// Writes items whole into a slot.
type StoreWhole<'a> = &'a dyn Fn(&mut Doc, Slot<'_>) -> Result<(), ModelError>;

/// [`Model::store_changed`] of [`Items`] `now` over the items `was` they
/// were copied from, changed as `changes` says when known, whose type
/// `table` describes and stored arranged or not: only what changed when the
/// changes tell, the whole items through `store` otherwise. `position_of`
/// finds an item of `now` by id. Compiled once for every item type.
#[inline(never)]
fn store_items_changed_in(
    (table, arranged): (&'static crate::table::ModelTable, bool),
    lists: (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    changes: Option<Changes<'_>>,
    (position_of, store): (PositionOf<'_>, StoreWhole<'_>),
    (doc, slot, dirty): (&mut Doc, Slot<'_>, &Delta),
) -> Result<(), ModelError> {
    match changes {
        Some(changes) if !arranged => {
            store_by_id_changed_in(table, position_of, lists, changes, (doc, slot, dirty))
        }
        Some(changes) => {
            if store_arranged_changed_in(table, lists, changes, (doc, slot, dirty))? {
                return Ok(());
            }
            store(doc, slot)
        }
        None => store(doc, slot),
    }
}

/// Hand each item of the JSON form of [`Items`] to `read`: a list of
/// items, or `null` for none.
/// Compiled once for every item type.
///
/// # Errors
/// Returns that `value` is no list of items, or the first item that does
/// not read.
#[inline(never)]
fn each_item_json(
    value: &serde_json::Value,
    read: &mut dyn FnMut(&serde_json::Value) -> Result<(), ModelError>,
) -> Result<(), ModelError> {
    match value {
        serde_json::Value::Array(items) => items.iter().try_for_each(read),
        serde_json::Value::Null => Ok(()),
        _ => Err(ModelError::shape("a list of items")),
    }
}

/// Write `item`, which `held` was, under `key` of `map`, as
/// [`write_key_changed`](crate::model::write_key_changed) writes it, for
/// items whose type `table` describes.
fn write_item_changed(
    table: &'static crate::table::ModelTable,
    (doc, map, key): (&mut Doc, &ObjId, &str),
    item: &dyn std::any::Any,
    held: &dyn std::any::Any,
    dirty: &Delta,
) -> Result<(), ModelError> {
    match dirty.key(map, key) {
        Touch::Written => (table.ops.store)(table, item, doc, Slot::Key(map, key)).located(key),
        Touch::Untouched if (table.ops.stores_as)(table, item, held) => Ok(()),
        Touch::Untouched | Touch::Below => {
            (table.ops.store_changed)(table, item, held, doc, Slot::Key(map, key), dirty)
                .located(key)
        }
    }
}

/// [`Model::store_changed`] of [`Keyed`] items `now` over the items `was`
/// they were copied from, changed, removed and added to as `changes` says:
/// the operations [`ById::store`] writes, found without visiting the other
/// items. The map holds each of `was` under its id as a store left it,
/// except where `dirty` says someone else wrote since; an item no change
/// touched is the very item `was` holds there, and storing it writes
/// nothing. `position_of` finds an item by id.
/// For items whose type `table` describes, compiled once for every item
/// type.
#[inline(never)]
fn store_by_id_changed_in(
    table: &'static crate::table::ModelTable,
    position_of: &dyn Fn(&Id) -> Option<usize>,
    (now, was): (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    changes: Changes<'_>,
    (doc, slot, dirty): (&mut Doc, Slot<'_>, &Delta),
) -> Result<(), ModelError> {
    // A slot holding no map yet is written whole, as is a map someone else
    // changed otherwise than item by item, or items that left and came.
    let Some((Value::Object(ObjType::Map), map)) = slot.current(doc)? else {
        return store_by_id_in(table, now, doc, slot);
    };
    let touched = dirty.under(&map);
    let plan = match plan(was, now, changes) {
        Some(plan) if !touched.is_all() => plan,
        _ => return store_by_id_in(table, now, doc, slot),
    };
    // What to write, by position now and then: the items changed and
    // added, and those someone else touched; what to delete: the keys of
    // the items removed, and a key someone else wrote no item is under.
    let mut writes: Vec<(usize, Option<usize>)> = Vec::new();
    let mut stale: Vec<&str> = Vec::new();
    for step in &plan.steps {
        match step.now {
            Some(now) => writes.push((now, Some(step.was))),
            None => stale.push(step.id.as_str()),
        }
    }
    writes.extend((changes.held..now.count()).map(|at| (at, None)));
    for key in touched.keys() {
        match position_of(&Id::from_raw(key)) {
            Some(at) if at < changes.held => {
                writes.push((at, Some(copied_at(changes.removed, at))));
            }
            Some(at) => writes.push((at, None)),
            None => stale.push(key),
        }
    }
    // In the order `store` writes: deletes by key, then items in order. It
    // deletes only what the map holds.
    stale.compact_sort();
    stale.dedup();
    for key in stale {
        if doc.get(&map, key)?.is_some() {
            doc.delete(&map, key)?;
        }
    }
    writes.compact_sort_by_key(|(now, _)| *now);
    writes.dedup_by_key(|(now, _)| *now);
    for (at, then) in writes {
        let Some((id, item)) = now.item(at) else {
            continue;
        };
        let key = id.as_str();
        match then.and_then(|then| was.item(then)) {
            Some((_, held)) => write_item_changed(table, (doc, &map, key), item, held, dirty)?,
            None => (table.ops.store)(table, item, doc, Slot::Key(&map, key)).located(key)?,
        }
    }
    Ok(())
}

/// [`ById::store`] over items whose type `table` describes, compiled once
/// for every item type.
#[inline(never)]
fn store_by_id_in(
    table: &'static crate::table::ModelTable,
    items: &dyn crate::collection::ItemList,
    doc: &mut Doc,
    slot: Slot<'_>,
) -> Result<(), ModelError> {
    let map = slot.object(doc, ObjType::Map)?;
    let ids: Vec<Id> = (0..items.count())
        .filter_map(|at| Some(items.item(at)?.0))
        .collect();
    let keys: HashSet<&str> = ids.iter().map(Id::as_str).collect();
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|key| !keys.contains(key.as_str()))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    for at in 0..items.count() {
        let Some((id, item)) = items.item(at) else {
            continue;
        };
        (table.ops.store)(table, item, doc, Slot::Key(&map, id.as_str())).located(id.as_str())?;
    }
    Ok(())
}

/// [`Model::store_changed`] of [`Ordered`](crate::Ordered) items `now` over
/// the items `was` they were copied from, changed, removed and added to as
/// `changes` says: the operations [`Arranged::store`](crate::Arranged)
/// writes, found without visiting the other items. `false`, with nothing
/// written, when only the whole write can say.
///
/// The map holds an entry `{ k, v }` for each of `was` as a store left it,
/// unless `dirty` says someone else wrote there since; then it is written
/// whole. A store leaves order keys rising strictly from item to item, so a
/// whole write keeps every order key of the items still held (they rise in
/// the order kept), deletes the entries of the items removed, writes the
/// items changed field by field and mints keys after the last item held for
/// the items added; nothing else.
/// For items whose type `table` describes, compiled once for every item
/// type.
#[inline(never)]
fn store_arranged_changed_in(
    table: &'static crate::table::ModelTable,
    (now, was): (
        &dyn crate::collection::ItemList,
        &dyn crate::collection::ItemList,
    ),
    changes: Changes<'_>,
    (doc, slot, dirty): (&mut Doc, Slot<'_>, &Delta),
) -> Result<bool, ModelError> {
    use crate::collection::{ORDER, VALUE, put_entry};
    let Some((Value::Object(ObjType::Map), map)) = slot.current(doc)? else {
        return Ok(false);
    };
    if dirty.touches(&map) {
        return Ok(false);
    }
    let Some(plan) = plan(was, now, changes) else {
        return Ok(false);
    };
    // The key the last item held is stored under, read before any write.
    let last_key = match changes.held.checked_sub(1).and_then(|at| now.item(at)) {
        Some((id, _)) if !plan.added.is_empty() => {
            let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? else {
                return Ok(false);
            };
            Some(
                crate::model::read_key::<crate::Atom>(doc, &entry, ORDER)
                    .located(id.as_str())?
                    .0,
            )
        }
        _ => None,
    };
    // Deletes first, in stored order: the order the items were copied in.
    for step in &plan.steps {
        if step.now.is_none() {
            doc.delete(&map, step.id.as_str())?;
        }
    }
    for step in &plan.steps {
        if let (Some(at), Some((_, held))) = (step.now, was.item(step.was))
            && let Some((_, item)) = now.item(at)
        {
            let id = step.id.as_str();
            let entry = Slot::Key(&map, id).object(doc, ObjType::Map)?;
            write_item_changed(table, (doc, &entry, VALUE), item, held, dirty).located(id)?;
        }
    }
    let keys = crate::order::sequence(last_key.as_deref(), None, plan.added.len());
    for ((offset, id), key) in plan.added.iter().enumerate().zip(keys) {
        let Some((_, item)) = now.item(changes.held.saturating_add(offset)) else {
            continue;
        };
        put_entry(doc, &map, id, key, false, &|doc, slot| {
            (table.ops.store)(table, item, doc, slot)
        })?;
    }
    Ok(true)
}

impl<T: Model + Identified, S> crate::items::ItemSource for Items<T, S> {
    fn item_collection(&self, field: &'static str) -> crate::items::ItemCollection {
        crate::collection::listed_items(T::TABLE, self, field)
    }
    fn item_presence(&self) -> usize {
        usize::from(!self.items().is_empty())
    }
    fn label_of_item(&self, id: &Id) -> Option<String> {
        let item = self.find(id)?;
        Some(crate::collection::label_of(T::TABLE, id, item))
    }
    fn item_texts(&self, field: &'static str, found: &mut Vec<crate::items::ItemText>) {
        crate::collection::item_texts(T::TABLE, self, field, found);
    }
    fn nested_items(&self, field: &'static str, out: &mut Vec<crate::items::ItemCollection>) {
        crate::collection::nested_items(T::TABLE, self, field, out);
    }
}

impl<T: Model + Identified, S: 'static> crate::table::PublishedItems for Items<T, S> {
    const ITEM_TABLE: &'static crate::table::ModelTable = T::TABLE;
}

impl<T: Model + Identified, S> crate::collection::ItemList for Items<T, S> {
    fn count(&self) -> usize {
        self.items().len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn std::any::Any)> {
        let item = self.items().get(at)?;
        Some((item.id(), item as &dyn std::any::Any))
    }
}

impl<T: Model + Identified, S: ItemStorage> Model for Items<T, S> {
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema()})
    }
    const SHAPE: crate::Shape = if S::ARRANGED {
        crate::Shape::Collection(&T::SHAPE)
    } else {
        crate::Shape::Keyed(&T::SHAPE)
    };
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();

    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.items().iter().map(Model::to_json).collect())
    }

    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let mut items = Vec::new();
        each_item_json(value, &mut |item| {
            items.push(T::from_json(item)?);
            Ok(())
        })?;
        Ok(Self::from(items))
    }

    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape(S::NAME));
        }
        S::load(doc, id).map(Self::from)
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        S::store(self.items(), doc, slot)
    }

    /// Items two clones still share are the same items, and store as they
    /// are: saving a record whose items no change touched visits none.
    fn stores_as(&self, before: &Self) -> bool {
        self.stock.same(&before.stock)
    }

    /// A copy changed one item at a time, removed from and added to writes
    /// only those items, and for a [`Keyed`] the ones someone else touched;
    /// anything else is written as [`store`](Model::store) writes it.
    #[inline(never)]
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let (now, was) = (self.stock.get(), before.stock.get());
        let changes = if self.stock.same(&before.stock) {
            Some(Changes::none(was.items.len()))
        } else {
            now.changed_since(was)
        };
        store_items_changed_in(
            (T::TABLE, S::ARRANGED),
            (&Rows(&now.items), &Rows(&was.items)),
            changes,
            (&|id| self.position_of(id), &|doc, slot| {
                self.store(doc, slot)
            }),
            (doc, slot, dirty),
        )
    }

    #[inline(never)]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        // Untouched items stay shared with any clone of them.
        if !delta.touches(id) {
            return Ok(());
        }
        let stock = self.stock.make_mut();
        let mut ids = stock.take_ids();
        let refreshed = S::refresh_with_ids(stock.all_mut(), &mut ids, doc, id, delta);
        stock.ids = ids;
        refreshed
    }
    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let edits = crate::collection::EntryEdits {
            item: &T::edit,
            parse: &|value| {
                let item = T::from_json(value)?;
                item.check().map_err(ModelError::refused)?;
                Ok((
                    Some(item.id()),
                    Box::new(item) as Box<dyn crate::model::Stores + '_>,
                ))
            },
            whole: &edit_whole::<Self>,
            order_only: false,
        };
        if S::ARRANGED {
            crate::collection::edit_entries(doc, slot, path, op, cx, &edits)
        } else {
            edit_by_id(doc, slot, path, op, cx, &edits)
        }
    }

    /// Items two clones still share are not compared; a copy changed one
    /// item at a time compares only the items it changed.
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<Edit<R>>) {
        if self.stock.same(&after.stock) {
            return;
        }
        let mut found = Vec::new();
        diff_items_in(
            (T::TABLE, S::DIFF, S::ARRANGED),
            (&Rows(self.items()), &Rows(after.items())),
            after.stock.get().changed_since(self.stock.get()),
            path,
            &mut found,
        );
        out.extend(found.into_iter().map(Edit::retyped));
    }

    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.items().iter().try_for_each(Model::check)?;
        }
        Ok(())
    }

    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        match path.split_first() {
            None => Some(self),
            Some((Segment::Item(id), rest)) => self.find(id)?.get_at(rest),
            Some(_) => None,
        }
    }

    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }

    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_continuable(rest, op),
            _ => None,
        }
    }

    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_describe(rest, op),
            _ => None,
        }
    }

    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        match path.split_first() {
            Some((Segment::Item(id), rest)) if T::OPS => self.find(id)?.op_lands(rest, op),
            _ => None,
        }
    }

    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        if T::OPS {
            T::op_schemas(&format!("{prefix}*."), out);
        }
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        for item in self.items() {
            item.collect_text(out);
        }
    }
}
