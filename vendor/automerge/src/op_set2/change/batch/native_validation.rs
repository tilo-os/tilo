//! Read-only semantic preflight for the exact automerge 0.12 batch input.
//!
//! Ported from the qOS 0.8 patch. `validate_incoming` simulates what
//! `Automerge::apply_changes_batch_log_patches` would do with the resident
//! queue plus `incoming` (hash dedup, duplicate actor/seq and author checks,
//! `ChangeQueue::pop_topo_sorted_ready`) without mutating anything, and then
//! checks every change that would become ready against the conditions under
//! which `BatchApply::apply` and `ChangeGraph::add_change` return an error or
//! panic (see the coverage table in `resource_limits/apply.rs`). It returns
//! the predicted `{ integrated, orphaned }` or a closed refusal; the caller
//! must run batch apply on the same exclusive document and changes right
//! after it returns successfully.
//!
//! Byte/column validity belongs to incoming parsing and the borrowed
//! preflight. Every validator allocation is charged to the caller's work
//! allowance before it happens: fixed-size tables up front, per-change clocks
//! as they are created.
//!
//! Causal accessibility (an operation may only reference operations of its
//! ancestors) is decided with per-change vector clocks of `(seq, max_op)` per
//! actor instead of the 0.8 per-change ancestor walk, which was quadratic in
//! the resident history. Under 0.12's own invariant that each actor's changes
//! cover increasing operation-counter ranges (which `ChangeGraph::opid_to_hash`
//! and `Clock::covers` already rely on, and which this validator enforces for
//! every change it admits), "op `(actor, counter)` is in an ancestor" is
//! exactly "`counter <= clock[actor].max_op`".
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use smol_str::SmolStr;

use crate::columnar::Key;
use crate::op_set2::op::Op;
use crate::op_set2::types::{Action, KeyRef, ScalarValue as NativeScalar};
use crate::resource_limits::apply::budget::{self, product, sum, Charge};
use crate::resource_limits::apply::preflight;
use crate::resource_limits::{NativeMutationError, NativeResourceError};
use crate::storage::ChangeOp as StoredOp;
use crate::types::{ObjId, OpId};
use crate::{ActorId, Automerge, Change, ChangeHash, ObjType, ScalarValue};

type Result<T = ()> = std::result::Result<T, NativeMutationError>;

fn invalid<T>() -> Result<T> {
    Err(NativeMutationError::InvalidOperation)
}
fn unaccounted<T>() -> Result<T> {
    Err(NativeResourceError::Unaccounted.into())
}

/// Batch outcome predicted by validation over the same queue and input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeValidated {
    /// Changes that batch apply will integrate into the graph.
    pub(crate) integrated: usize,
    /// Whether any new or previously queued change will remain queued.
    pub(crate) orphaned: bool,
}

/// An operation id in validation actor space: resident actors keep their
/// resident index, actors introduced by ready `seq == 1` changes follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Id {
    counter: u32,
    actor: u32,
}

enum Prop {
    Map(SmolStr),
    Elem(Option<Id>),
}

#[derive(PartialEq, Eq)]
enum PropView<'a> {
    Map(&'a str),
    Elem(Option<Id>),
}

/// The facts later rows need about an admitted incoming operation.
struct Row {
    change: u32,
    obj: Option<Id>,
    key: Prop,
    insert: bool,
    action: u64,
    mark_begin: bool,
    counter: Option<i64>,
}

enum Located<'r, 'a> {
    Source(Op<'a>),
    Incoming(&'r Row),
}

fn action_code(action: Action) -> u64 {
    match action {
        Action::MakeMap => 0,
        Action::Set => 1,
        Action::MakeList => 2,
        Action::Delete => 3,
        Action::MakeText => 4,
        Action::Increment => 5,
        Action::MakeTable => 6,
        Action::Mark => 7,
    }
}

fn source_id(id: OpId) -> Result<Id> {
    let counter = u32::try_from(id.counter()).map_err(|_| NativeResourceError::Unaccounted)?;
    let actor = u32::try_from(id.actor()).map_err(|_| NativeResourceError::Unaccounted)?;
    if counter == 0 {
        return unaccounted();
    }
    Ok(Id { counter, actor })
}

fn source_object(obj: ObjId) -> Result<Option<Id>> {
    if obj.is_root() {
        Ok(None)
    } else {
        source_id(obj.0).map(Some)
    }
}

impl<'a> Located<'_, 'a> {
    fn object(&self) -> Result<Option<Id>> {
        match self {
            Self::Source(op) => source_object(op.obj),
            Self::Incoming(row) => Ok(row.obj),
        }
    }
    fn insert(&self) -> bool {
        match self {
            Self::Source(op) => op.insert,
            Self::Incoming(row) => row.insert,
        }
    }
    fn action(&self) -> u64 {
        match self {
            Self::Source(op) => action_code(op.action),
            Self::Incoming(row) => row.action,
        }
    }
    fn property(&self, id: Id) -> Result<PropView<'_>> {
        if self.insert() {
            return Ok(PropView::Elem(Some(id)));
        }
        match self {
            Self::Source(op) => match &op.key {
                KeyRef::Map(key) => Ok(PropView::Map(key.as_ref())),
                KeyRef::Seq(elem) if elem.is_head() => Ok(PropView::Elem(None)),
                KeyRef::Seq(elem) => Ok(PropView::Elem(Some(source_id(elem.0)?))),
            },
            Self::Incoming(row) => Ok(match &row.key {
                Prop::Map(key) => PropView::Map(key.as_str()),
                Prop::Elem(elem) => PropView::Elem(*elem),
            }),
        }
    }
    fn is_mark_begin(&self) -> bool {
        match self {
            Self::Source(op) => op.action == Action::Mark && op.mark_name.is_some(),
            Self::Incoming(row) => row.mark_begin,
        }
    }
    fn counter(&self) -> Option<i64> {
        match self {
            Self::Source(op) => match op.value {
                NativeScalar::Counter(value) => Some(value),
                _ => None,
            },
            Self::Incoming(row) => row.counter,
        }
    }
}

