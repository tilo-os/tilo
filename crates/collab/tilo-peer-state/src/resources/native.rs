//! Documents a bounded peer restores from stored or received bytes.
//!
//! Every document is admitted through `tilo_automerge_isolate`, and one
//! operation's documents share an aggregate byte limit.
use automerge::AutoCommit;

use crate::room::read::LegacyRead;

/// Why a document was not restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// The operation's aggregate byte limit is spent.
    Capacity,
    /// The isolated load refused the bytes.
    Rejected(tilo_automerge_isolate::Rejected),
}

/// Loads documents against one operation's remaining byte limit. Dropping a
/// loaded document does not refund it.
pub(crate) struct NativeBatch<'a> {
    remaining: Remaining<'a>,
}
enum Remaining<'a> {
    #[cfg(test)]
    Owned(usize),
    Continuing(&'a mut usize),
}
impl NativeBatch<'_> {
    #[cfg(test)]
    pub(crate) fn new(limit: usize) -> NativeBatch<'static> {
        NativeBatch {
            remaining: Remaining::Owned(limit),
        }
    }
    fn remaining(&mut self) -> &mut usize {
        match &mut self.remaining {
            #[cfg(test)]
            Remaining::Owned(value) => value,
            Remaining::Continuing(value) => value,
        }
    }
    pub(crate) fn load(&mut self, bytes: &[u8]) -> Result<NativeDocument, LoadError> {
        let remaining = self.remaining();
        *remaining = remaining
            .checked_sub(bytes.len())
            .ok_or(LoadError::Capacity)?;
        let mut document = tilo_automerge_isolate::load(bytes).map_err(LoadError::Rejected)?;
        let heads = document.get_heads();
        Ok(NativeDocument { document, heads })
    }
}
impl<'a> NativeBatch<'a> {
    /// Draw on an operation's one continuing limit.
    pub(crate) fn continuing(remaining: &'a mut usize) -> Self {
        Self {
            remaining: Remaining::Continuing(remaining),
        }
    }
}

/// An admitted document, read-only for the bounded peer.
pub(crate) struct NativeDocument {
    document: AutoCommit,
    heads: Vec<automerge::ChangeHash>,
}
impl NativeDocument {
    pub(crate) fn read(&self) -> LegacyRead<'_, AutoCommit> {
        LegacyRead(&self.document)
    }
    pub(crate) fn document(&self) -> &AutoCommit {
        &self.document
    }
    pub(crate) fn heads(&self) -> &[automerge::ChangeHash] {
        &self.heads
    }
    pub(crate) fn has_missing_dependencies(&self) -> bool {
        !automerge::ReadDoc::get_missing_deps(&self.document, &[]).is_empty()
    }
}

#[cfg(test)]
mod tests;
