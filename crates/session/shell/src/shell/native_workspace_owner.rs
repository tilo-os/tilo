//! Actual native semantic owner, committed through the existing state.json writer.
//!
//! The owner itself is `tilo_joined_room`'s; this host keeps its record
//! inside the state file ([`TreeStore`]), so every owner publication also
//! binds an enrolled slot, projects the room into the tree and exports the
//! resulting file-system capabilities under the same writer lock.
mod local_filesystem_consents;
pub(crate) mod publication;
#[cfg(test)]
mod publication_tests;
pub(crate) mod store;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{test_enrolled_owner, tree_of};

use super::{
    persist,
    state::{RoomId, WorkspaceId, WorkspaceTreeHandle},
};
pub use store::NativeStateRuntime;
pub(crate) use store::NativeStateWitness;
pub use tilo_joined_room::NativeAuthorityStorage;
use tilo_joined_room::{Change, Store};
pub(crate) use tilo_joined_room::{DocumentRead, NativeCommittedView, NativeWorkspaceOwner};
#[cfg(test)]
pub(crate) use tilo_joined_room::{NativeBootstrap, PendingEnrollmentId, PendingEnrollmentStatus};

#[cfg(test)]
thread_local! {
    /// Owner publications this thread made, so a test can weigh what each costs.
    static PUBLICATIONS_ON_THIS_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The owner's record as one field of the state file, written through the
/// state file's single writer.
struct TreeStore {
    tree: WorkspaceTreeHandle,
    runtime: NativeStateRuntime,
    history: tilo_joined_room::HistoryContent,
}

impl Store for TreeStore {
    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        self.history.put(bytes)
    }
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.history.read(hash)
    }
    fn healthy(&self) -> bool {
        self.runtime.healthy()
    }

    fn uncertain(&self, room: RoomId) -> bool {
        self.runtime.requires_uncertain(room)
    }

    fn slot_room(&self, slot: WorkspaceId) -> Option<RoomId> {
        self.tree.read().room_of(slot)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the state writer stays locked from the slot check through publication and export"
    )]
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        #[cfg(test)]
        PUBLICATIONS_ON_THIS_THREAD.with(|count| count.set(count.get().saturating_add(1)));
        let mut tree = self.tree.write();
        let path = tree
            .persist_path
            .clone()
            .ok_or("native owner state-file path removed")?;
        // Everything but the documents: encoded once below, and installed
        // with the live documents moved into it only once it is published.
        let mut candidate = tree.clone_without_documents();
        let mut bound = false;
        if let Change::Room {
            room,
            bind: Some(target),
            ..
        } = change
        {
            if tree.room_of(target.workspace) != Some(target.prior_room) {
                return Err(
                    "enrollment target or pending generation changed before completion".into(),
                );
            }
            // This pure helper cannot reach any persistence/export/publisher channel.
            candidate.set_room_in_memory(target.workspace, room);
            if candidate.room_of(target.workspace) != Some(room) {
                return Err("enrollment local slot cannot be bound".into());
            }
            bound = true;
        }
        candidate.native_authority = next.clone();
        if let Change::Room { room, peer, .. } = change {
            candidate.project_native_room(room, peer);
        }
        // The encode is bounded: an oversized candidate is refused here.
        let bytes = persist::encode_tree(&candidate).map_err(|e| e.to_string())?;
        let sequence = tree.native_runtime.next_sequence();
        let published = tree
            .native_runtime
            .publish(
                &path,
                &tree.native_authority,
                &candidate.native_authority,
                &bytes,
                sequence,
            )
            .map_err(|e| e.to_string())?;
        // The tree is held from the sequence through here, so no later
        // snapshot of it can have reached the file first.
        if published == store::Publication::Superseded {
            return Err("native owner publication was superseded".into());
        }
        candidate.adopt_documents_of(&mut tree);
        if bound {
            // The binding mirrored the room into the candidate's placeholder
            // document; record it in the live one.
            candidate.seed_doc_from_cache();
        }
        match change {
            Change::Pending => {
                install();
                *tree = candidate;
                persist::save_document(&tree, &path);
            }
            Change::Room { .. } => {
                install();
                *tree = candidate;
                // Only the acknowledged final admitted projection reaches authority exports.
                tree.export_fs_capabilities();
                crate::shell::launcher::action_authority::Export::of(&tree).submit();
            }
        }
        Ok(())
    }
}

/// The owner of the state file behind `tree`, attached to its writer.
///
/// # Errors
/// The writer belongs to another tree, the file's record is invalid, or the
/// reopened generation cannot be published.
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers hand over their tree handle; the owner retains a clone of it"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the state writer stays locked from owner lookup through installation and export"
)]
pub(crate) fn open(tree: WorkspaceTreeHandle) -> Result<NativeWorkspaceOwner, String> {
    let mut native = tree.write();
    if let Some(owner) = native.native_runtime.owner_of(&tree)? {
        return Ok(owner);
    }
    store::preflight(&native.native_authority, store::MAX_AUTHORITY_BYTES)
        .map_err(|e| e.to_string())?;
    let path = native
        .persist_path
        .clone()
        .ok_or("native owner requires the actual state-file path")?;
    native
        .native_runtime
        .bind(&path, &native.native_authority)
        .map_err(|e| e.to_string())?;
    if !native.native_authority.every_pending_has_slot() {
        return Err("native pending enrollment has no local slot".into());
    }
    store::preflight(&*native, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
    let reopened = native.clone();
    let owner = NativeWorkspaceOwner::restore(
        std::sync::Arc::new(TreeStore {
            tree: tree.clone(),
            runtime: native.native_runtime.clone(),
            history: tilo_joined_room::HistoryContent::for_state_file(&path),
        }),
        native.native_authority.clone(),
        std::time::SystemTime::now(),
    )?;
    // Re-establish the publication barrier before exposing a reopened generation.
    let bytes = persist::encode_tree(&reopened).map_err(|e| e.to_string())?;
    let sequence = native.native_runtime.next_sequence();
    let published = native
        .native_runtime
        .publish(
            &path,
            &native.native_authority,
            &reopened.native_authority,
            &bytes,
            sequence,
        )
        .map_err(|e| e.to_string())?;
    if published == store::Publication::Superseded {
        return Err("reopened native generation was superseded".into());
    }
    *native = reopened;
    for room in owner.rooms() {
        owner.with_current(room, |view| {
            native.project_native_room(room, view.peer());
            Ok(())
        })?;
    }
    native.native_runtime.attach(&owner, &tree);
    native.export_fs_capabilities();
    Ok(owner)
}

#[cfg(test)]
pub(crate) fn test_tree_of(owner: &NativeWorkspaceOwner) -> WorkspaceTreeHandle {
    tests::tree_of(owner)
}