/// `(seq, max_op)` per validation actor for the ancestors of one change.
struct Clock {
    seq: Vec<u32>,
    max_op: Vec<u32>,
    max: u32,
}

struct Clocks {
    width: usize,
}
impl Clocks {
    fn bytes(&self) -> Result<usize> {
        Ok(product(self.width, 8)?)
    }
    fn zero(&self, charge: &mut Charge<'_>) -> Result<Clock> {
        charge.grow(self.bytes()?)?;
        let mut seq = Vec::new();
        let mut max_op = Vec::new();
        seq.try_reserve_exact(self.width).map_err(|_| NativeResourceError::WorkBytes)?;
        max_op.try_reserve_exact(self.width).map_err(|_| NativeResourceError::WorkBytes)?;
        seq.resize(self.width, 0);
        max_op.resize(self.width, 0);
        Ok(Clock { seq, max_op, max: 0 })
    }
    fn copy(&self, source: &Clock, charge: &mut Charge<'_>) -> Result<Clock> {
        let mut clock = self.zero(charge)?;
        clock.seq.copy_from_slice(&source.seq);
        clock.max_op.copy_from_slice(&source.max_op);
        clock.max = source.max;
        Ok(clock)
    }
    fn free(&self, clock: Clock, charge: &mut Charge<'_>) {
        drop(clock);
        charge.shrink(self.width * 8);
    }
}
fn merge(into: &mut Clock, from: &Clock) {
    for (a, b) in into.seq.iter_mut().zip(&from.seq) {
        *a = (*a).max(*b);
    }
    for (a, b) in into.max_op.iter_mut().zip(&from.max_op) {
        *a = (*a).max(*b);
    }
    into.max = into.max.max(from.max);
}

struct Validation<'a> {
    doc: &'a Automerge,
    /// Validation actor index to identity.
    actors: Vec<&'a ActorId>,
    new_actors: HashMap<&'a ActorId, u32>,
    rows: HashMap<Id, Row>,
    /// (counter operation, increment operation, increment value).
    increments: Vec<(Id, Id, i64)>,
    /// Mark-order walk scratch: element -> child on the walked path.
    walk_end: HashMap<Option<Id>, Option<Id>>,
    walk_begin: HashMap<Option<Id>, Option<Id>>,
    /// Remaining sequence-tree steps for all mark-order walks.
    walk_steps: usize,
}

impl<'a> Validation<'a> {
    fn actor_index(&self, actor: &ActorId) -> Option<u32> {
        match self.doc.ops.actors.binary_search(actor) {
            Ok(index) => u32::try_from(index).ok(),
            Err(_) => self.new_actors.get(actor).copied(),
        }
    }

    /// Total order of operation ids: counter, then actor identity.
    fn cmp(&self, a: Id, b: Id) -> Ordering {
        a.counter.cmp(&b.counter).then_with(|| {
            self.actors[a.actor as usize].cmp(self.actors[b.actor as usize])
        })
    }

    fn locate(&self, id: Id) -> Result<Located<'_, 'a>> {
        if let Some(row) = self.rows.get(&id) {
            return Ok(Located::Incoming(row));
        }
        if id.counter == 0 || id.actor as usize >= self.doc.ops.actors.len() {
            return invalid();
        }
        let doc: &'a Automerge = self.doc;
        doc.ops
            .find_op_by_id_and_vis_fast(&OpId::new(id.counter as u64, id.actor as usize))
            .map(|(op, _)| Located::Source(op))
            .ok_or(NativeMutationError::InvalidOperation)
    }

    /// Locate an operation `row` may reference: an earlier operation of the
    /// same change, or an operation of one of its ancestors (`clock`).
    fn accessible(&self, id: Id, row: Id, change: u32, clock: &Clock) -> Result<Located<'_, 'a>> {
        let operation = self.locate(id)?;
        if let Located::Incoming(previous) = &operation {
            if previous.change == change {
                return if id.counter < row.counter {
                    Ok(operation)
                } else {
                    invalid()
                };
            }
        }
        if id.counter > clock.max_op[id.actor as usize] {
            return invalid();
        }
        Ok(operation)
    }

