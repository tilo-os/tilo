//! This device's native owner (`tilo_peer_state::owner`'s), its record in a file
//! of its own beside the state file ([`owner_file`]). The tree keeps a
//! read-only projection of the record ([`WorkspaceTree::native_authority`])
//! and follows each publication ([`TreeHost`]): a room installed is
//! projected into its workspace, a room let go takes its workspace with it,
//! and the resulting file-system capabilities and action policy are
//! exported.
//!
//! [`WorkspaceTree::native_authority`]: super::state::WorkspaceTree::native_authority
#[cfg(test)]
mod frame_loop_tests;
mod local_filesystem_consents;
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
pub(crate) use tests::{grant_invite, test_enrolled_owner, test_owner_before_enrollment, tree_of};

use super::state::{RoomId, WorkspaceId, WorkspaceTreeHandle};
use std::path::{Path, PathBuf};
pub use store::NativeStateRuntime;
pub(crate) use store::NativeStateWitness;
pub use tilo_peer_state::owner::NativeAuthorityStorage;
use tilo_peer_state::owner::{Change, FileHost};
pub(crate) use tilo_peer_state::owner::{DocumentRead, NativeCommittedView, NativeWorkspaceOwner};
#[cfg(test)]
pub(crate) use tilo_peer_state::owner::{PendingEnrollmentId, PendingEnrollmentStatus};

#[cfg(test)]
thread_local! {
    /// Owner publications this thread made, so a test can weigh what each costs.
    static PUBLICATIONS_ON_THIS_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where the owner of the state file at `state` keeps its record.
pub(crate) fn owner_file(state: &Path) -> PathBuf {
    tilo_peer_state::owner::owner_file_of(state)
}

/// The tree following the owner's publications.
struct TreeHost {
    tree: WorkspaceTreeHandle,
}

impl FileHost for TreeHost {
    fn slot_room(&self, slot: WorkspaceId) -> Option<RoomId> {
        self.tree.read().room_of(slot)
    }

    /// Publish `next`, then apply `change` to the live tree.
    ///
    /// The tree is never held across the owner file's I/O. The action
    /// policy the publication leads to is taken before the write, from a
    /// copy of the tree with `change` applied: from the moment the record
    /// is installed until that export is published, the policy observers
    /// hold is the previous record's, and they must know its replacement
    /// is on the way rather than judge it withdrawn.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        #[cfg(test)]
        PUBLICATIONS_ON_THIS_THREAD.with(|count| count.set(count.get().saturating_add(1)));
        let change = &change;
        let rooms_change = matches!(change, Change::Room { .. } | Change::Released { .. });
        let export = {
            let mut tree = self.tree.write();
            // A completed enrollment installs into the workspace made for
            // the join, which is the room already: a workspace never changes
            // its room, so nothing is bound here.
            if let Change::Room {
                room,
                into: Some(target),
                ..
            } = *change
                && tree.room_of(target.workspace) != Some(room)
            {
                return Err("the workspace made for the join is gone".into());
            }
            let candidate = rooms_change.then(|| tree.clone_without_documents());
            drop(tree);
            candidate.map(|mut candidate| {
                apply(&mut candidate, next, change);
                crate::shell::action_authority::Export::of(&candidate)
            })
        };
        #[cfg(test)]
        {
            let runtime = self.tree.read().native_runtime.clone();
            store::test_seam::run(&runtime, write)?;
        }
        #[cfg(not(test))]
        write()?;
        let mut tree = self.tree.write();
        if apply(&mut tree, next, change) {
            // A room held with no workspace got one, or a workspace went
            // with its room: the tree document records it.
            tree.seed_doc_from_cache();
        }
        if rooms_change {
            // Only the final admitted projection reaches authority exports.
            tree.export_fs_capabilities();
            if let Some(export) = export {
                export.submit();
            }
        }
        // A save of the tree taken before this record reached it stood
        // down (`persist::TreeSnapshot`): this one carries the record. Its
        // policy and file-system exports are the ones taken above.
        if let Some(path) = tree.persist_path.clone() {
            super::persist::queue_save(&tree, &path, || {});
        }
        drop(tree);
        Ok(())
    }
}

/// Apply `next` and `change` to `tree`: the record, the room it now holds
/// projected into its workspace (a room held with no workspace gets one),
/// or a room let go with its workspace. Answers whether the set of
/// workspaces changed.
fn apply(
    tree: &mut super::state::WorkspaceTree,
    next: &NativeAuthorityStorage,
    change: &Change<'_>,
) -> bool {
    let before = tree.desktop.workspaces.len();
    tree.native_authority = next.clone();
    match *change {
        Change::Room {
            room, peer, host, ..
        } => tree.project_native_room(room, peer, host),
        // The room and its workspace go together.
        Change::Released { room } => {
            tree.forget_personal_room(room);
        }
        Change::Pending => {}
    }
    tree.desktop.workspaces.len() != before
}

