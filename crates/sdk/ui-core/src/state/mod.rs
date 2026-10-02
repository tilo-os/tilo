use crate::application::InstanceId;
use crate::permissions::UserId;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, PatchAction, ReadDoc, Value};
use std::collections::HashMap;
use tilo_automerge::ApplyIncoming;

mod built_on;
pub(crate) mod owned_restore;
mod sequence_replay;
use sequence_replay::SequenceReplay;
#[cfg(test)]
pub(crate) mod owned_restore_tests;

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};
pub mod diagnostics;
pub use diagnostics::{DocumentDiagnostics, FrontierDiagnostics};
pub mod revision;
pub use revision::{Revision, RevisionHistory, RevisionId, UnknownRevision};

/// An explicit outbound synchronization frontier for one document route.
///
/// Automerge's `save_incremental` cursor is process-global to an `AutoCommit`:
/// any full save, debug export, or second transport consumer may advance it.
/// A frontier instead names the exact heads one destination is known to have,
/// so serialization is independent of unrelated callers and can later be
/// advanced from an acknowledgement rather than merely from a send attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentFrontier {
    heads: Vec<ChangeHash>,
}

impl DocumentFrontier {
    /// Start at the document's current heads. Existing content is treated as
    /// the baseline; late joiners acquire it through the snapshot handshake.
    pub fn at_document(doc: &mut AutoCommit) -> Self {
        Self {
            heads: doc.get_heads(),
        }
    }

    /// Serialize everything after this frontier and advance it to the heads
    /// represented by that payload. Unlike `save_incremental`, this does not
    /// share hidden mutable cursor state with persistence or snapshot export.
    pub fn take_delta(&mut self, doc: &mut AutoCommit) -> Vec<u8> {
        let (delta, current) = self.pending_delta(doc);
        self.heads = current;
        delta
    }

    /// Build a delta without advancing the frontier. Call [`Self::advance`]
    /// only after the durable transport accepts it; a saturated/disconnected
    /// queue can then be retried instead of silently losing the mutation.
    pub fn pending_delta(&mut self, doc: &mut AutoCommit) -> (Vec<u8>, Vec<ChangeHash>) {
        let current = doc.get_heads();
        if current == self.heads {
            return (Vec::new(), current);
        }
        let delta = doc.save_after(&self.heads);
        // Multiple observed frontiers may already cover the whole graph even
        // when their vector is not Automerge's minimal current-head set.
        if delta.is_empty() {
            self.heads.clone_from(&current);
        }
        (delta, current)
    }

    /// Advance to heads whose delta a durable destination accepted.
    pub fn advance(&mut self, heads: Vec<ChangeHash>) {
        self.heads = heads;
    }

    /// Treat the document's current state as already observed on this route.
    /// Used after applying inbound data so it is not echoed back as a new
    /// outbound mutation.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.heads = doc.get_heads();
    }

    /// Record changes known to have arrived from this destination without
    /// claiming that it received locally pending changes. The union frontier
    /// lets `save_after` omit echoes while retaining a rejected local delta.
    pub fn observe(&mut self, observed: impl IntoIterator<Item = ChangeHash>) {
        for hash in observed {
            if !self.heads.contains(&hash) {
                self.heads.push(hash);
            }
        }
    }

    fn observe_changes_after(&mut self, doc: &mut AutoCommit, before: &[ChangeHash]) {
        self.observe(
            doc.get_changes(before)
                .into_iter()
                .map(|change| change.hash()),
        );
    }

    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }
}

/// A CRDT document paired with the explicit frontier for one collaboration
/// route. This is the small, undo-free replica used by metadata documents such
/// as room indexes; application documents add history through [`State`].
pub struct DocumentReplica {
    doc: AutoCommit,
    outbound: DocumentFrontier,
}

impl DocumentReplica {
    #[must_use]
    pub fn new(mut doc: AutoCommit) -> Self {
        let outbound = DocumentFrontier::at_document(&mut doc);
        Self { doc, outbound }
    }

    #[must_use]
    pub fn document(&self) -> &AutoCommit {
        &self.doc
    }

    pub fn document_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }

    pub fn pending_outbound_delta(&mut self) -> (Vec<u8>, Vec<ChangeHash>) {
        self.outbound.pending_delta(&mut self.doc)
    }

    pub fn advance_outbound(&mut self, heads: Vec<ChangeHash>) {
        self.outbound.advance(heads);
    }

    /// Apply incremental Automerge bytes and retain them in the outbound frontier.
    ///
    /// # Errors
    ///
    /// Returns an error when `data` is not a valid incremental Automerge payload
    /// for this document.
    pub fn load_incremental(&mut self, data: &[u8]) -> Result<usize, automerge::AutomergeError> {
        let before = self.doc.get_heads();
        let loaded = self.doc.load_incoming(data)?;
        self.outbound.observe_changes_after(&mut self.doc, &before);
        Ok(loaded)
    }

    /// Merge a complete Automerge snapshot and retain its observed frontier.
    ///
    /// # Errors
    ///
    /// Returns an error when `data` is not a valid Automerge snapshot or cannot
    /// be merged into this document.
    pub fn merge_snapshot(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        let mut incoming = AutoCommit::load(data)?;
        let observed = incoming.get_heads();
        self.doc.merge_incoming(&mut incoming)?;
        self.outbound.observe(observed);
        Ok(())
    }
}

/// Optional action-based undo/redo payloads for outside-consequence spans.
#[derive(Clone, Debug, Default)]
pub(crate) struct CommittedSpanInverse {
    pub undo_via: Option<tilo_actions::EncodedAction>,
    pub redo_via: Option<tilo_actions::EncodedAction>,
}

/// One reversible action on the undo/redo stacks.
///
/// **Collab-safe, change-based undo.** Rather than snapshotting the whole
/// document — which on undo would replace the live `AutoCommit` and silently
/// drop any concurrently-merged remote ops — an entry records only the heads
/// that bracket the local action (`before`/`after`); undo recomputes the
/// inverse and writes it as a new forward change, so concurrent remote edits
/// survive.
#[derive(Clone)]
pub struct UndoEntry {
    pub before: Vec<ChangeHash>,
    pub after: Vec<ChangeHash>,
    pub focused_node_idx: Option<usize>,
    pub widget_metadata: Option<String>,
    pub author: Option<UserId>,
    /// Runtime instance attribution, independent of authenticated author/CRDT actor.
    pub instance_id: Option<InstanceId>,
    /// When the forward action's [`tilo_actions::Action::inverse_before`]
    /// returned a compensating document action (an outbox / del_outbox /
    /// filesystem queue writer), reverting this entry applies `undo_via`
    /// through the registry instead of Automerge inverse patches — so the
    /// compensating queue entry is recorded the same way peers replay it.
    /// `redo_via` is the forward action, for the opposite stack after undo.
    /// Both are `None` for pure CRDT actions.
    pub undo_via: Option<tilo_actions::EncodedAction>,
    pub redo_via: Option<tilo_actions::EncodedAction>,
}

/// Per-document undo and redo depth. Entries retain two Automerge head sets
/// plus optional widget metadata, so an unbounded editing session otherwise
/// retains memory forever. Five hundred and twelve actions is deep enough for
/// interactive use while giving the framework a strict upper bound.
const UNDO_HISTORY_LIMIT: usize = 512;

fn push_history(stack: &mut Vec<UndoEntry>, entry: UndoEntry) {
    if stack.len() >= UNDO_HISTORY_LIMIT {
        stack.remove(0);
    }
    stack.push(entry);
}

/// Separated from [`State`] because a history belongs to a *document*, not a
/// process — a stack shared between two documents would revert against the
/// wrong one, as a second tab sharing the first tab's undo stack would.
#[derive(Default)]
pub struct UndoLog {
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// Heads as of the last commit boundary — the `before` for the next action.
    last_heads: Vec<ChangeHash>,
    /// Bumped every time a stack changes. Lets a host tell "the history moved"
    /// from "same as last frame" without reading the document — which matters
    /// because describing an entry reads `AutoCommit::diff`, and that closes
    /// the open transaction. See [`Self::revision`].
    revision: u64,
}

/// History line for a compensating action, from its encoded payload. The
/// variant name is the same label a door commit would derive.
fn history_message_for_encoded_action(encoded: &tilo_actions::EncodedAction) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(&encoded.payload).ok()?;
    tilo_actions::ActionContext::for_action(&encoded.type_id, &encoded.type_id, &value)
        .change_message()
}