    fn local_id(map: &[u32], id: OpId) -> Result<Id> {
        let actor = *map.get(id.actor()).ok_or(NativeMutationError::InvalidOperation)?;
        let counter = u32::try_from(id.counter()).map_err(|_| NativeMutationError::InvalidOperation)?;
        if counter == 0 {
            return invalid();
        }
        Ok(Id { counter, actor })
    }

    fn row(
        &mut self,
        change: u32,
        id: Id,
        op: StoredOp,
        map: &[u32],
        clock: &Clock,
        charge: &mut Charge<'_>,
    ) -> Result {
        if self.rows.contains_key(&id) {
            return invalid();
        }
        let object = if op.obj.is_root() {
            None
        } else {
            Some(Self::local_id(map, op.obj.0)?)
        };
        let kind = match object {
            None => ObjType::Map,
            Some(obj) => match self.accessible(obj, id, change, clock)?.action() {
                0 => ObjType::Map,
                2 => ObjType::List,
                4 => ObjType::Text,
                6 => ObjType::Table,
                _ => return invalid(),
            },
        };
        let key = match op.key {
            Key::Prop(key) => Prop::Map(key),
            Key::Elem(elem) if elem.is_head() => Prop::Elem(None),
            Key::Elem(elem) => Prop::Elem(Some(Self::local_id(map, elem.0)?)),
        };
        match (&key, kind) {
            (Prop::Map(_), ObjType::Map | ObjType::Table) => {
                if op.insert || op.action == 7 {
                    return invalid();
                }
            }
            (Prop::Elem(anchor), ObjType::List | ObjType::Text) => {
                if op.insert {
                    if !matches!(op.action, 0 | 1 | 2 | 4 | 6 | 7) || !op.pred.is_empty() {
                        return invalid();
                    }
                } else if anchor.is_none() || op.action == 7 {
                    return invalid();
                }
                if let Some(anchor) = anchor {
                    let previous = self.accessible(*anchor, id, change, clock)?;
                    if !previous.insert() || previous.object()? != object {
                        return invalid();
                    }
                }
            }
            _ => return invalid(),
        }
        // A delete without predecessors deletes nothing and has no document
        // encoding (deletes are stored as successors): the saved document
        // would no longer reproduce this change.
        if op.action == 3 && op.pred.is_empty() {
            return invalid();
        }
        if op.action == 7 {
            if kind != ObjType::Text || !op.insert || !op.pred.is_empty() {
                return invalid();
            }
            if op.mark_name.is_none() {
                let counter = id
                    .counter
                    .checked_sub(1)
                    .filter(|counter| *counter > 0)
                    .ok_or(NativeMutationError::InvalidOperation)?;
                let begin = Id {
                    counter,
                    actor: id.actor,
                };
                {
                    let begin = self.accessible(begin, id, change, clock)?;
                    if !begin.is_mark_begin() || begin.object()? != object {
                        return invalid();
                    }
                }
                // 0.12 refuses to load a document whose mark end precedes its
                // begin in sequence order.
                let anchor = match &key {
                    Prop::Elem(anchor) => *anchor,
                    Prop::Map(_) => return invalid(),
                };
                if !self.end_follows_begin(begin, id, anchor, charge)? {
                    return invalid();
                }
            }
        }
        let increment = match (&op.val, op.action) {
            (ScalarValue::Int(value), 5) => Some(*value),
            (ScalarValue::Uint(value), 5) => Some(*value as i64),
            (_, 5) => return invalid(),
            _ => None,
        };
        let key_view = match &key {
            Prop::Map(key) => PropView::Map(key.as_str()),
            Prop::Elem(elem) => PropView::Elem(*elem),
        };
        let mut last: Option<Id> = None;
        let mut counter_seen = false;
        for predecessor in &op.pred {
            let pred = Self::local_id(map, *predecessor)?;
            if last.is_some_and(|previous| self.cmp(previous, pred) != Ordering::Less) {
                return invalid();
            }
            last = Some(pred);
            let is_counter = {
                let previous = self.accessible(pred, id, change, clock)?;
                if previous.object()? != object
                    || previous.property(pred)? != key_view
                    || !matches!(previous.action(), 0 | 1 | 2 | 4 | 6)
                {
                    return invalid();
                }
                previous.counter().is_some()
            };
            if let (Some(value), true) = (increment, is_counter) {
                // Native increment resolves a conflicted property when at
                // least one predecessor is a counter. Other visible values
                // are ordinary overwritten predecessors, not a reason to
                // reject valid history.
                counter_seen = true;
                // Capacity is reserved for one entry per incoming predecessor.
                if self.increments.len() == self.increments.capacity() {
                    return unaccounted();
                }
                self.increments.push((pred, id, value));
            }
        }
        if increment.is_some() && !counter_seen {
            return invalid();
        }
        let counter = match &op.val {
            ScalarValue::Counter(value) => Some(value.start),
            _ => None,
        };
        let row = Row {
            change,
            obj: object,
            key,
            insert: op.insert,
            action: op.action,
            mark_begin: op.action == 7 && op.mark_name.is_some(),
            counter,
        };
        if self.rows.len() == self.rows.capacity() {
            return unaccounted();
        }
        self.rows.insert(id, row);
        Ok(())
    }

