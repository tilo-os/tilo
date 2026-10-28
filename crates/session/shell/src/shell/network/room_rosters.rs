//! Telling the actor each room's admitted roster: the devices the room's
//! governor admitted are the ones that send and receive its window streams,
//! each until its certificate expires.
use std::collections::HashMap;

use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::network::actor::RosterDevice;

/// The rosters last told to the actor, and the room epoch they were read at.
#[derive(Default)]
pub(super) struct Sent {
    epoch: Option<u64>,
    rosters: HashMap<u64, Vec<RosterDevice>>,
}

impl ShellNetwork {
    /// Tell the actor every room roster that changed since it was last told.
    /// Rosters change only with a projection of the room, which advances the
    /// room epoch, so a tick with none costs one read of the tree. Never
    /// waits for it: a tick that finds another thread writing the tree
    /// looks again on the tick that writer's release wakes.
    pub(super) fn send_room_rosters(&self) {
        let Ok(mut sent) = self.room_rosters_sent.lock() else {
            return;
        };
        let (epoch, rosters) = {
            let Some(tree) = self.tree.try_read_or_wake(self.release_wake()) else {
                return;
            };
            let epoch = tree.desktop.room_epoch;
            if sent.epoch == Some(epoch) {
                return;
            }
            let rosters: HashMap<u64, Vec<RosterDevice>> = tree
                .room_rosters
                .keys()
                .map(|room| {
                    let mut devices: Vec<RosterDevice> = tree
                        .room_peer_identities(*room)
                        .iter()
                        .filter_map(|(peer, identity)| {
                            let device: iroh::EndpointId = peer.parse().ok()?;
                            tree.room_admits_device(*room, &device)
                                .then_some((device, identity.device_cert.expires_at_ns))
                        })
                        .collect();
                    devices.sort_by_key(|(device, _)| *device.as_bytes());
                    (room_topic_id(*room), devices)
                })
                .collect();
            drop(tree);
            (epoch, rosters)
        };
        let gone = sent
            .rosters
            .keys()
            .filter(|ws| !rosters.contains_key(ws))
            .map(|ws| (*ws, Vec::new()))
            .collect::<Vec<_>>();
        let changed = rosters
            .iter()
            .filter(|(ws, devices)| sent.rosters.get(ws) != Some(devices))
            .map(|(ws, devices)| (*ws, devices.clone()))
            .chain(gone);
        let mut told = true;
        for (ws, devices) in changed {
            told &= self
                .cmd_tx
                .try_send(NetCmd::SetRoomRoster { ws, devices })
                .is_ok();
        }
        // A roster the actor did not take is told again on the next tick.
        if told {
            sent.epoch = Some(epoch);
            sent.rosters = rosters;
        }
    }
}
