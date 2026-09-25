//! Read-only sequence traversal, heads and counts; no admission or mutation.
use super::*;
use crate::transaction::Transactable;

impl<'doc> NativeReadSession<'doc, '_> {
    pub fn pending_ops(&self) -> usize {
        self.document.pending_ops()
    }

    pub fn operation_count(&self) -> usize {
        self.document.doc.ops.len()
    }

    /// Counts the complete current committed frontier before delivering any
    /// hashes. Refuses pending operations; never commits or emits a prefix.
    pub fn visit_heads(
        &self,
        maximum: usize,
        mut visitor: impl FnMut(crate::ChangeHash),
    ) -> Result<usize, NativeResourceError> {
        if self.pending_ops() != 0 {
            return Err(NativeResourceError::Unaccounted);
        }
        let bound = maximum
            .checked_add(1)
            .ok_or(NativeResourceError::Collection)?;
        let graph = self.document.doc.changes();
        let count = graph.heads().take(bound).count();
        if count > maximum {
            return Err(NativeResourceError::Collection);
        }
        for hash in graph.heads() {
            visitor(hash);
        }
        Ok(count)
    }

    /// Traverses one actual List/Text object; a stop may have consumed one
    /// operation of lookahead to identify the preceding insertion group.
    pub fn visit_list<E>(
        &self,
        object: NativeObject<'doc>,
        steps: &NativeReadSteps,
        mut visitor: impl for<'value> FnMut(NativeListEntry<'value, 'doc>) -> Result<ControlFlow<()>, E>,
    ) -> Result<usize, NativeReadError<E>> {
        let mut query = self.query(object, None, true, Some(steps))?;
        let mut started = false;
        let mut visible = 0usize;
        let mut index = 0usize;
        while let Some(op) = self.next_op(&mut query, Some(steps))? {
            // ListDiff groups by the cumulative insertion column. A new true
            // insertion starts the next group even if the prior one is deleted.
            if op.insert {
                if let Some(winner) = query.workspace.winner.take() {
                    let stop = self.list_entry(&winner, index, visible > 1, steps, &mut visitor)?;
                    index = index
                        .checked_add(1)
                        .ok_or(NativeResourceError::Operations)?;
                    if stop.is_break() {
                        return Ok(index);
                    }
                }
                visible = 0;
                started = true;
            } else if !started {
                return Err(NativeResourceError::Invalid.into());
            }
            if self.visible(&op, Some(steps))?
                && !matches!(op.action, Action::Delete | Action::Increment)
            {
                visible = visible
                    .checked_add(1)
                    .ok_or(NativeResourceError::Operations)?;
                query.workspace.winner = Some(op);
            }
        }
        if let Some(winner) = query.workspace.winner.take() {
            // The traversal ends here whether or not the visitor asks to stop.
            let _ = self.list_entry(&winner, index, visible > 1, steps, &mut visitor)?;
            index = index
                .checked_add(1)
                .ok_or(NativeResourceError::Operations)?;
        }
        Ok(index)
    }

    fn list_entry<E>(
        &self,
        op: &Op<'doc>,
        index: usize,
        conflict: bool,
        steps: &NativeReadSteps,
        visitor: &mut impl for<'value> FnMut(
            NativeListEntry<'value, 'doc>,
        ) -> Result<ControlFlow<()>, E>,
    ) -> Result<ControlFlow<()>, NativeReadError<E>> {
        let mut flow = ControlFlow::Continue(());
        self.visit_op(op, Some(steps), true, &mut |value, id| {
            flow = visitor(NativeListEntry {
                index,
                value,
                id,
                is_mark: op.action == Action::Mark,
                conflict,
            })?;
            Ok(())
        })?;
        Ok(flow)
    }
}
