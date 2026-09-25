//! qOS bounded admission (lane L1): requested heap bytes owned by an open
//! transaction, read from capacities. Allocates nothing; O(pending ops).
use super::TransactionInner;
use crate::op_set2::types::KeyRef;
use crate::types::OpId;
use crate::ChangeHash;
use std::borrow::Cow;
use std::mem::size_of;

impl TransactionInner {
    pub(crate) fn native_heap_bytes(&self) -> usize {
        let mut n = 0usize;
        let mut add = |v: usize| n = n.saturating_add(v);
        if let Some(m) = &self.message {
            add(m.capacity());
        }
        add(self.deps.capacity().saturating_mul(size_of::<ChangeHash>()));
        if let Some(scope) = &self.scope {
            add(scope.0.capacity().saturating_mul(size_of::<u32>()));
        }
        if let Some(author) = &self.author {
            // Author(Cow<[u8]>): an owned buffer has capacity ≤ 2× length.
            add(author.as_bytes().len().saturating_mul(2));
        }
        add(self
            .pending
            .capacity()
            .saturating_mul(size_of::<crate::op_set2::TxOp>()));
        for op in &self.pending {
            add(op
                .undo
                .capacity()
                .saturating_mul(size_of_val_undo(&op.undo)));
            if let KeyRef::Map(Cow::Owned(k)) = &op.bld.key {
                add(k.capacity());
            }
            add(op.bld.value.native_owned_bytes());
            if let Some(Cow::Owned(name)) = &op.bld.mark_name {
                add(name.capacity());
            }
            add(op.bld.pred.capacity().saturating_mul(size_of::<OpId>()));
        }
        n
    }
}

fn size_of_val_undo<T>(_: &[T]) -> usize {
    size_of::<T>()
}
