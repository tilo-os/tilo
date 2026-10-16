//! Take changes into an [`AutoCommit`] without growing it by a patch event per op.
//!
//! An `AutoCommit` owns a patch log that backs its incremental diff
//! ([`AutoCommit::update_diff_cursor`], [`AutoCommit::diff_incremental`]). It
//! passes that log to every apply path — [`AutoCommit::apply_changes`],
//! [`AutoCommit::apply_changes_batch`], [`AutoCommit::load_incremental`],
//! [`AutoCommit::merge`] and the sync wrapper's
//! [`SyncDoc::receive_sync_message`] — and to the view switches
//! [`AutoCommit::isolate`] and [`AutoCommit::integrate`]. Automerge (0.8
//! through at least 0.12) records an event per applied op into that log
//! whether or not it is active, and only an active log is ever drained
//! (`update_diff_cursor` truncates it). A document nobody diffs therefore
//! keeps one event for every op any peer ever sent it, for as long as it
//! lives: several times the size of the document itself.
//!
//! [`ApplyIncoming`] wraps each of those calls. When no diff cursor is armed
//! the log it leaves behind is dead, so it is replaced by an empty one
//! ([`AutoCommit::reset_diff_cursor`]); a document whose owner armed a cursor
//! keeps its log for the next diff. The raw methods are listed under
//! `disallowed-methods` in `clippy.toml`, so this trait is the one way in.
//!
//! [`Automerge`](automerge::Automerge) itself is not affected: its apply paths
//! log into a fresh inactive log per call and drop it.

#[cfg(feature = "heap-probe")]
pub mod heap;
pub mod read;
mod write;

pub use read::ReadTyped;
pub use write::WriteTyped;

use automerge::sync::{self, SyncDoc};
use automerge::{AutoCommit, AutomergeError, Change, ChangeHash};

