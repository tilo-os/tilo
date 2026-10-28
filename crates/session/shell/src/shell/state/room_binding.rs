//! A workspace is its room, and every room this device holds is a workspace.
//!
//! The record of which rooms the device holds is the native owner's
//! ([`WorkspaceTree::native_authority`]: installed replicas and joins in
//! flight) beside the rooms of the shell's own engine, whose workspace is the
//! room. Before a workspace was its room, a join made a workspace in a local
//! placeholder room and re-pointed it once the host's replica installed; a
//! join that never installed left the workspace on its placeholder, and a
//! site set up again left the room it had before held with no workspace.
//! [`WorkspaceTree::bind_workspaces_to_rooms`] brings such a tree to the one
//! record when it is loaded. It never deletes data and never changes an id:
//!
//! - a workspace a join in flight installs into, still on its placeholder,
//!   becomes the workspace of the room it joins, under its own id, with its
//!   windows (their ids kept); the placeholder room is dropped only if it
//!   holds nothing, else it keeps a workspace of its own with what it holds;
//! - a room held with no workspace gets one.

use super::{
    AtomicOrdering, BTreeSet, FreshId as _, NEXT_ID, RoomAuthority, RoomId, Strip, StripId,
    Workspace, WorkspaceId, WorkspaceTree,
};

/// What [`WorkspaceTree::bind_workspaces_to_rooms`] changed, for the log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoomBinding {
    /// Workspaces that became the workspace of the room their join
    /// installs into: the workspace, the placeholder room it was on, and the
    /// room it is.
    pub joined: Vec<(WorkspaceId, RoomId, RoomId)>,
    pub kept: Vec<(RoomId, WorkspaceId)>,
    /// Placeholder rooms that held nothing, dropped.
    pub dropped: Vec<RoomId>,
    /// Rooms held with no workspace, each given one.
    pub made: Vec<(RoomId, WorkspaceId)>,
}

impl RoomBinding {
    /// Whether nothing changed.
    pub(crate) fn is_empty(&self) -> bool {
        self.joined.is_empty()
            && self.kept.is_empty()
            && self.dropped.is_empty()
            && self.made.is_empty()
    }
}

/// Whether the workspace `left`, what remains of a placeholder once its
/// windows moved to the room it joined, holds anything of its own.
fn holds_anything(left: &Workspace) -> bool {
    left.strips
        .iter()
        .any(|strip| !strip.window_groups.is_empty())
        || !left.effect_receipts.is_empty()
        || !left.shortcuts.is_empty()
        || !left.schedule_claims.is_empty()
        || !left.presets.is_empty()
        || !left.sandbox.is_empty()
        || left.default_apps != desktop_policy::WorkspaceDefaults::default()
        || left.pending_ownership_transfer.is_some()
        || !left.ownership_transfer_audit.is_empty()
        || !left.ownership_transfer_cancellations.is_empty()
        || !left.policy.members.is_empty()
        || !left.shared_surfaces.is_empty()
}

impl WorkspaceTree {
    /// Bring this tree, as it was loaded, to the one record of rooms and
    /// workspaces (the module's rules). `holds_files` says whether any file
    /// on this device belongs to a room (its apps' data for it): a
    /// placeholder room that has some is kept. Idempotent: a tree in the one
    /// record comes back unchanged.
    pub(crate) fn bind_workspaces_to_rooms(
        &mut self,
        holds_files: &dyn Fn(RoomId) -> bool,
    ) -> RoomBinding {
        let mut binding = RoomBinding::default();
        // Ids minted below must not collide with any stored one.
        self.seed_next_id_from_self();
        let joins: Vec<(RoomId, WorkspaceId)> = self
            .native_authority
            .joins()
            .filter(|(room, _)| !self.native_authority.holds(*room))
            .filter_map(|(room, slot)| Some((room, slot?)))
            .collect();
        for (room, slot) in joins {
            self.bind_join(room, slot, holds_files, &mut binding);
        }
        let held: Vec<(RoomId, WorkspaceId)> = self.native_authority.held().collect();
        for (room, recorded) in held {
            if self.has_workspace(room) {
                continue;
            }
            let id = if self.desktop.workspaces.iter().any(|w| w.id == recorded)
                || self.workspace_aliases.contains_key(&recorded)
            {
                self.new_workspace_id(room)
            } else {
                recorded
            };
            let mut workspace =
                Workspace::in_room(id, room, crate::shell::i18n::t("workspace-joined-name"));
            let strip = StripId::new();
            workspace.strips = vec![Strip {
                id: strip,
                name: "Main".into(),
                window_groups: Vec::new(),
            }];
            // Nobody showed this room here: it stays off the switcher,
            // archived, kept whole.
            self.archived_workspaces.push(workspace);
            binding.made.push((room, id));
        }
        if !binding.is_empty() {
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        }
        binding
    }

