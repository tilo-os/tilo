//! Where windows are on each member's screen, as the room's live presence
//! carries it ([`LiveLayout`]): this session says its own, and keeps what
//! each live member said, for a mini view of their screen drawn from what
//! this desktop holds. A layout names windows by app and record digest only.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use tilo_relay_protocol::live::{LiveLayout, LiveRecord};

use super::{RoomId, ShellNetwork, room_topic_id};

/// Each live member's layout, per room and handle.
pub(super) type LiveLayouts = Mutex<HashMap<String, BTreeMap<u64, LiveLayout>>>;

impl ShellNetwork {
    /// Say where this person's windows are in `cluster_id`'s room (`None`
    /// to stop): only where its people may see where this person is, as
    /// their focus and pointer are, and otherwise it says none. Sent when
    /// it changes and with the frame's keepalives; returns whether a frame
    /// went out.
    pub fn publish_layout(&self, cluster_id: &str, layout: Option<LiveLayout>) -> bool {
        let Some(room) = RoomId::parse(cluster_id)
            .filter(|room| !room.is_nil() && self.person_root_of_room(*room).is_none())
        else {
            return false;
        };
        let layout = layout.filter(|_| self.presence_visibility(cluster_id).shows_where());
        let ws = room_topic_id(room);
        let desktop = LiveRecord {
            app: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            record: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
        };
        let frames = self.live_presence.lock().map_or_else(
            |_| Vec::new(),
            |mut live| {
                live.lay_out(
                    ws,
                    desktop,
                    Self::live_participant(),
                    layout,
                    std::time::Instant::now(),
                )
            },
        );
        let hosted = self.has_live_plane(cluster_id);
        let mut sent = false;
        for frame in frames
            .iter()
            .filter_map(|frame| self.seal_live(room, frame))
        {
            sent = self.send_live(ws, hosted, frame);
        }
        sent
    }

    /// Where each live member of `cluster_id`'s room said their windows
    /// are, by session handle (`peer@<handle>` in presence).
    pub fn live_layouts_of(&self, cluster_id: &str) -> BTreeMap<u64, LiveLayout> {
        self.live_layouts
            .lock()
            .ok()
            .and_then(|rooms| rooms.get(cluster_id).cloned())
            .unwrap_or_default()
    }

    /// Keep what `peer` said of its windows in `cluster_id`'s room. Someone
    /// who shows nothing sees nothing of where others are.
    pub(super) fn hear_layout(&self, cluster_id: &str, peer: u64, layout: Option<LiveLayout>) {
        let layout = layout.filter(|_| self.presence_visibility(cluster_id).sees_pointers());
        let Ok(mut rooms) = self.live_layouts.lock() else {
            return;
        };
        match layout {
            Some(layout) => {
                rooms
                    .entry(cluster_id.to_owned())
                    .or_default()
                    .insert(peer, layout);
            }
            None => {
                if let Some(room) = rooms.get_mut(cluster_id) {
                    room.remove(&peer);
                }
            }
        }
    }
}
