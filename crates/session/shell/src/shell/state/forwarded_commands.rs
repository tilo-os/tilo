//! Rearranging windows another of this person's devices opened.
//!
//! In a peer room every strip, group and tab has one writer: the device whose
//! salt minted it. A change this device made to such an entry would only live
//! in its own cache and be reverted by the next merge, so a gesture on a
//! window one of the person's other devices owns is handed to that device as
//! the exact [`DesktopCommand`] instead. The owner applies it as if the person
//! had made the gesture there, and its projection brings the result back.
//!
//! The layout is shared state, so the gesture takes effect here at once,
//! whether or not the owning device is reachable: the command is kept (in the
//! state file, across restarts) and shown on top of what the room says after
//! every merge, until the room shows it applied, the owner refuses it, or it
//! no longer means anything. The owning device receives it the next time it
//! is live in the room, and both devices converge on its result.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use tilo_desktop_model::commands::{
    DesktopCommand, FRONT, TabLocation, TabPlacement, apply_command, relocated_entries,
};
use tilo_desktop_model::{DropTarget, peer_salt_of};

use super::{AtomicOrdering, GroupId, NEXT_ID, RoomId, StripId, TabId, WorkspaceTree};

/// A gesture on entries another device of this person owns: shown here, and
/// handed to that device until its layout reflects it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedCommand {
    /// The invocation every delivery of this command carries, so an owner
    /// that already applied it does not apply it twice.
    pub id: uuid::Uuid,
    pub room: RoomId,
    /// The topology namespace of the device that must apply it.
    pub salt: u16,
    pub command: DesktopCommand,
    /// Ids this device minted to show the command (the window group a move
    /// or split creates). Reused every time it is shown again, and never
    /// published: the owner mints its own.
    #[serde(default)]
    pub minted: Vec<u64>,
    /// The owner answered that it applied the command; it stays shown until
    /// the owner's layout arrives.
    #[serde(default)]
    pub applied: bool,
}

/// What became of a gesture on a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Not this device's to apply: handed to the device that owns it.
    Forwarded,
    /// This device applies it itself.
    Local,
}

impl WorkspaceTree {
    /// Tell the tree which topology namespaces belong to this person's other
    /// devices. Gestures on their entries are forwarded rather than applied.
    pub fn set_personal_device_salts(&mut self, salts: BTreeSet<u16>) {
        self.personal_device_salts = salts;
    }

    /// The gestures still to be handed to their owning devices, oldest
    /// first. They stay queued until the owner answers.
    pub fn undelivered_forwarded_commands(&self) -> Vec<ForwardedCommand> {
        self.forwarded_commands
            .iter()
            .filter(|forwarded| !forwarded.applied)
            .cloned()
            .collect()
    }

    /// The owner applied gesture `id`: stop handing it over, and keep showing
    /// it until the owner's layout arrives.
    pub fn forwarded_command_applied(&mut self, id: uuid::Uuid) {
        if let Some(forwarded) = self.forwarded_commands.iter_mut().find(|f| f.id == id)
            && !forwarded.applied
        {
            forwarded.applied = true;
            self.queue_save();
        }
    }

    /// The owner refused gesture `id`: forget it, and show the room as it
    /// is. Returns whether the layout here changed.
    pub fn forwarded_command_refused(&mut self, id: uuid::Uuid) -> bool {
        let Some(index) = self.forwarded_commands.iter().position(|f| f.id == id) else {
            return false;
        };
        let room = self.forwarded_commands.get(index).map(|f| f.room);
        let Some(room) = room else {
            return false;
        };
        // Its shown group goes with it; the others are shown again on top
        // of the room by the merge.
        self.withdraw_forwarded(room);
        self.forwarded_commands.remove(index);
        let changed = self.merge_room_contents(room);
        if changed {
            self.seed_doc_from_cache();
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        }
        self.queue_save();
        changed
    }

    /// Groups minted only to show gestures still waiting on their owners in
    /// `room`. Never published.
    pub(super) fn forwarded_groups(&self, room: RoomId) -> BTreeSet<u64> {
        self.forwarded_commands
            .iter()
            .filter(|forwarded| forwarded.room == room)
            .flat_map(|forwarded| forwarded.minted.iter().copied())
            .collect()
    }

