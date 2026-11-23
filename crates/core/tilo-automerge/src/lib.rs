//! Take changes into an [`AutoCommit`] without growing it by a patch event per op.
//!
//! An `AutoCommit` records an event per applied op into its patch log whether or not a
//! diff cursor is armed, and only an armed log is ever drained; a document nobody diffs
//! therefore keeps one event for every op any peer ever sent it, for as long as it lives.
//!
//! [`ApplyIncoming`] wraps each apply path: with no cursor armed the log it leaves behind
//! is dead and is replaced by an empty one ([`AutoCommit::reset_diff_cursor`]), while a
//! document whose owner armed a cursor keeps its log for the next diff. The raw methods
//! are listed under `disallowed-methods` in `clippy.toml`, so this trait is the one way
//! in.

#[cfg(feature = "heap-probe")]
pub mod heap;
pub mod items;
pub mod json;
pub mod load_hint;
pub mod read;
pub mod rich_text;
pub mod shared;
pub mod whole;
mod write;

pub use read::ReadTyped;
pub use write::WriteTyped;

use automerge::sync::{self, SyncDoc};
use automerge::{AutoCommit, AutomergeError, Change, ChangeHash, ReadDoc as _};

/// The ways changes reach an [`AutoCommit`], each dropping the patch events
/// it logs unless the document has a diff cursor armed.
///
/// Every method has the signature and result of the automerge method it is
/// named after.
pub trait ApplyIncoming {
    fn apply_incoming(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError>;

    fn apply_incoming_batch(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError>;

    fn load_incoming(&mut self, data: &[u8]) -> Result<usize, AutomergeError>;

    /// [`Self::load_incoming`] of `data` whole or not at all: every chunk of
    /// it must parse ([`whole::check`]) before any is applied. Changes whose
    /// dependencies are neither here nor in `data` are queued, as
    /// [`AutoCommit::load_incremental`] queues them.
    ///
    /// # Errors
    ///
    /// A chunk that does not parse (the document is unchanged), or as
    /// [`AutoCommit::load_incremental`].
    fn load_incoming_whole(&mut self, data: &[u8]) -> Result<usize, AutomergeError> {
        whole::check(data)?;
        self.load_incoming(data)
    }

    fn merge_incoming(&mut self, other: &mut AutoCommit)
    -> Result<Vec<ChangeHash>, AutomergeError>;

    /// Take in `loaded`, a whole document another thread loaded, with work
    /// proportional to what it adds: a document that holds nothing yet
    /// becomes it (under its own actor and author, leaving `loaded` empty),
    /// and one that holds some of it gains only the changes it lacks. Whether
    /// the document gained anything.
    ///
    /// # Errors
    ///
    /// As [`Self::merge_incoming`].
    fn take_loaded(&mut self, loaded: &mut AutoCommit) -> Result<bool, AutomergeError>;

    fn receive_incoming_sync(
        &mut self,
        state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError>;

    fn isolate_at(&mut self, heads: &[ChangeHash]);

    fn integrate_all(&mut self);
}

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

    fn take_loaded(&mut self, loaded: &mut AutoCommit) -> Result<bool, AutomergeError> {
        let theirs = loaded.get_heads();
        // Closes an open transaction first, as every other apply path does.
        if self.get_missing_deps(&theirs).is_empty() {
            return Ok(false);
        }
        if self.is_empty() && self.text_encoding() == loaded.text_encoding() {
            // Nothing to merge with: rebuilding every change into this
            // document would cost what loading it did.
            let mut adopted = std::mem::take(loaded);
            adopted.set_actor(self.get_actor().clone());
            adopted.set_author(self.get_author().cloned());
            *self = adopted;
            return Ok(true);
        }
        self.merge_incoming(loaded)?;
        Ok(true)
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
    let len = author_width(doc.text_encoding(), inserted);
    doc.mark(
        object,
        automerge::marks::Mark::new("author".to_owned(), author, at, at.saturating_add(len)),
        automerge::marks::ExpandMark::None,
    )
}

/// A new text holding `text`, written by `author`: made with the mark
/// [`mark_text_author`] sets on it once written, so a batch that makes it
/// writes the mark with it rather than an operation after.
#[must_use]
pub fn authored_text(
    encoding: automerge::TextEncoding,
    text: &str,
    author: &str,
) -> automerge::hydrate::Text {
    let made = automerge::hydrate::Text::new(encoding, text);
    if text.is_empty() {
        return made;
    }
    made.with_mark(
        automerge::marks::Mark::new("author".to_owned(), author, 0, author_width(encoding, text)),
        automerge::marks::ExpandMark::None,
    )
}

/// The width of `text` in `encoding`'s units, as an author mark counts it.
fn author_width(encoding: automerge::TextEncoding, text: &str) -> usize {
    match encoding {
        automerge::TextEncoding::Utf8CodeUnit => text.len(),
        automerge::TextEncoding::Utf16CodeUnit => text.encode_utf16().count(),
        _ => text.chars().count(),
    }
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

    #[test]
    fn a_loaded_document_is_taken_in_with_work_for_what_it_adds() {
        let mut workspace = remote_with(40);
        let bytes = workspace.save();

        // A document holding nothing becomes the loaded one, under its own actor.
        let mut empty = AutoCommit::new();
        let actor = empty.get_actor().clone();
        assert!(
            empty
                .take_loaded(&mut AutoCommit::load(&bytes).unwrap())
                .unwrap()
        );
        assert_eq!(empty.get_heads(), workspace.get_heads());
        assert_eq!(empty.get_actor(), &actor);

        // One that holds all of it gains nothing.
        let mut same = workspace.fork();
        assert!(
            !same
                .take_loaded(&mut AutoCommit::load(&bytes).unwrap())
                .unwrap()
        );

        // One that holds part of it, and a change of its own, keeps both.
        let mut owner = workspace.fork();
        owner.put(ROOT, "mine", 1).unwrap();
        owner.commit();
        workspace.put(ROOT, "theirs", 2).unwrap();
        workspace.commit();
        let actor = owner.get_actor().clone();
        assert!(
            owner
                .take_loaded(&mut AutoCommit::load(&workspace.save()).unwrap())
                .unwrap()
        );
        assert!(owner.get(ROOT, "mine").unwrap().is_some());
        assert!(owner.get(ROOT, "theirs").unwrap().is_some());
        assert_eq!(owner.get_actor(), &actor);
        assert!(owner.get_missing_deps(&workspace.get_heads()).is_empty());
    }
}
