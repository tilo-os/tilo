//! Empty workspaces that repeated joins of one site left behind.
//!
//! Before a join retried its retained proof ([`super::site_join`]), the
//! community thread joined its site again every ten minutes: each try made
//! a new empty workspace named for the site, and the host's door admitted
//! the person again, which the proof the first slot retains refused. The
//! slots stayed. This takes them off once the network runs, keeping the
//! slot the saved proof binds (or the workspace the join installed into)
//! and everything that holds anything.
//!
//! A workspace goes only when all of this is provable:
//!
//! - its site's join has a keeper here: the slot the room's saved proof
//!   binds, still unjoined, or the workspace holding the site's room;
//! - it carries the keeper's name and is not the keeper;
//! - it is exactly what a join's slot is when made: one strip named
//!   "Main" with no windows, nobody present, no members or admissions, an
//!   owner who is this computer's person, grants only to the room's roles
//!   from its owner role, and no sandbox grant, grant set, default app,
//!   task, receipt, shortcut, schedule claim, shared surface or ownership
//!   transfer;
//! - no other authority, peer name, peer identity, share floor, continuation
//!   tab, retained strip or native room or enrollment names its room;
//! - no saved proof binds it, and no file under the data home belongs to its
//!   room (checked by the network, [`room_holds_files`]).

use std::collections::BTreeSet;
use std::path::Path;

use tilo_action_policy::{Role, Subject};

use super::ShellNetwork;
use crate::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};

/// The strip a join's slot is made with ([`WorkspaceTree::append_empty_workspace`]).
const SLOT_STRIP: &str = "Main";

/// A site's join this computer holds or retains: the keeper the empty
/// duplicates of its slot are measured against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SiteAnchor {
    /// The site's room.
    pub site_room: RoomId,
    /// The slot its saved proof binds, when one does.
    pub slot: Option<WorkspaceId>,
}

/// The workspace `anchor` keeps: the slot its proof binds while that still
/// shows another room, else the workspace holding the site's room.
fn keeper<'a>(tree: &'a WorkspaceTree, anchor: &SiteAnchor) -> Option<&'a Workspace> {
    let workspaces = &tree.desktop.workspaces;
    anchor
        .slot
        .and_then(|slot| {
            workspaces
                .iter()
                .find(|workspace| workspace.id == slot && workspace.room != anchor.site_room)
        })
        .or_else(|| {
            workspaces
                .iter()
                .find(|workspace| workspace.room == anchor.site_room)
        })
}

/// Whether `workspace` is exactly a join's slot as made: one empty strip,
/// nobody in it, owned by `person` with only the room's role grants, and
/// nothing else of its own.
fn is_untouched_slot(workspace: &Workspace, person: &str) -> bool {
    let [strip] = workspace.strips.as_slice() else {
        return false;
    };
    let policy = &workspace.policy;
    strip.name == SLOT_STRIP
        && strip.window_groups.is_empty()
        && workspace.collaborators.is_empty()
        && workspace.pending_ownership_transfer.is_none()
        && workspace.ownership_transfer_audit.is_empty()
        && workspace.ownership_transfer_cancellations.is_empty()
        && policy.owner.as_deref() == Some(person)
        && policy.members.is_empty()
        && policy.joined.is_empty()
        && policy.grants.iter().all(|grant| {
            matches!(grant.subject, Subject::Role(_))
                && grant.granted_by == Subject::Role(Role::Owner)
        })
        && workspace.sandbox.is_empty()
        && workspace.presets.is_empty()
        && workspace.default_apps == desktop_policy::WorkspaceDefaults::default()
        && workspace.tasks.is_empty()
        && workspace.effect_receipts.is_empty()
        && workspace.shortcuts.is_empty()
        && workspace.schedule_claims.is_empty()
        && workspace.shared_surfaces.is_empty()
}

/// Whether anything the tree keeps beside its workspaces names `workspace`
/// or its room.
fn tree_names(tree: &WorkspaceTree, workspace: &Workspace) -> bool {
    let room = workspace.room;
    tree.native_authority.requires(room)
        || tree.room_authority.contains_key(&room)
        || tree.room_peer_names.contains_key(&room)
        || tree.room_peer_identities.contains_key(&room)
        || tree.room_share_floor.contains_key(&room)
        || tree.continuation_rooms.contains(&room)
        || tree
            .continuation_owned_tabs
            .values()
            .any(|held| *held == room)
        || tree
            .continuation_closed_tabs
            .values()
            .any(|held| *held == room)
        || tree
            .desktop
            .room_records_held
            .get(&room)
            .is_some_and(|records| !records.is_empty())
        || workspace
            .strips
            .iter()
            .any(|strip| tree.strips_to_retain.contains(&strip.id))
}

