//! Read-only semantic preflight for the exact native batch input.
//!
//! Byte/column validity belongs to incoming parsing. This pass retains no
//! authority or token: its caller must invoke batch apply on the same exclusive
//! document and Changes immediately after this returns successfully.
use std::collections::{HashMap, HashSet, VecDeque};

use crate::columnar::column_range::Key;
use crate::op_set2::op::Op;
use crate::op_set2::types::{KeyRef, ScalarValue as NativeScalar};
use crate::resource_limits::reconstruct::{product, sum, Budget};
use crate::resource_limits::retained::mutate::plan::IncomingDimensions;
use crate::resource_limits::{NativeAllowance, NativeMutationError, NativeResourceError};
use crate::storage::change::ChangeOp as StoredOp;
use crate::types::{ObjId, OpId};
use crate::{ActorId, Automerge, Change, ChangeHash, ObjType, ScalarValue};

type Result<T = ()> = std::result::Result<T, NativeMutationError>;

fn invalid<T>() -> Result<T> {
    Err(NativeMutationError::InvalidOperation)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Id<'a> {
    counter: u64,
    actor: &'a ActorId,
}

struct Row<'a> {
    change: &'a Change,
    id: Id<'a>,
    op: StoredOp,
}

enum Located<'r, 'a> {
    Source(Op<'a>),
    Incoming(&'r Row<'a>),
}

#[derive(PartialEq, Eq)]
enum Property<'r, 'a> {
    Map(&'r str),
    Element(Option<Id<'a>>),
}

fn local_id(change: &Change, id: OpId) -> Result<Id<'_>> {
    let actor = change
        .actors()
        .nth(id.actor())
        .ok_or(NativeMutationError::InvalidOperation)?;
    if id.counter() == 0 {
        return invalid();
    }
    Ok(Id {
        counter: id.counter(),
        actor,
    })
}

fn source_id(doc: &Automerge, id: OpId) -> Result<Id<'_>> {
    let actor = doc
        .ops
        .get_actor_safe(id.actor())
        .ok_or(NativeResourceError::Unaccounted)?;
    if id.counter() == 0 {
        return Err(NativeResourceError::Unaccounted.into());
    }
    Ok(Id {
        counter: id.counter(),
        actor,
    })
}

fn local_object(change: &Change, obj: ObjId) -> Result<Option<Id<'_>>> {
    if obj.is_root() {
        Ok(None)
    } else {
        local_id(change, obj.0).map(Some)
    }
}

fn source_object(doc: &Automerge, obj: ObjId) -> Result<Option<Id<'_>>> {
    if obj.is_root() {
        Ok(None)
    } else {
        source_id(doc, obj.0).map(Some)
    }
}

impl<'a> Row<'a> {
    fn object(&self) -> Result<Option<Id<'a>>> {
        local_object(self.change, self.op.obj)
    }

    fn key(&self) -> Result<Property<'_, 'a>> {
        match &self.op.key {
            Key::Prop(key) => Ok(Property::Map(key.as_str())),
            Key::Elem(id) if id.is_head() => Ok(Property::Element(None)),
            Key::Elem(id) => Ok(Property::Element(Some(local_id(self.change, id.0)?))),
        }
    }
}

