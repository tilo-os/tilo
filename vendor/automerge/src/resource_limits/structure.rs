//! Resource-relevant change metadata checks before dense native collectors.
//! These checks do not replace complete native semantic/head verification.
use super::{NativeAllowance, NativeLimits, NativeResourceError as Error, NativeShape};
use super::inspect::{Column, Inflated, Reader, inflate};

enum Bytes<'a> { Borrowed(&'a [u8]), Inflated(Inflated) }
impl std::ops::Deref for Bytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] { match self { Self::Borrowed(b) => b, Self::Inflated(b) => b } }
}
fn column<'a>(columns: &[Column], data: &'a [u8], spec: u32, limits: &NativeLimits, allowance: &NativeAllowance) -> Result<Bytes<'a>, Error> {
    let mut reader = Reader(data);
    for column in columns {
        let bytes = reader.take(column.len)?;
        if column.spec & !8 == spec {
            return if column.spec & 8 == 0 { Ok(Bytes::Borrowed(bytes)) }
                else { Ok(Bytes::Inflated(inflate(bytes, limits.max_normalized_bytes, allowance)?)) };
        }
    }
    Ok(Bytes::Borrowed(&[]))
}

// Non-null integer columns only. Checked arithmetic is essential: the native
// delta cursor's arithmetic is unsuitable for an untrusted resource preflight.
struct Integers<'a> {
    reader: Reader<'a>, delta: bool, remaining: usize, literal: bool,
    repeated: i128, absolute: i128,
}
impl<'a> Integers<'a> {
    fn new(bytes: &'a [u8], delta: bool) -> Self {
        Self { reader: Reader(bytes), delta, remaining: 0, literal: false, repeated: 0, absolute: 0 }
    }
    fn value(&mut self) -> Result<i128, Error> {
        if self.delta { self.reader.sint().map(i128::from) }
        else { self.reader.uint().map(i128::from) }
    }
    fn next(&mut self) -> Result<Option<u64>, Error> {
        while self.remaining == 0 {
            if self.reader.0.is_empty() { return Ok(None); }
            let run = self.reader.sint()?;
            if run == 0 {
                if self.reader.size()? == 0 { continue; }
                return Err(Error::Invalid);
            }
            self.literal = run < 0;
            self.remaining = usize::try_from(run.checked_abs().ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)?;
            if !self.literal { self.repeated = self.value()?; }
        }
        self.remaining -= 1;
        let value = if self.literal { self.value()? } else { self.repeated };
        let value = if self.delta {
            self.absolute = self.absolute.checked_add(value).filter(|v| *v >= 0 && *v <= i64::MAX as i128).ok_or(Error::Invalid)?;
            self.absolute
        } else { value };
        u64::try_from(value).map(Some).map_err(|_| Error::Invalid)
    }
    fn required(&mut self) -> Result<u64, Error> { self.next()?.ok_or(Error::Invalid) }
    fn end(&mut self) -> Result<(), Error> {
        if self.next()?.is_none() { Ok(()) } else { Err(Error::Invalid) }
    }
}

fn counter(value: u64, limits: &NativeLimits, shape: &mut NativeShape) -> Result<(), Error> {
    if value > limits.max_operation_counter { return Err(Error::Operations); }
    shape.maximum_operation_counter = shape.maximum_operation_counter.max(value);
    Ok(())
}
fn dense(value: u64, limits: &NativeLimits, shape: &mut NativeShape) -> Result<(), Error> {
    let value = usize::try_from(value).map_err(|_| Error::Operations)?;
    shape.dense_operations = shape.dense_operations.checked_add(value)
        .filter(|v| *v <= limits.max_operations).ok_or(Error::Operations)?;
    shape.max_change_ops = shape.max_change_ops.max(value);
    Ok(())
}

pub(super) fn change(start: u64, rows: usize, limits: &NativeLimits, shape: &mut NativeShape) -> Result<(), Error> {
    let end = start.checked_add(u64::try_from(rows).map_err(|_| Error::Operations)?)
        .and_then(|n| n.checked_sub(1)).ok_or(Error::Operations)?;
    counter(end, limits, shape)?;
    dense(rows as u64, limits, shape)
}

