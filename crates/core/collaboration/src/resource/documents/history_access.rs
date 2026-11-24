//! Changing a record now, or learning why it can't be changed yet.

use automerge::AutoCommit;

use super::{DocumentId, ResourceStore};

/// Why a record could not be changed now ([`ResourceStore::with_history_or_why`]).
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

impl ResourceStore {
    /// [`Self::with_history`], saying why the record could not be changed
    /// now: the store is in use, the record is lent to a frame or still
    /// being created, or it is not open here.
    ///
    /// # Errors
    /// Why `f` did not run.
    pub fn with_history_or_why<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut AutoCommit, &mut collab_types::history::UndoLog) -> R,
    ) -> Result<R, HistoryUnavailable> {
        let mut f = Some(f);
        let mut out = None;
        self.with_history_or_why_with(id, &mut |doc, history| {
            if let Some(f) = f.take() {
                out = Some(f(doc, history));
            }
        })?;
        out.ok_or(HistoryUnavailable::NotOpen)
    }

    /// [`Self::with_history_or_why`] with the change erased: one body for
    /// every change.
    #[inline(never)]
    fn with_history_or_why_with(
        &self,
        id: &DocumentId,
        f: &mut dyn FnMut(&mut AutoCommit, &mut collab_types::history::UndoLog),
    ) -> Result<(), HistoryUnavailable> {
        let mut set = self
            .try_set_mut()
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
        f(doc, history);
        Ok(())
    }
}
