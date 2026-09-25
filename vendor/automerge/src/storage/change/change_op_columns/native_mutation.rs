//! No ChangeOp or predecessor Vec is created by this counting pass.
use super::*;
use crate::change::native_mutation::NativeChangeDimensions;
use crate::resource_limits::NativeResourceError;
use crate::resource_limits::reconstruct::{product, sum};
use hexane::ColumnCursor;

type Result<T> = std::result::Result<T, NativeResourceError>;

fn expanded_strings(range: Range<usize>, data: &[u8]) -> Result<usize> {
    let bytes = data.get(range).ok_or(NativeResourceError::Invalid)?;
    let mut cursor = hexane::StrCursor::empty();
    let mut total = 0;
    while let Some(run) = cursor
        .try_next(bytes)
        .map_err(|_| NativeResourceError::Invalid)?
    {
        total = sum(
            total,
            product(run.count, run.value.as_deref().map(str::len).unwrap_or(0))?,
        )?;
    }
    Ok(total)
}

impl ChangeOpsColumns {
    pub(crate) fn native_mutation_dimensions(
        &self,
        data: &[u8],
        operations: usize,
    ) -> Result<NativeChangeDimensions> {
        let mut dimensions = NativeChangeDimensions {
            operations,
            key_bytes: expanded_strings(self.key.string_range().clone().into(), data)?,
            value_bytes: data
                .get(self.val.raw_range().as_ref().clone())
                .ok_or(NativeResourceError::Invalid)?
                .len(),
            mark_name_bytes: expanded_strings(self.mark_name.clone().into(), data)?,
            ..NativeChangeDimensions::default()
        };
        let mut rows = 0;
        for count in self.pred.group_range().decoder(data) {
            let count = count
                .map_err(|_| NativeResourceError::Invalid)?
                .ok_or(NativeResourceError::Invalid)?;
            let count = usize::try_from(count).map_err(|_| NativeResourceError::WorkBytes)?;
            dimensions.predecessors = sum(dimensions.predecessors, count)?;
            dimensions.maximum_predecessors = dimensions.maximum_predecessors.max(count);
            rows = sum(rows, 1)?;
        }
        if rows != operations {
            return Err(NativeResourceError::Invalid);
        }
        let mut rows = 0;
        for action in self.action.decoder(data) {
            if action.map_err(|_| NativeResourceError::Invalid)? == Some(7) {
                dimensions.marks = sum(dimensions.marks, 1)?;
            }
            rows = sum(rows, 1)?;
        }
        if rows != operations {
            return Err(NativeResourceError::Invalid);
        }
        Ok(dimensions)
    }
}