    /// Take the groups minted to show gestures out of `room`'s layout before
    /// the room document is folded in, so the room's own placement of their
    /// windows is not duplicated. A group this device has since put one of
    /// its own windows into is its own group now and stays.
    pub(super) fn withdraw_forwarded(&mut self, room: RoomId) {
        let shown = self.forwarded_groups(room);
        if shown.is_empty() {
            return;
        }
        let local = self.local_salt();
        let mut kept = BTreeSet::new();
        if let Some(workspace) = self.desktop.workspaces.iter_mut().find(|w| w.room == room) {
            for strip in &mut workspace.strips {
                strip.window_groups.retain(|group| {
                    if !shown.contains(&group.id) {
                        return true;
                    }
                    let own = group.tabs.iter().any(|tab| peer_salt_of(tab.id) == local);
                    if own {
                        kept.insert(group.id);
                    }
                    own
                });
            }
        }
        for forwarded in &mut self.forwarded_commands {
            forwarded.minted.retain(|id| !kept.contains(id));
        }
    }

    /// Show every gesture still waiting on its owner in `room` on top of
    /// what the room says, oldest first. A gesture that no longer changes
    /// anything (the owner's layout already shows it) or no longer applies
    /// (its window moved or closed meanwhile) is done and forgotten.
    pub(super) fn show_forwarded(&mut self, room: RoomId) {
        let Some(index) = self.desktop.workspaces.iter().position(|w| w.room == room) else {
            return;
        };
        let salt = self.local_salt();
        let mut keep = Vec::with_capacity(self.forwarded_commands.len());
        let mut forgot = false;
        for mut forwarded in std::mem::take(&mut self.forwarded_commands) {
            if forwarded.room != room {
                keep.push(forwarded);
                continue;
            }
            let Some(current) = self.desktop.workspaces.get(index) else {
                break;
            };
            let mut workspace = current.clone();
            let mut reuse = forwarded.minted.clone().into_iter();
            let mut minted = Vec::new();
            let applied = apply_command(
                &mut workspace,
                &forwarded.command,
                &mut || {
                    let id = reuse.next().unwrap_or_else(|| {
                        tilo_desktop_model::compose_id(
                            salt,
                            NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed),
                        )
                    });
                    minted.push(id);
                    Ok(id)
                },
                &mut |_, _| Err("a rearrangement creates no record".into()),
            );
            if applied.is_err() || shape(&workspace) == shape(current) {
                forgot = true;
                continue;
            }
            forwarded.minted = minted;
            if let Some(slot) = self.desktop.workspaces.get_mut(index) {
                *slot = workspace;
            }
            keep.push(forwarded);
        }
        self.forwarded_commands = keep;
        if forgot {
            self.queue_save();
        }
    }

    /// The namespace of the device of this person that must apply `command`
    /// in `room`, when that is not this device: every entry it relocates
    /// carries that one other device's salt. `None` when this device applies
    /// it (its own entries, a governed room, or entries of somebody else's
    /// device, which stay where their writer put them).
    pub fn personal_owner_of(&self, room: RoomId, command: &DesktopCommand) -> Option<u16> {
        let local = self.local_salt();
        if local == 0 || self.personal_device_salts.is_empty() || self.requires_native_owner(room) {
            return None;
        }
        let workspace = self.desktop.workspaces.iter().find(|w| w.room == room)?;
        let salts: BTreeSet<u16> = relocated_entries(workspace, command)
            .into_iter()
            .map(peer_salt_of)
            .collect();
        let mut salts = salts.into_iter();
        let salt = salts.next()?;
        (salts.next().is_none() && salt != local && self.personal_device_salts.contains(&salt))
            .then_some(salt)
    }

    /// Queue `command` for its owning device when another device of this
    /// person owns what it relocates, and show it here at once;
    /// [`Gesture::Local`] otherwise. A close is queued but not shown: the
    /// caller closes the window here itself, client and all.
    pub fn forward_if_personal(&mut self, room: RoomId, command: DesktopCommand) -> Gesture {
        let Some(salt) = self.personal_owner_of(room, &command) else {
            return Gesture::Local;
        };
        let close = matches!(command, DesktopCommand::CloseTab { .. });
        self.forwarded_commands.push(ForwardedCommand {
            id: uuid::Uuid::new_v4(),
            room,
            salt,
            command,
            minted: Vec::new(),
            applied: false,
        });
        if !close {
            self.show_forwarded(room);
            self.seed_doc_from_cache();
            self.external_revision_bump();
        }
        self.queue_save();
        Gesture::Forwarded
    }

    fn external_revision_bump(&mut self) {
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
    }

    /// [`Self::forward_if_personal`] in whichever room holds what `command`
    /// relocates.
    pub fn forward_command(&mut self, command: &DesktopCommand) -> Gesture {
        let room = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| !relocated_entries(workspace, command).is_empty())
            .map(|workspace| workspace.room);
        match room {
            Some(room) => self.forward_if_personal(room, command.clone()),
            None => Gesture::Local,
        }
    }

    /// The room `strip` belongs to.
    fn room_of_strip(&self, strip: StripId) -> Option<RoomId> {
        let (workspace, _) = self.desktop.strip_position(strip)?;
        self.desktop.workspaces.get(workspace).map(|w| w.room)
    }

    /// Where `tab` is: its strip and group.
    fn tab_location(&self, tab: TabId) -> Option<TabLocation> {
        self.desktop.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip.window_groups.iter().find_map(|group| {
                    group
                        .tabs
                        .iter()
                        .any(|t| t.id == tab)
                        .then_some(TabLocation {
                            strip: strip.id.0,
                            group: group.id,
                            tab,
                        })
                })
            })
        })
    }

    /// The command a cross-strip drop of `tab` onto `target` in `dest`
    /// makes, when another device of this person must apply it; queued.
    /// [`Gesture::Local`] for this device's own windows. A window of another
    /// of this person's devices cannot leave its room this way (its owner
    /// would have to carry it), so that is refused as `None`.
    pub(super) fn forward_tab_drop(
        &mut self,
        src: StripId,
        dest: StripId,
        tab: TabId,
        target: DropTarget,
    ) -> Option<Gesture> {
        let Some(location) = self.tab_location(tab) else {
            return Some(Gesture::Local);
        };
        let (Some(room), Some(dest_room)) = (self.room_of_strip(src), self.room_of_strip(dest))
        else {
            return Some(Gesture::Local);
        };
        let groups: Vec<(GroupId, Vec<TabId>)> = self
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|strip| strip.id == dest)
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                    .collect()
            })
            .unwrap_or_default();
        let placement = match target {
            DropTarget::ExistingGroup {
                group_id,
                tab_index,
            } => {
                let tabs = groups
                    .iter()
                    .find(|(id, _)| *id == group_id)
                    .map(|(_, tabs)| tabs.as_slice())
                    .unwrap_or_default();
                TabPlacement::InGroup {
                    strip: dest.0,
                    group: group_id,
                    after: anchor_before(tabs, tab_index),
                }
            }
            DropTarget::GroupGap { gap_index } => {
                let ids: Vec<GroupId> = groups.iter().map(|(id, _)| *id).collect();
                TabPlacement::NewGroup {
                    strip: dest.0,
                    after: anchor_before(&ids, gap_index),
                }
            }
        };
        let command = DesktopCommand::MoveTab {
            location,
            placement,
        };
        if room != dest_room {
            return self
                .personal_owner_of(room, &command)
                .is_none()
                .then_some(Gesture::Local);
        }
        Some(self.forward_if_personal(room, command))
    }

    /// [`Self::forward_if_personal`] for splitting `tab` into a new group at
    /// `insert_at` in its own strip.
    pub(super) fn forward_split(
        &mut self,
        strip: StripId,
        tab: TabId,
        insert_at: usize,
    ) -> Gesture {
        let (Some(location), Some(room)) = (self.tab_location(tab), self.room_of_strip(strip))
        else {
            return Gesture::Local;
        };
        // The split's index counts groups as they stand; `before` names the
        // group that will follow the new one.
        let before = self
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|candidate| candidate.id == strip)
            .and_then(|candidate| candidate.window_groups.get(insert_at))
            .map(|group| group.id);
        self.forward_if_personal(room, DesktopCommand::SplitTab { location, before })
    }

    /// The room holding `tab`.
    pub fn room_of_tab(&self, tab: TabId) -> Option<RoomId> {
        self.room_of_strip(StripId(self.tab_location(tab)?.strip))
    }

    /// The namespace of the other device of this person that closes `tab`,
    /// when another one does.
    pub fn personal_owner_of_close(&self, room: RoomId, tab: TabId) -> Option<u16> {
        let location = self.tab_location(tab)?;
        self.personal_owner_of(room, &DesktopCommand::CloseTab { location })
    }

    /// [`Self::forward_if_personal`] for closing `tab`.
    pub fn forward_close(&mut self, tab: TabId) -> Gesture {
        let Some(location) = self.tab_location(tab) else {
            return Gesture::Local;
        };
        let Some(room) = self.room_of_strip(StripId(location.strip)) else {
            return Gesture::Local;
        };
        self.forward_if_personal(room, DesktopCommand::CloseTab { location })
    }

    /// Refuse a command handed over by another device unless everything it
    /// relocates in `room` is this device's to write.
    ///
    /// # Errors
    ///
    /// A description of why this device may not apply it.
    pub fn check_handed_over(&self, room: RoomId, command: &DesktopCommand) -> Result<(), String> {
        if self.requires_native_owner(room) {
            return Err("this workspace's windows are placed by its host".into());
        }
        let workspace = self
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room == room)
            .ok_or("this device is not in that workspace")?;
        let entries = relocated_entries(workspace, command);
        if entries.is_empty() {
            return Err("that window is no longer here".into());
        }
        let local = self.local_salt();
        if entries.iter().any(|id| peer_salt_of(*id) != local) {
            return Err("that window belongs to another device".into());
        }
        Ok(())
    }

    /// Apply a rearrangement another device of this person handed over, as
    /// if the gesture were made here: the cache, the desktop document and
    /// the restart snapshot change together. Closing goes through the
    /// chrome's close instead, which also ends the window's client.
    ///
    /// # Errors
    ///
    /// Why the command was refused; nothing changed then.
    pub fn apply_handed_over(
        &mut self,
        room: RoomId,
        command: &DesktopCommand,
    ) -> Result<bool, String> {
        self.check_handed_over(room, command)?;
        match command {
            DesktopCommand::ReorderTab { .. }
            | DesktopCommand::ReorderGroup { .. }
            | DesktopCommand::MoveGroup { .. } => self.commit_command(command),
            DesktopCommand::MoveTab { .. } | DesktopCommand::SplitTab { .. } => {
                let salt = self.local_salt();
                let index = self
                    .desktop
                    .workspaces
                    .iter()
                    .position(|w| w.room == room)
                    .ok_or("this device is not in that workspace")?;
                let mut workspace = self
                    .desktop
                    .workspaces
                    .get(index)
                    .cloned()
                    .ok_or("this device is not in that workspace")?;
                apply_command(
                    &mut workspace,
                    command,
                    &mut || {
                        Ok(tilo_desktop_model::compose_id(
                            salt,
                            NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed),
                        ))
                    },
                    &mut |_, _| Err("a move creates no record".into()),
                )?;
                if let Some(slot) = self.desktop.workspaces.get_mut(index) {
                    *slot = workspace;
                }
                self.seed_doc_from_cache();
                self.queue_save();
                Ok(true)
            }
            _ => Err("only window rearrangements are handed between devices".into()),
        }
    }
}

