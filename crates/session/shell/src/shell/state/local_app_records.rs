//! Device-local launcher record associations. These are restart metadata,
//! never room membership, permissions, or a replicated document projection.

use super::{RoomId, TabId, TileContent, WorkspaceTree};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAppRecord {
    pub room: RoomId,
    pub app_id: String,
    pub record_instance: u64,
}

impl WorkspaceTree {
    fn local_app_at(&self, tab: TabId) -> Option<(RoomId, &str)> {
        self.desktop.workspaces.iter().find_map(|workspace| {
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
                        TileContent::Foreign { app_id, .. }
                        | TileContent::LocalApp { app_id, .. } => {
                            Some((workspace.room, app_id.as_str()))
                        }
                        _ => None,
                    }
                })
        })
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

    pub(crate) fn prune_local_app_records(&mut self) {
        let valid: std::collections::HashSet<_> = self
            .local_app_records
            .iter()
            .filter_map(|(tab, record)| {
                self.local_app_record(*tab, record.room, &record.app_id)
                    .map(|_| *tab)
            })
            .collect();
        self.local_app_records.retain(|tab, _| valid.contains(tab));
    }
}