/// The empty duplicates of each anchored join's slot in `tree`, as
/// `(workspace, room)`: what the module's rule takes off, before the
/// network's own checks (saved proofs, files).
pub(super) fn duplicate_slots(
    tree: &WorkspaceTree,
    anchors: &[SiteAnchor],
    person: &str,
) -> Vec<(WorkspaceId, RoomId)> {
    let keepers: Vec<&Workspace> = anchors
        .iter()
        .filter_map(|anchor| keeper(tree, anchor))
        .collect();
    let site_rooms: BTreeSet<RoomId> = anchors.iter().map(|anchor| anchor.site_room).collect();
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| {
            !workspace.room.is_nil()
                && !site_rooms.contains(&workspace.room)
                && keepers.iter().all(|kept| kept.id != workspace.id)
                && keepers.iter().any(|kept| kept.name == workspace.name)
                && is_untouched_slot(workspace, person)
                && !tree_names(tree, workspace)
        })
        .map(|workspace| (workspace.id, workspace.room))
        .collect()
}

/// Take off the duplicates [`duplicate_slots`] finds in `tree` now that
/// `cleared` also passes (the network's checks of saved proofs and files),
/// through the same deletion the person's own uses. Answers the rooms taken
/// off.
pub(super) fn retire_duplicate_slots(
    tree: &mut WorkspaceTree,
    anchors: &[SiteAnchor],
    person: &str,
    cleared: impl Fn(WorkspaceId, RoomId) -> bool,
) -> Vec<RoomId> {
    let mut retired = Vec::new();
    for (workspace, room) in duplicate_slots(tree, anchors, person) {
        if cleared(workspace, room) && tree.delete_workspace(workspace).is_some() {
            retired.push(room);
        }
    }
    retired
}

/// Whether any file under `data_home` belongs to `room`: an app's data for
/// the room (`tilo-rooms/<room>`) or any package's index of it
/// (`<package>/rooms/<room>`). Anything not readable counts as holding
/// files.
pub(super) fn room_holds_files(data_home: &Path, room: RoomId) -> bool {
    let name = room.to_string();
    let present = |path: &Path| !matches!(path.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    if present(
        &data_home
            .join(launcher::appdir::ROOMS_DIR)
            .join(launcher::appdir::room_slug(Some(&name))),
    ) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(data_home) else {
        return true;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return true;
        };
        if present(&entry.path().join(app_state::ROOMS_DIR).join(&name)) {
            return true;
        }
    }
    false
}

impl ShellNetwork {
    /// Take off the empty workspaces repeated joins of a site left beside
    /// the one its saved proof binds (the module's rule). Opens the native
    /// owner and reads the disk: call it off the frame loop. A computer
    /// whose saved proofs or data home cannot be read keeps everything.
    pub(super) fn retire_duplicate_site_slots(&self) {
        let sites: Vec<RoomId> = self
            .hosted_sites
            .rooms()
            .into_iter()
            .filter(|room| {
                self.hosted_sites
                    .hosted(*room)
                    .is_some_and(|hosted| hosted.site.is_some())
            })
            .collect();
        if sites.is_empty() {
            return;
        }
        let Ok(owner) = self.native_owner() else {
            return;
        };
        let mut anchors = Vec::with_capacity(sites.len());
        for site_room in sites {
            let Ok(pending) = owner.pending_enrollment(site_room) else {
                return;
            };
            anchors.push(SiteAnchor {
                site_room,
                slot: pending.and_then(|pending| pending.target_workspace()),
            });
        }
        let Some(data_home) = launcher::appdir::host_data_home() else {
            return;
        };
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let tree = replica.tree.clone();
        drop(replica);
        let person = crate::shell::state::local_user_id();
        let found = duplicate_slots(&tree.read(), &anchors, &person);
        if found.is_empty() {
            return;
        }
        // Proofs and files are read with the tree released; the deletion
        // runs the rule again under the write lock and takes off only what
        // both passes found and these checks cleared.
        let cleared: BTreeSet<(WorkspaceId, RoomId)> = found
            .into_iter()
            .filter(|(workspace, room)| {
                matches!(owner.pending_enrollment_for_slot(*workspace), Ok(None))
                    && !room_holds_files(&data_home, *room)
            })
            .collect();
        if cleared.is_empty() {
            return;
        }
        let retired =
            retire_duplicate_slots(&mut tree.write(), &anchors, &person, |workspace, room| {
                cleared.contains(&(workspace, room))
            });
        if retired.is_empty() {
            return;
        }
        tracing::info!(
            count = retired.len(),
            rooms = ?retired,
            "took off empty workspaces that repeated joins of a site left"
        );
        self.export_personal_rooms();
    }
}

#[cfg(test)]
#[path = "site_slots_tests.rs"]
mod tests;