pub(super) fn document(columns: &[Column], ops: &[Column], data: &[u8], actors: usize, limits: &NativeLimits, allowance: &NativeAllowance, shape: &mut NativeShape) -> Result<(), Error> {
    // Set again by the op-column scan that follows (`Scan::columns`).
    shape.last_document_raw_values = 0;
    shape.last_document_ordered_bytes = 0;
    shape.last_document_ordered_per_switch = 0;
    let max_bytes = column(columns, data, 0x13, limits, allowance)?;
    let mut maxima = Integers::new(&max_bytes, true);
    let mut count = 0usize;
    while let Some(value) = maxima.next()? {
        counter(value, limits, shape)?;
        count = count.checked_add(1).filter(|n| *n <= limits.max_changes).ok_or(Error::Collection)?;
    }
    // Materialize only these bounded per-change lookup arrays (max op,
    // start op, actor), after their count and absolute counters have
    // passed. No OpSet/ChangeCollector exists yet.
    let per_change = 8 + 8 + 4 + 4 + 4;
    let bytes = count.checked_mul(per_change).ok_or(Error::WorkBytes)?;
    let _maxima_charge = allowance.reserve(bytes)?;
    let mut max_ops = Vec::new();
    max_ops.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;
    let mut maxima = Integers::new(&max_bytes, true);
    for _ in 0..count { max_ops.push(maxima.required()?); }
    drop(max_bytes);
    let mut starts: Vec<u64> = Vec::new();
    starts.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;
    let mut owners: Vec<u32> = Vec::new();
    owners.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;

    let actor_bytes = column(columns, data, 0x01, limits, allowance)?;
    let seq_bytes = column(columns, data, 0x03, limits, allowance)?;
    let deps_count = column(columns, data, 0x40, limits, allowance)?;
    let deps_values = column(columns, data, 0x43, limits, allowance)?;
    let mut actor_values = Integers::new(&actor_bytes, false);
    let mut seq_values = Integers::new(&seq_bytes, true);
    let mut dep_counts = Integers::new(&deps_count, false);
    let mut dependencies = Integers::new(&deps_values, true);
    let bytes = actors.checked_add(1).and_then(|n| n.checked_mul(std::mem::size_of::<u64>())).ok_or(Error::WorkBytes)?;
    let _seq_charge = allowance.reserve(bytes)?;
    let mut sequences = Vec::new();
    sequences.try_reserve_exact(actors + 1).map_err(|_| Error::WorkBytes)?;
    sequences.resize(actors + 1, 0u64);
    let mut edges = 0usize;
    let mut declared = 0usize;
    for (index, maximum) in max_ops.iter().copied().enumerate() {
        let actor = usize::try_from(actor_values.required()?).map_err(|_| Error::Invalid)?;
        let seq = sequences.get_mut(actor).filter(|_| actor < actors).ok_or(Error::Invalid)?;
        *seq = seq.checked_add(1).ok_or(Error::Invalid)?;
        if seq_values.required()? != *seq { return Err(Error::Invalid); }
        let count = usize::try_from(dep_counts.required()?).map_err(|_| Error::Collection)?;
        edges = edges.checked_add(count).filter(|v| *v <= limits.max_edges).ok_or(Error::Collection)?;
        let mut parent_max = 0;
        for _ in 0..count {
            let dependency = usize::try_from(dependencies.required()?).map_err(|_| Error::Invalid)?;
            if dependency >= index { return Err(Error::Invalid); }
            parent_max = parent_max.max(max_ops[dependency]);
        }
        let ops = maximum.checked_sub(parent_max).ok_or(Error::Invalid)?;
        dense(ops, limits, shape)?;
        declared = declared.saturating_add(usize::try_from(ops).unwrap_or(usize::MAX));
        starts.push(parent_max + 1);
        owners.push(u32::try_from(actor).map_err(|_| Error::Invalid)?);
    }
    actor_values.end()?;
    seq_values.end()?;
    dep_counts.end()?;
    dependencies.end()?;
    drop((actor_bytes, seq_bytes, deps_count, deps_values));

    // Per-actor change lists (counting sort): an actor's changes appear in
    // seq order, so each list is sorted by counter range. `sequences[a]`
    // holds the actor's change count; turn it into list offsets.
    let mut offset = 0u64;
    for slot in sequences.iter_mut() {
        let n = *slot;
        *slot = offset;
        offset += n;
    }
    let mut order: Vec<u32> = Vec::new();
    order.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;
    order.resize(count, 0);
    let mut next: Vec<u32> = Vec::new();
    next.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;
    next.resize(count, 0);
    for (index, &actor) in owners.iter().enumerate() {
        let slot = &mut sequences[actor as usize];
        order[*slot as usize] = index as u32;
        *slot += 1;
    }
    // `sequences[a]` is now the end of actor a's list; its start is the
    // previous actor's end.
    let order = collector_order(ops, data, columns, &max_ops, &starts, &order,
        &sequences, &mut next, declared, limits, allowance)?;
    shape.last_document_queued_ops = order.queued;
    shape.last_document_literal_ops = order.literal;
    shape.last_document_switches = order.switches;
    Ok(())
}

