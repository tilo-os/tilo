//! Actual native semantic owner, committed through the existing state.json writer.
//!
//! The owner itself is `tilo_joined_room`'s; this host keeps its record
//! inside the state file ([`TreeStore`]), so every owner publication also
//! binds an enrolled slot, projects the room into the tree and exports the
//! resulting file-system capabilities under the same writer lock, without
//! holding the tree across the state file's I/O.
#[cfg(test)]
mod frame_loop_tests;
mod local_filesystem_consents;
pub(crate) mod publication;
#[cfg(test)]
mod publication_lock_tests;
#[cfg(test)]
mod publication_tests;
pub(crate) mod store;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use publication_tests::edit as test_document_edit;
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

    /// Publish `next` with `change` applied to the tree, and install both.
    ///
    /// The tree is never held across the state file's I/O. The writer is
    /// taken first and kept until the record is installed, so no other
    /// write of the file can land between this publication's sequence and
    /// its install; the tree is held only to copy it and take that
    /// sequence, then again to install. Between the two the frame loop,
    /// the network and every other reader and writer of the tree go on: a
    /// data sync on a loaded disk takes seconds. What they change meanwhile
    /// stays in the tree (the record and the room it projects are applied
    /// to the live tree, not swapped in from the copy), and a save they
    /// queue meanwhile, which still carries the old record, is followed by
    /// a save of the installed tree (see [`store::NativeStateRuntime::begin`]).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the tree is held through taking the sequence, so a later save of it carries a later one"
    )]
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        #[cfg(test)]
        PUBLICATIONS_ON_THIS_THREAD.with(|count| count.set(count.get().saturating_add(1)));
        let mut turn = self.runtime.begin();
        let (path, mut candidate, sequence) = {
            let mut tree = self.tree.write();
            if !tree.native_runtime.is(&self.runtime) {
                return Err("native state writer replaced under its owner".into());
            }
            let path = tree
                .persist_path
                .clone()
                .ok_or("native owner state-file path removed")?;
            // A completed enrollment installs into the workspace made for
            // the join, which is the room already: a workspace never changes
            // its room, so nothing is bound here.
            if let Change::Room {
                room,
                into: Some(target),
                ..
            } = change
                && tree.room_of(target.workspace) != Some(room)
            {
                return Err("the workspace made for the join is gone".into());
            }
            // Everything but the documents: encoded once below.
            let candidate = tree.clone_without_documents();
            // Taken while the tree is held, so any later save of it carries
            // a later sequence; it waits for the writer this turn holds.
            let sequence = turn.start();
            (path, candidate, sequence)
        };
        let prepared = Self::prepare(&mut candidate, next, &change);
        let published = prepared.and_then(|base| {
            // The encode is bounded: an oversized candidate is refused here.
            let encoded = persist::encode_tree(&candidate).map_err(|e| e.to_string())?;
            // Taken before the writer accepts the record: from then until
            // this export is published, the policy observers hold is the
            // previous record's, and they must know its replacement is on
            // the way rather than judge it withdrawn.
            let export = matches!(change, Change::Room { .. } | Change::Released { .. })
                .then(|| crate::shell::action_authority::Export::of(&candidate));
            turn.write(&path, base, &candidate.native_authority, &encoded, sequence)
                .map_err(|e| e.to_string())?;
            Ok(export)
        });
        let mut tree = self.tree.write();
        let export = match published {
            Ok(published) => published,
            Err(error) => {
                if turn.finish() {
                    tree.queue_save();
                }
                drop(tree);
                drop(turn);
                return Err(error);
            }
        };
        // The record the file now holds becomes the accepted one in the
        // same write that installs it, so nothing that reads the tree sees
        // one without the other.
        turn.accept();
        install();
        if Self::apply(&mut tree, next, &change) {
            // A room held with no workspace got one, or a workspace went
            // with its room: the tree document records it.
            tree.seed_doc_from_cache();
        }
        let follow_up = turn.finish();
        match change {
            Change::Pending => tree.native_runtime.document_log().cut(&tree.doc),
            Change::Room { .. } | Change::Released { .. } => {
                // Only the acknowledged final admitted projection reaches
                // authority exports.
                tree.export_fs_capabilities();
                if let Some(export) = export {
                    export.submit();
                }
            }
        }
        if follow_up {
            tree.queue_save();
        }
        drop(tree);
        drop(turn);
        if matches!(change, Change::Pending) {
            persist::write_document(&self.runtime, &path);
        }
        Ok(())
    }
}