impl<'r, 'a> Located<'r, 'a> {
    fn id(&self, doc: &'a Automerge) -> Result<Id<'a>> {
        match self {
            Self::Source(op) => source_id(doc, op.id),
            Self::Incoming(row) => Ok(row.id),
        }
    }

    fn object(&self, doc: &'a Automerge) -> Result<Option<Id<'a>>> {
        match self {
            Self::Source(op) => source_object(doc, op.obj),
            Self::Incoming(row) => row.object(),
        }
    }

    fn insert(&self) -> bool {
        match self {
            Self::Source(op) => op.insert,
            Self::Incoming(row) => row.op.insert,
        }
    }

    fn action(&self) -> u64 {
        match self {
            Self::Source(op) => op.action.into(),
            Self::Incoming(row) => row.op.action,
        }
    }

    fn property(&self, doc: &'a Automerge) -> Result<Property<'_, 'a>> {
        if self.insert() {
            return Ok(Property::Element(Some(self.id(doc)?)));
        }
        match self {
            Self::Source(op) => match &op.key {
                KeyRef::Map(key) => Ok(Property::Map(key.as_ref())),
                KeyRef::Seq(id) if id.is_head() => Ok(Property::Element(None)),
                KeyRef::Seq(id) => Ok(Property::Element(Some(source_id(doc, id.0)?))),
            },
            Self::Incoming(row) => row.key(),
        }
    }

    fn is_mark_begin(&self) -> bool {
        self.action() == 7
            && match self {
                Self::Source(op) => op.mark_name.is_some(),
                Self::Incoming(row) => row.op.mark_name.is_some(),
            }
    }

    fn counter(&self) -> Option<i64> {
        match self {
            Self::Source(op) => match op.value {
                NativeScalar::Counter(value) => Some(value),
                _ => None,
            },
            Self::Incoming(row) => match &row.op.val {
                ScalarValue::Counter(value) => Some(value.start),
                _ => None,
            },
        }
    }
}

struct Validation<'a> {
    doc: &'a Automerge,
    rows: Vec<Row<'a>>,
    operations: HashMap<Id<'a>, usize>,
    ancestors: HashSet<ChangeHash>,
    stack: Vec<ChangeHash>,
    increments: HashMap<Id<'a>, Vec<(Id<'a>, i64)>>,
}

impl<'a> Validation<'a> {
    fn locate(&self, id: Id<'a>) -> Result<Located<'_, 'a>> {
        if let Some(index) = self.operations.get(&id) {
            return Ok(Located::Incoming(&self.rows[*index]));
        }
        let actor = self
            .doc
            .ops
            .lookup_actor(id.actor)
            .ok_or(NativeMutationError::InvalidOperation)?;
        // Both source and incoming counters are checked before constructing a
        // native packed OpId; this constructor is otherwise infallible only by
        // convention.
        if id.counter == 0 || id.counter > u32::MAX as u64 {
            return invalid();
        }
        self.doc
            .ops
            .find_op_by_id_and_vis_fast(&OpId::new(id.counter, actor))
            .map(|(op, _)| Located::Source(op))
            .ok_or(NativeMutationError::InvalidOperation)
    }

