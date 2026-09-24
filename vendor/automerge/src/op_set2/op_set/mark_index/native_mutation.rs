//! Nested mark ownership during retained-column edits and actor rewrites.
use super::*;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{Budget, product, sum};
use crate::resource_limits::retained::mutate::columns;
use hexane::tree::TreeAllocation;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

impl MarkIndexColumn {
    pub(crate) fn native_mutation_plan(
        &self,
        supplied_rows: usize,
        supplied_marks: usize,
        supplied_mark_payload: usize,
        budget: &mut Budget,
    ) -> Result {
        let events = sum(self.data.len(), supplied_rows)?;
        let nodes = columns::scalar(&self.data, supplied_rows, budget)?;
        let mut high_water = 0;
        let mut source_marks = 0;
        self.data.slabs.visit_allocations(|allocation| -> Result {
            match allocation {
                TreeAllocation::Weight(weight) => {
                    high_water = high_water
                        .max(weight.start_capacity_high_water)
                        .max(weight.end_capacity_high_water);
                }
                TreeAllocation::Element(slab) => {
                    let mut cursor = MarkIndex::empty();
                    while let Some(run) = cursor
                        .try_next(slab.as_slice())
                        .map_err(|_| NativeResourceError::Invalid)?
                    {
                        if run.value.is_some() {
                            source_marks = sum(source_marks, run.count)?;
                        }
                    }
                }
                TreeAllocation::OwnedBytes(_) => {}
            }
            Ok(())
        })?;
        let marks = sum(self.cache.len(), supplied_marks)?;
        // Count start and end rows themselves, including a valid retained end
        // with no cached start. Cache cardinality alone is not this universe.
        let set_entries = sum(source_marks, supplied_marks)?.max(high_water);
        let height = usize::BITS as usize - events.max(1).leading_zeros() as usize + 1;
        // Column splice retains its located cursor weight (1), SpanTree insert
        // retains its element weight (1), and each recursive insertion frame
        // can retain another element weight (height). split_child keeps five
        // named weight copies; recompute/check_weight overlaps its accumulator
        // and one W::alloc result (2). Searches/last_weight/replace/remove have
        // no larger live set. Tree nodes themselves are counted above.
        let temporary_weights = sum(height, 9)?;
        let weights = sum(nodes, temporary_weights)?;
        budget.hash_many::<OpId, ()>(product(weights, 2)?, set_entries)?;

        // Cache insertion and actor rewrite use their actual retained table
        // capacity; a short logical length does not erase its allocation.
        budget.hash::<OpId, MarkData<'static>>(self.cache.capacity().max(marks))?;
        let mut payload = supplied_mark_payload;
        for mark in self.cache.values() {
            payload = sum(payload, mark.name.len())?;
            payload = sum(
                payload,
                match &mark.value {
                    crate::op_set2::types::ScalarValue::Bytes(value)
                    | crate::op_set2::types::ScalarValue::Unknown { bytes: value, .. } => {
                        value.len()
                    }
                    crate::op_set2::types::ScalarValue::Str(value) => value.len(),
                    _ => 0,
                },
            )?;
        }
        // A completed rewritten cache and its following replacement may
        // coexist during successive actor insertions. These are owned Cows.
        budget.add(product(payload, 2)?)?;
        budget.growing_vec::<Option<MarkIndexBuilder>>(supplied_rows)?;
        budget.growing_vec::<Option<MarkIndexValue>>(supplied_rows)?;
        Ok(())
    }
}
