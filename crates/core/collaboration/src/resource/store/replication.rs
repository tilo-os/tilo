//! The store's replication half: a peer's document merged in, and the
//! deltas and snapshots another replica catches up from.

use super::*;

impl ResourceStore {
    /// Merge a peer's full document into an open record, exactly as a
    /// member's state arriving over sync is merged: validated first,
    /// and without moving the local author's undo history onto the peer's
    /// edits. Returns whether anything changed.
    ///
    /// # Errors
    /// Returns why the peer's document was refused; the record is unchanged.
    pub fn merge_peer_document(
        &self,
        checkout: &DocumentCheckout,
        document: Vec<u8>,
    ) -> Result<bool, String> {
        self.merge_replica(checkout, collab_types::DocumentMutation::Snapshot(document))
            .map(|(changed, _)| changed)
    }

    /// Merge another replica's incremental changes (what
    /// [`Self::replication_delta`] answers there) into the open record, as
    /// the workspace's broker delivers one window's changes to the others on the
    /// record. Whether anything changed.
    ///
    /// # Errors
    /// The record is not open here or the changes do not merge.
    pub fn merge_peer_changes(
        &self,
        checkout: &DocumentCheckout,
        changes: Vec<u8>,
    ) -> Result<bool, String> {
        self.merge_replica(
            checkout,
            collab_types::DocumentMutation::Incremental(changes),
        )
        .map(|(changed, _)| changed)
    }

    /// Changes to the open record `id` that `frontier` has not covered, for a
    /// host replicating open records itself (a browser page over the relay).
    /// The record goes whole, another replica's changes included; a route
    /// publishes its own alone ([`Self::own_delta`]).
    /// The pending action is committed first, as for a save. Advance the
    /// frontier with the returned heads only once the delta was accepted.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn replication_delta(
        &self,
        id: &DocumentId,
        frontier: &mut crate::state::DocumentFrontier,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), ResourceOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let (delta, heads) = frontier.pending_mirror_delta(doc);
            if delta.is_empty() {
                actions.notices.replicating();
            } else {
                actions.notices.sent(&heads);
            }
            (delta, heads)
        })
    }

    /// The changes to the open record `id` this replica made and `frontier`
    /// has not published yet, for a route, which publishes its own device's
    /// changes and never another replica's
    /// ([`crate::state::DocumentFrontier::pending_delta`]). The pending action
    /// is committed first, as for a save. Advance the frontier with the
    /// returned heads only once the delta was accepted.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn own_delta(
        &self,
        id: &DocumentId,
        frontier: &mut crate::state::DocumentFrontier,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), ResourceOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let own = actions.writers.borrow().clone();
            let (delta, heads) = frontier.pending_delta(doc, &own);
            if delta.is_empty() {
                actions.notices.replicating();
            } else {
                actions.notices.sent(&heads);
            }
            (delta, heads)
        })
    }

    /// Bring the open record `id` up with `migrate`, and note the actors it
    /// wrote as among this replica's own: a migration is one housekeeping
    /// change whose actor is derived from the document rather than being this
    /// device's, so a route publishing this replica's own changes
    /// ([`crate::state::DocumentFrontier::pending_delta`]) sends it too.
    ///
    /// # Errors
    /// The record is not open here, its owner is busy, or `migrate` refused.
    pub fn migrate<T>(
        &self,
        id: &DocumentId,
        migrate: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut migrate = Some(migrate);
        let mut outcome = None;
        self.migrate_with(id, &mut |doc| {
            outcome = migrate.take().map(|migrate| migrate(doc));
        })?;
        outcome.unwrap_or_else(|| Err("the document was not migrated".into()))
    }

    /// [`Self::migrate`] by one function for every caller: the generic
    /// shell above only carries the outcome.
    fn migrate_with(
        &self,
        id: &DocumentId,
        migrate: &mut dyn FnMut(&mut AutoCommit),
    ) -> Result<(), String> {
        self.with_action_state(id, |doc, _, actions| {
            let before = doc.get_heads();
            migrate(doc);
            for change in doc.get_changes(&before) {
                actions.writers.borrow_mut().note(change.actor_id().clone());
            }
        })
        .map_err(|error| error.to_string())
    }

    /// The open record `id` as a whole-document snapshot with its heads, for
    /// a replica's first publication.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn replication_snapshot(
        &self,
        id: &DocumentId,
    ) -> Result<(Vec<u8>, Vec<automerge::ChangeHash>), ResourceOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let heads = doc.get_heads();
            actions.notices.sent(&heads);
            (doc.save(), heads)
        })
    }

    /// The open record `id` whole, as its changes from the empty document, and
    /// its heads: for a replica handing a record it started to a host as the
    /// changes it made, while the host founds the record from the whole.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn replication_whole_and_changes(
        &self,
        id: &DocumentId,
    ) -> Result<(Vec<u8>, Vec<u8>, Vec<automerge::ChangeHash>), ResourceOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let heads = doc.get_heads();
            actions.notices.sent(&heads);
            (doc.save(), doc.save_after(&[]), heads)
        })
    }
}