impl TreeStore {
    /// Apply `next` and `change` to `candidate`, a copy of the tree, as the
    /// state file will hold them; answer the digest of the record the file
    /// holds now.
    fn prepare(
        candidate: &mut super::state::WorkspaceTree,
        next: &NativeAuthorityStorage,
        change: &Change<'_>,
    ) -> Result<[u8; 32], String> {
        // What the file must still hold: the record this tree last installed.
        let base = store::digest(&candidate.native_authority).map_err(|e| e.to_string())?;
        Self::apply(candidate, next, change);
        Ok(base)
    }

    /// Apply `next` and `change` to `tree`: the record, the room it now
    /// holds projected into its workspace (a room held with no workspace
    /// gets one), or a room let go with its workspace. Answers whether the
    /// set of workspaces changed.
    fn apply(
        tree: &mut super::state::WorkspaceTree,
        next: &NativeAuthorityStorage,
        change: &Change<'_>,
    ) -> bool {
        let before = tree.desktop.workspaces.len();
        tree.native_authority = next.clone();
        match *change {
            Change::Room { room, peer, .. } => tree.project_native_room(room, peer),
            // The room and its workspace go together.
            Change::Released { room } => {
                tree.forget_personal_room(room);
            }
            Change::Pending => {}
        }
        tree.desktop.workspaces.len() != before
    }
}

/// The owner of the state file behind `tree`, attached to its writer.
///
/// The tree is never held across the state file's I/O: binding the writer
/// (its lease, and on first use syncing every directory up to `/`) and the
/// reopened generation's publication (a data and a directory sync) each take
/// seconds on a loaded disk, and the frame loop reads the tree every tick.
/// The record is encoded from one read of the tree, published, and the
/// owner installed under a short write only if the tree still holds that
/// record. Opens of one writer are serialized by the writer itself.
///
/// # Errors
/// The writer belongs to another tree, the file's record is invalid, the
/// tree's record changed while it was being published, or the reopened
/// generation cannot be published.
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers hand over their tree handle; the owner retains a clone of it"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the writer's opening guard is held from the owner lookup through attachment, so two opens never both restore an owner"
)]
pub(crate) fn open(tree: WorkspaceTreeHandle) -> Result<NativeWorkspaceOwner, String> {
    let runtime = tree.read().native_runtime.clone();
    let _opening = runtime.opening();
    let (path, authority, encoded, sequence) = {
        let native = tree.read();
        if !native.native_runtime.is(&runtime) {
            return Err("native state writer replaced while the owner was opening".into());
        }
        if let Some(owner) = runtime.owner_of(&tree)? {
            return Ok(owner);
        }
        store::preflight(&native.native_authority, store::MAX_AUTHORITY_BYTES)
            .map_err(|e| e.to_string())?;
        let path = native
            .persist_path
            .clone()
            .ok_or("native owner requires the actual state-file path")?;
        if !native.native_authority.every_pending_has_slot() {
            return Err("native pending enrollment has no local slot".into());
        }
        store::preflight(&*native, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
        let encoded = persist::encode_tree(&native).map_err(|e| e.to_string())?;
        // Taken while the tree is held, so a save of any later state of it
        // carries a later sequence.
        let sequence = runtime.next_sequence();
        (path, native.native_authority.clone(), encoded, sequence)
    };
    runtime.bind(&path, &authority).map_err(|e| e.to_string())?;
    let owner = NativeWorkspaceOwner::restore(
        std::sync::Arc::new(TreeStore {
            tree: tree.clone(),
            runtime: runtime.clone(),
            history: tilo_joined_room::HistoryContent::for_state_file(&path),
        }),
        authority.clone(),
        std::time::SystemTime::now(),
    )?;
    // Re-establish the publication barrier before exposing a reopened generation.
    // The reopened record is the one the file holds.
    let published = runtime
        .publish(&path, encoded.authority, &authority, &encoded, sequence)
        .map_err(|e| e.to_string())?;
    // A save of a later state of the tree landed first. No owner exists to
    // change the record, so that save carried this same record and is the
    // barrier; the writer confirms it is what the file holds.
    if published == store::Publication::Superseded && !runtime.holds(encoded.authority) {
        return Err("reopened native generation was superseded".into());
    }
    let mut native = tree.write();
    if !native.native_runtime.is(&runtime) {
        return Err("native state writer replaced while the owner was opening".into());
    }
    if store::digest(&native.native_authority).map_err(|e| e.to_string())? != encoded.authority {
        return Err("native authority changed while the owner was opening".into());
    }
    for room in owner.rooms() {
        owner.with_current(room, |view| {
            native.project_native_room(room, view.peer());
            Ok(())
        })?;
    }
    runtime.attach(&owner, &tree);
    native.export_fs_capabilities();
    drop(native);
    Ok(owner)
}

/// Take the workspace that is `room` off this device, with the room: a room
/// the native owner holds, or is joining, leaves the owner's record in the
/// same publication that takes its workspace off ([`Change::Released`]), so
/// the two never part; a room of the shell's own engine is its workspace.
/// Answers the workspace's place and value when it can be put back (an
/// undo): only a room of the shell's own engine can, since a host's room is
/// joined again through its host.
///
/// # Errors
///
/// The workspace is the last one, the native owner is not open yet, or the
/// record cannot be published.
pub(crate) fn release_workspace_room(
    tree: &WorkspaceTreeHandle,
    room: RoomId,
) -> Result<Option<(usize, super::state::Workspace)>, String> {
    let (native, last) = {
        let tree = tree.read();
        let workspaces = &tree.desktop.workspaces;
        (
            tree.requires_native_owner(room),
            workspaces.len() < 2 && workspaces.iter().any(|w| w.room() == room),
        )
    };
    if last {
        return Err("the last workspace stays".into());
    }
    if !native {
        let mut tree = tree.write();
        let id = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room() == room)
            .map(|w| w.id)
            .ok_or("no workspace is that room")?;
        return Ok(tree.delete_workspace(id));
    }
    let owner = attached(tree).ok_or("this device's workspaces are still opening")?;
    if !owner.release_room(room)? {
        // A join that never reached the host's record: only its workspace
        // is here.
        tree.write().forget_personal_room(room);
    }
    Ok(None)
}

