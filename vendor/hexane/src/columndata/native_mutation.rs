//! Layout for a mutation of an existing, possibly fragmented column.
use super::*;
use crate::allocation::ColumnAllocation;
use crate::slab::tree::TreeAllocation;

impl<C: ColumnCursor> ColumnData<C> {
    /// Bounds the concrete encoding/tree buffers for one serial edit batch.
    ///
    /// `maximum_events` must cover the existing rows plus every supplied row
    /// event, including replacements, seen by any encoder in the batch.
    /// `maximum_encoded_bytes` must bound the complete column's encoded bytes
    /// at every intermediate state. These are layout inputs, not admission
    /// evidence. The embedding owner derives them from its actual operations.
    ///
    /// Owning item payloads and nested weight allocations are excluded. The
    /// returned weight-node count covers source and replacement tree nodes;
    /// their traversal/recompute temporary weights require a separate bound.
    pub fn mutation_allocation_bound(
        &self,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Option<ColumnAllocation> {
        if maximum_events < self.len || self.has_diagnostic_exports() {
            return None;
        }
        let mut old_buffers = 0usize;
        let mut old_nodes = 0usize;
        self.slabs
            .visit_allocations(|allocation| -> Result<(), ()> {
                match allocation {
                    TreeAllocation::OwnedBytes(bytes) => {
                        old_buffers = old_buffers.checked_add(bytes).ok_or(())?;
                    }
                    TreeAllocation::Weight(_) => {
                        old_nodes = old_nodes.checked_add(1).ok_or(())?;
                    }
                    TreeAllocation::Element(_) => {}
                }
                Ok(())
            })
            .ok()?;

        // Repeated splices can leave short slabs. Unlike a fresh construction,
        // encoded_bytes/64 is therefore not a slab-count proof. A nonempty
        // slab contains at least one event; an empty column keeps one root.
        let slabs = maximum_events.max(1);
        let writer =
            slab::SlabWriter::<C::Item>::construction_allocation_bound(maximum_events, slabs)?;
        let literals = if maximum_events == 0 {
            0
        } else {
            maximum_events
                .checked_mul(2)?
                .max(4)
                .checked_add(maximum_events)?
                .checked_mul(std::mem::size_of::<Cow<'_, C::Item>>())?
        };
        let tree = SpanTree::<Slab, C::SlabIndex>::construction_allocation_bound(slabs)?;
        // A completed current encoding can overlap its next replacement.
        // Include two whole byte/Arc/tree results, in addition to writer and
        // literal scratch, plus the actual original tree capacity. No original
        // capacity is inferred from current length after removals or splits.
        let output = maximum_encoded_bytes.checked_add(
            slabs.checked_mul(std::mem::size_of::<crate::allocation::ArcVecLayout>())?,
        )?;
        let bytes = old_buffers
            .checked_add(writer)?
            .checked_add(literals)?
            .checked_add(output.checked_mul(2)?)?
            .checked_add(tree.checked_mul(2)?)?
            .checked_add(
                SpanTree::<Slab, C::SlabIndex>::search_stack_allocation_bound(self.slabs.len())?,
            )?;
        let nodes = (slabs / (slab::tree::B - 1)).checked_add(2)?;
        Some(ColumnAllocation {
            bytes,
            backings: slabs,
            weight_nodes: old_nodes.checked_add(nodes.checked_mul(2)?)?,
        })
    }
}

impl ColumnData<crate::RawCursor> {
    /// Concrete bound for inserting at most `values` byte slices in at most
    /// `values` serial splices. This does not cover raw deletion or arbitrary
    /// custom cursor behavior. Existing bytes and source slabs are read here.
    pub fn raw_insertion_allocation_bound(
        &self,
        values: usize,
        inserted_bytes: usize,
    ) -> Option<ColumnAllocation> {
        if self.has_diagnostic_exports() {
            return None;
        }
        let mut old_buffers = 0usize;
        let mut old_nodes = 0usize;
        self.slabs
            .visit_allocations(|allocation| -> Result<(), ()> {
                match allocation {
                    TreeAllocation::OwnedBytes(bytes) => {
                        old_buffers = old_buffers.checked_add(bytes).ok_or(())?;
                    }
                    TreeAllocation::Weight(_) => {
                        old_nodes = old_nodes.checked_add(1).ok_or(())?;
                    }
                    TreeAllocation::Element(_) => {}
                }
                Ok(())
            })
            .ok()?;
        // RawCursor::splice_encoder/finalize_state adds one borrowed prefix
        // and suffix copy. Each input slice is one Raw writer action, including
        // a slice larger than 4096. Replacing the touched slab adds at most two
        // slabs per value; byte length is not a writer-event count.
        let events = values.checked_add(2)?;
        let slabs = self.slabs.len().checked_add(values.checked_mul(2)?)?.max(1);
        let writer = slab::SlabWriter::<[u8]>::construction_allocation_bound(events, events)?;
        let encoded = self.len.checked_add(inserted_bytes)?;
        let tree = SpanTree::<Slab, slab::SlabWeight>::construction_allocation_bound(slabs)?;
        let output = encoded.checked_add(
            slabs.checked_mul(std::mem::size_of::<crate::allocation::ArcVecLayout>())?,
        )?;
        let bytes = old_buffers
            .checked_add(writer)?
            .checked_add(output.checked_mul(2)?)?
            .checked_add(tree.checked_mul(2)?)?;
        Some(ColumnAllocation {
            bytes,
            backings: slabs,
            weight_nodes: old_nodes
                .checked_add((slabs / (slab::tree::B - 1) + 2).checked_mul(2)?)?,
        })
    }
}