    /// The workspace `slot` a join into `room` installs into: made the
    /// workspace of `room` if it is still on a placeholder room.
    fn bind_join(
        &mut self,
        room: RoomId,
        slot: WorkspaceId,
        holds_files: &dyn Fn(RoomId) -> bool,
        binding: &mut RoomBinding,
    ) {
        let Some(index) = self.desktop.workspaces.iter().position(|w| w.id == slot) else {
            return;
        };
        self.joining_rooms.insert(room);
        let before = self.desktop.workspaces.remove(index);
        let placeholder = before.room();
        if placeholder == room {
            self.desktop.workspaces.insert(index, before);
            return;
        }
        let mut left = before;
        let strips = std::mem::take(&mut left.strips);
        if let Some(target) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.room() == room)
        {
            // The room has its workspace already: the slot's windows move
            // into it, and its id names that workspace from now on.
            target.strips.extend(strips);
            let target = target.id;
            self.workspace_aliases.insert(left.id, room);
            if self.desktop.active == left.id {
                self.desktop.active = target;
            }
            self.desktop.active_strip.remove(&left.id);
        } else {
            // The slot becomes the room's workspace, under its own id and in
            // its place, with its windows.
            let mut joined = Workspace::in_room(left.id, room, left.name.clone());
            joined.strips = strips;
            self.desktop.workspaces.insert(index, joined);
        }
        binding.joined.push((slot, placeholder, room));
        // The windows were opened before this device joined the room: none
        // of them is published into it by being here.
        self.room_authority
            .entry(room)
            .or_insert(RoomAuthority::Unpinned);
        self.room_share_floor
            .insert(room, NEXT_ID.load(AtomicOrdering::Relaxed));
        let records = self
            .desktop
            .room_records_held
            .get(&placeholder)
            .is_some_and(|records| !records.is_empty());
        if holds_anything(&left) || records || holds_files(placeholder) {
            // What the placeholder holds stays reachable, as a workspace of
            // its own: never deleted.
            let id = self.new_workspace_id(placeholder);
            let name = crate::shell::i18n::t_args(
                "workspace-kept-before-joining",
                &[("name", left.name.clone().into())],
            );
            let mut kept = Workspace::in_room(id, placeholder, name);
            kept.policy = left.policy.clone();
            kept.sandbox = std::mem::take(&mut left.sandbox);
            kept.presets = std::mem::take(&mut left.presets);
            kept.default_apps = left.default_apps.clone();
            kept.effect_receipts = std::mem::take(&mut left.effect_receipts);
            kept.shortcuts = std::mem::take(&mut left.shortcuts);
            kept.schedule_claims = std::mem::take(&mut left.schedule_claims);
            kept.pending_ownership_transfer = left.pending_ownership_transfer.take();
            kept.ownership_transfer_audit = std::mem::take(&mut left.ownership_transfer_audit);
            kept.ownership_transfer_cancellations =
                std::mem::take(&mut left.ownership_transfer_cancellations);
            kept.shared_surfaces = std::mem::take(&mut left.shared_surfaces);
            let strip = StripId::new();
            kept.strips = vec![Strip {
                id: strip,
                name: "Main".into(),
                window_groups: Vec::new(),
            }];
            // What the placeholder holds is kept, archived: off the
            // switcher, restorable or deletable from workspace settings.
            self.archived_workspaces.push(kept);
            binding.kept.push((placeholder, id));
        } else {
            self.drop_placeholder_room(placeholder);
            binding.dropped.push(placeholder);
        }
    }

    /// Forget `room`, a placeholder that held nothing and no workspace is
    /// any more: what the tree kept about it beside its workspace.
    fn drop_placeholder_room(&mut self, room: RoomId) {
        self.room_authority.remove(&room);
        self.room_peer_names.remove(&room);
        self.room_peer_identities.remove(&room);
        self.room_share_floor.remove(&room);
        self.room_docs.remove(&room);
        self.continuation_rooms.remove(&room);
        self.desktop.room_records_held.remove(&room);
    }

    /// The invariant of the one record, for tests and checks: every room the
    /// native owner holds or joins has exactly one workspace, every
    /// workspace routed to the native owner is a room it holds or joins, and
    /// no two workspaces are one room. Answers what breaks it, if anything.
    pub(crate) fn one_record_violations(&self) -> Vec<String> {
        let mut violations = Vec::new();
        let mut seen = BTreeSet::new();
        for workspace in self
            .desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
        {
            if !seen.insert(workspace.room()) {
                violations.push(format!("two workspaces are room {}", workspace.room()));
            }
            let room = workspace.room();
            if self.requires_native_owner(room)
                && !self.native_authority.requires(room)
                && !self.joining_rooms.contains(&room)
            {
                violations.push(format!("workspace {} has no room", workspace.id.raw()));
            }
        }
        for room in self.native_authority.rooms() {
            if !self.has_workspace(room) {
                violations.push(format!("room {room} has no workspace"));
            }
        }
        for room in &self.joining_rooms {
            if !self.has_workspace(*room) {
                violations.push(format!("joining room {room} has no workspace"));
            }
        }
        violations
    }
}

/// Whether any file under `data_home` belongs to `room`: an app's data for
/// the room (`tilo-rooms/<room>`) or any package's index of it
/// (`<package>/rooms/<room>`, below each directory of the data home; a plain
/// file there is no package). Anything not readable counts as holding files.
pub(crate) fn room_holds_files(data_home: &std::path::Path, room: RoomId) -> bool {
    let name = room.to_string();
    // Absent is only "no such entry" or "a file stands where a directory
    // would": every other answer may hide something.
    let present = |path: &std::path::Path| {
        !matches!(
            path.symlink_metadata(),
            Err(error) if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            )
        )
    };
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
        // A package's directory, or a link to one; a plain file holds no
        // room index.
        match std::fs::metadata(entry.path()) {
            Ok(metadata) if !metadata.is_dir() => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            _ => {}
        }
        if present(&entry.path().join(app_state::ROOMS_DIR).join(&name)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "room_binding_tests.rs"]
mod tests;
