//! Layout contributions for native-owned typed column mutation.
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{Budget, product, sum};
use hexane::{ColumnCursor, ColumnData, StrCursor, tree::TreeAllocation};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

pub(crate) fn expanded_strings(column: &ColumnData<StrCursor>) -> Result<usize> {
    let mut bytes = 0;
    column.slabs.visit_allocations(|allocation| -> Result {
        if let TreeAllocation::Element(slab) = allocation {
            let mut cursor = StrCursor::empty();
            while let Some(run) = cursor
                .try_next(slab.as_slice())
                .map_err(|_| NativeResourceError::Invalid)?
            {
                bytes = sum(
                    bytes,
                    product(run.count, run.value.as_deref().map(str::len).unwrap_or(0))?,
                )?;
            }
        }
        Ok(())
    })?;
    Ok(bytes)
}

/// The native caller supplies actual source-derived events and byte extent.
/// Nested weights and owning iterator payloads are separate owner obligations.
pub(crate) fn column<C: ColumnCursor>(
    source: &ColumnData<C>,
    events: usize,
    encoded: usize,
    budget: &mut Budget,
) -> Result<usize> {
    // Splice may retain copied noncanonical source runs beside new encodings.
    // Read their actual extent without the weight-allocating tree iterator.
    let mut copied_source = 0;
    source.slabs.visit_allocations(|allocation| -> Result {
        if let TreeAllocation::Element(slab) = allocation {
            copied_source = sum(copied_source, slab.as_slice().len())?;
        }
        Ok(())
    })?;
    let layout = source
        .mutation_allocation_bound(events, sum(copied_source, encoded)?)
        .ok_or(NativeResourceError::WorkBytes)?;
    budget.add(layout.bytes)?;
    budget.backings = sum(budget.backings, layout.backings)?;
    Ok(layout.weight_nodes)
}

pub(crate) fn raw_insert(
    source: &ColumnData<hexane::RawCursor>,
    values: usize,
    bytes: usize,
    budget: &mut Budget,
) -> Result {
    let layout = source
        .raw_insertion_allocation_bound(values, bytes)
        .ok_or(NativeResourceError::WorkBytes)?;
    budget.add(layout.bytes)?;
    budget.backings = sum(budget.backings, layout.backings)?;
    Ok(())
}

/// Native scalar columns encode at most a ten-byte value and ten-byte run
/// header per event. String payloads have their own explicit contribution.
pub(crate) fn scalar<C: ColumnCursor>(
    source: &ColumnData<C>,
    supplied_events: usize,
    budget: &mut Budget,
) -> Result<usize> {
    let events = sum(source.len(), supplied_events)?;
    column(source, events, product(events, 20)?, budget)
}

pub(crate) fn string(
    source: &ColumnData<StrCursor>,
    supplied_events: usize,
    supplied_string_bytes: usize,
    budget: &mut Budget,
) -> Result<usize> {
    let events = sum(source.len(), supplied_events)?;
    let payload = sum(expanded_strings(source)?, supplied_string_bytes)?;
    column(source, events, sum(product(events, 20)?, payload)?, budget)
}
