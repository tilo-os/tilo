//! A replica's changes merged into an open resource, validated on a private copy first.

use automerge::{AutoCommit, ReadDoc as _};
use tilo_automerge::ApplyIncoming as _;

use super::{DocumentCheckout, ResourceStore};

/// Apply an incremental stream to `doc`, whole or not at all: the hashes of the changes it
/// carried, parsed once.
pub(super) fn load_incremental(
    doc: &mut AutoCommit,
    bytes: &[u8],
) -> Result<Vec<automerge::ChangeHash>, String> {
    let changes = automerge::Automerge::parse_changes(bytes).map_err(|error| error.to_string())?;
    let carried = changes.iter().map(automerge::Change::hash).collect();
    doc.apply_incoming(changes)
        .map_err(|error| error.to_string())?;
    Ok(carried)
}

impl ResourceStore {
    /// Merge a replica's mutation into the open resource, validated on a private
    /// copy first: whether it changed, and the change hashes it carried.
    /// # Errors
    /// The bytes are not a valid mutation of this resource, or it is not open.
    pub fn merge_replica(
        &self,
        checkout: &DocumentCheckout,
        mutation: collab_types::DocumentMutation,
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        self.merge_with(checkout, mutation, None)
    }

    /// [`Self::merge_replica`] of `mutation`, which brings the resource to `heads`. Changes a
    /// reader on this thread brought its own copy of the resource to `heads` with (a page's
    /// replica, [`tilo_automerge::shared::hold_at`]) are taken in as that copy, as a snapshot
    /// is, rather than applied again: a tracker's write of hundreds of issues is most of a
    /// second to apply on a page, and a copy a millisecond.
    /// # Errors
    /// As [`Self::merge_replica`].
    pub fn merge_at(
        &self,
        checkout: &DocumentCheckout,
        mutation: collab_types::DocumentMutation,
        heads: &[automerge::ChangeHash],
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        let held = match &mutation {
            collab_types::DocumentMutation::Incremental(_) => {
                tilo_automerge::shared::held_at(heads)
            }
            collab_types::DocumentMutation::Snapshot(_) => None,
        };
        self.merge_with(checkout, mutation, held)
    }

    fn merge_with(
        &self,
        checkout: &DocumentCheckout,
        mutation: collab_types::DocumentMutation,
        held: Option<AutoCommit>,
    ) -> Result<(bool, Vec<automerge::ChangeHash>), String> {
        let identity = self.replica_identity_stamp(checkout)?;
        let merged = self.with_action_state(&checkout.id, |doc, history, actions| {
            let mut local = doc.clone();
            actions.commit_for_save(&mut local);
            let before = local.get_heads();
            let whole = match (&mutation, held) {
                (_, Some(held)) => Ok(held),
                // The workspace's replica on this thread holds the document these bytes
                // were saved from: a copy of it, not a load.
                (collab_types::DocumentMutation::Snapshot(bytes), None) => {
                    Ok(tilo_automerge::shared::load(bytes).map_err(|error| error.to_string())?)
                }
                (collab_types::DocumentMutation::Incremental(bytes), None) => Err(bytes),
            };
            let mut candidate;
            // The heads the payload names: a snapshot re-delivered unchanged
            // still tells the route that these are the sender's own.
            let delivered = match whole {
                Ok(mut incoming) => {
                    // The workspace's empty document for a resource a member is
                    // creating carries nothing to merge, and no identity.
                    if incoming.get_heads().is_empty() {
                        return Ok((false, Vec::new()));
                    }
                    identity.validate(&incoming)?;
                    let heads = incoming.get_heads();
                    // Merging copies every change the receiving side lacks, so
                    // the local changes go into the snapshot rather than the
                    // other way round: a late joiner's replica is a few
                    // changes next to a workspace's checkpoint. Every ancestor of
                    // the heads both sides know is already in the snapshot.
                    let shared: Vec<automerge::ChangeHash> = heads
                        .iter()
                        .copied()
                        .filter(|head| local.get_change_meta_by_hash(head).is_some())
                        .collect();
                    if shared.len() == heads.len() {
                        candidate = local.clone();
                    } else if !local.get_missing_deps(&[]).is_empty() {
                        // Changes the replica received before the ones they
                        // build on wait in it, and only there: copying the
                        // replica's changes into the snapshot would leave
                        // them behind, so the snapshot's come to the replica.
                        candidate = local.clone();
                        candidate
                            .load_incoming(&incoming.save_after(&[]))
                            .map_err(|error| error.to_string())?;
                    } else {
                        // Merging walks back to what both sides hold. Saving
                        // after the shared heads alone would re-encode the
                        // whole local history when the snapshot is ahead.
                        incoming.set_actor(local.get_actor().clone());
                        incoming
                            .merge_incoming(&mut local)
                            .map_err(|error| error.to_string())?;
                        candidate = incoming;
                    }
                    heads
                }
                Err(bytes) => {
                    candidate = local.clone();
                    // What the changes carried beyond what applied: a change whose history this
                    // replica lacks waits for it, but this route delivered it now.
                    let mut carried = load_incremental(&mut candidate, bytes)?;
                    carried.retain(|hash| !candidate.has_change(hash));
                    carried
                }
            };
            identity.validate(&candidate)?;
            let changed = candidate.get_heads() != before;
            // What the route delivered. A head stands for all of its
            // ancestors only in an ancestry frontier; this one names each
            // delivered change, so the merge's applied changes are listed,
            // and so are the ones still waiting for history this replica
            // lacks. Neither is ever sent back on the route that brought it.
            // Told by their hashes alone: rebuilding every change the merge brought (a
            // newcomer's window takes in a tracker of thousands of them at once) decoded each
            // of their ops again, seconds on a page, for hashes the graph holds.
            let mut observed = candidate
                .committed_document()
                .change_hashes()
                .iter()
                .filter(|hash| !local.has_change(hash))
                .copied()
                .collect::<Vec<_>>();
            observed.extend(delivered);
            let mut seen = std::collections::HashSet::new();
            observed.retain(|hash| seen.insert(*hash));
            if changed {
                // Finish the local span against LOCAL heads, never the merged
                // heads: undoing it must leave the remote author's edits intact.
                actions.finish_external(&mut local, history);
                history.rebase(&mut candidate);
                actions.generation = actions.generation.and_then(|value| value.checked_add(1));
            }
            // Keep valid changes with dependencies still in flight, too.
            let missing = candidate.get_missing_deps(&[]);
            if !missing.is_empty() {
                // Shown until they arrive; a resource that keeps waiting never
                // received a change another replica built on.
                tracing::debug!(
                    resource = %checkout.id,
                    missing = ?missing.iter().take(4).map(|hash| hash.to_string()).collect::<Vec<_>>(),
                    "remote changes wait for changes this replica has not received"
                );
            }
            *doc = candidate;
            Ok((changed, observed))
        })
        .map_err(|error| error.to_string())?;
        if let Ok((true, _)) = &merged {
            self.peer_change_merged(&checkout.id);
        }
        merged
    }
}