    /// The anchor of a sequence element (`None` is HEAD).
    fn parent(&self, id: Id) -> Result<Option<Id>> {
        match self.locate(id)? {
            Located::Incoming(row) if row.insert => match row.key {
                Prop::Elem(anchor) => Ok(anchor),
                Prop::Map(_) => invalid(),
            },
            Located::Source(op) if op.insert => match &op.key {
                KeyRef::Seq(elem) if elem.is_head() => Ok(None),
                KeyRef::Seq(elem) => Ok(Some(source_id(elem.0)?)),
                KeyRef::Map(_) => invalid(),
            },
            _ => invalid(),
        }
    }

    fn walk_insert(
        map: &mut HashMap<Option<Id>, Option<Id>>,
        node: Option<Id>,
        child: Option<Id>,
        charge: &mut Charge<'_>,
    ) -> Result {
        if map.len() == map.capacity() {
            let target = map.capacity().max(8).checked_mul(2).ok_or(NativeResourceError::WorkBytes)?;
            let grown = budget::hash::<(Option<Id>, Option<Id>)>(target)?;
            // Old and new tables coexist while rehashing.
            let old = budget::hash::<(Option<Id>, Option<Id>)>(map.capacity())?;
            charge.grow(sum(grown, old)?)?;
            map.try_reserve(target - map.len())
                .map_err(|_| NativeResourceError::WorkBytes)?;
            charge.shrink(old);
        }
        map.insert(node, child);
        Ok(())
    }

    /// Whether the new mark end `end` (anchored after `anchor`) follows its
    /// begin in the sequence. Automerge sequences order elements as the
    /// preorder of the anchor tree with siblings in descending id order, and
    /// adding elements never reorders existing ones, so this is decided from
    /// the ancestor paths of both elements: `end` follows `begin` iff `begin`
    /// is an ancestor of `end`, or at their lowest common ancestor the child
    /// towards `begin` has the greater id. Both paths are walked alternately
    /// until they meet; all walks share a step budget proportional to the
    /// document and input (refusal with `WorkBytes` beyond it).
    fn end_follows_begin(
        &mut self,
        begin: Id,
        end: Id,
        anchor: Option<Id>,
        charge: &mut Charge<'_>,
    ) -> Result<bool> {
        let mut ends = std::mem::take(&mut self.walk_end);
        let mut begins = std::mem::take(&mut self.walk_begin);
        ends.clear();
        begins.clear();
        let result = (|| -> Result<bool> {
            Self::walk_insert(&mut ends, Some(end), None, charge)?;
            Self::walk_insert(&mut begins, Some(begin), None, charge)?;
            if ends.contains_key(&Some(begin)) {
                return Ok(true);
            }
            let mut end_at = (Some(end), anchor);
            let mut begin_at = (Some(begin), Some(begin));
            let mut begin_parent = Some(self.parent(begin)?);
            let mut end_done = false;
            let mut begin_done = false;
            loop {
                if end_done && begin_done {
                    return invalid();
                }
                if !end_done {
                    self.walk_steps = self
                        .walk_steps
                        .checked_sub(1)
                        .ok_or(NativeResourceError::WorkBytes)?;
                    let (child, node) = end_at;
                    if let Some(on_begin) = begins.get(&node) {
                        return Ok(self.begin_first(node, *on_begin, child, begin));
                    }
                    Self::walk_insert(&mut ends, node, child, charge)?;
                    match node {
                        None => end_done = true,
                        Some(id) => end_at = (Some(id), self.parent(id)?),
                    }
                }
                if !begin_done {
                    self.walk_steps = self
                        .walk_steps
                        .checked_sub(1)
                        .ok_or(NativeResourceError::WorkBytes)?;
                    let Some(parent) = begin_parent else {
                        begin_done = true;
                        continue;
                    };
                    let child = begin_at.1;
                    if let Some(on_end) = ends.get(&parent) {
                        return Ok(self.begin_first(parent, child, *on_end, begin));
                    }
                    Self::walk_insert(&mut begins, parent, child, charge)?;
                    begin_at = (child, parent);
                    begin_parent = match parent {
                        None => None,
                        Some(id) => Some(self.parent(id)?),
                    };
                }
            }
        })();
        self.walk_end = ends;
        self.walk_begin = begins;
        result
    }

    /// At common ancestor `node`, reached from `begin` through `on_begin`
    /// and from the end through `on_end` (`None`: the path starts there).
    fn begin_first(&self, node: Option<Id>, on_begin: Option<Id>, on_end: Option<Id>, begin: Id) -> bool {
        if node == Some(begin) {
            // `begin` is an ancestor of the end.
            return true;
        }
        match (on_begin, on_end) {
            (Some(b), Some(e)) => self.cmp(b, e) == Ordering::Greater,
            _ => false,
        }
    }

