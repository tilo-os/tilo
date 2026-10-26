//! Changing a record now, or learning why it can't be changed yet.

use automerge::AutoCommit;

use super::{RecordId, RecordStore};

/// Why a record could not be changed now ([`RecordStore::with_history_or_why`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryUnavailable {
    /// Something reads or changes the store right now.
    #[error("the record's store is in use")]
    StoreInUse,
    /// The record is lent to a frame running now.
    #[error("the record is in use")]
    Lent,
    /// The record is still being created.
    #[error("the record is still being created")]
    Creating,
    /// The record is not open in this store.
    #[error("the record is not open")]
    NotOpen,
}

impl HistoryUnavailable {
    /// Whether the record can be changed once what holds it now lets go.
    #[must_use]
    pub fn is_momentary(self) -> bool {
        !matches!(self, Self::NotOpen)
    }
}

impl RecordStore {
    /// [`Self::with_history`], saying why the record could not be changed
    /// now: the store is in use, the record is lent to a frame or still
    /// being created, or it is not open here.
    ///
    /// # Errors
    /// Why `f` did not run.
    pub fn with_history_or_why<R>(
        &self,
        id: &RecordId,
        f: impl FnOnce(&mut AutoCommit, &mut crate::state::UndoLog) -> R,
    ) -> Result<R, HistoryUnavailable> {
        let mut set = self
            .0
            .try_borrow_mut()
            .map_err(|_| HistoryUnavailable::StoreInUse)?;
        if set.lent.contains(id) {
            return Err(HistoryUnavailable::Lent);
        }
        if set.pending.contains_key(id) {
            return Err(HistoryUnavailable::Creating);
        }
        let (doc, history) = set
            .document_and_history_mut(id)
            .ok_or(HistoryUnavailable::NotOpen)?;
        Ok(f(doc, history))
    }
}