    fn accessible<'r>(&'r self, id: Id<'a>, row: &Row<'a>) -> Result<Located<'r, 'a>> {
        let operation = self.locate(id)?;
        let hash = match &operation {
            Located::Incoming(previous) if previous.change.hash() == row.change.hash() => {
                if previous.id.counter < row.id.counter {
                    return Ok(operation);
                }
                return invalid();
            }
            Located::Incoming(previous) => previous.change.hash(),
            Located::Source(op) => self
                .doc
                .change_graph
                .opid_to_hash(op.id)
                .ok_or(NativeResourceError::Unaccounted)?,
        };
        if !self.ancestors.contains(&hash) {
            return invalid();
        }
        Ok(operation)
    }

    fn ancestors(
        &mut self,
        change: &Change,
        pool: &[&Change],
        indexes: &HashMap<ChangeHash, usize>,
        applied: &[bool],
    ) -> Result<u64> {
        self.ancestors.clear();
        self.stack.clear();
        for hash in change.deps() {
            if self.ancestors.insert(*hash) {
                self.stack.push(*hash);
            }
        }
        let mut maximum = 0;
        while let Some(hash) = self.stack.pop() {
            if let Some((_, _, max)) = self.doc.change_graph.native_validation_metadata(&hash) {
                maximum = maximum.max(max);
                for parent in self.doc.change_graph.deps_for_hash(&hash) {
                    if self.ancestors.insert(parent) {
                        self.stack.push(parent);
                    }
                }
            } else {
                let index = *indexes
                    .get(&hash)
                    .ok_or(NativeMutationError::InvalidOperation)?;
                if !applied[index] {
                    return invalid();
                }
                let parent = pool[index];
                maximum = maximum.max(parent.max_op());
                for hash in parent.deps() {
                    if self.ancestors.insert(*hash) {
                        self.stack.push(*hash);
                    }
                }
            }
        }
        Ok(maximum)
    }

    fn row(&mut self, row: Row<'a>) -> Result {
        if self.operations.contains_key(&row.id) {
            return invalid();
        }
        let object = row.object()?;
        let kind = match object {
            None => ObjType::Map,
            Some(id) => match self.accessible(id, &row)?.action() {
                0 => ObjType::Map,
                2 => ObjType::List,
                4 => ObjType::Text,
                6 => ObjType::Table,
                _ => return invalid(),
            },
        };
        let key = row.key()?;
        match (&key, kind) {
            (Property::Map(_), ObjType::Map | ObjType::Table) => {
                if row.op.insert || row.op.action == 7 {
                    return invalid();
                }
            }
            (Property::Element(anchor), ObjType::List | ObjType::Text) => {
                if row.op.insert {
                    if !matches!(row.op.action, 0 | 1 | 2 | 4 | 6 | 7) || !row.op.pred.is_empty() {
                        return invalid();
                    }
                } else if anchor.is_none() || row.op.action == 7 {
                    return invalid();
                }
                if let Some(anchor) = anchor {
                    let previous = self.accessible(*anchor, &row)?;
                    if !previous.insert() || previous.object(self.doc)? != object {
                        return invalid();
                    }
                }
            }
            _ => return invalid(),
        }
        if row.op.action == 7 {
            if kind != ObjType::Text || !row.op.insert || !row.op.pred.is_empty() {
                return invalid();
            }
            if row.op.mark_name.is_none() {
                let counter = row
                    .id
                    .counter
                    .checked_sub(1)
                    .filter(|counter| *counter > 0)
                    .ok_or(NativeMutationError::InvalidOperation)?;
                let begin = self.accessible(
                    Id {
                        counter,
                        actor: row.id.actor,
                    },
                    &row,
                )?;
                if !begin.is_mark_begin() || begin.object(self.doc)? != object {
                    return invalid();
                }
            }
        }
        let increment = match (&row.op.val, row.op.action) {
            (ScalarValue::Int(value), 5) => Some(*value),
            (ScalarValue::Uint(value), 5) => Some(*value as i64),
            (_, 5) => return invalid(),
            _ => None,
        };
        let mut last = None;
        let mut counter_seen = false;
        for predecessor in &row.op.pred {
            let id = local_id(row.change, *predecessor)?;
            if last.is_some_and(|previous| previous >= id) {
                return invalid();
            }
            last = Some(id);
            let previous = self.accessible(id, &row)?;
            if previous.object(self.doc)? != object
                || previous.property(self.doc)? != key
                || !matches!(previous.action(), 0 | 1 | 2 | 4 | 6)
            {
                return invalid();
            }
            if let Some(value) = increment {
                // Native increment resolves a conflicted property when at
                // least one predecessor is a counter. Other visible values
                // are ordinary overwritten predecessors (add_succ clears
                // their increment), not a reason to reject valid history.
                if previous.counter().is_some() {
                    counter_seen = true;
                    self.increments.entry(id).or_default().push((row.id, value));
                }
            }
        }
        if increment.is_some() && !counter_seen {
            return invalid();
        }
        self.operations.insert(row.id, self.rows.len());
        self.rows.push(row);
        Ok(())
    }

    // A history is refused when a counter's complete merged value, before or
    // after this batch, is not representable. Intermediate native paths (patch
    // deltas, raw-base hydration of a counter hidden by a scalar conflict,
    // clock-scoped readback) wrap instead, so they impose no further
    // condition: an accepted valid history cannot be rejected by an ordering
    // of arithmetic that native apply might not even execute.
    fn counters(&mut self) -> Result {
        for values in self.increments.values_mut() {
            values.sort_unstable_by_key(|(id, _)| *id);
        }
        for (id, values) in &self.increments {
            let counter = self.locate(*id)?;
            let base = counter
                .counter()
                .ok_or(NativeMutationError::InvalidOperation)?;
            let mut old_sum = 0i64;
            let mut merged = 0i64;
            let mut incoming = values.iter().peekable();
            if let Located::Source(op) = &counter {
                for (old_id, increment) in op.succ_inc() {
                    let old_id = source_id(self.doc, old_id)?;
                    while incoming.peek().is_some_and(|(id, _)| *id < old_id) {
                        let (_, value) = incoming.next().ok_or(NativeResourceError::Unaccounted)?;
                        merged = merged
                            .checked_add(*value)
                            .ok_or(NativeMutationError::InvalidOperation)?;
                    }
                    if incoming.peek().is_some_and(|(id, _)| *id == old_id) {
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
            for (_, value) in incoming {
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

/// Batch outcome predicted by validation over the same queue and input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeValidated {
    /// Changes that batch apply will integrate into the graph.
    pub(crate) integrated: usize,
    /// Whether any new or previously queued change will remain queued.
    pub(crate) orphaned: bool,
}

/// The reservation precedes every validator collection and decoded owning row.
/// All collections die before its guard; source owners remain with the caller.
pub(super) fn validate_incoming(
    doc: &Automerge,
    incoming: &[Change],
    allowance: Option<&NativeAllowance>,
) -> Result<NativeValidated> {
    if doc.ops.actors.len() != doc.change_graph.num_actors()
        || doc.ops.actors.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(NativeResourceError::Unaccounted.into());
    }
    let dimensions = IncomingDimensions::read(doc.queue.iter().chain(incoming.iter()))?;
    let nodes = sum(doc.change_graph.len(), dimensions.changes)?;
    let actors = sum(doc.ops.actors.len(), dimensions.actors)?;
    if actors > i32::MAX as usize {
        // MarkIndex packs the actor into the positive half of a signed i64.
        return Err(NativeResourceError::WorkBytes.into());
    }
    let mut budget = Budget::default();
    budget.growing_vec::<&Change>(dimensions.changes)?;
    budget.hash::<ChangeHash, usize>(dimensions.changes)?;
    budget.hash::<(&ActorId, u64), ChangeHash>(dimensions.changes)?;
    budget.hash::<ChangeHash, Vec<usize>>(dimensions.dependencies)?;
    budget.growing_vec_forest::<usize>(dimensions.dependencies, dimensions.dependencies)?;
    for _ in 0..3 {
        budget.growing_vec::<usize>(dimensions.changes)?;
    }
    budget.growing_vec::<bool>(dimensions.changes)?;
    budget.hash::<&ActorId, ()>(actors)?;
    budget.hash::<&ActorId, (u64, ChangeHash)>(actors)?;
    budget.growing_vec::<Row<'_>>(dimensions.operations)?;
    budget.hash::<Id<'_>, usize>(dimensions.operations)?;
    budget.hash::<Id<'_>, Vec<(Id<'_>, i64)>>(dimensions.predecessors)?;
    budget.growing_vec_forest::<(Id<'_>, i64)>(dimensions.predecessors, dimensions.predecessors)?;
    budget.growing_vec_forest::<OpId>(dimensions.predecessors, dimensions.operations)?;
    budget.hash::<ChangeHash, ()>(nodes)?;
    budget.growing_vec::<ChangeHash>(nodes)?;
    // Each decoded row owns at most key/value/mark-name SmolStr Arc payloads;
    // inline values allocate nothing. The usize slot also covers tail padding.
    budget.arc::<usize>(product(dimensions.operations, 3)?)?;
    let payload = sum(
        sum(dimensions.key_bytes, dimensions.value_bytes)?,
        dimensions.mark_name_bytes,
    )?;
    budget.add(payload)?;
    // Native scalar/string decoding may hold its temporary byte/String
    // allocation while constructing the row's owning value. At most the
    // current decoded row overlaps all retained rows; the complete actual
    // input payload bounds that row without trusting a guessed row size.
    budget.growing_vec::<u8>(payload)?;
    let (keys, values, marks) = doc.ops.native_incoming_source_payload()?;
    budget.add(sum(sum(keys, values)?, marks)?)?;
    budget.add(
        doc.ops
            .native_read_search_bytes()
            .ok_or(NativeResourceError::WorkBytes)?,
    )?;
    budget.add(
        hexane::SlabTree::<hexane::SlabWeight>::search_stack_allocation_bound(
            doc.change_graph.len().max(1),
        )
        .ok_or(NativeResourceError::WorkBytes)?,
    )?;
    // Ordinary-document admission passes no allowance: its inspected input
    // limits bound this scratch instead of the conservative plan above.
    let _scratch = allowance.map(|a| a.reserve(budget.bytes())).transpose()?;

    let mut pool = Vec::new();
    let mut indexes = HashMap::new();
    let mut sequences = HashMap::new();
    for change in doc.queue.iter().chain(incoming.iter()) {
        let hash = change.hash();
        if doc.change_graph.has_change(&hash) || indexes.contains_key(&hash) {
            continue;
        }
        if doc.has_actor_seq(change)
            || sequences
                .insert((change.actor_id(), change.seq()), hash)
                .is_some()
        {
            return Err(NativeMutationError::DuplicateSequence);
        }
        indexes.insert(hash, pool.len());
        pool.push(change);
    }
    let mut unsatisfied = vec![0usize; pool.len()];
    let mut waiting: HashMap<ChangeHash, Vec<usize>> = HashMap::new();
    for (index, change) in pool.iter().enumerate() {
        for hash in change.deps() {
            if !doc.change_graph.has_change(hash) {
                unsatisfied[index] = sum(unsatisfied[index], 1)?;
                waiting.entry(*hash).or_default().push(index);
            }
        }
    }
    let mut ready = VecDeque::new();
    for (index, count) in unsatisfied.iter().enumerate() {
        if *count == 0 {
            ready.push_back(index);
        }
    }
    let mut order = Vec::new();
    while let Some(index) = ready.pop_front() {
        order.push(index);
        if let Some(children) = waiting.remove(&pool[index].hash()) {
            for child in children {
                unsatisfied[child] = unsatisfied[child]
                    .checked_sub(1)
                    .ok_or(NativeResourceError::Unaccounted)?;
                if unsatisfied[child] == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    let mut catalog: HashSet<&ActorId> = doc.ops.actors.iter().collect();
    for index in &order {
        if pool[*index].seq() == 1 {
            catalog.insert(pool[*index].actor_id());
        }
    }
    let mut last: HashMap<&ActorId, (u64, ChangeHash)> = HashMap::new();
    for (actor, identity) in doc.ops.actors.iter().enumerate() {
        let seq = doc.change_graph.seq_for_actor(actor);
        if seq != 0 {
            let hash = doc
                .change_graph
                .get_hash_for_actor_seq(actor, seq)
                .map_err(|_| NativeResourceError::Unaccounted)?;
            last.insert(identity, (seq, hash));
        }
    }
    let mut validation = Validation {
        doc,
        rows: Vec::new(),
        operations: HashMap::new(),
        ancestors: HashSet::new(),
        stack: Vec::new(),
        increments: HashMap::new(),
    };
    let mut applied = vec![false; pool.len()];
    let integrated = order.len();
    for index in order {
        let change = pool[index];
        let maximum = validation.ancestors(change, &pool, &indexes, &applied)?;
        let previous = last.get(change.actor_id()).copied();
        let expected = previous
            .map_or(0, |(seq, _)| seq)
            .checked_add(1)
            .ok_or(NativeResourceError::WorkBytes)?;
        if change.seq() != expected
            || previous.is_some_and(|(_, hash)| !validation.ancestors.contains(&hash))
        {
            return invalid();
        }
        let start = change.start_op().get();
        let end = start
            .checked_add(u64::try_from(change.len()).map_err(|_| NativeResourceError::WorkBytes)?)
            .and_then(|end| end.checked_sub(1))
            .ok_or(NativeResourceError::WorkBytes)?;
        if start > u32::MAX as u64 || end > u32::MAX as u64 || change.seq() > u32::MAX as u64 {
            return Err(NativeResourceError::WorkBytes.into());
        }
        if start <= maximum || change.actors().any(|actor| !catalog.contains(actor)) {
            return invalid();
        }
        for (offset, op) in change.iter_ops().enumerate() {
            validation.row(Row {
                change,
                op,
                id: Id {
                    counter: start + offset as u64,
                    actor: change.actor_id(),
                },
            })?;
        }
        applied[index] = true;
        last.insert(change.actor_id(), (change.seq(), change.hash()));
    }
    validation.counters()?;
    Ok(NativeValidated {
        integrated,
        orphaned: integrated < pool.len(),
    })
}
