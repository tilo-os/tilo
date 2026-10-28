//! Device-local launcher record associations. These are restart metadata,
//! never room membership, permissions, or a replicated document projection.

use super::{DesktopTree, RoomId, TabId, TileContent, WorkspaceTree};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAppRecord {
    pub room: RoomId,
    pub app_id: String,
    pub record_instance: u64,
}

/// The room and app `tab` shows in `desktop`, when it shows an app.
fn local_app_at(desktop: &DesktopTree, tab: TabId) -> Option<(RoomId, &str)> {
    desktop.workspaces.iter().find_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find_map(|candidate| {
                if candidate.id != tab {
                    return None;
                }
                match &candidate.tile {
                    TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
                        Some((workspace.room(), app_id.as_str()))
                    }
                    _ => None,
                }
            })
    })
}

impl WorkspaceTree {
    fn local_app_at(&self, tab: TabId) -> Option<(RoomId, &str)> {
        local_app_at(&self.desktop, tab)
    }

    /// Called only after the launcher/toplevel handshake supplies an instance.
    /// Client metadata reports do not write this device-local association.
    pub(crate) fn remember_local_app_record(&mut self, tab: TabId, record_instance: u64) {
        if record_instance == 0 {
            return;
        }
        let Some((room, app_id)) = self.local_app_at(tab) else {
            return;
        };
        if room.is_nil() || app_id.trim().is_empty() {
            return;
        }
        let record = LocalAppRecord {
            room,
            app_id: app_id.to_owned(),
            record_instance,
        };
        self.local_app_records.insert(tab, record);
    }

    /// Revalidate the current tab, not just the state it had at deserialization.
    /// Moving/repurposing a tab must not recover another room or app's record.
    pub(crate) fn local_app_record(&self, tab: TabId, room: RoomId, app_id: &str) -> Option<u64> {
        let record = self.local_app_records.get(&tab)?;
        (record.record_instance != 0
            && !room.is_nil()
            && !app_id.trim().is_empty()
            && record.room == room
            && record.app_id == app_id
            && self.local_app_at(tab) == Some((room, app_id)))
        .then_some(record.record_instance)
    }

    /// The room and app `tab` shows and the window instance the launcher
    /// started it for, when it shows one.
    pub(crate) fn local_app_window(&self, tab: TabId) -> Option<(RoomId, &str, u64)> {
        let (room, app) = self.local_app_at(tab)?;
        Some((room, app, self.local_app_record(tab, room, app)?))
    }

    pub(crate) fn prune_local_app_records(&mut self) {
        if self.archived_workspaces.is_empty() {
            Self::prune_local_app_records_of(&self.desktop, &mut self.local_app_records);
            return;
        }
        // An archived workspace's windows keep what is theirs.
        let mut held = self.desktop.clone();
        held.workspaces
            .extend(self.archived_workspaces.iter().cloned());
        Self::prune_local_app_records_of(&held, &mut self.local_app_records);
    }

    /// Drop the records in `records` whose tab no longer shows their room's
    /// app in `desktop`, as [`Self::local_app_record`] revalidates them.
    pub(crate) fn prune_local_app_records_of(
        desktop: &DesktopTree,
        records: &mut HashMap<TabId, LocalAppRecord>,
    ) {
        records.retain(|tab, record| {
            record.record_instance != 0
                && !record.room.is_nil()
                && !record.app_id.trim().is_empty()
                && local_app_at(desktop, *tab) == Some((record.room, record.app_id.as_str()))
        });
    }
}