/// Which windows sit together where, ignoring the ids of the groups that hold
/// them: two layouts with the same shape show the same thing.
fn shape(workspace: &crate::shell::state::Workspace) -> Vec<Vec<Vec<TabId>>> {
    workspace
        .strips
        .iter()
        .map(|strip| {
            strip
                .window_groups
                .iter()
                .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
                .collect()
        })
        .collect()
}

/// The `after` anchor that lands an item at `index` among `ids`.
fn anchor_before(ids: &[u64], index: usize) -> Option<u64> {
    if index == 0 && !ids.is_empty() {
        return Some(FRONT);
    }
    index.checked_sub(1).and_then(|at| ids.get(at)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        AppKind, PEER_ID_SALT_SHIFT, PeerOrigin, Strip, Tab, TileContent, WindowGroup, WindowWidth,
    };

    const LAPTOP: u16 = 0xA1A1;
    const PI: u16 = 0xB2B2;

    /// An id in `salt`'s namespace, minted after any device joined the
    /// room (a joined device publishes only what it minted since).
    fn salted(salt: u16, counter: u64) -> u64 {
        (u64::from(salt) << PEER_ID_SALT_SHIFT) | (1 << 40) | counter
    }

    fn tab(id: u64, title: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("so.tilo.notes".into()),
                label: title.into(),
            },
        }
    }

    fn strip(salt: u16, tabs: &[(u64, &str)]) -> Strip {
        let tabs: Vec<Tab> = tabs.iter().map(|(id, title)| tab(*id, title)).collect();
        Strip {
            id: StripId::from(salted(salt, 1)),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: salted(salt, 2),
                active_tab: tabs[0].id,
                tabs,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }
    }

    fn ship(from: &mut WorkspaceTree, to: &mut WorkspaceTree, room: RoomId) {
        let changes = from
            .export_room_changes_since(room, &[])
            .expect("in the room");
        to.apply_room_changes(room, &changes, &PeerOrigin::hub(1))
            .expect("merges");
    }

    fn workspace_mut(
        tree: &mut WorkspaceTree,
        room: RoomId,
    ) -> &mut crate::shell::state::Workspace {
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|w| w.room == room)
            .expect("in the room")
    }

    fn group_tabs(tree: &WorkspaceTree, group: GroupId) -> Vec<u64> {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
            .find(|g| g.id == group)
            .map(|g| g.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default()
    }

    /// A laptop and a Pi of one person in one workspace: the laptop's strip
    /// holds two of its windows, the Pi's strip one of its own.
    fn laptop_and_pi() -> (WorkspaceTree, WorkspaceTree, RoomId) {
        let user_key = [0x71; 32];
        let person = super::super::person_id_of(user_key).expect("a real person principal");
        let mut laptop = WorkspaceTree::bootstrap_demo();
        laptop.local_person = Some(user_key);
        laptop
            .authenticated_persons
            .insert(PeerOrigin::hub(1), user_key);
        let room = laptop.desktop.workspaces[0].room;
        laptop.adopt_ownerless_workspaces(&person);
        let mut pi = WorkspaceTree::bootstrap_demo();
        pi.local_person = Some(user_key);
        pi.authenticated_persons
            .insert(PeerOrigin::hub(1), user_key);
        let pi_ws = pi.desktop.workspaces[0].id;
        assert!(pi.set_room(pi_ws, room));
        assert!(pi.adopt_personal_room(room, "Both devices", &person, None));
        laptop.set_salt_override(LAPTOP);
        pi.set_salt_override(PI);
        workspace_mut(&mut laptop, room).strips = vec![strip(
            LAPTOP,
            &[(salted(LAPTOP, 3), "Plan"), (salted(LAPTOP, 4), "Notes")],
        )];
        workspace_mut(&mut pi, room).strips = vec![strip(PI, &[(salted(PI, 3), "Shopping")])];
        laptop.seed_doc_from_cache();
        pi.seed_doc_from_cache();
        ship(&mut laptop, &mut pi, room);
        ship(&mut pi, &mut laptop, room);
        pi.set_personal_device_salts(BTreeSet::from([LAPTOP]));
        laptop.set_personal_device_salts(BTreeSet::from([PI]));
        (laptop, pi, room)
    }

    fn published_groups(tree: &mut WorkspaceTree, room: RoomId) -> Vec<u64> {
        let changes = tree
            .export_room_changes_since(room, &[])
            .expect("in the room");
        let doc = tilo_peer_state::room::candidate(None, &changes).expect("a room document");
        tilo_desktop_model::projection::contents_of(&doc)
            .into_iter()
            .flat_map(|strip| strip.groups)
            .map(|group| group.id)
            .collect()
    }

    #[test]
    fn a_window_dragged_on_one_device_moves_on_the_device_that_opened_it() {
        let (mut laptop, mut pi, room) = laptop_and_pi();
        let notes = salted(LAPTOP, 4);
        let pi_group = salted(PI, 2);

        // On the Pi, drag the laptop's Notes to the front of the Pi's group.
        // It moves here at once, whether or not the laptop is reachable.
        assert!(pi.move_tab_across_strips(
            StripId::from(salted(LAPTOP, 1)),
            StripId::from(salted(PI, 1)),
            notes,
            salted(PI, 9),
            DropTarget::ExistingGroup {
                group_id: pi_group,
                tab_index: 0,
            },
        ));
        assert_eq!(group_tabs(&pi, pi_group), [notes, salted(PI, 3)]);
        let handed = pi.undelivered_forwarded_commands();
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].salt, LAPTOP);
        assert_eq!(
            handed[0].command,
            DesktopCommand::MoveTab {
                location: TabLocation {
                    strip: salted(LAPTOP, 1),
                    group: salted(LAPTOP, 2),
                    tab: notes,
                },
                placement: TabPlacement::InGroup {
                    strip: salted(PI, 1),
                    group: pi_group,
                    after: Some(FRONT),
                },
            }
        );

        // The laptop, not having it yet, keeps sending its own layout: the
        // gesture stays on the Pi across those merges.
        for _ in 0..2 {
            ship(&mut laptop, &mut pi, room);
            ship(&mut pi, &mut laptop, room);
            assert_eq!(group_tabs(&pi, pi_group), [notes, salted(PI, 3)]);
            assert_eq!(group_tabs(&laptop, pi_group), [salted(PI, 3)]);
        }

        // The laptop gets it (later, when it is back) and applies it as if
        // the person had dragged it there.
        assert_eq!(laptop.apply_handed_over(room, &handed[0].command), Ok(true));
        pi.forwarded_command_applied(handed[0].id);
        assert!(pi.undelivered_forwarded_commands().is_empty());
        assert_eq!(group_tabs(&laptop, pi_group), [notes, salted(PI, 3)]);

        // The laptop's layout brings it to the Pi in the same place, and the
        // Pi forgets the gesture: the room now says it.
        ship(&mut laptop, &mut pi, room);
        assert_eq!(group_tabs(&pi, pi_group), [notes, salted(PI, 3)]);
        assert_eq!(group_tabs(&pi, salted(LAPTOP, 2)), [salted(LAPTOP, 3)]);
        assert!(pi.forwarded_commands.is_empty());
        ship(&mut pi, &mut laptop, room);
        ship(&mut laptop, &mut pi, room);
        assert_eq!(group_tabs(&pi, pi_group), [notes, salted(PI, 3)]);
        assert_eq!(group_tabs(&laptop, pi_group), [notes, salted(PI, 3)]);
    }

    #[test]
    fn a_split_shows_at_once_under_a_group_nobody_else_sees() {
        let (mut laptop, mut pi, room) = laptop_and_pi();
        let laptop_strip = StripId::from(salted(LAPTOP, 1));
        let plan = salted(LAPTOP, 3);

        let shown = pi
            .split_tab_into_new_group(laptop_strip, plan, 1)
            .expect("the split shows here");
        assert_eq!(group_tabs(&pi, shown), [plan]);
        assert!(
            !published_groups(&mut pi, room).contains(&shown),
            "the group minted to show it is never published"
        );
        ship(&mut laptop, &mut pi, room);
        assert_eq!(
            group_tabs(&pi, shown),
            [plan],
            "and survives the laptop's layout"
        );

        let handed = pi.undelivered_forwarded_commands();
        assert!(matches!(
            handed.as_slice(),
            [ForwardedCommand {
                salt: LAPTOP,
                command: DesktopCommand::SplitTab { .. },
                ..
            }]
        ));
        assert_eq!(laptop.apply_handed_over(room, &handed[0].command), Ok(true));
        let groups: Vec<GroupId> = laptop
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|s| s.id == laptop_strip)
            .map(|s| s.window_groups.iter().map(|g| g.id).collect())
            .unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| peer_salt_of(*g) == LAPTOP));
        ship(&mut laptop, &mut pi, room);
        assert_eq!(group_tabs(&pi, groups[1]), [plan]);
        assert!(
            group_tabs(&pi, shown).is_empty(),
            "the shown group gives way"
        );
        assert!(pi.forwarded_commands.is_empty());

        // A reorder of the laptop's group shows here too.
        let reorder = DesktopCommand::ReorderGroup {
            strip: laptop_strip.0,
            group: groups[1],
            before: Some(groups[0]),
        };
        assert_eq!(pi.commit_command(&reorder), Ok(true));
        assert_eq!(pi.undelivered_forwarded_commands()[0].command, reorder);
        let order: Vec<GroupId> = pi
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|s| s.id == laptop_strip)
            .map(|s| s.window_groups.iter().map(|g| g.id).collect())
            .unwrap();
        assert_eq!(order, [groups[1], groups[0]]);

        // The Pi's own window is the Pi's: nothing is handed over.
        assert_eq!(pi.forward_close(salted(PI, 3)), Gesture::Local);
        assert_eq!(pi.undelivered_forwarded_commands().len(), 1);

        // A command naming the Pi's window is not the laptop's to apply.
        let foreign = DesktopCommand::CloseTab {
            location: TabLocation {
                strip: salted(PI, 1),
                group: salted(PI, 2),
                tab: salted(PI, 3),
            },
        };
        assert!(laptop.check_handed_over(room, &foreign).is_err());
        assert!(laptop.apply_handed_over(room, &foreign).is_err());
    }

    #[test]
    fn a_window_closed_while_its_device_is_away_stays_closed_until_it_closes_there() {
        let (mut laptop, mut pi, room) = laptop_and_pi();
        let plan = salted(LAPTOP, 3);

        assert_eq!(pi.forward_close(plan), Gesture::Forwarded);
        assert!(crate::shell::tab_binding::remove_tab(&mut pi, plan));
        let close = pi.undelivered_forwarded_commands().remove(0);
        assert!(matches!(close.command, DesktopCommand::CloseTab { .. }));

        // The laptop is away and its layout still has the window.
        ship(&mut laptop, &mut pi, room);
        assert_eq!(group_tabs(&pi, salted(LAPTOP, 2)), [salted(LAPTOP, 4)]);

        // Back, it closes it; the Pi forgets the gesture.
        assert_eq!(laptop.check_handed_over(room, &close.command), Ok(()));
        assert!(crate::shell::tab_binding::remove_tab(&mut laptop, plan));
        laptop.seed_doc_from_cache();
        ship(&mut laptop, &mut pi, room);
        assert_eq!(group_tabs(&pi, salted(LAPTOP, 2)), [salted(LAPTOP, 4)]);
        assert!(pi.forwarded_commands.is_empty());
    }

    #[test]
    fn a_refused_gesture_gives_way_to_the_room() {
        let (_laptop, mut pi, _room) = laptop_and_pi();
        let laptop_strip = StripId::from(salted(LAPTOP, 1));
        let plan = salted(LAPTOP, 3);
        let shown = pi
            .split_tab_into_new_group(laptop_strip, plan, 1)
            .expect("shown");
        let id = pi.undelivered_forwarded_commands()[0].id;
        assert!(pi.forwarded_command_refused(id));
        assert!(pi.forwarded_commands.is_empty());
        assert!(group_tabs(&pi, shown).is_empty());
        assert_eq!(
            group_tabs(&pi, salted(LAPTOP, 2)),
            [salted(LAPTOP, 3), salted(LAPTOP, 4)]
        );
    }

    #[test]
    fn waiting_gestures_survive_a_restart() {
        let (_laptop, mut pi, _room) = laptop_and_pi();
        assert_eq!(pi.forward_close(salted(LAPTOP, 3)), Gesture::Forwarded);
        let json = serde_json::to_value(&pi.forwarded_commands).expect("encodes");
        let back: Vec<ForwardedCommand> = serde_json::from_value(json).expect("decodes");
        assert_eq!(back, pi.forwarded_commands);
    }

    #[test]
    fn another_persons_windows_and_unknown_devices_are_not_forwarded() {
        let (_laptop, mut pi, room) = laptop_and_pi();
        // The laptop is not known as this person's device: its windows
        // behave as any collaborator's, applied (and settled) locally.
        pi.set_personal_device_salts(BTreeSet::new());
        assert_eq!(pi.forward_close(salted(LAPTOP, 3)), Gesture::Local);
        pi.set_personal_device_salts(BTreeSet::from([0x7777]));
        assert_eq!(pi.forward_close(salted(LAPTOP, 3)), Gesture::Local);
        assert!(pi.forwarded_commands.is_empty());
        // A window of this person's device cannot be carried out of its room
        // by another device.
        pi.set_personal_device_salts(BTreeSet::from([LAPTOP]));
        let other = pi.append_empty_workspace("Elsewhere");
        let other_strip = pi
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == other)
            .and_then(|w| w.strips.first())
            .map(|s| s.id)
            .unwrap();
        assert!(!pi.move_tab_across_strips(
            StripId::from(salted(LAPTOP, 1)),
            other_strip,
            salted(LAPTOP, 3),
            salted(PI, 10),
            DropTarget::GroupGap { gap_index: 0 },
        ));
        assert!(pi.forwarded_commands.is_empty());
        assert_eq!(
            pi.room_of_strip(StripId::from(salted(LAPTOP, 1))),
            Some(room)
        );
    }
}