    // A history is refused when a counter's complete merged value, before or
    // after this batch, is not representable. Intermediate native paths (patch
    // deltas, hydration of a counter hidden by a scalar conflict, clock-scoped
    // readback) wrap instead, so they impose no further condition.
    fn counters(&mut self) -> Result {
        let mut increments = std::mem::take(&mut self.increments);
        increments.sort_unstable_by(|a, b| {
            a.0.counter
                .cmp(&b.0.counter)
                .then(a.0.actor.cmp(&b.0.actor))
                .then_with(|| self.cmp(a.1, b.1))
        });
        let mut start = 0;
        while start < increments.len() {
            let counter_id = increments[start].0;
            let mut end = start;
            while end < increments.len() && increments[end].0 == counter_id {
                end += 1;
            }
            let values = &increments[start..end];
            start = end;
            let counter = self.locate(counter_id)?;
            let base = counter
                .counter()
                .ok_or(NativeMutationError::InvalidOperation)?;
            let mut old_sum = 0i64;
            let mut merged = 0i64;
            let mut incoming = values.iter().peekable();
            if let Located::Source(op) = &counter {
                for (old_id, increment) in op.succ_inc() {
                    let old_id = source_id(old_id)?;
                    while incoming
                        .peek()
                        .is_some_and(|(_, id, _)| self.cmp(*id, old_id) == Ordering::Less)
                    {
                        let (_, _, value) = incoming.next().ok_or(NativeResourceError::Unaccounted)?;
                        merged = merged
                            .checked_add(*value)
                            .ok_or(NativeMutationError::InvalidOperation)?;
                    }
                    if incoming.peek().is_some_and(|(_, id, _)| *id == old_id) {
                        return invalid();
                    }
                    if let Some(value) = increment {
                        old_sum = old_sum
                            .checked_add(value)
                            .ok_or(NativeMutationError::InvalidOperation)?;
                        merged = merged
                            .checked_add(value)
                            .ok_or(NativeMutationError::InvalidOperation)?;
                    }
                }
            }
            for (_, _, value) in incoming {
                merged = merged
                    .checked_add(*value)
                    .ok_or(NativeMutationError::InvalidOperation)?;
            }
            base.checked_add(old_sum)
                .ok_or(NativeMutationError::InvalidOperation)?;
            base.checked_add(merged)
                .ok_or(NativeMutationError::InvalidOperation)?;
        }
        Ok(())
    }
}

fn reserve_hash<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V>, n: usize) -> Result {
    map.try_reserve(n)
        .map_err(|_| NativeResourceError::WorkBytes.into())
}
fn reserve_set<K: std::hash::Hash + Eq>(set: &mut HashSet<K>, n: usize) -> Result {
    set.try_reserve(n)
        .map_err(|_| NativeResourceError::WorkBytes.into())
}
fn reserve_vec<T>(vec: &mut Vec<T>, n: usize) -> Result {
    vec.try_reserve_exact(n)
        .map_err(|_| NativeResourceError::WorkBytes.into())
}

/// Allocation-free counts over the queue plus the incoming changes.
#[derive(Default)]
struct Counts {
    changes: usize,
    ops: usize,
    preds: usize,
    deps: usize,
    max_deps: usize,
    max_actors: usize,
    key_bytes: usize,
    op_decode: usize,
}
impl Counts {
    fn read<'c>(changes: impl Iterator<Item = &'c Change>) -> Result<Self> {
        let mut counts = Self::default();
        for change in changes {
            let dims = preflight::change_dims(change.raw_bytes())?;
            if dims.ops != change.len() {
                return unaccounted();
            }
            counts.changes = sum(counts.changes, 1)?;
            counts.ops = sum(counts.ops, dims.ops)?;
            counts.preds = sum(counts.preds, dims.preds)?;
            counts.deps = sum(counts.deps, change.deps().len())?;
            counts.max_deps = counts.max_deps.max(change.deps().len());
            counts.max_actors = counts.max_actors.max(dims.actors);
            counts.key_bytes = sum(counts.key_bytes, dims.key_bytes)?;
            counts.op_decode = counts.op_decode.max(dims.op_decode_bytes()?);
        }
        Ok(counts)
    }
}

/// Upper bound on the transient allocation of `canonical(change)`: the
/// decoded `ExpandedChange` (actor table, a doubling `Vec<legacy::Op>` with
/// realloc overlap, per-op predecessor Vecs, cloned actor ids, owned keys,
/// values and mark names, one decoded storage op at a time) and the rebuilt
/// change (actor index, column encoders and output bytes).
fn canonical_bound(change: &Change) -> std::result::Result<usize, NativeResourceError> {
    let dims = preflight::change_dims(change.raw_bytes())?;
    let actor_heap = change.actors().map(|a| a.to_bytes().len()).max().unwrap_or(0);
    let refs = sum(product(dims.ops, 2)?, dims.preds)?;
    let mut total = 0usize;
    for bytes in [
        budget::hash::<(usize, ActorId)>(dims.actors)?,
        product(dims.actors, actor_heap)?,
        product(budget::vec::<crate::legacy::Op>(dims.ops)?, 3)?,
        product(budget::vec::<crate::legacy::OpId>(dims.preds)?, 3)?,
        product(refs, actor_heap)?,
        product(sum(sum(dims.key_bytes, dims.value_bytes)?, dims.mark_bytes)?, 3)?,
        product(dims.ops, 96)?,
        dims.op_decode_bytes()?,
        // Rebuild: actor index/table, encoders and output.
        product(sum(product(dims.ops, 3)?, dims.preds)?, 64)?,
        product(change.raw_bytes().len(), 4)?,
        product(sum(dims.ops, dims.preds)?, 64)?,
        product(sum(sum(dims.key_bytes, dims.value_bytes)?, dims.mark_bytes)?, 2)?,
        8192,
    ] {
        total = sum(total, bytes)?;
    }
    Ok(total)
}