impl UndoLog {
    /// An empty history over `doc` as it stands. The current heads become
    /// the baseline the next action brackets against, so content already
    /// in the document is not attributed to an action nobody performed.
    pub fn new(doc: &mut AutoCommit) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_heads: doc.get_heads(),
            revision: 0,
        }
    }

    /// A counter that changes whenever the undo or redo stack does, and never
    /// otherwise.
    ///
    /// The cheap "has anything to undo changed?" test. A caller that renders
    /// the history — an Undo button's enabled state, a description of the next
    /// undo — recomputes only when this moves, instead of interrogating the
    /// document every frame.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether another distinct stack revision can be issued.
    fn can_bump(&self) -> bool {
        self.revision != u64::MAX
    }

    /// Record that a stack changed. Called from every mutation below; if a new
    /// one forgets, a control renders stale. Callers must reserve capacity with
    /// [`Self::can_bump`] before changing a stack, so revision exhaustion never
    /// wraps or leaves a half-applied undo transition behind.
    fn bump(&mut self) {
        if let Some(next) = self.revision.checked_add(1) {
            self.revision = next;
        }
    }

    /// Re-baseline against the document's current heads, without touching
    /// the stacks. What a merge of remote changes needs: existing entries
    /// keep naming their own changes (still valid addresses in an
    /// append-only graph) while the *next* local action brackets against
    /// the merged picture.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.last_heads = doc.get_heads();
    }

    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.last()
    }

    /// How many entries undo can pop, whoever authored them.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// How many entries redo can pop, whoever authored them.
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Index of the entry [`undo_as`](Self::undo_as) would pop for `author`.
    ///
    /// The *one* place the author-scoping rule lives. Both the query half
    /// ([`next_undo_as`](Self::next_undo_as), which decides whether an Undo
    /// control paints enabled) and the acting half
    /// ([`undo_for_author`](Self::undo_for_author)) read it, so a control can
    /// never offer an undo the shortcut would decline.
    fn undo_index_for(&self, author: Option<&UserId>) -> Option<usize> {
        match author {
            Some(a) => self
                .undo_stack
                .iter()
                .rposition(|e| Self::claimable_by(e, a)),
            None => self.undo_stack.len().checked_sub(1),
        }
    }

    /// Index of the entry [`redo_as`](Self::redo_as) would pop for `author`.
    /// The redo mirror of [`undo_index_for`](Self::undo_index_for).
    fn redo_index_for(&self, author: Option<&UserId>) -> Option<usize> {
        match author {
            Some(a) => self
                .redo_stack
                .iter()
                .rposition(|e| Self::claimable_by(e, a)),
            None => self.redo_stack.len().checked_sub(1),
        }
    }

    /// The entry a Ctrl+Z by `author` would revert, without reverting it.
    ///
    /// Author-scoped for the same reason [`undo_as`](Self::undo_as) is: with an
    /// identity, undo skips past other people's actions, so a plain
    /// "is the stack non-empty?" would light up an Undo button in a session
    /// where every entry belongs to a collaborator and Ctrl+Z would do nothing.
    pub fn next_undo_as(&self, author: Option<&UserId>) -> Option<&UndoEntry> {
        self.undo_index_for(author)
            .and_then(|i| self.undo_stack.get(i))
    }

    /// The entry a Ctrl+Shift+Z by `author` would re-apply. See
    /// [`next_undo_as`](Self::next_undo_as).
    pub fn next_redo_as(&self, author: Option<&UserId>) -> Option<&UndoEntry> {
        self.redo_index_for(author)
            .and_then(|i| self.redo_stack.get(i))
    }

    /// Whether `author` has anything to undo — true exactly when
    /// [`undo_as`](Self::undo_as) would return `Some`.
    pub fn can_undo_as(&self, author: Option<&UserId>) -> bool {
        self.undo_index_for(author).is_some()
    }

    /// Whether `author` has anything to redo — true exactly when
    /// [`redo_as`](Self::redo_as) would return `Some`.
    pub fn can_redo_as(&self, author: Option<&UserId>) -> bool {
        self.redo_index_for(author).is_some()
    }

    /// Commit `doc`'s open transaction and push an entry bracketing it.
    /// Returns `true` iff this committed a change (there were pending
    /// ops). `false` means the transaction was empty — the caller can then skip
    /// any persistence / broadcast work, since the document is byte-identical to
    /// before. This is what lets an ephemeral, non-document interaction (a text
    /// selection drag, a focus move) avoid a full `doc.save()` on every event.
    pub fn commit_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.commit_action_authored_with_message(doc, focused_node_idx, metadata, author, None)
    }

    /// As [`Self::commit_action_authored`], stamping semantic provenance onto
    /// native CRDT edits that deliberately do not travel through an action
    /// envelope (for example, co-edited flowing text).
    pub fn commit_action_authored_with_message(
        &mut self,
        doc: &mut AutoCommit,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        message: Option<String>,
    ) -> bool {
        // Do this before committing the open Automerge transaction. At the
        // limit we must leave both the document's commit boundary and the
        // history stacks untouched rather than make a change widgets cannot
        // distinguish from an earlier one.
        if !self.can_bump() {
            return false;
        }
        self.commit_owned_pending(
            doc,
            self.last_heads.clone(),
            owned_restore::OwnedActionAttribution {
                author,
                instance_id: None,
                focused_node_idx,
                widget_metadata: metadata,
                message,
            },
        )
        .changed
    }

    /// Record an action already committed into `doc` out-of-band, bracketed
    /// by the caller-supplied `before` heads. See
    /// [`State::record_committed_action`].
    ///
    /// The entry lands unattributed, which an author-scoped Ctrl+Z *claims*
    /// rather than skips — see `claimable_by`. A caller
    /// that knows the change came from someone else must name them through
    /// [`record_committed_action_authored`](Self::record_committed_action_authored),
    /// or the local user's undo will reach an edit that was never theirs.
    pub fn record_committed_action(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) -> bool {
        self.record_committed_action_authored(doc, before, focused_node_idx, metadata, None)
    }

    /// Observe the recorded committed frontier without closing pending work.
    pub(crate) fn matches_recorded_frontier(&self, doc: &AutoCommit) -> bool {
        owned_restore::same_frontier(&self.last_heads, &doc.base_heads())
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a
    /// change that reached this document from *outside* the window (an agent,
    /// or a collaborator's own published operation), applied under the
    /// caller's own actor with no local commit boundary. Without recording it
    /// here, the action is unreachable and `last_heads` stays behind it, so
    /// the next local edit's bracket contains someone else's work and one
    /// Ctrl+Z reverts both. The name is what keeps them apart afterwards.
    pub fn record_committed_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.record_committed_action_owned(doc, before, focused_node_idx, metadata, author, None)
    }

    /// Record a bound action without clearing another instance's redo history.
    pub fn record_committed_action_in(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: UserId,
        instance: InstanceId,
    ) -> bool {
        self.record_committed_action_owned(
            doc,
            before,
            focused_node_idx,
            metadata,
            Some(author),
            Some(instance),
        )
    }

    fn record_committed_action_owned(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        instance_id: Option<InstanceId>,
    ) -> bool {
        let after = doc.get_heads();
        self.record_committed_span_owned(
            before,
            after,
            focused_node_idx,
            metadata,
            author,
            instance_id,
        )
    }

    pub(crate) fn record_committed_span_in(
        &mut self,
        before: Vec<ChangeHash>,
        after: Vec<ChangeHash>,
        metadata: Option<String>,
        author: UserId,
        instance: InstanceId,
    ) -> bool {
        self.record_committed_span_owned(
            before,
            after,
            None,
            metadata,
            Some(author),
            Some(instance),
        )
    }

    fn record_committed_span_owned(
        &mut self,
        before: Vec<ChangeHash>,
        after: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        instance_id: Option<InstanceId>,
    ) -> bool {
        self.record_committed_span_with_inverse(
            before,
            after,
            focused_node_idx,
            metadata,
            author,
            instance_id,
            CommittedSpanInverse::default(),
        )
    }

    /// As [`Self::record_committed_span_owned`], carrying optional
    /// action-based undo/redo for outside-consequence actions.
    pub(crate) fn record_committed_span_with_inverse(
        &mut self,
        before: Vec<ChangeHash>,
        after: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        instance_id: Option<InstanceId>,
        inverse: CommittedSpanInverse,
    ) -> bool {
        if after == before {
            // Nothing to undo, but the baseline moves up all the same — same
            // reason as the empty-transaction path in `commit_action_authored`.
            self.last_heads = after;
            return false;
        }
        if !self.can_bump() {
            return false;
        }
        push_history(
            &mut self.undo_stack,
            UndoEntry {
                before,
                after: after.clone(),
                focused_node_idx,
                widget_metadata: metadata,
                author,
                instance_id,
                undo_via: inverse.undo_via,
                redo_via: inverse.redo_via,
            },
        );
        if let Some(instance) = instance_id {
            self.redo_stack
                .retain(|entry| entry.instance_id != Some(instance));
        } else {
            self.redo_stack.clear();
        }
        self.last_heads = after;
        self.bump();
        true
    }

    /// Apply the inverse of `[entry.before, entry.after]` as a NEW forward
    /// change merged into `doc`, returning the `[before, after]` heads of
    /// that inverse so the opposite stack can later invert *it* (redo, or
    /// re-undo). Concurrent remote ops merged in since `entry.after` are
    /// untouched — only what the local action itself changed is rewritten.
    ///
    /// When `entry.undo_via` is set and `registry` can decode it, that
    /// document action is applied instead of Automerge inverse patches, so an
    /// outside consequence (outbox / del_outbox) is recorded through `apply`.
    fn apply_inverse(
        &mut self,
        doc: &mut AutoCommit,
        entry: &UndoEntry,
        message: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> (Vec<ChangeHash>, Vec<ChangeHash>) {
        let inverse_before = doc.get_heads();
        let via_action = entry
            .undo_via
            .as_ref()
            .zip(registry)
            .and_then(|(encoded, registry)| registry.decode(encoded).ok());
        // The compensating action is a document change of its own, named in
        // history the way the forward action was. A later move of this
        // record into another room carries that change with the rest of the
        // graph; a collaborator syncing the record sees the same queue write.
        let history_message = if let Some(action) = via_action {
            if action.apply(doc).is_err() {
                write_revert_patches(doc, &entry.after, &entry.before);
                message
            } else {
                message.or_else(|| {
                    entry
                        .undo_via
                        .as_ref()
                        .and_then(history_message_for_encoded_action)
                })
            }
        } else {
            write_revert_patches(doc, &entry.after, &entry.before);
            message
        };
        let _ = tilo_resource::history::commit_stamped_with_message(doc, history_message);
        let inverse_after = doc.get_heads();
        self.last_heads.clone_from(&inverse_after);
        (inverse_before, inverse_after)
    }

    pub fn undo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_with_registry(doc, current_focused_idx, current_metadata, None)
    }

    /// [`Self::undo`] with a registry so action-based outside-consequence
    /// undo can decode `undo_via`.
    pub fn undo_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        let index = self.undo_index_for(None)?;
        self.undo_at(doc, index, current_focused_idx, current_metadata, registry)
    }

    fn undo_at(
        &mut self,
        doc: &mut AutoCommit,
        index: usize,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        if !self.can_bump() {
            return None;
        }
        let entry = self.undo_stack.remove(index);
        let (before, after) = self.apply_inverse(doc, &entry, None, registry);
        // Opposite stack: applying `redo_via` re-does the forward action;
        // applying `undo_via` again undoes that redo.
        push_history(
            &mut self.redo_stack,
            UndoEntry {
                before,
                after,
                focused_node_idx: focus,
                widget_metadata: metadata,
                author: entry.author.clone(),
                instance_id: entry.instance_id,
                undo_via: entry.redo_via.clone(),
                redo_via: entry.undo_via.clone(),
            },
        );
        self.bump();
        Some(entry)
    }

    /// Whether `author` may revert `entry`: their own work, and anything
    /// **unattributed**.
    ///
    /// The second half is what lets an author-scoped undo reach entries the
    /// toolkit records for an app's own affordances —
    /// [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action)
    /// (toolbar buttons, context-menu items, palette dispatch) has no author
    /// to give, so those entries land with `author: None`.
    ///
    /// Sound rather than merely expedient: an entry only reaches these stacks
    /// via a *local* commit boundary — a peer's ops arrive through
    /// `State::load_incremental`, which re-baselines without pushing anything.
    /// An entry naming a different person still belongs to them and is still
    /// walked past.
    fn claimable_by(entry: &UndoEntry, author: &UserId) -> bool {
        entry.author.as_ref().is_none_or(|a| a == author)
    }

    pub fn undo_for_author(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_for_author_with_registry(doc, author, current_focused_idx, current_metadata, None)
    }

    pub fn undo_for_author_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        let index = self.undo_index_for(Some(author))?;
        self.undo_at(doc, index, current_focused_idx, current_metadata, registry)
    }

    pub fn undo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_as_with_registry(doc, author, current_focused_idx, current_metadata, None)
    }

    pub fn undo_as_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        match author {
            Some(a) => self.undo_for_author_with_registry(
                doc,
                a,
                current_focused_idx,
                current_metadata,
                registry,
            ),
            None => self.undo_with_registry(doc, current_focused_idx, current_metadata, registry),
        }
    }

    pub fn redo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_as_with_registry(doc, None, current_focused_idx, current_metadata, None)
    }

    pub fn redo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_as_with_registry(doc, author, current_focused_idx, current_metadata, None)
    }

    pub fn redo_as_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        let index = self.redo_index_for(author)?;
        self.redo_at(doc, index, current_focused_idx, current_metadata, registry)
    }

    fn redo_at(
        &mut self,
        doc: &mut AutoCommit,
        index: usize,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        if !self.can_bump() {
            return None;
        }
        let entry = self.redo_stack.remove(index);
        let (before, after) = self.apply_inverse(doc, &entry, None, registry);
        push_history(
            &mut self.undo_stack,
            UndoEntry {
                before,
                after,
                focused_node_idx: focus,
                widget_metadata: metadata,
                author: entry.author.clone(),
                instance_id: entry.instance_id,
                undo_via: entry.redo_via.clone(),
                redo_via: entry.undo_via.clone(),
            },
        );
        self.bump();
        Some(entry)
    }

    fn instance_index(stack: &[UndoEntry], author: &UserId, instance: InstanceId) -> Option<usize> {
        stack.iter().rposition(|entry| {
            entry.instance_id == Some(instance) && entry.author.as_ref() == Some(author)
        })
    }

    pub fn next_undo_in(&self, author: &UserId, instance: InstanceId) -> Option<&UndoEntry> {
        Self::instance_index(&self.undo_stack, author, instance)
            .and_then(|index| self.undo_stack.get(index))
    }

    pub fn next_redo_in(&self, author: &UserId, instance: InstanceId) -> Option<&UndoEntry> {
        Self::instance_index(&self.redo_stack, author, instance)
            .and_then(|index| self.redo_stack.get(index))
    }

    pub fn undo_in(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_in_with_registry(doc, author, instance, focus, metadata, None)
    }

    pub fn undo_in_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        let index = Self::instance_index(&self.undo_stack, author, instance)?;
        self.undo_at(doc, index, focus, metadata, registry)
    }

    pub fn redo_in(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_in_with_registry(doc, author, instance, focus, metadata, None)
    }

    pub fn redo_in_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        let index = Self::instance_index(&self.redo_stack, author, instance)?;
        self.redo_at(doc, index, focus, metadata, registry)
    }

    /// See [`State::restore_to_heads`].
    pub fn restore_to_heads(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
            None,
            None,
        )
    }

    /// See [`State::restore_to_heads_with_context`].
    pub fn restore_to_heads_with_message(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        message: Option<String>,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
            None,
            message,
        )
    }

    pub fn restore_to_heads_in(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: UserId,
        instance: InstanceId,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            focused_node_idx,
            metadata,
            Some(author),
            Some(instance),
            None,
        )
    }

    fn restore_to_heads_owned(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        instance_id: Option<InstanceId>,
        message: Option<String>,
    ) -> bool {
        let current = doc.get_heads();
        if current == target_heads {
            return false;
        }
        if !self.can_bump() {
            return false;
        }
        // `apply_inverse` writes the delta that leads from `after` back to
        // `before`; naming the live state as `after` and the historical state
        // as `before` is what turns "undo one action" into "travel to this
        // version".
        let entry = UndoEntry {
            before: target_heads.to_vec(),
            after: current,
            focused_node_idx,
            widget_metadata: metadata.clone(),
            author: author.clone(),
            instance_id,
            undo_via: None,
            redo_via: None,
        };
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry, message, None);
        if inv_before == inv_after {
            return false;
        }
        push_history(
            &mut self.undo_stack,
            UndoEntry {
                before: inv_before,
                after: inv_after,
                focused_node_idx,
                widget_metadata: metadata,
                author,
                instance_id,
                undo_via: None,
                redo_via: None,
            },
        );
        if let Some(instance) = instance_id {
            self.redo_stack
                .retain(|entry| entry.instance_id != Some(instance));
        } else {
            self.redo_stack.clear();
        }
        self.bump();
        true
    }
}