/// Delete the archived workspace that is `room`, with the room: a room the
/// native owner holds or is joining leaves its record in the same
/// publication ([`Change::Released`]); a room of the shell's own engine is
/// its workspace, answered for an undo, which archives it again.
///
/// # Errors
///
/// No archived workspace is that room, the native owner is not open yet,
/// or the record cannot be published.
pub(crate) fn delete_archived_workspace(
    tree: &WorkspaceTreeHandle,
    room: RoomId,
) -> Result<Option<super::state::Workspace>, String> {
    let native = {
        let tree = tree.read();
        if !tree.archived_workspaces.iter().any(|w| w.room() == room) {
            return Err("no archived workspace is that room".into());
        }
        tree.requires_native_owner(room)
    };
    if native {
        let owner = attached(tree).ok_or("this device's workspaces are still opening")?;
        if !owner.release_room(room)? {
            tree.write().forget_personal_room(room);
        }
        return Ok(None);
    }
    let mut held = tree.write();
    let index = held
        .archived_workspaces
        .iter()
        .position(|w| w.room() == room)
        .ok_or("no archived workspace is that room")?;
    let removed = held.archived_workspaces.remove(index);
    held.room_docs.remove(&room);
    held.queue_save();
    drop(held);
    Ok(Some(removed))
}

/// The owner already attached to `tree`'s writer, which an [`open`] of it
/// would answer, found without waiting for the writer's file.
pub(crate) fn attached(tree: &WorkspaceTreeHandle) -> Option<NativeWorkspaceOwner> {
    let runtime = tree.read().native_runtime.clone();
    runtime.owner_of(tree).ok().flatten()
}

/// Another open of `tree`'s writer, in progress on a thread of its own
/// until the returned sender is dropped (or `longest` passed): every other
/// open of the writer waits behind it, so the owner stays opening
/// meanwhile.
#[cfg(test)]
pub(crate) fn test_open_in_progress(
    tree: &WorkspaceTreeHandle,
    longest: std::time::Duration,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let runtime = tree.read().native_runtime.clone();
    let (held, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let opening = runtime.opening();
        let _ = held.send(());
        let _ = released.recv_timeout(longest);
        drop(opening);
    });
    tilo_test_env::recv("the other open to take the writer", &holding);
    (release, holder)
}

#[cfg(test)]
pub(crate) fn test_tree_of(owner: &NativeWorkspaceOwner) -> WorkspaceTreeHandle {
    tests::tree_of(owner)
}