/// Whether `change` is canonically encoded: a document save stores changes
/// as columns and later reconstructs them canonically, so a change whose raw
/// bytes differ from its canonical encoding would get a different hash after
/// save and load (a permanently unloadable document).
fn canonical(change: &Change) -> bool {
    let expanded = change.decode();
    let rebuilt = crate::storage::Change::builder()
        .with_actor(expanded.actor_id)
        .with_extra_bytes(expanded.extra_bytes)
        .with_seq(expanded.seq)
        .with_dependencies(expanded.deps)
        .with_timestamp(expanded.time)
        .with_start_op(expanded.start_op)
        .with_message(expanded.message)
        .build(expanded.operations.iter());
    matches!(rebuilt, Ok(rebuilt) if rebuilt.bytes() == change.raw_bytes())
}

/// Fixed slack for the transient search stacks of resident operation-id
/// lookups (hexane `FindByValue` starts with 16 frames).
const LOOKUP_SLACK: usize = 64 * 1024;

pub(crate) fn validate_incoming(
    doc: &Automerge,
    incoming: &[Change],
    charge: &mut Charge<'_>,
) -> Result<NativeValidated> {
    let resident = doc.ops.actors.len();
    // 0.12 batch apply panics (debug) on unused actors and indexes the change
    // graph by the op-set actor table.
    if resident != doc.change_graph.num_actors()
        || doc.change_graph.actor_ids().count() != resident
        || doc.ops.actors.windows(2).any(|pair| pair[0] >= pair[1])
        || resident > u32::MAX as usize
    {
        return unaccounted();
    }
    let counts = Counts::read(doc.queue.iter().chain(incoming.iter()))?;
    let graph = doc.change_graph.len();
    let n = counts.changes;

    // Everything below except per-change clocks is covered here, before any
    // of it is allocated.
    let mut fixed = 0usize;
    for bytes in [
        budget::vec::<&Change>(n)?,                     // pool
        budget::hash::<(ChangeHash, u32)>(n)?,          // index
        budget::hash::<(&ActorId, u64)>(n)?,            // sequences
        budget::hash::<&ActorId>(n)?,                   // authors
        budget::vec::<u32>(n)?,                         // unsatisfied
        budget::vec::<(ChangeHash, u32)>(counts.deps)?, // waiting edges
        product(budget::vec::<u32>(n)?, 2)?,            // ready VecDeque
        budget::vec::<u32>(n)?,                         // order
        budget::hash::<(&ActorId, u32)>(n)?,            // new actors
        budget::vec::<&ActorId>(sum(resident, n)?)?,    // actor table
        budget::vec::<u32>(n)?,                         // remaining dependents
        budget::vec::<Option<Clock>>(n)?,               // live clocks
        budget::hash::<(Id, Row)>(counts.ops)?,         // rows
        budget::vec::<(Id, Id, i64)>(counts.preds)?,    // increments
        budget::vec::<u32>(counts.max_actors)?,         // local actor map
        budget::vec::<ChangeHash>(counts.max_deps)?,    // resident deps
        budget::vec::<ChangeHash>(counts.max_deps)?,    // memoized resident deps
        product(counts.key_bytes, 2)?,                  // retained SmolStr keys
        counts.op_decode,                               // one decoded op
        LOOKUP_SLACK,
        // One resident clock computation at a time: SeqClock + Clock of the
        // resident actor width and two BTreeSet traversals of at most every
        // resident change (plus the head set).
        product(resident, 8)?,
        product(budget::btree_set(sum(graph, counts.max_deps)?)?, 2)?,
    ] {
        fixed = sum(fixed, bytes)?;
    }
    charge.grow(fixed)?;

    // Pool, dedup and pre-checks, in the exact order of
    // `apply_changes_batch_log_patches`: the queue first, then the input.
    let mut pool: Vec<&Change> = Vec::new();
    reserve_vec(&mut pool, n)?;
    let mut index: HashMap<ChangeHash, u32> = HashMap::new();
    reserve_hash(&mut index, n)?;
    let mut sequences: HashSet<(&ActorId, u64)> = HashSet::new();
    reserve_set(&mut sequences, n)?;
    let mut authors: HashSet<&ActorId> = HashSet::new();
    reserve_set(&mut authors, n)?;
    let queued = doc.queue.iter().count();
    for (position, change) in doc.queue.iter().chain(incoming.iter()).enumerate() {
        let is_queued = position < queued;
        let hash = change.hash();
        if doc.change_graph.has_change(&hash) || index.contains_key(&hash) {
            if is_queued {
                // 0.12 would import an integrated or repeated queued change a
                // second time.
                return unaccounted();
            }
            continue;
        }
        // 0.12 prunes the queue (`remove_actor_branch_from`) before returning
        // DuplicateSeqNumber for a resident duplicate; refusing here means
        // that mutation never runs. Queued duplicates are refused as in 0.8.
        if doc.has_actor_seq(change) || !sequences.insert((change.actor_id(), change.seq())) {
            return Err(NativeMutationError::DuplicateSequence);
        }
        if change.author().is_some() {
            let conflict = doc.get_author_for_actor(change.actor_id()).is_some()
                || authors.contains(change.actor_id());
            if conflict && !is_queued {
                return invalid();
            }
            authors.insert(change.actor_id());
        }
        index.insert(hash, u32::try_from(pool.len()).map_err(|_| NativeResourceError::WorkBytes)?);
        pool.push(change);
    }
    drop(authors);
    drop(sequences);

    // Kahn's algorithm with the exact visiting order of
    // `ChangeQueue::pop_topo_sorted_ready`.
    let mut unsatisfied: Vec<u32> = Vec::new();
    reserve_vec(&mut unsatisfied, pool.len())?;
    unsatisfied.resize(pool.len(), 0);
    let mut waiting: Vec<(ChangeHash, u32)> = Vec::new();
    reserve_vec(&mut waiting, counts.deps)?;
    for (i, change) in pool.iter().enumerate() {
        for dep in change.deps() {
            if !doc.change_graph.has_change(dep) {
                unsatisfied[i] = unsatisfied[i]
                    .checked_add(1)
                    .ok_or(NativeResourceError::WorkBytes)?;
                waiting.push((*dep, i as u32));
            }
        }
    }
    // Stable by construction: children of one hash keep ascending order.
    waiting.sort_unstable();
    let mut ready: VecDeque<u32> = VecDeque::new();
    ready
        .try_reserve_exact(pool.len())
        .map_err(|_| NativeResourceError::WorkBytes)?;
    for (i, count) in unsatisfied.iter().enumerate() {
        if *count == 0 {
            ready.push_back(i as u32);
        }
    }
    let mut order: Vec<u32> = Vec::new();
    reserve_vec(&mut order, pool.len())?;
    while let Some(i) = ready.pop_front() {
        order.push(i);
        let hash = pool[i as usize].hash();
        let first = waiting.partition_point(|(h, _)| *h < hash);
        for (h, child) in &waiting[first..] {
            if *h != hash {
                break;
            }
            let count = &mut unsatisfied[*child as usize];
            *count = count.checked_sub(1).ok_or(NativeResourceError::Unaccounted)?;
            if *count == 0 {
                if ready.len() == ready.capacity() {
                    return unaccounted();
                }
                ready.push_back(*child);
            }
        }
    }
    drop(ready);
    drop(waiting);
    drop(unsatisfied);
    let integrated = order.len();
    let orphaned = integrated < pool.len();
    if sum(graph, integrated)? >= u32::MAX as usize {
        return Err(NativeResourceError::WorkBytes.into());
    }

    // Actor catalog: `insert_new_actors` adds the actor of every ready
    // `seq == 1` change; `import_ops` unwraps every change actor lookup.
    let mut validation = Validation {
        doc,
        actors: Vec::new(),
        new_actors: HashMap::new(),
        rows: HashMap::new(),
        increments: Vec::new(),
        walk_end: HashMap::new(),
        walk_begin: HashMap::new(),
        walk_steps: sum(product(sum(doc.ops.len(), counts.ops)?, 16)?, 1 << 20)?,
    };
    reserve_vec(&mut validation.actors, sum(resident, n)?)?;
    validation.actors.extend(doc.ops.actors.iter());
    reserve_hash(&mut validation.new_actors, n)?;
    for i in &order {
        let change = pool[*i as usize];
        if change.seq() == 1 && doc.ops.actors.binary_search(change.actor_id()).is_err() {
            let next = u32::try_from(validation.actors.len()).map_err(|_| NativeResourceError::WorkBytes)?;
            if let std::collections::hash_map::Entry::Vacant(entry) =
                validation.new_actors.entry(change.actor_id())
            {
                entry.insert(next);
                validation.actors.push(change.actor_id());
            }
        }
    }
    let clocks = Clocks {
        width: validation.actors.len(),
    };
    reserve_hash(&mut validation.rows, counts.ops)?;
    reserve_vec(&mut validation.increments, counts.preds)?;

    // Dependents among ready changes, so a clock is dropped (or moved into
    // its last child) as soon as nothing needs it.
    let mut remaining: Vec<u32> = Vec::new();
    reserve_vec(&mut remaining, pool.len())?;
    remaining.resize(pool.len(), 0);
    for i in &order {
        for dep in pool[*i as usize].deps() {
            if let Some(j) = index.get(dep) {
                remaining[*j as usize] = remaining[*j as usize]
                    .checked_add(1)
                    .ok_or(NativeResourceError::WorkBytes)?;
            }
        }
    }
    let mut live: Vec<Option<Clock>> = Vec::new();
    reserve_vec(&mut live, pool.len())?;
    live.resize_with(pool.len(), || None);
    let mut map: Vec<u32> = Vec::new();
    reserve_vec(&mut map, counts.max_actors)?;
    let mut resident_deps: Vec<ChangeHash> = Vec::new();
    reserve_vec(&mut resident_deps, counts.max_deps)?;
    let mut memo_deps: Vec<ChangeHash> = Vec::new();
    reserve_vec(&mut memo_deps, counts.max_deps)?;
    let mut memo: Option<Clock> = None;

    let result = (|| -> Result {
        for i in &order {
            let i = *i as usize;
            let change = pool[i];
            let actor = validation
                .actor_index(change.actor_id())
                .ok_or(NativeMutationError::InvalidOperation)?;
            map.clear();
            for a in change.actors() {
                if map.len() == map.capacity() {
                    return unaccounted();
                }
                map.push(validation.actor_index(a).ok_or(NativeMutationError::InvalidOperation)?);
            }
            // ChangeGraph::add_changes asserts an author only on seq 1.
            if change.author().is_some() && change.seq() != 1 {
                return invalid();
            }
            let seq = u32::try_from(change.seq()).map_err(|_| NativeResourceError::WorkBytes)?;
            let start = change.start_op().get();
            let end = start
                .checked_add(u64::try_from(change.len()).map_err(|_| NativeResourceError::WorkBytes)?)
                .and_then(|end| end.checked_sub(1))
                .ok_or(NativeResourceError::WorkBytes)?;
            let start = u32::try_from(start).map_err(|_| NativeResourceError::WorkBytes)?;
            let end = u32::try_from(end).map_err(|_| NativeResourceError::WorkBytes)?;

            // Clock of the ancestors.
            let mut clock: Option<Clock> = None;
            resident_deps.clear();
            for dep in change.deps() {
                if doc.change_graph.has_change(dep) {
                    if resident_deps.len() == resident_deps.capacity() {
                        return unaccounted();
                    }
                    resident_deps.push(*dep);
                    continue;
                }
                let j = *index.get(dep).ok_or(NativeResourceError::Unaccounted)? as usize;
                remaining[j] = remaining[j].checked_sub(1).ok_or(NativeResourceError::Unaccounted)?;
                let last = remaining[j] == 0;
                match (&mut clock, last) {
                    (None, true) => clock = live[j].take(),
                    (None, false) => {
                        let source = live[j].as_ref().ok_or(NativeResourceError::Unaccounted)?;
                        clock = Some(clocks.copy(source, charge)?);
                    }
                    (Some(current), _) => {
                        let source = live[j].as_ref().ok_or(NativeResourceError::Unaccounted)?;
                        merge(current, source);
                        if last {
                            if let Some(done) = live[j].take() {
                                clocks.free(done, charge);
                            }
                        }
                    }
                }
                if clock.is_none() {
                    return unaccounted();
                }
            }
            if !resident_deps.is_empty() {
                if memo.is_none() || memo_deps != resident_deps {
                    if memo.is_none() {
                        memo = Some(clocks.zero(charge)?);
                    }
                    let target = memo.as_mut().ok_or(NativeResourceError::Unaccounted)?;
                    let seqs = doc.change_graph.seq_clock_for_heads(&resident_deps);
                    let maxima = doc.change_graph.clock_at(&resident_deps);
                    if seqs.0.len() != resident || maxima.0.len() != resident {
                        return unaccounted();
                    }
                    target.seq.fill(0);
                    target.max_op.fill(0);
                    target.max = 0;
                    for a in 0..resident {
                        target.seq[a] = seqs.0[a].map_or(0, |s| s.get());
                        target.max_op[a] = maxima.0[a];
                        target.max = target.max.max(maxima.0[a]);
                    }
                    memo_deps.clear();
                    memo_deps.extend_from_slice(&resident_deps);
                }
                let resident_clock = memo.as_ref().ok_or(NativeResourceError::Unaccounted)?;
                match &mut clock {
                    Some(current) => merge(current, resident_clock),
                    None => clock = Some(clocks.copy(resident_clock, charge)?),
                }
            }
            let mut clock = match clock {
                Some(clock) => clock,
                None => clocks.zero(charge)?,
            };

            // The previous change of this actor must be an ancestor (0.12
            // asserts consecutive sequence numbers per actor), and counters
            // follow every ancestor operation (Lamport order; also keeps op
            // ids unique per actor).
            let a = actor as usize;
            if clock.seq[a].checked_add(1) != Some(seq) || start <= clock.max {
                return invalid();
            }
            let change_index = i as u32;
            for (offset, op) in change.iter_ops().enumerate() {
                let counter = u32::try_from(offset)
                    .ok()
                    .and_then(|offset| start.checked_add(offset))
                    .ok_or(NativeResourceError::WorkBytes)?;
                validation.row(change_index, Id { counter, actor }, op, &map, &clock, charge)?;
            }
            let bound = canonical_bound(change)?;
            charge.grow(bound)?;
            let canonical = canonical(change);
            charge.shrink(bound);
            if !canonical {
                return invalid();
            }
            clock.seq[a] = seq;
            clock.max_op[a] = clock.max_op[a].max(end);
            clock.max = clock.max.max(end);
            if remaining[i] > 0 {
                live[i] = Some(clock);
            } else {
                clocks.free(clock, charge);
            }
        }
        validation.counters()
    })();
    // Release every clock before returning the charge's bytes.
    for slot in live.iter_mut() {
        if let Some(clock) = slot.take() {
            clocks.free(clock, charge);
        }
    }
    if let Some(clock) = memo.take() {
        clocks.free(clock, charge);
    }
    result?;
    Ok(NativeValidated {
        integrated,
        orphaned,
    })
}