/// The owner of the state file behind `tree`, its record in [`owner_file`].
///
/// The tree is never held across the owner file's I/O. Opens of one writer
/// are serialized by the writer itself.
///
/// # Errors
/// The state file's writer belongs to another tree or another process, the
/// owner file holds an invalid record, or a pending enrollment names no
/// workspace of its own.
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
    let path = {
        let native = tree.read();
        if !native.native_runtime.is(&runtime) {
            return Err("native state writer replaced while the owner was opening".into());
        }
        if let Some(owner) = runtime.owner_of(&tree)? {
            return Ok(owner);
        }
        native
            .persist_path
            .clone()
            .ok_or("native owner requires the actual state-file path")?
    };
    runtime.bind(&path).map_err(|e| e.to_string())?;
    let owner = tilo_peer_state::owner::open_file_with(
        &owner_file(&path),
        std::sync::Arc::new(TreeHost { tree: tree.clone() }),
    )?;
    let storage = owner.storage();
    if !storage.every_pending_has_slot() {
        return Err("native pending enrollment has no local slot".into());
    }
    let mut native = tree.write();
    if !native.native_runtime.is(&runtime) {
        return Err("native state writer replaced while the owner was opening".into());
    }
    native.native_authority = storage;
    for room in owner.rooms() {
        owner.with_current(room, |view| {
            native.project_native_room(room, view.peer(), view.room_host());
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
/// the two never part. A room is joined again through its host, never put
/// back here.
///
/// # Errors
///
/// The workspace is the last one, the native owner is not open yet, or the
/// record cannot be published.
pub(crate) fn release_workspace_room(
    tree: &WorkspaceTreeHandle,
    room: RoomId,
) -> Result<(), String> {
    let last = {
        let held = tree.read();
        let workspaces = &held.desktop.workspaces;
        let last = workspaces.len() < 2 && workspaces.iter().any(|w| w.room() == room);
        drop(held);
        last
    };
    if last {
        return Err("the last workspace stays".into());
    }
    let owner = attached(tree).ok_or("this device's workspaces are still opening")?;
    if !owner.release_room(room)? {
        // A join that never reached the host's record: only its workspace
        // is here.
        tree.write().forget_personal_room(room);
    }
    Ok(())
}

/// Delete the archived workspace that is `room`, with the room: a room the
/// native owner holds or is joining leaves its record in the same
/// publication ([`Change::Released`]).
///
/// # Errors
///
/// No archived workspace is that room, the native owner is not open yet,
/// or the record cannot be published.
pub(crate) fn delete_archived_workspace(
    tree: &WorkspaceTreeHandle,
    room: RoomId,
) -> Result<(), String> {
    if !tree
        .read()
        .archived_workspaces
        .iter()
        .any(|w| w.room() == room)
    {
        return Err("no archived workspace is that room".into());
    }
    let owner = attached(tree).ok_or("this device's workspaces are still opening")?;
    if !owner.release_room(room)? {
        tree.write().forget_personal_room(room);
    }
    Ok(())
}

/// The owner already attached to `tree`'s writer, which an [`open`] of it
/// would answer, found without waiting for the writer's file.
pub(crate) fn attached(tree: &WorkspaceTreeHandle) -> Option<NativeWorkspaceOwner> {
    let runtime = tree.read().native_runtime.clone();
    runtime.owner_of(tree).ok().flatten()
}

/// Whether `room`'s host is one that hosts rooms for others: a device that
/// acts as a host, named by the room's own transport hint. Such a host issues
/// an invitation for every member the room's policy lets invite, where another
/// person's own device signs its invitations alone.
pub(crate) fn host_hosts_for_others(view: &NativeCommittedView, room: RoomId) -> bool {
    let Ok(hint) = view.transport_hint() else {
        return false;
    };
    let Ok((hint, _)) =
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::authenticate_transport_hint(
            hint,
        )
    else {
        return false;
    };
    hint.room == room.to_string()
        && &hint.host_device_certificate == view.room_host()
        && view.room_host().capabilities.can_act_as_host
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

/// What keeps [`test_chrome_governing_its_room`]'s room alive: its
/// directory and its owner.
#[cfg(test)]
pub(crate) type GovernedRoomGuard = (tempfile::TempDir, NativeWorkspaceOwner);

/// A chrome acting as the person who governs the active room, a room this
/// device holds, with the room's invite action granted: where an
/// invitation can be issued. Keep the guard alive for the test's length.
#[cfg(test)]
pub(crate) fn test_chrome_governing_its_room()
-> (crate::shell::chrome_state::Chrome, GovernedRoomGuard) {
    use crate::shell::native_workspace_owner::grant_invite;
    let (directory, owner, room, _now, _key) = test_enrolled_owner();
    let governor = owner
        .with_current(room, |view| Ok(view.owner()))
        .ok()
        .and_then(crate::shell::state::person_id_of)
        .expect("the room's admitted governor");
    let handle = test_tree_of(&owner);
    {
        let mut tree = handle.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
            .expect("the enrolled room's workspace");
        grant_invite(workspace, &governor);
        let local = workspace.id;
        tree.desktop.active = local;
    }
    let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
        handle,
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    chrome.borrow_mut().act_as(governor);
    (chrome, (directory, owner))
}

#[cfg(test)]
pub(crate) fn test_tree_of(owner: &NativeWorkspaceOwner) -> WorkspaceTreeHandle {
    tests::tree_of(owner)
}