/// The unified state object that holds our CRDT document.
pub struct State {
    /// Compatibility cursor surface; modern collaboration uses `PresenceCtx`.
    pub collab: CollaborationState,
    /// Process-local document lifetime, distinct from a persistent record name
    /// or committed frontier. Captured readers must not survive replacement.
    document_identity: uuid::Uuid,
    pub doc: AutoCommit,
    pub root: ObjId,
    /// The primary collaboration route's explicit outbound frontier. Kept
    /// beside the document so every merge/adoption rebases it atomically.
    outbound: DocumentFrontier,
    /// This document's history. Swapped out with the document itself in
    /// [`Self::adopt_doc`] — see there for why it cannot travel.
    undo: UndoLog,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Self {
            document_identity: uuid::Uuid::new_v4(),
            doc,
            root,
            outbound,
            collab: CollaborationState::new(),
            undo,
        }
    }

    /// Replace the live document with another one — a different record's.
    ///
    /// The undo history goes with it: an [`UndoEntry`] is a pair of change
    /// hashes in *this* document's graph, and carried across a swap it would
    /// name changes the new document has never seen. Each record's history is
    /// its own, so the stacks reset rather than travel.
    ///
    /// `collab` (remote cursors and presence) is kept: presence is about who
    /// else is here, which does not change because the document on screen did.
    /// Returns `false` when the history revision namespace is exhausted. In
    /// that case the live document is deliberately left in place: adopting a
    /// new one under an indistinguishable revision could reuse cached history
    /// UI from the old document.
    pub fn adopt_doc(&mut self, doc: AutoCommit) -> bool {
        let Some(next) = self.undo.revision().checked_add(1) else {
            return false;
        };
        self.doc = doc;
        self.document_identity = uuid::Uuid::new_v4();
        self.root = automerge::ROOT;
        self.undo = UndoLog::new(&mut self.doc);
        self.outbound = DocumentFrontier::at_document(&mut self.doc);
        self.undo.revision = next;
        true
    }

    /// The lifetime of the currently adopted document. Commit, undo, merge and
    /// save keep it stable; successful `adopt_doc` changes it even for identical
    /// bytes. Replace documents through `adopt_doc`, not direct field assignment.
    pub fn document_identity(&self) -> uuid::Uuid {
        self.document_identity
    }

    /// This document's history, for a caller that drives undo itself.
    pub fn history_mut(&mut self) -> &mut UndoLog {
        &mut self.undo
    }

    /// A counter that changes whenever this document's history does. See
    /// [`UndoLog::revision`].
    pub fn history_revision(&self) -> u64 {
        self.undo.revision()
    }

    /// Reconstruct the document's persistent causal history, including
    /// branches and merges. No app-defined history schema is required.
    pub fn revision_history(&mut self) -> RevisionHistory {
        RevisionHistory::from_document(&mut self.doc)
    }

    /// Restore the content at one persistent revision as a new forward,
    /// undoable change. The old branch remains in [`Self::revision_history`].
    ///
    /// # Errors
    ///
    /// Returns [`UnknownRevision`] when `revision` belongs to another
    /// document or is otherwise absent from this document's change graph.
    pub fn restore_revision(
        &mut self,
        revision: RevisionId,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> Result<bool, UnknownRevision> {
        let hash = revision.as_hash();
        if self.doc.get_change_by_hash(&hash).is_none() {
            return Err(UnknownRevision(revision));
        }
        Ok(self.restore_to_heads(&[hash], focused_node_idx, metadata, author))
    }

    /// Commit the current transaction and push a change-based undo entry
    /// recording the heads that bracket this action.
    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    /// Commit the current transaction and push a change-based undo entry,
    /// attributing it to `author` so multi-user undo can isolate users. Returns
    /// `true` iff a change was committed (see the [`UndoLog`] method).
    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.undo
            .commit_action_authored(&mut self.doc, focused_node_idx, metadata, author)
    }

    pub fn commit_action_authored_with_context(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        context: Option<tilo_actions::ActionContext>,
    ) -> bool {
        self.undo.commit_action_authored_with_message(
            &mut self.doc,
            focused_node_idx,
            metadata,
            author,
            context.and_then(|context| context.change_message()),
        )
    }

    /// Inspect the most recently pushed undo entry without popping it.
    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo.peek_last_undo()
    }

    /// How many entries this document's undo stack holds. See
    /// [`UndoLog::undo_depth`].
    pub fn undo_depth(&self) -> usize {
        self.undo.undo_depth()
    }

    /// How many entries this document's redo stack holds. See
    /// [`UndoLog::redo_depth`].
    pub fn redo_depth(&self) -> usize {
        self.undo.redo_depth()
    }

    /// Whether `author` has anything to undo / redo. See
    /// [`UndoLog::can_undo_as`].
    pub fn can_undo_as(&self, author: Option<&UserId>) -> bool {
        self.undo.can_undo_as(author)
    }

    pub fn can_redo_as(&self, author: Option<&UserId>) -> bool {
        self.undo.can_redo_as(author)
    }

    /// What the history can do for `author` right now, ready to hand to a
    /// widget — see [`UndoStatus`](crate::undo::UndoStatus).
    ///
    /// The summaries are derived from the bracketed changes themselves through
    /// [`history_source::change_summary`](crate::history_source::change_summary),
    /// the same producer the history pane uses, so a control and the timeline
    /// describe an action the same way. Both read "the action, as performed":
    /// an undo entry brackets `before → after`, while a redo entry holds the
    /// *inverse* change, so its summary reads `after → before`.
    pub fn undo_status(&mut self, author: Option<&UserId>) -> crate::undo::UndoStatus {
        let undo_span = self
            .undo
            .next_undo_as(author)
            .map(|e| (e.before.clone(), e.after.clone()));
        let redo_span = self
            .undo
            .next_redo_as(author)
            .map(|e| (e.after.clone(), e.before.clone()));
        crate::undo::UndoStatus {
            can_undo: undo_span.is_some(),
            can_redo: redo_span.is_some(),
            undo_summary: undo_span
                .map(|(from, to)| crate::history_source::change_summary(&mut self.doc, &from, &to)),
            redo_summary: redo_span
                .map(|(from, to)| crate::history_source::change_summary(&mut self.doc, &from, &to)),
        }
    }

    /// Record an action that has *already* been committed into `doc`
    /// out-of-band (e.g. a `tilo_actions` action the Ctrl+K palette dispatched,
    /// which owns its own actor + timestamp commit boundary). Brackets the
    /// undo entry with the caller-supplied `before` heads, clears the redo
    /// stack, and realigns the undo baseline.
    pub fn record_committed_action(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) -> bool {
        self.undo
            .record_committed_action(&mut self.doc, before, focused_node_idx, metadata)
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a
    /// change that reached this document from outside the window. See
    /// [`UndoLog::record_committed_action_authored`].
    pub fn record_committed_action_authored(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.record_committed_action_authored_with_revert(
            before,
            focused_node_idx,
            metadata,
            author,
            None,
            None,
        )
    }

    /// As [`Self::record_committed_action_authored`], carrying optional
    /// action-based undo/redo for outside-consequence document actions.
    pub fn record_committed_action_authored_with_revert(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        undo_via: Option<tilo_actions::EncodedAction>,
        redo_via: Option<tilo_actions::EncodedAction>,
    ) -> bool {
        let after = self.doc.get_heads();
        self.undo.record_committed_span_with_inverse(
            before,
            after,
            focused_node_idx,
            metadata,
            author,
            None,
            CommittedSpanInverse {
                undo_via,
                redo_via,
            },
        )
    }

    /// Rewrite the document to the content it had at `target_heads`, as a NEW
    /// forward change — "restore to this version" from the history scrubber.
    /// Takes the same route as undo: derive the patches from current to
    /// target, then write them, so history is appended to, never rewritten.
    ///
    /// Consequences: the restore shows up in the timeline as its own entry
    /// (the versions it rolled past are still there); remote edits merged in
    /// while scrubbing survive, since only what actually differs is
    /// rewritten; and the restore is itself undoable.
    ///
    /// Returns `true` when something changed; a restore to the current state
    /// is a no-op.
    pub fn restore_to_heads(
        &mut self,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.undo.restore_to_heads(
            &mut self.doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
        )
    }

    /// [`Self::restore_to_heads`], stamping the restore change with `context`
    /// so History names it rather than describing its raw patch set.
    pub fn restore_to_heads_with_context(
        &mut self,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        context: &tilo_actions::ActionContext,
    ) -> bool {
        self.undo.restore_to_heads_with_message(
            &mut self.doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
            context.change_message(),
        )
    }

    /// Undo the most recent action by writing its inverse as a new forward
    /// change. Callers pass the *current* focus index and widget metadata so
    /// the redo entry carries the caret/focus state at the moment undo fired
    /// — otherwise Ctrl+Shift+Z would rehydrate the text body but leave the
    /// caret at offset 0.
    pub fn undo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo(&mut self.doc, current_focused_idx, current_metadata)
    }

    /// Multi-user aware undo. Pops the topmost entry whose `author` equals
    /// `author`, writing its inverse as a new forward change. Entries from
    /// other users are preserved in the stack, so one collaborator's undo
    /// never reverts — or clobbers — another's committed work.
    ///
    /// `current_focused_idx` / `current_metadata` are captured on the redo
    /// stack so a subsequent redo can restore the caret — same rationale as
    /// [`undo`](crate::undo).
    pub fn undo_for_author(
        &mut self,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo_for_author(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Undo on behalf of `author`.
    ///
    /// This is the entry point every interactive Ctrl+Z takes, and the reason
    /// it exists rather than callers picking between [`Self::undo`] and
    /// [`Self::undo_for_author`] themselves: with an author, undo must skip
    /// past other people's actions, and a call site that forgets is a session
    /// in which one collaborator silently reverts another's work. `None` — an
    /// app running with no identity at all — keeps the plain LIFO behaviour.
    pub fn undo_as(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_as_with_registry(author, current_focused_idx, current_metadata, None)
    }

    /// [`Self::undo_as`] with a registry so outside-consequence undo can
    /// apply `undo_via` through [`tilo_actions::Action::apply`].
    pub fn undo_as_with_registry(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo.undo_as_with_registry(
            &mut self.doc,
            author,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    /// Redo on behalf of `author` — the mirror of [`Self::undo_as`], so a
    /// redo cannot re-apply an action the local user never undid.
    pub fn redo_as(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_as_with_registry(author, current_focused_idx, current_metadata, None)
    }

    /// [`Self::redo_as`] with a registry for action-based redo.
    pub fn redo_as_with_registry(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo.redo_as_with_registry(
            &mut self.doc,
            author,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    /// Redo the most recently undone action by inverting the inverse change
    /// (re-applying the original action's delta as a new forward change).
    /// `current_focused_idx` / `current_metadata` are captured on the undo
    /// stack so a subsequent undo can park the caret back where it was
    /// pre-redo.
    pub fn redo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .redo(&mut self.doc, current_focused_idx, current_metadata)
    }

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Wrap an already-loaded document.
    ///
    /// The entry point for persistence (`tilo_ui::persist`): the store hands back
    /// the previous session's `AutoCommit` — history, actor identity and all
    /// — and this adopts it as the live document without a byte round-trip.
    pub fn from_doc(mut doc: AutoCommit) -> Self {
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Self {
            document_identity: uuid::Uuid::new_v4(),
            doc,
            root: automerge::ROOT,
            outbound,
            collab: CollaborationState::new(),
            undo,
        }
    }

    /// Restores an application state from a serialized byte vector.
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid Automerge document (corrupt
    /// bytes, or bytes produced by an incompatible format).
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Ok(Self {
            document_identity: uuid::Uuid::new_v4(),
            doc,
            root,
            outbound,
            collab: CollaborationState::new(),
            undo,
        })
    }

    /// Apply an incremental CRDT patch (bytes produced by
    /// `AutoCommit::save_incremental` on another peer) into this document.
    /// Returns the number of ops applied on success.
    ///
    /// This is how an incremental `CollabEvent::Document` is merged on the
    /// receiver side — without it, incremental deltas broadcast by one peer
    /// would never reach another peer's CRDT.
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid incremental Automerge patch
    /// (corrupt bytes, or bytes that don't apply to this document's history).
    pub fn load_incremental(&mut self, data: &[u8]) -> Result<usize, automerge::AutomergeError> {
        let before = self.doc.get_heads();
        let n = self.doc.load_incoming(data)?;
        // Keep the undo baseline aligned with the new, merged document state
        // so the *next* local action brackets against a complete picture.
        // Existing entries keep their original before/after heads — those
        // remain valid addresses into the append-only change graph after a
        // merge, so undoing an earlier action still reverts exactly that
        // action, leaving the just-merged remote ops intact.
        self.undo.rebase(&mut self.doc);
        self.outbound.observe_changes_after(&mut self.doc, &before);
        Ok(n)
    }

    /// Merge a complete peer snapshot without replacing local work, then
    /// atomically align undo and outbound frontiers with the merged graph.
    ///
    /// # Errors
    ///
    /// Returns an error when `data` is not a valid Automerge snapshot or cannot
    /// be merged into this document.
    pub fn merge_snapshot(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        let mut incoming = AutoCommit::load(data)?;
        let observed = incoming.get_heads();
        self.doc.merge_incoming(&mut incoming)?;
        self.undo.rebase(&mut self.doc);
        self.outbound.observe(observed);
        Ok(())
    }

    /// Prepare primary-route bytes without advancing its frontier. The caller
    /// advances only after its durable queue accepts the payload.
    pub fn pending_outbound_delta(&mut self) -> (Vec<u8>, Vec<ChangeHash>) {
        self.outbound.pending_delta(&mut self.doc)
    }

    pub fn advance_outbound(&mut self, heads: Vec<ChangeHash>) {
        self.outbound.advance(heads);
    }

    /// Reset the primary route after an address/record change.
    pub fn rebase_outbound(&mut self) {
        self.outbound.rebase(&mut self.doc);
    }

    /// Serialize the changes made since the last `save/save_incremental` call.
    /// The returned bytes can be fed into another peer's `load_incremental`
    /// to replay just the delta — the basic building block of efficient
    /// CRDT sync.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }
}

/// Write the delta that leads from `from_heads` to `to_heads` into `doc`'s open
/// transaction, without committing.
///
/// The one primitive behind both undo and restore-from-history: it re-derives
/// exactly the properties and elements that differ and writes them forward,
/// rather than replacing the document with an old state and dropping every
/// concurrently-merged remote op.
/// A container the delta would remove that somebody has written inside since
/// `from_heads` — the paragraph a collaborator typed into after the action
/// that created it — is left standing, so undo takes back only the action.
///
/// The caller commits. That is deliberate: a `ui` app signals `needs_commit()`
/// and lets `App::commit_action` do the commit, so a restore performed from
/// inside a widget lands on the undo stack with the right author and timestamp
/// exactly like any other edit. [`State::restore_to_heads`] is the equivalent
/// for a host that owns a whole [`State`].
pub fn write_revert_patches(
    doc: &mut AutoCommit,
    from_heads: &[ChangeHash],
    to_heads: &[ChangeHash],
) {
    // Patches describing how to get from `from_heads` back to `to_heads`.
    let patches = doc.diff(from_heads, to_heads);
    // A container somebody wrote inside since `from_heads` stays standing.
    let patches = built_on::hold_back_built_on(doc, patches, from_heads);
    write_selected_patches(doc, patches, to_heads, from_heads);
}

/// Write an already-derived patch set forward into `doc`'s open transaction.
///
/// The half of [`write_revert_patches`] after the diff, split out so a caller
/// that already derived a full patch stream can reuse the index translation.
/// Filtered history uses `write_selected_patches` to retain skipped structural
/// steps in the logical index stream without changing their live content.
///
/// The two anchors bracket the change being inverted. `index_anchor` is the
/// state the patches start from: the first patch into a sequence indexes it as
/// of then, and each later one as the patches before it left it.
/// `insert_anchor` is the state they lead to, whose formatting a recreated
/// text object takes back. Passing them the wrong way round moves edits to
/// the wrong elements, so both are named rather than positional.
pub fn write_patches(
    doc: &mut AutoCommit,
    patches: Vec<automerge::Patch>,
    insert_anchor: &[ChangeHash],
    index_anchor: &[ChangeHash],
) {
    write_selected_patches(
        doc,
        patches.into_iter().map(|patch| (patch, true)).collect(),
        insert_anchor,
        index_anchor,
    );
}

/// Filtered history must retain skipped structural patches: later diff indices
/// still include their logical effects, even though their live content survives.
#[allow(
    clippy::too_many_lines,
    reason = "the patch replay must share object remapping and sequence state across every Automerge patch variant"
)]
pub(crate) fn write_selected_patches(
    doc: &mut AutoCommit,
    patches: Vec<(automerge::Patch, bool)>,
    insert_anchor: &[ChangeHash],
    index_anchor: &[ChangeHash],
) {
    // Recreated objects get a fresh ExId; map the patch's (old) ObjId to it
    // so nested child patches target the new object.
    let mut remap: HashMap<ObjId, ObjId> = HashMap::new();
    // Diff indices refer to the sequence as earlier patches transform it,
    // which the live document — holding later collaborator edits too — is
    // not; each sequence object follows both.
    let mut sequences = sequence_replay::prepare(doc, &patches, index_anchor);
    for (patch, apply) in patches {
        let target = remap
            .get(&patch.obj)
            .cloned()
            .unwrap_or_else(|| patch.obj.clone());
        let is_text = matches!(doc.object_type(&target), Ok(automerge::ObjType::Text));
        let sequence =
            if is_text || matches!(doc.object_type(&target), Ok(automerge::ObjType::List)) {
                Some(
                    sequences
                        .entry(patch.obj.clone())
                        .or_insert_with(SequenceReplay::fresh),
                )
            } else {
                None
            };
        if !apply {
            // A skipped list patch still moves the indices of the ones after
            // it. (Filtered history keeps or skips a text object whole.)
            if let Some(sequence) = sequence {
                match &patch.action {
                    PatchAction::DeleteSeq { index, length } => {
                        sequence.skipped_delete(*index, *length);
                    }
                    PatchAction::Insert { index, values } => {
                        sequence.skipped_insert(*index, values.len());
                    }
                    _ => {}
                }
            }
            continue;
        }
        match patch.action {
            PatchAction::PutMap { key, value, .. } => match value.0 {
                Value::Scalar(s) => {
                    let _ = doc.put(&target, key, s.into_owned());
                }
                Value::Object(ot) => {
                    if let Ok(new_id) = doc.put_object(&target, key, ot) {
                        remap.insert(value.1, new_id);
                    }
                }
            },
            PatchAction::PutSeq { index, value, .. } => {
                // The element still exists after the action; leave it be when
                // a collaborator removed it since.
                let Some(sequence) = sequence else { continue };
                let Some(at) = sequence.element(index) else {
                    continue;
                };
                match value.0 {
                    Value::Scalar(s) => {
                        let _ = doc.put(&target, at.unit, s.into_owned());
                    }
                    Value::Object(ot) => {
                        if let Ok(new_id) = doc.put_object(&target, at.unit, ot) {
                            remap.insert(value.1, new_id);
                        }
                    }
                }
                sequence.replaced(doc, &target, index, at);
            }
            PatchAction::Insert { index, values } => {
                // Re-inserting elements the local action deleted.
                let Some(sequence) = sequence else { continue };
                let at = sequence.insertion(index);
                let mut written = 0usize;
                for (li, (val, old_id, _)) in (at.unit..).zip(&values) {
                    let inserted = match val {
                        Value::Scalar(s) => doc.insert(&target, li, s.clone().into_owned()).is_ok(),
                        Value::Object(ot) => doc
                            .insert_object(&target, li, *ot)
                            .map(|new_id| remap.insert(old_id.clone(), new_id))
                            .is_ok(),
                    };
                    if !inserted {
                        break;
                    }
                    written = written.saturating_add(1);
                }
                sequence.inserted(doc, &target, index, at, written);
            }
            PatchAction::SpliceText {
                index,
                value,
                marks,
            } => {
                // Re-inserting text the local action deleted, including native
                // formatting when a move/undo recreates its owning object.
                let Some(sequence) = sequence else { continue };
                let at = sequence.insertion(index);
                let li = at.unit;
                let previous_length = doc.length(&target);
                if doc
                    .splice_text(&target, li, 0, &value.make_string())
                    .is_ok()
                {
                    let written = doc.length(&target).saturating_sub(previous_length);
                    sequence.inserted(doc, &target, index, at, written);
                    let end = li.saturating_add(written);
                    // Insertion can inherit formatting from its new neighbors.
                    // The patch markset describes the complete restored span,
                    // so names absent from it must be removed, including when
                    // marks is None (an intentionally unformatted insertion).
                    let mut inherited = doc
                        .marks(&target)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|mark| mark.start < end && mark.end > li)
                        .filter(|mark| {
                            !marks.as_ref().is_some_and(|wanted| {
                                wanted.iter().any(|(name, _)| name == mark.name.as_str())
                            })
                        })
                        .map(|mark| mark.name.to_string())
                        .collect::<Vec<_>>();
                    inherited.sort();
                    inherited.dedup();
                    for name in inherited {
                        let _ = doc.unmark(
                            &target,
                            &name,
                            li,
                            end,
                            automerge::marks::ExpandMark::After,
                        );
                    }
                    if let Some(marks) = marks {
                        for (name, value) in marks.iter() {
                            let _ = doc.mark(
                                &target,
                                automerge::marks::Mark::new(
                                    name.to_string(),
                                    value.clone(),
                                    li,
                                    end,
                                ),
                                automerge::marks::ExpandMark::After,
                            );
                        }
                    }
                }
            }
            PatchAction::Increment { prop, value } => {
                let prop = match (prop, sequence) {
                    (automerge::Prop::Seq(index), Some(sequence)) => {
                        let Some(at) = sequence.element(index) else {
                            continue;
                        };
                        automerge::Prop::Seq(at.unit)
                    }
                    (prop, _) => prop,
                };
                let _ = doc.increment(&target, prop, value);
            }
            PatchAction::DeleteMap { key } => {
                let _ = doc.delete(&target, key);
            }
            PatchAction::DeleteSeq { index, length } => {
                // Removing elements the local action inserted, except those
                // a collaborator removed since.
                let Some(sequence) = sequence else { continue };
                for (at, units) in sequence.delete(index, length) {
                    let Ok(units) = isize::try_from(units) else {
                        continue;
                    };
                    let _ = if is_text {
                        doc.splice_text(&target, at.unit, units, "")
                    } else {
                        doc.splice(
                            &target,
                            at.unit,
                            units,
                            std::iter::empty::<automerge::ScalarValue>(),
                        )
                    };
                }
            }
            PatchAction::Mark { marks } => {
                let Some(sequence) = sequence else { continue };
                for mut mark in marks {
                    let Some((start, end)) = sequence.span(mark.start, mark.end) else {
                        continue;
                    };
                    mark.start = start;
                    mark.end = end;
                    let _ = doc.mark(&target, mark, automerge::marks::ExpandMark::After);
                }
            }
            // Conflict markers carry no content to revert.
            PatchAction::Conflict { .. } => {}
        }
    }
    // Automerge 0.8's ExposeQueue::flush_obj emits re-exposed Text contents
    // with marks=None (its TODO for text_spans). Recreated text therefore needs
    // its native marks restored from the same historical frontier as its body.
    // Only fresh objects are touched; surviving peer text is never overwritten.
    for (old, fresh) in remap {
        if !matches!(doc.object_type(&fresh), Ok(automerge::ObjType::Text)) {
            continue;
        }
        let Ok(restored) = doc.marks_at(&old, insert_anchor) else {
            continue;
        };
        for mark in doc.marks(&fresh).unwrap_or_default() {
            let _ = doc.unmark(
                &fresh,
                mark.name.as_str(),
                mark.start,
                mark.end,
                automerge::marks::ExpandMark::After,
            );
        }
        for mark in restored {
            let _ = doc.mark(&fresh, mark, automerge::marks::ExpandMark::After);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ReadDoc, ScalarValue, transaction::Transactable};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn undo_restores_text_marks_and_list_order_exactly() {
        // phase: undo_restores_multi_span_and_multi_character_text_edits_exactly
        {
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state.commit_action(None, None);
            for next in [
                "first thought",
                "second thought",
                "a second, longer thought",
                "",
            ] {
                state.doc.update_text(&text, next).unwrap();
                state.commit_action(None, None);
            }
            for expected in [
                "a second, longer thought",
                "second thought",
                "first thought",
                "",
            ] {
                state.undo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), expected);
            }
            for expected in [
                "first thought",
                "second thought",
                "a second, longer thought",
                "",
            ] {
                state.redo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), expected);
            }
        }
        // phase: undo_unformatted_text_between_formatted_neighbors_does_not_inherit_marks
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "aXb").unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 0, 1),
                    ExpandMark::After,
                )
                .unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 2, 3),
                    ExpandMark::After,
                )
                .unwrap();
            state.commit_action(None, None);
            let before = state.doc.marks(&text).unwrap();
            assert_eq!(
                before
                    .iter()
                    .map(|mark| (mark.start, mark.end))
                    .collect::<Vec<_>>(),
                [(0, 1), (2, 3)]
            );
            state.doc.splice_text(&text, 1, 1, "").unwrap();
            state.commit_action(None, None);
            let after = state.doc.marks(&text).unwrap();
            assert_eq!(state.doc.text(&text).unwrap(), "ab");
            for _ in 0..2 {
                state.undo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "aXb");
                assert_eq!(
                    state.doc.marks(&text).unwrap(),
                    before,
                    "restored X must remain unformatted"
                );
                state.redo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "ab");
                assert_eq!(state.doc.marks(&text).unwrap(), after);
            }
        }
        // phase: grouped_list_reorder_undo_redo_preserves_objects_marks_and_order
        {
            fn insert(doc: &mut AutoCommit, list: &ObjId, index: usize, label: &str) {
                let block = doc.insert_object(list, index, ObjType::Map).unwrap();
                doc.put(&block, "id", label).unwrap();
                let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
                doc.splice_text(&text, 0, 0, label).unwrap();
                doc.mark(
                    &text,
                    automerge::marks::Mark::new("bold".into(), true, 0, label.len()),
                    automerge::marks::ExpandMark::After,
                )
                .unwrap();
            }
            fn rows(
                doc: &AutoCommit,
                list: &ObjId,
            ) -> Vec<(String, String, Vec<automerge::marks::Mark>)> {
                (0..doc.length(list))
                    .map(|index| {
                        let block = doc.get(list, index).unwrap().unwrap().1;
                        let id = doc
                            .get(&block, "id")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .unwrap()
                            .to_string();
                        let text = doc.get(&block, "text").unwrap().unwrap().1;
                        (id, doc.text(&text).unwrap(), doc.marks(&text).unwrap())
                    })
                    .collect()
            }
            let mut state = State::new();
            let list = state.doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
            for (index, label) in [
                "parent-a", "child-a", "task-a", "parent-b", "child-b", "parent-c",
            ]
            .iter()
            .enumerate()
            {
                insert(&mut state.doc, &list, index, label);
            }
            state.commit_action(None, None);
            let before = rows(&state.doc, &list);
            let base = state.doc.get_heads();
            let mut peer = state.doc.fork_at(&base).unwrap();
            insert(&mut peer, &list, 2, "peer-child");
            peer.commit();
            let peer_row = rows(&peer, &list)[2].clone();
            // The same delete/reinsert operations emitted by a Notes subtree move.
            state.doc.delete(&list, 3).unwrap();
            insert(&mut state.doc, &list, 0, "parent-b");
            state.doc.delete(&list, 4).unwrap();
            insert(&mut state.doc, &list, 1, "child-b");
            state.commit_action(None, None);
            let after = rows(&state.doc, &list);
            assert_eq!(
                after.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
                [
                    "parent-b", "child-b", "parent-a", "child-a", "task-a", "parent-c"
                ]
            );
            for cycle in 0..3 {
                state.undo(None, None).unwrap();
                assert_eq!(rows(&state.doc, &list), before, "undo cycle {cycle}");
                state.redo(None, None).unwrap();
                assert_eq!(rows(&state.doc, &list), after, "redo cycle {cycle}");
            }
            state.load_incremental(&peer.save_after(&base)).unwrap();
            let mut before_with_peer = before;
            before_with_peer.insert(2, peer_row.clone());
            let mut after_with_peer = after;
            after_with_peer.insert(4, peer_row);
            assert_eq!(rows(&state.doc, &list), after_with_peer);
            state.undo(None, None).unwrap();
            assert_eq!(rows(&state.doc, &list), before_with_peer);
            state.redo(None, None).unwrap();
            assert_eq!(rows(&state.doc, &list), after_with_peer);
        }
    }

    #[test]
    fn document_identity_is_stable_for_edits_and_unique_per_document() {
        // phase: document_identity_is_stable_for_edits_and_fresh_for_adoption
        {
            let mut state = State::new();
            let identity = state.document_identity();
            state.doc.put(automerge::ROOT, "body", "first").unwrap();
            state.commit_action(None, None);
            assert_eq!(state.document_identity(), identity);
            assert!(state.undo(None, None).is_some());
            assert_eq!(state.document_identity(), identity);
            assert!(state.redo(None, None).is_some());
            assert_eq!(state.document_identity(), identity);
            let bytes = state.save();
            assert_eq!(state.document_identity(), identity);

            // Reopening the same exact record bytes still creates a new lifetime.
            assert!(state.adopt_doc(AutoCommit::load(&bytes).unwrap()));
            assert_ne!(state.document_identity(), identity);
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "body")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("first")
            );
        }
        // phase: document_identity_is_unique_for_each_constructor_and_stable_on_failed_adoption
        {
            let mut original = State::new();
            let bytes = original.save();
            let loaded = State::load(&bytes).unwrap();
            let wrapped = State::from_doc(AutoCommit::load(&bytes).unwrap());
            let identities = [
                original.document_identity(),
                loaded.document_identity(),
                wrapped.document_identity(),
            ];
            assert_eq!(
                identities
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                3
            );
            let identity = original.document_identity();
            original.undo.revision = u64::MAX;
            assert!(!original.adopt_doc(AutoCommit::new()));
            assert_eq!(original.document_identity(), identity);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn frontiers_and_rejected_deltas_survive_merges_and_retries() {
        // phase: explicit_frontier_is_independent_of_automerge_save_cursor
        {
            let mut doc = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            doc.put(automerge::ROOT, "body", "draft").unwrap();
            let _snapshot = doc.save();

            let delta = frontier.take_delta(&mut doc);
            assert!(
                !delta.is_empty(),
                "a full snapshot must not consume this route's delta"
            );
            assert!(frontier.take_delta(&mut doc).is_empty());
        }
        // phase: rebased_frontier_does_not_echo_inbound_changes
        {
            let mut source = AutoCommit::new();
            source.put(automerge::ROOT, "remote", true).unwrap();
            source.commit();
            let bytes = source.save();

            let mut target = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut target);
            let mut incoming = AutoCommit::load(&bytes).unwrap();
            target.merge(&mut incoming).unwrap();
            frontier.rebase(&mut target);
            assert!(frontier.take_delta(&mut target).is_empty());

            target.put(automerge::ROOT, "local", true).unwrap();
            assert!(!frontier.take_delta(&mut target).is_empty());
        }
        // phase: pending_delta_is_retryable_until_transport_accepts_it
        {
            let mut doc = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            doc.put(automerge::ROOT, "body", "retry me").unwrap();

            let (first, heads) = frontier.pending_delta(&mut doc);
            let (retry, _) = frontier.pending_delta(&mut doc);
            assert_eq!(retry, first);

            frontier.advance(heads);
            assert!(frontier.pending_delta(&mut doc).0.is_empty());
        }
        // phase: rejected_local_delta_survives_concurrent_incremental_merge
        {
            let mut state = State::new();
            state.doc.put(ROOT, "local", true).unwrap();
            state.commit_action(None, None);
            let (rejected, _) = state.pending_outbound_delta();
            assert!(!rejected.is_empty());

            let mut peer = AutoCommit::new();
            peer.put(ROOT, "remote", true).unwrap();
            peer.commit();
            let remote_delta = peer.save_after(&[]);
            state.load_incremental(&remote_delta).unwrap();

            let (retry, accepted_heads) = state.pending_outbound_delta();
            assert!(
                !retry.is_empty(),
                "the rejected local edit must remain pending"
            );
            let mut retry_receiver = AutoCommit::new();
            retry_receiver.load_incremental(&retry).unwrap();
            assert_eq!(
                retry_receiver.get(ROOT, "local").unwrap().unwrap().0,
                true.into()
            );
            assert!(
                retry_receiver.get(ROOT, "remote").unwrap().is_none(),
                "the inbound change must not be echoed back"
            );

            state.advance_outbound(accepted_heads);
            assert!(state.pending_outbound_delta().0.is_empty());
        }
        // phase: rejected_local_delta_survives_concurrent_snapshot_merge
        {
            let mut state = State::new();
            state.doc.put(ROOT, "local", true).unwrap();
            state.commit_action(None, None);
            let (rejected, _) = state.pending_outbound_delta();
            assert!(!rejected.is_empty());

            let mut peer = AutoCommit::new();
            peer.put(ROOT, "remote", true).unwrap();
            peer.commit();
            state.merge_snapshot(&peer.save()).unwrap();

            let (retry, accepted_heads) = state.pending_outbound_delta();
            assert!(
                !retry.is_empty(),
                "the rejected local edit must remain pending"
            );
            let mut retry_receiver = AutoCommit::new();
            retry_receiver.load_incremental(&retry).unwrap();
            assert_eq!(
                retry_receiver.get(ROOT, "local").unwrap().unwrap().0,
                true.into()
            );
            assert!(
                retry_receiver.get(ROOT, "remote").unwrap().is_none(),
                "the snapshot's changes must not be echoed back"
            );

            state.advance_outbound(accepted_heads);
            assert!(state.pending_outbound_delta().0.is_empty());
        }
        // phase: metadata_replica_does_not_lose_rejected_delta_on_receive
        {
            let mut replica = DocumentReplica::new(AutoCommit::new());
            replica.document_mut().put(ROOT, "local", true).unwrap();
            let (rejected, _) = replica.pending_outbound_delta();
            assert!(!rejected.is_empty());

            let mut peer = AutoCommit::new();
            peer.put(ROOT, "remote", true).unwrap();
            peer.commit();
            replica.load_incremental(&peer.save_after(&[])).unwrap();

            let (retry, accepted_heads) = replica.pending_outbound_delta();
            let mut receiver = AutoCommit::new();
            receiver.load_incremental(&retry).unwrap();
            assert!(receiver.get(ROOT, "local").unwrap().is_some());
            assert!(receiver.get(ROOT, "remote").unwrap().is_none());
            replica.advance_outbound(accepted_heads);
            assert!(replica.pending_outbound_delta().0.is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn the_history_stack_is_a_bounded_lifo_carrying_focus_and_context() {
        // phase: history_stack_keeps_only_the_newest_bounded_window
        {
            let mut stack = Vec::new();
            let extra = 7usize;
            for index in 0..UNDO_HISTORY_LIMIT.saturating_add(extra) {
                push_history(
                    &mut stack,
                    UndoEntry {
                        before: Vec::new(),
                        after: Vec::new(),
                        focused_node_idx: Some(index),
                        widget_metadata: None,
                        author: None,
                        instance_id: None,
                        undo_via: None,
                        redo_via: None,
                    },
                );
            }
            assert_eq!(stack.len(), UNDO_HISTORY_LIMIT);
            assert_eq!(
                stack.first().and_then(|entry| entry.focused_node_idx),
                Some(extra),
                "the oldest entries should be evicted first"
            );
            assert_eq!(
                stack.last().and_then(|entry| entry.focused_node_idx),
                UNDO_HISTORY_LIMIT.saturating_add(extra).checked_sub(1),
                "the newest action must remain undoable"
            );
        }
        // phase: redo_entry_carries_current_focus_and_metadata
        // A single `commit_action` → undo → redo round-trip must preserve the
        // focus index and widget metadata captured on the stacks — that's
        // what restores the caret in `EditorApp` after Ctrl+Shift+Z.
        {
            let mut state = State::new();

            // Seed a mutation so commit_action has something to push.
            state
                .doc
                .put(&automerge::ROOT, "k", ScalarValue::Str("v".into()))
                .unwrap();
            state.commit_action(Some(7), Some("meta1".into()));

            // Simulate a second mutation whose current caret/focus should
            // be the one we expect redo to rehydrate.
            state
                .doc
                .put(&automerge::ROOT, "k", ScalarValue::Str("v2".into()))
                .unwrap();

            // Undo with *current* state (Some(3) / "meta_current") — this is
            // what callers like `App::commit_action` pass through.
            let popped = state
                .undo(Some(3), Some("meta_current".into()))
                .expect("undo must pop the seeded entry");
            assert_eq!(popped.focused_node_idx, Some(7));
            assert_eq!(popped.widget_metadata, Some("meta1".into()));

            // Redo — the test-only callers here don't need to bump the stack,
            // so pass None/None. The returned entry must reflect the state
            // that was current at the moment `undo(...)` fired.
            let redone = state
                .redo(None, None)
                .expect("redo must pop the redo-stack entry");
            assert_eq!(redone.focused_node_idx, Some(3));
            assert_eq!(redone.widget_metadata, Some("meta_current".into()));

            // Sanity: the doc itself is back at the post-second-mutation
            // state so renderers stay in sync.
            let (val, _) = state.doc.get(&automerge::ROOT, "k").unwrap().unwrap();
            assert_eq!(val.as_str(), Some("v2"));
        }
        // phase: native_commit_context_is_stamped_without_changing_undo
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "body", "hello").unwrap();
            let context = tilo_actions::ActionContext::for_resource(
                "Edit document",
                "document:1",
                "document",
                "Draft",
            )
            .with_app("org.tilo.Writer", "Writer");
            assert!(state.commit_action_authored_with_context(
                None,
                None,
                Some(UserId("ada".into())),
                Some(context.clone()),
            ));
            let changes = state.doc.get_changes(&[]);
            let change = changes.last().expect("native change");
            let decoded = change
                .message()
                .and_then(|message| tilo_actions::ActionContext::from_change_message(message));
            assert_eq!(decoded.as_ref(), Some(&context));
            assert!(state.peek_last_undo().is_some());
        }
        // phase: single_user_undo_redo_roundtrip
        // Single-user undo/redo round-trip on a map value returns to the exact
        // pre/post values.
        {
            let mut state = State::new();
            state
                .doc
                .put(&ROOT, "k", ScalarValue::Str("a".into()))
                .unwrap();
            state.commit_action(None, None);
            state
                .doc
                .put(&ROOT, "k", ScalarValue::Str("b".into()))
                .unwrap();
            state.commit_action(None, None);

            let val = |s: &State| {
                s.doc
                    .get(&ROOT, "k")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            assert_eq!(val(&state), "b");
            state.undo(None, None).unwrap();
            assert_eq!(val(&state), "a", "undo reverts to pre-action value");
            state.redo(None, None).unwrap();
            assert_eq!(val(&state), "b", "redo restores the action");
        }
        // phase: multi_action_undo_order_is_lifo
        // Multiple actions undo in LIFO order, each reverting only its own change.
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "b", 2).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "c", 3).unwrap();
            state.commit_action(None, None);

            let has = |s: &State, k: &str| s.doc.get(&ROOT, k).unwrap().is_some();
            assert!(has(&state, "a") && has(&state, "b") && has(&state, "c"));

            state.undo(None, None).unwrap(); // reverts "c"
            assert!(has(&state, "a") && has(&state, "b") && !has(&state, "c"));
            state.undo(None, None).unwrap(); // reverts "b"
            assert!(has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
            state.undo(None, None).unwrap(); // reverts "a"
            assert!(!has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
        }
    }

    use crate::permissions::UserId;
    use automerge::{ObjType, ROOT};

    fn text_body(state: &State, obj: &ObjId) -> String {
        state.doc.text(obj).unwrap_or_default()
    }

    #[test]
    fn undo_and_redo_keep_concurrent_remote_edits() {
        // phase: undo_preserves_concurrent_remote_edit
        // A committed local action, then a *concurrent remote change* merged in,
        // then `undo()`. The remote change MUST survive and only the local
        // action is reverted.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
            state.commit_action(None, None);

            // Capture the heads a remote peer would have forked from (pre local action).
            let before_local = state.doc.get_heads();

            // LOCAL action: append " World".
            state.doc.splice_text(&text, 5, 0, " World").unwrap();
            state.commit_action(None, None);

            // CONCURRENT remote edit forked from before the local action: prepend "X".
            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            // Merge the remote delta into the live doc the way collab sync does.
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "precondition merge"
            );

            // UNDO the local action.
            state.undo(None, None).expect("undo pops the local action");

            // Local " World" reverted; remote "X" survives.
            assert_eq!(
                text_body(&state, &text),
                "XHello",
                "remote prepend survives; only local append reverted"
            );
        }
        // phase: redo_after_concurrent_merge_keeps_both
        // Redoing the undone local action must reinstate it while *still* keeping
        // the concurrently-merged remote edit.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
            state.commit_action(None, None);
            let before_local = state.doc.get_heads();
            state.doc.splice_text(&text, 5, 0, " World").unwrap();
            state.commit_action(None, None);

            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();

            state.undo(None, None).unwrap();
            assert_eq!(text_body(&state, &text), "XHello");
            state.redo(None, None).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "redo reinstates local append; remote prepend still present"
            );
        }
        // phase: undo_text_delete_with_concurrent_prepend
        // A local text *deletion* undone after a concurrent remote prepend must
        // re-insert the deleted text at the correctly-shifted position.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello World").unwrap();
            state.commit_action(None, None);
            let before_local = state.doc.get_heads();

            // LOCAL action: delete " World".
            state.doc.splice_text(&text, 5, 6, "").unwrap();
            state.commit_action(None, None);

            // Concurrent remote prepend "X".
            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();
            assert_eq!(text_body(&state, &text), "XHello");

            state.undo(None, None).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "deleted text restored at shifted position; remote survives"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn author_scoped_undo_and_redo_touch_only_that_authors_actions() {
        // phase: undo_for_author_reverts_only_that_authors_op
        // `undo_for_author` reverts only the requested author's most-recent action
        // as a new forward change, leaving other authors' committed ops intact in
        // the live document — not just unpopped from the stack.
        {
            let mut state = State::new();
            // Alice sets a=1
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action_authored(None, None, Some(UserId("alice".into())));
            // Bob sets b=1
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action_authored(None, None, Some(UserId("bob".into())));

            // Alice undoes — only "a" is reverted; Bob's "b" stays in the doc.
            let popped = state
                .undo_for_author(&UserId("alice".into()), None, None)
                .expect("alice's entry");
            assert_eq!(popped.author, Some(UserId("alice".into())));
            assert!(
                state.doc.get(&ROOT, "a").unwrap().is_none(),
                "alice's change reverted in the live doc"
            );
            assert!(
                state.doc.get(&ROOT, "b").unwrap().is_some(),
                "bob's change survives in the live doc, not just on the stack"
            );
        }
        // phase: undo_as_an_author_skips_a_collaborators_action
        // `undo_as` is the entry point `App`'s Ctrl+Z takes. With an author it
        // must behave like `undo_for_author` — walk past a collaborator's action
        // to the local user's own — not like plain LIFO undo.
        {
            let mut state = State::new();
            let alice = UserId("alice".into());
            let bob = UserId("bob".into());

            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action_authored(None, None, Some(alice.clone()));
            state.doc.put(&ROOT, "b", 1).unwrap();
            // `bob` is never referenced again, so this is its last use.
            state.commit_action_authored(None, None, Some(bob));

            // Alice hits Ctrl+Z. The newest entry is Bob's.
            let popped = state
                .undo_as(Some(&alice), None, None)
                .expect("alice has an action to undo");
            assert_eq!(
                popped.author,
                Some(alice.clone()),
                "alice undid her own action"
            );
            assert!(
                state.doc.get(&ROOT, "a").unwrap().is_none(),
                "alice's own change is reverted"
            );
            assert!(
                state.doc.get(&ROOT, "b").unwrap().is_some(),
                "bob's change survives alice's undo"
            );
        }
        // phase: undo_as_without_an_author_is_plain_lifo
        // With no identity at all — an app running outside any session — `undo_as`
        // degrades to plain LIFO rather than refusing to undo.
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action(None, None);

            state
                .undo_as(None, None, None)
                .expect("undo pops the newest");
            assert!(state.doc.get(&ROOT, "b").unwrap().is_none());
            assert!(state.doc.get(&ROOT, "a").unwrap().is_some());
        }
        // phase: redo_as_an_author_only_reapplies_that_authors_undo
        // Redo is the mirror: Bob's undone action must not be re-applied by
        // Alice's Ctrl+Y.
        {
            let mut state = State::new();
            let alice = UserId("alice".into());
            let bob = UserId("bob".into());

            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action_authored(None, None, Some(alice.clone()));
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action_authored(None, None, Some(bob.clone()));

            // Bob undoes his own action, putting it on the redo stack.
            state.undo_as(Some(&bob), None, None).unwrap();
            assert!(state.doc.get(&ROOT, "b").unwrap().is_none());

            // Alice hits Ctrl+Y. There is nothing of hers to redo, and Bob's
            // undone action must stay undone.
            assert!(
                state.redo_as(Some(&alice), None, None).is_none(),
                "alice has nothing to redo"
            );
            assert!(
                state.doc.get(&ROOT, "b").unwrap().is_none(),
                "alice's redo must not resurrect bob's undone action"
            );

            // Bob's own redo works.
            state
                .redo_as(Some(&bob), None, None)
                .expect("bob redoes his own");
            assert!(state.doc.get(&ROOT, "b").unwrap().is_some());
        }
        // phase: can_undo_predicts_exactly_what_undo_as_does
        // The availability a control renders from must be the availability the
        // act has. Asserted by *doing* the undo and checking the query predicted
        // it — not by reading the stack length, which is what would let the two
        // drift.
        {
            let mut state = State::new();
            let a = alice();

            assert!(
                !state.can_undo_as(Some(&a)),
                "a fresh document has no history"
            );
            assert!(!state.can_redo_as(Some(&a)));
            assert!(
                state.undo_as(Some(&a), None, None).is_none(),
                "and undo agrees"
            );

            type_action(&mut state, "body", "hello", &a);
            assert!(state.can_undo_as(Some(&a)));
            assert!(!state.can_redo_as(Some(&a)));

            assert!(
                state.undo_as(Some(&a), None, None).is_some(),
                "can_undo promised an undo; it has to be there"
            );
            assert!(!state.can_undo_as(Some(&a)), "the stack is empty again");
            assert!(state.can_redo_as(Some(&a)), "and the action is redoable");

            assert!(state.redo_as(Some(&a), None, None).is_some());
            assert!(state.can_undo_as(Some(&a)));
            assert!(!state.can_redo_as(Some(&a)));
        }
        // phase: can_undo_is_author_scoped_like_the_shortcut
        // A session where every entry belongs to a collaborator must report
        // nothing to undo. Without author scoping the control would light up and
        // then do nothing — or, worse, revert someone else's work.
        {
            let mut state = State::new();
            type_action(&mut state, "body", "hello", &alice());

            assert!(state.can_undo_as(Some(&alice())));
            assert!(
                !state.can_undo_as(Some(&bob())),
                "Bob has performed no action here"
            );
            assert!(
                state.undo_as(Some(&bob()), None, None).is_none(),
                "and undo declines for Bob — the query must match"
            );
            // With no identity at all the history is plain LIFO, and the query
            // follows that too.
            assert!(state.can_undo_as(None));
        }
    }

    // ================================================================
    // What a widget is told about the history
    // ================================================================

    fn alice() -> crate::permissions::UserId {
        crate::permissions::UserId("alice".into())
    }

    fn bob() -> crate::permissions::UserId {
        crate::permissions::UserId("bob".into())
    }

    /// Type `text` into a fresh text field and commit it as one action by
    /// `author` — a real edit through the real commit path, so the summary
    /// under test describes a change that actually happened.
    fn type_action(state: &mut State, key: &str, text: &str, author: &crate::permissions::UserId) {
        let obj = state
            .doc
            .put_object(&automerge::ROOT, key, automerge::ObjType::Text)
            .unwrap();
        state.doc.splice_text(&obj, 0, 0, text).unwrap();
        assert!(
            state.commit_action_authored(None, None, Some(author.clone())),
            "the edit must actually have committed"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn undo_status_and_revision_describe_and_bound_every_stack_change() {
        // phase: undo_status_describes_the_action_on_the_stack
        // The status a widget receives describes the change that is on the
        // stack, through the same producer the history pane uses.
        {
            let mut state = State::new();
            let a = alice();

            let empty = state.undo_status(Some(&a));
            assert_eq!(empty, crate::undo::UndoStatus::empty());

            type_action(&mut state, "body", "hello", &a);
            let s = state.undo_status(Some(&a));
            assert!(s.can_undo && !s.can_redo);
            assert_eq!(
                s.undo_summary.as_deref(),
                Some("+5 -0 chars"),
                "the summary counts the characters the action actually inserted"
            );
            assert_eq!(s.redo_summary, None, "nothing has been undone yet");
        }
        // phase: redo_summary_describes_the_action_restored_not_the_reversal
        // Redo's summary must read as the action being put *back*, not as the
        // reversal that is physically on the redo stack — otherwise "Redo" is
        // described to the user as deleting their text.
        {
            let mut state = State::new();
            let a = alice();
            type_action(&mut state, "body", "hello", &a);
            state.undo_as(Some(&a), None, None).unwrap();

            let s = state.undo_status(Some(&a));
            assert!(!s.can_undo && s.can_redo);
            assert_eq!(
                s.redo_summary.as_deref(),
                Some("+5 -0 chars"),
                "redo puts five characters back"
            );
        }
        // phase: the_revision_moves_on_every_stack_change
        // The status is only ever recomputed when the revision moves, so the
        // revision has to move on every stack change — a miss leaves a control
        // rendering a history that is no longer there.
        {
            let mut state = State::new();
            let a = alice();

            let start = state.history_revision();
            type_action(&mut state, "body", "hi", &a);
            let after_commit = state.history_revision();
            assert_ne!(after_commit, start, "committing an action moves it");

            state.undo_as(Some(&a), None, None).unwrap();
            let after_undo = state.history_revision();
            assert_ne!(after_undo, after_commit, "undo moves it");

            state.redo_as(Some(&a), None, None).unwrap();
            let after_redo = state.history_revision();
            assert_ne!(after_redo, after_undo, "redo moves it");

            // An event that changes nothing must NOT move it, or the host
            // re-derives the summaries every frame — and each derivation closes
            // the open transaction.
            assert!(!state.commit_action_authored(None, None, Some(a.clone())));
            assert_eq!(
                state.history_revision(),
                after_redo,
                "an empty commit is not a history change"
            );

            // The author-less paths are separate methods with their own stack
            // handling; walk them too, or a missing bump there ships unseen.
            let mut anon = State::new();
            anon.doc.put(&automerge::ROOT, "k", 1).unwrap();
            anon.commit_action(None, None);
            let anon_committed = anon.history_revision();
            anon.undo(None, None).unwrap();
            let anon_undone = anon.history_revision();
            assert_ne!(anon_undone, anon_committed, "plain undo moves the revision");
            anon.redo(None, None).unwrap();
            assert_ne!(
                anon.history_revision(),
                anon_undone,
                "plain redo moves the revision"
            );

            // Swapping the document resets the stacks; the revision must still
            // move, so a status cached against the old document is not reused.
            let mut other = automerge::AutoCommit::new();
            other.put(&automerge::ROOT, "k", 1).unwrap();
            state.adopt_doc(other);
            assert_ne!(
                state.history_revision(),
                after_redo,
                "adopting a document is a history change"
            );
            assert!(
                !state.can_undo_as(Some(&a)),
                "the new document has no history"
            );
        }
        // phase: exhausted_history_revision_refuses_transitions_without_mutating
        {
            let mut state = State::new();
            let author = alice();
            type_action(&mut state, "body", "before exhaustion", &author);
            let heads_before_undo = state.doc.get_heads();
            state.undo.revision = u64::MAX;

            assert!(
                state.undo_as(Some(&author), None, None).is_none(),
                "undo must fail rather than reuse an old revision"
            );
            assert_eq!(state.doc.get_heads(), heads_before_undo);
            assert!(state.can_undo_as(Some(&author)), "the entry was not popped");

            state.doc.put(&automerge::ROOT, "later", 1).unwrap();
            let heads_before_commit = state.doc.get_heads();
            assert!(
                !state.commit_action_authored(None, None, Some(author)),
                "committing at exhaustion must fail closed"
            );
            assert_eq!(state.doc.get_heads(), heads_before_commit);
            assert_eq!(state.history_revision(), u64::MAX);

            let replacement = automerge::AutoCommit::new();
            assert!(!state.adopt_doc(replacement));
            assert_eq!(state.doc.get_heads(), heads_before_commit);
        }
        // phase: task4_bound_history_revision_limit_refuses_undo_redo_and_restore
        {
            let mut doc = AutoCommit::new();
            let mut history = UndoLog::new(&mut doc);
            let before = doc.get_heads();
            doc.put(ROOT, "value", 1).unwrap();
            history.record_committed_action_in(&mut doc, before.clone(), None, None, alice(), 11);
            history.undo_in(&mut doc, &alice(), 11, None, None).unwrap();
            history.revision = u64::MAX;
            let heads = doc.get_heads();
            assert!(
                history
                    .redo_in(&mut doc, &alice(), 11, None, None)
                    .is_none()
            );
            assert!(!history.restore_to_heads_in(&mut doc, &before, None, None, alice(), 11));
            assert_eq!(doc.get_heads(), heads);
            assert_eq!(
                history.next_redo_in(&alice(), 11).unwrap().instance_id,
                Some(11)
            );
        }
        // phase: a_restore_is_visible_to_a_control
        // A restore from the history scrubber lands on the undo stack, so a
        // control must report it as undoable.
        {
            let mut state = State::new();
            let a = alice();
            type_action(&mut state, "body", "hello", &a);
            let version = state.doc.get_heads();
            type_action(&mut state, "other", "more", &a);

            assert!(state.restore_to_heads(&version, None, None, Some(a.clone())));
            assert!(
                state.can_undo_as(Some(&a)),
                "the restore itself is undoable"
            );
        }
    }

    #[test]
    fn undo_of_one_mark_over_several_texts_clears_it_from_each() {
        use automerge::marks::{ExpandMark, Mark};
        let mut state = State::new();
        let texts: Vec<ObjId> = ["Background", "Method", "Results"]
            .into_iter()
            .map(|word| {
                let text = state.doc.put_object(ROOT, word, ObjType::Text).unwrap();
                state.doc.splice_text(&text, 0, 0, word).unwrap();
                text
            })
            .collect();
        state.commit_action(None, None);
        for text in &texts {
            let end = state.doc.length(text);
            state
                .doc
                .mark(
                    text,
                    Mark::new("bold".into(), true, 0, end),
                    ExpandMark::After,
                )
                .unwrap();
        }
        state.commit_action(None, None);
        state.undo(None, None).unwrap();
        for text in &texts {
            assert_eq!(state.doc.marks(text).unwrap(), [], "{text}");
        }
    }

    #[test]
    fn undo_of_several_splices_in_formatted_text_restores_text_and_marks() {
        use automerge::marks::{ExpandMark, Mark};
        let mut state = State::new();
        let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        state
            .doc
            .splice_text(&text, 0, 0, "Project Falcon remains")
            .unwrap();
        state
            .doc
            .mark(
                &text,
                Mark::new("bold".into(), true, 0, 14),
                ExpandMark::After,
            )
            .unwrap();
        state.commit_action(None, None);
        let before = state.doc.marks(&text).unwrap();
        // One action: insert inside the bold run, then delete on both sides.
        state.doc.splice_text(&text, 9, 0, "Osprey").unwrap();
        state.doc.splice_text(&text, 8, 1, "").unwrap();
        state.doc.splice_text(&text, 14, 5, "").unwrap();
        state.commit_action(None, None);
        assert_eq!(state.doc.text(&text).unwrap(), "Project Osprey remains");
        let after = state.doc.marks(&text).unwrap();
        for _ in 0..2 {
            state.undo(None, None).unwrap();
            assert_eq!(state.doc.text(&text).unwrap(), "Project Falcon remains");
            assert_eq!(state.doc.marks(&text).unwrap(), before);
            state.redo(None, None).unwrap();
            assert_eq!(state.doc.text(&text).unwrap(), "Project Osprey remains");
            assert_eq!(state.doc.marks(&text).unwrap(), after);
        }
    }

    #[test]
    fn undo_of_formatted_text_keeps_a_concurrent_edit_in_it() {
        use automerge::marks::{ExpandMark, Mark};
        let mut state = State::new();
        let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        state
            .doc
            .splice_text(&text, 0, 0, "Project Falcon remains")
            .unwrap();
        state
            .doc
            .mark(
                &text,
                Mark::new("bold".into(), true, 0, 14),
                ExpandMark::After,
            )
            .unwrap();
        state.commit_action(None, None);
        let before_local = state.doc.get_heads();
        state.doc.splice_text(&text, 9, 0, "Osprey").unwrap();
        state.doc.splice_text(&text, 8, 1, "").unwrap();
        state.doc.splice_text(&text, 14, 5, "").unwrap();
        state.commit_action(None, None);
        // A collaborator, concurrently, prepends to the paragraph and appends
        // to it.
        let mut peer = state.doc.fork_at(&before_local).unwrap();
        peer.splice_text(&text, 0, 0, "New: ").unwrap();
        peer.splice_text(&text, 27, 0, " today").unwrap();
        peer.commit();
        state
            .load_incremental(&peer.save_after(&before_local))
            .unwrap();
        assert_eq!(
            state.doc.text(&text).unwrap(),
            "New: Project Osprey remains today"
        );
        state.undo(None, None).unwrap();
        assert_eq!(
            state.doc.text(&text).unwrap(),
            "New: Project Falcon remains today"
        );
        let bold: Vec<_> = state
            .doc
            .marks(&text)
            .unwrap()
            .into_iter()
            .map(|mark| (mark.name.to_string(), mark.start, mark.end))
            .collect();
        assert_eq!(bold, [("bold".to_owned(), 5, 19)]);
    }

    #[test]
    fn revert_beside_a_concurrent_edit_keeps_wide_characters_whole_in_every_encoding() {
        use automerge::TextEncoding;
        for encoding in [
            TextEncoding::UnicodeCodePoint,
            TextEncoding::Utf8CodeUnit,
            TextEncoding::Utf16CodeUnit,
            TextEncoding::GraphemeCluster,
        ] {
            let width = |s: &str| match encoding {
                TextEncoding::Utf8CodeUnit => s.len(),
                TextEncoding::Utf16CodeUnit => s.encode_utf16().count(),
                // Every character here is its own grapheme.
                TextEncoding::UnicodeCodePoint | TextEncoding::GraphemeCluster => s.chars().count(),
            };
            let mut doc = AutoCommit::new_with_encoding(encoding);
            let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, "a🦀b").unwrap();
            let before = doc.get_heads();
            // Insert after the crab, then delete the character before it.
            doc.splice_text(&text, width("a🦀"), 0, "XY").unwrap();
            doc.splice_text(&text, 0, 1, "").unwrap();
            let after = doc.get_heads();
            assert_eq!(doc.text(&text).unwrap(), "🦀XYb", "{encoding:?}");
            let mut peer = doc.fork_at(&before).unwrap();
            peer.splice_text(&text, width("a🦀b"), 0, "!").unwrap();
            peer.commit();
            doc.load_incremental(&peer.save_after(&before)).unwrap();
            assert_eq!(doc.text(&text).unwrap(), "🦀XYb!", "{encoding:?}");
            write_revert_patches(&mut doc, &after, &before);
            doc.commit();
            assert_eq!(doc.text(&text).unwrap(), "a🦀b!", "{encoding:?}");
        }
    }

    /// Undoing the action that created a paragraph keeps the paragraph when a
    /// collaborator has written in it since: their words are not the undoing
    /// author's to take back.
    #[test]
    fn undo_keeps_a_created_paragraph_a_collaborator_wrote_in() {
        fn paragraph(state: &mut State, blocks: &ObjId, id: &str) -> ObjId {
            let block = state.doc.put_object(blocks, id, ObjType::Map).unwrap();
            state.doc.put(&block, "order", id).unwrap();
            state.doc.put_object(&block, "text", ObjType::Text).unwrap()
        }
        fn texts(state: &State, blocks: &ObjId) -> Vec<(String, String)> {
            state
                .doc
                .keys(blocks)
                .map(|id| {
                    let block = state.doc.get(blocks, &id).unwrap().unwrap().1;
                    let text = state.doc.get(&block, "text").unwrap().unwrap().1;
                    (id, state.doc.text(&text).unwrap())
                })
                .collect()
        }
        let mut state = State::new();
        let blocks = state.doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
        let first = paragraph(&mut state, &blocks, "a");
        state.commit_action(None, None);
        // Type a word, then press Enter: a new paragraph after it.
        state.doc.splice_text(&first, 0, 0, "hello").unwrap();
        state.commit_action(None, None);
        paragraph(&mut state, &blocks, "b");
        state.commit_action(None, None);
        // A collaborator types into the new paragraph.
        let seen = state.doc.get_heads();
        let mut peer = state.doc.fork_at(&seen).unwrap();
        let block = peer.get(&blocks, "b").unwrap().unwrap().1;
        let text = peer.get(&block, "text").unwrap().unwrap().1;
        peer.splice_text(&text, 0, 0, "their words").unwrap();
        peer.commit();
        state.load_incremental(&peer.save_after(&seen)).unwrap();

        state.undo(None, None).unwrap();
        state.undo(None, None).unwrap();
        assert_eq!(
            texts(&state, &blocks),
            [
                ("a".to_owned(), String::new()),
                ("b".to_owned(), "their words".to_owned())
            ]
        );
        // Redo puts back only what the undo took.
        state.redo(None, None).unwrap();
        state.redo(None, None).unwrap();
        assert_eq!(
            texts(&state, &blocks),
            [
                ("a".to_owned(), "hello".to_owned()),
                ("b".to_owned(), "their words".to_owned())
            ]
        );
    }
}