/// The ways changes reach an [`AutoCommit`], each dropping the patch events
/// it logs unless the document has a diff cursor armed.
///
/// Every method has the signature and result of the automerge method it is
/// named after.
pub trait ApplyIncoming {
    /// [`AutoCommit::apply_changes`].
    ///
    /// # Errors
    ///
    /// As [`AutoCommit::apply_changes`].
    fn apply_incoming(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError>;

    /// [`AutoCommit::apply_changes_batch`].
    ///
    /// # Errors
    ///
    /// As [`AutoCommit::apply_changes_batch`].
    fn apply_incoming_batch(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError>;

    /// [`AutoCommit::load_incremental`]: the number of ops applied.
    ///
    /// # Errors
    ///
    /// As [`AutoCommit::load_incremental`].
    fn load_incoming(&mut self, data: &[u8]) -> Result<usize, AutomergeError>;

    /// [`AutoCommit::merge`]: the hashes of the changes taken from `other`.
    ///
    /// # Errors
    ///
    /// As [`AutoCommit::merge`].
    fn merge_incoming(&mut self, other: &mut AutoCommit)
    -> Result<Vec<ChangeHash>, AutomergeError>;

    /// [`SyncDoc::receive_sync_message`] through [`AutoCommit::sync`].
    ///
    /// # Errors
    ///
    /// As [`SyncDoc::receive_sync_message`].
    fn receive_incoming_sync(
        &mut self,
        state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError>;

    /// [`AutoCommit::isolate`]: view the document at `heads`.
    fn isolate_at(&mut self, heads: &[ChangeHash]);

    /// [`AutoCommit::integrate`]: leave an isolated view for the current heads.
    fn integrate_all(&mut self);
}

/// Drop the events an apply left in the patch log when no diff cursor will read them.
fn discard_unread_patches(doc: &mut AutoCommit) {
    if doc.diff_cursor().is_empty() {
        doc.reset_diff_cursor();
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one wrapper around automerge's apply paths"
)]
impl ApplyIncoming for AutoCommit {
    fn apply_incoming(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError> {
        let applied = self.apply_changes(changes);
        discard_unread_patches(self);
        applied
    }

    fn apply_incoming_batch(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError> {
        let applied = self.apply_changes_batch(changes);
        discard_unread_patches(self);
        applied
    }

    fn load_incoming(&mut self, data: &[u8]) -> Result<usize, AutomergeError> {
        let loaded = self.load_incremental(data);
        discard_unread_patches(self);
        loaded
    }

    fn merge_incoming(
        &mut self,
        other: &mut AutoCommit,
    ) -> Result<Vec<ChangeHash>, AutomergeError> {
        let merged = self.merge(other);
        discard_unread_patches(self);
        merged
    }

    fn receive_incoming_sync(
        &mut self,
        state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        let received = self.sync().receive_sync_message(state, message);
        discard_unread_patches(self);
        received
    }

    fn isolate_at(&mut self, heads: &[ChangeHash]) {
        self.isolate(heads);
        discard_unread_patches(self);
    }

    fn integrate_all(&mut self) {
        self.integrate();
        discard_unread_patches(self);
    }
}

/// Attribute exactly the newly inserted text to an author. Authorship marks do
/// not expand into text subsequently inserted by another collaborator.
///
/// # Errors
/// Returns the document error if the text object cannot be marked.
pub fn mark_text_author(
    doc: &mut impl automerge::transaction::Transactable,
    object: &automerge::ObjId,
    at: usize,
    inserted: &str,
    author: &str,
) -> Result<(), automerge::AutomergeError> {
    if inserted.is_empty() {
        return Ok(());
    }
    let len = match doc.text_encoding() {
        automerge::TextEncoding::Utf8CodeUnit => inserted.len(),
        automerge::TextEncoding::Utf16CodeUnit => inserted.encode_utf16().count(),
        _ => inserted.chars().count(),
    };
    doc.mark(
        object,
        automerge::marks::Mark::new("author".to_owned(), author, at, at.saturating_add(len)),
        automerge::marks::ExpandMark::None,
    )
}

#[cfg(test)]
mod tests {
    use super::ApplyIncoming;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc};

    fn remote_with(ops: u32) -> AutoCommit {
        let mut remote = AutoCommit::new();
        for i in 0..ops {
            remote
                .put(ROOT, format!("k{}", i % 7), i64::from(i))
                .unwrap();
        }
        remote.commit();
        remote
    }

    #[test]
    fn incoming_wrappers_leave_the_document_raw_automerge_would() {
        // Phase `every_path_leaves_the_document_it_would_have_without_the_wrapper`.
        {
            let mut remote = remote_with(20);
            let bytes = remote.save();
            let changes = remote.get_changes(&[]);

            let mut merged = AutoCommit::new();
            merged.merge_incoming(&mut remote).unwrap();
            let mut loaded = AutoCommit::new();
            assert_eq!(loaded.load_incoming(&bytes).unwrap(), 20);
            let mut applied = AutoCommit::new();
            applied.apply_incoming(changes.clone()).unwrap();
            let mut batched = AutoCommit::new();
            batched.apply_incoming_batch(changes).unwrap();

            let heads = remote.get_heads();
            for doc in [&mut merged, &mut loaded, &mut applied, &mut batched] {
                assert_eq!(doc.get_heads(), heads);
                assert_eq!(doc.keys(ROOT).count(), 7);
            }
        }
        // Phase `sync_reaches_the_peer`.
        {
            let mut remote = remote_with(5);
            let mut local = AutoCommit::new();
            let (mut ours, mut theirs) =
                (automerge::sync::State::new(), automerge::sync::State::new());
            for _ in 0..8 {
                let to_local =
                    automerge::sync::SyncDoc::generate_sync_message(&remote.sync(), &mut theirs);
                if let Some(message) = to_local {
                    local.receive_incoming_sync(&mut ours, message).unwrap();
                }
                let to_remote =
                    automerge::sync::SyncDoc::generate_sync_message(&local.sync(), &mut ours);
                if let Some(message) = to_remote {
                    remote.receive_incoming_sync(&mut theirs, message).unwrap();
                }
            }
            assert_eq!(local.get_heads(), remote.get_heads());
        }
        // Phase `an_armed_diff_cursor_still_sees_the_incoming_changes`.
        {
            let mut local = AutoCommit::new();
            local.put(ROOT, "seed", 1).unwrap();
            local.update_diff_cursor();
            let mut remote = local.fork();
            remote.put(ROOT, "from_remote", 2).unwrap();
            remote.commit();

            local.merge_incoming(&mut remote).unwrap();

            let patches = local.diff_incremental();
            assert_eq!(patches.len(), 1, "{patches:?}");
        }
        // Phase `isolating_and_integrating_keep_their_views`.
        {
            let mut doc = remote_with(3);
            let early = doc.get_heads();
            doc.put(ROOT, "late", 1).unwrap();
            doc.commit();
            doc.isolate_at(&early);
            assert!(doc.get(ROOT, "late").unwrap().is_none());
            doc.integrate_all();
            assert!(doc.get(ROOT, "late").unwrap().is_some());
        }
    }
}