/// Upper bound on the ops the load collector (`ChangeCollector`) holds in a
/// `ProgressiveEncoder` queue at any time while it walks this document's
/// ops in row order.
///
/// Each op row goes to the builder of the change covering its op id
/// (`builders_index`: the actor's change whose `start_op..=max_op` holds the
/// counter, `start_op = max_op - declared ops + 1`). A change whose declared
/// op count makes its dense slots larger than one `ProgressiveEncoder`
/// encodes ops as they arrive and queues an op whose index is not the next
/// expected one, draining queued ops once the gap closes. This simulation
/// counts every op of such a change from its first out-of-order arrival on
/// as queued (never draining), which bounds the real queue from above.
/// Delete ops are synthesized from successor lists in hash-map order, so
/// every declared op without a row (declared − rows) is counted as queued
/// too. Dense-slot changes (and wasm, which always uses dense slots) never
/// queue.
///
/// The same walk yields `literal`: ops of any change from its first
/// out-of-order arrival on (and every synthesized delete), whose encoding is
/// not the document's row order; and `switches`: consecutive rows owned by
/// different changes, which bound how often a document run is cut when the
/// in-order ops are re-encoded per change.
struct CollectorOrder { queued: usize, literal: usize, switches: usize }

#[allow(clippy::too_many_arguments)]
fn collector_order(ops: &[Column], data: &[u8], changes: &[Column], max_ops: &[u64], starts: &[u64],
    order: &[u32], ends: &[u64], next: &mut [u32], declared: usize, limits: &NativeLimits,
    allowance: &NativeAllowance) -> Result<CollectorOrder, Error> {
    use crate::op_set2::change::collector::ProgressiveEncoder;
    use crate::op_set2::OpBuilder;
    const DIRTY: u32 = u32::MAX;
    let dense_only = cfg!(all(feature = "wasm", target_family = "wasm"));
    let slot = std::mem::size_of::<Option<OpBuilder<'static>>>() as u64;
    let encoder = std::mem::size_of::<ProgressiveEncoder<'static>>() as u64;
    let queues = |c: usize| !dense_only
        && max_ops[c].saturating_sub(starts[c]).saturating_add(1).saturating_mul(slot) > encoder;
    // The op columns follow the change columns in the chunk's data.
    let skip = changes.iter().try_fold(0usize, |a, c| a.checked_add(c.len)).ok_or(Error::Invalid)?;
    let op_data = data.get(skip..).ok_or(Error::Invalid)?;
    let id_actor = column(ops, op_data, 0x21, limits, allowance)?;
    let id_ctr = column(ops, op_data, 0x23, limits, allowance)?;
    let mut actors = Integers::new(&id_actor, false);
    let mut counters = Integers::new(&id_ctr, true);
    let mut queued = 0usize;
    let mut literal = 0usize;
    let mut switches = 0usize;
    let mut previous = usize::MAX;
    let mut rows = 0usize;
    while let Some(actor) = actors.next()? {
        let counter = counters.required()?;
        rows += 1;
        // Conservatively a switch unless the row provably continues the
        // previous row's change.
        let mut owner = usize::MAX;
        let actor = usize::try_from(actor).map_err(|_| Error::Invalid)?;
        let list = match (ends.get(actor), actor.checked_sub(1).map_or(Some(&0), |a| ends.get(a))) {
            (Some(&end), Some(&start)) => &order[start as usize..end as usize],
            _ => &order[..0], // not encodable: the collector refuses the document
        };
        let at = list.partition_point(|&c| max_ops[c as usize] < counter);
        if let Some(&change) = list.get(at) {
            let change = change as usize;
            if counter >= starts[change] {
                owner = change;
                let index = counter - starts[change];
                if next[change] != DIRTY && u64::from(next[change]) == index {
                    next[change] += 1;
                } else {
                    next[change] = DIRTY;
                    literal += 1;
                    if queues(change) {
                        queued += 1;
                    }
                }
            }
        }
        if owner != previous || owner == usize::MAX {
            switches += 1;
        }
        previous = owner;
    }
    counters.end()?;
    let deletes = declared.saturating_sub(rows);
    Ok(CollectorOrder {
        queued: queued.saturating_add(deletes),
        literal: literal.saturating_add(deletes),
        switches,
    })
}

pub(super) fn bundle(columns: &[Column], data: &[u8], limits: &NativeLimits, allowance: &NativeAllowance, shape: &mut NativeShape) -> Result<(), Error> {
    let starts = column(columns, data, 0x13, limits, allowance)?;
    let maxima = column(columns, data, 0x23, limits, allowance)?;
    let mut starts = Integers::new(&starts, true);
    let mut maxima = Integers::new(&maxima, true);
    let mut count = 0usize;
    while let Some(start) = starts.next()? {
        if start == 0 { return Err(Error::Invalid); }
        count = count.checked_add(1).filter(|n| *n <= limits.max_changes).ok_or(Error::Collection)?;
        let max = maxima.required()?;
        counter(max, limits, shape)?;
        dense(max.checked_add(1).and_then(|v| v.checked_sub(start)).ok_or(Error::Invalid)?, limits, shape)?;
    }
    maxima.end()
}
