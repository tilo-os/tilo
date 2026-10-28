//! Waves on the room's live presence, as this session sends them: over a
//! workspace host's live plane, or the room's own topic where none reaches.

use super::{NeighbourMsg, NetCmd, RoomId, ShellNetwork, room_topic_id};

impl ShellNetwork {
    /// Wave at `target` (a roster id) in `cluster_id`'s room on its live
    /// presence, where browser tabs and desktops alike hear it: over the
    /// workspace host's live plane, or the room's own topic where no host
    /// reaches it. Only where its people may see that this person is here;
    /// returns whether it went out.
    pub fn publish_wave(&self, cluster_id: &str, target: &str) -> bool {
        let Some(room) = RoomId::parse(cluster_id).filter(|room| {
            !room.is_nil()
                && self.person_root_of_room(*room).is_none()
                && self.presence_visibility(cluster_id).shows_presence()
        }) else {
            return false;
        };
        let others: Vec<[u8; 32]> = self
            .room_devices(room)
            .iter()
            .map(|d| *d.as_bytes())
            .collect();
        let Some(wave) = self
            .live_waves
            .lock()
            .ok()
            .and_then(|mut waves| waves.aim(target, &others, tilo_clock::unix_now_ms()))
        else {
            return false;
        };
        let ws = room_topic_id(room);
        let desktop = tilo_relay_protocol::live::LiveRecord {
            app: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            record: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
        };
        let frames = self.live_presence.lock().map_or_else(
            |_| Vec::new(),
            |mut live| {
                let at = std::time::Instant::now();
                live.wave(ws, desktop, Self::live_participant(), wave, at)
            },
        );
        // Every frame goes, in order: the wave rides the last.
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

    /// Say on `cluster_id`'s live presence whose view this person follows:
    /// `target` (a roster id; `None` to stop saying), as the device behind
    /// it, so browser tabs show it on their roster as desktops do from the
    /// mesh. Only where its people may see that this person is here;
    /// returns whether a frame went out.
    pub fn publish_following(&self, cluster_id: &str, target: Option<&str>) -> bool {
        let Some(room) = RoomId::parse(cluster_id).filter(|room| {
            !room.is_nil()
                && self.person_root_of_room(*room).is_none()
                && self.presence_visibility(cluster_id).shows_presence()
        }) else {
            return false;
        };
        let others: Vec<[u8; 32]> = self
            .room_devices(room)
            .iter()
            .map(|d| *d.as_bytes())
            .collect();
        let following = target.and_then(|target| {
            self.live_waves
                .lock()
                .ok()?
                .device_of(target, &others)
                .map(hex::encode)
        });
        let ws = room_topic_id(room);
        let desktop = tilo_relay_protocol::live::LiveRecord {
            app: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            record: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
        };
        let frames = self.live_presence.lock().map_or_else(
            |_| Vec::new(),
            |mut live| {
                let at = std::time::Instant::now();
                live.follow(ws, desktop, Self::live_participant(), following, at)
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

    /// Send sealed live `frame` of the room on topic `ws`: on the workspace
    /// host's live plane when the room has one (`hosted`), else on the
    /// room's own topic, where its members read it as a live frame all the
    /// same ([`NeighbourMsg::LivePresence`]).
    pub(super) fn send_live(&self, ws: u64, hosted: bool, frame: Vec<u8>) -> bool {
        if hosted {
            return self
                .cmd_tx
                .try_send(NetCmd::PublishLive { ws, frame })
                .is_ok();
        }
        let msg = NeighbourMsg::LivePresence {
            peer: self.local_peer_id_hash(),
            frame,
        };
        self.join_topic(ws)
            && self
                .cmd_tx
                .try_send(NetCmd::Publish {
                    at: tokio::time::Instant::now(),
                    ws,
                    msg,
                })
                .is_ok()
    }
}
