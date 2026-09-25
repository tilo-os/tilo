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
    Ok(())
}

pub(super) fn change(start: u64, rows: usize, limits: &NativeLimits, shape: &mut NativeShape) -> Result<(), Error> {
    let end = start.checked_add(u64::try_from(rows).map_err(|_| Error::Operations)?)
        .and_then(|n| n.checked_sub(1)).ok_or(Error::Operations)?;
    counter(end, limits, shape)?;
    dense(rows as u64, limits, shape)
}

pub(super) fn document(columns: &[Column], data: &[u8], actors: usize, limits: &NativeLimits, allowance: &NativeAllowance, shape: &mut NativeShape) -> Result<(), Error> {
    let max_bytes = column(columns, data, 0x13, limits, allowance)?;
    let mut maxima = Integers::new(&max_bytes, true);
    let mut count = 0usize;
    while let Some(value) = maxima.next()? {
        counter(value, limits, shape)?;
        count = count.checked_add(1).filter(|n| *n <= limits.max_changes).ok_or(Error::Collection)?;
    }
    // Materialize only this bounded integer lookup array, after its count and
    // absolute counters have passed. No OpSet/ChangeCollector exists yet.
    let bytes = count.checked_mul(std::mem::size_of::<u64>()).ok_or(Error::WorkBytes)?;
    let _maxima_charge = allowance.reserve(bytes)?;
    let mut max_ops = Vec::new();
    max_ops.try_reserve_exact(count).map_err(|_| Error::WorkBytes)?;
    let mut maxima = Integers::new(&max_bytes, true);
    for _ in 0..count { max_ops.push(maxima.required()?); }
    drop(max_bytes);

    let actor_bytes = column(columns, data, 0x01, limits, allowance)?;
    let seq_bytes = column(columns, data, 0x03, limits, allowance)?;
    let deps_count = column(columns, data, 0x40, limits, allowance)?;
    let deps_values = column(columns, data, 0x43, limits, allowance)?;
    let mut actor_values = Integers::new(&actor_bytes, false);
    let mut seq_values = Integers::new(&seq_bytes, true);
    let mut dep_counts = Integers::new(&deps_count, false);
    let mut dependencies = Integers::new(&deps_values, true);
    let bytes = actors.checked_mul(std::mem::size_of::<u64>()).ok_or(Error::WorkBytes)?;
    let _seq_charge = allowance.reserve(bytes)?;
    let mut sequences = Vec::new();
    sequences.try_reserve_exact(actors).map_err(|_| Error::WorkBytes)?;
    sequences.resize(actors, 0u64);
    let mut edges = 0usize;
    for (index, maximum) in max_ops.iter().copied().enumerate() {
        let actor = usize::try_from(actor_values.required()?).map_err(|_| Error::Invalid)?;
        let seq = sequences.get_mut(actor).ok_or(Error::Invalid)?;
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
        dense(maximum.checked_sub(parent_max).ok_or(Error::Invalid)?, limits, shape)?;
    }
    actor_values.end()?;
    seq_values.end()?;
    dep_counts.end()?;
    dependencies.end()
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
