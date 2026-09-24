//! Private counts read from the actual integrated/queued Change owners.
use crate::Change;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::sum;

#[derive(Default)]
pub(crate) struct IncomingDimensions {
    pub(crate) changes: usize,
    pub(crate) operations: usize,
    pub(crate) predecessors: usize,
    pub(crate) maximum_predecessors: usize,
    pub(crate) dependencies: usize,
    pub(crate) actors: usize,
    pub(crate) actor_bytes: usize,
    pub(crate) key_bytes: usize,
    pub(crate) value_bytes: usize,
    pub(crate) mark_name_bytes: usize,
    pub(crate) marks: usize,
}

impl IncomingDimensions {
    pub(crate) fn read<'a>(
        changes: impl Iterator<Item = &'a Change>,
    ) -> Result<Self, NativeResourceError> {
        let mut result = Self::default();
        for change in changes {
            let rows = change.native_mutation_dimensions()?;
            result.changes = sum(result.changes, 1)?;
            result.operations = sum(result.operations, rows.operations)?;
            result.predecessors = sum(result.predecessors, rows.predecessors)?;
            result.maximum_predecessors =
                result.maximum_predecessors.max(rows.maximum_predecessors);
            result.dependencies = sum(result.dependencies, change.deps().len())?;
            for actor in change.actors() {
                result.actors = sum(result.actors, 1)?;
                result.actor_bytes = sum(result.actor_bytes, actor.to_bytes().len())?;
            }
            result.key_bytes = sum(result.key_bytes, rows.key_bytes)?;
            result.value_bytes = sum(result.value_bytes, rows.value_bytes)?;
            result.mark_name_bytes = sum(result.mark_name_bytes, rows.mark_name_bytes)?;
            result.marks = sum(result.marks, rows.marks)?;
        }
        Ok(result)
    }
}
